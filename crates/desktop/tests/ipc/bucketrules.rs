//! Bucket rule commands: lifecycle, Intelligent-Tiering, CORS and
//! replication round-trip through IPC (read, write, read again), sections
//! fail independently, and invalid input is rejected before anything is sent.
//! S3 is the stateful `RulesMock` (s3s-fs implements none of these APIs).

use bucketbridge_tests::RulesMock;
use serde_json::{Value, json};

use crate::harness::*;

fn account_for(h: &Harness, server: &RulesMock) -> String {
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Main",
            "access_key_id": "AKIDMOCK",
            "secret_access_key": "mock-secret",
            "session_token": null,
            "region": "us-east-1",
            "endpoint": server.endpoint(),
            "path_style": true,
            "request_payer": false,
            "mfa_serial": null,
        } }),
    );
    account["id"].as_str().unwrap().to_owned()
}

fn on(id: &str) -> Value {
    json!({ "account_id": id, "bucket": "site" })
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn lifecycle() -> Value {
    json!({ "rules": [{
        "id": "expire-logs",
        "status": "enabled",
        "filter": { "prefix": "logs/", "tags": [], "object_size_greater_than": null, "object_size_less_than": null },
        "transitions": [{ "days": 30, "date": null, "storage_class": "STANDARD_IA" }],
        "expiration": { "days": 365, "date": null, "expired_object_delete_marker": false },
        "noncurrent_version_transitions": [],
        "noncurrent_version_expiration": { "noncurrent_days": 30, "newer_noncurrent_versions": 3 },
        "abort_incomplete_multipart_upload_days": 7
    }] })
}

#[test]
fn lifecycle_round_trips_through_ipc() {
    let server = RulesMock::start();
    let h = Harness::new();
    let site = on(&account_for(&h, &server));

    assert_eq!(h.ok("get_bucket_lifecycle", site.clone()), Value::Null);
    h.ok(
        "put_bucket_lifecycle",
        with(site.clone(), "config", lifecycle()),
    );
    assert_eq!(h.ok("get_bucket_lifecycle", site.clone()), lifecycle());

    // The combined read shows it next to the other (empty) settings.
    let rules = h.ok("get_bucket_rules", site.clone());
    assert_eq!(
        keys(&rules),
        ["cors", "lifecycle", "replication", "tiering"]
    );
    assert_eq!(
        rules["lifecycle"],
        json!({ "type": "loaded", "value": lifecycle() })
    );
    assert_eq!(rules["cors"], json!({ "type": "loaded", "value": null }));
    assert_eq!(rules["tiering"], json!({ "type": "loaded", "value": [] }));

    // Partial documents work too: the model fills in the defaults.
    let minimal = json!({ "rules": [{ "id": "m", "expiration": { "days": 2 } }] });
    h.ok(
        "put_bucket_lifecycle",
        with(site.clone(), "config", minimal),
    );
    let read = h.ok("get_bucket_lifecycle", site.clone());
    assert_eq!(read["rules"][0]["expiration"]["days"], 2);
    assert_eq!(read["rules"][0]["status"], "enabled");

    assert_eq!(h.ok("delete_bucket_lifecycle", site.clone()), Value::Null);
    assert_eq!(h.ok("get_bucket_lifecycle", site.clone()), Value::Null);
    assert_eq!(h.ok("delete_bucket_lifecycle", site), Value::Null);
}

#[test]
fn invalid_documents_are_rejected_before_anything_is_sent() {
    let server = RulesMock::start();
    let h = Harness::new();
    let id = account_for(&h, &server);
    let site = on(&id);

    let mut bad = lifecycle();
    bad["rules"][0]["transitions"][0]["days"] = json!(5);
    let (code, message) = h.error(
        "put_bucket_lifecycle",
        with(site.clone(), "config", bad.clone()),
    );
    assert_eq!(code, "usage");
    assert!(
        message.contains("rules[0].transitions[0].days: objects stay at least 30 days"),
        "{message}"
    );

    let cors = json!({ "rules": [{ "allowed_origins": [], "allowed_methods": ["get"] }] });
    let (code, message) = h.error("put_bucket_cors", with(site.clone(), "config", cors));
    assert_eq!(code, "usage");
    assert!(message.contains("rules[0].allowed_origins: allow at least one origin"));
    assert!(message.contains("rules[0].allowed_methods[0]"));

    let tiering =
        json!({ "id": "t", "tierings": [{ "access_tier": "ARCHIVE_ACCESS", "days": 1 }] });
    assert_eq!(
        h.code("put_bucket_tiering", with(site.clone(), "config", tiering)),
        "usage"
    );
    assert!(server.requests().is_empty(), "nothing may be sent");

    // The validators need no account and report every problem with its path.
    let issues = h.ok(
        "validate_bucket_lifecycle",
        json!({ "document": bad.to_string() }),
    );
    assert_eq!(issues.as_array().unwrap().len(), 1);
    assert_eq!(issues[0]["path"], "rules[0].transitions[0].days");
    assert_eq!(
        h.ok(
            "validate_bucket_lifecycle",
            json!({ "document": lifecycle().to_string() })
        ),
        json!([])
    );
    let broken = h.ok(
        "validate_bucket_cors",
        json!({ "document": "{\"rules\": [{\"origins\": 1}]}" }),
    );
    assert!(
        broken[0]["message"]
            .as_str()
            .unwrap()
            .contains("unknown field `origins`")
    );
    assert_eq!(broken[0]["path"], "");
    assert_eq!(
        h.ok("validate_bucket_tiering", json!({ "document": "" }))[0]["message"],
        "the document is empty"
    );
    assert_eq!(
        h.ok(
            "validate_bucket_replication",
            json!({ "document": "{\"role\":\"\"}" })
        )[0]["path"],
        "role"
    );
}

#[test]
fn tiering_cors_and_replication_round_trip_through_ipc() {
    let server = RulesMock::start();
    let h = Harness::new();
    let site = on(&account_for(&h, &server));

    // Intelligent-Tiering: several configurations, listed by id.
    let archive = json!({
        "id": "archive", "status": "enabled",
        "filter": { "prefix": "cold/", "tags": [] },
        "tierings": [{ "access_tier": "ARCHIVE_ACCESS", "days": 90 }]
    });
    let deep = json!({
        "id": "deep", "status": "disabled",
        "filter": { "prefix": null, "tags": [{ "key": "t", "value": "1" }] },
        "tierings": [{ "access_tier": "DEEP_ARCHIVE_ACCESS", "days": 365 }]
    });
    assert_eq!(h.ok("list_bucket_tiering", site.clone()), json!([]));
    h.ok(
        "put_bucket_tiering",
        with(site.clone(), "config", deep.clone()),
    );
    h.ok(
        "put_bucket_tiering",
        with(site.clone(), "config", archive.clone()),
    );
    assert_eq!(
        h.ok("list_bucket_tiering", site.clone()),
        json!([archive, deep])
    );
    h.ok(
        "delete_bucket_tiering",
        with(site.clone(), "id", json!("deep")),
    );
    assert_eq!(h.ok("list_bucket_tiering", site.clone()), json!([archive]));

    // CORS.
    let cors = json!({ "rules": [{
        "id": "web",
        "allowed_origins": ["https://app.example.org"],
        "allowed_methods": ["GET", "HEAD"],
        "allowed_headers": ["*"],
        "expose_headers": ["ETag"],
        "max_age_seconds": 600
    }] });
    assert_eq!(h.ok("get_bucket_cors", site.clone()), Value::Null);
    h.ok(
        "put_bucket_cors",
        with(site.clone(), "config", cors.clone()),
    );
    assert_eq!(h.ok("get_bucket_cors", site.clone()), cors);
    h.ok("delete_bucket_cors", site.clone());
    assert_eq!(h.ok("get_bucket_cors", site.clone()), Value::Null);

    // Replication needs versioning on the source.
    let replication = json!({
        "role": "arn:aws:iam::123456789012:role/replicate",
        "rules": [{
            "id": "all", "priority": 1, "status": "enabled",
            "filter": { "prefix": null, "tags": [] },
            "destination_bucket": "arn:aws:s3:::backup",
            "storage_class": "STANDARD_IA",
            "delete_marker_replication": true
        }]
    });
    assert_eq!(h.ok("get_bucket_replication", site.clone()), Value::Null);
    let (code, message) = h.error(
        "put_bucket_replication",
        with(site.clone(), "config", replication.clone()),
    );
    assert_eq!(code, "usage");
    assert!(
        message.contains("versioning of 'site' is not enabled; enable versioning"),
        "{message}"
    );
    server.set_versioning("Enabled");
    h.ok(
        "put_bucket_replication",
        with(site.clone(), "config", replication.clone()),
    );
    assert_eq!(h.ok("get_bucket_replication", site.clone()), replication);
    h.ok("delete_bucket_replication", site.clone());
    assert_eq!(h.ok("get_bucket_replication", site), Value::Null);
}

#[test]
fn a_failing_rule_section_never_hides_the_others() {
    let server = RulesMock::start();
    let h = Harness::new();
    let site = on(&account_for(&h, &server));
    server.not_implemented("intelligent-tiering");
    server.deny("replication");

    let rules = h.ok("get_bucket_rules", site.clone());
    assert_eq!(rules["cors"]["type"], "loaded");
    assert_eq!(rules["lifecycle"]["type"], "loaded");
    assert_eq!(rules["tiering"]["type"], "unsupported");
    assert!(
        rules["tiering"]["message"]
            .as_str()
            .unwrap()
            .starts_with("This server does not support Intelligent-Tiering")
    );
    assert_eq!(rules["replication"]["type"], "failed");
    assert_eq!(rules["replication"]["error"]["code"], "auth");
    assert_eq!(h.code("get_bucket_replication", site.clone()), "auth");
    assert_eq!(h.code("list_bucket_tiering", site), "remote");
}

#[test]
fn rules_documents_are_exported_to_and_imported_from_json_files() {
    let h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lifecycle.json");
    let path = path.to_str().unwrap().to_owned();
    let text = serde_json::to_string_pretty(&lifecycle()).unwrap();
    h.ok(
        "write_rules_file",
        json!({ "path": path, "document": text }),
    );
    let back = h.ok("read_rules_file", json!({ "path": path }));
    let back: Value = serde_json::from_str(back.as_str().unwrap()).unwrap();
    assert_eq!(back, lifecycle());

    // Only .json files, only JSON, only absolute paths.
    let other = dir.path().join("lifecycle.txt");
    assert_eq!(
        h.code(
            "write_rules_file",
            json!({ "path": other.to_str().unwrap(), "document": text })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "write_rules_file",
            json!({ "path": path, "document": "not json" })
        ),
        "usage"
    );
    assert_eq!(
        h.code("read_rules_file", json!({ "path": "relative.json" })),
        "usage"
    );
}
