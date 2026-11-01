//! `bb scan`

use super::*;

#[tokio::test]
async fn scan_lists_findings_and_never_fails_for_a_skipped_check() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;

    let out = env.run(&["scan", "--bucket", &bucket, "--json"]);
    out.expect_code(0);
    let records = out.records();
    // s3s-fs implements none of the configuration APIs: each check is a
    // skipped record or a finding, never a failure of the command.
    assert!(!records.is_empty());
    for record in &records {
        assert_eq!(record["bucket"], bucket.as_str());
        assert!(matches!(
            record["kind"].as_str(),
            Some("finding" | "skipped" | "error")
        ));
    }

    let out = env.run(&["scan", "--bucket", &bucket]);
    out.expect_code(0);
    assert!(out.stderr.contains("1 buckets scanned"), "{}", out.stderr);

    // The whole account.
    let out = env.run(&["scan", "--json"]);
    out.expect_code(0);
    assert!(out.records().iter().any(|r| r["bucket"] == bucket.as_str()));
}

#[tokio::test]
async fn scan_rejects_a_bad_bucket_name() {
    let env = Env::start().await;
    env.run(&["scan", "--bucket", "bad name"]).expect_code(1);
}
