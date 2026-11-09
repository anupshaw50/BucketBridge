//! `bb lock get|set` and `bb restore`

use bucketbridge_cli::i18n::{tr, trf};
use bucketbridge_tests::MockResponse;

use super::*;

const NS: &str = xml::NS;

const DENIED: &str = "Access Denied because object protected by object lock.";

fn lock_server() -> MockS3 {
    MockS3::start(|request| {
        let retention = request.query_param("retention").is_some();
        let hold = request.query_param("legal-hold").is_some();
        match (request.method.as_str(), retention, hold) {
            ("GET", true, _) => MockResponse::xml(format!(
                "<Retention {NS}><Mode>GOVERNANCE</Mode><RetainUntilDate>2099-01-02T03:04:05Z</RetainUntilDate></Retention>"
            )),
            ("GET", _, true) => {
                MockResponse::xml(format!("<LegalHold {NS}><Status>OFF</Status></LegalHold>"))
            }
            ("PUT", true, _) if request.path.ends_with("/locked") => {
                MockResponse::error(403, "AccessDenied", DENIED)
            }
            ("PUT", _, _) => MockResponse::empty(200),
            ("POST", _, _) if request.path.ends_with("/busy") => MockResponse::error(
                409,
                "RestoreAlreadyInProgress",
                "Object restore is already in progress",
            ),
            ("POST", _, _) => MockResponse::empty(202),
            _ => MockResponse::error(400, "Unexpected", "unexpected request"),
        }
    })
}

#[tokio::test]
async fn lock_get_prints_the_state() {
    let mock = lock_server();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["lock", "get", "s3://b/k"]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        "s3://b/k\tGOVERNANCE until 2099-01-02T03:04:05Z"
    );
    let out = run(bb_mock(&mock, config.path()).args(["--json", "lock", "get", "s3://b/k"]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b",
            "key": "k",
            "mode": "GOVERNANCE",
            "retain_until": "2099-01-02T03:04:05Z",
            "legal_hold": false,
            "error": null
        })]
    );
}

#[tokio::test]
async fn lock_set_sends_retention_hold_and_bypass() {
    let mock = lock_server();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "lock",
        "set",
        "s3://b/k",
        "--mode",
        "compliance",
        "--until",
        "2099-01-02",
        "--legal-hold",
        "on",
        "--bypass-governance",
    ]));
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["aspect"], "retention");
    assert_eq!(records[0]["mode"], "COMPLIANCE");
    assert_eq!(records[0]["retain_until"], "2099-01-02T00:00:00Z");
    assert_eq!(records[0]["changed"], true);
    assert_eq!(records[1]["aspect"], "legal_hold");
    assert_eq!(records[1]["legal_hold"], true);
    let puts: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(puts.len(), 2);
    assert_eq!(
        puts[0].header("x-amz-bypass-governance-retention"),
        Some("true")
    );
    assert!(puts[0].body_text().contains("<Mode>COMPLIANCE</Mode>"));
    assert!(puts[1].body_text().contains("<Status>ON</Status>"));
}

#[tokio::test]
async fn lock_set_prints_the_server_error_verbatim_and_fails() {
    let mock = lock_server();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "lock",
        "set",
        "s3://b/locked",
        "--clear-retention",
    ]));
    assert_ne!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains(DENIED), "{}", out.stderr);
}

#[tokio::test]
async fn lock_set_dry_run_sends_nothing_and_usage_errors_come_first() {
    let mock = lock_server();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "lock",
        "set",
        "s3://b/k",
        "--legal-hold",
        "on",
        "--dry-run",
    ]));
    out.expect_code(0);
    assert!(out.stdout.contains(&trf(
        "would turn the legal hold of {uri} {word}",
        &[("uri", "s3://b/k"), ("word", tr("on").as_str())]
    )));
    assert!(mock.requests().iter().all(|r| r.method == "GET"));

    let before = mock.requests().len();
    for args in [
        vec![
            "lock",
            "set",
            "s3://b/k",
            "--mode",
            "governance",
            "--until",
            "2001-01-01",
        ],
        vec![
            "lock",
            "set",
            "s3://b/k",
            "--legal-hold",
            "on",
            "--bypass-governance",
        ],
        vec!["lock", "set", "s3://b/k"],
    ] {
        let out = run(bb_mock(&mock, config.path()).args(args.clone()));
        assert_eq!(out.code, 1, "{args:?}: {}", out.stderr);
    }
    assert_eq!(mock.requests().len(), before);
}

#[tokio::test]
async fn restore_requests_days_and_tier_and_reports_each_object() {
    let mock = lock_server();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "restore",
        "s3://b/cold",
        "--days",
        "4",
        "--tier",
        "bulk",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "requested the restore of {uri} for {days} days ({tier})",
            &[("uri", "s3://b/cold"), ("days", "4"), ("tier", "Bulk")],
        )
    );
    let post = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert!(post.body_text().contains("<Days>4</Days>"));
    assert!(post.body_text().contains("<Tier>Bulk</Tier>"));

    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "restore",
        "s3://b/busy",
        "--days",
        "1",
    ]));
    assert_ne!(out.code, 0);
    let records = out.records();
    assert_eq!(records[0]["tier"], "Standard");
    assert_eq!(records[0]["requested"], false);
    assert_eq!(records[0]["error"]["code"], "conflict");
    assert!(
        records[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Object restore is already in progress")
    );
}
