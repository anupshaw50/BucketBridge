//! The two inputs of the planner: a scan of the local folder ([`scan_local`]) and the
//! remote listing keyed by relative path ([`scan_remote`]).
//!
//! # Local scan rules
//!
//! - Paths are relative to the local root and use "/" as the separator on every OS.
//! - Only files are synced; folders are walked (iteratively, so depth is not limited by
//!   the stack) and empty folders are ignored.
//! - Symbolic links to folders (and Windows junctions) are never followed, so link
//!   cycles cannot occur; each one becomes a failed entry because its contents are
//!   unknown. On Unix a folder reached twice (bind mounts) is also reported instead of
//!   being walked again.
//! - Symbolic links to files are followed: the link is synced as a regular file with the
//!   size and modification time of its target. A link whose target is missing, or that
//!   points at something other than a file or folder, is a failed entry. A link whose
//!   target cannot be examined (for example no permission), and an entry whose type
//!   cannot be read, are failed entries treated like unreadable folders, since they may
//!   be folders.
//! - Devices, sockets and pipes are failed entries (they are never opened).
//! - Entries that cannot be read are failed entries, never silently dropped: a folder
//!   that cannot be listed, a file whose metadata cannot be read, and, when
//!   [`LocalScanOptions::check_readable`] is set, a file that cannot be opened for
//!   reading. A folder whose listing fails part way is reported as a whole.
//! - Names that are not valid UTF-8 cannot become S3 keys; they are reported with a
//!   printable form of the path (invalid bytes written as `\xNN`, or `\u{XXXX}` for
//!   unpaired UTF-16 surrogates on Windows). Such a folder is not entered.
//! - Files whose name ends in [`PART_SUFFIX`] are the downloader's temporary files and
//!   are skipped.
//! - Only the path criteria of the optional [`Filter`] (prefix and glob) are applied
//!   here; folders that cannot contain a match are not entered. Size, date and storage
//!   class criteria are applied by the planner, which knows both sides of each path.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirEntry, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::{FileMeta, ObjectEntry};
use crate::path::safe_local_path;

/// Suffix of the downloader's temporary files (the download engine's
/// [`PARTIAL_SUFFIX`](crate::engine::PARTIAL_SUFFIX)). Local files with this suffix are
/// skipped and remote keys with it are never synced.
pub const PART_SUFFIX: &str = crate::engine::PARTIAL_SUFFIX;

/// How many existing names [`is_case_insensitive`] tries before giving up.
const CASE_PROBE_LIMIT: usize = 8;

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

fn not_found(message: impl Into<String>) -> Error {
    Error::NotFound(message.into())
}

fn io_error(context: impl Into<String>, err: io::Error) -> Error {
    Error::io(context, err)
}

/// One local path as seen by the scan.
#[derive(Debug, Clone)]
pub(crate) enum LocalEntry {
    File(FileMeta),
    Failed(LocalFailure),
}

/// A local entry that exists but cannot be synced.
#[derive(Debug, Clone)]
pub(crate) struct LocalFailure {
    /// Metadata when it could be read (for example a file that cannot be opened).
    pub(crate) meta: Option<FileMeta>,
    pub(crate) error: String,
    /// The entry is a folder whose contents are unknown (unreadable, a symbolic link, a
    /// loop); remote keys below it must not be acted on.
    pub(crate) folder: bool,
}

/// A local entry whose name cannot be represented as a relative path (not UTF-8).
#[derive(Debug, Clone)]
pub(crate) struct Unmapped {
    pub(crate) display: String,
    pub(crate) error: String,
}

/// Result of [`scan_local`].
#[derive(Debug, Clone, Default)]
pub struct LocalTree {
    pub(crate) entries: BTreeMap<String, LocalEntry>,
    pub(crate) unmapped: Vec<Unmapped>,
    /// Folders without any entry, `/`-separated and without a trailing `/`, in scan
    /// order; filled only with [`LocalScanOptions::collect_empty_dirs`].
    pub(crate) empty_dirs: Vec<String>,
}

impl LocalTree {
    /// Files that can be synced, keyed by relative path.
    pub fn files(&self) -> impl Iterator<Item = (&str, &FileMeta)> {
        self.entries
            .iter()
            .filter_map(|(rel_path, entry)| match entry {
                LocalEntry::File(meta) => Some((rel_path.as_str(), meta)),
                LocalEntry::Failed(_) => None,
            })
    }

    /// Number of files that can be synced.
    pub fn file_count(&self) -> usize {
        self.files().count()
    }

    /// Number of entries that will become failed rows.
    pub fn problem_count(&self) -> usize {
        self.entries.len() - self.file_count() + self.unmapped.len()
    }

    #[cfg(test)]
    pub(crate) fn insert_file(&mut self, rel_path: &str, meta: FileMeta) {
        self.entries
            .insert(rel_path.to_owned(), LocalEntry::File(meta));
    }

    #[cfg(test)]
    pub(crate) fn insert_failed(&mut self, rel_path: &str, error: &str, folder: bool) {
        self.entries.insert(
            rel_path.to_owned(),
            LocalEntry::Failed(LocalFailure {
                meta: None,
                error: error.to_owned(),
                folder,
            }),
        );
    }

    #[cfg(test)]
    pub(crate) fn insert_unmapped(&mut self, display: &str, error: &str) {
        self.unmapped.push(Unmapped {
            display: display.to_owned(),
            error: error.to_owned(),
        });
    }
}

/// Options of [`scan_local`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalScanOptions<'a> {
    /// Only the prefix and glob criteria are applied by the scan.
    pub filter: Option<&'a Filter>,
    /// Open every file once to make sure it can be read; set when the plan may upload.
    pub check_readable: bool,
    /// Also report folders that hold nothing at all ([`LocalTree::empty_dirs`]).
    pub collect_empty_dirs: bool,
}

/// Scans `root` (which must be an existing folder; a symbolic link to a folder is
/// followed for the root itself). Blocking: run it on a blocking thread.
///
/// Errors: `not_found` when `root` does not exist, `usage` when it is not a folder, `io`
/// when the root itself cannot be listed. Problems below the root never fail the scan;
/// they become failed entries.
pub fn scan_local(root: &Path, options: &LocalScanOptions<'_>) -> Result<LocalTree> {
    let root_meta = fs::metadata(root).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            not_found(format!("local folder {} does not exist", root.display()))
        } else {
            io_error(format!("cannot read local folder {}", root.display()), err)
        }
    })?;
    if !root_meta.is_dir() {
        return Err(usage(format!("{} is not a folder", root.display())));
    }
    let mut scanner = Scanner {
        options,
        tree: LocalTree::default(),
        #[cfg(unix)]
        seen_folders: HashSet::from([folder_id(&root_meta)]),
    };
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel_path)) = pending.pop() {
        scanner.scan_folder(&dir, &rel_path, &mut pending)?;
    }
    let mut tree = scanner.tree;
    tree.unmapped.sort_by(|a, b| a.display.cmp(&b.display));
    tree.empty_dirs.sort();
    Ok(tree)
}

struct Scanner<'o, 'f> {
    options: &'o LocalScanOptions<'f>,
    tree: LocalTree,
    #[cfg(unix)]
    seen_folders: HashSet<(u64, u64)>,
}

impl Scanner<'_, '_> {
    fn path_in_scope(&self, rel_path: &str) -> bool {
        self.options
            .filter
            .is_none_or(|filter| filter.matches_path(rel_path))
    }

    fn folder_in_scope(&self, rel_path: &str) -> bool {
        self.options
            .filter
            .is_none_or(|filter| filter.may_match_below(rel_path))
    }

    fn fail(&mut self, rel_path: String, meta: Option<FileMeta>, error: String, folder: bool) {
        self.tree.entries.insert(
            rel_path,
            LocalEntry::Failed(LocalFailure {
                meta,
                error,
                folder,
            }),
        );
    }

    fn scan_folder(
        &mut self,
        dir: &Path,
        rel_path: &str,
        pending: &mut Vec<(PathBuf, String)>,
    ) -> Result<()> {
        let is_root = rel_path.is_empty();
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(err) if is_root => {
                return Err(io_error(
                    format!("cannot list local folder {}", dir.display()),
                    err,
                ));
            }
            Err(err) => {
                self.fail(
                    rel_path.to_owned(),
                    None,
                    format!("cannot read folder: {err}"),
                    true,
                );
                return Ok(());
            }
        };
        let mut children: Vec<(OsString, DirEntry)> = Vec::new();
        for item in listing {
            match item {
                Ok(entry) => children.push((entry.file_name(), entry)),
                Err(err) if is_root => {
                    return Err(io_error(
                        format!("listing local folder {} failed part way", dir.display()),
                        err,
                    ));
                }
                Err(err) => {
                    // A partial listing must not be mistaken for the folder's contents.
                    self.fail(
                        rel_path.to_owned(),
                        None,
                        format!("listing the folder failed part way: {err}"),
                        true,
                    );
                    return Ok(());
                }
            }
        }
        if children.is_empty() && !is_root && self.options.collect_empty_dirs {
            self.tree.empty_dirs.push(rel_path.to_owned());
        }
        children.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in children {
            self.visit(&entry, &name, rel_path, pending);
        }
        Ok(())
    }

    fn visit(
        &mut self,
        entry: &DirEntry,
        name: &OsStr,
        parent: &str,
        pending: &mut Vec<(PathBuf, String)>,
    ) {
        let file_type = entry.file_type();
        let Some(name) = name.to_str() else {
            let display = join_rel(parent, &escape_name(name));
            let is_folder = file_type.as_ref().is_ok_and(fs::FileType::is_dir);
            let (in_scope, error) = if is_folder {
                (
                    self.folder_in_scope(&display),
                    "folder name is not valid UTF-8; the folder and its contents cannot be synced",
                )
            } else {
                (
                    self.path_in_scope(&display),
                    "name is not valid UTF-8 and cannot be used in an S3 key",
                )
            };
            if in_scope {
                self.tree.unmapped.push(Unmapped {
                    display,
                    error: error.to_owned(),
                });
            }
            return;
        };
        let rel_path = join_rel(parent, name);
        let file_type = match file_type {
            Ok(file_type) => file_type,
            Err(err) => {
                // Unknown type: it may be a folder, so it also shadows remote keys below it.
                if self.path_in_scope(&rel_path) || self.folder_in_scope(&rel_path) {
                    self.fail(
                        rel_path,
                        None,
                        format!("cannot read file information: {err}"),
                        true,
                    );
                }
                return;
            }
        };
        if file_type.is_dir() {
            if self.folder_in_scope(&rel_path) {
                self.enter_folder(entry, rel_path, pending);
            }
            return;
        }
        if name.ends_with(PART_SUFFIX) {
            // The downloader's temporary files are regular files and are skipped. A link
            // with such a name may lead to a folder (or to something that cannot be
            // examined), whose contents are unknown: remote keys below it must not be
            // acted on, exactly as for any other link.
            if file_type.is_symlink() {
                self.visit_symlink(&entry.path(), rel_path, true);
            }
            return;
        }
        if file_type.is_symlink() {
            self.visit_symlink(&entry.path(), rel_path, false);
            return;
        }
        if !self.path_in_scope(&rel_path) {
            return;
        }
        if !file_type.is_file() {
            self.fail(
                rel_path,
                None,
                "not a regular file (device, socket or pipe)".to_owned(),
                false,
            );
            return;
        }
        match entry.metadata() {
            Ok(meta) => self.add_file(&entry.path(), rel_path, &meta),
            Err(err) => self.fail(
                rel_path,
                None,
                format!("cannot read file information: {err}"),
                false,
            ),
        }
    }

    #[cfg(unix)]
    fn enter_folder(
        &mut self,
        entry: &DirEntry,
        rel_path: String,
        pending: &mut Vec<(PathBuf, String)>,
    ) {
        match entry.metadata() {
            Ok(meta) => {
                if self.seen_folders.insert(folder_id(&meta)) {
                    pending.push((entry.path(), rel_path));
                } else {
                    self.fail(
                        rel_path,
                        None,
                        "folder loop detected (this folder is mounted inside itself)".to_owned(),
                        true,
                    );
                }
            }
            Err(err) => self.fail(
                rel_path,
                None,
                format!("cannot read folder information: {err}"),
                true,
            ),
        }
    }

    #[cfg(not(unix))]
    fn enter_folder(
        &mut self,
        entry: &DirEntry,
        rel_path: String,
        pending: &mut Vec<(PathBuf, String)>,
    ) {
        pending.push((entry.path(), rel_path));
    }

    /// `reserved`: the link's name ends in [`PART_SUFFIX`]. Such a link is skipped like a
    /// downloader temporary file unless it leads to a folder or cannot be examined.
    fn visit_symlink(&mut self, path: &Path, rel_path: String, reserved: bool) {
        // `fs::metadata` follows the link.
        match fs::metadata(path) {
            Ok(meta) if meta.is_dir() => {
                if self.folder_in_scope(&rel_path) {
                    self.fail(
                        rel_path,
                        None,
                        "symbolic link to a folder; links to folders are not followed".to_owned(),
                        true,
                    );
                }
            }
            // A reserved name that leads to a file, to something else or nowhere is
            // skipped like a temporary file.
            Ok(_) if reserved => {}
            Err(err) if reserved && err.kind() == io::ErrorKind::NotFound => {}
            Ok(meta) if meta.is_file() => {
                if self.path_in_scope(&rel_path) {
                    self.add_file(path, rel_path, &meta);
                }
            }
            Ok(_) => {
                if self.path_in_scope(&rel_path) {
                    self.fail(
                        rel_path,
                        None,
                        "symbolic link to something that is not a regular file".to_owned(),
                        false,
                    );
                }
            }
            // A dangling link: nothing exists below it.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if self.path_in_scope(&rel_path) {
                    self.fail(
                        rel_path,
                        None,
                        format!("broken symbolic link: {err}"),
                        false,
                    );
                }
            }
            // Unknown target (for example no permission): it may be a folder, so it
            // also shadows remote keys below it.
            Err(err) => {
                if self.path_in_scope(&rel_path) || self.folder_in_scope(&rel_path) {
                    self.fail(
                        rel_path,
                        None,
                        format!("cannot follow symbolic link: {err}"),
                        true,
                    );
                }
            }
        }
    }

    fn add_file(&mut self, path: &Path, rel_path: String, meta: &Metadata) {
        let file_meta = file_meta(meta);
        if self.options.check_readable
            && let Err(err) = File::open(path)
        {
            self.fail(
                rel_path,
                Some(file_meta),
                format!("cannot open file for reading: {err}"),
                false,
            );
            return;
        }
        self.tree
            .entries
            .insert(rel_path, LocalEntry::File(file_meta));
    }
}

#[cfg(unix)]
fn folder_id(meta: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        let mut rel_path = String::with_capacity(parent.len() + 1 + name.len());
        rel_path.push_str(parent);
        rel_path.push('/');
        rel_path.push_str(name);
        rel_path
    }
}

/// Printable form of a name that is not valid Unicode.
#[cfg(unix)]
pub(crate) fn escape_name(name: &OsStr) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt;

    let mut out = String::with_capacity(name.len() + 8);
    for chunk in name.as_bytes().utf8_chunks() {
        out.push_str(chunk.valid());
        for byte in chunk.invalid() {
            let _ = write!(out, "\\x{byte:02X}");
        }
    }
    out
}

/// Printable form of a name that is not valid Unicode.
#[cfg(windows)]
pub(crate) fn escape_name(name: &OsStr) -> String {
    use std::os::windows::ffi::OsStrExt;

    char::decode_utf16(name.encode_wide())
        .map(|unit| match unit {
            Ok(c) => c.to_string(),
            Err(err) => format!("\\u{{{:04X}}}", err.unpaired_surrogate()),
        })
        .collect()
}

/// Printable form of a name that is not valid Unicode.
#[cfg(not(any(unix, windows)))]
pub(crate) fn escape_name(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

/// Size and modification time (UTC) of a file.
pub(crate) fn file_meta(meta: &Metadata) -> FileMeta {
    FileMeta {
        size: meta.len(),
        modified: meta.modified().ok().and_then(system_time_to_utc),
    }
}

/// Converts a file time without the panic of chrono's `From<SystemTime>` on
/// out-of-range values; such times count as unknown.
pub(crate) fn system_time_to_utc(time: SystemTime) -> Option<DateTime<Utc>> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => {
            DateTime::from_timestamp(i64::try_from(after.as_secs()).ok()?, after.subsec_nanos())
        }
        Err(err) => {
            let before = err.duration();
            let secs = i64::try_from(before.as_secs()).ok()?;
            match before.subsec_nanos() {
                0 => DateTime::from_timestamp(secs.checked_neg()?, 0),
                nanos => DateTime::from_timestamp(
                    secs.checked_neg()?.checked_sub(1)?,
                    1_000_000_000 - nanos,
                ),
            }
        }
    }
}

/// Why a remote key cannot be handled like an ordinary file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteProblem {
    /// The key cannot be written as a local file (unsafe or ambiguous as a path).
    NotLocal(String),
    /// The name ends in [`PART_SUFFIX`]; local scans skip such names, so the local
    /// state of the path is unknown.
    Reserved,
}

/// One remote object keyed by relative path.
#[derive(Debug, Clone)]
pub(crate) struct RemoteObject {
    pub(crate) entry: ObjectEntry,
    pub(crate) problem: Option<RemoteProblem>,
}

/// Result of [`scan_remote`].
#[derive(Debug, Clone, Default)]
pub struct RemoteTree {
    pub(crate) objects: BTreeMap<String, RemoteObject>,
}

impl RemoteTree {
    /// Number of objects in scope.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// True when no object is in scope.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

/// Checks that a key prefix is empty (the whole bucket) or ends with "/".
pub fn validate_key_prefix(key_prefix: &str) -> Result<()> {
    if key_prefix.is_empty() || key_prefix.ends_with('/') {
        Ok(())
    } else {
        Err(usage(format!(
            "the S3 prefix '{key_prefix}' must be empty or end with '/'"
        )))
    }
}

/// Builds the remote side from a flat listing of `key_prefix` (no delimiter).
///
/// Folder markers and prefix entries are skipped, keys outside `key_prefix` are ignored,
/// and when a key occurs more than once the last occurrence wins. Keys that cannot be
/// stored as a file below `local_root` (rejected by [`safe_local_path`]: keys that could
/// leave the folder, empty, "." and ".." segments that would alias another key, and on
/// Windows names Windows cannot store) and names ending in [`PART_SUFFIX`] are kept but
/// flagged; the planner turns them into failed rows when the plan would act on them
/// locally.
pub fn scan_remote<I>(
    entries: I,
    key_prefix: &str,
    local_root: &Path,
    filter: Option<&Filter>,
) -> Result<RemoteTree>
where
    I: IntoIterator<Item = ObjectEntry>,
{
    validate_key_prefix(key_prefix)?;
    let mut tree = RemoteTree::default();
    for entry in entries {
        if entry.is_prefix || entry.key.ends_with('/') {
            continue;
        }
        let Some(rel_path) = entry.key.strip_prefix(key_prefix) else {
            continue;
        };
        if rel_path.is_empty() || filter.is_some_and(|filter| !filter.matches_path(rel_path)) {
            continue;
        }
        let problem = remote_problem(rel_path, local_root);
        let rel_path = rel_path.to_owned();
        tree.objects
            .insert(rel_path, RemoteObject { entry, problem });
    }
    Ok(tree)
}

fn remote_problem(rel_path: &str, local_root: &Path) -> Option<RemoteProblem> {
    let name = rel_path.rsplit_once('/').map_or(rel_path, |(_, name)| name);
    if name.ends_with(PART_SUFFIX) {
        return Some(RemoteProblem::Reserved);
    }
    // The path module rejects keys that could leave the folder or alias another key
    // (empty, "." and ".." segments) and, on Windows, names Windows cannot store.
    match safe_local_path(local_root, rel_path) {
        Ok(_) => None,
        Err(err) => Some(RemoteProblem::NotLocal(format!(
            "the key cannot be stored as a local file: {err}"
        ))),
    }
}

/// Best-effort, read-only test of whether the file system holding `local_root` ignores
/// letter case. Looks up an existing name from `tree` with its ASCII letters' case
/// swapped: same file means case-insensitive, missing or a different file means
/// case-sensitive. Windows is always treated as case-insensitive, and so is a folder
/// that offers no usable name (the safe assumption: it can only produce extra failed
/// rows, never an overwrite).
pub fn is_case_insensitive(local_root: &Path, tree: &LocalTree) -> bool {
    if cfg!(windows) {
        return true;
    }
    for rel_path in tree
        .entries
        .keys()
        .filter(|rel_path| {
            let name = rel_path
                .rsplit_once('/')
                .map_or(rel_path.as_str(), |(_, name)| name);
            name.bytes().any(|byte| byte.is_ascii_alphabetic())
        })
        .take(CASE_PROBE_LIMIT)
    {
        if let Some(answer) = probe_case(local_root, rel_path) {
            return answer;
        }
    }
    true
}

fn probe_case(local_root: &Path, rel_path: &str) -> Option<bool> {
    let (parent, name) = match rel_path.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None => (None, rel_path),
    };
    let swapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let mut folder = local_root.to_path_buf();
    if let Some(parent) = parent {
        folder.extend(parent.split('/'));
    }
    let original = fs::symlink_metadata(folder.join(name)).ok()?;
    match fs::symlink_metadata(folder.join(&swapped)) {
        Ok(other) => Some(same_file(&original, &other)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    folder_id(a) == folder_id(b)
}

#[cfg(not(unix))]
fn same_file(_a: &Metadata, _b: &Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::error::ErrorKind;
    use crate::model::{Checksums, FilterSpec};

    fn write(root: &Path, rel_path: &str, contents: &str) {
        let path = root.join(rel_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn scan(root: &Path) -> LocalTree {
        scan_local(
            root,
            &LocalScanOptions {
                filter: None,
                check_readable: true,
                ..LocalScanOptions::default()
            },
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn failure<'t>(tree: &'t LocalTree, rel_path: &str) -> &'t LocalFailure {
        match tree.entries.get(rel_path) {
            Some(LocalEntry::Failed(failure)) => failure,
            other => panic!("expected a failed entry for {rel_path}, got {other:?}"),
        }
    }

    fn object(key: &str) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size: 1,
            etag: None,
            last_modified: None,
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    #[test]
    fn scans_nested_files_with_slash_separators() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "aa");
        write(dir.path(), "sub/b.txt", "bbb");
        write(dir.path(), "sub/deeper/c.txt", "");
        fs::create_dir_all(dir.path().join("empty/folder")).unwrap();
        let tree = scan(dir.path());
        let files: Vec<(&str, u64)> = tree.files().map(|(path, meta)| (path, meta.size)).collect();
        assert_eq!(
            files,
            vec![("a.txt", 2), ("sub/b.txt", 3), ("sub/deeper/c.txt", 0)]
        );
        assert_eq!(tree.problem_count(), 0);
        let modified = tree.files().next().unwrap().1.modified.unwrap();
        assert!((Utc::now() - modified).num_seconds().abs() < 600);
    }

    #[test]
    fn missing_root_and_file_root_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let options = LocalScanOptions::default();
        let err = scan_local(&dir.path().join("missing"), &options).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        write(dir.path(), "file", "x");
        let err = scan_local(&dir.path().join("file"), &options).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real/a.txt", "a");
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        let tree = scan(&dir.path().join("alias"));
        let files: Vec<&str> = tree.files().map(|(path, _)| path).collect();
        assert_eq!(files, vec!["a.txt"]);
    }

    #[test]
    fn part_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "a");
        write(dir.path(), "a.txt.bbpart", "partial");
        write(dir.path(), "folder.bbpart/inside.txt", "kept");
        let tree = scan(dir.path());
        let files: Vec<&str> = tree.files().map(|(path, _)| path).collect();
        assert_eq!(files, vec!["a.txt", "folder.bbpart/inside.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_named_like_part_files_are_skipped_unless_they_lead_to_a_folder() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        write(&root, "plain.txt", "p");
        write(dir.path(), "outside/y.txt", "y");
        write(dir.path(), "target.txt", "t");
        symlink(dir.path().join("outside"), root.join("x.bbpart")).unwrap();
        symlink(dir.path().join("target.txt"), root.join("file.bbpart")).unwrap();
        symlink(dir.path().join("missing"), root.join("broken.bbpart")).unwrap();
        let tree = scan(&root);
        // A link to a folder has unknown contents, whatever its name: remote keys below
        // it must not be acted on.
        assert!(failure(&tree, "x.bbpart").folder);
        // A link to a file or to nothing is skipped like a temporary file.
        let files: Vec<&str> = tree.files().map(|(path, _)| path).collect();
        assert_eq!(files, vec!["plain.txt"]);
        assert_eq!(tree.entries.len(), 2);
    }

    #[test]
    fn path_filter_is_applied_and_prunes_folders() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "keep/a.jpg", "a");
        write(dir.path(), "keep/b.txt", "b");
        write(dir.path(), "skip/c.jpg", "c");
        let filter = Filter::compile(&FilterSpec {
            prefix: Some("keep/".to_owned()),
            glob: Some("**/*.jpg".to_owned()),
            // Metadata criteria are not applied by the scan.
            min_size: Some(1000),
            ..FilterSpec::default()
        })
        .unwrap();
        let tree = scan_local(
            dir.path(),
            &LocalScanOptions {
                filter: Some(&filter),
                check_readable: false,
                ..LocalScanOptions::default()
            },
        )
        .unwrap();
        let files: Vec<&str> = tree.files().map(|(path, _)| path).collect();
        assert_eq!(files, vec!["keep/a.jpg"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_files_followed_folders_reported_loops_impossible() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        write(&root, "real/file.txt", "12345");
        write(dir.path(), "outside.txt", "outside");
        symlink(root.join("real/file.txt"), root.join("link.txt")).unwrap();
        symlink(dir.path().join("outside.txt"), root.join("out-link.txt")).unwrap();
        // A link to an ancestor would loop forever if it were followed.
        symlink(&root, root.join("real/loop")).unwrap();
        symlink(root.join("real"), root.join("folder-link")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();

        let tree = scan(&root);
        let files: Vec<(&str, u64)> = tree.files().map(|(path, meta)| (path, meta.size)).collect();
        assert_eq!(
            files,
            vec![("link.txt", 5), ("out-link.txt", 7), ("real/file.txt", 5)]
        );
        assert!(failure(&tree, "real/loop").folder);
        assert!(failure(&tree, "folder-link").folder);
        let broken = failure(&tree, "broken");
        assert!(!broken.folder);
        assert!(broken.error.contains("symbolic link"), "{}", broken.error);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_entries_become_failures() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "secret.txt", "s");
        write(dir.path(), "locked/inner.txt", "i");
        write(dir.path(), "open.txt", "o");
        let secret = dir.path().join("secret.txt");
        let locked = dir.path().join("locked");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Running as root bypasses permissions; there is nothing to test then.
        let enforced = File::open(&secret).is_err() && fs::read_dir(&locked).is_err();
        let tree = scan(dir.path());
        let without_check = scan_local(dir.path(), &LocalScanOptions::default()).unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if !enforced {
            return;
        }
        let secret_failure = failure(&tree, "secret.txt");
        assert!(!secret_failure.folder);
        assert_eq!(secret_failure.meta.as_ref().map(|meta| meta.size), Some(1));
        assert!(
            secret_failure.error.contains("cannot open"),
            "{}",
            secret_failure.error
        );
        let locked_failure = failure(&tree, "locked");
        assert!(locked_failure.folder);
        assert!(!tree.entries.contains_key("locked/inner.txt"));
        assert!(tree.entries.contains_key("open.txt"));
        // Without the read check only the folder fails; the file's metadata is readable.
        assert!(matches!(
            without_check.entries.get("secret.txt"),
            Some(LocalEntry::File(_))
        ));
        assert!(failure(&without_check, "locked").folder);
    }

    #[cfg(unix)]
    #[test]
    fn links_that_cannot_be_examined_shadow_like_folders() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let hidden = dir.path().join("hidden");
        write(&hidden, "inside/file.txt", "x");
        fs::create_dir_all(&root).unwrap();
        symlink(hidden.join("inside"), root.join("link")).unwrap();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        let enforced = fs::metadata(hidden.join("inside")).is_err();
        let tree = scan(&root);
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o755)).unwrap();
        if !enforced {
            return;
        }
        let link = failure(&tree, "link");
        assert!(link.folder, "{link:?}");
        assert!(link.error.contains("cannot follow"), "{}", link.error);
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_reported_and_never_opened() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let _listener = UnixListener::bind(dir.path().join("sock")).unwrap();
        // Opening a FIFO for reading blocks until a writer appears, so this test would
        // hang if the readability check opened anything but regular files.
        let fifo = dir.path().join("fifo");
        let have_fifo = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|status| status.success());
        if have_fifo {
            symlink(&fifo, dir.path().join("fifo-link")).unwrap();
        }
        let tree = scan(dir.path());
        assert!(failure(&tree, "sock").error.contains("not a regular file"));
        if have_fifo {
            assert!(failure(&tree, "fifo").error.contains("not a regular file"));
            assert!(
                failure(&tree, "fifo-link")
                    .error
                    .contains("not a regular file")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_are_escaped() {
        use std::os::unix::ffi::OsStrExt;

        let name = OsStr::from_bytes(b"bad\xFF\xFEname.txt");
        assert_eq!(escape_name(name), "bad\\xFF\\xFEname.txt");
        assert_eq!(escape_name(OsStr::new("ok")), "ok");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_files_on_disk_are_reported_when_the_file_system_allows_them() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sub/fine.txt", "f");
        let bad_file = dir
            .path()
            .join("sub")
            .join(OsStr::from_bytes(b"bad\xFF.txt"));
        let bad_folder = dir.path().join(OsStr::from_bytes(b"dir\xFE"));
        // APFS (macOS) refuses names that are not UTF-8; Linux file systems accept them.
        if fs::write(&bad_file, "x").is_err() || fs::create_dir(&bad_folder).is_err() {
            return;
        }
        fs::write(bad_folder.join("inner.txt"), "y").unwrap();
        let tree = scan(dir.path());
        let unmapped: Vec<(&str, &str)> = tree
            .unmapped
            .iter()
            .map(|entry| (entry.display.as_str(), entry.error.as_str()))
            .collect();
        assert_eq!(unmapped.len(), 2);
        assert_eq!(unmapped[0].0, "dir\\xFE");
        assert!(unmapped[0].1.contains("folder"));
        assert_eq!(unmapped[1].0, "sub/bad\\xFF.txt");
        let files: Vec<&str> = tree.files().map(|(path, _)| path).collect();
        assert_eq!(files, vec!["sub/fine.txt"]);
        assert_eq!(tree.problem_count(), 2);
    }

    #[test]
    fn system_time_conversion_never_panics() {
        assert_eq!(
            system_time_to_utc(UNIX_EPOCH + Duration::new(1, 5)),
            DateTime::from_timestamp(1, 5)
        );
        assert_eq!(
            system_time_to_utc(UNIX_EPOCH - Duration::new(1, 0)),
            DateTime::from_timestamp(-1, 0)
        );
        assert_eq!(
            system_time_to_utc(UNIX_EPOCH - Duration::new(0, 1)),
            DateTime::from_timestamp(-1, 999_999_999)
        );
        if let Some(far) = UNIX_EPOCH.checked_add(Duration::from_secs(u64::MAX / 4)) {
            assert_eq!(system_time_to_utc(far), None);
        }
    }

    #[test]
    fn remote_scan_strips_prefix_and_skips_folders() {
        let root = Path::new("/sync/root");
        let mut folder = object("photos/2024/");
        folder.is_prefix = true;
        let tree = scan_remote(
            [
                object("photos/a.jpg"),
                object("photos/2024/b.jpg"),
                object("photos/"),
                object("photos/2024/"),
                folder,
                object("photosX/c.jpg"),
                object("other/d.jpg"),
            ],
            "photos/",
            root,
            None,
        )
        .unwrap();
        let keys: Vec<&str> = tree.objects.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["2024/b.jpg", "a.jpg"]);
        assert!(tree.objects.values().all(|object| object.problem.is_none()));
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn remote_scan_requires_a_folder_prefix() {
        let err =
            scan_remote([object("photos/a.jpg")], "photos", Path::new("/r"), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let whole_bucket =
            scan_remote([object("a"), object("b/c")], "", Path::new("/r"), None).unwrap();
        assert_eq!(whole_bucket.len(), 2);
    }

    #[test]
    fn unsafe_and_reserved_keys_are_flagged() {
        let tree = scan_remote(
            [
                object("p/../escape.txt"),
                object("p/a//b.txt"),
                object("p/./c.txt"),
                object("p/back\\slash.txt"),
                object("p//abs.txt"),
                object("p/C:/drive.txt"),
                object("p/file.bbpart"),
                object("p/fine.txt"),
            ],
            "p/",
            Path::new("/root"),
            None,
        )
        .unwrap();
        let problem = |rel: &str| tree.objects.get(rel).unwrap().problem.clone();
        for rel in [
            "../escape.txt",
            "a//b.txt",
            "./c.txt",
            "back\\slash.txt",
            "/abs.txt",
            "C:/drive.txt",
        ] {
            assert!(
                matches!(problem(rel), Some(RemoteProblem::NotLocal(_))),
                "{rel}: {:?}",
                problem(rel)
            );
        }
        assert_eq!(problem("file.bbpart"), Some(RemoteProblem::Reserved));
        assert_eq!(problem("fine.txt"), None);
    }

    #[test]
    fn remote_scan_applies_path_filter_and_last_duplicate_wins() {
        let filter = Filter::compile(&FilterSpec {
            glob: Some("*.txt".to_owned()),
            ..FilterSpec::default()
        })
        .unwrap();
        let mut newer = object("p/a.txt");
        newer.size = 9;
        let tree = scan_remote(
            [
                object("p/a.txt"),
                newer,
                object("p/b.jpg"),
                object("p/sub/c.txt"),
            ],
            "p/",
            Path::new("/root"),
            Some(&filter),
        )
        .unwrap();
        let keys: Vec<&str> = tree.objects.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["a.txt"]);
        assert_eq!(tree.objects["a.txt"].entry.size, 9);
    }

    #[test]
    fn case_probe_matches_the_real_file_system() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Probe.txt", "x");
        let truth = fs::metadata(dir.path().join("pROBE.TXT")).is_ok();
        let tree = scan(dir.path());
        assert_eq!(is_case_insensitive(dir.path(), &tree), truth);
        // Nothing to probe with: assume case-insensitive.
        let empty = tempfile::tempdir().unwrap();
        assert!(is_case_insensitive(empty.path(), &LocalTree::default()));
    }

    #[test]
    fn case_probe_uses_names_in_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "123/Name.txt", "x");
        let truth = fs::metadata(dir.path().join("123/nAME.TXT")).is_ok();
        let tree = scan(dir.path());
        assert_eq!(is_case_insensitive(dir.path(), &tree), truth);
    }
}
