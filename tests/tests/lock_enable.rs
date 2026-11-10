//! Turning Object Lock on for an existing bucket
//! (`bucketbridge_core::S3::enable_bucket_object_lock`), checked against the
//! recorded [`MockS3`]: it needs versioning, it is refused clearly without it,
//! and nothing is written when the lock already is on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ErrorKind;
use bucketbridge_tests::{MockResponse, MockS3};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// A bucket with the given versioning status (`None`: never enabled) and
/// Object Lock state.
fn bucket(versioning: Option<&'static str>, lock_on: bool) -> MockS3 {
    MockS3::start(
        move |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "object-lock") if lock_on => MockResponse::xml(format!(
                "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled><Rule><DefaultRetention><Mode>GOVERNANCE</Mode><Days>3</Days></DefaultRetention></Rule></ObjectLockConfiguration>"
            )),
            ("GET", "object-lock") => MockResponse::error(
                404,
                "ObjectLockConfigurationNotFoundError",
                "Object Lock configuration does not exist for this bucket",
            ),
            ("GET", "versioning") => MockResponse::xml(match versioning {
                Some(status) => {
                    format!(
                        "<VersioningConfiguration {NS}><Status>{status}</Status></VersioningConfiguration>"
                    )
                }
                None => format!("<VersioningConfiguration {NS}/>"),
            }),
            _ => MockResponse::empty(200),
        },
    )
}

#[tokio::test]
async fn a_versioned_bucket_gets_the_lock_and_nothing_else_is_sent() {
    let mock = bucket(Some("Enabled"), false);
    assert!(mock.ops().enable_bucket_object_lock("vault").await.unwrap());
    let requests = mock.requests();
    let put = requests.last().unwrap();
    assert_eq!(
        (put.method.as_str(), put.query.as_str()),
        ("PUT", "object-lock")
    );
    assert_eq!(
        put.body_text(),
        format!(
            "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled></ObjectLockConfiguration>"
        )
    );
    assert_eq!(requests.iter().filter(|r| r.method == "PUT").count(), 1);
}

#[tokio::test]
async fn a_bucket_without_versioning_is_a_conflict_and_nothing_is_written() {
    for (status, word) in [(None, "not enabled"), (Some("Suspended"), "suspended")] {
        let mock = bucket(status, false);
        let err = mock
            .ops()
            .enable_bucket_object_lock("vault")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
        let text = err.to_string();
        assert!(text.contains("needs versioning"), "{text}");
        assert!(text.contains(word), "{text}");
        assert!(mock.requests().iter().all(|r| r.method == "GET"));
    }
}

#[tokio::test]
async fn a_bucket_that_already_has_the_lock_is_left_alone() {
    let mock = bucket(Some("Enabled"), true);
    assert!(!mock.ops().enable_bucket_object_lock("vault").await.unwrap());
    // Only the read: a default retention that is set stays.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn a_refusal_by_the_server_is_reported() {
    let mock = MockS3::start(
        |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "object-lock") => {
                MockResponse::error(404, "ObjectLockConfigurationNotFoundError", "none")
            }
            ("GET", "versioning") => MockResponse::xml(format!(
                "<VersioningConfiguration {NS}><Status>Enabled</Status></VersioningConfiguration>"
            )),
            _ => MockResponse::error(403, "AccessDenied", "no"),
        },
    );
    let err = mock
        .ops()
        .enable_bucket_object_lock("vault")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
}
