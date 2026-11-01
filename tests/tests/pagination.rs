//! Pagination guards against servers that repeat continuation tokens or
//! markers: with entries, and in cycles such as A, B, A that a comparison
//! with the previous token alone does not catch. Every listing must end
//! with a `remote` error after a few requests instead of running forever.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::time::Duration;

use bucketbridge_core::{ErrorKind, Result};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// The next token of the cycle A, B, A, ... for a request carrying `current`.
fn cycle(current: Option<&str>) -> &'static str {
    match current {
        None | Some("B") => "A",
        Some(_) => "B",
    }
}

fn object_page(keys: &[&str], next: &str) -> MockResponse {
    let contents: String = keys
        .iter()
        .map(|k| format!("<Contents><Key>{k}</Key><Size>1</Size></Contents>"))
        .collect();
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><NextContinuationToken>{next}</NextContinuationToken>{contents}</ListBucketResult>"
    ))
}

fn unexpected(request: &RecordedRequest) -> MockResponse {
    MockResponse::error(
        400,
        "UnexpectedRequest",
        &format!(
            "unexpected {} {}?{}",
            request.method, request.path, request.query
        ),
    )
}

/// Runs a listing that must fail with the repeated-token error.
async fn assert_stops<T: std::fmt::Debug>(what: &str, listing: impl Future<Output = Result<T>>) {
    let result = tokio::time::timeout(Duration::from_secs(10), listing)
        .await
        .unwrap_or_else(|_| panic!("{what} loops forever"));
    let err = result.expect_err(what);
    assert_eq!(err.kind(), ErrorKind::Remote, "{what}: {err}");
    assert!(
        err.to_string().contains("same continuation token"),
        "{what}: {err}"
    );
}

#[tokio::test]
async fn list_objects_rejects_a_token_repeated_with_entries() {
    let mock = MockS3::start(|request| {
        if request.query_param("list-type").is_some() {
            object_page(&["k"], "again")
        } else {
            unexpected(request)
        }
    });
    let ops = mock.ops();
    let first = ops
        .list_objects("bucket", "", false, None, 1000)
        .await
        .unwrap();
    assert_eq!(first.next_token.as_deref(), Some("again"));
    assert_stops(
        "list_objects",
        ops.list_objects("bucket", "", false, first.next_token.as_deref(), 1000),
    )
    .await;
    assert_eq!(mock.requests().len(), 2);

    // A caller following next_token, as `bb ls` does, stops too.
    let mut token: Option<String> = None;
    let mut pages = 0;
    let err = loop {
        match ops
            .list_objects("bucket", "", false, token.as_deref(), 1000)
            .await
        {
            Ok(page) => {
                pages += 1;
                assert!(pages < 10, "the caller loops");
                match page.next_token {
                    Some(next) => token = Some(next),
                    None => panic!("the listing cannot end normally"),
                }
            }
            Err(err) => break err,
        }
    };
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert_eq!(pages, 1);
}

#[tokio::test]
async fn object_token_cycles_are_detected() {
    let mock = MockS3::start(|request| {
        if request.query_param("list-type").is_none() {
            return unexpected(request);
        }
        let token = request.query_param("continuation-token");
        // Under dir/ every page holds only the folder marker, which
        // list_objects skips on its own.
        let key = if request.query_param("prefix").as_deref() == Some("dir/") {
            "dir/"
        } else {
            "k"
        };
        object_page(&[key], cycle(token.as_deref()))
    });
    let ops = mock.ops();
    assert_stops("list_all", ops.list_all("bucket", "")).await;
    assert_eq!(mock.requests().len(), 3);

    let before = mock.requests().len();
    assert_stops("delete_prefix", ops.delete_prefix("bucket", "dir/")).await;
    let requests = mock.requests();
    assert_eq!(requests.len() - before, 3);
    assert!(
        requests.iter().all(|r| r.method == "GET"),
        "nothing is deleted"
    );

    let before = mock.requests().len();
    assert_stops(
        "list_objects skipping marker-only pages",
        ops.list_objects("bucket", "dir/", false, None, 1000),
    )
    .await;
    assert_eq!(mock.requests().len() - before, 3);

    let stream_err = {
        use futures::TryStreamExt;
        let entries: Result<Vec<_>> = ops.list_all_stream("bucket", "").try_collect().await;
        entries.unwrap_err()
    };
    assert_eq!(stream_err.kind(), ErrorKind::Remote);
}

#[tokio::test]
async fn bucket_token_cycles_are_detected() {
    let mock = MockS3::start(|request| {
        if request.method != "GET" || request.path != "/" {
            return unexpected(request);
        }
        let token = request.query_param("continuation-token");
        let name = format!(
            "bucket-{}",
            token.as_deref().unwrap_or("first").to_lowercase()
        );
        MockResponse::xml(format!(
            "<ListAllMyBucketsResult {NS}><Buckets><Bucket><Name>{name}</Name></Bucket></Buckets><ContinuationToken>{}</ContinuationToken></ListAllMyBucketsResult>",
            cycle(token.as_deref())
        ))
    });
    assert_stops("list_buckets", mock.ops().list_buckets()).await;
    assert_eq!(mock.requests().len(), 3);
}

#[tokio::test]
async fn version_marker_cycles_are_detected() {
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        let next = cycle(request.query_param("key-marker").as_deref());
        // Only the folder marker itself: list_versions skips such pages.
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><Prefix>dir/</Prefix><NextKeyMarker>{next}</NextKeyMarker><NextVersionIdMarker>v</NextVersionIdMarker><IsTruncated>true</IsTruncated>\
             <Version><Key>dir/</Key><VersionId>v0</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><Size>0</Size></Version></ListVersionsResult>"
        ))
    });
    assert_stops(
        "list_versions",
        mock.ops()
            .list_versions("bucket", "dir/", false, None, 1000),
    )
    .await;
    assert_eq!(mock.requests().len(), 3);
}

#[tokio::test]
async fn upload_marker_cycles_are_detected() {
    let mock = MockS3::start(|request| {
        if request.query_param("uploads").is_none() {
            return unexpected(request);
        }
        let next = cycle(request.query_param("key-marker").as_deref());
        MockResponse::xml(format!(
            "<ListMultipartUploadsResult {NS}><Bucket>bucket</Bucket><NextKeyMarker>{next}</NextKeyMarker><NextUploadIdMarker>u</NextUploadIdMarker><IsTruncated>true</IsTruncated>\
             <Upload><Key>k</Key><UploadId>u-{next}</UploadId></Upload></ListMultipartUploadsResult>"
        ))
    });
    assert_stops(
        "list_multipart_uploads",
        mock.ops().list_multipart_uploads("bucket", ""),
    )
    .await;
    assert_eq!(mock.requests().len(), 3);
}
