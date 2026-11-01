//! `bb scan`: the security scan of T13.

use bucketbridge_core::scan::{self, Finding, FindingId};
use serde::Serialize;

use crate::cli::ScanArgs;
use crate::context::Context;
use crate::output::{Failure, Output};

/// One finding, one skipped check or one failed bucket.
#[derive(Debug, Serialize)]
struct ScanRecord {
    bucket: String,
    /// `finding`, `skipped` or `error`.
    kind: &'static str,
    severity: Option<&'static str>,
    id: Option<FindingId>,
    title: Option<String>,
    fix_tab: Option<&'static str>,
    explanation: String,
}

fn finding(bucket: &str, f: &Finding) -> ScanRecord {
    ScanRecord {
        bucket: bucket.to_owned(),
        kind: "finding",
        severity: Some(f.severity.as_str()),
        id: Some(f.id),
        title: Some(f.title.clone()),
        fix_tab: Some(f.fix_tab.as_str()),
        explanation: f.explanation.clone(),
    }
}

fn note(bucket: &str, kind: &'static str, message: &str) -> ScanRecord {
    ScanRecord {
        bucket: bucket.to_owned(),
        kind,
        severity: None,
        id: None,
        title: None,
        fix_tab: None,
        explanation: message.to_owned(),
    }
}

/// Scans one bucket or all of them and prints every finding, most severe
/// first. Findings are results, not failures: the exit code is 0 when the
/// scan ran.
pub async fn run(ctx: &Context, args: &ScanArgs, out: &mut Output) -> Result<(), Failure> {
    let report = match &args.bucket {
        Some(bucket) => scan::scan_buckets(&ctx.s3, std::slice::from_ref(bucket)).await,
        None => scan::scan_account(&ctx.s3).await?,
    };
    for (bucket, f) in report.rows() {
        let record = finding(bucket, f);
        out.row(&record, || {
            format!(
                "{:<8} {bucket}  {}  [fix: {} tab]\n         {}",
                f.severity.as_str(),
                f.title,
                f.fix_tab.as_str(),
                f.explanation
            )
        })?;
    }
    for scan in &report.buckets {
        for skip in &scan.skipped {
            let record = note(&scan.bucket, "skipped", &skip.message);
            out.row(&record, || {
                format!("skipped  {}  {}", scan.bucket, skip.message)
            })?;
        }
        if let Some(error) = &scan.error {
            let record = note(&scan.bucket, "error", &error.message);
            out.row(&record, || {
                format!("error    {}  {}", scan.bucket, error.message)
            })?;
        }
    }
    if !out.json() {
        out.notice(&format!(
            "{} buckets scanned: {} critical, {} high, {} medium, {} low",
            report.buckets.len(),
            report.count(scan::Severity::Critical),
            report.count(scan::Severity::High),
            report.count(scan::Severity::Medium),
            report.count(scan::Severity::Low),
        ));
    }
    Ok(())
}
