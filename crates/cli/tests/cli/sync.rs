//! `bb sync`: uploads, downloads and two-way syncs of a local folder with an
//! S3 prefix, deletes, dry runs, failing rows, resuming a stored plan and
//! Ctrl-C.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use bucketbridge_core::model::SyncDirection;
use bucketbridge_core::sync::{AnalyzeRequest, SyncStore, analyze};
use bucketbridge_core::{Dirs, S3Uri};
use serde_json::Value;

use super::*;

/// A plan ID that no test stores.
const UNKNOWN_PLAN: &str = "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35";

/// Writes files (relative path, contents) below `dir`.
fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
}

fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, body)| ((*key).to_owned(), (*body).to_owned()))
        .collect()
}

/// Every file below `dir`: `/`-separated relative path to contents.
fn files(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, found);
            } else {
                let rel: Vec<String> = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                found.insert(rel.join("/"), fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut found);
    }
    found
}

/// Every object of a bucket: key to contents.
async fn objects(env: &Env, bucket: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for key in env.keys(bucket).await {
        let body = env.s3.get(bucket, &key).await;
        found.insert(key, String::from_utf8(body).unwrap());
    }
    found
}

/// The plan files left in the plan store of the test's config directory.
fn stored_plans(env: &Env) -> Vec<String> {
    let dir = env.config.path().join("data").join("sync");
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json") || name.ends_with(".journal"))
        .collect()
}

/// Whether the plan `id` is still stored.
fn is_stored(env: &Env, id: &str) -> bool {
    stored_plans(env).contains(&format!("{id}.json"))
}

/// The row records of a `--json` run, sorted by path.
fn row_records(out: &Output) -> Vec<Value> {
    let mut rows: Vec<Value> = out
        .records()
        .into_iter()
        .filter(|record| record["type"] == "row")
        .collect();
    rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    rows
}

/// The summary record of a `--json` run: the only one, and the last record.
fn summary_record(out: &Output) -> Value {
    let records = out.records();
    let last = records.last().expect("a summary record").clone();
    assert_eq!(last["type"], "summary", "{}", out.stdout);
    let summaries = records.iter().filter(|r| r["type"] == "summary").count();
    assert_eq!(summaries, 1, "{}", out.stdout);
    last
}

/// `[equal, copied, deleted, failed, pending]` of a summary record.
fn counts(summary: &Value) -> [u64; 5] {
    ["equal", "copied", "deleted", "failed", "pending"].map(|field| {
        summary[field]
            .as_u64()
            .unwrap_or_else(|| panic!("{field}: {summary}"))
    })
}

/// `path action target` of each row, to compare plans at a glance.
fn describe(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{} {} {}",
                row["path"].as_str().unwrap(),
                row["action"].as_str().unwrap(),
                row["target"].as_str().unwrap_or("-")
            )
        })
        .collect()
}

/// Sets the modification time of a file.
fn set_modified(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn hour() -> Duration {
    Duration::from_secs(3600)
}

/// `args` plus more arguments.
fn with<'a>(args: &[&'a str], more: &[&'a str]) -> Vec<&'a str> {
    args.iter().chain(more).copied().collect()
}

/// Runs `args` twice more (with `--json` and without) and expects nothing to
/// be left to do: only a summary, and no plan stored.
fn assert_nothing_to_do(env: &Env, args: &[&str], equal: u64) {
    let out = env.run(&with(args, &["--json"]));
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 1, "{}", out.stdout);
    let summary = &records[0];
    assert_eq!(summary["type"], "summary");
    assert_eq!(counts(summary), [equal, 0, 0, 0, 0], "{summary}");
    assert!(summary["plan_id"].is_null() && summary["cancelled"] == false);

    let out = env.run(args);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!("0 copied, 0 deleted, {equal} already in sync")
    );
    assert!(out.stderr.is_empty(), "{}", out.stderr);
    assert!(stored_plans(env).is_empty());
}

#[tokio::test]
async fn upload_makes_the_prefix_match_the_folder() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("photos");
    write_files(
        &dir,
        &[
            ("a.txt", "alpha"),
            ("docs/b.txt", "bravo"),
            ("docs/deep/c.txt", "charlie"),
        ],
    );
    let local = dir.to_str().unwrap();
    let prefix = format!("s3://{bucket}/backup");

    // One record per file, and a summary. The default direction is upload.
    let out = env.run(&["sync", local, &prefix, "--json"]);
    out.expect_code(0);
    assert!(out.stderr.is_empty(), "{}", out.stderr);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        [
            "a.txt copy remote",
            "docs/b.txt copy remote",
            "docs/deep/c.txt copy remote"
        ]
    );
    assert!(rows.iter().all(|row| {
        row["done"] == true
            && row["dry_run"] == false
            && row["error"].is_null()
            && row["plan_id"].is_string()
            && row["plan_id"] == rows[0]["plan_id"]
    }));
    assert_eq!(rows[0]["size"], 5);
    assert_eq!(rows[0]["local"], dir.join("a.txt").to_str().unwrap());
    assert_eq!(rows[0]["remote"], format!("s3://{bucket}/backup/a.txt"));
    let summary = summary_record(&out);
    assert_eq!(summary["direction"], "upload");
    assert_eq!(summary["delete"], false);
    assert_eq!(summary["local"], local);
    assert_eq!(summary["remote"], format!("s3://{bucket}/backup/"));
    assert_eq!(summary["dry_run"], false);
    assert_eq!(summary["cancelled"], false);
    assert_eq!(counts(&summary), [0, 3, 0, 0, 0]);
    // A plan that ran to the end is not kept.
    assert!(summary["plan_id"].is_null());
    assert!(stored_plans(&env).is_empty());
    assert_eq!(
        objects(&env, &bucket).await,
        map(&[
            ("backup/a.txt", "alpha"),
            ("backup/docs/b.txt", "bravo"),
            ("backup/docs/deep/c.txt", "charlie"),
        ])
    );
    // The local folder is untouched.
    assert_eq!(files(&dir).len(), 3);

    // Only what changed is uploaded: a changed size and a new file.
    fs::write(dir.join("a.txt"), "alpha, changed").unwrap();
    write_files(&dir, &[("new.txt", "n")]);
    let out = env.run(&["sync", local, &prefix]);
    out.expect_code(0);
    let mut lines: Vec<String> = out.lines().into_iter().map(str::to_owned).collect();
    assert_eq!(
        lines.pop().as_deref(),
        Some("2 copied, 0 deleted, 2 already in sync")
    );
    lines.sort();
    assert_eq!(
        lines,
        [
            format!(
                "uploaded {} to s3://{bucket}/backup/a.txt",
                dir.join("a.txt").display()
            ),
            format!(
                "uploaded {} to s3://{bucket}/backup/new.txt",
                dir.join("new.txt").display()
            ),
        ]
    );
    assert_eq!(env.s3.get(&bucket, "backup/a.txt").await, b"alpha, changed");

    // --quiet prints nothing for a successful run.
    fs::write(dir.join("new.txt"), "n, changed").unwrap();
    let out = env.run(&["sync", local, &prefix, "-q"]);
    out.expect_code(0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert_eq!(env.s3.get(&bucket, "backup/new.txt").await, b"n, changed");

    // A whole bucket: the keys are the relative paths.
    let whole = env.s3.new_bucket().await;
    env.run(&["sync", local, &format!("s3://{whole}")])
        .expect_code(0);
    assert_eq!(
        env.keys(&whole).await,
        ["a.txt", "docs/b.txt", "docs/deep/c.txt", "new.txt"]
    );
}

#[tokio::test]
async fn download_makes_the_folder_match_the_prefix() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "backup/a.txt", "alpha").await;
    env.s3.put(&bucket, "backup/docs/b.txt", "bravo").await;
    env.s3
        .put(&bucket, "other/x.txt", "outside the prefix")
        .await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("restore");
    write_files(&dir, &[("mine.txt", "only here")]);
    let local = dir.to_str().unwrap();

    let out = env.run(&[
        "sync",
        local,
        &format!("s3://{bucket}/backup/"),
        "--direction",
        "download",
        "--json",
    ]);
    out.expect_code(0);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        ["a.txt copy local", "docs/b.txt copy local"]
    );
    assert!(rows.iter().all(|row| row["done"] == true));
    assert_eq!(rows[1]["size"], 5);
    let summary = summary_record(&out);
    assert_eq!(summary["direction"], "download");
    assert_eq!(counts(&summary), [0, 2, 0, 0, 0]);
    // Without --delete, what only exists locally stays.
    assert_eq!(
        files(&dir),
        map(&[
            ("a.txt", "alpha"),
            ("docs/b.txt", "bravo"),
            ("mine.txt", "only here")
        ])
    );
    assert!(!dir.join("a.txt.bbpart").exists());

    // A folder that does not exist yet is created, with its parents.
    let fresh = work.path().join("new").join("deeper");
    env.run(&[
        "sync",
        fresh.to_str().unwrap(),
        &format!("s3://{bucket}/backup"),
        "--direction",
        "download",
    ])
    .expect_code(0);
    assert_eq!(
        files(&fresh),
        map(&[("a.txt", "alpha"), ("docs/b.txt", "bravo")])
    );
}

#[tokio::test]
async fn a_second_run_finds_nothing_to_do() {
    let env = Env::start().await;
    let work = tempfile::tempdir().unwrap();

    // Upload.
    let bucket = env.s3.new_bucket().await;
    let up = work.path().join("up");
    write_files(&up, &[("a.txt", "alpha"), ("docs/b.txt", "bravo")]);
    let up_args = ["sync", up.to_str().unwrap(), &format!("s3://{bucket}/p")];
    env.run(&up_args).expect_code(0);
    assert_nothing_to_do(&env, &up_args, 2);

    // Download.
    let down = work.path().join("down");
    let down_args = [
        "sync",
        down.to_str().unwrap(),
        &format!("s3://{bucket}/p"),
        "--direction",
        "download",
    ];
    env.run(&down_args).expect_code(0);
    assert_eq!(files(&down).len(), 2);
    assert_nothing_to_do(&env, &down_args, 2);

    // Both ways: one new file on each side.
    let both = work.path().join("both");
    write_files(&both, &[("here.txt", "local only")]);
    let other = env.s3.new_bucket().await;
    env.s3.put(&other, "q/there.txt", "remote only").await;
    let both_args = [
        "sync",
        both.to_str().unwrap(),
        &format!("s3://{other}/q"),
        "--direction",
        "bidirectional",
    ];
    env.run(&both_args).expect_code(0);
    assert_eq!(
        files(&both),
        map(&[("here.txt", "local only"), ("there.txt", "remote only")])
    );
    assert_eq!(
        objects(&env, &other).await,
        map(&[("q/here.txt", "local only"), ("q/there.txt", "remote only")])
    );
    assert_nothing_to_do(&env, &both_args, 2);
}

#[tokio::test]
async fn delete_removes_what_only_the_target_side_has() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();

    // Upload: the prefix is made to match the folder.
    let dir = work.path().join("src");
    write_files(&dir, &[("keep.txt", "kept"), ("sub/new.txt", "new")]);
    let local = dir.to_str().unwrap();
    let prefix = format!("s3://{bucket}/backup");
    env.run(&["sync", local, &prefix]).expect_code(0);
    for key in [
        "backup/stale.txt",
        "backup/old/stale.txt",
        "backup-old/sibling.txt",
        "other/x.txt",
    ] {
        env.s3.put(&bucket, key, "stale").await;
    }
    let before = objects(&env, &bucket).await;

    // Without --delete the extra objects stay.
    let out = env.run(&["sync", local, &prefix, "--json"]);
    out.expect_code(0);
    assert_eq!(counts(&summary_record(&out)), [2, 0, 0, 0, 0]);
    assert_eq!(objects(&env, &bucket).await, before);

    let out = env.run(&["sync", local, &prefix, "--delete", "--json"]);
    out.expect_code(0);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        ["old/stale.txt delete remote", "stale.txt delete remote"]
    );
    assert!(rows.iter().all(|row| row["done"] == true));
    assert_eq!(rows[1]["remote"], format!("s3://{bucket}/backup/stale.txt"));
    assert_eq!(rows[1]["size"], 5);
    let summary = summary_record(&out);
    assert_eq!(counts(&summary), [2, 0, 2, 0, 0]);
    assert_eq!(summary["delete"], true);
    // Only the prefix is touched: not `backup-old/`, not `other/`.
    assert_eq!(
        objects(&env, &bucket).await,
        map(&[
            ("backup-old/sibling.txt", "stale"),
            ("backup/keep.txt", "kept"),
            ("backup/sub/new.txt", "new"),
            ("other/x.txt", "stale"),
        ])
    );

    // Download: the folder is made to match the prefix, and the folders that
    // deleting emptied go away.
    let mirror = work.path().join("mirror");
    let download = [
        "sync",
        mirror.to_str().unwrap(),
        &format!("s3://{bucket}/backup"),
        "--direction",
        "download",
    ];
    env.run(&download).expect_code(0);
    write_files(
        &mirror,
        &[
            ("extra.txt", "extra"),
            ("sub/extra.txt", "extra"),
            ("emptied/only.txt", "x"),
            ("deep/er/only.txt", "x"),
        ],
    );
    let with_extras = files(&mirror);
    env.run(&download).expect_code(0);
    assert_eq!(files(&mirror), with_extras, "no --delete, no deletes");

    let out = env.run(&with(&download, &["--delete", "--json"]));
    out.expect_code(0);
    assert_eq!(
        describe(&row_records(&out)),
        [
            "deep/er/only.txt delete local",
            "emptied/only.txt delete local",
            "extra.txt delete local",
            "sub/extra.txt delete local"
        ]
    );
    assert_eq!(
        files(&mirror),
        map(&[("keep.txt", "kept"), ("sub/new.txt", "new")])
    );
    assert!(!mirror.join("deep").exists() && !mirror.join("emptied").exists());
    assert!(mirror.join("sub").is_dir() && mirror.is_dir());
    // Deleting locally never touches the bucket.
    assert_eq!(objects(&env, &bucket).await.len(), 4);
}

#[tokio::test]
async fn bidirectional_copies_the_newer_file_to_the_other_side() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("both");
    write_files(
        &dir,
        &[
            ("only-local.txt", "local"),
            ("newer-local.txt", "the local copy is newer"),
            ("newer-remote.txt", "old"),
        ],
    );
    env.s3.put(&bucket, "p/only-remote.txt", "remote").await;
    env.s3.put(&bucket, "p/newer-local.txt", "old").await;
    env.s3
        .put(&bucket, "p/newer-remote.txt", "the remote copy is newer")
        .await;
    let now = SystemTime::now();
    set_modified(&dir.join("newer-local.txt"), now + hour());
    set_modified(&dir.join("newer-remote.txt"), now - hour());

    let args = [
        "sync",
        dir.to_str().unwrap(),
        &format!("s3://{bucket}/p"),
        "--direction",
        "bidirectional",
    ];
    let out = env.run(&with(&args, &["--json"]));
    out.expect_code(0);
    assert_eq!(
        describe(&row_records(&out)),
        [
            "newer-local.txt copy remote",
            "newer-remote.txt copy local",
            "only-local.txt copy remote",
            "only-remote.txt copy local"
        ]
    );
    let summary = summary_record(&out);
    assert_eq!(summary["direction"], "bidirectional");
    assert_eq!(counts(&summary), [0, 4, 0, 0, 0]);
    let expected = map(&[
        ("newer-local.txt", "the local copy is newer"),
        ("newer-remote.txt", "the remote copy is newer"),
        ("only-local.txt", "local"),
        ("only-remote.txt", "remote"),
    ]);
    assert_eq!(files(&dir), expected);
    let remote: BTreeMap<String, String> = objects(&env, &bucket)
        .await
        .into_iter()
        .map(|(key, body)| (key.trim_start_matches("p/").to_owned(), body))
        .collect();
    assert_eq!(remote, expected);
    assert_nothing_to_do(&env, &args, 4);
}

#[tokio::test]
async fn bidirectional_conflicts_are_failed_rows_that_are_left_alone() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("both");
    write_files(&dir, &[("conflict.txt", "local edit, longer")]);
    env.s3.put(&bucket, "p/conflict.txt", "remote edit").await;
    // Different sizes and the same time: neither side is clearly newer.
    let head = env
        .s3
        .ops()
        .head_object(&bucket, "p/conflict.txt", None)
        .await
        .unwrap();
    set_modified(
        &dir.join("conflict.txt"),
        SystemTime::from(head.last_modified.unwrap()),
    );
    let local = dir.to_str().unwrap();
    let prefix = format!("s3://{bucket}/p");
    let args = ["sync", local, &prefix, "--direction", "bidirectional"];

    // Nothing else to do: the conflict is reported, nothing is executed and
    // nothing is stored.
    let out = env.run(&with(&args, &["--json"]));
    out.expect_code(2);
    let rows = row_records(&out);
    assert_eq!(describe(&rows), ["conflict.txt failed -"]);
    assert!(rows[0]["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert!(rows[0]["local"].is_null() && rows[0]["remote"].is_null());
    assert!(rows[0]["plan_id"].is_null() && rows[0]["done"] == false);
    let summary = summary_record(&out);
    assert_eq!(counts(&summary), [0, 0, 0, 1, 0]);
    assert!(summary["plan_id"].is_null());
    assert_eq!(
        out.error_record()["message"],
        "1 of 1 files could not be synced"
    );
    // No plan is stored (the listing cache may be written).
    assert!(!env.config.path().join("data").join("sync").exists());

    // A new local file is uploaded all the same; the conflict stays.
    write_files(&dir, &[("fresh.txt", "fresh")]);
    let out = env.run(&with(&args, &["--json"]));
    out.expect_code(2);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        ["conflict.txt failed -", "fresh.txt copy remote"]
    );
    assert_eq!(rows[1]["done"], true);
    assert_eq!(
        out.error_record()["message"],
        "1 of 2 files could not be synced"
    );
    assert_eq!(
        files(&dir),
        map(&[
            ("conflict.txt", "local edit, longer"),
            ("fresh.txt", "fresh")
        ])
    );
    assert_eq!(env.s3.get(&bucket, "p/conflict.txt").await, b"remote edit");
    assert!(env.s3.exists(&bucket, "p/fresh.txt").await);

    // Without --json the failed row is a line on stderr.
    let out = env.run(&args);
    out.expect_code(2);
    assert!(
        out.stderr.contains("bb: cannot sync conflict.txt: "),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .trim_end()
            .ends_with("bb: error: 1 of 1 files could not be synced"),
        "{}",
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        "0 copied, 0 deleted, 1 already in sync, 1 failed"
    );

    // A dry run reports the same problem with the same exit code.
    let out = env.run(&with(&args, &["--dry-run"]));
    out.expect_code(2);
    assert!(out.stderr.contains("cannot sync conflict.txt"));
}

#[tokio::test]
async fn dry_runs_write_nothing() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(
        &dir,
        &[
            ("same.txt", "same"),
            ("only-local.txt", "local"),
            ("sub/changed.txt", "a longer local text"),
        ],
    );
    env.s3.put(&bucket, "p/same.txt", "same").await;
    env.s3.put(&bucket, "p/only-remote.txt", "remote").await;
    env.s3.put(&bucket, "p/sub/changed.txt", "short").await;
    // The local copy of `sub/changed.txt` is the newer one.
    set_modified(
        &dir.join("sub").join("changed.txt"),
        SystemTime::now() + hour(),
    );
    let local = dir.to_str().unwrap();
    let prefix = format!("s3://{bucket}/p");
    let local_before = files(&dir);
    let remote_before = objects(&env, &bucket).await;

    for (direction, delete, expected) in [
        (
            "upload",
            true,
            [
                "only-local.txt copy remote",
                "only-remote.txt delete remote",
                "sub/changed.txt copy remote",
            ],
        ),
        (
            "download",
            true,
            [
                "only-local.txt delete local",
                "only-remote.txt copy local",
                "sub/changed.txt copy local",
            ],
        ),
        (
            "bidirectional",
            false,
            [
                "only-local.txt copy remote",
                "only-remote.txt copy local",
                "sub/changed.txt copy remote",
            ],
        ),
    ] {
        let mut args = vec![
            "sync",
            local,
            &prefix,
            "--direction",
            direction,
            "--dry-run",
        ];
        if delete {
            args.push("--delete");
        }

        let out = env.run(&with(&args, &["--json"]));
        out.expect_code(0);
        let rows = row_records(&out);
        assert_eq!(describe(&rows), expected, "{direction}");
        assert!(
            rows.iter().all(|row| row["dry_run"] == true
                && row["done"] == false
                && row["error"].is_null()
                && row["plan_id"].is_null()),
            "{direction}: {}",
            out.stdout
        );
        let summary = summary_record(&out);
        assert_eq!(summary["dry_run"], true);
        assert!(summary["plan_id"].is_null());
        // Nothing is done; the rows a real run would execute are pending.
        assert_eq!(counts(&summary), [1, 0, 0, 0, 3], "{direction}");
        assert_eq!(summary["delete"], delete);

        // Nothing changed anywhere: not the folder, not the bucket, and not
        // even the plan store.
        assert_eq!(files(&dir), local_before, "{direction}");
        assert_eq!(objects(&env, &bucket).await, remote_before, "{direction}");
        assert!(
            !env.config.path().join("data").exists(),
            "{direction}: the plan store was created"
        );

        // The same in the human format: a line per row and a summary.
        let out = env.run(&args);
        out.expect_code(0);
        assert_eq!(out.lines().len(), 4, "{}", out.stdout);
        assert!(out.lines().iter().all(|line| line.starts_with("would ")));
        assert!(out.stderr.is_empty(), "{}", out.stderr);
        assert_eq!(files(&dir), local_before);
        assert_eq!(objects(&env, &bucket).await, remote_before);
        assert!(!env.config.path().join("data").exists());
    }

    // The exact lines of an upload.
    let out = env.run(&["sync", local, &prefix, "--dry-run", "--delete"]);
    out.expect_code(0);
    let mut lines: Vec<String> = out.lines().into_iter().map(str::to_owned).collect();
    assert_eq!(
        lines.pop().as_deref(),
        Some("would copy 2, delete 1; 1 already in sync")
    );
    lines.sort();
    assert_eq!(
        lines,
        [
            format!("would delete s3://{bucket}/p/only-remote.txt"),
            format!(
                "would upload {} to s3://{bucket}/p/only-local.txt",
                dir.join("only-local.txt").display()
            ),
            format!(
                "would upload {} to s3://{bucket}/p/sub/changed.txt",
                dir.join("sub").join("changed.txt").display()
            ),
        ]
    );
    // --quiet hides a dry run like any other successful change.
    let out = env.run(&["sync", local, &prefix, "--dry-run", "-q"]);
    out.expect_code(0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_file_fails_its_row_and_the_others_complete() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("photos");
    write_files(
        &dir,
        &[("a.txt", "a"), ("locked.txt", "secret"), ("sub/c.txt", "c")],
    );
    let locked = dir.join("locked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&locked).is_ok() {
        // Running as root: permissions do not apply.
        return;
    }
    let local = dir.to_str().unwrap();
    let prefix = format!("s3://{bucket}/");
    let out = env.run(&["sync", local, &prefix, "--json"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

    out.expect_code(2);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        [
            "a.txt copy remote",
            "locked.txt failed -",
            "sub/c.txt copy remote"
        ]
    );
    assert_eq!(rows[0]["done"], true);
    assert_eq!(rows[1]["done"], false);
    // The row names the file; the error says what is wrong with it.
    assert!(rows[1]["error"].as_str().is_some_and(|why| !why.is_empty()));
    assert_eq!(rows[2]["done"], true);
    assert_eq!(counts(&summary_record(&out)), [0, 2, 0, 1, 0]);
    let error = out.error_record();
    assert_eq!(error["code"], "remote");
    assert_eq!(error["message"], "1 of 3 files could not be synced");
    assert_eq!(env.keys(&bucket).await, ["a.txt", "sub/c.txt"]);
    // Every row that could run did; the unplannable one cannot be resumed.
    assert!(stored_plans(&env).is_empty());

    // Without --json: a line per failure on stderr, then the error.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = env.run(&["sync", local, &prefix]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    out.expect_code(2);
    assert_eq!(
        out.stdout.trim(),
        "0 copied, 0 deleted, 2 already in sync, 1 failed"
    );
    assert!(
        out.stderr.contains("bb: cannot sync locked.txt: "),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .trim_end()
            .ends_with("bb: error: 1 of 1 files could not be synced"),
        "{}",
        out.stderr
    );
    // --quiet still reports failures.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = env.run(&["sync", local, &prefix, "-q"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    out.expect_code(2);
    assert!(
        out.stderr.contains("cannot sync locked.txt"),
        "{}",
        out.stderr
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_row_that_fails_while_running_is_kept_for_resume() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "p/ok.txt", "fine").await;
    env.s3
        .put(&bucket, "p/closed/a.txt", "cannot be written")
        .await;
    env.s3.put(&bucket, "p/z.txt", "also fine").await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("restore");
    let closed = dir.join("closed");
    fs::create_dir_all(&closed).unwrap();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(closed.join("probe"), "x").is_ok() {
        // Running as root: permissions do not apply.
        return;
    }
    let args = [
        "sync",
        dir.to_str().unwrap(),
        &format!("s3://{bucket}/p"),
        "--direction",
        "download",
    ];
    let out = env.run(&with(&args, &["--json"]));
    out.expect_code(2);
    let rows = row_records(&out);
    assert_eq!(
        describe(&rows),
        [
            "closed/a.txt copy local",
            "ok.txt copy local",
            "z.txt copy local"
        ]
    );
    // The others completed; the failed row keeps its action.
    let done: Vec<bool> = rows.iter().map(|row| row["done"] == true).collect();
    assert_eq!(done, [false, true, true]);
    assert!(rows[0]["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert!(rows[1]["error"].is_null());
    assert_eq!(
        out.error_record()["message"],
        "1 of 3 files could not be synced"
    );
    let summary = summary_record(&out);
    assert_eq!(counts(&summary), [0, 2, 0, 1, 0]);
    // The plan stays, so that --resume can try the row again.
    let plan_id = summary["plan_id"].as_str().unwrap().to_owned();
    assert_eq!(rows[0]["plan_id"], plan_id);
    assert!(is_stored(&env, &plan_id));
    assert_eq!(
        files(&dir),
        map(&[("ok.txt", "fine"), ("z.txt", "also fine")])
    );

    // Without --json the failure is a line on stderr, and so is the way to
    // try again. (This run analyzes again and stores a plan of its own.)
    let human = env.run(&args);
    human.expect_code(2);
    assert!(
        human.stderr.contains("bb: failed to download "),
        "{}",
        human.stderr
    );
    assert!(
        human.stderr.contains("with: bb sync --resume "),
        "{}",
        human.stderr
    );

    // Make the folder writable and resume the first plan: only the failed
    // row runs.
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).unwrap();
    let out = env.run(&["sync", "--resume", &plan_id, "--json"]);
    out.expect_code(0);
    let rows = row_records(&out);
    assert_eq!(describe(&rows), ["closed/a.txt copy local"]);
    assert_eq!(rows[0]["done"], true);
    assert!(rows[0]["error"].is_null());
    let summary = summary_record(&out);
    // The summary counts the whole plan.
    assert_eq!(counts(&summary), [0, 3, 0, 0, 0]);
    assert!(summary["plan_id"].is_null());
    assert_eq!(
        files(&dir),
        map(&[
            ("closed/a.txt", "cannot be written"),
            ("ok.txt", "fine"),
            ("z.txt", "also fine")
        ])
    );
    assert!(!is_stored(&env, &plan_id));
}

#[tokio::test]
async fn usage_errors_exit_with_1() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a")]);
    let file = dir.join("a.txt");
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");
    let id = UNKNOWN_PLAN;

    // Found while reading the command line or checking the options: before
    // any credentials are needed.
    let config = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (vec!["sync"], "required"),
        (vec!["sync", local], "required"),
        (vec!["sync", &uri, local], "local folder first"),
        (vec!["sync", local, "plain-text"], "not an S3 URI"),
        (vec!["sync", "", &uri], "empty"),
        (
            vec!["sync", local, &uri, "--direction", "sideways"],
            "sideways",
        ),
        (
            vec![
                "sync",
                local,
                &uri,
                "--direction",
                "bidirectional",
                "--delete",
            ],
            "--delete cannot be combined with --direction bidirectional",
        ),
        (vec!["sync", "--resume", "not-a-plan-id"], "not a plan ID"),
        (vec!["sync", "--resume", id, local, &uri], "cannot be used"),
        (vec!["sync", "--resume", id, "--dry-run"], "cannot be used"),
        (vec!["sync", "--resume", id, "--delete"], "cannot be used"),
        (
            vec!["sync", "--resume", id, "--direction", "download"],
            "cannot be used",
        ),
    ] {
        let out = run(bb(config.path()).args(&args));
        out.expect_code(1);
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(out.stderr.contains(expected), "{args:?}: {}", out.stderr);
    }

    // Found by the analysis of the folder.
    let out = env.run(&["sync", file.to_str().unwrap(), &uri]);
    out.expect_code(1);
    assert!(out.stderr.contains("is not a folder"), "{}", out.stderr);
    let out = env.run(&["sync", "--json", file.to_str().unwrap(), &uri]);
    out.expect_code(1);
    assert_eq!(out.error_record()["code"], "usage");
    assert!(out.stdout.is_empty());

    // Missing things are not usage errors: a missing folder to upload, a
    // missing bucket, a plan that does not exist.
    let out = env.run(&["sync", work.path().join("missing").to_str().unwrap(), &uri]);
    out.expect_code(2);
    assert!(out.stderr.contains("does not exist"), "{}", out.stderr);
    let out = env.run(&["sync", local, "s3://no-such-bucket/p"]);
    out.expect_code(2);
    assert!(out.stderr.contains("no-such-bucket"), "{}", out.stderr);
    let out = env.run(&["sync", "--resume", id]);
    out.expect_code(2);
    assert!(
        out.stderr
            .contains(&format!("sync plan {id} does not exist")),
        "{}",
        out.stderr
    );
    let out = env.run(&["sync", "--resume", id, "--json"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");

    // No credentials: an authentication error.
    let out = run(bb(config.path())
        .env("BB_ENDPOINT", env.s3.endpoint())
        .args(["sync", local, &uri]));
    out.expect_code(3);
    // Nothing was changed by any of this.
    assert!(env.keys(&bucket).await.is_empty());
    assert_eq!(files(&dir), map(&[("a.txt", "a")]));
}

#[tokio::test]
async fn relative_folders_are_made_absolute() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    write_files(&work.path().join("photos"), &[("a.txt", "a")]);
    let out = env.run_in(
        work.path(),
        &["sync", "photos", &format!("s3://{bucket}/"), "--json"],
    );
    out.expect_code(0);
    let rows = row_records(&out);
    let local = rows[0]["local"].as_str().unwrap();
    assert!(Path::new(local).is_absolute(), "{local}");
    assert!(
        Path::new(local).ends_with(Path::new("photos").join("a.txt")),
        "{local}"
    );
    let summary = summary_record(&out);
    let root = summary["local"].as_str().unwrap();
    assert!(Path::new(root).is_absolute(), "{root}");
    assert!(Path::new(root).ends_with("photos"), "{root}");
    assert_eq!(env.keys(&bucket).await, ["a.txt"]);

    // `.` is the current folder.
    let out = env.run_in(
        &work.path().join("photos"),
        &["sync", ".", &format!("s3://{bucket}/again/")],
    );
    out.expect_code(0);
    assert!(env.s3.exists(&bucket, "again/a.txt").await);
}

/// A plan as the analysis makes it, stored in the test's plan store.
async fn stored_upload_plan(env: &Env, account_id: &str, dir: &Path, bucket: &str) -> String {
    let uri = S3Uri::parse(&format!("s3://{bucket}/p/")).unwrap();
    let listing = env.s3.ops().list_all(bucket, "p/").await.unwrap();
    let request = AnalyzeRequest::new(account_id, dir, &uri, SyncDirection::Upload);
    let plan = analyze(&request, listing).unwrap();
    let dirs = Dirs::under(env.config.path());
    SyncStore::open_in(&dirs).unwrap().insert(&plan).unwrap();
    plan.id
}

#[tokio::test]
async fn resume_runs_the_rows_of_a_stored_plan_that_are_not_done() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);
    let id = stored_upload_plan(&env, "environment", &dir, &bucket).await;
    // `a.txt` counts as done, so a resume does not upload it again.
    let dirs = Dirs::under(env.config.path());
    SyncStore::open_in(&dirs)
        .unwrap()
        .mark_done(&id, 0)
        .unwrap();
    // The folder changes after the analysis: a resume runs the plan as it was
    // made, it does not analyze again.
    write_files(&dir, &[("d.txt", "d")]);

    let out = env.run(&["sync", "--resume", &id, "--json"]);
    out.expect_code(0);
    let rows = row_records(&out);
    assert_eq!(describe(&rows), ["b.txt copy remote", "c.txt copy remote"]);
    assert!(rows.iter().all(|row| row["plan_id"] == id.as_str()));
    let summary = summary_record(&out);
    assert_eq!(counts(&summary), [0, 3, 0, 0, 0]);
    assert_eq!(summary["local"], dir.to_str().unwrap());
    assert_eq!(summary["remote"], format!("s3://{bucket}/p/"));
    assert!(summary["plan_id"].is_null());
    assert_eq!(env.keys(&bucket).await, ["p/b.txt", "p/c.txt"]);
    assert!(stored_plans(&env).is_empty());

    // The plan is gone now.
    let out = env.run(&["sync", "--resume", &id]);
    out.expect_code(2);
    assert!(out.stderr.contains("does not exist"), "{}", out.stderr);
}

#[tokio::test]
async fn a_plan_is_resumed_with_the_account_that_made_it() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a")]);
    let id = stored_upload_plan(&env, "work", &dir, &bucket).await;

    let out = env.run(&["sync", "--resume", &id]);
    out.expect_code(1);
    assert!(
        out.stderr.contains(&format!(
            "plan {id} was made with account 'work'; resume it with --account work"
        )),
        "{}",
        out.stderr
    );
    // Nothing ran, and the plan is still there.
    assert!(env.keys(&bucket).await.is_empty());
    assert!(is_stored(&env, &id));
}

#[tokio::test]
async fn resume_checks_the_account_and_the_bucket_before_running_any_row() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a"), ("b.txt", "b")]);
    let id = stored_upload_plan(&env, "environment", &dir, &bucket).await;

    // Changed credentials are one authentication error, not a failed row each.
    let out = run(env
        .bb()
        .env("BB_SECRET_ACCESS_KEY", "wrong")
        .args(["sync", "--resume", &id, "--json"]));
    out.expect_code(3);
    assert_eq!(out.error_record()["code"], "auth");
    assert!(out.stdout.is_empty(), "{}", out.stdout);

    // So is a bucket that is gone.
    env.s3
        .client()
        .delete_bucket()
        .bucket(&bucket)
        .send()
        .await
        .unwrap();
    let out = env.run(&["sync", "--resume", &id, "--json"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    assert!(out.stdout.is_empty(), "{}", out.stdout);
    // The plan is untouched, with every row still to do.
    assert!(is_stored(&env, &id));
    let store = SyncStore::open_in(&Dirs::under(env.config.path())).unwrap();
    assert_eq!(store.counts(&id).unwrap().pending(), 2);
}

/// Transfers slow enough to interrupt: 5 MiB parts at 256 KiB/s.
#[cfg(unix)]
fn slow_transfers(env: &Env) {
    fs::write(
        env.config.path().join("config.toml"),
        "[settings.transfer]\nmultipart_threshold_mib = 5\npart_size_mib = 5\nbandwidth_limit_kibps = 256\nretry_attempts = 0\n",
    )
    .unwrap();
}

/// How an interrupted `bb` ended.
#[cfg(unix)]
struct Interrupted {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    took: Duration,
}

/// Sends Ctrl-C (SIGINT) to `child` once `ready` holds and collects how it
/// ended. The child's standard output and error must be pipes.
#[cfg(unix)]
async fn interrupt_when(
    mut child: std::process::Child,
    mut ready: impl AsyncFnMut() -> bool,
) -> Interrupted {
    use std::io::Read;
    use std::time::Instant;

    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready().await {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("bb exited early: {status}");
        }
        assert!(Instant::now() < deadline, "the transfer never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let started = Instant::now();
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "bb did not stop after Ctrl-C"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    Interrupted {
        code: exit.code(),
        stdout,
        stderr,
        took: started.elapsed(),
    }
}

/// A folder with one big file (a multipart upload that can be interrupted)
/// and two small ones.
#[cfg(unix)]
fn big_folder(work: &Path) -> std::path::PathBuf {
    let dir = work.join("data");
    write_files(&dir, &[("small.txt", "small"), ("sub/tiny.txt", "tiny")]);
    fs::write(dir.join("big.bin"), vec![7u8; 12 * 1024 * 1024]).unwrap();
    dir
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_stops_the_sync_and_resume_finishes_it() {
    use std::process::Stdio;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    slow_transfers(&env);
    let work = tempfile::tempdir().unwrap();
    let dir = big_folder(work.path());
    let child = env
        .process()
        .args([
            "sync",
            dir.to_str().unwrap(),
            &format!("s3://{bucket}/backup"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ended = interrupt_when(child, async || {
        env.s3.pending_uploads(&bucket, "backup/big.bin").await > 0
    })
    .await;
    assert_eq!(ended.code, Some(130), "{}", ended.stderr);
    assert!(ended.took < Duration::from_secs(10), "{:?}", ended.took);

    // The plan ID is on stderr, with how to continue.
    let hint = ended
        .stderr
        .lines()
        .find(|line| line.contains("--resume"))
        .unwrap_or_else(|| panic!("no resume hint: {}", ended.stderr));
    assert!(hint.starts_with("bb: interrupted"), "{hint}");
    let plan_id = hint.rsplit(' ').next().unwrap().to_owned();
    assert!(
        ended
            .stderr
            .trim_end()
            .ends_with("bb: error: operation cancelled"),
        "{}",
        ended.stderr
    );
    assert!(
        ended.stdout.contains("not done"),
        "the summary counts what is left: {}",
        ended.stdout
    );
    // The multipart upload was aborted, and the big file is not in S3.
    assert_eq!(env.s3.pending_uploads(&bucket, "backup/big.bin").await, 0);
    assert!(!env.s3.exists(&bucket, "backup/big.bin").await);
    assert!(is_stored(&env, &plan_id));

    // Resume without the speed limit: the rest is uploaded and the plan goes.
    fs::write(env.config.path().join("config.toml"), "").unwrap();
    let out = env.run(&["sync", "--resume", &plan_id]);
    out.expect_code(0);
    assert!(
        out.stdout
            .trim_end()
            .ends_with("3 copied, 0 deleted, 0 already in sync"),
        "{}",
        out.stdout
    );
    assert_eq!(
        env.keys(&bucket).await,
        ["backup/big.bin", "backup/small.txt", "backup/sub/tiny.txt"]
    );
    let big = env.s3.get(&bucket, "backup/big.bin").await;
    assert!(big == vec![7u8; 12 * 1024 * 1024], "big.bin differs");
    assert!(stored_plans(&env).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_with_json_ends_with_a_cancelled_summary() {
    use std::process::Stdio;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    slow_transfers(&env);
    let work = tempfile::tempdir().unwrap();
    let dir = big_folder(work.path());
    let child = env
        .process()
        .args([
            "sync",
            "--json",
            dir.to_str().unwrap(),
            &format!("s3://{bucket}/backup"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ended = interrupt_when(child, async || {
        env.s3.pending_uploads(&bucket, "backup/big.bin").await > 0
    })
    .await;
    assert_eq!(ended.code, Some(130), "{}", ended.stderr);
    assert!(ended.took < Duration::from_secs(10), "{:?}", ended.took);

    // Standard error carries the one error record, as for every command.
    let lines: Vec<&str> = ended.stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{}", ended.stderr);
    let error: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(error["code"], "cancelled");
    // The summary tells what is left and names the plan to resume.
    let records: Vec<Value> = ended
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = records.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["cancelled"], true);
    assert!(counts(summary)[4] >= 1, "{summary}");
    let plan_id = summary["plan_id"].as_str().unwrap().to_owned();
    assert!(is_stored(&env, &plan_id));
    // The rows that finished before the interrupt carry the same plan ID.
    for row in records.iter().filter(|r| r["type"] == "row") {
        assert_eq!(row["plan_id"], plan_id.as_str());
        assert_eq!(row["done"], true);
    }
    assert_eq!(env.s3.pending_uploads(&bucket, "backup/big.bin").await, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_during_the_analysis_stops_at_once_and_stores_nothing() {
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use bucketbridge_tests::MockResponse;

    // The server holds the listing until the test releases it.
    let entered = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let seen = entered.clone();
    let mock = MockS3::start(move |_| {
        seen.store(true, Ordering::SeqCst);
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20));
        MockResponse::xml(xml::objects(&[]))
    });
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a")]);
    let mut cmd = bb_process(config.path());
    mock_env(&mut cmd, &mock);
    let child = cmd
        .args(["sync", dir.to_str().unwrap(), "s3://bucket/p"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ended = interrupt_when(child, async || entered.load(Ordering::SeqCst)).await;
    release_tx.send(()).unwrap();

    assert_eq!(ended.code, Some(130), "{}", ended.stderr);
    assert!(ended.took < Duration::from_secs(5), "{:?}", ended.took);
    assert!(ended.stderr.contains("cancelled"), "{}", ended.stderr);
    assert!(ended.stdout.is_empty(), "{}", ended.stdout);
    // Nothing was stored, and nothing was uploaded.
    assert!(!config.path().join("data").exists());
    assert!(
        mock.requests()
            .iter()
            .all(|request| request.method == "GET" && request.query_param("list-type").is_some())
    );
}

#[tokio::test]
async fn closed_stdout_ends_the_sync_quietly() {
    use std::process::Stdio;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write_files(&dir, &[("a.txt", "a")]);
    let mut child = env
        .process()
        .args([
            "sync",
            "--json",
            dir.to_str().unwrap(),
            &format!("s3://{bucket}/p"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the read end before bb writes anything.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    // A row finishes before it is reported.
    assert!(env.s3.exists(&bucket, "p/a.txt").await);
}
