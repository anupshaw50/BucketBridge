//! Edit in place: `start_edit`, `list_edits`, `stop_edit`,
//! `resolve_edit_conflict` and the `edit://changed` event.

use std::fs;
use std::path::Path;
use std::sync::mpsc::Receiver;

use serde_json::{Value, json};

use crate::harness::*;

/// Receives `edit://changed` payloads until `check` holds for one.
fn wait_edit(events: &Receiver<Value>, what: &str, check: impl Fn(&Value) -> bool) -> Value {
    loop {
        let event = events
            .recv_timeout(EVENT_TIMEOUT)
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
        if check(&event) {
            return event;
        }
    }
}

fn state_is(state: &'static str) -> impl Fn(&Value) -> bool {
    move |event| event["state"] == state
}

#[test]
fn start_edit_opens_the_copy_and_uploads_saves_until_stopped() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "docs/note.txt", b"one".to_vec()));
    let events = h.events("edit://changed");

    let args = json!({ "account_id": id, "bucket": bucket, "key": "docs/note.txt", "app": null });
    let session = h.ok("start_edit", args.clone());
    assert_eq!(
        keys(&session),
        [
            "account_id",
            "bucket",
            "conflict",
            "error",
            "etag",
            "id",
            "key",
            "last_upload_at",
            "local_path",
            "started_at",
            "state",
            "uploads",
            "version_id"
        ]
    );
    assert_eq!(session["state"], "watching");
    let path = session["local_path"].as_str().unwrap().to_owned();
    assert_eq!(*h.opened.lock().unwrap(), [Path::new(&path)]);
    assert_eq!(fs::read(&path).unwrap(), b"one");
    assert!(Path::new(&path).starts_with(h.core.dirs().data_dir.join("edit")));
    assert_eq!(h.ok("list_edits", json!({})).as_array().unwrap().len(), 1);

    // A second start of the same object opens the same copy again.
    let again = h.ok("start_edit", args);
    assert_eq!(again["id"], session["id"]);
    assert_eq!(h.opened.lock().unwrap().len(), 2);

    fs::write(&path, b"two").unwrap();
    let uploaded = wait_edit(&events, "the upload", |event| {
        event["state"] == "uploaded" && event["uploads"] == 1
    });
    assert_eq!(uploaded["id"], session["id"]);
    assert_eq!(block_on(server.get(&bucket, "docs/note.txt")), b"two");

    assert_eq!(
        h.ok("stop_edit", json!({ "id": session["id"], "discard": null })),
        Value::Null
    );
    wait_edit(&events, "the end", state_is("stopped"));
    assert!(h.ok("list_edits", json!({})).as_array().unwrap().is_empty());
    assert!(!Path::new(&path).exists());
    assert_eq!(
        h.code("stop_edit", json!({ "id": session["id"], "discard": null })),
        "not_found"
    );
}

#[test]
fn start_edit_with_an_application_remembers_it_and_a_failed_start_leaves_nothing() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a.txt", b"hello".to_vec()));
    let args = |key: &str, app: &str| json!({ "account_id": id, "bucket": bucket, "key": key, "app": app });

    let session = h.ok("start_edit", args("a.txt", "/Apps/Editor.app"));
    let opened = h.opened_with.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].1, "/Apps/Editor.app");
    assert!(h.opened.lock().unwrap().is_empty());
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(
        settings["general"]["open_with_recent"],
        json!(["/Apps/Editor.app"])
    );
    h.ok("stop_edit", json!({ "id": session["id"], "discard": true }));

    // An application that does not start ends the session and removes the copy.
    assert_eq!(h.code("start_edit", args("a.txt", "/broken.app")), "io");
    assert!(h.ok("list_edits", json!({})).as_array().unwrap().is_empty());
    let root = h.core.dirs().data_dir.join("edit");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

    // Folders and missing objects are rejected.
    assert_eq!(h.code("start_edit", args("dir/", "")), "usage");
    assert_eq!(h.code("start_edit", args("missing.txt", "")), "not_found");
}

#[test]
fn a_conflict_is_resolved_by_overwriting_saving_a_copy_or_discarding() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "c.txt", b"start".to_vec()));
    let events = h.events("edit://changed");
    let start = |key: &str| {
        h.ok(
            "start_edit",
            json!({ "account_id": id, "bucket": bucket, "key": key, "app": null }),
        )
    };

    // Overwrite.
    let session = start("c.txt");
    let path = session["local_path"].as_str().unwrap().to_owned();
    block_on(server.put(&bucket, "c.txt", b"theirs".to_vec()));
    fs::write(&path, b"mine").unwrap();
    let conflict = wait_edit(&events, "the conflict", state_is("conflict"));
    assert_eq!(conflict["conflict"]["remote_deleted"], false);
    assert!(conflict["conflict"]["remote_etag"].is_string());
    assert_eq!(block_on(server.get(&bucket, "c.txt")), b"theirs");
    assert_eq!(
        h.code(
            "stop_edit",
            json!({ "id": session["id"], "discard": false })
        ),
        "conflict"
    );
    let resolved = h.ok(
        "resolve_edit_conflict",
        json!({ "id": session["id"], "resolution": "overwrite" }),
    );
    assert_eq!(keys(&resolved), ["copy_key", "session"]);
    assert_eq!(resolved["session"]["state"], "uploaded");
    assert_eq!(block_on(server.get(&bucket, "c.txt")), b"mine");

    // Save as a copy ends the session.
    block_on(server.put(&bucket, "c.txt", b"theirs again".to_vec()));
    fs::write(&path, b"mine again").unwrap();
    wait_edit(&events, "the second conflict", |event| {
        event["state"] == "conflict" && event["uploads"] == 1
    });
    let resolved = h.ok(
        "resolve_edit_conflict",
        json!({ "id": session["id"], "resolution": "save_copy" }),
    );
    assert_eq!(resolved["session"], Value::Null);
    assert_eq!(resolved["copy_key"], "c (edited).txt");
    assert_eq!(
        block_on(server.get(&bucket, "c (edited).txt")),
        b"mine again"
    );
    assert_eq!(block_on(server.get(&bucket, "c.txt")), b"theirs again");
    assert!(!Path::new(&path).exists());

    // Discard.
    let session = start("c.txt");
    let path = session["local_path"].as_str().unwrap().to_owned();
    block_on(server.put(&bucket, "c.txt", b"theirs 3".to_vec()));
    fs::write(&path, b"unwanted").unwrap();
    wait_edit(&events, "the third conflict", |event| {
        event["state"] == "conflict" && event["id"] == session["id"]
    });
    h.ok(
        "resolve_edit_conflict",
        json!({ "id": session["id"], "resolution": "discard" }),
    );
    assert_eq!(block_on(server.get(&bucket, "c.txt")), b"theirs 3");
    assert!(h.ok("list_edits", json!({})).as_array().unwrap().is_empty());

    // Without a conflict there is nothing to overwrite; unknown ids are
    // rejected.
    let session = start("c.txt");
    assert_eq!(
        h.code(
            "resolve_edit_conflict",
            json!({ "id": session["id"], "resolution": "overwrite" })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "resolve_edit_conflict",
            json!({ "id": "nope", "resolution": "discard" })
        ),
        "not_found"
    );
    h.ok("stop_edit", json!({ "id": session["id"], "discard": true }));
}

#[test]
fn pending_saves_are_uploaded_when_the_app_shuts_down() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "s.txt", b"start".to_vec()));
    let session = h.ok(
        "start_edit",
        json!({ "account_id": id, "bucket": bucket, "key": "s.txt", "app": null }),
    );
    let path = session["local_path"].as_str().unwrap().to_owned();
    // Saved just before exit: the quiet period has not passed.
    fs::write(&path, b"saved at exit").unwrap();
    h.core.shutdown(std::time::Duration::from_secs(20));
    assert_eq!(block_on(server.get(&bucket, "s.txt")), b"saved at exit");
    assert!(!Path::new(&path).exists());
}
