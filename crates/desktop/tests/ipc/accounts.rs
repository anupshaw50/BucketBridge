//! Security and account commands, `security://changed`, and the startup
//! failure path.

use bucketbridge_desktop::{AppState, SECURITY_CHANGED};
use serde_json::{Value, json};

use crate::harness::*;

const UNKNOWN_ID: &str = "00000000-0000-4000-8000-000000000000";

#[test]
fn accounts_lifecycle() {
    let server = start_s3();
    let h = Harness::new();

    assert_eq!(
        h.ok("list_accounts", json!({})),
        json!({ "accounts": [], "default_account_id": null })
    );

    let account = h.ok(
        "add_account",
        json!({ "input": account_input(&server, "Work") }),
    );
    assert_eq!(
        keys(&account),
        [
            "access_key_id",
            "endpoint",
            "id",
            "label",
            "mfa_serial",
            "path_style",
            "region",
            "request_payer",
            "secret_ref",
            "session_token_ref",
            "source",
        ]
    );
    assert_eq!(account["source"], json!({ "type": "static_keys" }));
    assert_eq!(account["label"], "Work");
    assert_eq!(account["endpoint"], server.endpoint());
    assert_eq!(account["path_style"], true);
    assert_eq!(account["session_token_ref"], Value::Null);
    let secret_ref = account["secret_ref"].as_str().unwrap();
    assert!(!secret_ref.contains(server.secret_key()));
    let id = account["id"].as_str().unwrap().to_owned();

    // The first account becomes the default.
    let list = h.ok("list_accounts", json!({}));
    assert_eq!(list["default_account_id"], id.as_str());
    assert_eq!(list["accounts"].as_array().unwrap().len(), 1);

    // Labels are unique, ignoring case; an empty label is invalid.
    assert_eq!(
        h.code(
            "add_account",
            json!({ "input": account_input(&server, "work") })
        ),
        "conflict"
    );
    assert_eq!(
        h.code(
            "add_account",
            json!({ "input": account_input(&server, "") })
        ),
        "usage"
    );

    let mut update = no_change();
    update["label"] = json!("Work 2");
    let updated = h.ok("update_account", json!({ "id": id, "update": update }));
    assert_eq!(updated["label"], "Work 2");
    assert_eq!(updated["id"], id.as_str());

    let second = h.add_account(&server, "Second");
    assert_eq!(
        h.ok("set_default_account", json!({ "id": second })),
        Value::Null
    );
    assert_eq!(
        h.ok("list_accounts", json!({}))["default_account_id"],
        second.as_str()
    );

    for (command, args) in [
        ("remove_account", json!({ "id": UNKNOWN_ID })),
        ("set_default_account", json!({ "id": UNKNOWN_ID })),
        (
            "update_account",
            json!({ "id": UNKNOWN_ID, "update": no_change() }),
        ),
        ("list_buckets", json!({ "account_id": UNKNOWN_ID })),
    ] {
        assert_eq!(h.code(command, args), "not_found", "{command}");
    }

    assert_eq!(h.ok("remove_account", json!({ "id": second })), Value::Null);
    assert_eq!(h.ok("remove_account", json!({ "id": id })), Value::Null);
    assert_eq!(
        h.ok("list_accounts", json!({})),
        json!({ "accounts": [], "default_account_id": null })
    );
}

#[test]
fn master_password_lock_and_unlock() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let changes = h.events(SECURITY_CHANGED);

    assert_eq!(
        h.ok("security_status", json!({})),
        json!({ "master_password_set": false, "unlocked": true })
    );
    assert_eq!(
        h.ok(
            "set_master_password",
            json!({ "current": null, "new_password": "correct horse battery" })
        ),
        Value::Null
    );
    assert_eq!(
        next(&changes),
        json!({ "master_password_set": true, "unlocked": true })
    );

    // Changing it requires the current password.
    assert_eq!(
        h.code(
            "set_master_password",
            json!({ "current": null, "new_password": "other" })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "set_master_password",
            json!({ "current": "wrong", "new_password": "other" })
        ),
        "auth"
    );

    assert_eq!(h.ok("lock", json!({})), Value::Null);
    assert_eq!(
        next(&changes),
        json!({ "master_password_set": true, "unlocked": false })
    );
    assert_eq!(
        h.ok("security_status", json!({})),
        json!({ "master_password_set": true, "unlocked": false })
    );
    // Account-bound commands are locked; listing accounts still works.
    assert_eq!(
        h.code("list_buckets", json!({ "account_id": id })),
        "locked"
    );
    assert_eq!(
        h.code(
            "presign",
            json!({
                "account_id": id, "bucket": "b", "key": "k",
                "method": "get", "expires_secs": 60
            })
        ),
        "locked"
    );
    assert_eq!(
        h.ok("list_accounts", json!({}))["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A wrong password changes nothing.
    assert_eq!(h.code("unlock", json!({ "password": "wrong" })), "auth");
    assert_eq!(
        h.code("list_buckets", json!({ "account_id": id })),
        "locked"
    );

    assert_eq!(
        h.ok("unlock", json!({ "password": "correct horse battery" })),
        Value::Null
    );
    assert_eq!(
        next(&changes),
        json!({ "master_password_set": true, "unlocked": true })
    );
    assert_eq!(h.ok("list_buckets", json!({ "account_id": id })), json!([]));

    // Removing the master password.
    h.ok(
        "set_master_password",
        json!({ "current": "correct horse battery", "new_password": null }),
    );
    assert_eq!(
        next(&changes),
        json!({ "master_password_set": false, "unlocked": true })
    );
    assert_eq!(
        h.code(
            "set_master_password",
            json!({ "current": null, "new_password": null })
        ),
        "usage"
    );
}

#[test]
fn accounts_with_an_mfa_serial_need_an_mfa_session() {
    let server = start_s3();
    let h = Harness::new();
    let mut input = account_input(&server, "MFA");
    input["mfa_serial"] = json!("arn:aws:iam::123456789012:mfa/example-device");
    let account = h.ok("add_account", json!({ "input": input }));
    let id = account["id"].as_str().unwrap();
    assert_eq!(
        h.code("list_buckets", json!({ "account_id": id })),
        "mfa_required"
    );
    assert_eq!(
        h.code(
            "list_objects",
            json!({
                "account_id": id, "bucket": "b", "prefix": "",
                "flat": false, "token": null
            })
        ),
        "mfa_required"
    );
}

#[test]
fn a_failed_startup_is_reported_by_every_command() {
    let app = mock_app(AppState::failed(startup_error(
        "invalid configuration file /x/config.toml: expected a table",
    )));
    let view = webview(&app);
    for (command, args) in [
        ("security_status", json!({})),
        ("list_accounts", json!({})),
        ("get_settings", json!({})),
        (
            "get_log",
            json!({ "levels": null, "text": null, "limit": null }),
        ),
        ("app_info", json!({})),
        ("sync_get", json!({ "plan_id": UNKNOWN_ID })),
        ("list_tasks", json!({})),
        ("start_all", json!({})),
        ("clear_finished", json!({})),
        ("pause_task", json!({ "id": UNKNOWN_ID })),
        (
            "enqueue_delete",
            json!({ "account_id": UNKNOWN_ID, "bucket": "bkt", "keys": ["k"] }),
        ),
    ] {
        let (code, message) = bb_error(command, invoke(&view, command, args));
        assert_eq!(code, "io", "{command}");
        assert!(message.contains("config.toml"), "{command}: {message}");
    }
}
