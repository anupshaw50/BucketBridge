//! Folder synchronisation (task T10): dry-run analysis and the resumable plan store.
//!
//! The analysis is a pipeline of plain functions, so each step can be tested and reused:
//!
//! 1. [`scan_local`] walks the local folder (blocking file-system access),
//! 2. [`scan_remote`] keys a flat S3 listing of the prefix by relative path,
//! 3. [`plan_rows`] compares the two sides and produces sorted [`SyncRow`]s,
//! 4. [`analyze`] runs all three and wraps the rows in a new [`SyncPlan`],
//! 5. [`SyncStore`] keeps plans on disk and records per-row progress while a plan is
//!    executed, so execution can resume after a crash or a cancel,
//! 6. [`SyncExecutor`] executes a stored plan with the transfer engines (see
//!    [`execute`]).
//!
//! The rules (directions, tolerance, symbolic links, case collisions, filters) are
//! described in docs/sync.md and in the module docs of [`scan`] and [`plan`]. Only
//! [`execute`] writes to the local folder or to S3.
//!
//! Row life cycle: analysis yields `copy` / `delete` rows (`done: false`), `equal` rows
//! (`done: true`) and `failed` rows (reason in `error`, never executed). Execution runs
//! every row for which [`row_is_pending`] holds, never stops at the first failure, and
//! reports each result with [`SyncStore::mark_done`] (clears `error`),
//! [`SyncStore::record_error`] (sets `error`, keeps the action and `done: false`) or a
//! batch of both with [`SyncStore::update_rows`]. Running the plan again re-runs every
//! copy and delete row that is not done, including the ones that failed before.
//! [`RowCounts`] holds the progress numbers of `SyncProgress`, defined once for the
//! executor and the desktop host.

pub mod execute;
pub mod plan;
pub mod scan;
pub mod store;

use std::path::{Path, PathBuf};

use chrono::{TimeDelta, Utc};
use uuid::Uuid;

use crate::error::{Error, ErrorKind, Result};
use crate::filter::Filter;
use crate::model::{ObjectEntry, Side, SyncAction, SyncDirection, SyncPlan, SyncRow};
use crate::path::safe_local_path;
use crate::s3uri::S3Uri;

pub use execute::{
    ExecuteOptions, NoSyncEvents, RunSummary, SyncEvents, SyncExecutor, SyncProgress, SyncRun,
};
pub use plan::{DEFAULT_TOLERANCE, PlanOptions, plan_rows};
pub use scan::{
    LocalScanOptions, LocalTree, PART_SUFFIX, RemoteTree, is_case_insensitive, scan_local,
    scan_remote, validate_key_prefix,
};
pub use store::{PlanSummary, RowOutcome, RowUpdate, SYNC_DIR_NAME, SyncStore};

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

/// Input of [`analyze`].
#[derive(Debug, Clone, Copy)]
pub struct AnalyzeRequest<'a> {
    /// Account the plan will run with; stored in the plan.
    pub account_id: &'a str,
    /// Local folder; a relative path is made absolute against the current directory.
    pub local_root: &'a Path,
    /// Bucket and prefix of the remote side. The prefix names a folder, so
    /// `s3://bucket/photos` is used as `s3://bucket/photos/` (see [`as_folder`]).
    pub s3_prefix: &'a S3Uri,
    /// Which way the plan copies.
    pub direction: SyncDirection,
    /// Delete what exists only on the target side (a usage error for bidirectional).
    pub delete: bool,
    /// Optional filter; see docs/sync.md for how it applies to the two sides.
    pub filter: Option<&'a Filter>,
    /// Modification times closer than this count as equal.
    pub tolerance: TimeDelta,
}

impl<'a> AnalyzeRequest<'a> {
    /// A request without delete or filter and with [`DEFAULT_TOLERANCE`].
    pub fn new(
        account_id: &'a str,
        local_root: &'a Path,
        s3_prefix: &'a S3Uri,
        direction: SyncDirection,
    ) -> Self {
        Self {
            account_id,
            local_root,
            s3_prefix,
            direction,
            delete: false,
            filter: None,
            tolerance: DEFAULT_TOLERANCE,
        }
    }
}

/// Dry-run analysis: scans the local folder, takes the flat listing of the prefix
/// (`remote_entries`, for example `S3::list_all(bucket, key)` of [`as_folder`]'s result)
/// and returns a new plan with a fresh id. Writes nothing; store the plan with
/// [`SyncStore::insert`].
///
/// Blocking (walks the local folder): call it from a blocking thread.
///
/// A missing local folder is an error for `upload` (it would turn the plan into "delete
/// everything" with `delete`) and an empty folder for `download` and `bidirectional`.
pub fn analyze<I>(request: &AnalyzeRequest<'_>, remote_entries: I) -> Result<SyncPlan>
where
    I: IntoIterator<Item = ObjectEntry>,
{
    if request.delete && request.direction == SyncDirection::Bidirectional {
        return Err(usage(
            "delete cannot be combined with bidirectional sync: without a record of earlier runs a deletion on one side looks exactly like a new file on the other",
        ));
    }
    let s3_prefix = as_folder(request.s3_prefix)?;
    let key_prefix = s3_prefix.key();
    let local_root = std::path::absolute(request.local_root).map_err(|err| {
        usage(format!(
            "invalid local folder '{}': {err}",
            request.local_root.display()
        ))
    })?;
    // Plans are stored and sent to the UI as JSON, where paths are strings.
    if local_root.to_str().is_none() {
        return Err(usage(format!(
            "the local folder path '{}' is not valid UTF-8",
            local_root.display()
        )));
    }

    let scan_options = LocalScanOptions {
        filter: request.filter,
        check_readable: request.direction != SyncDirection::Download,
    };
    let local = match scan_local(&local_root, &scan_options) {
        Ok(tree) => tree,
        Err(err)
            if err.kind() == ErrorKind::NotFound && request.direction != SyncDirection::Upload =>
        {
            LocalTree::default()
        }
        Err(err) => return Err(err),
    };
    let remote = scan_remote(remote_entries, key_prefix, &local_root, request.filter)?;
    let case_insensitive =
        request.direction != SyncDirection::Upload && is_case_insensitive(&local_root, &local);
    let options = PlanOptions {
        direction: request.direction,
        delete: request.delete,
        tolerance: request.tolerance,
        case_insensitive,
        key_prefix,
        filter: request.filter,
    };
    let mut rows = plan_rows(local, remote, &options)?;
    if request.direction != SyncDirection::Upload {
        let hidden_names_possible = request.filter.is_some_and(Filter::has_path_criteria);
        fail_unseen_targets(&mut rows, &local_root, hidden_names_possible);
    }
    Ok(SyncPlan {
        id: new_plan_id(),
        account_id: request.account_id.to_owned(),
        local_root,
        s3_prefix,
        direction: request.direction,
        delete: request.delete,
        created_at: Utc::now(),
        rows,
    })
}

/// A copy to a local path where the analysis saw no file must find nothing there. The scan
/// cannot see names a path filter hides, and the planner only knows the spellings it
/// compares (letter case and Unicode normalization are approximations of what the file
/// system does); a name that exists anyway, such as `Docs/a.txt` under the filter prefix
/// `docs/` on a case-insensitive file system, would be overwritten. Such rows become
/// failed rows. Looks at the file system only when a path filter is set or the name is not
/// plain ASCII; execution checks again whatever the analysis let through.
fn fail_unseen_targets(rows: &mut [SyncRow], local_root: &Path, hidden_names_possible: bool) {
    for row in rows {
        let copies_to_new_local_path = row.action == SyncAction::Copy
            && row.target == Some(Side::Local)
            && row.local_meta.is_none();
        if !copies_to_new_local_path || !(hidden_names_possible || !row.rel_path.is_ascii()) {
            continue;
        }
        let Ok(path) = safe_local_path(local_root, &row.rel_path) else {
            continue;
        };
        let problem = match std::fs::symlink_metadata(&path) {
            Ok(entry) => Some(execute::unseen_entry_message(&entry, &row.rel_path)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => Some(format!("cannot create '{}': {err}", row.rel_path)),
        };
        if let Some(problem) = problem {
            row.action = SyncAction::Failed;
            row.target = None;
            row.error = Some(problem);
        }
    }
}

/// A new plan id (lowercase, hyphenated UUID v4).
pub fn new_plan_id() -> String {
    Uuid::new_v4().to_string()
}

/// Turns a user-supplied folder prefix into a key prefix: empty stays empty (the whole
/// bucket), anything else gets a trailing "/" if it lacks one.
pub fn normalize_key_prefix(prefix: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_owned()
    } else {
        format!("{prefix}/")
    }
}

/// The folder form of a sync prefix: the same URI when its key is empty or ends in
/// "/", otherwise the key with a "/" appended (`s3://bucket/photos` gives
/// `s3://bucket/photos/`). List the remote side with this URI's key; a row's key is
/// `path::join_key(plan.s3_prefix.key(), &row.rel_path)`.
pub fn as_folder(s3_prefix: &S3Uri) -> Result<S3Uri> {
    if s3_prefix.is_prefix() {
        Ok(s3_prefix.clone())
    } else {
        S3Uri::new(s3_prefix.bucket(), normalize_key_prefix(s3_prefix.key()))
    }
}

/// The local path a row reads, writes or deletes.
///
/// Downloads (copy to local) go through the path module's `safe_local_path`, because
/// the relative path comes from a key. Every other row's path was produced by the local
/// scan (real names joined with "/"); it is rebuilt segment by segment and refused if a
/// segment could leave `local_root` (which a damaged or edited plan file could contain).
pub fn row_local_path(local_root: &Path, row: &SyncRow) -> Result<PathBuf> {
    if row.action == SyncAction::Copy && row.target == Some(Side::Local) {
        return safe_local_path(local_root, &row.rel_path);
    }
    local_path_from_segments(local_root, &row.rel_path)
}

/// `local_root` joined with `rel_path`, rebuilt segment by segment; a segment that could
/// leave the root (empty, `.`, `..`, NUL, on Windows a separator or drive) is refused.
pub(crate) fn local_path_from_segments(local_root: &Path, rel_path: &str) -> Result<PathBuf> {
    let mut path = local_root.to_path_buf();
    for segment in rel_path.split('/') {
        let unsafe_segment = segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\0')
            || (cfg!(windows) && (segment.contains('\\') || segment.contains(':')));
        if unsafe_segment {
            return Err(usage(format!("'{rel_path}' is not a safe relative path")));
        }
        path.push(segment);
    }
    Ok(path)
}

/// Whether execution has to run this row: a copy or delete row that is not done,
/// including one whose last attempt failed.
pub fn row_is_pending(row: &SyncRow) -> bool {
    !row.done && is_executable(row.action)
}

/// Whether the row belongs in the "failed only" view and in [`RowCounts::failed`]: a
/// `failed` row from the analysis, or a row that is not done and carries an `error`
/// from its last execution attempt. (A row marked done never keeps an error.)
pub fn row_is_failed(row: &SyncRow) -> bool {
    is_failed(row.action, row.done, row.error.is_some())
}

fn is_executable(action: SyncAction) -> bool {
    matches!(action, SyncAction::Copy | SyncAction::Delete)
}

fn is_failed(action: SyncAction, done: bool, has_error: bool) -> bool {
    action == SyncAction::Failed || (!done && has_error)
}

/// The progress numbers of `SyncProgress` (docs/desktop-api.md), always over the whole
/// plan. Build them with [`row_counts`] or [`SyncStore::counts`]; `plan_id` and
/// `finished` are added by the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct RowCounts {
    /// `copy` and `delete` rows.
    pub total: usize,
    /// `copy` and `delete` rows with `done: true`.
    pub done: usize,
    /// `failed` rows plus rows that are not done and have an `error`.
    pub failed: usize,
}

impl RowCounts {
    /// Adds one row.
    pub fn add(&mut self, action: SyncAction, done: bool, has_error: bool) {
        if is_executable(action) {
            self.total += 1;
            self.done += usize::from(done);
        }
        self.failed += usize::from(is_failed(action, done, has_error));
    }

    /// Copy and delete rows that are not done: what the next run executes.
    pub fn pending(&self) -> usize {
        self.total.saturating_sub(self.done)
    }
}

/// Counts the rows of a plan (see [`RowCounts`]).
pub fn row_counts(rows: &[SyncRow]) -> RowCounts {
    let mut counts = RowCounts::default();
    for row in rows {
        counts.add(row.action, row.done, row.error.is_some());
    }
    counts
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;

    use super::*;
    use crate::model::{Checksums, FileMeta, FilterSpec};

    fn uri(text: &str) -> S3Uri {
        S3Uri::parse(text).unwrap()
    }

    fn object(key: &str, size: u64, modified: Option<DateTime<Utc>>) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: None,
            last_modified: modified,
            storage_class: Some("STANDARD".to_owned()),
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    fn file_modified(path: &Path) -> DateTime<Utc> {
        DateTime::<Utc>::from(fs::metadata(path).unwrap().modified().unwrap())
    }

    #[test]
    fn analyze_builds_a_plan_against_a_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("same.txt"), "12345").unwrap();
        fs::write(root.join("changed.txt"), "123").unwrap();
        fs::write(root.join("sub/new.txt"), "n").unwrap();
        fs::write(root.join("partial.bbpart"), "p").unwrap();
        let same_time = file_modified(&root.join("same.txt"));
        let listing = vec![
            object("backup/same.txt", 5, Some(same_time)),
            object("backup/changed.txt", 4, Some(same_time)),
            object("backup/gone.txt", 1, Some(same_time)),
            object("backup/", 0, None),
        ];

        let prefix = uri("s3://bucket/backup/");
        let mut request = AnalyzeRequest::new("acct", &root, &prefix, SyncDirection::Upload);
        request.delete = true;
        let plan = analyze(&request, listing.clone()).unwrap();
        assert_eq!(Uuid::try_parse(&plan.id).unwrap().get_version_num(), 4);
        assert_eq!(plan.s3_prefix, prefix);
        assert_eq!(plan.local_root, root);
        assert_eq!(plan.account_id, "acct");
        assert!(plan.delete);
        let summary: Vec<(&str, SyncAction, Option<Side>)> = plan
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action, row.target))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("changed.txt", SyncAction::Copy, Some(Side::Remote)),
                ("gone.txt", SyncAction::Delete, Some(Side::Remote)),
                ("same.txt", SyncAction::Equal, None),
                ("sub/new.txt", SyncAction::Copy, Some(Side::Remote)),
            ]
        );
        assert_eq!(
            row_counts(&plan.rows),
            RowCounts {
                total: 3,
                done: 0,
                failed: 0
            }
        );
        assert_eq!(row_counts(&plan.rows).pending(), 3);

        // The same folder, downloading.
        request.direction = SyncDirection::Download;
        let plan = analyze(&request, listing).unwrap();
        let actions: Vec<(&str, SyncAction)> = plan
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action))
            .collect();
        assert_eq!(
            actions,
            vec![
                ("changed.txt", SyncAction::Copy),
                ("gone.txt", SyncAction::Copy),
                ("same.txt", SyncAction::Equal),
                ("sub/new.txt", SyncAction::Delete),
            ]
        );
    }

    #[test]
    fn analyze_handles_missing_local_folders_per_direction() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let listing = || vec![object("a.txt", 1, None)];
        let whole_bucket = uri("s3://bucket");
        let upload = AnalyzeRequest::new("acct", &missing, &whole_bucket, SyncDirection::Upload);
        assert_eq!(
            analyze(&upload, listing()).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let download =
            AnalyzeRequest::new("acct", &missing, &whole_bucket, SyncDirection::Download);
        let plan = analyze(&download, listing()).unwrap();
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].action, SyncAction::Copy);
        assert_eq!(plan.s3_prefix.to_string(), "s3://bucket/");
    }

    #[test]
    fn analyze_validates_its_input() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = uri("s3://bucket/p/");
        let mut request =
            AnalyzeRequest::new("acct", dir.path(), &prefix, SyncDirection::Bidirectional);
        request.delete = true;
        assert_eq!(
            analyze(&request, Vec::new()).unwrap_err().kind(),
            ErrorKind::Usage
        );
        // A prefix without a trailing "/" names the folder.
        let not_a_folder = uri("s3://bucket/p");
        let mut request =
            AnalyzeRequest::new("acct", dir.path(), &not_a_folder, SyncDirection::Upload);
        request.delete = true;
        let plan = analyze(
            &request,
            vec![object("p/x", 1, None), object("px/y", 1, None)],
        )
        .unwrap();
        assert_eq!(plan.s3_prefix, prefix);
        let rows: Vec<(&str, SyncAction)> = plan
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action))
            .collect();
        assert_eq!(rows, vec![("x", SyncAction::Delete)]);
        let empty = Path::new("");
        let request = AnalyzeRequest::new("acct", empty, &prefix, SyncDirection::Upload);
        assert_eq!(
            analyze(&request, Vec::new()).unwrap_err().kind(),
            ErrorKind::Usage
        );
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        let whole_bucket = uri("s3://bucket/");
        let request = AnalyzeRequest::new("acct", &file, &whole_bucket, SyncDirection::Download);
        assert_eq!(
            analyze(&request, Vec::new()).unwrap_err().kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn analyze_applies_the_filter_to_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.jpg"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        let filter = Filter::compile(&FilterSpec {
            glob: Some("*.jpg".to_owned()),
            ..FilterSpec::default()
        })
        .unwrap();
        let whole_bucket = uri("s3://bucket/");
        let mut request =
            AnalyzeRequest::new("acct", dir.path(), &whole_bucket, SyncDirection::Upload);
        request.delete = true;
        request.filter = Some(&filter);
        let plan = analyze(
            &request,
            vec![object("c.txt", 1, None), object("d.jpg", 1, None)],
        )
        .unwrap();
        let paths: Vec<(&str, SyncAction)> = plan
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action))
            .collect();
        assert_eq!(
            paths,
            vec![("a.jpg", SyncAction::Copy), ("d.jpg", SyncAction::Delete)]
        );
    }

    #[test]
    fn analyzed_plans_round_trip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("x"), "x").unwrap();
        let whole_bucket = uri("s3://bucket/");
        let request = AnalyzeRequest::new("acct", &root, &whole_bucket, SyncDirection::Upload);
        let plan = analyze(&request, Vec::new()).unwrap();
        let store = SyncStore::open(dir.path().join("sync")).unwrap();
        store.insert(&plan).unwrap();
        assert_eq!(store.get(&plan.id).unwrap(), plan);
    }

    #[test]
    fn prefix_helpers() {
        assert_eq!(normalize_key_prefix(""), "");
        assert_eq!(normalize_key_prefix("a"), "a/");
        assert_eq!(normalize_key_prefix("a/b/"), "a/b/");
        assert_eq!(
            as_folder(&uri("s3://bucket/a/b/")).unwrap(),
            uri("s3://bucket/a/b/")
        );
        assert_eq!(
            as_folder(&uri("s3://bucket/a")).unwrap(),
            uri("s3://bucket/a/")
        );
        assert_eq!(as_folder(&uri("s3://bucket")).unwrap(), uri("s3://bucket/"));
        // Appending "/" to a key of the maximum length makes it too long.
        let longest = format!("s3://bucket/{}", "k".repeat(crate::s3uri::MAX_KEY_BYTES));
        assert_eq!(
            as_folder(&uri(&longest)).unwrap_err().kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn row_local_paths_refuse_escapes() {
        let root = Path::new("/sync/root");
        let make = |rel_path: &str, action: SyncAction, target: Option<Side>| SyncRow {
            rel_path: rel_path.to_owned(),
            action,
            target,
            local_meta: Some(FileMeta {
                size: 1,
                modified: None,
            }),
            s3_meta: None,
            error: None,
            done: false,
        };
        let upload = make("a/b.txt", SyncAction::Copy, Some(Side::Remote));
        assert_eq!(
            row_local_path(root, &upload).unwrap(),
            root.join("a").join("b.txt")
        );
        let download = make("a/b.txt", SyncAction::Copy, Some(Side::Local));
        assert_eq!(
            row_local_path(root, &download).unwrap(),
            root.join("a").join("b.txt")
        );
        for bad in ["../x", "a/../../x", "a//b", "./a", "", "a/\0"] {
            for (action, target) in [
                (SyncAction::Copy, Some(Side::Remote)),
                (SyncAction::Copy, Some(Side::Local)),
                (SyncAction::Delete, Some(Side::Local)),
            ] {
                let err = row_local_path(root, &make(bad, action, target)).unwrap_err();
                assert_eq!(err.kind(), ErrorKind::Usage, "{bad:?}");
            }
        }
    }

    #[test]
    fn pending_and_failed_rows() {
        let base = SyncRow {
            rel_path: "x".to_owned(),
            action: SyncAction::Copy,
            target: Some(Side::Remote),
            local_meta: None,
            s3_meta: None,
            error: None,
            done: false,
        };
        assert!(row_is_pending(&base));
        let retry = SyncRow {
            error: Some("timeout".to_owned()),
            ..base.clone()
        };
        assert!(row_is_pending(&retry) && row_is_failed(&retry));
        let done = SyncRow {
            done: true,
            ..base.clone()
        };
        assert!(!row_is_pending(&done));
        let analysis_failure = SyncRow {
            action: SyncAction::Failed,
            target: None,
            error: Some("unreadable".to_owned()),
            ..base
        };
        assert!(!row_is_pending(&analysis_failure) && row_is_failed(&analysis_failure));
    }

    #[test]
    fn progress_counts_follow_the_contract() {
        let make = |action: SyncAction, done: bool, error: Option<&str>| SyncRow {
            rel_path: "x".to_owned(),
            action,
            target: None,
            local_meta: None,
            s3_meta: None,
            error: error.map(str::to_owned),
            done,
        };
        let rows = vec![
            make(SyncAction::Copy, false, None),
            make(SyncAction::Copy, true, None),
            make(SyncAction::Copy, false, Some("timeout")),
            make(SyncAction::Delete, true, None),
            make(SyncAction::Delete, false, Some("denied")),
            make(SyncAction::Equal, true, None),
            make(SyncAction::Equal, true, None),
            make(SyncAction::Failed, false, Some("unreadable")),
        ];
        let counts = row_counts(&rows);
        // total: copy and delete rows; done: those that are done; failed: failed rows
        // plus not-done rows with an error.
        assert_eq!(
            counts,
            RowCounts {
                total: 5,
                done: 2,
                failed: 3
            }
        );
        assert_eq!(counts.pending(), 3);
        assert_eq!(rows.iter().filter(|row| row_is_failed(row)).count(), 3);
        assert_eq!(rows.iter().filter(|row| row_is_pending(row)).count(), 3);
        assert_eq!(row_counts(&[]), RowCounts::default());
    }
}
