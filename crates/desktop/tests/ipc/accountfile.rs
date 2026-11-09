//! Account folders, order, export and import commands.

use serde_json::json;

use crate::harness::*;

#[test]
fn layout_export_and_import() {
    let server = start_s3();
    let h = Harness::new();
    let a = h.add_account(&server, "A");
    let mut second = account_input(&server, "B");
    second["access_key_id"] = json!("AKIDSECOND");
    let b = h.ok("add_account", json!({ "input": second }))["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A layout: B in a folder, A after it.
    let list = h.ok(
        "set_account_layout",
        json!({ "layout": {
            "folders": [{ "name": "Work", "account_ids": [b] }],
            "accounts": [b, a],
        } }),
    );
    assert_eq!(list["accounts"][0]["id"], b.as_str());
    assert_eq!(list["folders"][0]["name"], "Work");
    assert_eq!(list["folders"][0]["account_ids"], json!([b]));
    assert_eq!(h.ok("list_accounts", json!({})), list);

    // Not the stored accounts: conflict, nothing changes.
    assert_eq!(
        h.code(
            "set_account_layout",
            json!({ "layout": { "folders": [], "accounts": [a] } })
        ),
        "conflict"
    );
    assert_eq!(h.ok("list_accounts", json!({})), list);

    // Export without secrets.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("accounts.json");
    let path = file.to_str().unwrap();
    let outcome = h.ok(
        "export_accounts",
        json!({ "path": path, "account_ids": null }),
    );
    assert_eq!(outcome["accounts"], 2);
    assert_eq!(outcome["folders"], 1);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(server.secret_key()));
    assert!(!text.contains("secret_ref"));
    assert_eq!(
        h.code(
            "export_accounts",
            json!({ "path": "relative.json", "account_ids": null })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "export_accounts",
            json!({ "path": path, "account_ids": ["00000000-0000-4000-8000-000000000000"] })
        ),
        "not_found"
    );

    // Importing the same file into the same app: duplicates, nothing added.
    let report = h.ok(
        "import_accounts",
        json!({ "path": path, "duplicates": "skip" }),
    );
    assert_eq!(report["imported"], json!([]));
    assert_eq!(report["duplicates"].as_array().unwrap().len(), 2);
    assert_eq!(
        h.ok("list_accounts", json!({}))["accounts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Into a fresh app: both arrive and need a secret, which a rotation supplies.
    let other = Harness::new();
    let report = other.ok(
        "import_accounts",
        json!({ "path": path, "duplicates": "skip" }),
    );
    assert_eq!(report["imported"].as_array().unwrap().len(), 2);
    assert_eq!(report["imported"][0]["needs_secret"], true);
    assert_eq!(report["folders_created"], json!(["Work"]));
    let imported = other.ok("list_accounts", json!({}));
    let id = imported["accounts"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(imported["accounts"][0]["secret_ref"], "");
    let bucket = block_on(server.new_bucket());
    assert_eq!(
        other.code("list_objects", json!({ "account_id": id, "bucket": bucket, "prefix": "", "flat": false, "page_size": 10, "token": null })),
        "usage"
    );
    let mut update = no_change();
    update["secret_access_key"] = json!(server.secret_key());
    other.ok("update_account", json!({ "id": id, "update": update }));
    assert_ne!(
        other.ok("list_accounts", json!({}))["accounts"][0]["secret_ref"],
        ""
    );

    // Not an account file.
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{}").unwrap();
    assert_eq!(
        other.code(
            "import_accounts",
            json!({ "path": bad.to_str().unwrap(), "duplicates": "skip" })
        ),
        "usage"
    );
}
