//! The asynchronous analysis used by the desktop host and the CLI (v0.2.0).
//!
//! [`analyze_async`] lists the remote side, scans the local folder and plans the rows,
//! with the [`SyncOptions`] of a job: filters and exclusion rules, and the analysis
//! options. The listing and the local scan run at the same time, and for an
//! `s3_to_s3` plan both prefixes are listed at the same time.
//!
//! [`analyze_s3`] is the pure half for `s3_to_s3`: it compares two listings. It reuses
//! the planner with the source listing standing in for the local folder, so the
//! rules are those of `upload` (`docs/sync.md`): copy what is missing or different
//! (different size, or a source modified after the destination), delete extras with
//! `delete`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;

use super::duplicate::{same_content_s3, skip_duplicates, skip_duplicates_s3};
use super::folders::{add_marker_copies, folder_markers};
use super::listing::{ListOptions, ListingCache, list_folder};
use super::plan::{DEFAULT_TOLERANCE, PlanOptions, plan_rows};
use super::scan::{LocalEntry, LocalTree, scan_remote};
use super::{AnalyzeRequest, analyze_scanned, as_folder, new_plan_id, prepare_local_root};
use super::{SyncOptions, scan_local_side};
use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::{FileMeta, ObjectEntry, SyncAction, SyncDirection, SyncPlan};
use crate::ops::S3;
use crate::s3uri::S3Uri;

/// What to analyse.
#[derive(Debug, Clone)]
pub struct AnalysisRequest {
    /// Account the plan will run with.
    pub account_id: String,
    /// Which way files are copied.
    pub direction: SyncDirection,
    /// Local folder (not used for `s3_to_s3`).
    pub local_root: PathBuf,
    /// The S3 prefix; the destination for `s3_to_s3`.
    pub s3_prefix: S3Uri,
    /// The source prefix of an `s3_to_s3` sync.
    pub source_prefix: Option<S3Uri>,
    /// Delete what exists only on the target side.
    pub delete: bool,
    /// Filters and analysis options.
    pub options: SyncOptions,
    /// The folder of the listing cache (see [`super::listing`]); `None` never uses it.
    /// The cache is also left alone when `delete` is set or `options.use_cache` is off.
    pub cache_dir: Option<PathBuf>,
}

fn join_error(err: tokio::task::JoinError) -> Error {
    Error::Internal(format!("an analysis thread failed: {err}"))
}

/// Analyses a sync and returns a new plan (not stored; use `SyncStore::insert`).
/// Writes nothing. Dropping the future stops the work, except for a local scan that
/// is already running on its thread, which finishes by itself.
pub async fn analyze_async(s3: &S3, request: &AnalysisRequest) -> Result<SyncPlan> {
    let filter = {
        let options = request.options.clone();
        tokio::task::spawn_blocking(move || options.compile_filter())
            .await
            .map_err(join_error)??
    };
    if request.direction == SyncDirection::S3ToS3 {
        let source = request
            .source_prefix
            .as_ref()
            .ok_or_else(|| Error::Usage("an S3 to S3 sync needs a source prefix".into()))?;
        let source = as_folder(source)?;
        let destination = as_folder(&request.s3_prefix)?;
        check_disjoint(&source, &destination)?;
        let listing = list_options(request);
        let (source_entries, destination_entries) = tokio::try_join!(
            list_remote(s3, &source, &listing),
            list_remote(s3, &destination, &listing)
        )?;
        let account_id = request.account_id.clone();
        let delete = request.delete;
        let skip_duplicates = request.options.skip_duplicate_content;
        let empty_folders = !request.options.skip_empty_folders;
        let mut plan = tokio::task::spawn_blocking(move || {
            analyze_s3(
                &account_id,
                &source,
                source_entries,
                &destination,
                destination_entries,
                delete,
                Some(&filter),
                skip_duplicates,
                empty_folders,
            )
        })
        .await
        .map_err(join_error)??;
        if skip_duplicates {
            let parallelism = usize::try_from(request.options.parallelism).unwrap_or(1);
            skip_duplicates_s3(s3, &mut plan, parallelism).await?;
        }
        return Ok(plan);
    }

    let remote = as_folder(&request.s3_prefix)?;
    let local_root = request.local_root.clone();
    let direction = request.direction;
    let delete = request.delete;
    let empty_folders = !request.options.skip_empty_folders;
    let scan_filter = filter.clone();
    let scan_remote_uri = remote.clone();
    let account_id = request.account_id.clone();
    // The local scan and the remote listing run at the same time.
    let scan = tokio::task::spawn_blocking(move || {
        let probe = AnalyzeRequest {
            delete,
            empty_folders,
            filter: Some(&scan_filter),
            ..AnalyzeRequest::new(&account_id, &local_root, &scan_remote_uri, direction)
        };
        let root = prepare_local_root(&probe)?;
        let tree = scan_local_side(&probe, &root)?;
        Ok::<_, Error>((root, tree))
    });
    let listing_options = list_options(request);
    let listing = list_remote(s3, &remote, &listing_options);
    let (scanned, entries) = tokio::join!(scan, listing);
    let (root, tree) = scanned.map_err(join_error)??;
    let entries = entries?;

    let account_id = request.account_id.clone();
    let mut plan = tokio::task::spawn_blocking(move || {
        let analysis = AnalyzeRequest {
            delete,
            empty_folders,
            filter: Some(&filter),
            ..AnalyzeRequest::new(&account_id, &root, &remote, direction)
        };
        analyze_scanned(&analysis, root.clone(), tree, entries)
    })
    .await
    .map_err(join_error)??;
    if request.options.skip_duplicate_content {
        let parallelism = usize::try_from(request.options.parallelism).unwrap_or(1);
        skip_duplicates(s3, &mut plan, parallelism).await?;
    }
    Ok(plan)
}

/// How the remote side is listed: `parallelism` sub-folders at a time, through the
/// cache only when it is on, a folder is known and nothing is deleted (a stale entry must
/// never decide a deletion).
fn list_options(request: &AnalysisRequest) -> ListOptions {
    let cache = match &request.cache_dir {
        Some(dir) if request.options.use_cache && !request.delete => {
            Some((ListingCache::at(dir.clone()), request.account_id.clone()))
        }
        _ => None,
    };
    ListOptions {
        parallelism: usize::try_from(request.options.parallelism).unwrap_or(1),
        cache,
    }
}

/// Lists a folder prefix flat.
async fn list_remote(s3: &S3, folder: &S3Uri, options: &ListOptions) -> Result<Vec<ObjectEntry>> {
    Ok(list_folder(s3, folder, options).await?.entries)
}

/// The two sides of an S3 to S3 sync must not contain each other, or the copy would
/// feed on its own output.
fn check_disjoint(source: &S3Uri, destination: &S3Uri) -> Result<()> {
    if source.bucket() == destination.bucket()
        && (source.key().starts_with(destination.key())
            || destination.key().starts_with(source.key()))
    {
        return Err(Error::Usage(
            "the source and the destination prefix must not be the same or contain each other"
                .into(),
        ));
    }
    Ok(())
}

/// Plans an S3 to S3 sync from the two flat listings (`source` and `destination` are
/// the folder prefixes the listings were made with).
///
/// The plan has direction `s3_to_s3`; its rows copy to or delete from the `remote`
/// side (the destination). `local_meta` of a row is the source object's size and
/// `Last-Modified`, `s3_meta` the destination object. With `skip_duplicates`, a copy row
/// whose two objects have the same content (see [`same_content_s3`]) becomes `equal`.
#[allow(clippy::too_many_arguments)]
pub fn analyze_s3(
    account_id: &str,
    source: &S3Uri,
    source_entries: Vec<ObjectEntry>,
    destination: &S3Uri,
    destination_entries: Vec<ObjectEntry>,
    delete: bool,
    filter: Option<&Filter>,
    skip_duplicates: bool,
    empty_folders: bool,
) -> Result<SyncPlan> {
    let source = as_folder(source)?;
    let destination = as_folder(destination)?;
    check_disjoint(&source, &destination)?;
    let mut tree = LocalTree::default();
    let source_markers = if empty_folders {
        folder_markers(&source_entries, source.key(), filter)
    } else {
        Vec::new()
    };
    let destination_markers = if empty_folders {
        folder_markers(&destination_entries, destination.key(), filter)
    } else {
        Vec::new()
    };
    let mut sources: HashMap<String, ObjectEntry> = HashMap::new();
    for entry in source_entries {
        if entry.is_prefix || entry.key.ends_with('/') {
            continue;
        }
        let Some(rel_path) = entry.key.strip_prefix(source.key()) else {
            continue;
        };
        if rel_path.is_empty() || filter.is_some_and(|filter| !filter.matches_path(rel_path)) {
            continue;
        }
        tree.entries.insert(
            rel_path.to_owned(),
            LocalEntry::File(FileMeta {
                size: entry.size,
                modified: entry.last_modified,
            }),
        );
        if skip_duplicates {
            sources.insert(rel_path.to_owned(), entry);
        }
    }
    let mut remote = scan_remote(
        destination_entries,
        destination.key(),
        Path::new(""),
        filter,
    )?;
    // Keys are never written to a local file here, so no key is a problem.
    for object in remote.objects.values_mut() {
        object.problem = None;
    }
    let options = PlanOptions {
        direction: SyncDirection::S3ToS3,
        delete,
        tolerance: DEFAULT_TOLERANCE,
        case_insensitive: false,
        key_prefix: destination.key(),
        filter,
    };
    let mut rows = plan_rows(tree, remote, &options)?;
    for row in &mut rows {
        let same = row.action == SyncAction::Copy
            && matches!(
                (sources.get(&row.rel_path), &row.s3_meta),
                (Some(source), Some(target)) if same_content_s3(source, target)
            );
        if same {
            row.action = SyncAction::Equal;
            row.target = None;
            row.done = true;
        }
    }
    if empty_folders {
        add_marker_copies(&mut rows, &source_markers, &destination_markers);
    }
    Ok(SyncPlan {
        id: new_plan_id(),
        account_id: account_id.to_owned(),
        local_root: PathBuf::new(),
        s3_prefix: destination,
        source_prefix: Some(source),
        direction: SyncDirection::S3ToS3,
        delete,
        created_at: Utc::now(),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone};

    use super::*;
    use crate::model::{Checksums, Side, SyncAction};

    fn at(second: u32) -> Option<DateTime<Utc>> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, second).single()
    }

    fn object(key: &str, size: u64, second: u32) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: None,
            last_modified: at(second),
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    fn uri(text: &str) -> S3Uri {
        S3Uri::parse(text).unwrap()
    }

    fn rows(plan: &SyncPlan) -> Vec<(&str, SyncAction, Option<Side>)> {
        plan.rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action, row.target))
            .collect()
    }

    #[test]
    fn s3_to_s3_follows_the_upload_rules() {
        let source = vec![
            object("a/new.txt", 1, 0),
            object("a/same.txt", 5, 0),
            object("a/bigger.txt", 9, 0),
            object("a/newer.txt", 4, 30),
            object("a/older.txt", 4, 0),
            object("a/", 0, 0),
            object("elsewhere.txt", 1, 0),
        ];
        let destination = vec![
            object("b/same.txt", 5, 0),
            object("b/bigger.txt", 3, 0),
            object("b/newer.txt", 4, 0),
            object("b/older.txt", 4, 30),
            object("b/extra.txt", 2, 0),
        ];
        let plan = analyze_s3(
            "acct",
            &uri("s3://src/a"),
            source.clone(),
            &uri("s3://dst/b"),
            destination.clone(),
            false,
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!(plan.direction, SyncDirection::S3ToS3);
        assert_eq!(plan.s3_prefix, uri("s3://dst/b/"));
        assert_eq!(plan.source_prefix, Some(uri("s3://src/a/")));
        assert!(plan.local_root.as_os_str().is_empty());
        assert_eq!(
            rows(&plan),
            vec![
                ("bigger.txt", SyncAction::Copy, Some(Side::Remote)),
                ("new.txt", SyncAction::Copy, Some(Side::Remote)),
                ("newer.txt", SyncAction::Copy, Some(Side::Remote)),
                ("older.txt", SyncAction::Equal, None),
                ("same.txt", SyncAction::Equal, None),
            ]
        );
        let copy = plan.rows.iter().find(|r| r.rel_path == "new.txt").unwrap();
        assert_eq!(copy.local_meta.as_ref().unwrap().size, 1);
        assert!(copy.s3_meta.is_none());

        let with_delete = analyze_s3(
            "acct",
            &uri("s3://src/a/"),
            source,
            &uri("s3://dst/b/"),
            destination,
            true,
            None,
            false,
            false,
        )
        .unwrap();
        assert!(rows(&with_delete).contains(&(
            "extra.txt",
            SyncAction::Delete,
            Some(Side::Remote)
        )));
    }

    #[test]
    fn s3_to_s3_applies_the_filters_to_both_sides() {
        let filter = Filter::default().with_path_rules(
            Vec::new(),
            crate::ignore::IgnoreRules::parse_text("*.tmp\n").unwrap(),
        );
        let plan = analyze_s3(
            "acct",
            &uri("s3://src/"),
            vec![object("x.txt", 1, 0), object("x.tmp", 1, 0)],
            &uri("s3://dst/"),
            vec![object("old.tmp", 1, 0)],
            true,
            Some(&filter),
            false,
            false,
        )
        .unwrap();
        // The excluded destination object is not deleted, the excluded source not copied.
        assert_eq!(
            rows(&plan),
            vec![("x.txt", SyncAction::Copy, Some(Side::Remote))]
        );
    }

    #[test]
    fn overlapping_prefixes_are_refused() {
        for (a, b) in [
            ("s3://x/a/", "s3://x/a/b/"),
            ("s3://x/a/b/", "s3://x/a/"),
            ("s3://x/a/", "s3://x/a/"),
            ("s3://x/", "s3://x/a/"),
        ] {
            let err = analyze_s3(
                "acct",
                &uri(a),
                vec![],
                &uri(b),
                vec![],
                false,
                None,
                false,
                false,
            )
            .unwrap_err();
            assert_eq!(err.kind(), crate::error::ErrorKind::Usage, "{a} {b}");
        }
        assert!(
            analyze_s3(
                "acct",
                &uri("s3://x/a/"),
                vec![],
                &uri("s3://y/a/"),
                vec![],
                false,
                None,
                false,
                false
            )
            .is_ok()
        );
    }
}
