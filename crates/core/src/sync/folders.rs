//! Empty folders (v0.2.0, `SyncOptions::skip_empty_folders` turned off).
//!
//! By default sync only handles files: empty local folders are ignored and so are S3
//! folder markers (keys that end in `/`). With the option off, the plan also carries
//! *folder rows*, whose `rel_path` ends in `/`:
//!
//! - upload and `s3_to_s3`: a copy row to the remote side for an empty local folder (or a
//!   marker of the source) that has no marker in S3,
//! - download: a copy row to the local side for a marker in S3 whose folder does not exist
//!   locally; the executor creates the empty folder,
//! - bidirectional: both.
//!
//! Markers are never deleted by sync, and a folder that holds files needs no row (the
//! files create it). The rows are executed like file rows: they are recorded in the plan
//! store, retried by `--resume`, and shown in the results like any other row.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::filter::Filter;
use crate::model::{FileMeta, ObjectEntry, Side, SyncAction, SyncDirection, SyncRow};
use crate::path::safe_local_path;

/// A folder marker of an S3 listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMarker {
    /// Path relative to the synced prefix, with the trailing `/`.
    pub rel_path: String,
    /// The marker object.
    pub entry: ObjectEntry,
}

/// The folder markers below `key_prefix` in a flat listing: empty objects whose key ends
/// in `/`. The prefix's own marker and markers the filter excludes are left out.
pub fn folder_markers(
    entries: &[ObjectEntry],
    key_prefix: &str,
    filter: Option<&Filter>,
) -> Vec<FolderMarker> {
    let mut markers: Vec<FolderMarker> = entries
        .iter()
        .filter(|entry| !entry.is_prefix && entry.key.ends_with('/') && entry.size == 0)
        .filter_map(|entry| {
            let rel_path = entry.key.strip_prefix(key_prefix)?;
            let folder = rel_path.trim_end_matches('/');
            if folder.is_empty() || folder.starts_with('/') || folder.contains("//") {
                return None;
            }
            if filter.is_some_and(|filter| !filter.may_match_below(folder)) {
                return None;
            }
            Some(FolderMarker {
                rel_path: format!("{folder}/"),
                entry: entry.clone(),
            })
        })
        .collect();
    markers.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    markers.dedup_by(|a, b| a.rel_path == b.rel_path);
    markers
}

fn folder_row(rel_path: String, target: Side) -> SyncRow {
    SyncRow {
        rel_path,
        action: SyncAction::Copy,
        target: Some(target),
        local_meta: None,
        s3_meta: None,
        error: None,
        done: false,
    }
}

/// Adds the folder rows of a local plan (see the [module docs](self)) and keeps the rows
/// sorted by `rel_path`. `empty_dirs` are the empty local folders (without trailing `/`).
pub fn add_folder_rows(
    rows: &mut Vec<SyncRow>,
    direction: SyncDirection,
    local_root: &Path,
    empty_dirs: &[String],
    remote_markers: &[FolderMarker],
) {
    let known: HashSet<&str> = remote_markers
        .iter()
        .map(|marker| marker.rel_path.as_str())
        .collect();
    let uploads = matches!(
        direction,
        SyncDirection::Upload | SyncDirection::Bidirectional
    );
    let downloads = matches!(
        direction,
        SyncDirection::Download | SyncDirection::Bidirectional
    );
    if uploads {
        for folder in empty_dirs {
            let rel_path = format!("{folder}/");
            if known.contains(rel_path.as_str()) {
                continue;
            }
            let mut row = folder_row(rel_path, Side::Remote);
            row.local_meta = Some(FileMeta {
                size: 0,
                modified: None,
            });
            rows.push(row);
        }
    }
    if downloads {
        for marker in remote_markers {
            let folder = marker.rel_path.trim_end_matches('/');
            let Ok(path) = safe_local_path(local_root, folder) else {
                continue;
            };
            // Whatever is there (a folder, a file, a link) is left alone.
            if fs::symlink_metadata(&path).is_ok() {
                continue;
            }
            let mut row = folder_row(marker.rel_path.clone(), Side::Local);
            row.s3_meta = Some(marker.entry.clone());
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
}

/// Adds the folder rows of an `s3_to_s3` plan: every marker of the source that the
/// destination lacks.
pub fn add_marker_copies(
    rows: &mut Vec<SyncRow>,
    source_markers: &[FolderMarker],
    destination_markers: &[FolderMarker],
) {
    let known: HashSet<&str> = destination_markers
        .iter()
        .map(|marker| marker.rel_path.as_str())
        .collect();
    for marker in source_markers {
        if known.contains(marker.rel_path.as_str()) {
            continue;
        }
        let mut row = folder_row(marker.rel_path.clone(), Side::Remote);
        row.local_meta = Some(FileMeta {
            size: 0,
            modified: marker.entry.last_modified,
        });
        rows.push(row);
    }
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
}

/// Whether a row is a folder row.
pub fn is_folder_row(row: &SyncRow) -> bool {
    row.rel_path.ends_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Checksums;

    fn object(key: &str, size: u64) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: None,
            last_modified: None,
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    #[test]
    fn markers_are_empty_keys_ending_in_a_slash_below_the_prefix() {
        let entries = [
            object("p/", 0),
            object("p/a/", 0),
            object("p/a/b/", 0),
            object("p/data/", 5),
            object("p/file.txt", 0),
            object("other/c/", 0),
            object("p//x/", 0),
        ];
        let markers = folder_markers(&entries, "p/", None);
        let rels: Vec<&str> = markers.iter().map(|m| m.rel_path.as_str()).collect();
        assert_eq!(rels, ["a/", "a/b/"]);
        let filter = Filter::default().with_path_rules(
            Vec::new(),
            crate::ignore::IgnoreRules::parse_text("a/b/\n").unwrap(),
        );
        let filtered = folder_markers(&entries, "p/", Some(&filter));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].rel_path, "a/");
    }

    #[test]
    fn upload_rows_for_empty_local_folders_without_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let markers = folder_markers(&[object("p/known/", 0)], "p/", None);
        let mut rows = vec![SyncRow {
            rel_path: "a.txt".to_owned(),
            action: SyncAction::Equal,
            target: None,
            local_meta: None,
            s3_meta: None,
            error: None,
            done: true,
        }];
        add_folder_rows(
            &mut rows,
            SyncDirection::Upload,
            dir.path(),
            &["empty".to_owned(), "known".to_owned(), "z/inner".to_owned()],
            &markers,
        );
        let summary: Vec<(&str, SyncAction, Option<Side>)> = rows
            .iter()
            .map(|row| (row.rel_path.as_str(), row.action, row.target))
            .collect();
        assert_eq!(
            summary,
            [
                ("a.txt", SyncAction::Equal, None),
                ("empty/", SyncAction::Copy, Some(Side::Remote)),
                ("z/inner/", SyncAction::Copy, Some(Side::Remote)),
            ]
        );
        assert!(rows.iter().filter(|r| is_folder_row(r)).all(|r| !r.done));
    }

    #[test]
    fn download_rows_for_markers_whose_folder_is_missing_locally() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("present")).unwrap();
        fs::write(dir.path().join("file"), "x").unwrap();
        let entries = [
            object("p/missing/", 0),
            object("p/present/", 0),
            object("p/file/", 0),
            object("p/../evil/", 0),
        ];
        let markers = folder_markers(&entries, "p/", None);
        let mut rows = Vec::new();
        add_folder_rows(
            &mut rows,
            SyncDirection::Download,
            dir.path(),
            &[],
            &markers,
        );
        let rels: Vec<&str> = rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rels, ["missing/"]);
        assert_eq!(rows[0].target, Some(Side::Local));
        assert!(rows[0].s3_meta.is_some());
        // Local empty folders are not uploaded by a download.
        let mut none = Vec::new();
        add_folder_rows(
            &mut none,
            SyncDirection::Download,
            dir.path(),
            &["local-only".to_owned()],
            &[],
        );
        assert!(none.is_empty());
    }

    #[test]
    fn s3_to_s3_copies_the_markers_the_destination_lacks() {
        let source = folder_markers(&[object("a/x/", 0), object("a/y/", 0)], "a/", None);
        let destination = folder_markers(&[object("b/y/", 0)], "b/", None);
        let mut rows = Vec::new();
        add_marker_copies(&mut rows, &source, &destination);
        let rels: Vec<&str> = rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rels, ["x/"]);
        assert_eq!(rows[0].target, Some(Side::Remote));
        assert_eq!(rows[0].local_meta.as_ref().map(|m| m.size), Some(0));
    }
}
