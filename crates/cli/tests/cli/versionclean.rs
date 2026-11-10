//! `bb ls --versions` and `bb rm --versions` with filters, the confirmation
//! and the summary. s3s-fs has no versioning, so a scripted server lists the
//! versions.

use bucketbridge_cli::i18n::trf;
use bucketbridge_tests::MockResponse;

use super::*;

const OLD: &str = "2020-01-01T00:00:00.000Z";
const NEW: &str = "2099-01-01T00:00:00.000Z";

/// `logs/`: two old versions of `a.gz` (the newer one current), an old
/// `b.txt` version, a new `c.gz` delete marker that is current.
fn logs() -> MockS3 {
    MockS3::start(|request| match request.method.as_str() {
        "GET" if request.query_param("versions").is_some() => {
            MockResponse::xml(xml::dated_versions(&[
                ("logs/a.gz", "v1", true, false, OLD),
                ("logs/a.gz", "v0", false, false, OLD),
                ("logs/b.txt", "w1", false, false, OLD),
                ("logs/c.gz", "dm", true, true, NEW),
            ]))
        }
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    })
}

fn listed(mock: &MockS3, extra: &[&str]) -> Vec<String> {
    let config = tempfile::tempdir().unwrap();
    let mut args = vec!["--json", "ls", "--versions", "-r", "s3://bucket/logs/"];
    args.extend_from_slice(extra);
    let out = run(bb_mock(mock, config.path()).args(args));
    out.expect_code(0);
    out.records()
        .iter()
        .map(|r| r["version_id"].as_str().unwrap().to_owned())
        .collect()
}

fn deleted(mock: &MockS3) -> Vec<String> {
    let mut ids: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.query_param("versionId").unwrap_or_default())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn ls_versions_filters() {
    let mock = logs();
    assert_eq!(listed(&mock, &[]), ["v1", "v0", "w1", "dm"]);
    assert_eq!(listed(&mock, &["--noncurrent-only"]), ["v0", "w1"]);
    assert_eq!(listed(&mock, &["--delete-markers-only"]), ["dm"]);
    assert_eq!(listed(&mock, &["--key-glob", "*.gz"]), ["v1", "v0", "dm"]);
    assert_eq!(listed(&mock, &["--older-than", "30d"]), ["v1", "v0", "w1"]);
    assert_eq!(
        listed(&mock, &["--older-than", "2021-01-01"]),
        ["v1", "v0", "w1"]
    );
    assert_eq!(
        listed(
            &mock,
            &[
                "--older-than",
                "30d",
                "--noncurrent-only",
                "--key-glob",
                "*.gz"
            ]
        ),
        ["v0"]
    );
    assert!(listed(&mock, &["--older-than", "2019-01-01"]).is_empty());
}

#[tokio::test]
async fn filters_need_versions_and_a_valid_value() {
    let mock = logs();
    let config = tempfile::tempdir().unwrap();
    for args in [
        vec!["ls", "--noncurrent-only", "s3://bucket/"],
        vec!["rm", "--older-than", "30d", "s3://bucket/doc"],
        vec!["ls", "--versions", "--older-than", "soon", "s3://bucket/"],
        vec!["ls", "--versions", "--key-glob", "[", "s3://bucket/"],
    ] {
        let out = run(bb_mock(&mock, config.path()).args(args.clone()));
        out.expect_code(1);
        assert!(mock.requests().is_empty(), "{args:?} reached the server");
    }
}

#[tokio::test]
async fn folders_are_not_listed_with_filters() {
    let mock = MockS3::start(|_| {
        MockResponse::xml(xml::versions(
            &[("logs/a.gz", "v1", true, false)],
            &["logs/sub/"],
            None,
        ))
    });
    let config = tempfile::tempdir().unwrap();
    let plain = run(bb_mock(&mock, config.path()).args([
        "--json",
        "ls",
        "--versions",
        "s3://bucket/logs/",
    ]));
    assert_eq!(plain.records().len(), 2);
    let filtered = run(bb_mock(&mock, config.path()).args([
        "--json",
        "ls",
        "--versions",
        "--key-glob",
        "*.gz",
        "s3://bucket/logs/",
    ]));
    assert_eq!(filtered.records().len(), 1);
}

#[tokio::test]
async fn rm_versions_deletes_only_what_the_filters_keep() {
    let mock = logs();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "-r",
        "--versions",
        "--noncurrent-only",
        "--older-than",
        "30d",
        "--yes",
        "--json",
        "s3://bucket/logs/",
    ]));
    out.expect_code(0);
    assert_eq!(deleted(&mock), ["v0", "w1"]);
    assert_eq!(out.records().len(), 2);
    // The summary goes to standard error, not into the records.
    assert!(out.stderr.is_empty(), "{}", out.stderr);
}

#[tokio::test]
async fn rm_versions_prints_a_summary() {
    let mock = logs();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "-r",
        "--versions",
        "--delete-markers-only",
        "--yes",
        "s3://bucket/logs/",
    ]));
    out.expect_code(0);
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    let summary = trf(
        "deleted {what} under {uri}",
        &[
            (
                "what",
                trf(
                    "{versions} versions ({markers} delete markers, {current} current), {bytes} bytes",
                    &[
                        ("versions", "1"),
                        ("markers", "1"),
                        ("current", "1"),
                        ("bytes", "0"),
                    ],
                )
                .as_str(),
            ),
            ("uri", "s3://bucket/logs/"),
        ],
    );
    assert_eq!(out.stderr.trim(), summary);
}

#[tokio::test]
async fn rm_versions_dry_run_needs_no_confirmation() {
    let mock = logs();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "-r",
        "--versions",
        "--older-than",
        "30d",
        "--dry-run",
        "s3://bucket/logs/",
    ]));
    out.expect_code(0);
    assert_eq!(out.stdout.lines().count(), 3);
    assert!(deleted(&mock).is_empty());
    assert!(out.stderr.contains("s3://bucket/logs/"), "{}", out.stderr);
}

#[tokio::test]
async fn rm_versions_refuses_without_yes_or_a_terminal() {
    let mock = logs();
    let config = tempfile::tempdir().unwrap();
    let out =
        run(bb_mock(&mock, config.path()).args(["rm", "-r", "--versions", "s3://bucket/logs/"]));
    out.expect_code(1);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert!(deleted(&mock).is_empty());
    assert!(
        mock.requests().is_empty(),
        "the refusal comes before any request"
    );
}

fn terminal_env(mock: &MockS3) -> Vec<(&'static str, String)> {
    vec![
        ("BB_ACCESS_KEY_ID", "AKIDMOCK".to_owned()),
        ("BB_SECRET_ACCESS_KEY", "mock-secret".to_owned()),
        ("BB_ENDPOINT", mock.endpoint().to_string()),
        ("BB_PATH_STYLE", "true".to_owned()),
    ]
}

async fn rm_on_terminal(mock: &MockS3, terminal: &Terminal) -> Output {
    let owned = terminal_env(mock);
    let vars: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    InProcess::default()
        .run_with(
            &[
                "rm",
                "-r",
                "--versions",
                "--noncurrent-only",
                "s3://bucket/logs/",
            ],
            &vars,
            b"",
            terminal,
        )
        .await
}

#[tokio::test]
async fn rm_versions_asks_on_a_terminal() {
    // Yes: the versions are deleted, after a question with the count.
    let mock = logs();
    let terminal = Terminal::confirming(&[true]);
    let out = rm_on_terminal(&mock, &terminal).await;
    out.expect_code(0);
    assert_eq!(deleted(&mock), ["v0", "w1"]);
    let questions = terminal.questions();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].contains('2'), "{questions:?}");
    // The examples are on standard error before the question.
    assert!(out.stderr.contains("logs/a.gz (v0)"), "{}", out.stderr);

    // No: nothing is deleted and the command is cancelled.
    let mock = logs();
    let terminal = Terminal::confirming(&[false]);
    let out = rm_on_terminal(&mock, &terminal).await;
    out.expect_code(130);
    assert!(deleted(&mock).is_empty());
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn rm_versions_with_nothing_to_delete_does_not_ask() {
    let mock = logs();
    let terminal = Terminal::confirming(&[]);
    let owned = terminal_env(&mock);
    let vars: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let out = InProcess::default()
        .run_with(
            &[
                "rm",
                "-r",
                "--versions",
                "--older-than",
                "2019-01-01",
                "s3://bucket/logs/",
            ],
            &vars,
            b"",
            &terminal,
        )
        .await;
    out.expect_code(0);
    assert!(terminal.questions().is_empty());
    assert!(deleted(&mock).is_empty());
    assert!(out.stderr.contains("s3://bucket/logs/"), "{}", out.stderr);
}
