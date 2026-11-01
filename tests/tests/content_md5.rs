//! `Content-MD5` on the requests for which S3 requires an integrity header.
//! The SDK sends only a CRC32 checksum there, which some S3-compatible
//! servers do not accept; the client adds `Content-MD5` in every checksum
//! mode.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ClientOptions;
use bucketbridge_core::aws_sdk_s3::types::{Tag, Tagging};
use bucketbridge_core::settings::ChecksumAlgorithm;
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};
use md5::{Digest, Md5};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn expected_md5(request: &RecordedRequest) -> String {
    base64(&Md5::digest(&request.body))
}

fn answer(request: &RecordedRequest) -> MockResponse {
    match request.method.as_str() {
        "POST" if request.query_param("delete").is_some() => {
            MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>"))
        }
        "PUT" => MockResponse::empty(200),
        "GET" if request.query_param("list-type").is_some() => MockResponse::xml(format!(
            "<ListBucketResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated></ListBucketResult>"
        )),
        _ => MockResponse::error(400, "UnexpectedRequest", "unexpected request"),
    }
}

#[test]
fn the_test_encoder_is_standard_base64() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    // MD5 of the empty string.
    assert_eq!(base64(&Md5::digest(b"")), "1B2M2Y8AsgTpgAmY7PhCfg==");
}

#[tokio::test]
async fn delete_objects_sends_content_md5_in_every_checksum_mode() {
    let mock = MockS3::start(answer);
    for algorithm in [
        ChecksumAlgorithm::Crc32,
        ChecksumAlgorithm::Sha256,
        ChecksumAlgorithm::None,
    ] {
        let options = ClientOptions {
            max_attempts: 1,
            checksum_algorithm: algorithm,
            ..ClientOptions::default()
        };
        let keys: Vec<String> = (0..1500).map(|i| format!("dir/{i}.txt")).collect();
        let before = mock.requests().len();
        let outcome = mock
            .ops_with(&options, false)
            .delete_objects("bucket", &keys)
            .await
            .unwrap();
        assert_eq!(outcome.deleted, 1500);
        let requests = mock.requests();
        assert_eq!(requests.len() - before, 2, "{algorithm:?}");
        for request in &requests[before..] {
            assert!(request.body_text().contains("<Delete"), "{algorithm:?}");
            assert_eq!(
                request.header("content-md5"),
                Some(expected_md5(request).as_str()),
                "{algorithm:?}"
            );
        }
    }

    // delete_prefix deletes through DeleteObjects as well.
    let mock = MockS3::start(|request| {
        if request.query_param("list-type").is_some() {
            MockResponse::xml(format!(
                "<ListBucketResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated><Contents><Key>dir/a</Key><Size>1</Size></Contents></ListBucketResult>"
            ))
        } else {
            answer(request)
        }
    });
    mock.ops().delete_prefix("bucket", "dir/").await.unwrap();
    let delete = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .expect("a DeleteObjects request");
    assert_eq!(
        delete.header("content-md5"),
        Some(expected_md5(&delete).as_str())
    );
}

#[tokio::test]
async fn content_md5_is_added_only_where_s3_requires_an_integrity_header() {
    let mock = MockS3::start(answer);
    let ops = mock.ops();
    let tagging = Tagging::builder()
        .tag_set(Tag::builder().key("team").value("photos").build().unwrap())
        .build()
        .unwrap();
    ops.client()
        .put_object_tagging()
        .bucket("bucket")
        .key("k")
        .tagging(tagging.clone())
        .send()
        .await
        .unwrap();
    // A value the caller sets is kept as it is.
    ops.client()
        .put_object_tagging()
        .bucket("bucket")
        .key("k")
        .tagging(tagging)
        .content_md5("caller-value")
        .send()
        .await
        .unwrap();
    ops.create_folder("bucket", "dir/").await.unwrap();
    ops.list_objects("bucket", "", false, None, 10)
        .await
        .unwrap();

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].header("content-md5"),
        Some(expected_md5(&requests[0]).as_str())
    );
    assert_eq!(requests[1].header("content-md5"), Some("caller-value"));
    for request in &requests[2..] {
        assert_eq!(request.header("content-md5"), None, "{request:?}");
    }
}
