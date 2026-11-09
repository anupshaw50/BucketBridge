//! `bb scan`: the security scan of T13.

use bucketbridge_core::scan::{self, Finding, FindingId};
use serde::Serialize;

use crate::cli::ScanArgs;
use crate::context::Context;
use crate::i18n::trf;
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
            trf(
                "{severity} {bucket}  {title}  [fix: {tab} tab]\n         {explanation}",
                &[
                    ("severity", f.severity.as_str()),
                    ("bucket", bucket),
                    ("title", f.title.as_str()),
                    ("tab", f.fix_tab.as_str()),
                    ("explanation", f.explanation.as_str()),
                ],
            )
        })?;
    }
    for scan in &report.buckets {
        for skip in &scan.skipped {
            let record = note(&scan.bucket, "skipped", &skip.message);
            out.row(&record, || {
                trf(
                    "skipped  {bucket}  {message}",
                    &[("bucket", &scan.bucket), ("message", skip.message.as_str())],
                )
            })?;
        }
        if let Some(error) = &scan.error {
            let record = note(&scan.bucket, "error", &error.message);
            out.row(&record, || {
                trf(
                    "error    {bucket}  {message}",
                    &[
                        ("bucket", &scan.bucket),
                        ("message", error.message.as_str()),
                    ],
                )
            })?;
        }
    }
    if !out.json() {
        out.notice(&trf(
            "{scanned} buckets scanned: {critical} critical, {high} high, {medium} medium, {low} low",
            &[
                ("scanned", report.buckets.len().to_string().as_str()),
                ("critical", report.count(scan::Severity::Critical).to_string().as_str()),
                ("high", report.count(scan::Severity::High).to_string().as_str()),
                ("medium", report.count(scan::Severity::Medium).to_string().as_str()),
                ("low", report.count(scan::Severity::Low).to_string().as_str()),
            ],
        ));
    }
    Ok(())
}
