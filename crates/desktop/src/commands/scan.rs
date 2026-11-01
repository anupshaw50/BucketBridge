//! Security scan commands (docs/desktop-api.md, "Security scan (T13)").

use std::path::PathBuf;

use bucketbridge_core::s3uri::validate_bucket_name;
use bucketbridge_core::scan::{self, ReportFormat, ScanReport};
use tauri::State;

use super::{CommandResult, blocking, logged, require_absolute};
use crate::state::AppState;

/// Scans the named buckets of an account, or every bucket when `buckets` is
/// `null`. A bucket or check that fails is part of the report.
#[tauri::command(rename_all = "snake_case")]
pub async fn run_security_scan(
    state: State<'_, AppState>,
    account_id: String,
    buckets: Option<Vec<String>>,
) -> CommandResult<ScanReport> {
    let core = state.core()?;
    logged("running the security scan", async {
        if let Some(names) = &buckets {
            for name in names {
                validate_bucket_name(name)?;
            }
            if names.is_empty() {
                return Err(bucketbridge_core::Error::Usage(
                    "name at least one bucket".into(),
                ));
            }
        }
        let s3 = core.session.s3_for(&account_id).await?;
        let report = match buckets {
            Some(names) => scan::scan_buckets(&s3, &names).await,
            None => scan::scan_account(&s3).await?,
        };
        tracing::info!(
            target: "bucketbridge::desktop",
            "security scan: {} buckets, {} critical, {} high",
            report.buckets.len(),
            report.count(scan::Severity::Critical),
            report.count(scan::Severity::High)
        );
        Ok(report)
    })
    .await
}

/// Writes a report to `path` (absolute; replaced atomically).
#[tauri::command(rename_all = "snake_case")]
pub async fn export_scan_report(
    report: ScanReport,
    format: ReportFormat,
    path: String,
) -> CommandResult<()> {
    logged("exporting the scan report", async {
        require_absolute("the report path", &path)?;
        let path = PathBuf::from(path);
        blocking(move || scan::write_report(&report, format, &path)).await
    })
    .await
}
