//! The sync planner: a pure function from the two scanned sides to the rows of a plan.
//!
//! Rules (see docs/sync.md for examples):
//!
//! - `upload`: local only => copy to remote; on both sides and different => copy to
//!   remote; equal => equal (done); remote only => delete from remote with `delete`,
//!   otherwise no row. Different means the sizes differ or the local modification time
//!   is newer than the object's `last_modified` by more than the tolerance.
//! - `download`: the mirror image; different means the sizes differ or the object is
//!   newer than the local file by more than the tolerance.
//! - `bidirectional`: one side only => copy to the other side; on both sides the side
//!   that is newer by more than the tolerance wins; same size with times within the
//!   tolerance => equal; different sizes with times within the tolerance (or a missing
//!   time) => failed, because the newer copy cannot be determined. `delete` is a usage
//!   error: without a record of earlier runs a deletion on one side cannot be told
//!   apart from a creation on the other.
//! - A missing modification time never counts as newer.
//! - Local entries that cannot be synced (see [`super::scan`]) become failed rows
//!   whenever the direction would act on them: always for upload and bidirectional, and
//!   for download when S3 has the same path or `delete` is on. A download without
//!   `delete` leaves local-only entries alone, so like readable local-only files they
//!   produce no row. Keys that cannot be stored locally fail when the plan would write
//!   them locally, and a remote key below a local folder that could not be scanned is
//!   failed instead of being copied or deleted.
//! - With a [`Filter`], the size, date and storage-class criteria are judged on the
//!   entry an action reads (the source of a copy) or removes (the victim of a delete);
//!   equal and ambiguous rows are kept when the side the direction treats as the source
//!   matches (either side for bidirectional). An entry that fails the criteria is never
//!   treated as absent, so a filter can never cause a deletion.
//! - When the plan writes locally (download, bidirectional), paths whose spelling differs
//!   only in Unicode normalization (a composed and a decomposed accent, which macOS file
//!   systems treat as one name) or, on a file system that ignores letter case, only in
//!   case, compared segment by segment, cannot coexist; every row that would create,
//!   overwrite or delete such a local path is failed. A local copy is also failed when
//!   one of its parent segments is a file that stays, or when its own name is needed as
//!   a folder by another path.
//! - Copies to the remote side whose key would exceed [`MAX_KEY_BYTES`] are failed.
//! - Rows are sorted by `rel_path` (byte order); equal rows start with `done: true`,
//!   failed rows carry the reason in `error` and no `target`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;

use chrono::{DateTime, TimeDelta, Utc};
use unicode_normalization::UnicodeNormalization;

use super::scan::{LocalEntry, LocalFailure, LocalTree, RemoteObject, RemoteProblem, RemoteTree};
use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::{FileMeta, ObjectEntry, Side, SyncAction, SyncDirection, SyncRow};
use crate::s3uri::MAX_KEY_BYTES;

/// Default tolerance for comparing modification times (file systems and S3 store times
/// with different precision).
pub const DEFAULT_TOLERANCE: TimeDelta = TimeDelta::seconds(2);

const RESERVED_MESSAGE: &str =
    "names ending in '.bbpart' are reserved for temporary download files and are never synced";

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

/// Options of [`plan_rows`].
#[derive(Debug, Clone, Copy)]
pub struct PlanOptions<'a> {
    /// Which way the plan copies.
    pub direction: SyncDirection,
    /// Delete what only exists on the target side (not allowed for bidirectional).
    pub delete: bool,
    /// Modification times closer than this are considered equal; must not be negative.
    pub tolerance: TimeDelta,
    /// Whether the local file system ignores letter case (see
    /// [`super::scan::is_case_insensitive`]). Only used when the plan writes locally.
    pub case_insensitive: bool,
    /// Key prefix of the remote side, for the key length check.
    pub key_prefix: &'a str,
    /// Size, date and storage-class criteria (the path criteria were applied by the scans).
    pub filter: Option<&'a Filter>,
}

impl<'a> PlanOptions<'a> {
    /// Options with `delete` off, the default tolerance and the case-insensitive
    /// (safe) assumption.
    pub fn new(direction: SyncDirection, key_prefix: &'a str) -> Self {
        Self {
            direction,
            delete: false,
            tolerance: DEFAULT_TOLERANCE,
            case_insensitive: true,
            key_prefix,
            filter: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// No row.
    Skip,
    Equal,
    Copy(Side),
    Delete(Side),
    Failed(String),
}

impl Outcome {
    /// Whether the outcome creates, overwrites or removes a local file.
    fn touches_local(&self) -> bool {
        matches!(
            self,
            Outcome::Copy(Side::Local) | Outcome::Delete(Side::Local)
        )
    }
}

struct Draft {
    rel_path: String,
    local: Option<LocalEntry>,
    remote: Option<RemoteObject>,
    outcome: Outcome,
}

impl Draft {
    fn is_local_folder(&self) -> bool {
        matches!(
            self.local,
            Some(LocalEntry::Failed(LocalFailure { folder: true, .. }))
        )
    }

    fn into_row(self) -> Option<SyncRow> {
        let (action, target, error, done) = match self.outcome {
            Outcome::Skip => return None,
            Outcome::Equal => (SyncAction::Equal, None, None, true),
            Outcome::Copy(side) => (SyncAction::Copy, Some(side), None, false),
            Outcome::Delete(side) => (SyncAction::Delete, Some(side), None, false),
            Outcome::Failed(error) => (SyncAction::Failed, None, Some(error), false),
        };
        let local_meta = match self.local {
            Some(LocalEntry::File(meta)) => Some(meta),
            Some(LocalEntry::Failed(failure)) => failure.meta,
            None => None,
        };
        Some(SyncRow {
            rel_path: self.rel_path,
            action,
            target,
            local_meta,
            s3_meta: self.remote.map(|remote| remote.entry),
            error,
            done,
        })
    }
}

/// Plans the rows for `local` against `remote`. Deterministic: the same inputs always
/// give the same rows, sorted by `rel_path`.
///
/// Errors (`usage`): `delete` with [`SyncDirection::Bidirectional`], negative tolerance.
pub fn plan_rows(
    local: LocalTree,
    remote: RemoteTree,
    options: &PlanOptions<'_>,
) -> Result<Vec<SyncRow>> {
    if options.delete && options.direction == SyncDirection::Bidirectional {
        return Err(usage(
            "delete cannot be combined with bidirectional sync: without a record of earlier runs a deletion on one side looks exactly like a new file on the other",
        ));
    }
    if options.tolerance < TimeDelta::zero() {
        return Err(usage("the time tolerance must not be negative"));
    }
    let LocalTree {
        entries, unmapped, ..
    } = local;
    let blocked: HashMap<String, String> = entries
        .iter()
        .filter_map(|(rel_path, entry)| match entry {
            LocalEntry::Failed(failure) if failure.folder => {
                Some((rel_path.clone(), failure.error.clone()))
            }
            _ => None,
        })
        .collect();

    let mut drafts = Vec::with_capacity(entries.len().max(remote.objects.len()));
    let mut locals = entries.into_iter().peekable();
    let mut remotes = remote.objects.into_iter().peekable();
    loop {
        let order = match (locals.peek(), remotes.peek()) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some((local_path, _)), Some((remote_path, _))) => local_path.cmp(remote_path),
        };
        let (rel_path, local, remote) = match order {
            Ordering::Less => match locals.next() {
                Some((rel_path, local)) => (rel_path, Some(local), None),
                None => break,
            },
            Ordering::Greater => match remotes.next() {
                Some((rel_path, remote)) => (rel_path, None, Some(remote)),
                None => break,
            },
            Ordering::Equal => match (locals.next(), remotes.next()) {
                (Some((rel_path, local)), Some((_, remote))) => {
                    (rel_path, Some(local), Some(remote))
                }
                _ => break,
            },
        };
        let outcome = decide(
            &rel_path,
            local.as_ref(),
            remote.as_ref(),
            &blocked,
            options,
        );
        drafts.push(Draft {
            rel_path,
            local,
            remote,
            outcome,
        });
    }

    if options.direction != SyncDirection::Upload {
        veto_local_conflicts(&mut drafts, options.case_insensitive);
    }

    let mut rows: Vec<SyncRow> = drafts.into_iter().filter_map(Draft::into_row).collect();
    // Names that are not UTF-8 never match a key, so they are local-only entries.
    let unmapped = if leaves_local_only_alone(options) {
        Vec::new()
    } else {
        unmapped
    };
    rows.extend(unmapped.into_iter().map(|entry| SyncRow {
        rel_path: entry.display,
        action: SyncAction::Failed,
        target: None,
        local_meta: None,
        s3_meta: None,
        error: Some(entry.error),
        done: false,
    }));
    // Stable: on equal paths a mapped entry stays ahead of an escaped unmapped name.
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(rows)
}

fn decide(
    rel_path: &str,
    local: Option<&LocalEntry>,
    remote: Option<&RemoteObject>,
    blocked: &HashMap<String, String>,
    options: &PlanOptions<'_>,
) -> Outcome {
    let local_meta = match local {
        Some(LocalEntry::Failed(_)) if remote.is_none() && leaves_local_only_alone(options) => {
            return Outcome::Skip;
        }
        // The direction would act on this entry, but it cannot be synced.
        Some(LocalEntry::Failed(failure)) => return Outcome::Failed(failure.error.clone()),
        Some(LocalEntry::File(meta)) => Some(meta),
        None => None,
    };
    let entry = remote.map(|remote| &remote.entry);
    let outcome = match (local_meta, entry) {
        (Some(local_meta), Some(entry)) => compare(local_meta, entry, options),
        (Some(_), None) => match options.direction {
            SyncDirection::Upload | SyncDirection::S3ToS3 | SyncDirection::Bidirectional => {
                Outcome::Copy(Side::Remote)
            }
            SyncDirection::Download if options.delete => Outcome::Delete(Side::Local),
            SyncDirection::Download => Outcome::Skip,
        },
        (None, Some(_)) => match options.direction {
            SyncDirection::Download | SyncDirection::Bidirectional => Outcome::Copy(Side::Local),
            SyncDirection::Upload | SyncDirection::S3ToS3 if options.delete => {
                Outcome::Delete(Side::Remote)
            }
            SyncDirection::Upload | SyncDirection::S3ToS3 => Outcome::Skip,
        },
        (None, None) => Outcome::Skip,
    };
    if outcome == Outcome::Skip {
        return outcome;
    }
    if let Some(filter) = options.filter
        && filter.has_metadata_criteria()
        && !source_matches(filter, &outcome, local_meta, entry, options.direction)
    {
        return Outcome::Skip;
    }
    if local.is_none()
        && let Some((folder, reason)) = blocked_ancestor(rel_path, blocked)
    {
        return Outcome::Failed(format!(
            "inside local folder '{folder}', which could not be scanned ({reason})"
        ));
    }
    match remote.and_then(|remote| remote.problem.as_ref()) {
        Some(RemoteProblem::Reserved) => return Outcome::Failed(RESERVED_MESSAGE.to_owned()),
        Some(RemoteProblem::NotLocal(reason)) if outcome == Outcome::Copy(Side::Local) => {
            return Outcome::Failed(reason.clone());
        }
        _ => {}
    }
    if outcome == Outcome::Copy(Side::Remote) {
        let key_len = options.key_prefix.len() + rel_path.len();
        if key_len > MAX_KEY_BYTES {
            return Outcome::Failed(format!(
                "the S3 key would be {key_len} bytes long; the limit is {MAX_KEY_BYTES}"
            ));
        }
    }
    outcome
}

/// A download without `delete` never touches files that exist only locally.
fn leaves_local_only_alone(options: &PlanOptions<'_>) -> bool {
    options.direction == SyncDirection::Download && !options.delete
}

fn compare(local: &FileMeta, remote: &ObjectEntry, options: &PlanOptions<'_>) -> Outcome {
    let sizes_differ = local.size != remote.size;
    let tolerance = options.tolerance;
    match options.direction {
        SyncDirection::Upload | SyncDirection::S3ToS3 => {
            if sizes_differ || newer(local.modified, remote.last_modified, tolerance) {
                Outcome::Copy(Side::Remote)
            } else {
                Outcome::Equal
            }
        }
        SyncDirection::Download => {
            if sizes_differ || newer(remote.last_modified, local.modified, tolerance) {
                Outcome::Copy(Side::Local)
            } else {
                Outcome::Equal
            }
        }
        SyncDirection::Bidirectional => {
            if newer(local.modified, remote.last_modified, tolerance) {
                Outcome::Copy(Side::Remote)
            } else if newer(remote.last_modified, local.modified, tolerance) {
                Outcome::Copy(Side::Local)
            } else if !sizes_differ {
                Outcome::Equal
            } else if local.modified.is_some() && remote.last_modified.is_some() {
                Outcome::Failed(format!(
                    "conflict: the sizes differ ({} bytes locally, {} bytes in S3) but the modification times are within {} of each other, so the newer copy cannot be determined",
                    local.size,
                    remote.size,
                    describe(tolerance)
                ))
            } else {
                Outcome::Failed(format!(
                    "conflict: the sizes differ ({} bytes locally, {} bytes in S3) and a modification time is missing, so the newer copy cannot be determined",
                    local.size, remote.size
                ))
            }
        }
    }
}

/// `a` is newer than `b` by more than `tolerance`; false when either is unknown.
fn newer(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>, tolerance: TimeDelta) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.signed_duration_since(b) > tolerance,
        _ => false,
    }
}

fn describe(tolerance: TimeDelta) -> String {
    let millis = tolerance.num_milliseconds();
    if millis % 1000 == 0 {
        format!("{} s", millis / 1000)
    } else {
        format!("{millis} ms")
    }
}

/// Whether the entry the outcome reads or removes passes the metadata criteria.
fn source_matches(
    filter: &Filter,
    outcome: &Outcome,
    local: Option<&FileMeta>,
    remote: Option<&ObjectEntry>,
    direction: SyncDirection,
) -> bool {
    let local_matches = || local.is_some_and(|meta| filter.matches_local_metadata(meta));
    let remote_matches = || remote.is_some_and(|entry| filter.matches_object_metadata(entry));
    match outcome {
        Outcome::Copy(Side::Remote) | Outcome::Delete(Side::Local) => local_matches(),
        Outcome::Copy(Side::Local) | Outcome::Delete(Side::Remote) => remote_matches(),
        Outcome::Equal | Outcome::Failed(_) => match direction {
            SyncDirection::Upload | SyncDirection::S3ToS3 => local_matches(),
            SyncDirection::Download => remote_matches(),
            SyncDirection::Bidirectional => local_matches() || remote_matches(),
        },
        Outcome::Skip => false,
    }
}

fn blocked_ancestor<'b>(
    rel_path: &str,
    blocked: &'b HashMap<String, String>,
) -> Option<(&'b str, &'b str)> {
    if blocked.is_empty() {
        return None;
    }
    rel_path.match_indices('/').find_map(|(end, _)| {
        blocked
            .get_key_value(&rel_path[..end])
            .map(|(folder, reason)| (folder.as_str(), reason.as_str()))
    })
}

/// Fails local writes that would collide on the local file system.
fn veto_local_conflicts(drafts: &mut [Draft], case_insensitive: bool) {
    let mut vetoes = Vec::new();
    // Plain ASCII names on a case-sensitive file system cannot collide.
    if case_insensitive || drafts.iter().any(|draft| !draft.rel_path.is_ascii()) {
        name_collisions(drafts, &mut vetoes, case_insensitive);
    }
    file_folder_conflicts(drafts, &mut vetoes);
    // The first veto of a draft wins.
    for (index, reason) in vetoes {
        if let Some(draft) = drafts.get_mut(index)
            && draft.outcome.touches_local()
        {
            draft.outcome = Outcome::Failed(reason);
        }
    }
}

/// Comparison key of a local name: canonical Unicode equivalence (so a composed and a
/// decomposed accent compare equal, as they are one name on macOS file systems) and, when
/// the file system ignores letter case, Unicode lowercase per character, an approximation
/// of what each file system does. Never produces or removes "/".
fn fold_name(text: &str, case_insensitive: bool) -> Cow<'_, str> {
    if text.is_ascii() {
        return if case_insensitive {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
        };
    }
    let decomposed: String = text.nfd().collect();
    if case_insensitive {
        let lowered: String = decomposed.chars().flat_map(char::to_lowercase).collect();
        Cow::Owned(lowered.nfc().collect())
    } else {
        Cow::Owned(decomposed.nfc().collect())
    }
}

/// Why two spellings of one local name collide.
fn collision_message(mine: &str, other: &str) -> String {
    let same_normalization = fold_name(mine, false) == fold_name(other, false);
    let same_case = mine.to_lowercase() == other.to_lowercase();
    if same_normalization {
        format!(
            "'{mine}' and '{other}' differ only in Unicode normalization (a composed and a decomposed accent) and cannot both be written to this local folder: macOS file systems treat them as one name"
        )
    } else if same_case {
        format!(
            "'{mine}' and '{other}' differ only in letter case and cannot both exist in this local folder, whose file system ignores case"
        )
    } else {
        format!(
            "'{mine}' and '{other}' differ only in letter case and Unicode normalization and cannot both exist in this local folder"
        )
    }
}

/// End offsets of every segment prefix ("a", "a/b", "a/b/c") in `actual` and in its
/// case fold, which has the same number of "/" separators.
fn prefix_ends<'s>(actual: &'s str, folded: &'s str) -> impl Iterator<Item = (usize, usize)> + 's {
    actual
        .match_indices('/')
        .map(|(end, _)| end)
        .zip(folded.match_indices('/').map(|(end, _)| end))
        .chain(std::iter::once((actual.len(), folded.len())))
}

struct Spellings<'a> {
    first: &'a str,
    other: Option<&'a str>,
}

fn name_collisions(drafts: &[Draft], vetoes: &mut Vec<(usize, String)>, case_insensitive: bool) {
    // Every path that exists locally or that the plan creates locally.
    let occupied: Vec<usize> = drafts
        .iter()
        .enumerate()
        .filter(|(_, draft)| draft.local.is_some() || draft.outcome == Outcome::Copy(Side::Local))
        .map(|(index, _)| index)
        .collect();
    let folded: Vec<Cow<'_, str>> = occupied
        .iter()
        .map(|&index| fold_name(&drafts[index].rel_path, case_insensitive))
        .collect();
    let mut spellings: HashMap<&str, Spellings<'_>> = HashMap::new();
    for (&index, fold) in occupied.iter().zip(&folded) {
        let actual = drafts[index].rel_path.as_str();
        for (actual_end, fold_end) in prefix_ends(actual, fold) {
            let spelling = &actual[..actual_end];
            let seen = spellings.entry(&fold[..fold_end]).or_insert(Spellings {
                first: spelling,
                other: None,
            });
            if seen.first != spelling && seen.other.is_none() {
                seen.other = Some(spelling);
            }
        }
    }
    for (&index, fold) in occupied.iter().zip(&folded) {
        let draft = &drafts[index];
        if !draft.outcome.touches_local() {
            continue;
        }
        let actual = draft.rel_path.as_str();
        for (actual_end, fold_end) in prefix_ends(actual, fold) {
            let Some(seen) = spellings.get(&fold[..fold_end]) else {
                continue;
            };
            let mine = &actual[..actual_end];
            let other = if seen.first == mine {
                seen.other
            } else {
                Some(seen.first)
            };
            if let Some(other) = other {
                vetoes.push((index, collision_message(mine, other)));
                break;
            }
        }
    }
}

fn file_folder_conflicts(drafts: &[Draft], vetoes: &mut Vec<(usize, String)>) {
    // Local names once the plan has run; local deletions are applied before copies.
    let mut names_after: BTreeSet<&str> = BTreeSet::new();
    let mut files_after: HashSet<&str> = HashSet::new();
    for draft in drafts {
        let stays = draft.local.is_some() && draft.outcome != Outcome::Delete(Side::Local);
        if stays || draft.outcome == Outcome::Copy(Side::Local) {
            names_after.insert(&draft.rel_path);
            if !draft.is_local_folder() {
                files_after.insert(&draft.rel_path);
            }
        }
    }
    for (index, draft) in drafts.iter().enumerate() {
        if draft.outcome != Outcome::Copy(Side::Local) {
            continue;
        }
        let rel_path = draft.rel_path.as_str();
        if let Some(parent) = rel_path
            .match_indices('/')
            .map(|(end, _)| &rel_path[..end])
            .find(|parent| files_after.contains(parent))
        {
            vetoes.push((
                index,
                format!("cannot create '{rel_path}' because '{parent}' is a file"),
            ));
            continue;
        }
        let mut below = String::with_capacity(rel_path.len() + 1);
        below.push_str(rel_path);
        below.push('/');
        if let Some(child) = names_after
            .range::<str, _>((Bound::Included(below.as_str()), Bound::Unbounded))
            .next()
            .filter(|child| child.starts_with(below.as_str()))
        {
            vetoes.push((
                index,
                format!("cannot create the file '{rel_path}' because '{child}' needs a folder with that name"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::TimeZone;

    use super::*;
    use crate::error::ErrorKind;
    use crate::model::{Checksums, FilterSpec};
    use crate::sync::scan::scan_remote;

    const ROOT: &str = "/sync/root";

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + TimeDelta::seconds(secs)
    }

    fn meta(size: u64, modified: Option<DateTime<Utc>>) -> FileMeta {
        FileMeta { size, modified }
    }

    fn object(key: &str, size: u64, modified: Option<DateTime<Utc>>) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: Some("\"etag\"".to_owned()),
            last_modified: modified,
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    fn local(files: &[(&str, u64, Option<DateTime<Utc>>)]) -> LocalTree {
        let mut tree = LocalTree::default();
        for (rel_path, size, modified) in files {
            tree.insert_file(rel_path, meta(*size, *modified));
        }
        tree
    }

    fn remote(objects: &[(&str, u64, Option<DateTime<Utc>>)]) -> RemoteTree {
        scan_remote(
            objects.iter().map(|(rel_path, size, modified)| {
                object(&format!("p/{rel_path}"), *size, *modified)
            }),
            "p/",
            Path::new(ROOT),
            None,
        )
        .unwrap()
    }

    fn plan(
        local: LocalTree,
        remote: RemoteTree,
        direction: SyncDirection,
        delete: bool,
    ) -> Vec<SyncRow> {
        let mut options = PlanOptions::new(direction, "p/");
        options.delete = delete;
        plan_rows(local, remote, &options).unwrap()
    }

    /// (rel_path, action, target) triples.
    fn summary(rows: &[SyncRow]) -> Vec<(&str, SyncAction, Option<Side>)> {
        rows.iter()
            .map(|row| (row.rel_path.as_str(), row.action, row.target))
            .collect()
    }

    fn row<'r>(rows: &'r [SyncRow], rel_path: &str) -> &'r SyncRow {
        rows.iter().find(|row| row.rel_path == rel_path).unwrap()
    }

    use SyncAction::{Copy, Delete, Equal, Failed};

    #[test]
    fn upload_branches() {
        let t = Some(at(0));
        let local_side = local(&[
            ("both-equal", 5, t),
            ("both-size", 6, t),
            ("both-local-newer", 5, Some(at(10))),
            ("both-remote-newer", 5, t),
            ("local-only", 1, t),
        ]);
        let remote_side = || {
            remote(&[
                ("both-equal", 5, t),
                ("both-size", 5, Some(at(100))),
                ("both-local-newer", 5, t),
                ("both-remote-newer", 5, Some(at(100))),
                ("remote-only", 1, t),
            ])
        };
        let rows = plan(
            local_side.clone(),
            remote_side(),
            SyncDirection::Upload,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("both-equal", Equal, None),
                ("both-local-newer", Copy, Some(Side::Remote)),
                ("both-remote-newer", Equal, None),
                ("both-size", Copy, Some(Side::Remote)),
                ("local-only", Copy, Some(Side::Remote)),
            ]
        );
        let equal = row(&rows, "both-equal");
        assert!(equal.done);
        assert!(equal.local_meta.is_some() && equal.s3_meta.is_some());
        assert!(
            rows.iter()
                .filter(|row| row.action != Equal)
                .all(|row| !row.done && row.error.is_none())
        );

        let rows = plan(local_side, remote_side(), SyncDirection::Upload, true);
        let deleted = row(&rows, "remote-only");
        assert_eq!(
            (deleted.action, deleted.target),
            (Delete, Some(Side::Remote))
        );
        assert!(deleted.local_meta.is_none());
        assert_eq!(deleted.s3_meta.as_ref().unwrap().key, "p/remote-only");
    }

    #[test]
    fn download_branches() {
        let t = Some(at(0));
        let local_side = || {
            local(&[
                ("both-equal", 5, t),
                ("both-size", 6, Some(at(100))),
                ("both-local-newer", 5, Some(at(100))),
                ("both-remote-newer", 5, t),
                ("local-only", 1, t),
            ])
        };
        let remote_side = || {
            remote(&[
                ("both-equal", 5, t),
                ("both-size", 5, t),
                ("both-local-newer", 5, t),
                ("both-remote-newer", 5, Some(at(10))),
                ("remote-only", 1, t),
            ])
        };
        let rows = plan(local_side(), remote_side(), SyncDirection::Download, false);
        assert_eq!(
            summary(&rows),
            vec![
                ("both-equal", Equal, None),
                ("both-local-newer", Equal, None),
                ("both-remote-newer", Copy, Some(Side::Local)),
                ("both-size", Copy, Some(Side::Local)),
                ("remote-only", Copy, Some(Side::Local)),
            ]
        );
        let rows = plan(local_side(), remote_side(), SyncDirection::Download, true);
        let deleted = row(&rows, "local-only");
        assert_eq!(
            (deleted.action, deleted.target),
            (Delete, Some(Side::Local))
        );
        assert!(deleted.s3_meta.is_none());
        assert_eq!(deleted.local_meta.as_ref().unwrap().size, 1);
    }

    #[test]
    fn bidirectional_branches() {
        let t = Some(at(0));
        let rows = plan(
            local(&[
                ("equal", 5, t),
                ("local-newer", 5, Some(at(10))),
                ("remote-newer", 9, t),
                ("ambiguous", 6, Some(at(1))),
                ("missing-time-same-size", 5, None),
                ("missing-time-other-size", 6, None),
                ("local-only", 1, t),
            ]),
            remote(&[
                ("equal", 5, Some(at(1))),
                ("local-newer", 7, t),
                ("remote-newer", 5, Some(at(10))),
                ("ambiguous", 5, t),
                ("missing-time-same-size", 5, t),
                ("missing-time-other-size", 5, t),
                ("remote-only", 1, t),
            ]),
            SyncDirection::Bidirectional,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("ambiguous", Failed, None),
                ("equal", Equal, None),
                ("local-newer", Copy, Some(Side::Remote)),
                ("local-only", Copy, Some(Side::Remote)),
                ("missing-time-other-size", Failed, None),
                ("missing-time-same-size", Equal, None),
                ("remote-newer", Copy, Some(Side::Local)),
                ("remote-only", Copy, Some(Side::Local)),
            ]
        );
        let ambiguous = row(&rows, "ambiguous");
        let error = ambiguous.error.as_deref().unwrap();
        assert!(
            error.contains("6 bytes locally") && error.contains("within 2 s"),
            "{error}"
        );
        assert!(!ambiguous.done);
        assert!(
            row(&rows, "missing-time-other-size")
                .error
                .as_deref()
                .unwrap()
                .contains("missing")
        );
    }

    #[test]
    fn bidirectional_delete_is_a_usage_error() {
        let options = PlanOptions {
            delete: true,
            ..PlanOptions::new(SyncDirection::Bidirectional, "p/")
        };
        let err = plan_rows(LocalTree::default(), RemoteTree::default(), &options).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let negative = PlanOptions {
            tolerance: TimeDelta::seconds(-1),
            ..PlanOptions::new(SyncDirection::Upload, "p/")
        };
        let err = plan_rows(LocalTree::default(), RemoteTree::default(), &negative).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn tolerance_edges() {
        let base = at(0);
        let exactly = base + DEFAULT_TOLERANCE;
        let beyond = exactly + TimeDelta::nanoseconds(1);
        // Upload: local newer by exactly the tolerance is equal, a nanosecond more copies.
        let rows = plan(
            local(&[("exact", 1, Some(exactly)), ("beyond", 1, Some(beyond))]),
            remote(&[("exact", 1, Some(base)), ("beyond", 1, Some(base))]),
            SyncDirection::Upload,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![("beyond", Copy, Some(Side::Remote)), ("exact", Equal, None)]
        );
        // Download, mirrored.
        let rows = plan(
            local(&[("exact", 1, Some(base)), ("beyond", 1, Some(base))]),
            remote(&[("exact", 1, Some(exactly)), ("beyond", 1, Some(beyond))]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![("beyond", Copy, Some(Side::Local)), ("exact", Equal, None)]
        );
        // Bidirectional with different sizes: within tolerance is ambiguous, beyond has a winner.
        let rows = plan(
            local(&[("exact", 2, Some(base)), ("beyond", 2, Some(base))]),
            remote(&[("exact", 1, Some(exactly)), ("beyond", 1, Some(beyond))]),
            SyncDirection::Bidirectional,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![("beyond", Copy, Some(Side::Local)), ("exact", Failed, None)]
        );
        // A custom tolerance of zero makes any difference count.
        let options = PlanOptions {
            tolerance: TimeDelta::zero(),
            ..PlanOptions::new(SyncDirection::Upload, "p/")
        };
        let rows = plan_rows(
            local(&[("a", 1, Some(base + TimeDelta::milliseconds(1)))]),
            remote(&[("a", 1, Some(base))]),
            &options,
        )
        .unwrap();
        assert_eq!(summary(&rows), vec![("a", Copy, Some(Side::Remote))]);
    }

    #[test]
    fn missing_times_never_count_as_newer() {
        let rows = plan(
            local(&[("a", 1, None), ("b", 1, Some(at(100)))]),
            remote(&[("a", 1, Some(at(100))), ("b", 1, None)]),
            SyncDirection::Upload,
            false,
        );
        assert_eq!(summary(&rows), vec![("a", Equal, None), ("b", Equal, None)]);
    }

    #[test]
    fn rows_are_sorted_by_rel_path_bytes() {
        let t = Some(at(0));
        let rows = plan(
            local(&[("b", 1, t), ("a/z", 1, t), ("a.txt", 1, t), ("B", 1, t)]),
            remote(&[("a/a", 1, t), ("c", 1, t)]),
            SyncDirection::Upload,
            true,
        );
        let paths: Vec<&str> = rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["B", "a.txt", "a/a", "a/z", "b", "c"]);
    }

    #[test]
    fn local_failures_become_failed_rows_when_acted_on_and_shadow_the_remote() {
        let t = Some(at(0));
        let mut local_side = local(&[("ok", 1, t)]);
        local_side.insert_failed(
            "unreadable.txt",
            "cannot open file for reading: denied",
            false,
        );
        local_side.insert_failed("locked", "cannot read folder: denied", true);
        local_side.insert_unmapped("bad\\xFF", "name is not valid UTF-8");
        let remote_side = || {
            remote(&[
                ("unreadable.txt", 1, t),
                ("locked/inner.txt", 1, t),
                ("locked/deeper/x", 1, t),
                ("lockedX", 1, t),
            ])
        };
        // Upload with delete: nothing below the unreadable folder may be deleted.
        let rows = plan(
            local_side.clone(),
            remote_side(),
            SyncDirection::Upload,
            true,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("bad\\xFF", Failed, None),
                ("locked", Failed, None),
                ("locked/deeper/x", Failed, None),
                ("locked/inner.txt", Failed, None),
                ("lockedX", Delete, Some(Side::Remote)),
                ("ok", Copy, Some(Side::Remote)),
                ("unreadable.txt", Failed, None),
            ]
        );
        let shadowed = row(&rows, "locked/inner.txt");
        assert!(shadowed.error.as_deref().unwrap().contains("'locked'"));
        assert!(shadowed.s3_meta.is_some());
        assert!(row(&rows, "unreadable.txt").s3_meta.is_some());
        // Upload without delete: remote keys below the folder would not be touched anyway.
        let rows = plan(
            local_side.clone(),
            remote_side(),
            SyncDirection::Upload,
            false,
        );
        assert!(!rows.iter().any(|row| row.rel_path.starts_with("locked/")));
        // Download without delete: copying into the unknown folder is refused, a local
        // entry that S3 also has is failed, and local-only entries are left alone (no
        // row, exactly like readable local-only files).
        let rows = plan(
            local_side.clone(),
            remote_side(),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("locked/deeper/x", Failed, None),
                ("locked/inner.txt", Failed, None),
                ("lockedX", Copy, Some(Side::Local)),
                ("unreadable.txt", Failed, None),
            ]
        );
        // Download with delete: the local-only entries would be deleted, but cannot be.
        let rows = plan(local_side, remote_side(), SyncDirection::Download, true);
        assert_eq!(
            summary(&rows),
            vec![
                ("bad\\xFF", Failed, None),
                ("locked", Failed, None),
                ("locked/deeper/x", Failed, None),
                ("locked/inner.txt", Failed, None),
                ("lockedX", Copy, Some(Side::Local)),
                ("ok", Delete, Some(Side::Local)),
                ("unreadable.txt", Failed, None),
            ]
        );
    }

    #[test]
    fn skipped_local_failures_still_block_case_collisions() {
        let t = Some(at(0));
        let mut local_side = LocalTree::default();
        local_side.insert_failed("Notes.txt", "cannot read file information: denied", false);
        let rows = plan(
            local_side,
            remote(&[("notes.txt", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(summary(&rows), vec![("notes.txt", Failed, None)]);
        assert!(rows[0].error.as_deref().unwrap().contains("'Notes.txt'"));
    }

    #[test]
    fn unsafe_keys_fail_only_when_writing_locally() {
        let t = Some(at(0));
        let remote_side = || remote(&[("../escape", 1, t), ("a//b", 1, t), ("fine", 1, t)]);
        let rows = plan(
            LocalTree::default(),
            remote_side(),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("../escape", Failed, None),
                ("a//b", Failed, None),
                ("fine", Copy, Some(Side::Local))
            ]
        );
        assert!(
            row(&rows, "../escape")
                .error
                .as_deref()
                .unwrap()
                .contains("local file")
        );
        let rows = plan(
            LocalTree::default(),
            remote_side(),
            SyncDirection::Bidirectional,
            false,
        );
        assert_eq!(row(&rows, "a//b").action, Failed);
        // Uploading never writes locally: the keys are ordinary remote-only objects.
        let rows = plan(
            LocalTree::default(),
            remote_side(),
            SyncDirection::Upload,
            true,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("../escape", Delete, Some(Side::Remote)),
                ("a//b", Delete, Some(Side::Remote)),
                ("fine", Delete, Some(Side::Remote)),
            ]
        );
        let rows = plan(
            LocalTree::default(),
            remote_side(),
            SyncDirection::Upload,
            false,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn reserved_part_names_are_never_acted_on() {
        let t = Some(at(0));
        let rows = plan(
            LocalTree::default(),
            remote(&[("x.bbpart", 1, t)]),
            SyncDirection::Upload,
            true,
        );
        assert_eq!(summary(&rows), vec![("x.bbpart", Failed, None)]);
        assert!(rows[0].error.as_deref().unwrap().contains(".bbpart"));
        let rows = plan(
            LocalTree::default(),
            remote(&[("x.bbpart", 1, t)]),
            SyncDirection::Upload,
            false,
        );
        assert!(rows.is_empty());
        let rows = plan(
            LocalTree::default(),
            remote(&[("x.bbpart", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(summary(&rows), vec![("x.bbpart", Failed, None)]);
    }

    #[test]
    fn case_collisions_fail_local_writes() {
        let t = Some(at(0));
        // Two remote keys that differ only by case.
        let rows = plan(
            LocalTree::default(),
            remote(&[("Readme.md", 1, t), ("README.md", 2, t), ("other", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("README.md", Failed, None),
                ("Readme.md", Failed, None),
                ("other", Copy, Some(Side::Local))
            ]
        );
        assert!(
            row(&rows, "README.md")
                .error
                .as_deref()
                .unwrap()
                .contains("'Readme.md'")
        );

        // A local folder spelled differently from the remote one: the copy and the
        // deletion would hit the same file.
        let rows = plan(
            local(&[("Photos/a.jpg", 1, t), ("Photos/b.jpg", 1, t)]),
            remote(&[("photos/a.jpg", 1, t), ("Photos/b.jpg", 1, t)]),
            SyncDirection::Download,
            true,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("Photos/a.jpg", Failed, None),
                ("Photos/b.jpg", Equal, None),
                ("photos/a.jpg", Failed, None),
            ]
        );
        assert!(
            row(&rows, "photos/a.jpg")
                .error
                .as_deref()
                .unwrap()
                .contains("'photos' and 'Photos'")
        );

        // A new file whose parent folder exists with another spelling.
        let rows = plan(
            local(&[("Docs/a.txt", 1, t)]),
            remote(&[("docs/b.txt", 1, t)]),
            SyncDirection::Bidirectional,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("Docs/a.txt", Copy, Some(Side::Remote)),
                ("docs/b.txt", Failed, None)
            ]
        );

        // Case-sensitive file system: no collision.
        let options = PlanOptions {
            case_insensitive: false,
            ..PlanOptions::new(SyncDirection::Download, "p/")
        };
        let rows = plan_rows(
            LocalTree::default(),
            remote(&[("Readme.md", 1, t), ("README.md", 2, t)]),
            &options,
        )
        .unwrap();
        assert!(rows.iter().all(|row| row.action == Copy));

        // Uploads never write locally, so case does not matter.
        let rows = plan(
            local(&[("A", 1, t), ("a", 1, t)]),
            RemoteTree::default(),
            SyncDirection::Upload,
            false,
        );
        assert!(rows.iter().all(|row| row.action == Copy));
    }

    #[test]
    fn case_folding_handles_non_ascii() {
        let t = Some(at(0));
        let rows = plan(
            LocalTree::default(),
            remote(&[("\u{c9}t\u{e9}.txt", 1, t), ("\u{e9}t\u{e9}.txt", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert!(rows.iter().all(|row| row.action == Failed));
    }

    #[test]
    fn composed_and_decomposed_names_collide_on_every_file_system() {
        let t = Some(at(0));
        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        for case_insensitive in [false, true] {
            let options = PlanOptions {
                case_insensitive,
                ..PlanOptions::new(SyncDirection::Download, "p/")
            };
            // Two keys of one name: neither can be written next to the other.
            let rows = plan_rows(
                LocalTree::default(),
                remote(&[(composed, 10, t), (decomposed, 20, t), ("other.txt", 1, t)]),
                &options,
            )
            .unwrap();
            assert_eq!(
                summary(&rows),
                vec![
                    (decomposed, Failed, None),
                    (composed, Failed, None),
                    ("other.txt", Copy, Some(Side::Local)),
                ]
            );
            let error = row(&rows, composed).error.as_deref().unwrap();
            assert!(error.contains("Unicode normalization"), "{error}");

            // A local file spelled one way blocks the key spelled the other way.
            let rows = plan_rows(
                local(&[(decomposed, 5, t)]),
                remote(&[(composed, 5, t)]),
                &options,
            )
            .unwrap();
            assert_eq!(summary(&rows), vec![(composed, Failed, None)]);
        }
        // Folder names count too.
        let rows = plan(
            local(&[("cafe\u{301}/a.txt", 1, t)]),
            remote(&[("caf\u{e9}/b.txt", 1, t)]),
            SyncDirection::Bidirectional,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("cafe\u{301}/a.txt", Copy, Some(Side::Remote)),
                ("caf\u{e9}/b.txt", Failed, None),
            ]
        );
        // Both spellings and letter case together.
        let rows = plan(
            LocalTree::default(),
            remote(&[("CAF\u{c9}.txt", 1, t), (decomposed, 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert!(rows.iter().all(|row| row.action == Failed), "{rows:?}");
    }

    #[test]
    fn file_and_folder_with_the_same_name_cannot_both_be_created() {
        let t = Some(at(0));
        // Both are new: neither can be created next to the other.
        let rows = plan(
            LocalTree::default(),
            remote(&[("a", 1, t), ("a/b", 1, t), ("c", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("a", Failed, None),
                ("a/b", Failed, None),
                ("c", Copy, Some(Side::Local))
            ]
        );
        assert!(
            row(&rows, "a")
                .error
                .as_deref()
                .unwrap()
                .contains("needs a folder")
        );
        assert!(
            row(&rows, "a/b")
                .error
                .as_deref()
                .unwrap()
                .contains("is a file")
        );

        // A local file that stays blocks a folder of the same name.
        let rows = plan(
            local(&[("x", 1, t)]),
            remote(&[("x/y", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(summary(&rows), vec![("x/y", Failed, None)]);

        // With delete the local file is removed first, so the folder can be created.
        let rows = plan(
            local(&[("x", 1, t)]),
            remote(&[("x/y", 1, t)]),
            SyncDirection::Download,
            true,
        );
        assert_eq!(
            summary(&rows),
            vec![
                ("x", Delete, Some(Side::Local)),
                ("x/y", Copy, Some(Side::Local))
            ]
        );

        // A local folder that keeps a file blocks a file of the same name.
        let rows = plan(
            local(&[("d/keep", 1, t)]),
            remote(&[("d", 1, t), ("d/keep", 1, t)]),
            SyncDirection::Download,
            false,
        );
        assert_eq!(
            summary(&rows),
            vec![("d", Failed, None), ("d/keep", Equal, None)]
        );
    }

    #[test]
    fn filter_metadata_is_judged_on_the_source_and_never_causes_deletes() {
        let t = Some(at(0));
        let filter = Filter::compile(&FilterSpec {
            min_size: Some(100),
            ..FilterSpec::default()
        })
        .unwrap();
        let options = PlanOptions {
            delete: true,
            filter: Some(&filter),
            ..PlanOptions::new(SyncDirection::Upload, "p/")
        };
        let rows = plan_rows(
            local(&[
                ("small-local", 10, Some(at(50))),
                ("big-local", 200, t),
                ("big-new", 300, t),
            ]),
            remote(&[
                ("small-local", 500, t),
                ("big-local", 5, t),
                ("small-orphan", 1, t),
                ("big-orphan", 1000, t),
            ]),
            &options,
        )
        .unwrap();
        // "small-local" is out of scope (its source is small), so the big remote copy is
        // neither overwritten nor deleted; the small orphan is not deleted either.
        assert_eq!(
            summary(&rows),
            vec![
                ("big-local", Copy, Some(Side::Remote)),
                ("big-new", Copy, Some(Side::Remote)),
                ("big-orphan", Delete, Some(Side::Remote)),
            ]
        );

        let options = PlanOptions {
            filter: Some(&filter),
            ..PlanOptions::new(SyncDirection::Bidirectional, "p/")
        };
        let rows = plan_rows(
            local(&[
                ("loser-matches", 500, t),
                ("winner-matches", 500, Some(at(50))),
            ]),
            remote(&[("loser-matches", 5, Some(at(50))), ("winner-matches", 5, t)]),
            &options,
        )
        .unwrap();
        assert_eq!(
            summary(&rows),
            vec![("winner-matches", Copy, Some(Side::Remote))]
        );
    }

    #[test]
    fn storage_class_filter_treats_local_files_as_standard() {
        let t = Some(at(0));
        let filter = Filter::compile(&FilterSpec {
            storage_classes: vec!["GLACIER".to_owned()],
            ..FilterSpec::default()
        })
        .unwrap();
        let mut glacier = object("p/cold", 1, t);
        glacier.storage_class = Some("GLACIER".to_owned());
        let remote_side = scan_remote(
            [glacier, object("p/warm", 1, t)],
            "p/",
            Path::new(ROOT),
            None,
        )
        .unwrap();
        let options = PlanOptions {
            filter: Some(&filter),
            ..PlanOptions::new(SyncDirection::Download, "p/")
        };
        let rows = plan_rows(local(&[("local-only", 1, t)]), remote_side, &options).unwrap();
        assert_eq!(summary(&rows), vec![("cold", Copy, Some(Side::Local))]);
    }

    #[test]
    fn keys_longer_than_the_limit_fail_on_upload() {
        let t = Some(at(0));
        let fits = "f".repeat(MAX_KEY_BYTES - 2);
        let too_long = "x".repeat(MAX_KEY_BYTES - 1);
        let rows = plan(
            local(&[(&fits, 1, t), (&too_long, 1, t)]),
            RemoteTree::default(),
            SyncDirection::Upload,
            false,
        );
        assert_eq!(row(&rows, &fits).action, Copy);
        let failed = row(&rows, &too_long);
        assert_eq!(failed.action, Failed);
        assert!(failed.error.as_deref().unwrap().contains("1025 bytes"));
    }

    #[test]
    fn planning_is_deterministic() {
        let t = Some(at(0));
        let build = || {
            plan(
                local(&[("b", 1, t), ("A", 2, t), ("c/d", 3, Some(at(9)))]),
                remote(&[("a", 1, t), ("b", 2, t), ("c/d", 3, t)]),
                SyncDirection::Bidirectional,
                false,
            )
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn large_trees_are_planned_in_reasonable_time() {
        const FILES: usize = 100_000;
        let t = Some(at(0));
        let mut local_side = LocalTree::default();
        for i in 0..FILES {
            local_side.insert_file(&format!("dir{:03}/file-{i:06}.dat", i % 500), meta(10, t));
        }
        let listing = (0..FILES).map(|i| {
            // Every tenth file differs in size, every hundredth exists only remotely.
            let rel_path = if i % 100 == 0 {
                format!("remote-only/file-{i:06}.dat")
            } else {
                format!("dir{:03}/file-{i:06}.dat", i % 500)
            };
            let size = if i % 10 == 0 { 11 } else { 10 };
            object(&format!("p/{rel_path}"), size, t)
        });
        let remote_side = scan_remote(listing, "p/", Path::new(ROOT), None).unwrap();
        let started = std::time::Instant::now();
        let options = PlanOptions {
            delete: true,
            ..PlanOptions::new(SyncDirection::Download, "p/")
        };
        let rows = plan_rows(local_side, remote_side, &options).unwrap();
        let elapsed = started.elapsed();
        let count = |action: SyncAction| rows.iter().filter(|row| row.action == action).count();
        assert_eq!(count(Copy), FILES / 10);
        assert_eq!(count(Delete), FILES / 100);
        assert_eq!(count(Equal), FILES - FILES / 10);
        assert_eq!(count(Failed), 0);
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].rel_path < pair[1].rel_path)
        );
        eprintln!("planned {} rows in {elapsed:?}", rows.len());
        // Generous (a debug build takes a few seconds): this only catches quadratic work.
        assert!(elapsed < std::time::Duration::from_secs(60), "{elapsed:?}");
    }
}
