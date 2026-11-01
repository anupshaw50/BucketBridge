//! Object Lock, object ownership, Glacier restore, MFA delete and the website
//! redirect header (plan task T07).
//!
//! The requests are checked against [`MockS3`], which records every request
//! with its headers and body, so the tests assert the exact XML and headers
//! that are sent. s3s-fs implements none of these APIs; the tests against a
//! real lock-capable server need MinIO and are ignored without the `minio`
//! feature (docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::bucketcfg::VersioningStatus;
use bucketbridge_core::model::LockMode;
use bucketbridge_core::objectlock::{
    DefaultRetentionRule, MfaDeleteAuth, ObjectOwnership, RestoreTier, RetentionChange,
};
use bucketbridge_core::objectmeta::{BatchOptions, HeaderChange, HeaderChanges};
use bucketbridge_core::versions::{PurgeOptions, VersionRef};
use bucketbridge_core::{ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};
use chrono::{DateTime, TimeZone, Utc};

const BUCKET: &str = "vault";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const SERIAL: &str = "arn:aws:iam::123456789012:mfa/example-device";

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn until() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2099, 1, 2, 3, 4, 5).unwrap()
}

fn only(mock: &MockS3) -> RecordedRequest {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    requests[0].clone()
}

fn ok() -> MockS3 {
    MockS3::start(|_| MockResponse::empty(200))
}

// ---- bucket Object Lock configuration ----

#[tokio::test]
async fn the_bucket_lock_configuration_is_read() {
    let body = format!(
        "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled><Rule><DefaultRetention><Mode>COMPLIANCE</Mode><Years>2</Years></DefaultRetention></Rule></ObjectLockConfiguration>"
    );
    let mock = MockS3::start(move |_| MockResponse::xml(body.clone()));
    let config = mock.ops().get_bucket_object_lock(BUCKET).await.unwrap();
    assert!(config.enabled);
    let rule = config.default_retention.unwrap();
    assert_eq!(
        (rule.mode, rule.days, rule.years),
        (LockMode::Compliance, None, Some(2))
    );
    let request = only(&mock);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, format!("/{BUCKET}/"));
    assert_eq!(request.query, "object-lock");
}

#[tokio::test]
async fn a_bucket_without_object_lock_is_disabled_not_an_error() {
    let mock = MockS3::start(|_| {
        MockResponse::error(
            404,
            "ObjectLockConfigurationNotFoundError",
            "Object Lock configuration does not exist for this bucket",
        )
    });
    let config = mock.ops().get_bucket_object_lock(BUCKET).await.unwrap();
    assert!(!config.enabled && config.default_retention.is_none());
}

#[tokio::test]
async fn the_default_retention_is_sent_as_exact_xml_with_content_md5() {
    let mock = ok();
    let rule = DefaultRetentionRule {
        mode: LockMode::Governance,
        days: Some(30),
        years: None,
    };
    mock.ops()
        .put_bucket_object_lock(BUCKET, Some(&rule))
        .await
        .unwrap();
    let request = only(&mock);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.query, "object-lock");
    assert_eq!(
        request.body_text(),
        format!(
            "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled><Rule><DefaultRetention><Mode>GOVERNANCE</Mode><Days>30</Days></DefaultRetention></Rule></ObjectLockConfiguration>"
        )
    );
    assert!(request.header("content-md5").is_some());
}

#[tokio::test]
async fn clearing_the_default_retention_keeps_object_lock_on() {
    let mock = ok();
    mock.ops()
        .put_bucket_object_lock(BUCKET, None)
        .await
        .unwrap();
    assert_eq!(
        only(&mock).body_text(),
        format!(
            "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled></ObjectLockConfiguration>"
        )
    );
}

#[tokio::test]
async fn bad_default_retentions_are_refused_before_anything_is_sent() {
    let mock = ok();
    let s3 = mock.ops();
    for (days, years) in [
        (None, None),
        (Some(1), Some(1)),
        (Some(0), None),
        (None, Some(101)),
        (Some(36_501), None),
    ] {
        let rule = DefaultRetentionRule {
            mode: LockMode::Governance,
            days,
            years,
        };
        let err = s3
            .put_bucket_object_lock(BUCKET, Some(&rule))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{days:?} {years:?}");
    }
    assert!(mock.requests().is_empty());
}

// ---- object retention ----

#[tokio::test]
async fn retention_is_sent_with_mode_date_version_and_bypass() {
    let mock = ok();
    let results = mock
        .ops()
        .set_object_retention(
            BUCKET,
            &keys(&["dir/a b.txt"]),
            RetentionChange::Set {
                mode: LockMode::Governance,
                retain_until: until(),
            },
            Some("v1"),
            true,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed && results[0].error.is_none());
    let request = only(&mock);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.path, format!("/{BUCKET}/dir/a%20b.txt"));
    assert_eq!(request.query_param("retention").as_deref(), Some(""));
    assert_eq!(request.query_param("versionId").as_deref(), Some("v1"));
    assert_eq!(
        request.header("x-amz-bypass-governance-retention"),
        Some("true")
    );
    assert_eq!(
        request.body_text(),
        format!(
            "<Retention {NS}><Mode>GOVERNANCE</Mode><RetainUntilDate>2099-01-02T03:04:05Z</RetainUntilDate></Retention>"
        )
    );
    assert!(request.header("content-md5").is_some());
}

#[tokio::test]
async fn clearing_a_retention_sends_an_empty_element_and_no_bypass_by_default() {
    let mock = ok();
    mock.ops()
        .set_object_retention(
            BUCKET,
            &keys(&["k"]),
            RetentionChange::Clear,
            None,
            false,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let request = only(&mock);
    assert_eq!(request.header("x-amz-bypass-governance-retention"), None);
    assert_eq!(request.query_param("versionId"), None);
    assert!(
        request.body_text().starts_with("<Retention") && !request.body_text().contains("<Mode>"),
        "{}",
        request.body_text()
    );
}

#[tokio::test]
async fn a_retention_in_the_past_is_a_usage_error_and_sends_nothing() {
    let mock = ok();
    let err = mock
        .ops()
        .set_object_retention(
            BUCKET,
            &keys(&["k"]),
            RetentionChange::Set {
                mode: LockMode::Compliance,
                retain_until: Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap(),
            },
            None,
            false,
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn the_server_error_for_a_locked_object_is_surfaced_verbatim_per_key() {
    const MESSAGE: &str = "Access Denied because object protected by object lock.";
    let mock = MockS3::start(|request| {
        if request.path.ends_with("/locked") {
            MockResponse::error(403, "AccessDenied", MESSAGE)
        } else {
            MockResponse::empty(200)
        }
    });
    let results = mock
        .ops()
        .set_object_retention(
            BUCKET,
            &keys(&["locked", "free"]),
            RetentionChange::Clear,
            None,
            false,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert!(
        error.message.contains(MESSAGE),
        "the message must carry the server text: {}",
        error.message
    );
    assert!(error.message.contains("AccessDenied") || error.message.contains("403"));
    assert!(!results[0].changed);
    // The other object is not held back by the failure.
    assert!(results[1].changed && results[1].error.is_none());
}

#[tokio::test]
async fn the_lock_of_an_object_is_read_and_a_missing_lock_reads_as_none() {
    let mock = MockS3::start(|request| {
        if request.query_param("retention").is_some() {
            MockResponse::xml(format!(
                "<Retention {NS}><Mode>COMPLIANCE</Mode><RetainUntilDate>2099-01-02T03:04:05.000Z</RetainUntilDate></Retention>"
            ))
        } else {
            MockResponse::xml(format!("<LegalHold {NS}><Status>ON</Status></LegalHold>"))
        }
    });
    let state = mock.ops().get_object_lock(BUCKET, "k", None).await.unwrap();
    assert_eq!(state.mode, Some(LockMode::Compliance));
    assert_eq!(state.retain_until, Some(until()));
    assert!(state.legal_hold);
    assert_eq!(
        state.summary(),
        "COMPLIANCE until 2099-01-02T03:04:05Z + legal hold"
    );

    let none = MockS3::start(|request| {
        if request.query_param("retention").is_some() {
            MockResponse::error(
                404,
                "NoSuchObjectLockConfiguration",
                "The specified object does not have a ObjectLock configuration",
            )
        } else {
            MockResponse::error(
                400,
                "InvalidRequest",
                "Bucket is missing Object Lock Configuration",
            )
        }
    });
    let state = none.ops().get_object_lock(BUCKET, "k", None).await.unwrap();
    assert_eq!(
        (state.mode, state.retain_until, state.legal_hold),
        (None, None, false)
    );
    assert_eq!(state.summary(), "none");

    let denied = MockS3::start(|_| MockResponse::error(403, "AccessDenied", "Access Denied"));
    let err = denied
        .ops()
        .get_object_lock(BUCKET, "k", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
}

// ---- legal hold ----

#[tokio::test]
async fn legal_hold_is_read_first_and_only_written_when_it_changes() {
    let mock = MockS3::start(|request| {
        match (request.method.as_str(), request.query_param("legal-hold")) {
            ("GET", Some(_)) => {
                MockResponse::xml(format!("<LegalHold {NS}><Status>OFF</Status></LegalHold>"))
            }
            ("GET", None) => MockResponse::error(404, "NoSuchObjectLockConfiguration", "none"),
            _ => MockResponse::empty(200),
        }
    });
    let s3 = mock.ops();
    let results = s3
        .set_object_legal_hold(BUCKET, &keys(&["k"]), true, None, BatchOptions::default())
        .await
        .unwrap();
    assert!(results[0].changed);
    assert_eq!(results[0].previous.as_deref(), Some("off"));
    let put = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(put.query_param("legal-hold").as_deref(), Some(""));
    assert_eq!(
        put.body_text(),
        format!("<LegalHold {NS}><Status>ON</Status></LegalHold>")
    );
    assert!(put.header("content-md5").is_some());

    // Already off: nothing is sent.
    let before = mock.requests().len();
    let results = s3
        .set_object_legal_hold(BUCKET, &keys(&["k"]), false, None, BatchOptions::default())
        .await
        .unwrap();
    assert!(!results[0].changed && results[0].error.is_none());
    assert!(
        mock.requests()
            .iter()
            .skip(before)
            .all(|r| r.method == "GET")
    );

    // A dry run writes nothing.
    let before = mock.requests().len();
    let results = s3
        .set_object_legal_hold(BUCKET, &keys(&["k"]), true, None, BatchOptions::dry_run())
        .await
        .unwrap();
    assert!(results[0].changed);
    assert!(
        mock.requests()
            .iter()
            .skip(before)
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn a_version_id_needs_exactly_one_object_and_folders_are_refused() {
    let mock = ok();
    let s3 = mock.ops();
    let err = s3
        .set_object_legal_hold(
            BUCKET,
            &keys(&["a", "b"]),
            true,
            Some("v"),
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let err = s3
        .set_object_retention(
            BUCKET,
            &keys(&["dir/"]),
            RetentionChange::Clear,
            None,
            false,
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(mock.requests().is_empty());
}

// ---- object ownership ----

#[tokio::test]
async fn ownership_round_trips_with_exact_xml() {
    let mock = ok();
    mock.ops()
        .put_object_ownership(BUCKET, ObjectOwnership::BucketOwnerPreferred)
        .await
        .unwrap();
    let put = only(&mock);
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query, "ownershipControls");
    assert_eq!(
        put.body_text(),
        format!(
            "<OwnershipControls {NS}><Rule><ObjectOwnership>BucketOwnerPreferred</ObjectOwnership></Rule></OwnershipControls>"
        )
    );
    assert!(put.header("content-md5").is_some());

    let read = MockS3::start(|_| {
        MockResponse::xml(format!(
            "<OwnershipControls {NS}><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>"
        ))
    });
    assert_eq!(
        read.ops().get_object_ownership(BUCKET).await.unwrap(),
        Some(ObjectOwnership::BucketOwnerEnforced)
    );
    let none =
        MockS3::start(|_| MockResponse::error(404, "OwnershipControlsNotFoundError", "not found"));
    assert_eq!(none.ops().get_object_ownership(BUCKET).await.unwrap(), None);
}

#[test]
fn ownership_names_parse_in_any_style() {
    for text in [
        "BucketOwnerEnforced",
        "bucket-owner-enforced",
        "bucket_owner_enforced",
    ] {
        assert_eq!(
            text.parse::<ObjectOwnership>().unwrap(),
            ObjectOwnership::BucketOwnerEnforced
        );
    }
    assert!("owner".parse::<ObjectOwnership>().is_err());
}

// ---- Glacier restore ----

#[tokio::test]
async fn a_restore_request_carries_days_and_tier() {
    let mock = MockS3::start(|_| MockResponse::empty(202));
    let results = mock
        .ops()
        .restore_objects(
            BUCKET,
            &keys(&["cold.bin"]),
            7,
            RestoreTier::Bulk,
            Some("v9"),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed);
    let request = only(&mock);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, format!("/{BUCKET}/cold.bin"));
    assert_eq!(request.query_param("restore").as_deref(), Some(""));
    assert_eq!(request.query_param("versionId").as_deref(), Some("v9"));
    assert_eq!(
        request.body_text(),
        format!(
            "<RestoreRequest {NS}><Days>7</Days><GlacierJobParameters><Tier>Bulk</Tier></GlacierJobParameters></RestoreRequest>"
        )
    );
}

#[tokio::test]
async fn restore_problems_are_per_object_and_worded_by_the_server() {
    let mock = MockS3::start(|request| {
        if request.path.ends_with("/busy") {
            MockResponse::error(
                409,
                "RestoreAlreadyInProgress",
                "Object restore is already in progress",
            )
        } else if request.path.ends_with("/warm") {
            MockResponse::error(
                403,
                "InvalidObjectState",
                "Restore is not allowed for the object's current storage class",
            )
        } else {
            MockResponse::empty(202)
        }
    });
    let results = mock
        .ops()
        .restore_objects(
            BUCKET,
            &keys(&["busy", "warm", "cold"]),
            1,
            RestoreTier::Standard,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(results[0].error.as_ref().unwrap().code, ErrorKind::Conflict);
    assert!(
        results[0]
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("Object restore is already in progress")
    );
    assert!(
        results[1]
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("Restore is not allowed for the object's current storage class")
    );
    assert!(results[2].changed);

    // Bad days are refused before anything is sent.
    let before = mock.requests().len();
    for days in [0, 3651] {
        let err = mock
            .ops()
            .restore_objects(
                BUCKET,
                &keys(&["a"]),
                days,
                RestoreTier::Bulk,
                None,
                BatchOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
    assert_eq!(mock.requests().len(), before);
}

#[tokio::test]
async fn the_restore_state_comes_from_head_object() {
    let mock = MockS3::start(|request| {
        if request.path.ends_with("/running") {
            MockResponse::empty(200)
                .with_header("x-amz-storage-class", "GLACIER")
                .with_header("x-amz-restore", "ongoing-request=\"true\"")
        } else if request.path.ends_with("/done") {
            MockResponse::empty(200)
                .with_header("x-amz-storage-class", "DEEP_ARCHIVE")
                .with_header(
                    "x-amz-restore",
                    "ongoing-request=\"false\", expiry-date=\"Sat, 02 Jan 2100 03:04:05 GMT\"",
                )
        } else if request.path.ends_with("/cold") {
            MockResponse::empty(200).with_header("x-amz-storage-class", "GLACIER")
        } else {
            MockResponse::empty(200)
        }
    });
    let s3 = mock.ops();
    let running = s3
        .object_restore_state(BUCKET, "running", None)
        .await
        .unwrap();
    assert!(running.archived && running.restore.as_ref().unwrap().in_progress);
    let done = s3
        .object_restore_state(BUCKET, "done", Some("v2"))
        .await
        .unwrap();
    assert!(!done.archived);
    let restore = done.restore.unwrap();
    assert!(!restore.in_progress);
    assert_eq!(
        restore.expires,
        Some(Utc.with_ymd_and_hms(2100, 1, 2, 3, 4, 5).unwrap())
    );
    assert_eq!(
        mock.requests()
            .last()
            .unwrap()
            .query_param("versionId")
            .as_deref(),
        Some("v2")
    );
    let cold = s3.object_restore_state(BUCKET, "cold", None).await.unwrap();
    assert!(cold.archived && cold.restore.is_none());
    let plain = s3
        .object_restore_state(BUCKET, "plain", None)
        .await
        .unwrap();
    assert!(!plain.archived && plain.storage_class == "STANDARD");
}

// ---- MFA delete ----

#[tokio::test]
async fn mfa_delete_changes_send_the_serial_and_code() {
    let mock = ok();
    let auth = MfaDeleteAuth::new(SERIAL, "123456").unwrap();
    mock.ops()
        .put_bucket_versioning_mfa(BUCKET, VersioningStatus::Enabled, true, &auth)
        .await
        .unwrap();
    let request = only(&mock);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.query, "versioning");
    assert_eq!(
        request.header("x-amz-mfa"),
        Some(format!("{SERIAL} 123456").as_str())
    );
    assert_eq!(
        request.body_text(),
        format!(
            "<VersioningConfiguration {NS}><MfaDelete>Enabled</MfaDelete><Status>Enabled</Status></VersioningConfiguration>"
        )
    );
    assert!(request.header("content-md5").is_some());

    let err = mock
        .ops()
        .put_bucket_versioning_mfa(BUCKET, VersioningStatus::Unversioned, false, &auth)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn purging_a_version_can_carry_mfa_and_the_governance_bypass() {
    let mock = MockS3::start(|_| MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>")));
    let versions = vec![VersionRef {
        key: "k".into(),
        version_id: "v1".into(),
    }];
    let options = PurgeOptions {
        mfa: Some(MfaDeleteAuth::new(SERIAL, "654321").unwrap()),
        bypass_governance: true,
    };
    let outcome = mock
        .ops()
        .purge_versions_with(BUCKET, &versions, &options)
        .await
        .unwrap();
    assert_eq!(outcome.deleted, 1);
    let request = only(&mock);
    assert_eq!(request.query, "delete");
    assert_eq!(
        request.header("x-amz-mfa"),
        Some(format!("{SERIAL} 654321").as_str())
    );
    assert_eq!(
        request.header("x-amz-bypass-governance-retention"),
        Some("true")
    );
    assert!(request.body_text().contains("<VersionId>v1</VersionId>"));

    // The plain purge sends neither header.
    let plain = MockS3::start(|_| MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>")));
    plain.ops().purge_versions(BUCKET, &versions).await.unwrap();
    let request = only(&plain);
    assert_eq!(request.header("x-amz-mfa"), None);
    assert_eq!(request.header("x-amz-bypass-governance-retention"), None);
}

#[tokio::test]
async fn a_version_the_lock_protects_is_a_failure_with_the_servers_words() {
    let body = format!(
        "<DeleteResult {NS}><Error><Key>k</Key><VersionId>v1</VersionId><Code>AccessDenied</Code><Message>Access Denied because object protected by object lock.</Message></Error></DeleteResult>"
    );
    let mock = MockS3::start(move |_| MockResponse::xml(body.clone()));
    let versions = vec![VersionRef {
        key: "k".into(),
        version_id: "v1".into(),
    }];
    let outcome = mock.ops().purge_versions(BUCKET, &versions).await.unwrap();
    assert_eq!(outcome.deleted, 0);
    assert_eq!(
        outcome.failures[0].message,
        "Access Denied because object protected by object lock."
    );
}

// ---- website redirect location ----

fn redirect_store() -> MockS3 {
    MockS3::start(|request| match request.method.as_str() {
        "HEAD" => MockResponse::empty(200)
            .with_header("content-length", "5")
            .with_header("etag", "\"e\"")
            .with_header("content-type", "text/plain")
            .with_header("x-amz-website-redirect-location", "/old"),
        "PUT" if request.header("x-amz-copy-source").is_some() => MockResponse::xml(
            "<CopyObjectResult><ETag>\"f\"</ETag><LastModified>2026-01-01T00:00:00.000Z</LastModified></CopyObjectResult>",
        ),
        _ => MockResponse::error(400, "Unexpected", &request.path),
    })
}

fn copy_of(mock: &MockS3) -> RecordedRequest {
    mock.requests()
        .into_iter()
        .find(|r| r.method == "PUT" && r.header("x-amz-copy-source").is_some())
        .expect("a copy")
}

async fn edit_redirect(
    mock: &MockS3,
    change: HeaderChange,
) -> Vec<bucketbridge_core::objectmeta::ObjectChange> {
    let changes = HeaderChanges {
        website_redirect: change,
        ..HeaderChanges::default()
    };
    mock.ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn the_website_redirect_is_set_through_the_header_edit() {
    let mock = redirect_store();
    let results = edit_redirect(
        &mock,
        HeaderChange::Set {
            value: "https://example.org/new".into(),
        },
    )
    .await;
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{results:?}"
    );
    let copy = copy_of(&mock);
    assert_eq!(
        copy.header("x-amz-website-redirect-location"),
        Some("https://example.org/new")
    );
    assert_eq!(copy.header("x-amz-metadata-directive"), Some("REPLACE"));
}

#[tokio::test]
async fn the_website_redirect_is_removed_and_otherwise_kept() {
    let mock = redirect_store();
    let results = edit_redirect(&mock, HeaderChange::Remove).await;
    assert!(results[0].changed, "{results:?}");
    assert_eq!(
        copy_of(&mock).header("x-amz-website-redirect-location"),
        None
    );

    // Editing another header keeps the redirect the object has.
    let mock = redirect_store();
    let changes = HeaderChanges {
        cache_control: HeaderChange::Set {
            value: "max-age=60".into(),
        },
        ..HeaderChanges::default()
    };
    mock.ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        copy_of(&mock).header("x-amz-website-redirect-location"),
        Some("/old")
    );

    // Setting what the object has is unchanged, nothing is copied.
    let mock = redirect_store();
    let results = edit_redirect(
        &mock,
        HeaderChange::Set {
            value: "/old".into(),
        },
    )
    .await;
    assert!(!results[0].changed);
    assert!(mock.requests().iter().all(|r| r.method == "HEAD"));
}

#[tokio::test]
async fn bad_redirect_locations_are_usage_errors() {
    let mock = redirect_store();
    for bad in ["new", "ftp://example.org/x", "", "/a\nb"] {
        let changes = HeaderChanges {
            website_redirect: HeaderChange::Set { value: bad.into() },
            ..HeaderChanges::default()
        };
        let err = mock
            .ops()
            .update_object_headers(
                BUCKET,
                &keys(&["k"]),
                &changes,
                None,
                BatchOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{bad:?}");
    }
    assert!(mock.requests().is_empty());
}

// ---- real lock-capable server ----

async fn locked_bucket(s3: &TestS3) -> String {
    let bucket = s3.unique_bucket();
    s3.client()
        .create_bucket()
        .bucket(&bucket)
        .object_lock_enabled_for_bucket(true)
        .send()
        .await
        .unwrap();
    bucket
}

fn ops(s3: &TestS3) -> &S3 {
    s3.ops()
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement Object Lock"
)]
async fn minio_enforces_retention_legal_hold_and_the_governance_bypass() {
    let s3 = TestS3::start_minio().await;
    let bucket = locked_bucket(&s3).await;
    let s3ops = ops(&s3);
    assert!(s3ops.get_bucket_object_lock(&bucket).await.unwrap().enabled);
    let rule = DefaultRetentionRule {
        mode: LockMode::Governance,
        days: Some(1),
        years: None,
    };
    s3ops
        .put_bucket_object_lock(&bucket, Some(&rule))
        .await
        .unwrap();
    assert_eq!(
        s3ops
            .get_bucket_object_lock(&bucket)
            .await
            .unwrap()
            .default_retention,
        Some(rule)
    );

    let put = s3
        .client()
        .put_object()
        .bucket(&bucket)
        .key("held.txt")
        .body(b"x".to_vec().into())
        .send()
        .await
        .unwrap();
    let version = put.version_id().unwrap().to_owned();
    // The default retention was applied to the new object.
    let state = s3ops
        .get_object_lock(&bucket, "held.txt", None)
        .await
        .unwrap();
    assert_eq!(state.mode, Some(LockMode::Governance));

    let one = [VersionRef {
        key: "held.txt".into(),
        version_id: version,
    }];
    // The delete is refused and the failure carries the server's words.
    let refused = s3ops.purge_versions(&bucket, &one).await.unwrap();
    assert_eq!(refused.deleted, 0);
    assert!(!refused.failures[0].message.is_empty());

    // Shortening a governance retention needs the bypass.
    let soon = Utc::now() + chrono::Duration::days(400);
    let longer = s3ops
        .set_object_retention(
            &bucket,
            &keys(&["held.txt"]),
            RetentionChange::Set {
                mode: LockMode::Governance,
                retain_until: soon,
            },
            None,
            false,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(longer[0].error.is_none());
    let hold = s3ops
        .set_object_legal_hold(
            &bucket,
            &keys(&["held.txt"]),
            true,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(hold[0].error.is_none());
    assert!(
        s3ops
            .get_object_lock(&bucket, "held.txt", None)
            .await
            .unwrap()
            .legal_hold
    );
    s3ops
        .set_object_legal_hold(
            &bucket,
            &keys(&["held.txt"]),
            false,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let cleared = s3ops
        .set_object_retention(
            &bucket,
            &keys(&["held.txt"]),
            RetentionChange::Clear,
            None,
            true,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(cleared[0].error.is_none(), "{cleared:?}");
    let purged = s3ops
        .purge_versions_with(
            &bucket,
            &one,
            &PurgeOptions {
                mfa: None,
                bypass_governance: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(purged.deleted, 1, "{purged:?}");
}
