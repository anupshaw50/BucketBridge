//! Transport behaviour that a real server does not produce on demand,
//! checked against the scripted [`MockS3`] endpoint: per-key delete
//! failures, batching, pagination guards, retries, version and upload
//! listings, request-payer headers and location constraints.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bucketbridge_core::model::{BucketKind, DeleteFailure};
use bucketbridge_core::{ClientOptions, ErrorKind};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

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

fn object_list(keys: &[&str], next_token: Option<&str>) -> MockResponse {
    let contents: String = keys
        .iter()
        .map(|k| {
            format!(
                "<Contents><Key>{k}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"etag-{k}\"</ETag><Size>3</Size><StorageClass>STANDARD</StorageClass></Contents>"
            )
        })
        .collect();
    let truncation = match next_token {
        Some(token) => format!(
            "<IsTruncated>true</IsTruncated><NextContinuationToken>{token}</NextContinuationToken>"
        ),
        None => "<IsTruncated>false</IsTruncated>".to_owned(),
    };
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>bucket</Name><Prefix></Prefix><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys>{truncation}{contents}</ListBucketResult>",
        keys.len()
    ))
}

fn is_list_v2(request: &RecordedRequest) -> bool {
    request.method == "GET" && request.query_param("list-type").as_deref() == Some("2")
}

#[tokio::test]
async fn delete_objects_reports_per_key_failures() {
    let mock = MockS3::start(|request| {
        if request.method == "POST" && request.query_param("delete").is_some() {
            MockResponse::xml(format!(
                "<DeleteResult {NS}><Error><Key>locked.txt</Key><VersionId>v1</VersionId><Code>AccessDenied</Code><Message>Access Denied</Message></Error></DeleteResult>"
            ))
        } else {
            unexpected(request)
        }
    });
    let keys = vec!["a.txt".to_owned(), "locked.txt".to_owned()];
    let outcome = mock.ops().delete_objects("bucket", &keys).await.unwrap();
    assert_eq!(outcome.deleted, 1);
    assert_eq!(
        outcome.failures,
        [DeleteFailure {
            key: "locked.txt".into(),
            version_id: Some("v1".into()),
            code: Some("AccessDenied".into()),
            message: "Access Denied".into(),
        }]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let body = requests[0].body_text();
    assert!(body.contains("<Quiet>true</Quiet>"), "{body}");
    assert!(
        body.contains("<Key>a.txt</Key>") && body.contains("<Key>locked.txt</Key>"),
        "{body}"
    );
    // DeleteObjects requires an integrity header; the SDK adds a checksum.
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(name, _)| name.starts_with("x-amz-checksum-") || name == "content-md5"),
        "{:?}",
        requests[0].headers
    );
}

#[tokio::test]
async fn delete_objects_splits_into_batches_of_1000() {
    let mock = MockS3::start(|request| {
        if request.method == "POST" && request.query_param("delete").is_some() {
            MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>"))
        } else {
            unexpected(request)
        }
    });
    let keys: Vec<String> = (0..2500).map(|i| format!("k/{i}")).collect();
    let outcome = mock.ops().delete_objects("bucket", &keys).await.unwrap();
    assert_eq!(outcome.deleted, 2500);
    let counts: Vec<usize> = mock
        .requests()
        .iter()
        .map(|r| r.body_text().matches("<Object>").count())
        .collect();
    assert_eq!(counts, [1000, 1000, 500]);

    let outcome = mock.ops().delete_objects("bucket", &[]).await.unwrap();
    assert_eq!(outcome.deleted, 0);
    assert_eq!(
        mock.requests().len(),
        3,
        "an empty key list must not send a request"
    );
}

#[tokio::test]
async fn a_failed_batch_request_is_an_error() {
    let mock = MockS3::start(|request| {
        if request.method == "POST" && request.query_param("delete").is_some() {
            MockResponse::error(403, "AccessDenied", "Access Denied")
        } else {
            unexpected(request)
        }
    });
    let err = mock
        .ops()
        .delete_objects("bucket", &["a".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth, "{err}");
}

#[tokio::test]
async fn repeated_continuation_tokens_stop_the_listing() {
    let mock = MockS3::start(|request| {
        if is_list_v2(request) {
            object_list(&["k"], Some("again"))
        } else {
            unexpected(request)
        }
    });
    let err = mock.ops().list_all("bucket", "").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert!(err.to_string().contains("same continuation token"), "{err}");
    assert_eq!(mock.requests().len(), 2);
}

#[tokio::test]
async fn truncated_listing_without_a_token_is_an_error() {
    let mock = MockS3::start(|request| {
        if is_list_v2(request) {
            MockResponse::xml(format!(
                "<ListBucketResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><Contents><Key>k</Key><Size>1</Size></Contents></ListBucketResult>"
            ))
        } else {
            unexpected(request)
        }
    });
    let err = mock
        .ops()
        .list_objects("bucket", "", false, None, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert!(err.to_string().contains("no continuation token"), "{err}");
}

#[tokio::test]
async fn empty_pages_with_a_token_are_skipped() {
    let mock = MockS3::start(|request| {
        if !is_list_v2(request) {
            return unexpected(request);
        }
        match request.query_param("continuation-token").as_deref() {
            None => object_list(&[], Some("t1")),
            Some("t1") => object_list(&["k1", "k2"], None),
            Some(_) => unexpected(request),
        }
    });
    let page = mock
        .ops()
        .list_objects("bucket", "", true, None, 1000)
        .await
        .unwrap();
    let keys: Vec<&str> = page.entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, ["k1", "k2"]);
    assert_eq!(page.next_token, None);
    assert_eq!(page.entries[0].etag.as_deref(), Some("etag-k1"));
    assert_eq!(page.entries[0].storage_class.as_deref(), Some("STANDARD"));
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].query_param("delimiter"), None);
    assert_eq!(requests[0].query_param("max-keys").as_deref(), Some("1000"));
}

#[tokio::test]
async fn delimiter_and_page_size_are_sent() {
    let mock = MockS3::start(|request| {
        if is_list_v2(request) {
            object_list(&[], None)
        } else {
            unexpected(request)
        }
    });
    mock.ops()
        .list_objects("bucket", "photos/", false, Some("tok"), 7)
        .await
        .unwrap();
    let request = &mock.requests()[0];
    assert_eq!(request.path, "/bucket/");
    assert_eq!(request.query_param("delimiter").as_deref(), Some("/"));
    assert_eq!(request.query_param("prefix").as_deref(), Some("photos/"));
    assert_eq!(request.query_param("max-keys").as_deref(), Some("7"));
    assert_eq!(
        request.query_param("continuation-token").as_deref(),
        Some("tok")
    );
}

#[tokio::test]
async fn delete_bucket_skips_an_unsupported_version_check() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if is_list_v2(request) => object_list(&[], None),
        "GET" if request.query_param("versions").is_some() => MockResponse::error(
            501,
            "NotImplemented",
            "A header you provided implies functionality that is not implemented",
        ),
        "DELETE" => MockResponse::empty(204),
        _ => unexpected(request),
    });
    mock.ops().delete_bucket("bucket").await.unwrap();
    let methods: Vec<String> = mock.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["GET", "GET", "DELETE"]);
}

#[tokio::test]
async fn delete_bucket_refuses_when_versions_remain() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if is_list_v2(request) => object_list(&[], None),
        "GET" if request.query_param("versions").is_some() => MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated><DeleteMarker><Key>gone</Key><VersionId>dm</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-29T08:15:00.000Z</LastModified></DeleteMarker></ListVersionsResult>"
        )),
        _ => unexpected(request),
    });
    let err = mock.ops().delete_bucket("bucket").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    assert!(mock.requests().iter().all(|r| r.method != "DELETE"));
}

#[tokio::test]
async fn bucket_not_empty_from_the_server_is_a_conflict() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if is_list_v2(request) => object_list(&[], None),
        "GET" if request.query_param("versions").is_some() => MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated></ListVersionsResult>"
        )),
        "DELETE" => MockResponse::error(
            409,
            "BucketNotEmpty",
            "The bucket you tried to delete is not empty",
        ),
        _ => unexpected(request),
    });
    let err = mock.ops().delete_bucket("bucket").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    assert!(err.to_string().contains("BucketNotEmpty"), "{err}");
}

#[tokio::test]
async fn server_errors_keep_code_status_and_request_id() {
    let mock = MockS3::start(|_| {
        MockResponse::error(500, "InternalError", "We encountered an internal error")
    });
    let err = mock.ops().list_buckets().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    let service = err.service().expect("service details");
    assert_eq!(service.code(), Some("InternalError"));
    assert_eq!(service.status(), Some(500));
    assert_eq!(service.request_id(), Some("MOCKREQUEST1"));
    let text = err.to_string();
    assert!(
        text.contains("InternalError")
            && text.contains("HTTP 500")
            && text.contains("MOCKREQUEST1"),
        "{text}"
    );
}

#[tokio::test]
async fn throttling_is_retried_with_the_configured_attempts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mock = MockS3::start(move |request| {
        if request.method != "GET" || request.path != "/" {
            return unexpected(request);
        }
        if seen.fetch_add(1, Ordering::SeqCst) < 2 {
            MockResponse::error(503, "SlowDown", "Please reduce your request rate")
        } else {
            MockResponse::xml(format!(
                "<ListAllMyBucketsResult {NS}><Owner><ID>owner-id</ID></Owner><Buckets><Bucket><Name>b1</Name><CreationDate>2026-10-29T08:15:00.000Z</CreationDate></Bucket></Buckets></ListAllMyBucketsResult>"
            ))
        }
    });
    let options = ClientOptions {
        max_attempts: 3,
        ..ClientOptions::default()
    };
    let buckets = mock.ops_with(&options, false).list_buckets().await.unwrap();
    assert_eq!(buckets.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    calls.store(0, Ordering::SeqCst);
    let err = mock.ops().list_buckets().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "max_attempts = 1 must not retry"
    );
}

#[tokio::test]
async fn list_buckets_follows_continuation_tokens() {
    let mock = MockS3::start(|request| {
        let page = |name: &str, region: &str, token: &str| {
            MockResponse::xml(format!(
                "<ListAllMyBucketsResult {NS}><Owner><ID>owner-id</ID><DisplayName>owner</DisplayName></Owner><Buckets><Bucket><Name>{name}</Name><CreationDate>2026-10-29T08:15:00.000Z</CreationDate><BucketRegion>{region}</BucketRegion></Bucket></Buckets>{token}</ListAllMyBucketsResult>"
            ))
        };
        match request.query_param("continuation-token").as_deref() {
            None => page(
                "first",
                "eu-west-1",
                "<ContinuationToken>next</ContinuationToken>",
            ),
            Some("next") => page("second", "us-east-1", ""),
            Some(_) => unexpected(request),
        }
    });
    let buckets = mock.ops().list_buckets().await.unwrap();
    let names: Vec<&str> = buckets.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(buckets[0].region.as_deref(), Some("eu-west-1"));
    assert_eq!(buckets[0].owner.as_deref(), Some("owner"));
    assert_eq!(buckets[0].kind, BucketKind::External);
    assert_eq!(
        buckets[0].creation_date.map(|d| d.to_rfc3339()),
        Some("2026-10-29T08:15:00+00:00".to_owned())
    );
}

#[tokio::test]
async fn list_versions_orders_entries_and_round_trips_the_token() {
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        match request.query_param("key-marker").as_deref() {
            None => MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>bucket</Name><Prefix></Prefix><NextKeyMarker>a</NextKeyMarker><NextVersionIdMarker>va1</NextVersionIdMarker><MaxKeys>3</MaxKeys><IsTruncated>true</IsTruncated>\
                 <Version><Key>a</Key><VersionId>va2</VersionId><IsLatest>false</IsLatest><LastModified>2026-10-02T00:00:00.000Z</LastModified><ETag>\"e2\"</ETag><Size>2</Size><StorageClass>STANDARD</StorageClass></Version>\
                 <DeleteMarker><Key>a</Key><VersionId>dm1</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-03T00:00:00.000Z</LastModified></DeleteMarker>\
                 <Version><Key>a</Key><VersionId>va1</VersionId><IsLatest>false</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e1\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version>\
                 <CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes></ListVersionsResult>"
            )),
            Some("a") if request.query_param("version-id-marker").as_deref() == Some("va1") => {
                MockResponse::xml(format!(
                    "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated><Version><Key>b</Key><VersionId>vb1</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-04T00:00:00.000Z</LastModified><ETag>\"e3\"</ETag><Size>3</Size></Version></ListVersionsResult>"
                ))
            }
            Some(_) => unexpected(request),
        }
    });
    let ops = mock.ops();
    let first = ops
        .list_versions("bucket", "", false, None, 3)
        .await
        .unwrap();
    let order: Vec<(&str, &str, bool, bool)> = first
        .entries
        .iter()
        .map(|v| {
            (
                v.key.as_str(),
                v.version_id.as_str(),
                v.is_delete_marker,
                v.is_latest,
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("a", "dm1", true, true),
            ("a", "va2", false, false),
            ("a", "va1", false, false)
        ]
    );
    assert_eq!(first.entries[1].etag.as_deref(), Some("e2"));
    assert_eq!(first.entries[0].size, 0);
    assert_eq!(first.prefixes, ["dir/"]);
    let token = first.next_token.expect("a next token");

    let second = ops
        .list_versions("bucket", "", false, Some(&token), 3)
        .await
        .unwrap();
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].version_id, "vb1");
    assert_eq!(second.next_token, None);

    let err = ops
        .list_versions("bucket", "", false, Some("garbage"), 3)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert_eq!(
        mock.requests().len(),
        2,
        "an invalid token must not reach the server"
    );
    assert_eq!(
        mock.requests()[0].query_param("delimiter").as_deref(),
        Some("/")
    );
}

#[tokio::test]
async fn list_multipart_uploads_follows_markers() {
    let mock = MockS3::start(|request| {
        if request.query_param("uploads").is_none() {
            return unexpected(request);
        }
        let upload = |key: &str, id: &str| {
            format!(
                "<Upload><Key>{key}</Key><UploadId>{id}</UploadId><Initiator><ID>initiator-id</ID><DisplayName>uploader</DisplayName></Initiator><StorageClass>STANDARD</StorageClass><Initiated>2026-10-29T08:15:00.000Z</Initiated></Upload>"
            )
        };
        match (
            request.query_param("key-marker").as_deref(),
            request.query_param("upload-id-marker").as_deref(),
        ) {
            (None, None) => MockResponse::xml(format!(
                "<ListMultipartUploadsResult {NS}><Bucket>bucket</Bucket><NextKeyMarker>k1</NextKeyMarker><NextUploadIdMarker>u1</NextUploadIdMarker><IsTruncated>true</IsTruncated>{}</ListMultipartUploadsResult>",
                upload("k1", "u1")
            )),
            (Some("k1"), Some("u1")) => MockResponse::xml(format!(
                "<ListMultipartUploadsResult {NS}><Bucket>bucket</Bucket><IsTruncated>false</IsTruncated>{}</ListMultipartUploadsResult>",
                upload("k2", "u2")
            )),
            _ => unexpected(request),
        }
    });
    let uploads = mock
        .ops()
        .list_multipart_uploads("bucket", "")
        .await
        .unwrap();
    let ids: Vec<(&str, &str)> = uploads
        .iter()
        .map(|u| (u.key.as_str(), u.upload_id.as_str()))
        .collect();
    assert_eq!(ids, [("k1", "u1"), ("k2", "u2")]);
    assert_eq!(uploads[0].initiator.as_deref(), Some("uploader"));
    assert_eq!(uploads[0].storage_class.as_deref(), Some("STANDARD"));
    assert!(uploads[0].initiated.is_some());
}

#[tokio::test]
async fn request_payer_header_is_sent_only_when_enabled() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if is_list_v2(request) => object_list(&[], None),
        "GET" if request.query_param("uploads").is_some() => MockResponse::xml(format!(
            "<ListMultipartUploadsResult {NS}><Bucket>bucket</Bucket><IsTruncated>false</IsTruncated></ListMultipartUploadsResult>"
        )),
        "HEAD" => MockResponse::empty(404),
        "DELETE" => MockResponse::empty(204),
        "POST" => MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>")),
        "PUT" => MockResponse::empty(200),
        _ => unexpected(request),
    });
    let options = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    for payer in [true, false] {
        let ops = mock.ops_with(&options, payer);
        let before = mock.requests().len();
        ops.list_objects("bucket", "", false, None, 10)
            .await
            .unwrap();
        assert_eq!(
            ops.head_object("bucket", "k", None)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
        ops.delete_object("bucket", "k", None).await.unwrap();
        ops.delete_objects("bucket", &["k".to_owned()])
            .await
            .unwrap();
        ops.list_multipart_uploads("bucket", "").await.unwrap();
        ops.abort_multipart_upload("bucket", "k", "upload")
            .await
            .unwrap();
        ops.create_folder("bucket", "dir/").await.unwrap();
        let requests = mock.requests();
        assert_eq!(requests.len() - before, 7);
        for request in &requests[before..] {
            let expected = payer.then_some("requester");
            assert_eq!(
                request.header("x-amz-request-payer"),
                expected,
                "{} {}?{}",
                request.method,
                request.path,
                request.query
            );
        }
    }
}

#[tokio::test]
async fn explicit_region_sends_a_location_constraint_signed_for_that_region() {
    let mock = MockS3::start(|request| {
        if request.method == "PUT" && request.path == "/new-bucket/" {
            MockResponse::empty(200)
        } else {
            unexpected(request)
        }
    });
    let ops = mock.ops();
    ops.create_bucket("new-bucket", Some("eu-west-1"))
        .await
        .unwrap();
    ops.create_bucket("new-bucket", None).await.unwrap();
    ops.create_bucket("new-bucket", Some("us-east-1"))
        .await
        .unwrap();
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let body = requests[0].body_text();
    assert!(
        body.contains("<LocationConstraint>eu-west-1</LocationConstraint>"),
        "{body}"
    );
    let auth = requests[0].header("authorization").unwrap_or_default();
    assert!(auth.contains("/eu-west-1/s3/aws4_request"), "{auth}");
    // A custom endpoint without an explicit region gets no constraint, and
    // us-east-1 never has one.
    assert!(!requests[1].body_text().contains("LocationConstraint"));
    assert!(!requests[2].body_text().contains("LocationConstraint"));
    assert!(
        requests[1]
            .header("authorization")
            .unwrap_or_default()
            .contains("/us-east-1/s3/aws4_request")
    );
}
