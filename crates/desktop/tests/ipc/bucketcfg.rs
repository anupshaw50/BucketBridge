//! Bucket configuration commands: every section of a bucket round-trips
//! through IPC (read, write, read again), sections fail independently, bad
//! input is rejected before anything is sent, and the external buckets of an
//! account are pinned, merged into the bucket list and removed. S3 is the
//! stateful `BucketConfigMock` (s3s-fs implements none of these APIs).

use bucketbridge_tests::{BucketConfigMock, ConfigKind};
use serde_json::{Value, json};

use crate::harness::*;

const UNKNOWN_ID: &str = "00000000-0000-4000-8000-000000000000";

const POLICY: &str = r#"{
    "Version": "2012-10-17",
    "Statement": [{
        "Sid": "PublicRead",
        "Effect": "Allow",
        "Principal": "*",
        "Action": ["s3:GetObject"],
        "Resource": "arn:aws:s3:::site/*"
    }]
}"#;

const POLICY_PRETTY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "PublicRead",
      "Effect": "Allow",
      "Principal": "*",
      "Action": [
        "s3:GetObject"
      ],
      "Resource": "arn:aws:s3:::site/*"
    }
  ]
}"#;

/// Adds an account that talks to `server` (a custom endpoint) and returns
/// its id.
fn account_for(h: &Harness, server: &BucketConfigMock, label: &str) -> String {
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": label,
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

fn on(id: &str, bucket: &str) -> Value {
    json!({ "account_id": id, "bucket": bucket })
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn loaded(value: Value) -> Value {
    json!({ "type": "loaded", "value": value })
}

#[test]
fn every_section_round_trips_through_ipc() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let site = on(&id, "site");

    // Nothing is configured: every section answers, with nothing.
    let config = h.ok("get_bucket_config", site.clone());
    assert_eq!(
        keys(&config),
        [
            "acceleration",
            "logging",
            "policy",
            "public_access_block",
            "versioning",
            "website"
        ]
    );
    assert_eq!(config["policy"], loaded(Value::Null));
    assert_eq!(config["public_access_block"], loaded(Value::Null));
    assert_eq!(
        config["versioning"],
        loaded(json!({ "status": "unversioned", "mfa_delete": false }))
    );
    assert_eq!(config["logging"], loaded(Value::Null));
    assert_eq!(config["website"], loaded(Value::Null));
    // A custom endpoint has no transfer acceleration.
    assert_eq!(config["acceleration"]["type"], "unsupported");
    assert!(
        config["acceleration"]["message"]
            .as_str()
            .unwrap()
            .contains("AWS S3 only")
    );

    // Policy: written as typed, read back pretty-printed.
    assert_eq!(h.ok("get_bucket_policy", site.clone()), Value::Null);
    assert_eq!(
        h.ok(
            "put_bucket_policy",
            with(site.clone(), "policy", json!(POLICY))
        ),
        Value::Null
    );
    assert_eq!(
        h.ok("get_bucket_policy", site.clone()),
        json!(POLICY_PRETTY)
    );
    let edited = POLICY_PRETTY.replace("Allow", "Deny");
    h.ok(
        "put_bucket_policy",
        with(site.clone(), "policy", json!(edited)),
    );
    assert_eq!(h.ok("get_bucket_policy", site.clone()), json!(edited));

    // Public access block: four toggles.
    assert_eq!(h.ok("get_public_access_block", site.clone()), Value::Null);
    let block = json!({
        "block_public_acls": true, "ignore_public_acls": false,
        "block_public_policy": true, "restrict_public_buckets": false
    });
    h.ok(
        "put_public_access_block",
        with(site.clone(), "config", block.clone()),
    );
    assert_eq!(h.ok("get_public_access_block", site.clone()), block);

    // Versioning: enable, then suspend; MFA delete is only reported.
    h.ok(
        "put_bucket_versioning",
        with(site.clone(), "status", json!("enabled")),
    );
    assert_eq!(
        h.ok("get_bucket_versioning", site.clone()),
        json!({ "status": "enabled", "mfa_delete": false })
    );
    h.ok(
        "put_bucket_versioning",
        with(site.clone(), "status", json!("suspended")),
    );
    assert_eq!(
        h.ok("get_bucket_versioning", site.clone())["status"],
        "suspended"
    );

    // Logging: on, changed, off.
    assert_eq!(h.ok("get_bucket_logging", site.clone()), Value::Null);
    let target = json!({ "target_bucket": "logs", "target_prefix": "site/" });
    h.ok(
        "put_bucket_logging",
        with(site.clone(), "target", target.clone()),
    );
    assert_eq!(h.ok("get_bucket_logging", site.clone()), target);
    let moved = json!({ "target_bucket": "logs", "target_prefix": "" });
    h.ok(
        "put_bucket_logging",
        with(site.clone(), "target", moved.clone()),
    );
    assert_eq!(h.ok("get_bucket_logging", site.clone()), moved);

    // Website: hosting with a routing rule, then a redirect of everything.
    assert_eq!(h.ok("get_bucket_website", site.clone()), Value::Null);
    let hosting = json!({
        "type": "hosting",
        "index_document": "index.html",
        "error_document": "error.html",
        "routing_rules": [{
            "condition": { "key_prefix_equals": "docs/", "http_error_code_returned_equals": null },
            "redirect": {
                "protocol": "https", "host_name": null,
                "replace_key_prefix_with": "documents/", "replace_key_with": null,
                "http_redirect_code": "301"
            }
        }]
    });
    h.ok(
        "put_bucket_website",
        with(site.clone(), "website", hosting.clone()),
    );
    assert_eq!(h.ok("get_bucket_website", site.clone()), hosting);

    // The combined read shows all of it at once.
    let config = h.ok("get_bucket_config", site.clone());
    assert_eq!(config["policy"], loaded(json!(edited)));
    assert_eq!(config["public_access_block"], loaded(block));
    assert_eq!(config["versioning"]["value"]["status"], "suspended");
    assert_eq!(config["logging"], loaded(moved));
    assert_eq!(config["website"], loaded(hosting));

    let redirect = json!({ "type": "redirect", "host_name": "example.com", "protocol": null });
    h.ok(
        "put_bucket_website",
        with(site.clone(), "website", redirect.clone()),
    );
    assert_eq!(h.ok("get_bucket_website", site.clone()), redirect);

    // Everything that can be removed, can be: nothing is left behind.
    assert_eq!(h.ok("disable_bucket_logging", site.clone()), Value::Null);
    assert_eq!(h.ok("get_bucket_logging", site.clone()), Value::Null);
    for (delete, get) in [
        ("delete_bucket_policy", "get_bucket_policy"),
        ("delete_public_access_block", "get_public_access_block"),
        ("delete_bucket_website", "get_bucket_website"),
    ] {
        assert_eq!(h.ok(delete, site.clone()), Value::Null, "{delete}");
        assert_eq!(h.ok(get, site.clone()), Value::Null, "{get}");
        // Removing it again is fine.
        assert_eq!(h.ok(delete, site.clone()), Value::Null, "{delete}");
    }
    for kind in [
        ConfigKind::Policy,
        ConfigKind::PublicAccessBlock,
        ConfigKind::Website,
    ] {
        assert_eq!(server.stored("site", kind), None, "{kind:?}");
    }
}

#[test]
fn transfer_acceleration_is_not_supported_on_a_custom_endpoint() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let (code, message) = h.error("get_bucket_acceleration", on(&id, "site"));
    assert_eq!(code, "usage");
    assert!(message.contains("AWS S3 only"), "{message}");
    assert_eq!(
        h.code(
            "put_bucket_acceleration",
            with(on(&id, "site"), "status", json!("enabled"))
        ),
        "usage"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_failing_section_never_hides_the_others() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let site = on(&id, "site");
    h.ok(
        "put_bucket_versioning",
        with(site.clone(), "status", json!("enabled")),
    );
    server.deny("site", ConfigKind::Policy);
    server.not_implemented(ConfigKind::Website);

    // The call itself succeeds; the sections say what happened to each.
    let config = h.ok("get_bucket_config", site.clone());
    assert_eq!(config["policy"]["type"], "failed");
    assert_eq!(config["policy"]["error"]["code"], "auth");
    assert!(
        config["policy"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("AccessDenied")
    );
    assert_eq!(config["website"]["type"], "unsupported");
    assert!(
        config["website"]["message"]
            .as_str()
            .unwrap()
            .starts_with("This server does not support static website hosting")
    );
    assert_eq!(config["versioning"]["type"], "loaded");
    assert_eq!(config["versioning"]["value"]["status"], "enabled");
    assert_eq!(config["public_access_block"], loaded(Value::Null));

    // The single reads reject with the same codes, as BbError objects.
    assert_eq!(h.code("get_bucket_policy", site.clone()), "auth");
    assert_eq!(
        h.code(
            "put_bucket_policy",
            with(site.clone(), "policy", json!(POLICY))
        ),
        "auth"
    );
    assert_eq!(h.code("get_bucket_website", site.clone()), "remote");

    // A bucket that does not exist fails every section as not found, which
    // is not mistaken for "nothing configured".
    let config = h.ok("get_bucket_config", on(&id, "missing"));
    for section in [
        "policy",
        "public_access_block",
        "versioning",
        "logging",
        "website",
    ] {
        assert_eq!(config[section]["type"], "failed", "{section}");
        assert_eq!(config[section]["error"]["code"], "not_found", "{section}");
    }
    assert_eq!(h.code("get_bucket_policy", on(&id, "missing")), "not_found");
}

#[test]
fn invalid_input_is_rejected_before_anything_is_sent() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let site = on(&id, "site");

    let (code, message) = h.error(
        "put_bucket_policy",
        with(
            site.clone(),
            "policy",
            json!(r#"{"Version":"2012-10-17","Statement":[{"Effect":"allow"}]}"#),
        ),
    );
    assert_eq!(code, "usage");
    assert!(message.starts_with("invalid bucket policy: "), "{message}");
    assert!(
        message.contains("Statement[0].Effect: must be \"Allow\" or \"Deny\" (found \"allow\")"),
        "{message}"
    );
    assert!(
        message.contains("needs \"Principal\" or \"NotPrincipal\""),
        "{message}"
    );
    assert_eq!(
        h.code(
            "put_bucket_policy",
            with(site.clone(), "policy", json!("{"))
        ),
        "usage"
    );
    let (code, message) = h.error(
        "put_bucket_website",
        with(
            site.clone(),
            "website",
            json!({ "type": "hosting", "index_document": "a/b.html",
                    "error_document": null, "routing_rules": [] }),
        ),
    );
    assert_eq!(code, "usage");
    assert!(message.contains("must not contain a slash"), "{message}");
    assert_eq!(
        h.code(
            "put_bucket_versioning",
            with(site.clone(), "status", json!("unversioned"))
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "put_bucket_logging",
            with(
                site.clone(),
                "target",
                json!({ "target_bucket": "bad/target", "target_prefix": "" })
            )
        ),
        "usage"
    );
    for command in [
        "get_bucket_policy",
        "get_bucket_config",
        "get_public_access_block",
        "delete_bucket_website",
    ] {
        assert_eq!(
            h.code(command, on(&id, "Not A Valid Name")),
            "usage",
            "{command}"
        );
    }
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}

#[test]
fn policies_can_be_validated_and_formatted_without_an_account() {
    let h = Harness::new();
    assert_eq!(
        h.ok("validate_bucket_policy", json!({ "policy": POLICY })),
        json!([])
    );
    let issues = h.ok(
        "validate_bucket_policy",
        json!({ "policy": r#"{"Version": "1999-01-01", "Statement": [{"Effect": "Allow", "Principal": "*", "Action": [], "Resource": "*"}]}"# }),
    );
    let issues = issues.as_array().unwrap();
    assert_eq!(issues.len(), 2, "{issues:?}");
    assert_eq!(keys(&issues[0]), ["message", "path"]);
    assert_eq!(issues[0]["path"], "Version");
    assert_eq!(
        issues[0]["message"],
        "must be \"2012-10-17\" or \"2008-10-17\" (found \"1999-01-01\")"
    );
    assert_eq!(issues[1]["path"], "Statement[0].Action");
    assert_eq!(issues[1]["message"], "must not be an empty list");
    // A syntax error is one issue, with the position.
    let issues = h.ok(
        "validate_bucket_policy",
        json!({ "policy": "{\n  \"a\": }" }),
    );
    assert_eq!(issues.as_array().unwrap().len(), 1);
    assert_eq!(issues[0]["path"], "");
    assert!(
        issues[0]["message"]
            .as_str()
            .unwrap()
            .contains("line 2 column")
    );

    assert_eq!(
        h.ok("format_bucket_policy", json!({ "policy": POLICY })),
        json!(POLICY_PRETTY)
    );
    // Formatting needs JSON only, so a policy under construction can be tidied.
    assert_eq!(
        h.ok(
            "format_bucket_policy",
            json!({ "policy": "{\"Version\":\"2012-10-17\"}" })
        ),
        json!("{\n  \"Version\": \"2012-10-17\"\n}")
    );
    let (code, message) = h.error("format_bucket_policy", json!({ "policy": "{" }));
    assert_eq!(code, "usage");
    assert!(message.starts_with("not valid JSON"), "{message}");
}

#[test]
fn account_problems_reject_every_account_bound_command() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let calls = |account: &str| -> Vec<(&'static str, Value)> {
        let site = on(account, "site");
        vec![
            ("get_bucket_config", site.clone()),
            ("get_bucket_policy", site.clone()),
            (
                "put_bucket_policy",
                with(site.clone(), "policy", json!(POLICY)),
            ),
            ("delete_bucket_policy", site.clone()),
            ("get_public_access_block", site.clone()),
            (
                "put_public_access_block",
                with(
                    site.clone(),
                    "config",
                    json!({ "block_public_acls": true, "ignore_public_acls": true,
                            "block_public_policy": true, "restrict_public_buckets": true }),
                ),
            ),
            ("delete_public_access_block", site.clone()),
            ("get_bucket_versioning", site.clone()),
            (
                "put_bucket_versioning",
                with(site.clone(), "status", json!("enabled")),
            ),
            ("get_bucket_logging", site.clone()),
            (
                "put_bucket_logging",
                with(
                    site.clone(),
                    "target",
                    json!({ "target_bucket": "logs", "target_prefix": "" }),
                ),
            ),
            ("disable_bucket_logging", site.clone()),
            ("get_bucket_website", site.clone()),
            (
                "put_bucket_website",
                with(
                    site.clone(),
                    "website",
                    json!({ "type": "redirect", "host_name": "example.com", "protocol": null }),
                ),
            ),
            ("delete_bucket_website", site.clone()),
            ("get_bucket_acceleration", site.clone()),
            (
                "put_bucket_acceleration",
                with(site, "status", json!("enabled")),
            ),
            (
                "add_external_bucket",
                json!({ "account_id": account, "name": "site" }),
            ),
        ]
    };

    // An unknown account.
    for (command, args) in calls(UNKNOWN_ID) {
        assert_eq!(h.code(command, args), "not_found", "{command}");
    }
    // Locked secrets.
    h.ok(
        "set_master_password",
        json!({ "current": null, "new_password": "correct horse battery" }),
    );
    h.ok("lock", json!({}));
    for (command, args) in calls(&id) {
        assert_eq!(h.code(command, args), "locked", "{command}");
    }
    h.ok("unlock", json!({ "password": "correct horse battery" }));
    // An MFA device without a session.
    let mut input = json!({
        "label": "MFA", "access_key_id": "AKIDMOCK", "secret_access_key": "mock-secret",
        "session_token": null, "region": "us-east-1", "endpoint": server.endpoint(),
        "path_style": true, "request_payer": false,
        "mfa_serial": "arn:aws:iam::123456789012:mfa/example-device"
    });
    input["label"] = json!("MFA");
    let mfa = h.ok("add_account", json!({ "input": input }));
    for (command, args) in calls(mfa["id"].as_str().unwrap()) {
        assert_eq!(h.code(command, args), "mfa_required", "{command}");
    }
    assert!(server.requests().is_empty(), "{:?}", server.requests());
}

#[test]
fn external_buckets_are_pinned_merged_into_the_list_and_removed() {
    let server = BucketConfigMock::with_buckets(&["mine", "shared"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    let list = |account: &str| h.ok("list_buckets", json!({ "account_id": account }));
    let names = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|bucket| bucket["name"].as_str().unwrap().to_owned())
            .collect()
    };

    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!([])
    );
    // The account cannot list its buckets: that is what pinning is for.
    server.deny_listing();
    assert_eq!(h.code("list_buckets", json!({ "account_id": id })), "auth");

    // A pinned bucket must exist; being allowed to list it is not required.
    assert_eq!(
        h.code(
            "add_external_bucket",
            json!({ "account_id": id, "name": "imaginary" })
        ),
        "not_found"
    );
    assert_eq!(
        h.code(
            "add_external_bucket",
            json!({ "account_id": id, "name": "bad name" })
        ),
        "usage"
    );
    assert_eq!(
        h.ok(
            "add_external_bucket",
            json!({ "account_id": id, "name": "shared" })
        ),
        Value::Null
    );
    // Pinning it again changes nothing.
    h.ok(
        "add_external_bucket",
        json!({ "account_id": id, "name": "shared" }),
    );
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!(["shared"])
    );
    // Listing is refused, so the pinned bucket is all there is.
    assert_eq!(
        list(&id),
        json!([{
            "name": "shared", "region": null, "creation_date": null,
            "owner": null, "kind": "external"
        }])
    );

    // When listing works, pinned buckets are added to what the server says.
    let server_ok = BucketConfigMock::with_buckets(&["mine", "shared"]);
    let other = account_for(&h, &server_ok, "Other");
    h.ok(
        "add_external_bucket",
        json!({ "account_id": other, "name": "shared" }),
    );
    h.ok(
        "add_external_bucket",
        json!({ "account_id": other, "name": "mine" }),
    );
    let merged = list(&other);
    assert_eq!(names(&merged), ["mine", "shared"]);
    // A bucket that is listed keeps its listed entry (its date), and does not appear twice.
    assert_ne!(merged[0]["creation_date"], Value::Null);
    // Pins belong to their account.
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!(["shared"])
    );
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": other })),
        json!(["mine", "shared"])
    );

    // Pins are in config.toml, so they survive a restart.
    let h = h.restart();
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!(["shared"])
    );

    // Removing a pin leaves the bucket itself alone.
    assert_eq!(
        h.ok(
            "remove_external_bucket",
            json!({ "account_id": id, "name": "shared" })
        ),
        Value::Null
    );
    assert_eq!(
        h.code(
            "remove_external_bucket",
            json!({ "account_id": id, "name": "shared" })
        ),
        "not_found"
    );
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!([])
    );
    assert_eq!(h.code("list_buckets", json!({ "account_id": id })), "auth");
    assert!(server.stored("shared", ConfigKind::Policy).is_none());

    // Removing an account takes its pins with it.
    h.ok("remove_account", json!({ "id": other }));
    assert_eq!(
        h.code("list_external_buckets", json!({ "account_id": other })),
        "not_found"
    );
    let file = std::fs::read_to_string(h.core.dirs().config_file()).unwrap();
    assert!(!file.contains("pinned_buckets"), "{file}");
}

#[test]
fn external_buckets_can_be_listed_and_removed_while_the_secrets_are_locked() {
    let server = BucketConfigMock::with_buckets(&["shared"]);
    let h = Harness::new();
    let id = account_for(&h, &server, "Main");
    h.ok(
        "add_external_bucket",
        json!({ "account_id": id, "name": "shared" }),
    );
    h.ok(
        "set_master_password",
        json!({ "current": null, "new_password": "correct horse battery" }),
    );
    h.ok("lock", json!({}));
    // They live in the configuration file, which needs no secret ...
    assert_eq!(
        h.ok("list_external_buckets", json!({ "account_id": id })),
        json!(["shared"])
    );
    // ... but checking that a bucket exists, and listing buckets, do.
    assert_eq!(
        h.code(
            "add_external_bucket",
            json!({ "account_id": id, "name": "shared" })
        ),
        "locked"
    );
    assert_eq!(
        h.code("list_buckets", json!({ "account_id": id })),
        "locked"
    );
    assert_eq!(
        h.ok(
            "remove_external_bucket",
            json!({ "account_id": id, "name": "shared" })
        ),
        Value::Null
    );
    assert_eq!(
        h.code("list_external_buckets", json!({ "account_id": UNKNOWN_ID })),
        "not_found"
    );
}
