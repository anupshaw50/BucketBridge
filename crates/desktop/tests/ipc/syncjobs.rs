//! Saved sync job commands, `run_sync_job`, the sync options of `sync_analyze`,
//! S3 to S3 sync and the `sync://report` event.

use std::fs;

use bucketbridge_desktop::{SYNC_JOBS_CHANGED, SYNC_PROGRESS, SYNC_REPORT};
use serde_json::{Value, json};

use crate::harness::*;

fn job(account: &str, name: &str, local: &str, s3: &str) -> Value {
    json!({
        "id": "",
        "name": name,
        "account_id": account,
        "direction": "upload",
        "local_root": local,
        "s3_prefix": s3,
        "delete": false,
        "options": {
            "exclude": ["*.tmp"],
        },
    })
}

#[test]
fn jobs_are_saved_listed_updated_deleted_and_announced() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let changed = h.events(SYNC_JOBS_CHANGED);
    assert_eq!(h.ok("list_sync_jobs", json!({})), json!([]));

    let local = tempfile::tempdir().unwrap();
    let local_root = local.path().to_str().unwrap().to_owned();
    let saved = h.ok(
        "save_sync_job",
        json!({ "job": job(&id, "  Nightly  ", &local_root, "s3://bucket/backup") }),
    );
    assert_eq!(saved["name"], "Nightly");
    assert_eq!(saved["s3_prefix"], "s3://bucket/backup/");
    assert_eq!(saved["options"]["exclude"], json!(["*.tmp"]));
    // Options not given take their defaults.
    assert_eq!(saved["options"]["skip_empty_folders"], true);
    assert_eq!(saved["options"]["skip_duplicate_content"], false);
    assert_eq!(saved["options"]["use_cache"], true);
    assert_eq!(saved["options"]["parallelism"], 4);
    assert_eq!(next(&changed).as_array().unwrap().len(), 1);
    let job_id = saved["id"].as_str().unwrap().to_owned();
    assert!(!job_id.is_empty());

    assert_eq!(h.ok("list_sync_jobs", json!({})), json!([saved]));

    // Saving with the id replaces the job.
    let mut edited = saved.clone();
    edited["delete"] = json!(true);
    edited["name"] = json!("Nightly backup");
    let again = h.ok("save_sync_job", json!({ "job": edited }));
    assert_eq!(again["id"], job_id.as_str());
    assert_eq!(again["delete"], true);
    next(&changed);

    // Errors.
    assert_eq!(
        h.code(
            "save_sync_job",
            json!({ "job": job(&id, "nightly BACKUP", &local_root, "s3://bucket/x") })
        ),
        "conflict"
    );
    assert_eq!(
        h.code(
            "save_sync_job",
            json!({ "job": job("nope", "other", &local_root, "s3://bucket/x") })
        ),
        "not_found"
    );
    assert_eq!(
        h.code(
            "save_sync_job",
            json!({ "job": job(&id, " ", &local_root, "s3://bucket/x") })
        ),
        "usage"
    );
    assert_eq!(
        h.code("delete_sync_job", json!({ "id": "missing" })),
        "not_found"
    );
    assert_eq!(
        h.code("run_sync_job", json!({ "id": "missing" })),
        "not_found"
    );

    let left = h.ok("delete_sync_job", json!({ "id": job_id }));
    assert_eq!(left, json!([]));
    assert_eq!(next(&changed), json!([]));
}

#[test]
fn running_a_job_analyses_with_its_filters_and_the_plan_executes() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("keep.txt"), b"keep").unwrap();
    fs::write(local.path().join("scratch.tmp"), b"tmp").unwrap();
    fs::create_dir(local.path().join("cache")).unwrap();
    fs::write(local.path().join("cache").join("c.bin"), b"c").unwrap();
    let rules = local.path().parent().unwrap().join("bb-job-rules.txt");
    fs::write(&rules, "# folders\ncache/\n").unwrap();
    let local_root = local.path().to_str().unwrap().to_owned();

    let mut definition = job(&id, "Photos", &local_root, &format!("s3://{bucket}/job"));
    definition["options"]["exclude_file"] = json!(rules.to_str().unwrap());
    let saved = h.ok("save_sync_job", json!({ "job": definition }));

    let plan = h.ok("run_sync_job", json!({ "id": saved["id"] }));
    let paths: Vec<&str> = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["rel_path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["keep.txt"]);
    assert!(!block_on(server.exists(&bucket, "job/keep.txt")));

    let progress = h.events(SYNC_PROGRESS);
    let report = h.events(SYNC_REPORT);
    h.ok("sync_execute", json!({ "plan_id": plan["id"] }));
    until_finished(&progress);
    let report = next(&report);
    assert_eq!(report["plan_id"], plan["id"]);
    assert_eq!(report["upload"]["files"], 1);
    assert_eq!(report["upload"]["bytes"], 4);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["cancelled"], false);
    assert!(report["duration_ms"].is_u64());
    assert_eq!(block_on(server.get(&bucket, "job/keep.txt")), b"keep");
    assert!(!block_on(server.exists(&bucket, "job/scratch.tmp")));
}

#[test]
fn a_missing_exclusion_file_fails_the_analysis_instead_of_syncing_everything() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("a.txt"), b"a").unwrap();
    let analyze = json!({
        "account_id": id,
        "local_root": local.path().to_str().unwrap(),
        "s3_prefix": format!("s3://{bucket}/x"),
        "direction": "upload",
        "delete": false,
        "options": { "exclude_file": local.path().join("none.txt").to_str().unwrap() },
    });
    assert_eq!(h.code("sync_analyze", analyze), "not_found");
}

#[test]
fn s3_to_s3_sync_through_the_commands() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let source = block_on(server.new_bucket());
    let destination = block_on(server.new_bucket());
    block_on(server.put(&source, "in/a.txt", "alpha"));
    block_on(server.put(&source, "in/sub/b.txt", "bravo"));

    let plan = h.ok(
        "sync_analyze",
        json!({
            "account_id": id,
            "local_root": "",
            "s3_prefix": format!("s3://{destination}/out"),
            "source_prefix": format!("s3://{source}/in"),
            "direction": "s3_to_s3",
            "delete": false,
        }),
    );
    assert_eq!(plan["direction"], "s3_to_s3");
    assert_eq!(plan["source_prefix"], format!("s3://{source}/in/"));
    assert_eq!(plan["rows"].as_array().unwrap().len(), 2);
    assert!(!block_on(server.exists(&destination, "out/a.txt")));

    let progress = h.events(SYNC_PROGRESS);
    h.ok("sync_execute", json!({ "plan_id": plan["id"] }));
    let finished = until_finished(&progress);
    assert_eq!(finished["done"], 2);
    assert_eq!(
        block_on(server.get(&destination, "out/sub/b.txt")),
        b"bravo"
    );

    // Without a source the command refuses.
    let err = h.code(
        "sync_analyze",
        json!({
            "account_id": id, "local_root": "", "s3_prefix": format!("s3://{destination}/o"),
            "direction": "s3_to_s3", "delete": false,
        }),
    );
    assert_eq!(err, "usage");
}
