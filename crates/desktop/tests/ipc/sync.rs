//! Sync commands and the `sync://progress` / `sync://row` events.

use std::fs;
use std::time::{Duration, Instant};

use bucketbridge_desktop::{SYNC_PROGRESS, SYNC_ROW};
use serde_json::{Value, json};

use crate::harness::*;

#[test]
fn analyze_execute_and_events() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("a.txt"), b"alpha").unwrap();
    fs::create_dir(local.path().join("sub")).unwrap();
    fs::write(local.path().join("sub").join("b.txt"), b"bravo").unwrap();
    let local_root = local.path().to_str().unwrap().to_owned();

    let analyze = json!({
        "account_id": id,
        "local_root": local_root,
        "s3_prefix": format!("s3://{bucket}/backup"),
        "direction": "upload",
        "delete": false,
    });
    let plan = h.ok("sync_analyze", analyze.clone());
    assert_eq!(
        keys(&plan),
        [
            "account_id",
            "created_at",
            "delete",
            "direction",
            "id",
            "local_root",
            "rows",
            "s3_prefix"
        ]
    );
    assert_eq!(plan["account_id"], id.as_str());
    assert_eq!(plan["s3_prefix"], format!("s3://{bucket}/backup/"));
    assert_eq!(plan["direction"], "upload");
    assert_eq!(plan["delete"], false);
    let rows = plan["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let paths: Vec<&str> = rows
        .iter()
        .map(|r| r["rel_path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["a.txt", "sub/b.txt"]);
    let row = &rows[0];
    assert_eq!(
        keys(row),
        [
            "action",
            "done",
            "error",
            "local_meta",
            "rel_path",
            "s3_meta",
            "target"
        ]
    );
    assert_eq!(row["action"], "copy");
    assert_eq!(row["target"], "remote");
    assert_eq!(row["done"], false);
    assert_eq!(row["error"], Value::Null);
    let plan_id = plan["id"].as_str().unwrap().to_owned();

    // The analysis is a dry run.
    assert!(!block_on(server.exists(&bucket, "backup/a.txt")));
    assert_eq!(h.ok("sync_get", json!({ "plan_id": plan_id })), plan);

    let progress = h.events(SYNC_PROGRESS);
    let row_events = h.events(SYNC_ROW);
    assert_eq!(
        h.ok("sync_execute", json!({ "plan_id": plan_id })),
        Value::Null
    );
    let finished = until_finished(&progress);
    assert_eq!(
        finished,
        json!({
            "plan_id": plan_id, "total": 2, "done": 2, "failed": 0, "finished": true
        })
    );
    let mut indexes = Vec::new();
    for _ in 0..2 {
        let event = next(&row_events);
        assert_eq!(keys(&event), ["index", "plan_id", "row"]);
        assert_eq!(event["plan_id"], plan_id.as_str());
        assert_eq!(event["row"]["done"], true);
        indexes.push(event["index"].as_u64().unwrap());
    }
    indexes.sort_unstable();
    assert_eq!(indexes, [0, 1]);
    assert_eq!(block_on(server.get(&bucket, "backup/a.txt")), b"alpha");
    assert_eq!(block_on(server.get(&bucket, "backup/sub/b.txt")), b"bravo");

    let stored = h.ok("sync_get", json!({ "plan_id": plan_id }));
    assert!(
        stored["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["done"] == true)
    );

    // Running it again resumes nothing and still reports the end once.
    h.ok("sync_execute", json!({ "plan_id": plan_id }));
    assert_eq!(until_finished(&progress)["done"], 2);

    // A second analysis sees both sides as equal.
    let again = h.ok("sync_analyze", analyze);
    assert!(
        again["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["action"] == "equal" && row["done"] == true)
    );
}

#[test]
fn sync_errors_use_the_contract_codes() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let local = tempfile::tempdir().unwrap();
    let local_root = local.path().to_str().unwrap().to_owned();
    let analyze = |local_root: &str, s3_prefix: &str, direction: &str, delete: bool| {
        json!({
            "account_id": id, "local_root": local_root, "s3_prefix": s3_prefix,
            "direction": direction, "delete": delete
        })
    };
    let prefix = format!("s3://{bucket}/data/");

    assert_eq!(
        h.code(
            "sync_analyze",
            analyze(&local_root, &prefix, "bidirectional", true)
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "sync_analyze",
            analyze("relative/folder", &prefix, "upload", false)
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "sync_analyze",
            analyze(&local_root, "not-an-s3-uri", "upload", false)
        ),
        "usage"
    );

    let unknown = "00000000-0000-4000-8000-000000000000";
    for command in ["sync_get", "sync_execute", "sync_cancel"] {
        assert_eq!(
            h.code(command, json!({ "plan_id": unknown })),
            "not_found",
            "{command}"
        );
        assert_eq!(
            h.code(command, json!({ "plan_id": "not-a-plan-id" })),
            "usage",
            "{command}"
        );
    }

    // Cancelling a plan that is not running changes nothing.
    let plan = h.ok(
        "sync_analyze",
        analyze(&local_root, &prefix, "download", false),
    );
    let plan_id = plan["id"].as_str().unwrap();
    assert_eq!(
        h.ok("sync_cancel", json!({ "plan_id": plan_id })),
        Value::Null
    );
}

#[test]
fn shutdown_cancels_running_syncs() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("big.bin"), vec![7u8; 256 * 1024]).unwrap();
    // 1 KiB/s: the upload would take minutes.
    h.set_setting("transfer", "bandwidth_limit_kibps", json!(1));

    let plan = h.ok(
        "sync_analyze",
        json!({
            "account_id": id,
            "local_root": local.path().to_str().unwrap(),
            "s3_prefix": format!("s3://{bucket}/slow/"),
            "direction": "upload",
            "delete": false,
        }),
    );
    let plan_id = plan["id"].as_str().unwrap().to_owned();
    let progress = h.events(SYNC_PROGRESS);
    h.ok("sync_execute", json!({ "plan_id": plan_id }));
    let started = next(&progress);
    assert_eq!(started["finished"], false);
    // A plan runs once at a time.
    assert_eq!(
        h.code("sync_execute", json!({ "plan_id": plan_id })),
        "conflict"
    );

    let begun = Instant::now();
    h.core.shutdown(Duration::from_secs(30));
    assert!(
        begun.elapsed() < Duration::from_secs(30),
        "the run did not stop"
    );
    let finished = until_finished(&progress);
    assert_eq!(finished["done"], 0);
    assert!(!block_on(server.exists(&bucket, "slow/big.bin")));
    let stored = h.ok("sync_get", json!({ "plan_id": plan_id }));
    assert_eq!(stored["rows"][0]["done"], false);
    assert_eq!(stored["rows"][0]["error"], Value::Null);
}
