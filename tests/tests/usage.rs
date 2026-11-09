//! Storage usage of a bucket or prefix (`S3::compute_usage`). Plain totals run
//! against the in-process server; versions and multipart uploads are
//! simulated with scripted answers (s3s-fs has neither, docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use bucketbridge_core::ErrorKind;
use bucketbridge_core::usage::{
    CancellationToken, MultipartUsage, NoncurrentUsage, UsageOptions, UsagePhase, UsageProgress,
};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

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

#[tokio::test]
async fn totals_count_current_objects_under_a_prefix() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a.txt", "12345").await;
    s3.put(&bucket, "dir/b.txt", "1234567890").await;
    s3.put(&bucket, "dir/sub/c.txt", "abc").await;
    s3.put(&bucket, "other/d.txt", "xy").await;

    let whole = s3
        .ops()
        .compute_usage(&bucket, "", UsageOptions::default(), None, |_| {})
        .await
        .unwrap();
    assert_eq!((whole.objects, whole.bytes), (4, 20));
    assert!(whole.noncurrent.is_none() && whole.multipart.is_none());

    let reports = Mutex::new(Vec::<UsageProgress>::new());
    let dir = s3
        .ops()
        .compute_usage(&bucket, "dir/", UsageOptions::default(), None, |p| {
            reports.lock().unwrap().push(p.clone());
        })
        .await
        .unwrap();
    assert_eq!((dir.objects, dir.bytes), (2, 13));
    let reports = reports.into_inner().unwrap();
    assert_eq!(reports.last().unwrap().usage, dir);
    assert_eq!(reports.last().unwrap().phase, UsagePhase::Objects);

    let empty = s3
        .ops()
        .compute_usage(&bucket, "nothing/", UsageOptions::default(), None, |_| {})
        .await
        .unwrap();
    assert_eq!((empty.objects, empty.bytes), (0, 0));
}

#[tokio::test]
async fn a_cancelled_token_ends_the_computation() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a.txt", "1").await;
    let token = CancellationToken::new();
    token.cancel();
    let err = s3
        .ops()
        .compute_usage(&bucket, "", UsageOptions::default(), Some(&token), |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Cancelled);
}

#[tokio::test]
async fn an_invalid_bucket_name_is_a_usage_error() {
    let s3 = TestS3::start().await;
    let err = s3
        .ops()
        .compute_usage("bad name", "", UsageOptions::default(), None, |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

fn version_xml(key: &str, id: &str, latest: bool, size: u64) -> String {
    format!(
        "<Version><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>{latest}</IsLatest><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"e\"</ETag><Size>{size}</Size><StorageClass>STANDARD</StorageClass></Version>"
    )
}

fn marker_xml(key: &str, id: &str, latest: bool) -> String {
    format!(
        "<DeleteMarker><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>{latest}</IsLatest><LastModified>2026-10-29T08:15:00.000Z</LastModified></DeleteMarker>"
    )
}

#[tokio::test]
async fn versions_split_into_current_noncurrent_and_delete_markers() {
    // Two pages: a.txt has a delete marker on top of two versions; b.txt has a
    // current version and one older one.
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        match request.query_param("key-marker").as_deref() {
            None => MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>bucket</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>true</IsTruncated><NextKeyMarker>a.txt</NextKeyMarker><NextVersionIdMarker>a1</NextVersionIdMarker>{}{}{}</ListVersionsResult>",
                marker_xml("a.txt", "a3", true),
                version_xml("a.txt", "a2", false, 100),
                version_xml("a.txt", "a1", false, 50),
            )),
            Some("a.txt") => MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>bucket</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{}{}</ListVersionsResult>",
                version_xml("b.txt", "b2", true, 7),
                version_xml("b.txt", "b1", false, 3),
            )),
            _ => unexpected(request),
        }
    });
    let reports = Mutex::new(0usize);
    let usage = mock
        .ops()
        .compute_usage(
            "bucket",
            "",
            UsageOptions {
                versions: true,
                multipart: false,
            },
            None,
            |_| *reports.lock().unwrap() += 1,
        )
        .await
        .unwrap();
    assert_eq!((usage.objects, usage.bytes), (1, 7));
    assert_eq!(
        usage.noncurrent,
        Some(NoncurrentUsage {
            versions: 3,
            bytes: 153,
            delete_markers: 1
        })
    );
    assert_eq!(*reports.lock().unwrap(), 2);
}

fn upload_xml(key: &str, id: &str) -> String {
    format!(
        "<Upload><Key>{key}</Key><UploadId>{id}</UploadId><StorageClass>STANDARD</StorageClass><Initiated>2026-10-29T08:15:00.000Z</Initiated></Upload>"
    )
}

fn part_xml(number: u32, size: u64) -> String {
    format!(
        "<Part><PartNumber>{number}</PartNumber><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"p{number}\"</ETag><Size>{size}</Size></Part>"
    )
}

fn multipart_mock() -> MockS3 {
    MockS3::start(|request| {
        if request.query_param("uploads").is_some() {
            return MockResponse::xml(format!(
                "<ListMultipartUploadsResult {NS}><Bucket>bucket</Bucket><IsTruncated>false</IsTruncated>{}{}</ListMultipartUploadsResult>",
                upload_xml("big.bin", "u1"),
                upload_xml("other.bin", "u2"),
            ));
        }
        if let Some(id) = request.query_param("uploadId") {
            return match (id.as_str(), request.query_param("part-number-marker")) {
                ("u1", None) => MockResponse::xml(format!(
                    "<ListPartsResult {NS}><Bucket>bucket</Bucket><Key>big.bin</Key><UploadId>u1</UploadId><NextPartNumberMarker>2</NextPartNumberMarker><IsTruncated>true</IsTruncated>{}{}</ListPartsResult>",
                    part_xml(1, 5000),
                    part_xml(2, 5000)
                )),
                ("u1", Some(_)) => MockResponse::xml(format!(
                    "<ListPartsResult {NS}><Bucket>bucket</Bucket><Key>big.bin</Key><UploadId>u1</UploadId><IsTruncated>false</IsTruncated>{}</ListPartsResult>",
                    part_xml(3, 1200)
                )),
                ("u2", _) => MockResponse::xml(format!(
                    "<ListPartsResult {NS}><Bucket>bucket</Bucket><Key>other.bin</Key><UploadId>u2</UploadId><IsTruncated>false</IsTruncated>{}</ListPartsResult>",
                    part_xml(1, 10)
                )),
                _ => unexpected(request),
            };
        }
        if request.query_param("list-type").as_deref() == Some("2") {
            return MockResponse::xml(format!(
                "<ListBucketResult {NS}><Name>bucket</Name><Prefix></Prefix><KeyCount>0</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated></ListBucketResult>"
            ));
        }
        unexpected(request)
    })
}

#[tokio::test]
async fn multipart_uploads_add_up_their_parts_across_pages() {
    let mock = multipart_mock();
    let phases = Mutex::new(Vec::new());
    let usage = mock
        .ops()
        .compute_usage(
            "bucket",
            "",
            UsageOptions {
                versions: false,
                multipart: true,
            },
            None,
            |p| phases.lock().unwrap().push(p.phase),
        )
        .await
        .unwrap();
    assert_eq!(
        usage.multipart,
        Some(MultipartUsage {
            uploads: 2,
            parts: 4,
            bytes: 11_210
        })
    );
    assert_eq!((usage.objects, usage.bytes), (0, 0));
    let phases = phases.into_inner().unwrap();
    assert_eq!(phases.first(), Some(&UsagePhase::Objects));
    assert_eq!(phases.last(), Some(&UsagePhase::Multipart));
}

#[tokio::test]
async fn cancelling_while_sizing_uploads_stops_with_cancelled() {
    let mock = multipart_mock();
    let token = CancellationToken::new();
    let seen = token.clone();
    let result = mock
        .ops()
        .compute_usage(
            "bucket",
            "",
            UsageOptions {
                versions: false,
                multipart: true,
            },
            Some(&token),
            move |p| {
                if p.phase == UsagePhase::Multipart {
                    seen.cancel();
                }
            },
        )
        .await;
    assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled);
    // Only the first upload was sized (two part pages at most).
    let part_requests = mock
        .requests()
        .iter()
        .filter(|r| r.query_param("uploadId").is_some())
        .count();
    assert!(part_requests <= 2, "{part_requests}");
}
