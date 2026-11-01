//! IPC tests of the security scan commands.

use std::fs;

use serde_json::{Value, json};

use crate::harness::{Harness, block_on, keys, start_s3};

#[test]
fn a_scan_reports_per_bucket_and_never_rejects_for_a_check() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());

    let report = h.ok(
        "run_security_scan",
        json!({ "account_id": id, "buckets": [bucket] }),
    );
    assert_eq!(keys(&report), ["buckets", "generated_at"]);
    let scan = &report["buckets"][0];
    assert_eq!(keys(scan), ["bucket", "error", "findings", "skipped"]);
    assert_eq!(scan["bucket"], bucket.as_str());
    assert!(scan["error"].is_null());
    // s3s-fs does not implement the configuration APIs: they are skipped
    // checks, and every finding has the contract's fields.
    assert!(scan["skipped"].is_array());
    for finding in scan["findings"].as_array().unwrap() {
        assert_eq!(
            keys(finding),
            ["explanation", "fix_tab", "id", "severity", "title"]
        );
    }

    // `null` scans the whole account.
    let all = h.ok(
        "run_security_scan",
        json!({ "account_id": id, "buckets": null }),
    );
    let names: Vec<&str> = all["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["bucket"].as_str().unwrap())
        .collect();
    assert!(names.contains(&bucket.as_str()));

    assert_eq!(
        h.code(
            "run_security_scan",
            json!({ "account_id": id, "buckets": [] })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "run_security_scan",
            json!({ "account_id": id, "buckets": ["bad name"] })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "run_security_scan",
            json!({ "account_id": "missing", "buckets": null })
        ),
        "not_found"
    );
}

#[test]
fn a_report_is_exported_as_csv_and_json() {
    let h = Harness::new();
    let report: Value = json!({
        "generated_at": "2026-01-01T00:00:00Z",
        "buckets": [{
            "bucket": "alpha",
            "findings": [{
                "id": "no_default_encryption", "severity": "medium",
                "title": "No default encryption", "explanation": "Turn it on.",
                "fix_tab": "encryption"
            }],
            "skipped": [],
            "error": null
        }]
    });
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("report.csv");
    h.ok(
        "export_scan_report",
        json!({ "report": report, "format": "csv", "path": csv.to_str().unwrap() }),
    );
    let text = fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("bucket,severity,finding,title,fix_tab,explanation"));
    assert!(text.contains("alpha,medium,no_default_encryption"));
    let json_path = dir.path().join("report.json");
    h.ok(
        "export_scan_report",
        json!({ "report": report, "format": "json", "path": json_path.to_str().unwrap() }),
    );
    let back: Value = serde_json::from_str(&fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(back, report);
    assert_eq!(
        h.code(
            "export_scan_report",
            json!({ "report": report, "format": "csv", "path": "relative.csv" })
        ),
        "usage"
    );
}
