//! `bb sync` with saved jobs (`--job`, `--save-job`, `bb sync jobs ...`), exclusion
//! rules (`--exclude`, `--include`, `--exclude-from`), S3 to S3 sync and the
//! `--summary` report.

use std::fs;
use std::path::Path;

use serde_json::Value;

use bucketbridge_cli::i18n::{tr, trf};
use bucketbridge_tests::TestS3;

use super::{InProcess, Output, Terminal};

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
}

/// A test server and an in-process `bb` that reaches it through the `BB_*` variables
/// (the account built from the environment).
struct Env {
    s3: TestS3,
    bb: InProcess,
}

impl Env {
    async fn start() -> Env {
        Env {
            s3: TestS3::start().await,
            bb: InProcess::default(),
        }
    }

    async fn run(&self, args: &[&str]) -> Output {
        let vars = [
            ("BB_ACCESS_KEY_ID", self.s3.access_key()),
            ("BB_SECRET_ACCESS_KEY", self.s3.secret_key()),
            ("BB_REGION", self.s3.region()),
            ("BB_ENDPOINT", self.s3.endpoint()),
            ("BB_PATH_STYLE", "true"),
        ];
        self.bb
            .run_with(args, &vars, b"", &Terminal::default())
            .await
    }

    async fn keys(&self, bucket: &str) -> Vec<String> {
        self.s3
            .ops()
            .list_all(bucket, "")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.key)
            .collect()
    }
}

fn paths(out: &Output) -> Vec<String> {
    let mut rows: Vec<String> = out
        .records()
        .into_iter()
        .filter(|record| record["type"] == "row")
        .map(|record| record["path"].as_str().unwrap().to_owned())
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn exclude_rules_and_files_limit_both_sides() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(
        &dir,
        &[
            ("keep.txt", "k"),
            ("scratch.tmp", "t"),
            ("keep.tmp", "kept by negation"),
            ("build/out.bin", "b"),
            ("docs/a.md", "a"),
        ],
    );
    let rules = work.path().join("rules.txt");
    fs::write(&rules, "# build output\nbuild/\n\n*.tmp\n!keep.tmp\n").unwrap();
    env.s3.put(&bucket, "p/old.tmp", "remote only").await;
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");

    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--exclude-from",
            rules.to_str().unwrap(),
            "--delete",
            "--dry-run",
            "--json",
        ])
        .await;
    out.expect_code(0);
    // Excluded files are out of scope on both sides: old.tmp is not deleted.
    assert_eq!(paths(&out), ["docs/a.md", "keep.tmp", "keep.txt"]);

    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--exclude-from",
            rules.to_str().unwrap(),
            "--exclude",
            "docs/",
            "--include",
            "**/*.txt",
            "--include",
            "*.tmp",
        ])
        .await;
    out.expect_code(0);
    let keys = env.keys(&bucket).await;
    assert!(keys.contains(&"p/keep.txt".to_owned()), "{keys:?}");
    assert!(keys.contains(&"p/keep.tmp".to_owned()), "{keys:?}");
    assert!(
        !keys
            .iter()
            .any(|k| k.contains("docs") || k.contains("build"))
    );
    assert!(!keys.contains(&"p/scratch.tmp".to_owned()));
    assert!(keys.contains(&"p/old.tmp".to_owned()));
}

#[tokio::test]
async fn bad_rules_and_missing_rule_files_are_errors() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(&dir, &[("a.txt", "a")]);
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");

    let out = env
        .run(&["sync", local, &uri, "--exclude", "[unclosed"])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("exclusion rule"), "{}", out.stderr);

    let missing = work.path().join("none.txt");
    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--exclude-from",
            missing.to_str().unwrap(),
        ])
        .await;
    out.expect_code(2);
    assert!(out.stderr.contains("exclusion file"), "{}", out.stderr);
    // Nothing was uploaded by the failed runs.
    assert!(env.keys(&bucket).await.is_empty());
}

#[tokio::test]
async fn summary_prints_a_report_and_json_ends_with_a_report_record() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(&dir, &[("a.txt", "12345"), ("b.txt", "123")]);
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");

    let out = env.run(&["sync", local, &uri, "--summary"]).await;
    out.expect_code(0);
    assert!(out.stdout.contains(&tr("summary:")), "{}", out.stdout);
    assert!(
        out.stdout.contains("uploaded: 2 files, 8 B"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("took "), "{}", out.stdout);
    // Without the flag there is no report.
    let again = env.run(&["sync", local, &uri]).await;
    assert!(!again.stdout.contains(&tr("summary:")), "{}", again.stdout);

    write(&dir, &[("c.txt", "1234567890")]);
    let out = env.run(&["sync", local, &uri, "--summary", "--json"]).await;
    out.expect_code(0);
    let records = out.records();
    let report = records.last().unwrap();
    assert_eq!(report["type"], "report");
    assert_eq!(report["upload"]["files"], 1);
    assert_eq!(report["upload"]["bytes"], 10);
    assert_eq!(report["equal"]["files"], 2);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["direction"], "upload");
    assert!(report["duration_ms"].is_u64());
    // The ordinary summary record comes just before it.
    assert_eq!(records[records.len() - 2]["type"], "summary");

    // A dry run reports what would be done.
    write(&dir, &[("d.txt", "dd")]);
    let out = env
        .run(&["sync", local, &uri, "--summary", "--json", "--dry-run"])
        .await;
    let records = out.records();
    let report = records.last().unwrap();
    assert_eq!(report["type"], "report");
    assert_eq!(report["upload"]["files"], 0);
    assert_eq!(report["upload"]["pending"], 1);
}

#[tokio::test]
async fn s3_to_s3_sync_copies_between_buckets() {
    let env = Env::start().await;
    let source = env.s3.new_bucket().await;
    let destination = env.s3.new_bucket().await;
    env.s3.put(&source, "in/a.txt", "alpha").await;
    env.s3.put(&source, "in/sub/b.txt", "bravo").await;
    env.s3.put(&destination, "out/extra.txt", "extra").await;
    let from = format!("s3://{source}/in");
    let to = format!("s3://{destination}/out");

    let out = env
        .run(&["sync", &from, &to, "--delete", "--dry-run", "--json"])
        .await;
    out.expect_code(0);
    let rows: Vec<Value> = out
        .records()
        .into_iter()
        .filter(|record| record["type"] == "row")
        .collect();
    assert_eq!(rows.len(), 3);
    let copy = rows.iter().find(|r| r["path"] == "a.txt").unwrap();
    assert_eq!(copy["source"], format!("s3://{source}/in/a.txt"));
    assert_eq!(copy["remote"], format!("s3://{destination}/out/a.txt"));
    assert_eq!(copy["local"], Value::Null);
    assert!(
        !env.keys(&destination)
            .await
            .contains(&"out/a.txt".to_owned())
    );

    let out = env.run(&["sync", &from, &to, "--delete"]).await;
    out.expect_code(0);
    assert!(out.stdout.contains(&tr("copied s3://")), "{}", out.stdout);
    let keys = env.keys(&destination).await;
    assert!(keys.contains(&"out/a.txt".to_owned()), "{keys:?}");
    assert!(keys.contains(&"out/sub/b.txt".to_owned()), "{keys:?}");
    assert!(!keys.contains(&"out/extra.txt".to_owned()), "{keys:?}");
    assert_eq!(env.s3.get(&destination, "out/sub/b.txt").await, b"bravo");

    // In sync now.
    let out = env.run(&["sync", &from, &to, "--json"]).await;
    let summary = out.records().pop().unwrap();
    assert_eq!(summary["copied"], 0);
    assert_eq!(summary["equal"], 2);
}

#[tokio::test]
async fn s3_to_s3_usage_errors() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let a = format!("s3://{bucket}/a");
    let nested = format!("s3://{bucket}/a/b");
    let out = env.run(&["sync", &a, &nested]).await;
    out.expect_code(1);
    assert!(
        out.stderr.contains("exclusion") || out.stderr.contains("contain each other"),
        "{}",
        out.stderr
    );
    let out = env
        .run(&[
            "sync",
            &a,
            &format!("s3://{bucket}/z"),
            "--direction",
            "download",
        ])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("--direction"), "{}", out.stderr);
}

/// Adds an account for the test server to an in-process `bb` and returns its label.
async fn add_account(bb: &InProcess, s3: &TestS3) -> &'static str {
    let mut args = vec![
        "account",
        "add",
        "--label",
        "main",
        "--access-key-id",
        s3.access_key(),
        "--region",
        s3.region(),
        "--endpoint",
        s3.endpoint(),
        "--path-style",
        "--secret-stdin",
    ];
    args.shrink_to_fit();
    let stdin = format!("{}\n", s3.secret_key());
    bb.run_with(&args, &[], stdin.as_bytes(), &Terminal::default())
        .await
        .expect_code(0);
    "main"
}

#[tokio::test]
async fn saved_jobs_are_created_listed_shown_run_and_deleted() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bb = InProcess::default();
    add_account(&bb, &s3).await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(&dir, &[("a.txt", "alpha"), ("skip.tmp", "t")]);
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/jobs");

    // No jobs yet.
    let out = bb.run(&["sync", "jobs", "list"]).await;
    out.expect_code(0);
    assert!(out.stdout.is_empty());

    // --save-job stores the setup with the account in use, then runs it.
    let out = bb
        .run(&[
            "--account",
            "main",
            "sync",
            local,
            &uri,
            "--exclude",
            "*.tmp",
            "--skip-duplicates",
            "--parallel",
            "2",
            "--save-job",
            "Nightly",
        ])
        .await;
    out.expect_code(0);
    assert!(
        out.stderr
            .contains(&trf("bb: saved sync job '{name}'", &[("name", "Nightly")])),
        "{}",
        out.stderr
    );
    assert_eq!(s3.get(&bucket, "jobs/a.txt").await, b"alpha");

    let out = bb.run(&["sync", "jobs", "list"]).await;
    out.expect_code(0);
    assert!(
        out.stdout.starts_with("Nightly\tupload\t"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains(&format!("{uri}/")), "{}", out.stdout);
    let out = bb.run(&["--json", "sync", "jobs", "list"]).await;
    let records = out.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "Nightly");
    assert_eq!(records[0]["options"]["exclude"][0], "*.tmp");
    assert_eq!(records[0]["options"]["skip_duplicate_content"], true);
    assert_eq!(records[0]["options"]["parallelism"], 2);

    let out = bb.run(&["sync", "jobs", "show", "nightly"]).await;
    out.expect_code(0);
    assert!(
        out.stdout
            .contains(&trf("name: {name}", &[("name", "Nightly")])),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains(&trf("exclude: {list}", &[("list", "*.tmp")])),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains(&trf("parallelism: {count}", &[("count", "2")])),
        "{}",
        out.stdout
    );

    // Run the job: a new file is picked up, the excluded one is not. The job needs
    // no --account.
    write(&dir, &[("b.txt", "bravo")]);
    let out = bb
        .run(&["sync", "--job", "NIGHTLY", "--dry-run", "--json"])
        .await;
    out.expect_code(0);
    assert_eq!(paths(&out), ["b.txt"]);
    assert!(!s3.exists(&bucket, "jobs/b.txt").await);
    let out = bb.run(&["sync", "--job", "Nightly", "--summary"]).await;
    out.expect_code(0);
    assert!(out.stdout.contains("uploaded: 1 file"), "{}", out.stdout);
    assert_eq!(s3.get(&bucket, "jobs/b.txt").await, b"bravo");
    assert!(!s3.exists(&bucket, "jobs/skip.tmp").await);

    // Saving under the same name replaces the job instead of adding another.
    bb.run(&[
        "--account",
        "main",
        "sync",
        local,
        &uri,
        "--delete",
        "--dry-run",
        "--save-job",
        "nightly",
    ])
    .await
    .expect_code(0);
    let out = bb.run(&["--json", "sync", "jobs", "list"]).await;
    let records = out.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["delete"], true);

    // Deleting needs --yes.
    let out = bb.run(&["sync", "jobs", "delete", "Nightly"]).await;
    out.expect_code(1);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert_eq!(bb.run(&["sync", "jobs", "list"]).await.lines().len(), 1);
    let out = bb
        .run(&["sync", "jobs", "delete", "Nightly", "--yes"])
        .await;
    out.expect_code(0);
    assert!(
        out.stdout.contains(&tr("deleted sync job")),
        "{}",
        out.stdout
    );
    assert!(bb.run(&["sync", "jobs", "list"]).await.stdout.is_empty());

    // A job that does not exist is an error, and the job commands take no account.
    let out = bb.run(&["sync", "--job", "gone"]).await;
    out.expect_code(2);
    assert!(out.stderr.contains("no sync job"), "{}", out.stderr); // core message
    let out = bb.run(&["sync", "jobs", "show", "gone"]).await;
    out.expect_code(2);
    let out = bb.run(&["--account", "main", "sync", "jobs", "list"]).await;
    out.expect_code(1);
}

#[tokio::test]
async fn a_job_cannot_be_combined_with_locations_and_runs_with_its_own_account() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bb = InProcess::default();
    add_account(&bb, &s3).await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(&dir, &[("a.txt", "a")]);
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");
    bb.run(&[
        "--account",
        "main",
        "sync",
        local,
        &uri,
        "--dry-run",
        "--save-job",
        "j",
    ])
    .await
    .expect_code(0);
    for extra in [
        vec!["sync", "--job", "j", local, &uri],
        vec!["sync", "--job", "j", "--delete"],
        vec!["sync", "--job", "j", "--direction", "download"],
        vec![
            "sync",
            "--job",
            "j",
            "--resume",
            "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35",
        ],
    ] {
        bb.run(&extra).await.expect_code(1);
    }
    // A different account than the job's is refused.
    let out = bb
        .run(&[
            "--account",
            "environment",
            "sync",
            "--job",
            "j",
            "--dry-run",
        ])
        .await;
    out.expect_code(1);
    assert!(
        out.stderr.contains(&tr("runs with the account")),
        "{}",
        out.stderr
    );
    // The job's own account works.
    bb.run(&["sync", "--job", "j", "--dry-run"])
        .await
        .expect_code(0);
}

#[tokio::test]
async fn sync_options_on_the_command_line() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path().join("folder");
    write(&dir, &[("same.txt", "identical"), ("other.txt", "AAAA")]);
    fs::create_dir_all(dir.join("empty")).unwrap();
    env.s3.put(&bucket, "p/same.txt", "identical").await;
    env.s3.put(&bucket, "p/other.txt", "BBBB").await;
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for name in ["same.txt", "other.txt"] {
        fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(later)
            .unwrap();
    }
    let local = dir.to_str().unwrap();
    let uri = format!("s3://{bucket}/p");

    // --no-cache leaves the cache alone (and --parallel is accepted).
    let cache = env.bb.config.path().join("data").join("sync-cache");
    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--dry-run",
            "--no-cache",
            "--parallel",
            "2",
        ])
        .await;
    out.expect_code(0);
    assert!(!cache.exists());

    // Empty folders are ignored unless asked for.
    let out = env.run(&["sync", local, &uri, "--dry-run", "--json"]).await;
    assert_eq!(paths(&out), ["other.txt", "same.txt"]);
    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--dry-run",
            "--json",
            "--keep-empty-folders",
        ])
        .await;
    assert_eq!(paths(&out), ["empty/", "other.txt", "same.txt"]);

    // Identical content is equal despite the newer time.
    let out = env
        .run(&[
            "sync",
            local,
            &uri,
            "--dry-run",
            "--json",
            "--skip-duplicates",
        ])
        .await;
    assert_eq!(paths(&out), ["other.txt"]);
    // A dry run writes nothing, not even the cache; a real run keeps it by default.
    env.run(&["sync", local, &uri, "--dry-run"])
        .await
        .expect_code(0);
    assert!(!cache.exists());
    env.run(&["sync", local, &uri, "--exclude", "*"])
        .await
        .expect_code(0);
    assert!(cache.is_dir());
    assert!(fs::read_dir(&cache).unwrap().count() >= 1);
}
