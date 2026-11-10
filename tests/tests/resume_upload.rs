//! Resuming an unfinished multipart upload that has no saved state
//! (`TransferEngine::check_upload_resume`), against the scripted `MockS3`
//! (s3s-fs cannot list the parts of an upload, see docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use bucketbridge_core::engine::{
    Hooks, PartVerification, ResumeProblemCode, TransferEngine, TransferOptions, checksum_of,
};
use bucketbridge_core::settings::{ChecksumAlgorithm, MIB};
use bucketbridge_core::{ErrorKind, S3Uri};
use bucketbridge_tests::{MockResponse, MockS3};
use md5::{Digest, Md5};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const PART: usize = 5 * MIB as usize;

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u8)
                .wrapping_mul(31)
                .wrapping_add(seed)
                .wrapping_add((i >> 11) as u8)
        })
        .collect()
}

/// What the server lists for the first `count` parts of `data`.
fn parts_xml(data: &[u8], count: usize, algorithm: ChecksumAlgorithm) -> String {
    let mut xml = format!(
        "<ListPartsResult {NS}><Bucket>b</Bucket><Key>big.bin</Key><UploadId>u1</UploadId><IsTruncated>false</IsTruncated>"
    );
    if algorithm == ChecksumAlgorithm::Crc32 {
        xml.push_str("<ChecksumAlgorithm>CRC32</ChecksumAlgorithm>");
    }
    for number in 1..=count {
        let chunk = &data[(number - 1) * PART..(number * PART).min(data.len())];
        let etag: String = Md5::digest(chunk)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        xml.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>\"{etag}\"</ETag><Size>{}</Size>",
            chunk.len()
        ));
        if algorithm == ChecksumAlgorithm::Crc32 {
            xml.push_str(&format!(
                "<ChecksumCRC32>{}</ChecksumCRC32>",
                checksum_of(ChecksumAlgorithm::Crc32, chunk).unwrap()
            ));
        }
        xml.push_str("</Part>");
    }
    xml.push_str("</ListPartsResult>");
    xml
}

/// A server that holds the first `count` parts of `server_data`, accepts the
/// rest and completes.
fn server(server_data: Vec<u8>, count: usize, algorithm: ChecksumAlgorithm) -> MockS3 {
    MockS3::start(move |request| {
        let query = |name: &str| request.query_param(name);
        match request.method.as_str() {
            "GET" if query("uploadId").is_some() => {
                MockResponse::xml(parts_xml(&server_data, count, algorithm))
            }
            "PUT" if query("partNumber").is_some() => {
                MockResponse::empty(200).with_header("etag", "\"new-part\"")
            }
            "POST" if query("uploadId").is_some() => MockResponse::xml(format!(
                "<CompleteMultipartUploadResult {NS}><Bucket>b</Bucket><Key>big.bin</Key><ETag>\"whole-3\"</ETag></CompleteMultipartUploadResult>"
            )),
            _ => MockResponse::error(404, "NoSuchUpload", "no such upload"),
        }
    })
}

fn engine(mock: &MockS3) -> TransferEngine {
    TransferEngine::new(
        mock.ops(),
        TransferOptions {
            multipart_threshold: 5 * MIB,
            part_size: 5 * MIB,
            max_parts_in_flight: 2,
            ..TransferOptions::default()
        },
    )
    .unwrap()
}

fn file(dir: &tempfile::TempDir, data: &[u8]) -> PathBuf {
    let path = dir.path().join("big.bin");
    std::fs::write(&path, data).unwrap();
    path
}

#[tokio::test]
async fn a_matching_file_verifies_by_checksum_and_uploads_only_the_missing_part() {
    let data = bytes(2 * PART + 17, 1);
    let mock = server(data.clone(), 2, ChecksumAlgorithm::Crc32);
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, &data);
    let engine = engine(&mock);

    let (check, state) = engine
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(check.ok(), "{check:?}");
    assert_eq!(check.part_size, 5 * MIB);
    assert_eq!(check.part_count, 3);
    assert_eq!(check.missing_parts, 1);
    assert_eq!(check.bytes_uploaded, 2 * 5 * MIB);
    assert_eq!(check.bytes_remaining, 17);
    assert_eq!(check.checksum_algorithm, ChecksumAlgorithm::Crc32);
    assert!(
        check
            .parts
            .iter()
            .all(|p| p.verified == PartVerification::Checksum)
    );
    let state = state.unwrap();
    assert_eq!(state.parts.len(), 2);

    // Running the state sends part 3 and completes; parts 1 and 2 are not sent.
    let before = mock.requests().len();
    let outcome = engine
        .upload_file(
            &path,
            &S3Uri::new("b", "big.bin").unwrap(),
            &Hooks::new(),
            Some(state),
            &|_| {},
        )
        .await
        .unwrap();
    assert!(outcome.is_completed(), "{outcome:?}");
    let sent: Vec<_> = mock.requests().split_off(before);
    let puts: Vec<_> = sent.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), 1, "{sent:?}");
    assert_eq!(puts[0].query_param("partNumber").as_deref(), Some("3"));
    assert_eq!(puts[0].body.len(), 17);
    let complete = sent.iter().find(|r| r.method == "POST").unwrap();
    assert!(complete.body_text().contains("<PartNumber>3</PartNumber>"));
    assert!(complete.body_text().contains("<PartNumber>1</PartNumber>"));
}

#[tokio::test]
async fn a_different_file_is_refused_with_the_part_that_differs() {
    let data = bytes(2 * PART + 17, 1);
    let mock = server(data.clone(), 2, ChecksumAlgorithm::Crc32);
    let dir = tempfile::tempdir().unwrap();
    let mut other = data.clone();
    other[PART + 100] ^= 0xFF;
    let path = file(&dir, &other);

    let (check, state) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(state.is_none());
    let problem = check.problem.unwrap();
    assert_eq!(problem.code, ResumeProblemCode::PartMismatch);
    assert_eq!(problem.part, Some(2));
}

#[tokio::test]
async fn parts_without_checksums_are_compared_by_etag() {
    let data = bytes(2 * PART + 17, 2);
    let mock = server(data.clone(), 2, ChecksumAlgorithm::None);
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, &data);
    let (check, state) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(check.ok(), "{check:?}");
    assert!(
        check
            .parts
            .iter()
            .all(|p| p.verified == PartVerification::Etag)
    );
    assert_eq!(check.checksum_algorithm, ChecksumAlgorithm::None);
    assert!(state.is_some());

    let mut other = data.clone();
    other[10] ^= 1;
    let path = file(&dir, &other);
    let engine = engine(&mock);
    let (check, state) = engine
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert_eq!(check.problem.unwrap().part, Some(1));
    assert!(state.is_none());

    // The content check can be left out (an upload that is encrypted with a
    // KMS key has ETags that are not an MD5); sizes and positions still count.
    let (check, state) = engine
        .check_upload_resume(&path, "b", "big.bin", "u1", true)
        .await
        .unwrap();
    assert!(check.ok());
    assert!(
        check
            .parts
            .iter()
            .all(|p| p.verified == PartVerification::Skipped)
    );
    assert!(state.is_some());
}

#[tokio::test]
async fn files_of_the_wrong_size_are_refused() {
    let data = bytes(2 * PART + 17, 3);
    let mock = server(data.clone(), 2, ChecksumAlgorithm::Crc32);
    let dir = tempfile::tempdir().unwrap();

    // Smaller than what was uploaded.
    let path = file(&dir, &data[..PART + 5]);
    let (check, state) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert_eq!(check.problem.unwrap().code, ResumeProblemCode::FileTooSmall);
    assert!(state.is_none());

    // Exactly the uploaded parts and nothing more: the last part has the
    // size of the others, so the file is complete after the uploaded parts.
    let path = file(&dir, &data[..2 * PART]);
    let (check, _) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(check.ok());
    assert_eq!(check.missing_parts, 0);
    assert_eq!(check.bytes_remaining, 0);

    // The server holds parts of 5 MiB, the file would need other parts.
    let mock = server(bytes(3 * PART, 3), 2, ChecksumAlgorithm::Crc32);
    let path = file(&dir, &bytes(PART + PART / 2, 3));
    let (check, _) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(!check.ok());
}

#[tokio::test]
async fn a_missing_upload_is_not_found() {
    let mock = MockS3::start(|_| MockResponse::error(404, "NoSuchUpload", "gone"));
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, &bytes(PART, 1));
    let err = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn an_upload_without_parts_starts_from_the_beginning() {
    let data = bytes(PART + 1, 4);
    let mock = server(data.clone(), 0, ChecksumAlgorithm::None);
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, &data);
    let (check, state) = engine(&mock)
        .check_upload_resume(&path, "b", "big.bin", "u1", false)
        .await
        .unwrap();
    assert!(check.ok());
    assert_eq!(check.missing_parts, 2);
    assert!(state.unwrap().parts.is_empty());
}
