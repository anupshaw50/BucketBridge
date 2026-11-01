//! Turning a selection into work items, one per object, following the
//! folder semantics of the transfer commands (`docs/desktop-api.md`): a key
//! ending in `/` is a prefix and expands recursively, and the parent part of
//! every selected key is stripped, so the selected name lands directly under
//! the destination. Local folders upload with their relative paths and
//! empty folders become folder markers.
//!
//! Two selected entries that would land on the same destination (the same
//! name in the destination folder; for downloads compared the way the local
//! file system compares names, without regard to case on Windows and macOS)
//! reject the whole request with [`Error::Conflict`] and produce no items,
//! so a selection can never overwrite itself. Selecting the same source
//! twice counts once.
//!
//! Problems with single entries (an unreadable file, a key that would
//! escape the download folder, two objects of one selected prefix that
//! cannot coexist as local files, an object that holds data under a key
//! ending in `/`, an object whose name is reserved for the partial files of
//! downloads) are reported in [`Expansion::failures`] while everything else
//! is still expanded. A selected prefix whose listing fails is reported as
//! one failure, without partial items, so a folder is never transferred
//! half.
//!
//! Names ending in `.bbpart` (in any letter case) are reserved for the
//! partial files of downloads ([`is_partial_name`]): an object with such a
//! name is a failure for a download, and local files with such names are
//! left out of an upload, as the sync planner does. That keeps a partial
//! file from ever being the file of another object.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, Metadata};
use std::io;
use std::ops::Bound;
use std::path::{Path, PathBuf};

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};

use super::download::{data_under_folder_key, is_partial_name};
use super::{PARTIAL_SUFFIX, TransferEngine, blocking};
use crate::error::{Error, ErrorKind, Result};
use crate::model::ObjectEntry;
use crate::path::{join_key, safe_local_path, strip_parent};
use crate::s3uri::{validate_bucket_name, validate_key};

/// Whether local file names are compared without regard to case: the
/// default file systems of Windows and macOS ignore case.
const LOCAL_NAMES_IGNORE_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// Work items and the entries that could not be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion<T> {
    /// One item per object (or folder marker).
    pub items: Vec<T>,
    /// Entries that were left out, with the reason.
    pub failures: Vec<ExpandFailure>,
}

impl<T> Default for Expansion<T> {
    fn default() -> Self {
        Expansion {
            items: Vec::new(),
            failures: Vec::new(),
        }
    }
}

/// An entry that could not be added (a "folder-add" failure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandFailure {
    /// The local path or the key concerned.
    pub entry: String,
    /// Error category.
    pub code: ErrorKind,
    /// What went wrong.
    pub message: String,
}

impl ExpandFailure {
    fn new(entry: impl Into<String>, code: ErrorKind, message: impl Into<String>) -> Self {
        ExpandFailure {
            entry: entry.into(),
            code,
            message: message.into(),
        }
    }

    fn from_error(entry: impl Into<String>, err: &Error) -> Self {
        ExpandFailure::new(entry, err.kind(), err.to_string())
    }
}

/// One upload: a local file, or an empty local folder that becomes a folder
/// marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadItem {
    /// The local file or folder.
    pub path: PathBuf,
    /// Destination key; ends with `/` for a folder marker.
    pub key: String,
    /// Size in bytes when expanded; 0 for folder markers.
    pub size: u64,
    /// True for a folder marker.
    pub is_folder: bool,
}

/// One download: an object to a file, or a folder marker (or selected
/// prefix) to a local folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    /// Source key; ends with `/` for a folder.
    pub key: String,
    /// Local file or folder, always inside the destination folder.
    pub path: PathBuf,
    /// Object size from the listing; `None` for selected single keys and
    /// for folders.
    pub size: Option<u64>,
    /// True for a folder (nothing to download, the folder is created).
    pub is_folder: bool,
}

/// One server-side copy or move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyItem {
    /// Source key.
    pub src_key: String,
    /// Destination key.
    pub dst_key: String,
    /// Object size from the listing; `None` for selected single keys.
    pub size: Option<u64>,
}

/// One delete: a key, or a prefix (ending in `/`) deleted as one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteItem {
    /// The key or prefix.
    pub key: String,
}

/// The destination names taken by the selected entries of one request.
struct Destinations {
    ignore_case: bool,
    /// What happens to the entries, such as `"copied"`.
    verb: &'static str,
    /// Where they go, such as `"into s3://bucket/backup/"`.
    place: String,
    /// Compared name to the entry that took it.
    taken: HashMap<String, String>,
}

impl Destinations {
    fn new(ignore_case: bool, verb: &'static str, place: String) -> Self {
        Destinations {
            ignore_case,
            verb,
            place,
            taken: HashMap::new(),
        }
    }

    /// Takes `name` for `entry`; fails with [`Error::Conflict`] when another
    /// selected entry already took it.
    fn claim(&mut self, entry: &str, name: &str) -> Result<()> {
        let compared = if self.ignore_case {
            name.to_lowercase()
        } else {
            name.to_owned()
        };
        if let Some(first) = self.taken.get(&compared) {
            return Err(Error::Conflict(format!(
                "'{first}' and '{entry}' would both be {} {} as '{name}'; nothing was queued",
                self.verb, self.place
            )));
        }
        self.taken.insert(compared, entry.to_owned());
        Ok(())
    }
}

/// The name an entry gets in the destination folder: the first segment of
/// its key relative to the stripped parent part.
fn destination_name(rel: &str) -> &str {
    rel.split('/').next().unwrap_or(rel)
}

/// Expands local files and folders for an upload under `prefix` (async
/// wrapper of [`expand_upload_blocking`]).
pub async fn expand_upload(paths: Vec<PathBuf>, prefix: String) -> Result<Expansion<UploadItem>> {
    blocking(move || expand_upload_blocking(&paths, &prefix)).await?
}

/// Expands local files and folders for an upload under `prefix` (blocking:
/// it walks the file system).
///
/// A selected file becomes `prefix + name`; a selected folder uploads its
/// content with relative paths kept (`prefix + folder/sub/file`), and every
/// folder without uploadable content becomes a folder marker. Symbolic
/// links to files are followed; links to folders are not (they are reported
/// as failures). Entries that cannot be read, names that are not valid
/// UTF-8 and keys longer than S3 allows are reported as failures. Files
/// whose names end in `.bbpart` are the temporary files of downloads: inside
/// a selected folder they are left out, and a selected file like that is a
/// failure.
///
/// Two selected paths with the same name (from different folders) would
/// upload to the same keys: the request fails with [`Error::Conflict`]
/// before anything is walked.
pub fn expand_upload_blocking(paths: &[PathBuf], prefix: &str) -> Result<Expansion<UploadItem>> {
    let place = if prefix.is_empty() {
        "to the top of the bucket".to_owned()
    } else {
        format!("under '{prefix}'")
    };
    let mut names = Destinations::new(false, "uploaded", place);
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut selected = Vec::with_capacity(paths.len());
    for path in paths {
        if !seen.insert(path.as_path()) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            names.claim(&path.display().to_string(), name)?;
        }
        selected.push(path.as_path());
    }
    let mut walk = UploadWalk {
        prefix,
        keys: HashSet::new(),
        out: Expansion::default(),
        clash: None,
    };
    for path in selected {
        walk.add_selected(path);
    }
    match walk.clash {
        Some(err) => Err(err),
        None => Ok(walk.out),
    }
}

struct UploadWalk<'a> {
    prefix: &'a str,
    keys: HashSet<String>,
    out: Expansion<UploadItem>,
    /// A key produced twice; the selected names are unique, so this only
    /// guards the invariant.
    clash: Option<Error>,
}

impl UploadWalk<'_> {
    fn fail(&mut self, path: &Path, code: ErrorKind, message: impl Into<String>) {
        self.out.failures.push(ExpandFailure::new(
            path.display().to_string(),
            code,
            message,
        ));
    }

    fn fail_io(&mut self, path: &Path, doing: &str, err: &io::Error) {
        let message = format!("{doing} '{}': {err}", path.display());
        self.fail(path, ErrorKind::Io, message);
    }

    fn add_selected(&mut self, path: &Path) {
        let Some(name) = path.file_name() else {
            self.fail(path, ErrorKind::Usage, "it does not name a file or folder");
            return;
        };
        let Some(name) = name.to_str() else {
            self.fail(
                path,
                ErrorKind::Usage,
                "the name is not valid UTF-8 and cannot be used in an S3 key",
            );
            return;
        };
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) => return self.fail_io(path, "reading", &err),
        };
        let key = join_key(self.prefix, name);
        if metadata.is_dir() {
            self.walk(path.to_path_buf(), key);
        } else if is_partial_name(name) {
            self.fail(
                path,
                ErrorKind::Usage,
                format!(
                    "names ending in '{PARTIAL_SUFFIX}' are reserved for the temporary files of downloads and are not uploaded"
                ),
            );
        } else {
            self.add_entry(path, key, &metadata);
        }
    }

    /// Adds a file or a symbolic link (anything but a real folder); returns
    /// whether an item was added.
    fn add_entry(&mut self, path: &Path, key: String, metadata: &Metadata) -> bool {
        if metadata.file_type().is_symlink() {
            return match fs::metadata(path) {
                Ok(target) if target.is_file() => self.add_file(path, key, target.len()),
                Ok(target) if target.is_dir() => {
                    self.fail(
                        path,
                        ErrorKind::Usage,
                        "it is a symbolic link to a folder; links to folders are not followed",
                    );
                    false
                }
                Ok(_) => {
                    self.fail(
                        path,
                        ErrorKind::Usage,
                        "it links to something that is neither a file nor a folder",
                    );
                    false
                }
                Err(err) => {
                    self.fail_io(path, "following the symbolic link", &err);
                    false
                }
            };
        }
        if metadata.is_file() {
            return self.add_file(path, key, metadata.len());
        }
        self.fail(
            path,
            ErrorKind::Usage,
            "it is neither a file nor a folder (a device, socket or pipe)",
        );
        false
    }

    fn add_file(&mut self, path: &Path, key: String, size: u64) -> bool {
        if let Err(err) = fs::File::open(path) {
            self.fail_io(path, "opening", &err);
            return false;
        }
        self.push(path, key, size, false)
    }

    fn push(&mut self, path: &Path, key: String, size: u64, is_folder: bool) -> bool {
        if let Err(err) = validate_key(&key) {
            self.fail(
                path,
                err.kind(),
                format!("the key '{key}' is unusable: {err}"),
            );
            return false;
        }
        if !self.keys.insert(key.clone()) {
            self.clash.get_or_insert_with(|| {
                Error::Conflict(format!(
                    "two selected entries would both be uploaded to '{key}'; nothing was queued"
                ))
            });
            return false;
        }
        self.out.items.push(UploadItem {
            path: path.to_path_buf(),
            key,
            size,
            is_folder,
        });
        true
    }

    /// Walks a folder depth-first, entries sorted by name.
    fn walk(&mut self, root: PathBuf, root_key: String) {
        let mut pending = vec![(root, root_key)];
        while let Some((dir, dir_key)) = pending.pop() {
            let reader = match fs::read_dir(&dir) {
                Ok(reader) => reader,
                Err(err) => {
                    self.fail_io(&dir, "listing the folder", &err);
                    continue;
                }
            };
            let mut children = Vec::new();
            for entry in reader {
                match entry.and_then(|e| e.metadata().map(|m| (e.file_name(), e.path(), m))) {
                    Ok(child) => children.push(child),
                    Err(err) => self.fail_io(&dir, "listing the folder", &err),
                }
            }
            children.sort_by(|a, b| a.0.cmp(&b.0));
            let mut produced = false;
            let mut folders = Vec::new();
            for (name, path, metadata) in children {
                let Some(name) = name.to_str() else {
                    self.fail(
                        &path,
                        ErrorKind::Usage,
                        "the name is not valid UTF-8 and cannot be used in an S3 key",
                    );
                    continue;
                };
                let key = format!("{dir_key}/{name}");
                if metadata.is_dir() {
                    folders.push((path, key));
                    produced = true;
                } else if is_partial_name(name) {
                    // A temporary file of a download (a leftover of an
                    // interrupted one, usually): never uploaded.
                    continue;
                } else if self.add_entry(&path, key, &metadata) {
                    produced = true;
                }
            }
            if !produced {
                self.push(&dir, format!("{dir_key}/"), 0, true);
            }
            pending.extend(folders.into_iter().rev());
        }
    }
}

/// Expands keys for a delete: one item per key; a prefix is one item.
/// Empty keys and prefixes made only of `/` (which would match the whole
/// bucket) are reported as failures; repeated keys are dropped.
pub fn expand_delete(bucket: &str, keys: &[String]) -> Result<Expansion<DeleteItem>> {
    validate_bucket_name(bucket)?;
    let mut out = Expansion::default();
    let mut seen = HashSet::new();
    for key in keys {
        if key.trim_end_matches('/').is_empty() {
            out.failures.push(ExpandFailure::new(
                key.clone(),
                ErrorKind::Usage,
                "the key is empty or names the whole bucket",
            ));
            continue;
        }
        if let Err(err) = validate_key(key) {
            out.failures
                .push(ExpandFailure::from_error(key.clone(), &err));
            continue;
        }
        if seen.insert(key.clone()) {
            out.items.push(DeleteItem { key: key.clone() });
        }
    }
    Ok(out)
}

/// Local paths claimed by the objects of one download, compared the way the
/// local file system compares names (see [`LOCAL_NAMES_IGNORE_CASE`]).
#[derive(Default)]
struct LocalNames {
    /// Compared relative path to "is a folder".
    claimed: HashMap<String, bool>,
}

impl LocalNames {
    /// Claims `rel` (a `/`-separated path relative to the destination
    /// folder) as a file or a folder; fails when it clashes with an earlier
    /// claim.
    fn claim(&mut self, rel: &str, is_folder: bool) -> std::result::Result<(), String> {
        let rel = rel.trim_end_matches('/');
        let compared = if LOCAL_NAMES_IGNORE_CASE {
            rel.to_lowercase()
        } else {
            rel.to_owned()
        };
        let ancestors: Vec<&str> = compared
            .match_indices('/')
            .map(|(at, _)| &compared[..at])
            .collect();
        if let Some(file) = ancestors
            .iter()
            .find(|ancestor| self.claimed.get(**ancestor) == Some(&false))
        {
            return Err(format!(
                "it needs a folder where another object is downloaded to the file '{file}'"
            ));
        }
        match self.claimed.get(&compared) {
            Some(true) if is_folder => {}
            Some(true) => {
                return Err("a folder of the same name is downloaded too".to_owned());
            }
            Some(false) => {
                return Err("another object is downloaded to the same file".to_owned());
            }
            None => {}
        }
        for ancestor in ancestors {
            self.claimed.insert(ancestor.to_owned(), true);
        }
        self.claimed.insert(compared, is_folder);
        Ok(())
    }
}

impl TransferEngine {
    /// Expands keys and prefixes of `bucket` for a download into `dest_dir`.
    ///
    /// `photos/2024/a.jpg` becomes `dest_dir/a.jpg`; the prefix
    /// `photos/2024/` becomes the folder `dest_dir/2024` and everything
    /// below it. Every path is checked with [`safe_local_path`]: keys that
    /// could escape `dest_dir` or cannot be represented on this OS are
    /// reported as failures, so nothing is ever written outside `dest_dir`.
    ///
    /// Two selected entries with the same name (without regard to case on
    /// Windows and macOS; a file and a folder count too) fail the request
    /// with [`Error::Conflict`] before anything is listed. Objects of one
    /// selected prefix that cannot coexist as local files (keys that differ
    /// only in case there, or `a` next to `a/b`) are per-entry failures.
    ///
    /// Two more kinds of objects are per-entry failures, because there is no
    /// way to store them: an object with a name ending in `.bbpart` (in any
    /// letter case; the name is reserved for the partial files of
    /// downloads, so a partial file can never be the file of another
    /// object), and an object that holds data under a key ending in `/`
    /// (which names a folder).
    pub async fn expand_download(
        &self,
        bucket: &str,
        keys: &[String],
        dest_dir: &Path,
    ) -> Result<Expansion<DownloadItem>> {
        validate_bucket_name(bucket)?;
        let mut out = Expansion::default();
        let mut names = Destinations::new(
            LOCAL_NAMES_IGNORE_CASE,
            "downloaded",
            format!("into '{}'", dest_dir.display()),
        );
        let mut seen = HashSet::new();
        let mut planned = Vec::with_capacity(keys.len());
        for selected in keys {
            if !seen.insert(selected.as_str()) {
                continue;
            }
            if selected.is_empty() {
                out.failures
                    .push(ExpandFailure::new("", ErrorKind::Usage, "the key is empty"));
                continue;
            }
            let rel = match strip_parent(selected, selected) {
                Ok(rel) => rel,
                Err(err) => {
                    out.failures
                        .push(ExpandFailure::from_error(selected.clone(), &err));
                    continue;
                }
            };
            if let Err(err) = safe_local_path(dest_dir, rel) {
                out.failures
                    .push(ExpandFailure::from_error(selected.clone(), &err));
                continue;
            }
            if !selected.ends_with('/') && is_partial_name(rel) {
                // Fails like any entry that cannot be stored; it takes no
                // destination name.
                out.failures.push(reserved_name_failure(selected));
                continue;
            }
            names.claim(selected, destination_name(rel))?;
            planned.push((selected, rel));
        }

        let mut local = LocalNames::default();
        for (selected, rel) in planned {
            if !selected.ends_with('/') {
                add_download(&mut out, &mut local, dest_dir, selected, rel, None);
                continue;
            }
            let entries = match self.list_prefix(bucket, selected).await {
                Ok(entries) => entries,
                Err(err) => {
                    out.failures
                        .push(ExpandFailure::from_error(selected.clone(), &err));
                    continue;
                }
            };
            // The selected folder itself (unless its own object holds data),
            // then everything below it.
            let marker_size = entries
                .iter()
                .find(|entry| entry.key == *selected)
                .map(|entry| entry.size);
            match marker_size.filter(|size| *size > 0) {
                Some(size) => out.failures.push(ExpandFailure::new(
                    selected.clone(),
                    ErrorKind::Usage,
                    data_under_folder_key(format!("'{selected}'"), size),
                )),
                None => add_download(&mut out, &mut local, dest_dir, selected, rel, None),
            }
            for entry in entries {
                if entry.key == *selected {
                    continue;
                }
                match strip_parent(selected, &entry.key) {
                    Ok(rel) => add_download(
                        &mut out,
                        &mut local,
                        dest_dir,
                        &entry.key,
                        rel,
                        Some(entry.size),
                    ),
                    Err(err) => out
                        .failures
                        .push(ExpandFailure::from_error(entry.key.clone(), &err)),
                }
            }
        }
        Ok(out)
    }

    /// Expands keys and prefixes of `src_bucket` for a copy to `dst_prefix`
    /// in `dst_bucket`: `photos/2024/` copied to `backup/` gives
    /// `backup/2024/...`.
    ///
    /// Copying an object onto itself is a per-entry failure, as are
    /// destination keys that are too long. Two selected entries with the
    /// same name fail the request with [`Error::Conflict`].
    pub async fn expand_copy(
        &self,
        src_bucket: &str,
        keys: &[String],
        dst_bucket: &str,
        dst_prefix: &str,
    ) -> Result<Expansion<CopyItem>> {
        self.expand_server_side(src_bucket, keys, dst_bucket, dst_prefix, false)
            .await
    }

    /// Like [`TransferEngine::expand_copy`] for a move. Moving a folder into
    /// itself (or onto itself) is a per-entry failure; a selected key inside
    /// another selected folder fails the request with [`Error::Usage`],
    /// since an object can only be moved once.
    pub async fn expand_move(
        &self,
        src_bucket: &str,
        keys: &[String],
        dst_bucket: &str,
        dst_prefix: &str,
    ) -> Result<Expansion<CopyItem>> {
        self.expand_server_side(src_bucket, keys, dst_bucket, dst_prefix, true)
            .await
    }

    /// Plans a rename within `bucket` as moves (the `rename` command).
    ///
    /// An object key is renamed to `to_key`, which must not end with `/`;
    /// it is a [`ErrorKind::Conflict`] when `to_key` exists. A prefix
    /// (ending in `/`) is renamed with everything below it to the prefix
    /// `to_key`, which must end with `/` and must not lie inside the prefix;
    /// it is a conflict when anything exists below `to_key`. There is a
    /// window between this check and the moves in which another client
    /// could create the destination.
    pub async fn plan_rename(
        &self,
        bucket: &str,
        from_key: &str,
        to_key: &str,
    ) -> Result<Vec<CopyItem>> {
        validate_bucket_name(bucket)?;
        validate_key(from_key).map_err(|err| err.with_context("rename source"))?;
        validate_key(to_key).map_err(|err| err.with_context("new name"))?;
        if from_key == to_key {
            return Err(Error::Usage(format!("'{from_key}' already has that name")));
        }
        if !from_key.ends_with('/') {
            if to_key.ends_with('/') {
                return Err(Error::Usage(format!(
                    "the new key of the object '{from_key}' must not end with '/'"
                )));
            }
            let head = self.s3.head_object(bucket, from_key, None).await?;
            match self.s3.head_object(bucket, to_key, None).await {
                Ok(_) => {
                    return Err(Error::Conflict(format!(
                        "s3://{bucket}/{to_key} already exists"
                    )));
                }
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            return Ok(vec![CopyItem {
                src_key: from_key.to_owned(),
                dst_key: to_key.to_owned(),
                size: Some(head.size),
            }]);
        }
        if !to_key.ends_with('/') || to_key.trim_end_matches('/').is_empty() {
            return Err(Error::Usage(format!(
                "the new name of the folder '{from_key}' must be a folder key ending with '/'"
            )));
        }
        if to_key.starts_with(from_key) {
            return Err(Error::Usage(format!(
                "cannot rename the folder '{from_key}' to '{to_key}', which is inside it"
            )));
        }
        let existing = self.s3.list_objects(bucket, to_key, true, None, 1).await?;
        if !existing.entries.is_empty() {
            return Err(Error::Conflict(format!(
                "the folder s3://{bucket}/{to_key} already exists and is not empty"
            )));
        }
        let entries = self.list_prefix(bucket, from_key).await?;
        if entries.is_empty() {
            return Err(Error::NotFound(format!(
                "nothing to rename: s3://{bucket}/{from_key} holds no objects"
            )));
        }
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(rest) = entry.key.strip_prefix(from_key) else {
                return Err(Error::Remote(format!(
                    "listing s3://{bucket}/{from_key} returned the key '{}' outside the folder",
                    entry.key
                )));
            };
            let dst_key = format!("{to_key}{rest}");
            validate_key(&dst_key)
                .map_err(|err| err.with_context(format!("renaming '{}'", entry.key)))?;
            items.push(CopyItem {
                src_key: entry.key,
                dst_key,
                size: Some(entry.size),
            });
        }
        Ok(items)
    }

    async fn expand_server_side(
        &self,
        src_bucket: &str,
        keys: &[String],
        dst_bucket: &str,
        dst_prefix: &str,
        is_move: bool,
    ) -> Result<Expansion<CopyItem>> {
        validate_bucket_name(src_bucket)?;
        validate_bucket_name(dst_bucket)?;
        let (verb, participle) = if is_move {
            ("move", "moved")
        } else {
            ("copy", "copied")
        };
        let same_bucket = src_bucket == dst_bucket;
        let mut out = Expansion::default();
        let mut names = Destinations::new(
            false,
            participle,
            format!("into s3://{dst_bucket}/{dst_prefix}"),
        );
        let mut seen = HashSet::new();
        let mut planned = Vec::with_capacity(keys.len());
        for selected in keys {
            if !seen.insert(selected.as_str()) {
                continue;
            }
            if selected.is_empty() {
                out.failures
                    .push(ExpandFailure::new("", ErrorKind::Usage, "the key is empty"));
                continue;
            }
            let rel = match strip_parent(selected, selected) {
                Ok(rel) => rel,
                Err(err) => {
                    out.failures
                        .push(ExpandFailure::from_error(selected.clone(), &err));
                    continue;
                }
            };
            let target = join_key(dst_prefix, rel);
            if same_bucket && target == *selected {
                out.failures.push(ExpandFailure::new(
                    selected.clone(),
                    ErrorKind::Usage,
                    format!("cannot {verb} '{selected}' onto itself"),
                ));
                continue;
            }
            if is_move
                && same_bucket
                && selected.ends_with('/')
                && target.starts_with(selected.as_str())
            {
                out.failures.push(ExpandFailure::new(
                    selected.clone(),
                    ErrorKind::Usage,
                    format!("cannot move the folder '{selected}' into itself ('{target}')"),
                ));
                continue;
            }
            names.claim(selected, destination_name(rel))?;
            planned.push((selected, target));
        }
        if is_move {
            reject_nested_moves(planned.iter().map(|(selected, _)| selected.as_str()))?;
        }

        let mut destinations = HashSet::new();
        for (selected, target) in planned {
            if !selected.ends_with('/') {
                add_copy(&mut out, &mut destinations, selected, target, None)?;
                continue;
            }
            let entries = match self.list_prefix(src_bucket, selected).await {
                Ok(entries) => entries,
                Err(err) => {
                    out.failures
                        .push(ExpandFailure::from_error(selected.clone(), &err));
                    continue;
                }
            };
            for entry in entries {
                match strip_parent(selected, &entry.key) {
                    Ok(rel) => {
                        let target = join_key(dst_prefix, rel);
                        add_copy(
                            &mut out,
                            &mut destinations,
                            &entry.key,
                            target,
                            Some(entry.size),
                        )?;
                    }
                    Err(err) => out
                        .failures
                        .push(ExpandFailure::from_error(entry.key.clone(), &err)),
                }
            }
        }
        Ok(out)
    }

    /// Every object under a prefix, folder markers included.
    async fn list_prefix(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectEntry>> {
        self.s3.list_all_stream(bucket, prefix).try_collect().await
    }
}

/// Fails when a selected key lies inside another selected folder: one move
/// would delete what the other still has to copy.
fn reject_nested_moves<'a>(selected: impl Iterator<Item = &'a str>) -> Result<()> {
    let sorted: BTreeSet<&str> = selected.collect();
    for folder in sorted.iter().filter(|key| key.ends_with('/')) {
        // Keys inside `folder` sort directly after it.
        let next = sorted
            .range::<&str, _>((Bound::Excluded(*folder), Bound::Unbounded))
            .next();
        if let Some(inner) = next.filter(|inner| inner.starts_with(*folder)) {
            return Err(Error::Usage(format!(
                "'{inner}' is inside the selected folder '{folder}'; an object can only be moved once"
            )));
        }
    }
    Ok(())
}

/// The failure of an object whose name is reserved for partial files.
fn reserved_name_failure(key: &str) -> ExpandFailure {
    ExpandFailure::new(
        key,
        ErrorKind::Usage,
        format!(
            "names ending in '{PARTIAL_SUFFIX}' are reserved for the temporary files of downloads, so '{key}' cannot be downloaded under that name (rename the object to download it)"
        ),
    )
}

fn add_download(
    out: &mut Expansion<DownloadItem>,
    local: &mut LocalNames,
    dest_dir: &Path,
    key: &str,
    rel: &str,
    size: Option<u64>,
) {
    let is_folder = key.ends_with('/');
    let path = match safe_local_path(dest_dir, rel) {
        Ok(path) => path,
        Err(err) => {
            out.failures.push(ExpandFailure::from_error(key, &err));
            return;
        }
    };
    if is_folder && let Some(size) = size.filter(|size| *size > 0) {
        // Not a folder marker: making the folder would drop its data.
        out.failures.push(ExpandFailure::new(
            key,
            ErrorKind::Usage,
            data_under_folder_key(format!("'{key}'"), size),
        ));
        return;
    }
    if !is_folder && is_partial_name(rel) {
        out.failures.push(reserved_name_failure(key));
        return;
    }
    if let Err(why) = local.claim(rel, is_folder) {
        out.failures.push(ExpandFailure::new(
            key,
            ErrorKind::Conflict,
            format!("'{}': {why}", path.display()),
        ));
        return;
    }
    out.items.push(DownloadItem {
        key: key.to_owned(),
        path,
        size: if is_folder { None } else { size },
        is_folder,
    });
}

/// Adds one copy; a destination key used twice fails the request (the
/// selected names are unique, so this only guards the invariant).
fn add_copy(
    out: &mut Expansion<CopyItem>,
    destinations: &mut HashSet<String>,
    src_key: &str,
    dst_key: String,
    size: Option<u64>,
) -> Result<()> {
    if let Err(err) = validate_key(&dst_key) {
        out.failures.push(ExpandFailure::new(
            src_key,
            err.kind(),
            format!("the destination key '{dst_key}' is unusable: {err}"),
        ));
        return Ok(());
    }
    if !destinations.insert(dst_key.clone()) {
        return Err(Error::Conflict(format!(
            "two selected objects would both be copied to '{dst_key}'; nothing was queued"
        )));
    }
    out.items.push(CopyItem {
        src_key: src_key.to_owned(),
        dst_key,
        size,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(items: &[UploadItem]) -> Vec<&str> {
        items.iter().map(|i| i.key.as_str()).collect()
    }

    #[test]
    fn folders_keep_relative_paths_and_empty_folders_become_markers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("photos");
        fs::create_dir_all(root.join("2024").join("trip one")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::create_dir_all(root.join("nested").join("deeper").join("empty too")).unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("2024").join("b c.jpg"), "bc").unwrap();
        fs::write(
            root.join("2024").join("trip one").join("ü ファイル.txt"),
            "u",
        )
        .unwrap();
        let single = dir.path().join("single.bin");
        fs::write(&single, "12345").unwrap();

        let out = expand_upload_blocking(&[root.clone(), single.clone()], "backup/").unwrap();
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(
            keys(&out.items),
            [
                "backup/photos/a.txt",
                "backup/photos/2024/b c.jpg",
                "backup/photos/2024/trip one/ü ファイル.txt",
                "backup/photos/empty/",
                "backup/photos/nested/deeper/empty too/",
                "backup/single.bin",
            ]
        );
        let single_item = out.items.last().unwrap();
        assert_eq!(single_item.size, 5);
        assert_eq!(single_item.path, single);
        assert!(!single_item.is_folder);
        let marker = &out.items[3];
        assert!(marker.is_folder);
        assert_eq!(marker.size, 0);
        assert_eq!(marker.path, root.join("empty"));

        // An empty selected folder is one marker; no prefix works too.
        let lonely = dir.path().join("lonely");
        fs::create_dir(&lonely).unwrap();
        let out = expand_upload_blocking(&[lonely], "").unwrap();
        assert_eq!(keys(&out.items), ["lonely/"]);
        assert!(expand_upload_blocking(&[], "x/").unwrap().items.is_empty());
    }

    #[test]
    fn missing_entries_are_failures() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("here.txt"), "1").unwrap();
        let out = expand_upload_blocking(
            &[
                dir.path().join("here.txt"),
                dir.path().join("missing.txt"),
                PathBuf::from("/"),
            ],
            "p",
        )
        .unwrap();
        assert_eq!(keys(&out.items), ["p/here.txt"]);
        let codes: Vec<ErrorKind> = out.failures.iter().map(|f| f.code).collect();
        assert_eq!(codes, [ErrorKind::Io, ErrorKind::Usage]);
        assert!(out.failures[0].message.contains("missing.txt"));

        let long = "k".repeat(1100);
        let out = expand_upload_blocking(&[dir.path().join("here.txt")], &long).unwrap();
        assert!(out.items.is_empty());
        assert_eq!(out.failures[0].code, ErrorKind::Usage);
    }

    #[test]
    fn same_names_reject_the_whole_upload() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["one", "two", "three"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        fs::write(dir.path().join("one").join("report.txt"), "1").unwrap();
        fs::write(dir.path().join("two").join("report.txt"), "2").unwrap();
        fs::create_dir_all(dir.path().join("three").join("report.txt")).unwrap();
        let one = dir.path().join("one").join("report.txt");
        let two = dir.path().join("two").join("report.txt");
        let folder = dir.path().join("three").join("report.txt");

        let err = expand_upload_blocking(&[one.clone(), two.clone()], "backup/").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
        let text = err.to_string();
        assert!(text.contains("one") && text.contains("two"), "{text}");
        assert!(
            text.contains("'report.txt'") && text.contains("backup/"),
            "{text}"
        );

        // A folder and a file of the same name collide too.
        let err = expand_upload_blocking(&[one.clone(), folder], "").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);

        // Names differing in case are different S3 keys.
        let upper = dir.path().join("two").join("REPORT.txt");
        fs::write(&upper, "3").unwrap();
        let out = expand_upload_blocking(&[one.clone(), upper], "").unwrap();
        assert_eq!(out.items.len(), 2);

        // The same path twice is one entry.
        let out = expand_upload_blocking(&[one.clone(), one.clone()], "").unwrap();
        assert_eq!(keys(&out.items), ["report.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_to_folders_are_not_followed_and_broken_links_fail() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("secret.txt"), "s").unwrap();
        fs::write(root.join("real.txt"), "r").unwrap();
        symlink(&elsewhere, root.join("folder-link")).unwrap();
        symlink(elsewhere.join("secret.txt"), root.join("file-link.txt")).unwrap();
        symlink(dir.path().join("nowhere"), root.join("broken")).unwrap();

        let out = expand_upload_blocking(std::slice::from_ref(&root), "").unwrap();
        assert_eq!(keys(&out.items), ["root/file-link.txt", "root/real.txt"]);
        assert_eq!(out.items[0].size, 1);
        let failed: Vec<&str> = out.failures.iter().map(|f| f.entry.as_str()).collect();
        assert_eq!(failed.len(), 2, "{:?}", out.failures);
        assert!(failed.iter().any(|f| f.ends_with("broken")));
        assert!(
            out.failures
                .iter()
                .any(|f| f.entry.ends_with("folder-link") && f.message.contains("not followed"))
        );

        // Selecting a link to a folder directly is refused as well.
        let out = expand_upload_blocking(&[root.join("folder-link")], "").unwrap();
        assert!(out.items.is_empty());
        assert_eq!(out.failures.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_entries_are_failures() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let locked_dir = root.join("locked-dir");
        fs::create_dir_all(&locked_dir).unwrap();
        fs::write(locked_dir.join("inside.txt"), "i").unwrap();
        let locked_file = root.join("locked.txt");
        fs::write(&locked_file, "l").unwrap();
        fs::write(root.join("ok.txt"), "o").unwrap();
        fs::set_permissions(&locked_file, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o000)).unwrap();
        // Superusers can read anything; nothing to check then.
        let enforced = fs::File::open(&locked_file).is_err();
        let out = expand_upload_blocking(std::slice::from_ref(&root), "");
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&locked_file, fs::Permissions::from_mode(0o644)).unwrap();
        let out = out.unwrap();
        if !enforced {
            return;
        }
        assert_eq!(keys(&out.items), ["root/ok.txt"]);
        assert_eq!(out.failures.len(), 2, "{:?}", out.failures);
        assert!(out.failures.iter().all(|f| f.code == ErrorKind::Io));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn names_that_are_not_utf8_are_failures() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(OsStr::from_bytes(b"bad\xffname")), "x").unwrap();
        fs::write(root.join("good"), "x").unwrap();
        let out = expand_upload_blocking(std::slice::from_ref(&root), "").unwrap();
        assert_eq!(keys(&out.items), ["root/good"]);
        assert_eq!(out.failures.len(), 1);
        assert!(out.failures[0].message.contains("UTF-8"));
    }

    #[test]
    fn delete_expansion() {
        let keys: Vec<String> = ["a.txt", "logs/", "a.txt", "", "/", "//", "x/y"]
            .iter()
            .map(|k| (*k).to_owned())
            .collect();
        let out = expand_delete("bucket", &keys).unwrap();
        let items: Vec<&str> = out.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(items, ["a.txt", "logs/", "x/y"]);
        assert_eq!(out.failures.len(), 3);
        assert!(expand_delete("Bad Bucket", &keys).is_err());
    }

    #[test]
    fn local_names_detect_clashes() {
        let mut names = LocalNames::default();
        names.claim("2024/", true).unwrap();
        names.claim("2024/a.jpg", false).unwrap();
        names.claim("2024/sub/b.jpg", false).unwrap();
        names.claim("2024/sub/", true).unwrap();
        assert!(names.claim("2024/a.jpg", false).is_err());
        assert!(names.claim("2024/a.jpg/x", false).is_err());
        assert!(names.claim("2024/sub", false).is_err());
        assert!(names.claim("2024", false).is_err());
        let case_clash = names.claim("2024/A.JPG", false);
        assert_eq!(
            case_clash.is_err(),
            LOCAL_NAMES_IGNORE_CASE,
            "{case_clash:?}"
        );
    }

    #[test]
    fn destination_names_and_nested_moves() {
        assert_eq!(destination_name("2024/"), "2024");
        assert_eq!(destination_name("a.jpg"), "a.jpg");
        assert_eq!(destination_name("2024/b/c"), "2024");

        let mut names = Destinations::new(true, "copied", "into x".into());
        names.claim("a/Report.txt", "Report.txt").unwrap();
        let err = names.claim("b/report.txt", "report.txt").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
        assert!(
            err.to_string()
                .contains("'a/Report.txt' and 'b/report.txt'"),
            "{err}"
        );
        let mut exact = Destinations::new(false, "copied", "into x".into());
        exact.claim("a/Report.txt", "Report.txt").unwrap();
        exact.claim("b/report.txt", "report.txt").unwrap();

        reject_nested_moves(["photos/", "photos2/x", "photo"].into_iter()).unwrap();
        reject_nested_moves(["a/", "a0", "b/c"].into_iter()).unwrap();
        let err = reject_nested_moves(["photos/2024/a.jpg", "photos/"].into_iter()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("'photos/2024/a.jpg'"), "{err}");
        assert!(reject_nested_moves(["x/", "x/y/"].into_iter()).is_err());
    }
}
