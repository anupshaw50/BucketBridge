//! Bucket-region resolution (`S3::client_for`), checked against [`MockS3`]
//! with resolution forced on, which emulates AWS: the mock answers like S3
//! does when a request is signed for the wrong region, and the signing
//! region of every request is visible in its `Authorization` header.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bucketbridge_core::{ClientOptions, ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// The region in the credential scope of a SigV4 `Authorization` header.
fn signing_region(request: &RecordedRequest) -> String {
    request
        .header("authorization")
        .and_then(|auth| auth.split("Credential=").nth(1))
        .and_then(|credential| credential.split('/').nth(2))
        .unwrap_or_default()
        .to_owned()
}

fn is_head_bucket(request: &RecordedRequest, bucket: &str) -> bool {
    request.method == "HEAD"
        && (request.path == format!("/{bucket}") || request.path == format!("/{bucket}/"))
}

fn head_bucket_count(mock: &MockS3, bucket: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|r| is_head_bucket(r, bucket))
        .count()
}

fn empty_listing() -> MockResponse {
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>b</Name><IsTruncated>false</IsTruncated></ListBucketResult>"
    ))
}

fn redirect() -> MockResponse {
    MockResponse::error(
        301,
        "PermanentRedirect",
        "The bucket you are attempting to access must be addressed using the specified endpoint.",
    )
}

/// Answers requests signed for `region` like a bucket living there, and
/// everything else like S3 answers a request sent to the wrong region.
fn bucket_in(region: &'static str, request: &RecordedRequest) -> MockResponse {
    if signing_region(request) != region {
        return redirect();
    }
    match request.method.as_str() {
        "GET" if request.query_param("list-type").is_some() => empty_listing(),
        "HEAD" => MockResponse::empty(200),
        "DELETE" => MockResponse::empty(204),
        "POST" if request.query_param("delete").is_some() => {
            MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>"))
        }
        _ => MockResponse::error(400, "UnexpectedRequest", "unexpected request"),
    }
}

fn resolving(mock: &MockS3) -> S3 {
    mock.ops().with_region_resolution(true)
}

#[tokio::test]
async fn a_redirect_naming_the_region_reroutes_and_is_cached() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "far") && signing_region(request) == "us-east-1" {
            MockResponse::empty(301).with_header("x-amz-bucket-region", "eu-west-1")
        } else {
            bucket_in("eu-west-1", request)
        }
    });
    let ops = resolving(&mock);
    ops.list_objects("far", "", false, None, 1000)
        .await
        .unwrap();
    ops.head_object("far", "k", None).await.unwrap();
    ops.delete_object("far", "k", None).await.unwrap();
    let deleted = ops.delete_objects("far", &["k".to_owned()]).await.unwrap();
    assert_eq!(deleted.deleted, 1);
    ops.clone().list_all("far", "").await.unwrap();
    let url = ops
        .presign_get("far", "k", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(url.contains("%2Feu-west-1%2Fs3%2Faws4_request"), "{url}");

    assert_eq!(head_bucket_count(&mock, "far"), 1, "the region is cached");
    let requests = mock.requests();
    for request in requests.iter().filter(|r| !is_head_bucket(r, "far")) {
        assert_eq!(signing_region(request), "eu-west-1", "{request:?}");
        assert!(
            request
                .header("authorization")
                .is_some_and(|auth| auth.contains("Credential=AKIDMOCK/")),
            "{request:?}"
        );
    }
    // The regional client keeps the interceptors of the account client.
    let delete = requests
        .iter()
        .find(|r| r.method == "POST")
        .expect("a DeleteObjects request");
    assert!(delete.header("content-md5").is_some(), "{delete:?}");
}

#[tokio::test]
async fn the_region_header_of_a_refusal_is_used() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "locked") {
            MockResponse::error(403, "AccessDenied", "Access Denied")
                .with_header("x-amz-bucket-region", "ap-southeast-2")
        } else {
            bucket_in("ap-southeast-2", request)
        }
    });
    let ops = resolving(&mock);
    for _ in 0..2 {
        ops.list_objects("locked", "", true, None, 10)
            .await
            .unwrap();
    }
    assert_eq!(head_bucket_count(&mock, "locked"), 1);
}

#[tokio::test]
async fn without_the_header_the_account_region_is_used_and_not_cached() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "plain") {
            MockResponse::empty(200)
        } else if is_head_bucket(request, "missing") {
            MockResponse::empty(404)
        } else {
            bucket_in("us-east-1", request)
        }
    });
    let ops = resolving(&mock);
    for bucket in ["plain", "missing"] {
        for _ in 0..2 {
            ops.list_objects(bucket, "", false, None, 10).await.unwrap();
        }
        assert_eq!(head_bucket_count(&mock, bucket), 2, "{bucket}");
    }
    assert!(
        mock.requests()
            .iter()
            .all(|r| signing_region(r) == "us-east-1")
    );
}

#[tokio::test]
async fn regions_reported_by_list_buckets_need_no_head_bucket() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.path == "/" {
            let page = |name: &str, region: &str, token: &str| {
                MockResponse::xml(format!(
                    "<ListAllMyBucketsResult {NS}><Buckets><Bucket><Name>{name}</Name><BucketRegion>{region}</BucketRegion></Bucket></Buckets>{token}</ListAllMyBucketsResult>"
                ))
            };
            return match request.query_param("continuation-token").as_deref() {
                None => page(
                    "frankfurt",
                    "eu-central-1",
                    "<ContinuationToken>next</ContinuationToken>",
                ),
                Some("next") => page("tokyo", "ap-northeast-1", ""),
                Some(_) => MockResponse::error(400, "UnexpectedRequest", "bad token"),
            };
        }
        let region = if request.path.starts_with("/frankfurt") {
            "eu-central-1"
        } else {
            "ap-northeast-1"
        };
        bucket_in(region, request)
    });
    let ops = resolving(&mock);
    let buckets = ops.list_buckets().await.unwrap();
    let regions: Vec<(&str, Option<&str>)> = buckets
        .iter()
        .map(|b| (b.name.as_str(), b.region.as_deref()))
        .collect();
    assert_eq!(
        regions,
        [
            ("frankfurt", Some("eu-central-1")),
            ("tokyo", Some("ap-northeast-1"))
        ]
    );
    ops.list_objects("frankfurt", "", false, None, 10)
        .await
        .unwrap();
    ops.list_objects("tokyo", "", false, None, 10)
        .await
        .unwrap();

    let requests = mock.requests();
    assert!(requests.iter().all(|r| r.method != "HEAD"), "{requests:?}");
    // Every ListBuckets page asks for 1000 buckets, so that S3 reports
    // regions and accounts with many buckets can paginate.
    let pages: Vec<&RecordedRequest> = requests.iter().filter(|r| r.path == "/").collect();
    assert_eq!(pages.len(), 2);
    for page in pages {
        assert_eq!(page.query_param("max-buckets").as_deref(), Some("1000"));
        assert_eq!(signing_region(page), "us-east-1");
    }
}

#[tokio::test]
async fn presigned_urls_are_signed_for_the_bucket_region() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "far") {
            MockResponse::empty(301).with_header("x-amz-bucket-region", "eu-west-3")
        } else {
            redirect()
        }
    });
    let ops = resolving(&mock);
    for url in [
        ops.presign_get("far", "a b.txt", Duration::from_secs(60))
            .await
            .unwrap(),
        ops.presign_put("far", "a b.txt", Duration::from_secs(60))
            .await
            .unwrap(),
    ] {
        assert!(url.contains("%2Feu-west-3%2Fs3%2Faws4_request"), "{url}");
    }
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn custom_endpoints_never_resolve_regions() {
    let mock = MockS3::start(|request| bucket_in("us-east-1", request));
    let ops = mock.ops();
    ops.list_objects("far", "", false, None, 10).await.unwrap();
    ops.presign_get("far", "k", Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        ops.client_for("far")
            .await
            .unwrap()
            .config()
            .region()
            .map(ToString::to_string)
            .as_deref(),
        Some("us-east-1")
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_ne!(requests[0].method, "HEAD");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_share_one_head_bucket() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "far") {
            std::thread::sleep(Duration::from_millis(300));
            MockResponse::empty(301).with_header("x-amz-bucket-region", "eu-west-1")
        } else {
            bucket_in("eu-west-1", request)
        }
    });
    let ops = resolving(&mock);
    let clients = futures::future::join_all((0..16).map(|_| ops.client_for("far"))).await;
    for client in clients {
        let client = client.unwrap();
        assert_eq!(
            client.config().region().map(ToString::to_string).as_deref(),
            Some("eu-west-1")
        );
    }
    assert_eq!(head_bucket_count(&mock, "far"), 1);
}

#[tokio::test]
async fn an_invalid_region_name_is_an_error() {
    let mock = MockS3::start(|request| {
        MockResponse::empty(301).with_header(
            "x-amz-bucket-region",
            &format!("eu west {}", request.method),
        )
    });
    let err = resolving(&mock)
        .list_objects("far", "", false, None, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert!(err.to_string().contains("invalid region"), "{err}");
    assert_eq!(
        mock.requests().len(),
        1,
        "nothing is sent to an invalid region"
    );
}

#[tokio::test]
async fn created_buckets_are_addressed_in_their_region_until_deleted() {
    let mock = MockS3::start(|request| {
        if is_head_bucket(request, "fresh") {
            MockResponse::empty(404)
        } else if request.method == "PUT" && request.path == "/fresh/" {
            MockResponse::empty(200)
        } else if request.method == "GET" && request.query_param("versions").is_some() {
            MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>fresh</Name><IsTruncated>false</IsTruncated></ListVersionsResult>"
            ))
        } else {
            bucket_in("eu-north-1", request)
        }
    });
    let ops = resolving(&mock);
    ops.create_bucket("fresh", Some("eu-north-1"))
        .await
        .unwrap();
    ops.list_objects("fresh", "", false, None, 10)
        .await
        .unwrap();
    ops.delete_bucket("fresh").await.unwrap();
    assert_eq!(head_bucket_count(&mock, "fresh"), 0);
    // After deleting, the name may come back in another region.
    let _ = ops.list_objects("fresh", "", false, None, 10).await;
    assert_eq!(head_bucket_count(&mock, "fresh"), 1);
}

#[tokio::test]
async fn regional_clients_retry_like_the_account_client() {
    let listings = Arc::new(AtomicUsize::new(0));
    let seen = listings.clone();
    let mock = MockS3::start(move |request| {
        if is_head_bucket(request, "far") {
            return MockResponse::empty(301).with_header("x-amz-bucket-region", "sa-east-1");
        }
        if signing_region(request) == "sa-east-1" && seen.fetch_add(1, Ordering::SeqCst) < 2 {
            return MockResponse::error(503, "SlowDown", "Please reduce your request rate");
        }
        bucket_in("sa-east-1", request)
    });
    let options = ClientOptions {
        max_attempts: 3,
        ..ClientOptions::default()
    };
    let ops = mock.ops_with(&options, false).with_region_resolution(true);
    ops.list_objects("far", "", false, None, 10).await.unwrap();
    assert_eq!(listings.load(Ordering::SeqCst), 3);
}
