//! The summary report of a sync run (v0.2.0): counts and bytes per action, the
//! duration and the failures. Shown at the end of a run by the UI and printed by
//! `bb sync --summary`; `bb sync --json` ends with the same data as one record.
//!
//! The report describes the final state of the plan's rows (rows done by an earlier
//! run of the same plan count as done), plus how long this run took.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::{Side, SyncAction, SyncDirection, SyncRow};

/// Failures listed in a report; the count in [`SyncReport::failed`] is always complete.
pub const MAX_REPORTED_FAILURES: usize = 100;

/// Totals of one kind of action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KindTotals {
    /// Rows that were done.
    pub files: usize,
    /// Bytes of the files of the done rows.
    pub bytes: u64,
    /// Rows whose last attempt failed.
    pub failed: usize,
    /// Rows that were not attempted (a cancelled run) or are still waiting.
    pub pending: usize,
}

/// One failed row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFailure {
    /// Path relative to the synced folder.
    pub rel_path: String,
    /// Why it failed.
    pub error: String,
}

/// The report. Field names are the contract (`docs/desktop-api.md`, "Sync jobs and
/// sync options").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Plan the report is about.
    pub plan_id: String,
    /// Direction of the plan.
    pub direction: SyncDirection,
    /// How long the run took, in milliseconds.
    pub duration_ms: u64,
    /// True when the run was cancelled.
    pub cancelled: bool,
    /// Local files copied to S3.
    pub upload: KindTotals,
    /// S3 objects copied to the local folder.
    pub download: KindTotals,
    /// S3 objects copied to another prefix (`s3_to_s3`).
    pub copy: KindTotals,
    /// Local files deleted.
    pub delete_local: KindTotals,
    /// S3 objects deleted.
    pub delete_remote: KindTotals,
    /// Files that were already in sync (`files` and `bytes` only).
    pub equal: KindTotals,
    /// Rows the analysis could not plan (they never run).
    pub unsyncable: usize,
    /// All failures: `unsyncable` plus the rows whose attempt failed.
    pub failed: usize,
    /// The failures, at most [`MAX_REPORTED_FAILURES`].
    pub failures: Vec<ReportFailure>,
}

/// What kind of work a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Upload,
    Download,
    Copy,
    DeleteLocal,
    DeleteRemote,
    Equal,
    Unsyncable,
}

fn kind_of(row: &SyncRow, direction: SyncDirection) -> Kind {
    match (row.action, row.target) {
        (SyncAction::Equal, _) => Kind::Equal,
        (SyncAction::Copy, Some(Side::Remote)) if direction == SyncDirection::S3ToS3 => Kind::Copy,
        (SyncAction::Copy, Some(Side::Remote)) => Kind::Upload,
        (SyncAction::Copy, Some(Side::Local)) => Kind::Download,
        (SyncAction::Delete, Some(Side::Local)) => Kind::DeleteLocal,
        (SyncAction::Delete, Some(Side::Remote)) => Kind::DeleteRemote,
        _ => Kind::Unsyncable,
    }
}

/// The size of the file a row copies, deletes or found equal.
fn bytes_of(row: &SyncRow, kind: Kind) -> u64 {
    let local = row.local_meta.as_ref().map(|meta| meta.size);
    let remote = row.s3_meta.as_ref().map(|entry| entry.size);
    match kind {
        Kind::Upload | Kind::Copy | Kind::DeleteLocal => local.or(remote),
        Kind::Download | Kind::DeleteRemote => remote.or(local),
        Kind::Equal => local.or(remote),
        Kind::Unsyncable => None,
    }
    .unwrap_or(0)
}

impl SyncReport {
    /// Builds the report of a plan's rows after a run that took `duration`.
    pub fn from_rows(
        plan_id: &str,
        direction: SyncDirection,
        rows: &[SyncRow],
        duration: Duration,
        cancelled: bool,
    ) -> SyncReport {
        let mut report = SyncReport {
            plan_id: plan_id.to_owned(),
            direction,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            cancelled,
            upload: KindTotals::default(),
            download: KindTotals::default(),
            copy: KindTotals::default(),
            delete_local: KindTotals::default(),
            delete_remote: KindTotals::default(),
            equal: KindTotals::default(),
            unsyncable: 0,
            failed: 0,
            failures: Vec::new(),
        };
        for row in rows {
            let kind = kind_of(row, direction);
            let bytes = bytes_of(row, kind);
            let totals = match kind {
                Kind::Upload => &mut report.upload,
                Kind::Download => &mut report.download,
                Kind::Copy => &mut report.copy,
                Kind::DeleteLocal => &mut report.delete_local,
                Kind::DeleteRemote => &mut report.delete_remote,
                Kind::Equal => &mut report.equal,
                Kind::Unsyncable => {
                    report.unsyncable += 1;
                    report.failed += 1;
                    push_failure(&mut report.failures, row);
                    continue;
                }
            };
            if row.done {
                totals.files += 1;
                totals.bytes += bytes;
            } else if row.error.is_some() {
                totals.failed += 1;
                report.failed += 1;
                push_failure(&mut report.failures, row);
            } else {
                totals.pending += 1;
            }
        }
        report
    }

    /// Files copied or deleted by the plan (done rows, equal rows not counted).
    pub fn changed_files(&self) -> usize {
        self.upload.files
            + self.download.files
            + self.copy.files
            + self.delete_local.files
            + self.delete_remote.files
    }

    /// Bytes of the files copied (not deleted).
    pub fn copied_bytes(&self) -> u64 {
        self.upload.bytes + self.download.bytes + self.copy.bytes
    }

    /// Rows still waiting to run.
    pub fn pending(&self) -> usize {
        self.upload.pending
            + self.download.pending
            + self.copy.pending
            + self.delete_local.pending
            + self.delete_remote.pending
    }

    /// The report as lines of text, one per action that had rows.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = |label: &str, totals: &KindTotals, with_bytes: bool| {
            if totals.files + totals.failed + totals.pending == 0 {
                return;
            }
            let mut text = format!("{label}: {}", plural(totals.files));
            if with_bytes {
                text.push_str(&format!(", {}", format_bytes(totals.bytes)));
            }
            if totals.failed > 0 {
                text.push_str(&format!(", {} failed", totals.failed));
            }
            if totals.pending > 0 {
                text.push_str(&format!(", {} not done", totals.pending));
            }
            lines.push(text);
        };
        line("uploaded", &self.upload, true);
        line("downloaded", &self.download, true);
        line("copied", &self.copy, true);
        line("deleted locally", &self.delete_local, false);
        line("deleted in S3", &self.delete_remote, false);
        line("already in sync", &self.equal, false);
        if self.unsyncable > 0 {
            lines.push(format!("could not be planned: {}", plural(self.unsyncable)));
        }
        lines.push(format!(
            "took {}{}",
            format_duration(self.duration_ms),
            if self.cancelled { " (cancelled)" } else { "" }
        ));
        lines
    }
}

fn push_failure(failures: &mut Vec<ReportFailure>, row: &SyncRow) {
    if failures.len() < MAX_REPORTED_FAILURES {
        failures.push(ReportFailure {
            rel_path: row.rel_path.clone(),
            error: row
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_owned()),
        });
    }
}

fn plural(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// `1.5 MiB` style size, binary units.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `850 ms`, `12.3 s`, `4 min 5 s`, `2 h 3 min`.
pub fn format_duration(millis: u64) -> String {
    if millis < 1000 {
        return format!("{millis} ms");
    }
    let seconds = millis / 1000;
    if seconds < 60 {
        return format!("{:.1} s", millis as f64 / 1000.0);
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes} min {} s", seconds % 60);
    }
    format!("{} h {} min", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Checksums, FileMeta, ObjectEntry};

    fn row(rel: &str, action: SyncAction, target: Option<Side>, size: u64) -> SyncRow {
        SyncRow {
            rel_path: rel.to_owned(),
            action,
            target,
            local_meta: Some(FileMeta {
                size,
                modified: None,
            }),
            s3_meta: Some(ObjectEntry {
                key: rel.to_owned(),
                size: size + 1,
                etag: None,
                last_modified: None,
                storage_class: None,
                checksums: Checksums::default(),
                is_prefix: false,
            }),
            error: None,
            done: action == SyncAction::Equal,
        }
    }

    fn rows() -> Vec<SyncRow> {
        let mut done_up = row("a", SyncAction::Copy, Some(Side::Remote), 100);
        done_up.done = true;
        let mut failed_up = row("b", SyncAction::Copy, Some(Side::Remote), 50);
        failed_up.error = Some("access denied".into());
        let mut done_down = row("c", SyncAction::Copy, Some(Side::Local), 10);
        done_down.done = true;
        let pending_del = row("d", SyncAction::Delete, Some(Side::Remote), 7);
        let mut unsyncable = row("e", SyncAction::Failed, None, 1);
        unsyncable.error = Some("conflict".into());
        vec![
            done_up,
            failed_up,
            done_down,
            pending_del,
            unsyncable,
            row("f", SyncAction::Equal, None, 3),
        ]
    }

    #[test]
    fn counts_and_bytes_per_action() {
        let report = SyncReport::from_rows(
            "p",
            SyncDirection::Bidirectional,
            &rows(),
            Duration::from_millis(1500),
            false,
        );
        assert_eq!(
            report.upload,
            KindTotals {
                files: 1,
                bytes: 100,
                failed: 1,
                pending: 0
            }
        );
        // A download reads the object's size.
        assert_eq!(report.download.bytes, 11);
        assert_eq!(report.delete_remote.pending, 1);
        assert_eq!(report.equal.files, 1);
        assert_eq!(report.unsyncable, 1);
        assert_eq!(report.failed, 2);
        assert_eq!(report.duration_ms, 1500);
        assert_eq!(report.changed_files(), 2);
        assert_eq!(report.copied_bytes(), 111);
        assert_eq!(report.pending(), 1);
        let failures: Vec<_> = report
            .failures
            .iter()
            .map(|f| (f.rel_path.as_str(), f.error.as_str()))
            .collect();
        assert_eq!(failures, [("b", "access denied"), ("e", "conflict")]);
    }

    #[test]
    fn s3_copies_have_their_own_kind() {
        let mut copied = row("a", SyncAction::Copy, Some(Side::Remote), 8);
        copied.done = true;
        let report =
            SyncReport::from_rows("p", SyncDirection::S3ToS3, &[copied], Duration::ZERO, false);
        assert_eq!(report.copy.files, 1);
        assert_eq!(report.copy.bytes, 8);
        assert_eq!(report.upload, KindTotals::default());
    }

    #[test]
    fn failures_are_capped_but_counted() {
        let many: Vec<SyncRow> = (0..MAX_REPORTED_FAILURES + 5)
            .map(|n| {
                let mut r = row(&format!("f{n}"), SyncAction::Copy, Some(Side::Remote), 1);
                r.error = Some("boom".into());
                r
            })
            .collect();
        let report =
            SyncReport::from_rows("p", SyncDirection::Upload, &many, Duration::ZERO, false);
        assert_eq!(report.failed, MAX_REPORTED_FAILURES + 5);
        assert_eq!(report.failures.len(), MAX_REPORTED_FAILURES);
    }

    #[test]
    fn text_lines_and_formats() {
        let report = SyncReport::from_rows(
            "p",
            SyncDirection::Upload,
            &rows(),
            Duration::from_secs(125),
            true,
        );
        let lines = report.lines();
        assert_eq!(lines[0], "uploaded: 1 file, 100 B, 1 failed");
        assert!(lines.contains(&"deleted in S3: 0 files, 1 not done".to_owned()));
        assert!(lines.contains(&"could not be planned: 1 file".to_owned()));
        assert_eq!(lines.last().unwrap(), "took 2 min 5 s (cancelled)");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(format_duration(850), "850 ms");
        assert_eq!(format_duration(12_340), "12.3 s");
        assert_eq!(format_duration(3 * 3_600_000 + 120_000), "3 h 2 min");
    }

    #[test]
    fn serializes_with_the_contract_names() {
        let report = SyncReport::from_rows("p", SyncDirection::Upload, &[], Duration::ZERO, false);
        let value = serde_json::to_value(&report).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "cancelled",
                "copy",
                "delete_local",
                "delete_remote",
                "direction",
                "download",
                "duration_ms",
                "equal",
                "failed",
                "failures",
                "plan_id",
                "unsyncable",
                "upload"
            ]
        );
    }
}
