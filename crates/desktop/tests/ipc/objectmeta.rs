//! The object metadata commands (docs/desktop-api.md, "Object metadata
//! (T06)"), invoked with the JSON the UI sends. S3 is the in-memory
//! `ObjectStore` of `bucketbridge-tests`, which answers like S3 does.

use bucketbridge_tests::{MockS3, ObjectStore, StoredEncryption, StoredObject};
use serde_json::{Value, json};

use crate::harness::*;

const BUCKET: &str = "bucket";
/// 32 bytes of 0x11 and 0x22.
const KEY_A_B64: &str = "ERERERERERERERERERERERERERERERERERERERERERE=";
const KEY_B_HEX: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const KEY_A_MD5: &str = "YSbdkQDUu1vJkAgt83fvFg==";

struct Fixture {
    h: Harness,
    store: std::sync::Arc<ObjectStore>,
    // Keeps the endpoint alive.
    _mock: MockS3,
    account: String,
}

fn fixture() -> Fixture {
    let store = ObjectStore::new();
    let mock = store.start();
    let h = Harness::new();
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Mock",
            "access_key_id": "AKIDMOCK",
            "secret_access_key": "mock-secret",
            "session_token": null,
            "region": "us-east-1",
            "endpoint": mock.endpoint(),
            "path_style": true,
            "request_payer": false,
            "mfa_serial": null,
        }}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    Fixture {
        h,
        store,
        _mock: mock,
        account,
    }
}

fn names(results: &Value) -> Vec<(String, bool, Option<String>)> {
    results
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_owned(),
                r["changed"].as_bool().unwrap(),
                r["error"]["code"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn details_have_the_contract_shape() {
    let f = fixture();
    f.store.put(
        BUCKET,
        "dir/a.csv",
        StoredObject::new(42)
            .content_type("text/csv")
            .class("STANDARD_IA")
            .meta("owner", "team")
            .tag("env", "prod")
            .public_read(),
    );
    let details = f.h.ok(
        "get_object_details",
        json!({ "account_id": f.account, "bucket": BUCKET, "key": "dir/a.csv", "sse_c_key": null }),
    );
    assert_eq!(
        keys(&details),
        [
            "acl",
            "acl_error",
            "archive_status",
            "checksums",
            "etag",
            "key",
            "last_modified",
            "meta",
            "restore",
            "size",
            "storage_class",
            "tags_error",
            "version_id"
        ]
    );
    assert_eq!(details["key"], "dir/a.csv");
    assert_eq!(details["size"], 42);
    assert_eq!(details["storage_class"], "STANDARD_IA");
    assert_eq!(details["meta"]["content_type"], "text/csv");
    assert_eq!(details["meta"]["user_meta"], json!({ "owner": "team" }));
    assert_eq!(details["meta"]["tags"], json!({ "env": "prod" }));
    assert_eq!(details["meta"]["acl"], "public-read");
    assert_eq!(details["meta"]["sse"], json!({ "type": "none" }));
    assert_eq!(keys(&details["acl"]), ["canned", "grants", "owner"]);
    assert_eq!(details["acl"]["canned"], "public-read");
    assert_eq!(
        keys(&details["acl"]["grants"][0]),
        ["grantee", "grantee_type", "permission"]
    );
    assert!(details["tags_error"].is_null() && details["acl_error"].is_null());

    assert_eq!(
        f.h.code(
            "get_object_details",
            json!({ "account_id": f.account, "bucket": BUCKET, "key": "missing", "sse_c_key": null })
        ),
        "not_found"
    );
}

#[test]
fn a_customer_key_is_needed_and_a_wrong_one_fails_loudly() {
    let f = fixture();
    f.store.put(
        BUCKET,
        "secret",
        StoredObject::new(1).encrypted_with_key(&[0x11; 32]),
    );
    let call = |key: Value| {
        f.h.invoke(
            "get_object_details",
            json!({ "account_id": f.account, "bucket": BUCKET, "key": "secret", "sse_c_key": key }),
        )
    };

    let (code, message) = bb_error("get_object_details", call(Value::Null));
    assert_eq!(code, "remote");
    assert!(message.contains("SSE-C"), "{message}");

    let (code, message) = bb_error("get_object_details", call(json!(KEY_B_HEX)));
    assert_eq!(code, "auth", "{message}");
    assert!(
        message.contains("SSE-C") && message.contains("refused"),
        "{message}"
    );
    assert!(
        !message.contains(KEY_B_HEX),
        "the key never comes back: {message}"
    );

    // A malformed key is a usage error that does not echo it.
    let (code, message) = bb_error("get_object_details", call(json!("hunter2-not-a-key")));
    assert_eq!(code, "usage");
    assert!(!message.contains("hunter2"), "{message}");

    let details = call(json!(KEY_A_B64)).unwrap();
    assert_eq!(
        details["meta"]["sse"],
        json!({ "type": "c", "key_md5": KEY_A_MD5 })
    );
    // Hex works too.
    assert!(call(json!("11".repeat(32))).is_ok());
}

#[test]
fn tags_are_edited_for_many_objects_with_per_key_results() {
    let f = fixture();
    f.store.put(
        BUCKET,
        "a",
        StoredObject::new(1).tag("env", "dev").tag("temp", "x"),
    );
    f.store
        .put(BUCKET, "b", StoredObject::new(1).tag("env", "prod"));
    let results = f.h.ok(
        "update_object_tags",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["a", "b", "missing"],
            "edit": { "mode": "merge", "set": { "env": "prod" }, "remove": ["temp"] }
        }),
    );
    assert_eq!(
        keys(&results[0]),
        ["changed", "error", "key", "previous", "warnings"]
    );
    assert_eq!(
        names(&results),
        [
            ("a".into(), true, None),
            ("b".into(), false, None),
            ("missing".into(), false, Some("not_found".into()))
        ]
    );
    assert_eq!(
        f.store.object(BUCKET, "a").unwrap().tags,
        [("env".to_owned(), "prod".to_owned())]
            .into_iter()
            .collect()
    );

    let results = f.h.ok(
        "update_object_tags",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["a"],
            "edit": { "mode": "replace", "tags": {} }
        }),
    );
    assert_eq!(names(&results), [("a".into(), true, None)]);
    assert!(f.store.object(BUCKET, "a").unwrap().tags.is_empty());

    // S3's rules are checked first; nothing is written.
    assert_eq!(
        f.h.code(
            "update_object_tags",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": ["b"],
                "edit": { "mode": "replace", "tags": { "k": "semi;colon" } }
            })
        ),
        "usage"
    );
    assert_eq!(
        f.h.code(
            "update_object_tags",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": [],
                "edit": { "mode": "replace", "tags": {} }
            })
        ),
        "usage"
    );
}

#[test]
fn the_storage_class_changes_and_archived_objects_are_a_conflict() {
    let f = fixture();
    f.store
        .put(BUCKET, "std", StoredObject::new(1).tag("k", "v"));
    f.store
        .put(BUCKET, "cold", StoredObject::new(1).class("GLACIER"));
    let results = f.h.ok(
        "set_object_storage_class",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["std", "cold"],
            "storage_class": "STANDARD_IA", "sse_c_key": null
        }),
    );
    assert_eq!(
        names(&results),
        [
            ("std".into(), true, None),
            ("cold".into(), false, Some("conflict".into()))
        ]
    );
    assert_eq!(results[0]["previous"], "STANDARD");
    assert!(
        results[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("restore"),
        "{results}"
    );
    let after = f.store.object(BUCKET, "std").unwrap();
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(after.tags.len(), 1, "tags are kept");

    let (code, message) = f.h.error(
        "set_object_storage_class",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["std"],
            "storage_class": "COLD", "sse_c_key": null
        }),
    );
    assert_eq!(code, "usage");
    assert!(message.contains("DEEP_ARCHIVE"), "{message}");
}

#[test]
fn the_encryption_changes_and_a_wrong_source_key_changes_nothing() {
    let f = fixture();
    f.store
        .put(BUCKET, "plain", StoredObject::new(1).class("STANDARD_IA"));
    f.store.put(
        BUCKET,
        "secret",
        StoredObject::new(1).encrypted_with_key(&[0x11; 32]),
    );
    let run = |key: &str, encryption: Value, source: Value| {
        f.h.ok(
            "set_object_encryption",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": [key],
                "encryption": encryption, "source_sse_c_key": source
            }),
        )
    };

    let results = run("plain", json!({ "type": "s3" }), Value::Null);
    assert_eq!(names(&results), [("plain".into(), true, None)]);
    assert_eq!(results[0]["previous"], "none");
    assert_eq!(
        f.store.object(BUCKET, "plain").unwrap().encryption,
        StoredEncryption::S3
    );
    let results = run(
        "plain",
        json!({ "type": "kms", "key_id": "alias/data", "bucket_key": true }),
        Value::Null,
    );
    assert_eq!(results[0]["previous"], "s3");
    assert_eq!(
        f.store
            .object(BUCKET, "plain")
            .unwrap()
            .storage_class
            .as_deref(),
        Some("STANDARD_IA"),
        "the class is kept"
    );

    // Wrong source key: that object's error, nothing changes.
    let results = run("secret", json!({ "type": "s3" }), json!(KEY_B_HEX));
    assert_eq!(
        names(&results),
        [("secret".into(), false, Some("auth".into()))]
    );
    assert!(
        results[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("SSE-C")
    );
    assert_eq!(
        f.store.object(BUCKET, "secret").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: KEY_A_MD5.into()
        }
    );
    // The right key moves it to a new customer key.
    let results = run(
        "secret",
        json!({ "type": "c", "key": KEY_B_HEX }),
        json!(KEY_A_B64),
    );
    assert_eq!(names(&results), [("secret".into(), true, None)]);
    assert_ne!(
        f.store.object(BUCKET, "secret").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: KEY_A_MD5.into()
        }
    );

    let (code, message) = f.h.error(
        "set_object_encryption",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["plain"],
            "encryption": { "type": "c", "key": "not a key" }, "source_sse_c_key": null
        }),
    );
    assert_eq!(code, "usage");
    assert!(!message.contains("not a key"), "{message}");
}

#[test]
fn headers_and_acls_are_edited_and_disabled_acls_are_a_conflict() {
    let f = fixture();
    f.store.put(
        BUCKET,
        "k",
        StoredObject::new(1)
            .content_type("text/plain")
            .meta("owner", "team")
            .tag("t", "1"),
    );
    let results = f.h.ok(
        "update_object_headers",
        json!({
            "account_id": f.account, "bucket": BUCKET, "keys": ["k"], "sse_c_key": null,
            "changes": {
                "content_type": { "action": "set", "value": "text/html" },
                "cache_control": { "action": "set", "value": "max-age=60" },
                "content_disposition": { "action": "keep" },
                "content_encoding": { "action": "remove" },
                "user_meta_set": { "stage": "done" },
                "user_meta_remove": ["owner"]
            }
        }),
    );
    assert_eq!(names(&results), [("k".into(), true, None)]);
    let after = f.store.object(BUCKET, "k").unwrap();
    assert_eq!(after.content_type.as_deref(), Some("text/html"));
    assert_eq!(after.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(after.user_meta.len(), 1);
    assert_eq!(after.user_meta["stage"], "done");
    assert_eq!(after.tags.len(), 1, "tags are kept");
    assert_eq!(
        f.h.code(
            "update_object_headers",
            json!({
                "account_id": f.account, "bucket": BUCKET, "keys": ["k"], "sse_c_key": null,
                "changes": {
                    "content_type": { "action": "set", "value": "no good\nvalue" },
                    "cache_control": { "action": "keep" },
                    "content_disposition": { "action": "keep" },
                    "content_encoding": { "action": "keep" },
                    "user_meta_set": {}, "user_meta_remove": []
                }
            })
        ),
        "usage"
    );

    let results = f.h.ok(
        "set_object_acl",
        json!({ "account_id": f.account, "bucket": BUCKET, "keys": ["k"], "acl": "public-read" }),
    );
    assert_eq!(names(&results), [("k".into(), true, None)]);
    assert_eq!(results[0]["previous"], "private");
    assert_eq!(
        f.h.code(
            "set_object_acl",
            json!({ "account_id": f.account, "bucket": BUCKET, "keys": ["k"], "acl": "world-readable" })
        ),
        "usage"
    );

    f.store.disable_acls(BUCKET);
    f.store.put(BUCKET, "j", StoredObject::new(1));
    let results = f.h.ok(
        "set_object_acl",
        json!({ "account_id": f.account, "bucket": BUCKET, "keys": ["j"], "acl": "public-read" }),
    );
    assert_eq!(
        names(&results),
        [("j".into(), false, Some("conflict".into()))]
    );
    assert!(
        results[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ACLs are disabled"),
        "{results}"
    );
}

#[test]
fn an_unknown_account_is_not_found() {
    let f = fixture();
    for (command, args) in [
        (
            "get_object_details",
            json!({ "account_id": "nope", "bucket": BUCKET, "key": "k", "sse_c_key": null }),
        ),
        (
            "set_object_acl",
            json!({ "account_id": "nope", "bucket": BUCKET, "keys": ["k"], "acl": "private" }),
        ),
    ] {
        assert_eq!(f.h.code(command, args), "not_found", "{command}");
    }
}
