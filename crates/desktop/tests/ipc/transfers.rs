//! The transfer queue: the `enqueue_*`, `rename` and queue-control commands,
//! `transfer://update` and `transfer://removed`, the settings that reach the
//! queue and the one bandwidth cap, and the queue file across a restart.
//!
//! Transfers that must stay unfinished run under a 1 KiB/s cap, which a test
//! lifts through `save_settings` when it wants them to end.

use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use bucketbridge_core::ErrorKind;
use bucketbridge_core::secrets::MemoryStore;
use bucketbridge_core::settings::ChecksumAlgorithm;
use bucketbridge_desktop::{Core, LOG_ENTRY, TRANSFER_CONFLICT, TRANSFER_REMOVED, TRANSFER_UPDATE};
use serde_json::{Value, json};

use crate::harness::*;

const UNKNOWN_ID: &str = "00000000-0000-4000-8000-000000000000";

/// The fields of a `TransferTask`, sorted.
const TASK_KEYS: [&str; 15] = [
    "account_id",
    "attempts",
    "bytes_done",
    "bytes_total",
    "created_at",
    "dst",
    "error",
    "eta",
    "group_id",
    "id",
    "kind",
    "speed",
    "src",
    "state",
    "updated_at",
];

/// Deterministic bytes; different seeds give different bytes.
fn payload(len: usize, seed: u32) -> Vec<u8> {
    let mut state: u32 = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
    if state == 0 {
        state = 0x1234_5678;
    }
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Writes a file (and its folders) and returns its path as the UI sends it.
fn write(path: &Path, bytes: Vec<u8>) -> String {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

/// Every file below `root` by its `/`-separated relative path.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &rel, out);
            } else {
                out.insert(rel, fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, "", &mut files);
    files
}

/// `Location` of an S3 key.
fn s3_at(bucket: &str, key: &str) -> Value {
    json!({ "type": "s3", "bucket": bucket, "key": key })
}

/// The tasks of a command result by the key of their S3 side: the source
/// for deletes and downloads, the destination for everything else.
fn by_key(tasks: &Value) -> BTreeMap<String, Value> {
    tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|task| {
            let side = if task["kind"] == "download" || task["kind"] == "delete" {
                &task["src"]
            } else {
                &task["dst"]
            };
            (side["key"].as_str().unwrap().to_owned(), task.clone())
        })
        .collect()
}

/// Waits until the events say that every task of a command's result has
/// completed, with all its bytes and no error.
fn complete(h: &Harness, events: &Receiver<Value>, tasks: &Value) {
    let ids = ids(tasks);
    let done = until_all(events, &ids, "completed");
    for (id, event) in &done {
        assert_eq!(event["bytes_done"], event["bytes_total"], "{event}");
        assert_eq!(event["error"], Value::Null, "{event}");
        assert_eq!(h.task(id)["state"], "completed");
    }
}

/// Holds uploads and downloads open: one transfer at a time, 1 KiB/s.
fn throttle(h: &Harness) {
    h.set_setting("transfer", "max_concurrent_tasks", json!(1));
    h.set_setting("transfer", "bandwidth_limit_kibps", json!(1));
    h.set_setting("transfer", "retry_attempts", json!(0));
}

/// Lifts the cap that [`throttle`] set.
fn unthrottle(h: &Harness) {
    h.set_setting("transfer", "bandwidth_limit_kibps", Value::Null);
}

#[test]
fn a_folder_uploads_and_downloads_back_byte_for_byte() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());

    // photos/a.txt, big.bin, sub/b.bin, sub/deep/empty.txt and an empty
    // folder, which becomes a folder marker.
    let local = tempfile::tempdir().unwrap();
    let photos = local.path().join("photos");
    let files = [
        ("a.txt", payload(1_234, 1)),
        ("big.bin", payload(300 * 1024, 2)),
        ("sub/b.bin", payload(70_000, 3)),
        ("sub/deep/empty.txt", Vec::new()),
    ];
    for (name, bytes) in &files {
        write(&photos.join(name), bytes.clone());
    }
    fs::create_dir_all(photos.join("blank")).unwrap();

    let updates = h.events(TRANSFER_UPDATE);
    let queued = h.ok(
        "enqueue_upload",
        json!({
            "account_id": id,
            "local_paths": [photos.to_str().unwrap()],
            "bucket": bucket,
            "prefix": "backup/",
        }),
    );
    let uploads = by_key(&queued);
    let mut expected: Vec<String> = files
        .iter()
        .map(|(name, _)| format!("backup/photos/{name}"))
        .collect();
    expected.push("backup/photos/blank/".to_owned());
    expected.sort_unstable();
    assert_eq!(uploads.keys().cloned().collect::<Vec<_>>(), expected);

    // Every task of the call is queued and shares one group.
    let group = queued[0]["group_id"].as_str().unwrap();
    for task in queued.as_array().unwrap() {
        assert_eq!(keys(task), TASK_KEYS);
        assert_eq!(task["account_id"], id.as_str());
        assert_eq!(task["group_id"], group);
        assert_eq!(task["kind"], "upload");
        assert_eq!(task["state"], "queued");
        assert_eq!(task["attempts"], 0);
        assert_eq!(task["bytes_done"], 0);
        assert_eq!(task["speed"], 0);
        assert_eq!(task["eta"], Value::Null);
        assert_eq!(task["error"], Value::Null);
        assert_eq!(task["src"]["type"], "local");
        assert_eq!(task["dst"]["type"], "s3");
        assert_eq!(task["dst"]["bucket"], bucket.as_str());
    }
    for (name, bytes) in &files {
        let task = &uploads[&format!("backup/photos/{name}")];
        assert_eq!(task["bytes_total"], bytes.len(), "{name}");
        assert_eq!(
            PathBuf::from(task["src"]["path"].as_str().unwrap()),
            photos.join(name)
        );
    }
    assert_eq!(uploads["backup/photos/blank/"]["bytes_total"], 0);
    assert_eq!(h.tasks().len(), 5);

    complete(&h, &updates, &queued);
    for (name, bytes) in &files {
        let key = format!("backup/photos/{name}");
        assert_eq!(&block_on(server.get(&bucket, &key)), bytes, "{key}");
    }
    if let Some(root) = server.root() {
        assert!(root.join(&bucket).join("backup/photos/blank").is_dir());
    }

    // And back: the selected folder lands under the destination.
    let restored = local.path().join("restored");
    let downloads = h.ok(
        "enqueue_download",
        json!({
            "account_id": id,
            "bucket": bucket,
            "keys": ["backup/photos/"],
            "dest_dir": restored.to_str().unwrap(),
        }),
    );
    let by_source = by_key(&downloads);
    for (name, bytes) in &files {
        let task = &by_source[&format!("backup/photos/{name}")];
        assert_eq!(task["kind"], "download");
        assert_eq!(
            task["src"],
            s3_at(&bucket, &format!("backup/photos/{name}"))
        );
        assert_eq!(task["dst"]["type"], "local");
        assert_eq!(
            PathBuf::from(task["dst"]["path"].as_str().unwrap()),
            restored.join("photos").join(name)
        );
        assert_eq!(task["bytes_total"], bytes.len(), "{name}");
    }
    complete(&h, &updates, &downloads);

    let sent: BTreeMap<String, Vec<u8>> = files
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.clone()))
        .collect();
    let received = tree(&restored.join("photos"));
    assert_eq!(
        received.keys().collect::<Vec<_>>(),
        sent.keys().collect::<Vec<_>>()
    );
    for (name, bytes) in &sent {
        assert!(
            received[name] == *bytes,
            "{name} differs after the round trip"
        );
    }
}

#[test]
fn copy_move_rename_and_delete_follow_the_folder_rules() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let source = block_on(server.new_bucket());
    let target = block_on(server.new_bucket());
    for (key, body) in [
        ("docs/a.txt", "alpha"),
        ("docs/sub/b.txt", "bravo"),
        ("docs/sub/c.txt", "charlie"),
        ("top.txt", "top"),
    ] {
        block_on(server.put(&source, key, body));
    }
    let updates = h.events(TRANSFER_UPDATE);

    // Copy: the selected names land directly under the prefix; sources stay.
    let copies = h.ok(
        "enqueue_copy",
        json!({
            "account_id": id, "src_bucket": source, "keys": ["docs/", "top.txt"],
            "dst_bucket": target, "dst_prefix": "backup/",
        }),
    );
    let by_destination = by_key(&copies);
    assert_eq!(
        by_destination
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "backup/docs/a.txt",
            "backup/docs/sub/b.txt",
            "backup/docs/sub/c.txt",
            "backup/top.txt"
        ]
    );
    let first = &by_destination["backup/docs/a.txt"];
    assert_eq!(keys(first), TASK_KEYS);
    assert_eq!(first["kind"], "copy");
    assert_eq!(first["src"], s3_at(&source, "docs/a.txt"));
    assert_eq!(first["dst"], s3_at(&target, "backup/docs/a.txt"));
    assert_eq!(first["bytes_total"], 5);
    // A single selected key has no size yet; the engine looks it up.
    assert_eq!(by_destination["backup/top.txt"]["bytes_total"], Value::Null);
    complete(&h, &updates, &copies);
    for (key, body) in [
        ("backup/docs/a.txt", "alpha"),
        ("backup/docs/sub/b.txt", "bravo"),
        ("backup/docs/sub/c.txt", "charlie"),
        ("backup/top.txt", "top"),
    ] {
        assert_eq!(block_on(server.get(&target, key)), body.as_bytes(), "{key}");
    }
    assert!(block_on(server.exists(&source, "docs/a.txt")));
    assert!(block_on(server.exists(&source, "top.txt")));

    // Move: the destination appears, the source goes.
    let moves = h.ok(
        "enqueue_move",
        json!({
            "account_id": id, "src_bucket": target, "keys": ["backup/docs/"],
            "dst_bucket": target, "dst_prefix": "moved/",
        }),
    );
    assert_eq!(
        by_key(&moves)
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "moved/docs/a.txt",
            "moved/docs/sub/b.txt",
            "moved/docs/sub/c.txt"
        ]
    );
    assert!(by_key(&moves).values().all(|task| task["kind"] == "move"));
    complete(&h, &updates, &moves);
    for key in [
        "backup/docs/a.txt",
        "backup/docs/sub/b.txt",
        "backup/docs/sub/c.txt",
    ] {
        assert!(
            !block_on(server.exists(&target, key)),
            "{key} is still there"
        );
    }
    assert_eq!(block_on(server.get(&target, "moved/docs/a.txt")), b"alpha");

    // Rename an object (one move), and a prefix (one move per object).
    let renamed = h.ok(
        "rename",
        json!({
            "account_id": id, "bucket": target,
            "from_key": "backup/top.txt", "to_key": "backup/renamed.txt",
        }),
    );
    assert_eq!(renamed.as_array().unwrap().len(), 1);
    assert_eq!(renamed[0]["kind"], "move");
    assert_eq!(renamed[0]["src"], s3_at(&target, "backup/top.txt"));
    assert_eq!(renamed[0]["dst"], s3_at(&target, "backup/renamed.txt"));
    complete(&h, &updates, &renamed);
    assert!(!block_on(server.exists(&target, "backup/top.txt")));
    assert_eq!(block_on(server.get(&target, "backup/renamed.txt")), b"top");

    let before = h.tasks().len();
    for (from_key, to_key) in [
        // The destination exists.
        ("moved/docs/a.txt", "moved/docs/sub/b.txt"),
        ("moved/docs/", "backup/"),
    ] {
        assert_eq!(
            h.code(
                "rename",
                json!({
                    "account_id": id, "bucket": target,
                    "from_key": from_key, "to_key": to_key,
                })
            ),
            "conflict",
            "{from_key} to {to_key}"
        );
    }
    assert_eq!(
        h.code(
            "rename",
            json!({
                "account_id": id, "bucket": target,
                "from_key": "missing.txt", "to_key": "other.txt",
            })
        ),
        "not_found"
    );
    assert_eq!(h.tasks().len(), before, "a rejected rename queues nothing");

    let folder = h.ok(
        "rename",
        json!({
            "account_id": id, "bucket": target,
            "from_key": "moved/docs/", "to_key": "moved/papers/",
        }),
    );
    assert_eq!(
        by_key(&folder)
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "moved/papers/a.txt",
            "moved/papers/sub/b.txt",
            "moved/papers/sub/c.txt"
        ]
    );
    complete(&h, &updates, &folder);
    assert!(!block_on(server.exists(&target, "moved/docs/a.txt")));
    assert_eq!(
        block_on(server.get(&target, "moved/papers/sub/c.txt")),
        b"charlie"
    );

    // Delete: one task per key, a prefix is one task.
    let deletes = h.ok(
        "enqueue_delete",
        json!({
            "account_id": id, "bucket": target,
            "keys": ["moved/papers/", "backup/renamed.txt"],
        }),
    );
    assert_eq!(deletes.as_array().unwrap().len(), 2);
    let by_target = by_key(&deletes);
    assert_eq!(
        by_target["moved/papers/"]["src"],
        s3_at(&target, "moved/papers/")
    );
    for task in by_target.values() {
        assert_eq!(task["kind"], "delete");
        assert_eq!(task["dst"], Value::Null);
    }
    complete(&h, &updates, &deletes);
    for key in [
        "moved/papers/a.txt",
        "moved/papers/sub/b.txt",
        "moved/papers/sub/c.txt",
        "backup/renamed.txt",
    ] {
        assert!(
            !block_on(server.exists(&target, key)),
            "{key} is still there"
        );
    }
    // The source bucket was only ever read.
    assert_eq!(block_on(server.get(&source, "docs/sub/c.txt")), b"charlie");
}

#[test]
fn selections_with_a_duplicate_destination_conflict_and_queue_nothing() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a/report.txt", "first"));
    block_on(server.put(&bucket, "b/report.txt", "second"));
    let local = tempfile::tempdir().unwrap();
    let first = write(&local.path().join("x").join("report.txt"), b"x".to_vec());
    let second = write(&local.path().join("y").join("report.txt"), b"y".to_vec());
    let dest = local.path().join("dest");

    for (command, args) in [
        (
            "enqueue_copy",
            json!({
                "account_id": id, "src_bucket": bucket,
                "keys": ["a/report.txt", "b/report.txt"],
                "dst_bucket": bucket, "dst_prefix": "dst/",
            }),
        ),
        (
            "enqueue_move",
            json!({
                "account_id": id, "src_bucket": bucket,
                "keys": ["a/report.txt", "b/report.txt"],
                "dst_bucket": bucket, "dst_prefix": "dst/",
            }),
        ),
        (
            "enqueue_download",
            json!({
                "account_id": id, "bucket": bucket,
                "keys": ["a/report.txt", "b/report.txt"],
                "dest_dir": dest.to_str().unwrap(),
            }),
        ),
        (
            "enqueue_upload",
            json!({
                "account_id": id, "local_paths": [first, second],
                "bucket": bucket, "prefix": "up/",
            }),
        ),
    ] {
        let (code, message) = h.error(command, args);
        assert_eq!(code, "conflict", "{command}");
        assert!(message.contains("report.txt"), "{command}: {message}");
        assert!(h.tasks().is_empty(), "{command} queued something");
    }
    assert!(!block_on(server.exists(&bucket, "dst/report.txt")));
    assert!(!block_on(server.exists(&bucket, "up/report.txt")));
    assert!(!dest.exists());

    // Each of them alone is fine.
    let copies = h.ok(
        "enqueue_copy",
        json!({
            "account_id": id, "src_bucket": bucket, "keys": ["a/report.txt"],
            "dst_bucket": bucket, "dst_prefix": "dst/",
        }),
    );
    assert_eq!(copies.as_array().unwrap().len(), 1);
    assert_eq!(copies[0]["dst"], s3_at(&bucket, "dst/report.txt"));
}

#[test]
fn enqueue_commands_reject_bad_input_and_unusable_accounts() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let local = tempfile::tempdir().unwrap();
    let here = local.path().to_str().unwrap().to_owned();

    // Paths must be absolute; bucket names valid.
    assert_eq!(
        h.code(
            "enqueue_upload",
            json!({
                "account_id": id, "local_paths": ["relative/file.txt"],
                "bucket": "bkt", "prefix": "",
            })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "enqueue_download",
            json!({
                "account_id": id, "bucket": "bkt", "keys": ["k.txt"],
                "dest_dir": "relative/folder",
            })
        ),
        "usage"
    );
    for (command, args) in [
        (
            "enqueue_upload",
            json!({
                "account_id": id, "local_paths": [here],
                "bucket": "Not A Valid Name", "prefix": "",
            }),
        ),
        (
            "enqueue_delete",
            json!({ "account_id": id, "bucket": "Not A Valid Name", "keys": ["k"] }),
        ),
    ] {
        assert_eq!(h.code(command, args), "usage", "{command}");
    }
    assert!(h.tasks().is_empty());

    // Nothing selected, nothing queued.
    assert_eq!(
        h.ok(
            "enqueue_upload",
            json!({ "account_id": id, "local_paths": [], "bucket": "bkt", "prefix": "" })
        ),
        json!([])
    );
    assert_eq!(
        h.ok(
            "enqueue_delete",
            json!({ "account_id": id, "bucket": "bkt", "keys": [] })
        ),
        json!([])
    );

    // An unknown account is rejected by every command that names one.
    for (command, args) in enqueue_calls(UNKNOWN_ID, &here) {
        assert_eq!(h.code(command, args), "not_found", "{command}");
    }

    // A locked vault rejects them with `locked`; the queue itself can still
    // be listed and controlled.
    h.ok(
        "set_master_password",
        json!({ "current": null, "new_password": "correct horse battery" }),
    );
    h.ok("lock", json!({}));
    for (command, args) in enqueue_calls(&id, &here) {
        assert_eq!(h.code(command, args), "locked", "{command}");
    }
    assert_eq!(h.ok("list_tasks", json!({})), json!([]));
    assert_eq!(h.ok("clear_finished", json!({})), Value::Null);
    assert_eq!(
        h.code("pause_task", json!({ "id": UNKNOWN_ID })),
        "not_found"
    );
    h.ok("unlock", json!({ "password": "correct horse battery" }));

    // An account with an MFA serial and no MFA session.
    let mut input = account_input(&server, "MFA");
    input["mfa_serial"] = json!("arn:aws:iam::123456789012:mfa/example-device");
    let mfa = h.ok("add_account", json!({ "input": input }));
    for (command, args) in enqueue_calls(mfa["id"].as_str().unwrap(), &here) {
        assert_eq!(h.code(command, args), "mfa_required", "{command}");
    }
    assert!(h.tasks().is_empty());
}

/// One valid call of every command that names an account and queues.
fn enqueue_calls(account: &str, local: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "enqueue_upload",
            json!({ "account_id": account, "local_paths": [local], "bucket": "bkt", "prefix": "" }),
        ),
        (
            "enqueue_download",
            json!({
                "account_id": account, "bucket": "bkt", "keys": ["k.txt"], "dest_dir": local
            }),
        ),
        (
            "enqueue_copy",
            json!({
                "account_id": account, "src_bucket": "bkt", "keys": ["k.txt"],
                "dst_bucket": "bkt", "dst_prefix": "copy/"
            }),
        ),
        (
            "enqueue_move",
            json!({
                "account_id": account, "src_bucket": "bkt", "keys": ["k.txt"],
                "dst_bucket": "bkt", "dst_prefix": "moved/"
            }),
        ),
        (
            "enqueue_delete",
            json!({ "account_id": account, "bucket": "bkt", "keys": ["k.txt"] }),
        ),
        (
            "rename",
            json!({
                "account_id": account, "bucket": "bkt", "from_key": "k.txt", "to_key": "l.txt"
            }),
        ),
    ]
}

#[test]
fn tasks_move_through_the_queue_rules() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    throttle(&h);
    let updates = h.events(TRANSFER_UPDATE);
    let removed = h.events(TRANSFER_REMOVED);

    let local = tempfile::tempdir().unwrap();
    let paths: Vec<String> = (1..=3)
        .map(|n| {
            write(
                &local.path().join(format!("f{n}.bin")),
                payload(64 * 1024, n),
            )
        })
        .collect();
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": paths, "bucket": bucket, "prefix": "q/" }),
    );
    let tasks = ids(&queued);
    let (t1, t2, t3) = (&tasks[0], &tasks[1], &tasks[2]);

    // One at a time, oldest first: t1 runs, the others wait.
    h.wait_state(t1, "running");
    assert_eq!(h.task(t2)["state"], "queued");
    assert_eq!(h.task(t3)["state"], "queued");
    expect_updates(
        &updates,
        &[
            (t1, "queued"),
            (t2, "queued"),
            (t3, "queued"),
            (t1, "running"),
        ],
    );

    // Pause and resume a queued task.
    h.ok("pause_task", json!({ "id": t2 }));
    assert_eq!(h.task(t2)["state"], "stopped");
    expect_updates(&updates, &[(t2, "stopped")]);
    for command in ["pause_task", "retry_task"] {
        assert_eq!(
            h.code(command, json!({ "id": t2 })),
            "conflict",
            "{command}"
        );
    }
    h.ok("resume_task", json!({ "id": t2 }));
    assert_eq!(h.task(t2)["state"], "queued");
    assert_eq!(h.code("resume_task", json!({ "id": t2 })), "conflict");
    expect_updates(&updates, &[(t2, "queued")]);

    // A running task cannot be resumed or retried; it can be paused, which
    // frees its place for the next one.
    for command in ["resume_task", "retry_task"] {
        assert_eq!(
            h.code(command, json!({ "id": t1 })),
            "conflict",
            "{command}"
        );
    }
    h.ok("pause_task", json!({ "id": t1 }));
    h.wait_state(t1, "stopped");
    h.wait_state(t2, "running");
    expect_updates(&updates, &[(t1, "stopped"), (t2, "running")]);
    h.ok("resume_task", json!({ "id": t1 }));
    assert_eq!(h.task(t1)["state"], "queued");
    expect_updates(&updates, &[(t1, "queued")]);

    // Cancel a queued task: final, and nothing else applies to it.
    h.ok("cancel_task", json!({ "id": t3 }));
    h.wait_state(t3, "cancelled");
    expect_updates(&updates, &[(t3, "cancelled")]);
    for command in ["pause_task", "resume_task", "retry_task", "cancel_task"] {
        assert_eq!(
            h.code(command, json!({ "id": t3 })),
            "conflict",
            "{command}"
        );
    }

    // stop_all stops what is queued and what runs; start_all queues it again.
    h.ok("stop_all", json!({}));
    h.wait_state(t2, "stopped");
    assert_eq!(h.task(t1)["state"], "stopped");
    assert_eq!(h.task(t3)["state"], "cancelled");
    expect_updates(&updates, &[(t1, "stopped"), (t2, "stopped")]);
    h.ok("start_all", json!({}));
    h.wait_state(t1, "running");
    assert_eq!(h.task(t2)["state"], "queued");
    assert_eq!(h.task(t3)["state"], "cancelled");
    expect_updates(&updates, &[(t1, "queued"), (t2, "queued"), (t1, "running")]);

    // cancel_all cleans up the running and the queued task; the cancelled one
    // is skipped. Nothing was uploaded.
    h.ok("cancel_all", json!({}));
    h.wait_state(t1, "cancelled");
    h.wait_state(t2, "cancelled");
    expect_updates(&updates, &[(t1, "cancelled"), (t2, "cancelled")]);
    for n in 1..=3 {
        assert!(!block_on(server.exists(&bucket, &format!("q/f{n}.bin"))));
    }
    assert_eq!(block_on(server.pending_uploads(&bucket, "q/f1.bin")), 0);

    // clear_finished removes the cancelled tasks.
    h.ok("clear_finished", json!({}));
    assert!(h.tasks().is_empty());
    let mut every = tasks.clone();
    every.sort_unstable();
    assert_eq!(removed_ids(&next(&removed)), every);

    // Failed tasks: a download into a folder that cannot be created, because
    // a file is in the way.
    unthrottle(&h);
    let wanted = payload(64 * 1024, 31);
    block_on(server.put(&bucket, "get/f1.bin", wanted.clone()));
    let in_the_way = local.path().join("in-the-way");
    fs::write(&in_the_way, b"a file").unwrap();
    let download = |dest: &Path| {
        json!({
            "account_id": id, "bucket": bucket, "keys": ["get/f1.bin"],
            "dest_dir": dest.to_str().unwrap(),
        })
    };
    let failing = h.ok("enqueue_download", download(&in_the_way.join("out")));
    let m1 = ids(&failing).remove(0);
    let failed = h.wait_state(&m1, "failed");
    assert!(!failed["error"].as_str().unwrap().is_empty());
    assert_eq!(failed["attempts"], 1);
    expect_updates(
        &updates,
        &[(&m1, "queued"), (&m1, "running"), (&m1, "failed")],
    );

    // Failed tasks stay until they are retried or cancelled.
    h.ok("clear_finished", json!({}));
    assert_eq!(h.tasks().len(), 1);
    assert!(
        removed.recv_timeout(Duration::from_millis(300)).is_err(),
        "clear_finished removed a failed task"
    );
    for command in ["pause_task", "resume_task"] {
        assert_eq!(
            h.code(command, json!({ "id": m1 })),
            "conflict",
            "{command}"
        );
    }
    h.ok("retry_task", json!({ "id": m1 }));
    h.wait_task(&m1, "failed again", |task| {
        task["state"] == "failed" && task["attempts"] == 2
    });
    expect_updates(
        &updates,
        &[(&m1, "queued"), (&m1, "running"), (&m1, "failed")],
    );

    // Once the file is gone, a retry completes the download.
    fs::remove_file(&in_the_way).unwrap();
    h.ok("retry_task", json!({ "id": m1 }));
    let done = h.wait_state(&m1, "completed");
    expect_updates(
        &updates,
        &[(&m1, "queued"), (&m1, "running"), (&m1, "completed")],
    );
    assert_eq!(done["error"], Value::Null);
    assert_eq!(done["attempts"], 3);
    assert_eq!(done["bytes_done"], 64 * 1024);
    assert_eq!(
        fs::read(in_the_way.join("out").join("f1.bin")).unwrap(),
        wanted
    );
    for command in ["pause_task", "resume_task", "retry_task", "cancel_task"] {
        assert_eq!(
            h.code(command, json!({ "id": m1 })),
            "conflict",
            "{command}"
        );
    }

    // A failed task can be cancelled.
    let second_way = local.path().join("second-way");
    fs::write(&second_way, b"another file").unwrap();
    let other = h.ok("enqueue_download", download(&second_way.join("out")));
    let m2 = ids(&other).remove(0);
    h.wait_state(&m2, "failed");
    expect_updates(&updates, &[(&m2, "failed")]);
    h.ok("cancel_task", json!({ "id": m2 }));
    h.wait_state(&m2, "cancelled");
    expect_updates(&updates, &[(&m2, "cancelled")]);
    assert!(!second_way.join("out").exists());

    // Unknown ids.
    for command in ["pause_task", "resume_task", "cancel_task", "retry_task"] {
        assert_eq!(
            h.code(command, json!({ "id": UNKNOWN_ID })),
            "not_found",
            "{command}"
        );
    }

    h.ok("clear_finished", json!({}));
    assert!(h.tasks().is_empty());
    let mut finished = vec![m1.clone(), m2.clone()];
    finished.sort_unstable();
    assert_eq!(removed_ids(&next(&removed)), finished);
}

#[test]
fn entries_that_cannot_be_added_become_failed_tasks() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "keep.txt", "kept"));

    // "/" would match the whole bucket: it fails on its own, the other key
    // is queued as usual.
    let tasks = h.ok(
        "enqueue_delete",
        json!({ "account_id": id, "bucket": bucket, "keys": ["keep.txt", "/"] }),
    );
    assert_eq!(tasks.as_array().unwrap().len(), 2);
    let queued = &tasks[0];
    let rejected = &tasks[1];
    assert_eq!(queued["state"], "queued");
    assert_eq!(rejected["state"], "failed");
    assert_eq!(rejected["kind"], "delete");
    assert_eq!(rejected["src"], s3_at(&bucket, "/"));
    assert_eq!(rejected["attempts"], 0);
    let reason = rejected["error"].as_str().unwrap().to_owned();
    assert!(reason.contains("whole bucket"), "{reason}");
    assert_eq!(queued["group_id"], rejected["group_id"]);

    h.wait_state(queued["id"].as_str().unwrap(), "completed");
    assert!(!block_on(server.exists(&bucket, "keep.txt")));

    // Retrying fails again with the same reason and touches nothing.
    let rejected_id = rejected["id"].as_str().unwrap();
    h.ok("retry_task", json!({ "id": rejected_id }));
    let again = h.wait_task(rejected_id, "failed again", |task| {
        task["state"] == "failed" && task["attempts"] == 1
    });
    assert_eq!(again["error"], reason.as_str());
    h.ok("cancel_task", json!({ "id": rejected_id }));
    h.wait_state(rejected_id, "cancelled");
}

#[test]
fn queue_events_arrive_in_order_and_nothing_is_dropped() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let updates = h.events(TRANSFER_UPDATE);
    let removed = h.events(TRANSFER_REMOVED);

    // One upload under 1 KiB/s: queued, running with progress, completed.
    h.set_setting("transfer", "bandwidth_limit_kibps", json!(1));
    let local = tempfile::tempdir().unwrap();
    let file = write(&local.path().join("slow.bin"), payload(3 * 1024, 9));
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": [file], "bucket": bucket, "prefix": "" }),
    );
    let slow = ids(&queued).remove(0);
    let mut states: Vec<String> = Vec::new();
    let mut progress = Vec::new();
    loop {
        let event = next(&updates);
        if event["id"] != slow.as_str() {
            continue;
        }
        assert_eq!(keys(&event), TASK_KEYS);
        let state = event["state"].as_str().unwrap().to_owned();
        if state == "running" {
            progress.push(event["bytes_done"].as_u64().unwrap());
        }
        if states.last() != Some(&state) {
            states.push(state.clone());
        }
        if state == "completed" {
            assert_eq!(event["bytes_done"], 3 * 1024);
            assert_eq!(event["bytes_total"], 3 * 1024);
            assert_eq!(event["speed"], 0);
            break;
        }
    }
    assert_eq!(states, ["queued", "running", "completed"]);
    // Progress was reported while it ran, and only ever grew.
    assert!(progress.iter().any(|done| *done > 0 && *done < 3 * 1024));
    assert!(progress.windows(2).all(|pair| pair[0] <= pair[1]));
    unthrottle(&h);

    // Many tasks: every one reports each of its states once, in order.
    let names: Vec<String> = (0..200).map(|n| format!("gone/{n:03}.txt")).collect();
    let queued = h.ok(
        "enqueue_delete",
        json!({ "account_id": id, "bucket": bucket, "keys": names }),
    );
    let all = ids(&queued);
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut finished = 0;
    while finished < all.len() {
        let event = next(&updates);
        let task = event["id"].as_str().unwrap().to_owned();
        if !all.contains(&task) {
            continue;
        }
        let state = event["state"].as_str().unwrap().to_owned();
        let states = seen.entry(task).or_default();
        if states.last() != Some(&state) {
            states.push(state.clone());
        }
        if state == "completed" {
            finished += 1;
        }
    }
    assert_eq!(seen.len(), 200);
    for (task, states) in &seen {
        assert_eq!(states, &["queued", "running", "completed"], "{task}");
    }

    // clear_finished: one `transfer://removed` with every id, none when
    // there is nothing to remove.
    h.ok("clear_finished", json!({}));
    let event = next(&removed);
    assert_eq!(keys(&event), ["ids"]);
    let mut gone: Vec<String> = event["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    let mut expected = all.clone();
    expected.push(slow);
    gone.sort_unstable();
    expected.sort_unstable();
    assert_eq!(gone, expected);
    assert!(h.tasks().is_empty());
    h.ok("clear_finished", json!({}));
    assert!(
        removed.recv_timeout(Duration::from_millis(500)).is_err(),
        "an empty clear_finished sent an event"
    );
}

#[test]
fn saving_settings_updates_the_queue_and_the_one_bandwidth_cap() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let core: &Core = &h.core;
    let cap = |kibps: u64| NonZeroU64::new(kibps * 1024);

    assert_eq!(core.limiter().rate(), None);
    assert_eq!(core.queue().settings().max_concurrent_tasks, 4);

    // One cap: the queue's limiter is the one that sync runs and
    // `open_object` use.
    h.set_setting("transfer", "bandwidth_limit_kibps", json!(1));
    assert_eq!(core.limiter().rate(), cap(1));
    assert_eq!(core.queue().limiter().rate(), cap(1));
    assert_eq!(core.queue().settings().bandwidth_limit_kibps, Some(1));

    // A rejected save changes nothing.
    let mut invalid = h.ok("get_settings", json!({}));
    invalid["transfer"]["bandwidth_limit_kibps"] = json!(0);
    invalid["transfer"]["max_concurrent_tasks"] = json!(2);
    assert_eq!(
        h.code("save_settings", json!({ "settings": invalid })),
        "usage"
    );
    assert_eq!(core.limiter().rate(), cap(1));
    assert_eq!(core.queue().settings().max_concurrent_tasks, 4);

    // Data integrity reaches the runners.
    assert_eq!(
        core.runner().data_integrity().checksum_algorithm,
        ChecksumAlgorithm::Crc32
    );
    h.set_setting("data_integrity", "checksum_algorithm", json!("sha256"));
    h.set_setting("data_integrity", "validate_downloads", json!(false));
    let integrity = core.runner().data_integrity();
    assert_eq!(integrity.checksum_algorithm, ChecksumAlgorithm::Sha256);
    assert!(!integrity.validate_downloads);

    // The cap applies to a running upload, and lifting it frees the upload
    // at once: 128 KiB would take two minutes at 1 KiB/s.
    h.set_setting("transfer", "max_concurrent_tasks", json!(1));
    assert_eq!(core.queue().settings().max_concurrent_tasks, 1);
    let local = tempfile::tempdir().unwrap();
    let paths = [
        write(&local.path().join("one.bin"), payload(128 * 1024, 11)),
        write(&local.path().join("two.bin"), payload(4 * 1024, 12)),
    ];
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": paths, "bucket": bucket, "prefix": "" }),
    );
    let (one, two) = (ids(&queued)[0].clone(), ids(&queued)[1].clone());
    h.wait_task(&one, "some progress", |task| {
        task["state"] == "running" && task["bytes_done"].as_u64().unwrap() > 0
    });
    std::thread::sleep(Duration::from_millis(1200));
    let task = h.task(&one);
    assert_eq!(task["state"], "running");
    assert!(
        task["bytes_done"].as_u64().unwrap() < 16 * 1024,
        "the cap does not hold: {task}"
    );

    // A higher concurrency starts the waiting task at once; it shares the
    // cap with the first one.
    assert_eq!(h.task(&two)["state"], "queued");
    h.set_setting("transfer", "max_concurrent_tasks", json!(2));
    h.wait_state(&two, "running");

    unthrottle(&h);
    assert_eq!(core.limiter().rate(), None);
    assert_eq!(core.queue().limiter().rate(), None);
    h.wait_state(&one, "completed");
    h.wait_state(&two, "completed");
    assert_eq!(
        block_on(server.get(&bucket, "one.bin")),
        fs::read(&paths[0]).unwrap()
    );
    assert_eq!(
        block_on(server.get(&bucket, "two.bin")),
        fs::read(&paths[1]).unwrap()
    );
}

#[test]
fn a_task_that_starts_while_the_secrets_are_locked_fails_and_can_be_retried() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    h.ok(
        "set_master_password",
        json!({ "current": null, "new_password": "correct horse battery" }),
    );
    throttle(&h);

    let local = tempfile::tempdir().unwrap();
    let paths = [
        write(&local.path().join("a.bin"), payload(32 * 1024, 21)),
        write(&local.path().join("b.bin"), payload(2 * 1024, 22)),
    ];
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": paths, "bucket": bucket, "prefix": "" }),
    );
    let tasks = ids(&queued);
    h.wait_state(&tasks[0], "running");

    // Stop everything, lock, and let the tasks start: each one asks the
    // session for its account and is refused.
    h.ok("stop_all", json!({}));
    h.wait_state(&tasks[0], "stopped");
    unthrottle(&h);
    h.ok("lock", json!({}));
    h.ok("start_all", json!({}));
    for task in &tasks {
        let failed = h.wait_state(task, "failed");
        let error = failed["error"].as_str().unwrap();
        assert!(error.contains("locked"), "{error}");
    }
    assert!(!block_on(server.exists(&bucket, "a.bin")));

    h.ok("unlock", json!({ "password": "correct horse battery" }));
    for task in &tasks {
        h.ok("retry_task", json!({ "id": task }));
    }
    for task in &tasks {
        h.wait_state(task, "completed");
    }
    assert_eq!(
        block_on(server.get(&bucket, "a.bin")),
        fs::read(&paths[0]).unwrap()
    );
    assert_eq!(
        block_on(server.get(&bucket, "b.bin")),
        fs::read(&paths[1]).unwrap()
    );
}

const MIB: usize = 1024 * 1024;

/// Multipart uploads from 5 MiB, in 5 MiB parts, one part at a time, one
/// transfer at a time, at 2 MiB/s: the first part of a 15 MiB file is done
/// after about 2.5 s, the whole upload takes about 7.5 s.
fn slow_multipart(h: &Harness) {
    for (field, value) in [
        ("multipart_threshold_mib", 5),
        ("part_size_mib", 5),
        ("max_parts_per_task", 1),
        ("max_concurrent_tasks", 1),
        ("bandwidth_limit_kibps", 2048),
    ] {
        h.set_setting("transfer", field, json!(value));
    }
}

/// Waits until the first part of the upload of `task` is uploaded and
/// checkpointed; returns the upload ID.
fn first_part(h: &Harness, task: &str) -> String {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        let state = h.core.queue().resume_state(task).unwrap();
        if let Some(state) = state
            && state["parts"]
                .as_array()
                .is_some_and(|parts| !parts.is_empty())
        {
            return state["upload_id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "no part was uploaded");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_queue_survives_a_restart_and_resumes_a_multipart_upload() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    slow_multipart(&h);
    let local = tempfile::tempdir().unwrap();
    let big = write(&local.path().join("big.bin"), payload(15 * MIB, 7));
    let small = write(&local.path().join("small.txt"), payload(2048, 8));
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": [big, small], "bucket": bucket, "prefix": "keep/" }),
    );
    let tasks = ids(&queued);
    let (big_id, small_id) = (tasks[0].clone(), tasks[1].clone());

    // The first part is uploaded and its checkpoint kept.
    let upload_id = first_part(&h, &big_id);
    assert_eq!(h.task(&small_id)["state"], "queued");

    // Shutting down stops the queue: it refuses commands but can be read.
    h.core.shutdown(Duration::from_secs(30));
    assert_eq!(h.code("start_all", json!({})), "conflict");
    assert_eq!(h.code("pause_task", json!({ "id": big_id })), "conflict");
    assert_eq!(h.task(&big_id)["state"], "stopped");
    assert_eq!(h.task(&small_id)["state"], "stopped");
    let queue_file = h.core.dirs().data_dir.join("transfers.json");
    assert!(queue_file.is_file());

    // After the restart both tasks are back, stopped, with the same ids.
    let h = h.restart();
    let restored = h.tasks();
    assert_eq!(
        restored
            .iter()
            .map(|task| task["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [big_id.as_str(), small_id.as_str()]
    );
    for task in &restored {
        assert_eq!(task["state"], "stopped");
        assert_eq!(task["speed"], 0);
        assert_eq!(task["eta"], Value::Null);
        assert_eq!(task["account_id"], id.as_str());
    }
    let big_task = h.task(&big_id);
    assert_eq!(big_task["bytes_total"], 15 * MIB);
    assert!(big_task["bytes_done"].as_u64().unwrap() >= 5 * MIB as u64);
    assert_eq!(
        h.core.queue().resume_state(&big_id).unwrap().unwrap()["upload_id"],
        upload_id.as_str()
    );
    assert_eq!(h.core.queue().resume_state(&small_id).unwrap(), None);
    assert!(!block_on(server.exists(&bucket, "keep/big.bin")));
    assert_eq!(block_on(server.pending_uploads(&bucket, "keep/big.bin")), 1);

    // The settings are the saved ones; lifting the cap and starting the
    // tasks finishes them, continuing the same multipart upload.
    assert_eq!(h.core.limiter().rate(), NonZeroU64::new(2048 * 1024));
    let updates = h.events(TRANSFER_UPDATE);
    unthrottle(&h);
    h.ok("start_all", json!({}));
    let done = until_all(&updates, &tasks, "completed");
    assert_eq!(done[&big_id]["bytes_done"], 15 * MIB);
    assert_eq!(done[&small_id]["bytes_done"], 2048);
    assert!(
        block_on(server.get(&bucket, "keep/big.bin"))
            == fs::read(local.path().join("big.bin")).unwrap(),
        "the resumed upload differs from the file"
    );
    assert_eq!(
        block_on(server.get(&bucket, "keep/small.txt")),
        fs::read(local.path().join("small.txt")).unwrap()
    );
    assert_eq!(block_on(server.pending_uploads(&bucket, "keep/big.bin")), 0);
    assert_eq!(h.core.queue().resume_state(&big_id).unwrap(), None);

    // Finished tasks are kept across a restart until they are cleared.
    let h = h.restart();
    let finished = h.tasks();
    assert_eq!(finished.len(), 2);
    assert!(finished.iter().all(|task| task["state"] == "completed"));
    h.ok("clear_finished", json!({}));
    let h = h.restart();
    assert!(h.tasks().is_empty());
}

#[test]
fn cancelling_after_a_restart_aborts_the_multipart_upload() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    slow_multipart(&h);
    let local = tempfile::tempdir().unwrap();
    let big = write(&local.path().join("big.bin"), payload(15 * MIB, 5));
    let queued = h.ok(
        "enqueue_upload",
        json!({ "account_id": id, "local_paths": [big], "bucket": bucket, "prefix": "" }),
    );
    let task = ids(&queued).remove(0);
    first_part(&h, &task);
    assert_eq!(block_on(server.pending_uploads(&bucket, "big.bin")), 1);

    // The restarted app still knows the upload: cancelling the stopped task
    // asks the account's session for the S3 handle and aborts it.
    let h = h.restart();
    assert_eq!(h.task(&task)["state"], "stopped");
    h.ok("cancel_task", json!({ "id": task }));
    let cancelled = h.wait_state(&task, "cancelled");
    assert_eq!(cancelled["error"], Value::Null);
    assert_eq!(block_on(server.pending_uploads(&bucket, "big.bin")), 0);
    assert!(!block_on(server.exists(&bucket, "big.bin")));
    assert_eq!(h.core.queue().resume_state(&task).unwrap(), None);
}

#[test]
fn a_damaged_queue_file_stops_the_startup_and_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (parts, _) = core_parts(dir.path(), Arc::new(MemoryStore::new()));
    let queue_file = parts.dirs.data_dir.join("transfers.json");
    fs::write(&queue_file, b"{ \"version\": 1, \"tasks\": [").unwrap();

    let error = Core::new(parts)
        .err()
        .expect("a damaged queue file must stop the startup");
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(error.to_string().contains("transfers.json"), "{error}");
    assert_eq!(
        fs::read(&queue_file).unwrap(),
        b"{ \"version\": 1, \"tasks\": ["
    );
}

#[test]
fn a_queue_file_that_cannot_be_saved_is_reported_in_the_event_log() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let entries = h.events(LOG_ENTRY);

    // A folder where the queue file belongs: the rename of every save fails.
    let queue_file = h.core.dirs().data_dir.join("transfers.json");
    fs::create_dir(&queue_file).unwrap();
    h.ok(
        "enqueue_delete",
        json!({ "account_id": id, "bucket": bucket, "keys": ["x.txt"] }),
    );

    let entry = loop {
        let entry = next(&entries);
        if entry["source"] == "bucketbridge::transfer" {
            break entry;
        }
    };
    assert_eq!(entry["level"], "error");
    assert!(
        entry["message"]
            .as_str()
            .unwrap()
            .contains("transfers.json"),
        "{entry}"
    );
    let logged = h.ok(
        "get_log",
        json!({ "levels": ["error"], "text": "transfer", "limit": null }),
    );
    assert_eq!(logged.as_array().unwrap().len(), 1);
    // The queue keeps working in memory.
    assert_eq!(h.tasks().len(), 1);
}

#[test]
fn the_overwrite_policy_skips_asks_and_renames() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a.txt", b"old".to_vec()));
    let local = tempfile::tempdir().unwrap();
    let path = write(&local.path().join("a.txt"), b"new".to_vec());

    // The defaults: overwrite, and keep modification times.
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(settings["transfer"]["if_exists"], "overwrite");
    assert_eq!(settings["data_integrity"]["preserve_mtime"], true);

    let upload = |extra: Value| {
        let mut args = json!({
            "account_id": id,
            "local_paths": [path],
            "bucket": bucket,
            "prefix": "",
        });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        h.ok("enqueue_upload", args)[0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // A per-call override: the task ends skipped, with the reason.
    let skipped = upload(json!({ "if_exists": "skip" }));
    let task = h.wait_state(&skipped, "skipped");
    assert!(task["error"].as_str().unwrap().contains("already exists"));
    assert_eq!(block_on(server.get(&bucket, "a.txt")), b"old");

    // `ask`: the task waits, the question is announced and listed, and the
    // answer decides.
    let conflicts = h.events(TRANSFER_CONFLICT);
    let asking = upload(json!({ "if_exists": "ask" }));
    let asked = next(&conflicts);
    assert_eq!(asked["type"], "asked");
    assert_eq!(asked["task_id"], asking);
    assert_eq!(
        asked["conflict"]["destination"],
        format!("s3://{bucket}/a.txt")
    );
    let listed = h.ok("list_conflicts", json!({}));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(h.task(&asking)["state"], "running");
    h.ok(
        "answer_conflict",
        json!({ "task_id": asking, "choice": "rename", "apply_to_batch": false }),
    );
    assert_eq!(next(&conflicts)["type"], "resolved");
    h.wait_state(&asking, "completed");
    assert_eq!(block_on(server.get(&bucket, "a (1).txt")), b"new");
    assert_eq!(block_on(server.get(&bucket, "a.txt")), b"old");
    assert_eq!(
        h.code(
            "answer_conflict",
            json!({ "task_id": asking, "choice": "skip", "apply_to_batch": false })
        ),
        "not_found"
    );

    // The setting is the default of calls that name no policy.
    h.set_setting("transfer", "if_exists", json!("skip"));
    let by_setting = upload(json!({}));
    h.wait_state(&by_setting, "skipped");
    h.set_setting("transfer", "if_exists", json!("rename"));
    let renamed = upload(json!({}));
    h.wait_state(&renamed, "completed");
    assert_eq!(block_on(server.get(&bucket, "a (2).txt")), b"new");
}

#[test]
fn copies_and_moves_can_name_another_destination_account() {
    let (first, second) = (start_s3(), start_s3());
    let h = Harness::new();
    let one = h.add_account(&first, "First");
    let two = h.add_account(&second, "Second");
    let src = block_on(first.new_bucket());
    let dst = block_on(second.new_bucket());
    block_on(first.put(&src, "d/a.txt", b"A".to_vec()));
    block_on(first.put(&src, "d/b.txt", b"B".to_vec()));

    let tasks = h.ok(
        "enqueue_copy",
        json!({
            "account_id": one,
            "src_bucket": src,
            "keys": ["d/"],
            "dst_bucket": dst,
            "dst_prefix": "in/",
            "dst_account_id": two,
        }),
    );
    for task in tasks.as_array().unwrap() {
        h.wait_state(task["id"].as_str().unwrap(), "completed");
    }
    assert_eq!(block_on(second.get(&dst, "in/d/a.txt")), b"A");
    assert!(block_on(first.exists(&src, "d/a.txt")));

    let tasks = h.ok(
        "enqueue_move",
        json!({
            "account_id": one,
            "src_bucket": src,
            "keys": ["d/"],
            "dst_bucket": dst,
            "dst_prefix": "moved/",
            "dst_account_id": two,
        }),
    );
    for task in tasks.as_array().unwrap() {
        h.wait_state(task["id"].as_str().unwrap(), "completed");
    }
    assert_eq!(block_on(second.get(&dst, "moved/d/b.txt")), b"B");
    assert!(!block_on(first.exists(&src, "d/b.txt")));

    // An unknown destination account rejects the command.
    assert_eq!(
        h.code(
            "enqueue_copy",
            json!({
                "account_id": one,
                "src_bucket": src,
                "keys": ["x"],
                "dst_bucket": dst,
                "dst_prefix": "",
                "dst_account_id": "no-such-account",
            }),
        ),
        "not_found"
    );
}
