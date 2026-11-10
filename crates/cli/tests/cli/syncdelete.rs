//! `bb sync --delete`: the question before deleting, `--yes`, the refusal
//! without a terminal, and the bytes and time of the summary.

use bucketbridge_cli::i18n::trf;

use super::*;

struct Setup {
    env: Env,
    bucket: String,
    work: tempfile::TempDir,
}

impl Setup {
    /// A folder with `a.txt` (5 bytes), and a prefix with the same file plus
    /// `stale1.txt` and `stale2.txt`, which only the prefix has.
    async fn new() -> Setup {
        let env = Env::start().await;
        let bucket = env.s3.new_bucket().await;
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir(work.path().join("src")).unwrap();
        std::fs::write(work.path().join("src/a.txt"), "alpha").unwrap();
        std::fs::write(work.path().join("src/new.txt"), "123").unwrap();
        for key in ["backup/a.txt", "backup/stale1.txt", "backup/stale2.txt"] {
            env.s3.put(&bucket, key, "alpha").await;
        }
        Setup { env, bucket, work }
    }

    fn local(&self) -> String {
        self.work.path().join("src").display().to_string()
    }

    fn prefix(&self) -> String {
        format!("s3://{}/backup/", self.bucket)
    }

    fn vars(&self) -> Vec<(&'static str, String)> {
        vec![
            ("BB_ACCESS_KEY_ID", self.env.s3.access_key().to_owned()),
            ("BB_SECRET_ACCESS_KEY", self.env.s3.secret_key().to_owned()),
            ("BB_REGION", self.env.s3.region().to_owned()),
            ("BB_ENDPOINT", self.env.s3.endpoint().to_string()),
            ("BB_PATH_STYLE", "true".to_owned()),
        ]
    }

    async fn sync(&self, process: &InProcess, extra: &[&str], terminal: &Terminal) -> Output {
        let local = self.local();
        let prefix = self.prefix();
        let mut args = vec!["sync", local.as_str(), prefix.as_str(), "--delete"];
        args.extend_from_slice(extra);
        let owned = self.vars();
        let vars: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        process.run_with(&args, &vars, b"", terminal).await
    }

    async fn keys(&self) -> Vec<String> {
        self.env.keys(&self.bucket).await
    }

    fn stored_plans(&self, process: &InProcess) -> usize {
        let dir = process.config.path().join("data").join("sync");
        std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0)
    }
}

const ALL: [&str; 3] = ["backup/a.txt", "backup/stale1.txt", "backup/stale2.txt"];

#[tokio::test]
async fn without_a_terminal_deleting_needs_yes() {
    let setup = Setup::new().await;
    let process = InProcess::default();
    let out = setup.sync(&process, &[], &Terminal::default()).await;
    out.expect_code(1);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    // The count and an example are in the message.
    assert!(out.stderr.contains('2'), "{}", out.stderr);
    assert!(out.stderr.contains("stale1.txt"), "{}", out.stderr);
    // Nothing was uploaded, deleted or stored.
    assert_eq!(setup.keys().await, ALL);
    assert_eq!(setup.stored_plans(&process), 0);

    // The same with --json: a usage error record.
    let out = setup
        .sync(&process, &["--json"], &Terminal::default())
        .await;
    out.expect_code(1);
    assert_eq!(out.error_record()["code"], "usage");
    assert_eq!(setup.keys().await, ALL);
}

#[tokio::test]
async fn yes_skips_the_question_and_the_summary_has_bytes_and_time() {
    let setup = Setup::new().await;
    let process = InProcess::default();
    let out = setup
        .sync(&process, &["--yes", "--json"], &Terminal::default())
        .await;
    out.expect_code(0);
    assert_eq!(setup.keys().await, ["backup/a.txt", "backup/new.txt"]);
    let records = out.records();
    let summary = records.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["deleted"], 2);
    assert_eq!(summary["copied"], 1);
    // Only new.txt (3 bytes) was uploaded.
    assert_eq!(summary["bytes"], 3);
    assert!(summary["elapsed_ms"].is_u64());

    // The human summary: the usual line, then the bytes and time on stderr.
    let setup = Setup::new().await;
    let out = setup.sync(&process, &["--yes"], &Terminal::default()).await;
    out.expect_code(0);
    let last = out.stdout.lines().last().unwrap().to_owned();
    assert_eq!(
        last,
        trf(
            "{copied} copied, {deleted} deleted, {equal} already in sync",
            &[("copied", "1"), ("deleted", "2"), ("equal", "1")],
        )
    );
    let suffix = trf(
        "{bytes} copied in {time}",
        &[("bytes", "3 B"), ("time", "")],
    );
    let prefix = suffix.trim_end().to_owned();
    assert!(out.stderr.contains(prefix.as_str()), "{}", out.stderr);

    // --quiet hides it.
    let setup = Setup::new().await;
    let out = setup
        .sync(&process, &["--yes", "-q"], &Terminal::default())
        .await;
    out.expect_code(0);
    assert!(out.stderr.is_empty(), "{}", out.stderr);
}

#[tokio::test]
async fn a_terminal_is_asked_with_the_count_and_examples() {
    let setup = Setup::new().await;
    let process = InProcess::default();

    // No: nothing changes, the exit code says cancelled.
    let terminal = Terminal::confirming(&[false]);
    let out = setup.sync(&process, &[], &terminal).await;
    out.expect_code(130);
    assert_eq!(setup.keys().await, ALL);
    assert_eq!(terminal.questions().len(), 1);
    assert!(
        terminal.questions()[0].contains('2'),
        "{:?}",
        terminal.questions()
    );
    // The examples are listed before the question.
    assert!(out.stderr.contains("stale1.txt"), "{}", out.stderr);
    assert!(out.stderr.contains("stale2.txt"), "{}", out.stderr);
    assert_eq!(setup.stored_plans(&process), 0);

    // Yes: the sync runs.
    let terminal = Terminal::confirming(&[true]);
    let out = setup.sync(&process, &[], &terminal).await;
    out.expect_code(0);
    assert_eq!(setup.keys().await, ["backup/a.txt", "backup/new.txt"]);
    assert_eq!(terminal.questions().len(), 1);
}

#[tokio::test]
async fn nothing_to_delete_means_no_question_and_dry_runs_never_ask() {
    let setup = Setup::new().await;
    let process = InProcess::default();
    // A dry run needs neither --yes nor a terminal.
    let out = setup
        .sync(&process, &["--dry-run", "--json"], &Terminal::default())
        .await;
    out.expect_code(0);
    assert_eq!(setup.keys().await, ALL);

    // Without --delete there is nothing to confirm (a plain upload).
    let local = setup.local();
    let prefix = setup.prefix();
    let owned = setup.vars();
    let vars: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let terminal = Terminal::confirming(&[]);
    let out = process
        .run_with(&["sync", &local, &prefix], &vars, b"", &terminal)
        .await;
    out.expect_code(0);
    assert!(terminal.questions().is_empty());

    // --delete with nothing extra on the target: no question, no refusal.
    std::fs::write(setup.work.path().join("src/stale1.txt"), "alpha").unwrap();
    std::fs::write(setup.work.path().join("src/stale2.txt"), "alpha").unwrap();
    let out = setup.sync(&process, &[], &Terminal::default()).await;
    out.expect_code(0);
}
