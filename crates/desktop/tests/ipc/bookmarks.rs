//! Bookmark commands: add, list, rename, remove, errors, persistence, the
//! `bookmarks://changed` event and the removal of an account's bookmarks.

use serde_json::{Value, json};

use crate::harness::*;

fn add(h: &Harness, account: &str, bucket: &str, prefix: &str, label: Value) -> Value {
    h.ok(
        "add_bookmark",
        json!({ "account_id": account, "bucket": bucket, "prefix": prefix, "label": label }),
    )
}

#[test]
fn bookmarks_are_added_renamed_removed_and_persisted() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let changed = h.events("bookmarks://changed");
    assert_eq!(h.ok("list_bookmarks", json!({})), json!([]));

    let list = add(&h, &id, "data", "logs/", Value::Null);
    assert_eq!(list[0]["label"], "data/logs/");
    assert_eq!(list[0]["account_id"], id.as_str());
    assert_eq!(list[0]["bucket"], "data");
    assert_eq!(list[0]["prefix"], "logs/");
    assert_eq!(
        keys(&list[0]),
        ["account_id", "bucket", "id", "label", "prefix"]
    );
    assert_eq!(next(&changed), list);

    let list = add(&h, &id, "data", "", json!("  Data  "));
    assert_eq!(list[1]["label"], "Data");
    assert_eq!(next(&changed), list);

    assert_eq!(
        h.code(
            "add_bookmark",
            json!({ "account_id": id, "bucket": "data", "prefix": "", "label": null })
        ),
        "conflict"
    );
    assert_eq!(
        h.code(
            "add_bookmark",
            json!({ "account_id": "nope", "bucket": "data", "prefix": "x", "label": null })
        ),
        "not_found"
    );
    assert_eq!(
        h.code(
            "add_bookmark",
            json!({ "account_id": id, "bucket": "", "prefix": "", "label": null })
        ),
        "usage"
    );

    let first = list[0]["id"].as_str().unwrap().to_owned();
    let renamed = h.ok("rename_bookmark", json!({ "id": first, "label": "Logs" }));
    assert_eq!(renamed[0]["label"], "Logs");
    assert_eq!(next(&changed), renamed);
    assert_eq!(
        h.code("rename_bookmark", json!({ "id": first, "label": " " })),
        "usage"
    );
    assert_eq!(
        h.code("rename_bookmark", json!({ "id": "nope", "label": "x" })),
        "not_found"
    );

    // Bookmarks are in config.toml: they survive a restart.
    let h = h.restart();
    let list = h.ok("list_bookmarks", json!({}));
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["label"], "Logs");

    let left = h.ok("remove_bookmark", json!({ "id": first }));
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["label"], "Data");
    assert_eq!(
        h.code("remove_bookmark", json!({ "id": first })),
        "not_found"
    );
}

#[test]
fn removing_an_account_drops_its_bookmarks() {
    let server = start_s3();
    let h = Harness::new();
    let a = h.add_account(&server, "One");
    let b = h.add_account(&server, "Two");
    add(&h, &a, "data", "", Value::Null);
    add(&h, &b, "data", "", Value::Null);
    h.ok("remove_account", json!({ "id": a }));
    let list = h.ok("list_bookmarks", json!({}));
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["account_id"], b.as_str());
}
