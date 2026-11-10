//! The Object Lock, ownership, restore and MFA delete commands
//! (docs/desktop-api.md, "Object Lock, ownership, restore and MFA delete
//! (T07)"), invoked with the JSON the UI sends. S3 is a scripted `MockS3`
//! that records the requests.

use bucketbridge_tests::{MockResponse, MockS3};
use serde_json::{Value, json};

use crate::harness::*;

const BUCKET: &str = "vault";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const SERIAL: &str = "arn:aws:iam::123456789012:mfa/example-device";

struct Fixture {
    h: Harness,
    mock: MockS3,
    account: String,
}

fn account_json(mock: &MockS3, serial: Option<&str>) -> Value {
    json!({ "input": {
        "label": "Mock",
        "access_key_id": "AKIDMOCK",
        "secret_access_key": "mock-secret",
        "session_token": null,
        "region": "us-east-1",
        "endpoint": mock.endpoint(),
        "path_style": true,
        "request_payer": false,
        "mfa_serial": serial,
    }})
}

fn fixture_with(mock: MockS3, serial: Option<&str>) -> Fixture {
    let h = Harness::new();
    let account = h.ok("add_account", account_json(&mock, serial))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    Fixture { h, mock, account }
}

fn fixture(
    handler: impl Fn(&bucketbridge_tests::RecordedRequest) -> MockResponse + Send + Sync + 'static,
) -> Fixture {
    fixture_with(MockS3::start(handler), None)
}

#[test]
fn the_bucket_lock_is_read_and_written() {
    let f = fixture(|request| match request.method.as_str() {
        "GET" => MockResponse::xml(format!(
            "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled><Rule><DefaultRetention><Mode>GOVERNANCE</Mode><Days>5</Days></DefaultRetention></Rule></ObjectLockConfiguration>"
        )),
        _ => MockResponse::empty(200),
    });
    let read = f.h.ok(
        "get_bucket_object_lock",
        json!({ "account_id": f.account, "bucket": BUCKET }),
    );
    assert_eq!(keys(&read), ["default_retention", "enabled"]);
    assert_eq!(read["enabled"], true);
    assert_eq!(
        read["default_retention"],
        json!({ "mode": "governance", "days": 5, "years": null })
    );

    f.h.ok(
        "put_bucket_object_lock",
        json!({
            "account_id": f.account,
            "bucket": BUCKET,
            "default_retention": { "mode": "compliance", "days": null, "years": 1 }
        }),
    );
    let put = f
        .mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert!(
        put.body_text()
            .contains("<Mode>COMPLIANCE</Mode><Years>1</Years>")
    );

    // Both units, or none, is a usage error.
    assert_eq!(
        f.h.code(
            "put_bucket_object_lock",
            json!({
                "account_id": f.account,
                "bucket": BUCKET,
                "default_retention": { "mode": "compliance", "days": 1, "years": 1 }
            }),
        ),
        "usage"
    );
}

#[test]
fn retention_and_legal_hold_commands_report_per_object_results() {
    let f = fixture(|request| {
        if request.path.ends_with("/locked") {
            MockResponse::error(
                403,
                "AccessDenied",
                "Access Denied because object protected by object lock.",
            )
        } else if request.method == "GET" && request.query_param("retention").is_some() {
            MockResponse::xml(format!(
                "<Retention {NS}><Mode>GOVERNANCE</Mode><RetainUntilDate>2099-01-02T03:04:05Z</RetainUntilDate></Retention>"
            ))
        } else if request.method == "GET" {
            MockResponse::xml(format!("<LegalHold {NS}><Status>OFF</Status></LegalHold>"))
        } else {
            MockResponse::empty(200)
        }
    });
    let state = f.h.ok(
        "get_object_lock",
        json!({ "account_id": f.account, "bucket": BUCKET, "key": "k", "version_id": null }),
    );
    assert_eq!(keys(&state), ["legal_hold", "mode", "retain_until"]);
    assert_eq!(state["mode"], "governance");
    assert_eq!(state["retain_until"], "2099-01-02T03:04:05Z");
    assert_eq!(state["legal_hold"], false);

    let results = f.h.ok(
        "set_object_retention",
        json!({
            "account_id": f.account,
            "bucket": BUCKET,
            "keys": ["locked", "free"],
            "change": { "action": "set", "mode": "compliance", "retain_until": "2099-01-02T03:04:05Z" },
            "version_id": null,
            "bypass_governance": true,
        }),
    );
    assert_eq!(
        keys(&results[0]),
        ["changed", "error", "key", "previous", "warnings"]
    );
    assert_eq!(results[0]["error"]["code"], "auth");
    assert!(
        results[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Access Denied because object protected by object lock.")
    );
    assert_eq!(results[1]["changed"], true);
    let put = f
        .mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT" && r.path.ends_with("/free"))
        .unwrap();
    assert_eq!(
        put.header("x-amz-bypass-governance-retention"),
        Some("true")
    );
    assert_eq!(put.query_param("versionId"), None);

    // A version ID is for one object.
    f.h.ok(
        "set_object_retention",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["free"],
            "change": { "action": "clear" }, "version_id": "v1", "bypass_governance": false,
        }),
    );
    let versioned = f.mock.requests().into_iter().last().unwrap();
    assert_eq!(versioned.query_param("versionId").as_deref(), Some("v1"));

    let cleared = f.h.ok(
        "set_object_retention",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["free"],
            "change": { "action": "clear" }, "version_id": null, "bypass_governance": false,
        }),
    );
    assert_eq!(cleared[0]["changed"], true);

    let hold = f.h.ok(
        "set_object_legal_hold",
        json!({ "account_id": f.account, "bucket": BUCKET, "keys": ["free"], "on": true, "version_id": null }),
    );
    assert_eq!(hold[0]["changed"], true);
    assert_eq!(hold[0]["previous"], "off");

    // A retention in the past never leaves the machine.
    assert_eq!(
        f.h.code(
            "set_object_retention",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": ["free"],
                "change": { "action": "set", "mode": "governance", "retain_until": "2001-01-01T00:00:00Z" },
                "version_id": null, "bypass_governance": false,
            }),
        ),
        "usage"
    );
}

#[test]
fn ownership_commands_round_trip() {
    let f = fixture(|request| match request.method.as_str() {
        "GET" => MockResponse::xml(format!(
            "<OwnershipControls {NS}><Rule><ObjectOwnership>ObjectWriter</ObjectOwnership></Rule></OwnershipControls>"
        )),
        _ => MockResponse::empty(200),
    });
    assert_eq!(
        f.h.ok(
            "get_object_ownership",
            json!({ "account_id": f.account, "bucket": BUCKET })
        ),
        json!("ObjectWriter")
    );
    f.h.ok(
        "put_object_ownership",
        json!({ "account_id": f.account, "bucket": BUCKET, "ownership": "BucketOwnerEnforced" }),
    );
    let put = f
        .mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert!(
        put.body_text()
            .contains("<ObjectOwnership>BucketOwnerEnforced</ObjectOwnership>")
    );
    assert_eq!(
        f.h.code(
            "put_object_ownership",
            json!({ "account_id": f.account, "bucket": BUCKET, "ownership": "nobody" }),
        ),
        "usage"
    );
}

#[test]
fn restore_commands_request_and_report_the_state() {
    let f = fixture(|request| match request.method.as_str() {
        "POST" => MockResponse::empty(202),
        _ => MockResponse::empty(200)
            .with_header("x-amz-storage-class", "GLACIER")
            .with_header("x-amz-restore", "ongoing-request=\"true\""),
    });
    let results = f.h.ok(
        "restore_objects",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["cold"],
            "days": 3, "tier": "expedited", "version_id": null,
        }),
    );
    assert_eq!(results[0]["changed"], true);
    let post = f
        .mock
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert!(post.body_text().contains("<Days>3</Days>"));
    assert!(post.body_text().contains("<Tier>Expedited</Tier>"));

    let state = f.h.ok(
        "get_restore_state",
        json!({ "account_id": f.account, "bucket": BUCKET, "key": "cold", "version_id": null }),
    );
    assert_eq!(
        keys(&state),
        ["archived", "kind", "restore", "storage_class"]
    );
    assert_eq!(state["kind"], "glacier");
    assert_eq!(state["storage_class"], "GLACIER");

    let estimate = f.h.ok(
        "estimate_restore",
        json!({ "account_id": f.account, "bucket": BUCKET, "keys": ["cold"] }),
    );
    assert_eq!(
        keys(&estimate),
        ["bytes", "note", "objects", "price_date", "summary", "tiers"]
    );
    assert_eq!(estimate["tiers"].as_array().unwrap().len(), 3);
    assert_eq!(estimate["tiers"][0]["tier"], "expedited");
    assert_eq!(state["archived"], true);
    assert_eq!(
        state["restore"],
        json!({ "in_progress": true, "expires": null })
    );

    assert_eq!(
        f.h.code(
            "restore_objects",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": ["cold"],
                "days": 0, "tier": "bulk", "version_id": null,
            }),
        ),
        "usage"
    );
}

#[test]
fn mfa_delete_needs_the_account_serial_and_a_valid_code() {
    let f = fixture(|_| MockResponse::empty(200));
    let args = |code: &str| {
        json!({
            "account_id": f.account, "bucket": BUCKET, "status": "enabled",
            "mfa_delete": true, "mfa_code": code,
        })
    };
    // The account has no MFA serial.
    let (code, message) = f.h.error("put_bucket_versioning_mfa", args("123456"));
    assert_eq!(code, "usage");
    assert!(message.contains("MFA serial"), "{message}");
    assert!(f.mock.requests().is_empty());

    // With a serial, a bad code is a usage error before any request.
    let with_serial = fixture_with(MockS3::start(|_| MockResponse::empty(200)), Some(SERIAL));
    let args = json!({
        "account_id": with_serial.account, "bucket": BUCKET, "status": "enabled",
        "mfa_delete": true, "mfa_code": "12",
    });
    assert_eq!(
        with_serial.h.code("put_bucket_versioning_mfa", args),
        "usage"
    );
    assert!(with_serial.mock.requests().is_empty());
}

#[test]
fn purge_with_options_sends_the_bypass() {
    let f = fixture(|_| MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>")));
    let outcome = f.h.ok(
        "purge_versions_with",
        json!({
            "account_id": f.account, "bucket": BUCKET,
            "versions": [{ "key": "k", "version_id": "v1" }],
            "bypass_governance": true, "mfa_code": null,
        }),
    );
    assert_eq!(outcome["deleted"], 1);
    let request = f.mock.requests().remove(0);
    assert_eq!(
        request.header("x-amz-bypass-governance-retention"),
        Some("true")
    );
    assert_eq!(request.header("x-amz-mfa"), None);
}
