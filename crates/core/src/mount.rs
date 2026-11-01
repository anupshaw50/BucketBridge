//! Read-only mount of a bucket (plan task T04, "Mount as drive").
//!
//! This module is the filesystem-independent half: an inode table that maps
//! inode numbers to S3 keys and prefixes, directory listings through
//! [`S3::list_objects`], and ranged reads through a small block cache. It
//! knows nothing about FUSE and is tested against the in-process S3 server.
//! The adapters (`fuse` on Linux and macOS, `windows` on Windows through
//! Dokan; both compiled only with the `mount` cargo feature) are thin shells
//! that turn kernel requests into calls of [`MountCore`].
//!
//! # Behaviour
//!
//! * Prefixes are directories, objects are files; the mount is read-only.
//! * A directory is listed once and kept for [`MountOptions::dir_ttl`];
//!   `lookup` is answered from that listing, so opening a file in a folder of
//!   thousands of objects costs one listing, not one request per name.
//! * Reads fetch aligned blocks of [`MountOptions::block_size`] bytes with
//!   `Range` requests that carry `If-Match` of the listed ETag, so a file that
//!   changed on the server is never read as a mix of old and new content: the
//!   read fails and the next listing shows the new version.
//! * Blocks are cached in memory, at most [`MountOptions::cache_blocks`] of
//!   them, least recently used first out.
//! * When a prefix holds an object and a folder with the same name, the
//!   folder wins and the object is not reachable. Names that cannot be file
//!   names (empty, `.`, `..`, containing a NUL) are skipped.
//! * Inode numbers are never reused while the mount lives.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aws_sdk_s3::config::ResponseChecksumValidation;
use aws_smithy_types::error::display::DisplayErrorContext;
use bytes::Bytes;
use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::ops::S3;

/// The inode of the mount's root directory.
pub const ROOT_INODE: u64 = 1;

/// Whether this build can mount: the `mount` cargo feature on Linux, macOS or
/// Windows.
pub const fn supported() -> bool {
    cfg!(all(
        feature = "mount",
        any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )
    ))
}

/// Whether the machine has what a mount needs at run time: the FUSE device
/// and `fusermount` on Linux, macFUSE or FUSE-T on macOS, the Dokan driver
/// and library on Windows. `false` in builds without [`supported`].
pub fn fuse_installed() -> bool {
    if !supported() {
        return false;
    }
    installed_here()
}

#[cfg(target_os = "linux")]
fn installed_here() -> bool {
    let device = std::path::Path::new("/dev/fuse").exists();
    let tool = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            dir.join("fusermount3").is_file()
                || dir.join("fusermount").is_file()
                || std::env::var_os("FUSERMOUNT_PATH").is_some()
        })
    });
    device && tool
}

#[cfg(target_os = "macos")]
fn installed_here() -> bool {
    [
        "/Library/Filesystems/macfuse.fs",
        "/Library/Filesystems/fuse-t.fs",
    ]
    .iter()
    .any(|path| std::path::Path::new(path).exists())
}

#[cfg(target_os = "windows")]
fn installed_here() -> bool {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let root = std::path::Path::new(&root);
    root.join("System32").join("dokan2.dll").is_file()
        && root
            .join("System32")
            .join("drivers")
            .join("dokan2.sys")
            .is_file()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn installed_here() -> bool {
    false
}

/// What to install when [`fuse_installed`] is `false`, for error messages.
pub const fn driver_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "Dokan is not installed (install the Dokan 2 runtime from github.com/dokan-dev/dokany/releases)"
    } else if cfg!(target_os = "macos") {
        "FUSE is not installed (install macFUSE or FUSE-T)"
    } else {
        "FUSE is not installed (install fuse3)"
    }
}

/// Drive letters a Windows mount can still use (`C` and up); empty on other
/// platforms and in builds without [`supported`].
pub fn free_drive_letters() -> Vec<char> {
    #[cfg(all(feature = "mount", windows))]
    {
        windows::free_drive_letters()
    }
    #[cfg(not(all(feature = "mount", windows)))]
    {
        Vec::new()
    }
}

/// Tuning of a mount.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Size of the blocks that reads fetch and cache, in bytes.
    pub block_size: u64,
    /// How many blocks stay in memory.
    pub cache_blocks: usize,
    /// How long a directory listing is trusted.
    pub dir_ttl: Duration,
    /// How long the kernel may cache names and attributes.
    pub attr_ttl: Duration,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            block_size: 1024 * 1024,
            cache_blocks: 64,
            dir_ttl: Duration::from_secs(30),
            attr_ttl: Duration::from_secs(5),
        }
    }
}

/// File or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A prefix.
    Dir,
    /// An object.
    File,
}

/// What `getattr` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Inode number.
    pub ino: u64,
    /// File or directory.
    pub kind: NodeKind,
    /// Object size in bytes; 0 for directories.
    pub size: u64,
    /// Last modification time, when S3 reported one.
    pub modified: Option<DateTime<Utc>>,
}

/// One name in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Inode number of the child.
    pub ino: u64,
    /// Name inside the directory (no `/`).
    pub name: String,
    /// File or directory.
    pub kind: NodeKind,
}

#[derive(Debug, Clone)]
struct Node {
    /// Full object key; ends with `/` for directories (empty for a root
    /// without prefix).
    key: String,
    kind: NodeKind,
    parent: u64,
    size: u64,
    etag: Option<String>,
    modified: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct DirCache {
    entries: Vec<DirEntry>,
    fetched: Instant,
}

/// LRU cache of blocks keyed by inode and block number.
#[derive(Debug, Default)]
struct BlockCache {
    blocks: HashMap<(u64, u64), (Bytes, u64)>,
    tick: u64,
}

impl BlockCache {
    fn get(&mut self, ino: u64, block: u64) -> Option<Bytes> {
        self.tick += 1;
        let tick = self.tick;
        let (bytes, used) = self.blocks.get_mut(&(ino, block))?;
        *used = tick;
        Some(bytes.clone())
    }

    fn put(&mut self, ino: u64, block: u64, bytes: Bytes, capacity: usize) {
        self.tick += 1;
        self.blocks.insert((ino, block), (bytes, self.tick));
        while self.blocks.len() > capacity.max(1) {
            let oldest = self
                .blocks
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| *key);
            match oldest {
                Some(key) => {
                    self.blocks.remove(&key);
                }
                None => break,
            }
        }
    }

    fn forget(&mut self, ino: u64) {
        self.blocks.retain(|(owner, _), _| *owner != ino);
    }
}

#[derive(Debug, Default)]
struct State {
    /// Index `n` is inode `n + 1`.
    nodes: Vec<Node>,
    by_key: HashMap<String, u64>,
    dirs: HashMap<u64, DirCache>,
    blocks: BlockCache,
}

impl State {
    fn node(&self, ino: u64) -> Result<&Node> {
        usize::try_from(ino)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| self.nodes.get(n))
            .ok_or_else(|| Error::NotFound(format!("inode {ino} does not exist")))
    }
}

/// The filesystem-independent core of a mount of one bucket (or one prefix of
/// it). Cheap calls take a short lock; S3 requests never run under it.
pub struct MountCore {
    s3: S3,
    bucket: String,
    options: MountOptions,
    state: Mutex<State>,
}

impl MountCore {
    /// A mount of `bucket`, restricted to `prefix` when it is not empty (a
    /// trailing `/` is added when missing). Makes no request.
    pub fn new(s3: S3, bucket: &str, prefix: &str, options: MountOptions) -> Self {
        let mut prefix = prefix.trim_start_matches('/').to_owned();
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        let root = Node {
            key: prefix.clone(),
            kind: NodeKind::Dir,
            parent: ROOT_INODE,
            size: 0,
            etag: None,
            modified: None,
        };
        let mut state = State::default();
        state.by_key.insert(prefix, ROOT_INODE);
        state.nodes.push(root);
        Self {
            s3,
            bucket: bucket.to_owned(),
            options,
            state: Mutex::new(state),
        }
    }

    /// The bucket that is mounted.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The tuning in use.
    pub fn options(&self) -> &MountOptions {
        &self.options
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Attributes of an inode, from what the last listing reported.
    pub fn getattr(&self, ino: u64) -> Result<Attr> {
        let state = self.state();
        let node = state.node(ino)?;
        Ok(attr_of(ino, node))
    }

    /// The entries of a directory, from the cached listing while it is fresh.
    /// `.` and `..` are not part of the result.
    pub async fn readdir(&self, ino: u64) -> Result<Vec<DirEntry>> {
        let key = {
            let state = self.state();
            let node = state.node(ino)?;
            if node.kind != NodeKind::Dir {
                return Err(Error::Usage(format!("inode {ino} is not a directory")));
            }
            if let Some(cache) = state.dirs.get(&ino)
                && cache.fetched.elapsed() < self.options.dir_ttl
            {
                return Ok(cache.entries.clone());
            }
            node.key.clone()
        };
        let mut found: Vec<(String, NodeKind, Option<crate::model::ObjectEntry>)> = Vec::new();
        let mut seen_names: HashSet<String> = HashSet::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .s3
                .list_objects(&self.bucket, &key, false, token.as_deref(), 1000)
                .await?;
            for entry in page.entries {
                let name = entry
                    .key
                    .strip_prefix(key.as_str())
                    .unwrap_or(&entry.key)
                    .trim_end_matches('/')
                    .to_owned();
                if !valid_name(&name) || name.contains('/') {
                    continue;
                }
                if !seen_names.insert(name.clone()) {
                    continue;
                }
                if entry.is_prefix {
                    found.push((name, NodeKind::Dir, None));
                } else {
                    found.push((name, NodeKind::File, Some(entry)));
                }
            }
            token = page.next_token;
            if token.is_none() {
                break;
            }
        }
        let mut state = self.state();
        let mut entries = Vec::with_capacity(found.len());
        for (name, kind, object) in found {
            let child_key = match kind {
                NodeKind::Dir => format!("{key}{name}/"),
                NodeKind::File => format!("{key}{name}"),
            };
            let (size, etag, modified) = object.map_or((0, None, None), |o| {
                (o.size, o.etag.clone(), o.last_modified)
            });
            let child = match state.by_key.get(&child_key).copied() {
                Some(existing) => {
                    let changed = state
                        .node(existing)
                        .map(|n| n.etag != etag || n.size != size)
                        .unwrap_or(false);
                    if changed {
                        state.blocks.forget(existing);
                    }
                    if let Some(node) = usize::try_from(existing)
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|n| state.nodes.get_mut(n))
                    {
                        node.size = size;
                        node.etag = etag;
                        node.modified = modified;
                    }
                    existing
                }
                None => {
                    state.nodes.push(Node {
                        key: child_key.clone(),
                        kind,
                        parent: ino,
                        size,
                        etag,
                        modified,
                    });
                    let created = state.nodes.len() as u64;
                    state.by_key.insert(child_key, created);
                    created
                }
            };
            entries.push(DirEntry {
                ino: child,
                name,
                kind,
            });
        }
        state.dirs.insert(
            ino,
            DirCache {
                entries: entries.clone(),
                fetched: Instant::now(),
            },
        );
        Ok(entries)
    }

    /// The parent of an inode (the root is its own parent).
    pub fn parent(&self, ino: u64) -> Result<u64> {
        Ok(self.state().node(ino)?.parent)
    }

    /// Finds `name` in directory `parent`; `not_found` when it is absent.
    pub async fn lookup(&self, parent: u64, name: &str) -> Result<Attr> {
        let entries = self.readdir(parent).await?;
        let entry = entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| Error::NotFound(format!("'{name}' does not exist")))?;
        self.getattr(entry.ino)
    }

    /// Reads up to `size` bytes of a file at `offset`. Shorter than `size`
    /// only at the end of the file; empty past it.
    pub async fn read(&self, ino: u64, offset: u64, size: u32) -> Result<Vec<u8>> {
        let (key, total, etag, parent) = {
            let state = self.state();
            let node = state.node(ino)?;
            if node.kind != NodeKind::File {
                return Err(Error::Usage(format!("inode {ino} is a directory")));
            }
            (node.key.clone(), node.size, node.etag.clone(), node.parent)
        };
        if offset >= total || size == 0 {
            return Ok(Vec::new());
        }
        let end = total.min(offset.saturating_add(u64::from(size)));
        let block_size = self.options.block_size.max(1);
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        for block in (offset / block_size)..=((end - 1) / block_size) {
            let cached = self.state().blocks.get(ino, block);
            let bytes = match cached {
                Some(bytes) => bytes,
                None => {
                    let first = block * block_size;
                    let last = (first + block_size).min(total) - 1;
                    match self.fetch(&key, first, last, etag.as_deref()).await {
                        Ok(bytes) => {
                            self.state().blocks.put(
                                ino,
                                block,
                                bytes.clone(),
                                self.options.cache_blocks,
                            );
                            bytes
                        }
                        Err(err) => {
                            // The object may have changed: list again next time.
                            let mut state = self.state();
                            state.dirs.remove(&parent);
                            state.blocks.forget(ino);
                            return Err(err);
                        }
                    }
                }
            };
            let block_start = block * block_size;
            let from = usize::try_from(offset.saturating_sub(block_start)).unwrap_or(0);
            let to = usize::try_from(end - block_start)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            if from < to {
                out.extend_from_slice(&bytes[from..to]);
            }
        }
        Ok(out)
    }

    /// One ranged `GetObject` of bytes `first` to `last` (inclusive).
    async fn fetch(&self, key: &str, first: u64, last: u64, etag: Option<&str>) -> Result<Bytes> {
        let ctx = || format!("s3://{}/{key}", self.bucket);
        let client = self.s3.client_for(&self.bucket).await?;
        let out = client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .range(format!("bytes={first}-{last}"))
            .set_if_match(etag.map(|etag| format!("\"{}\"", etag.trim_matches('"'))))
            .set_request_payer(self.s3.request_payer())
            .customize()
            .config_override(
                self.s3
                    .metadata_timeout()
                    .response_checksum_validation(ResponseChecksumValidation::WhenRequired),
            )
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(ctx()))?;
        let body = out.body.collect().await.map_err(|err| {
            Error::Remote(format!("reading {}: {}", ctx(), DisplayErrorContext(&err)))
        })?;
        let bytes = body.into_bytes();
        let wanted = usize::try_from(last - first + 1).unwrap_or(usize::MAX);
        // A server that ignored the range would send the whole object.
        if bytes.len() != wanted {
            return Err(Error::Remote(format!(
                "reading {}: expected {wanted} bytes of the range, got {}",
                ctx(),
                bytes.len()
            )));
        }
        Ok(bytes)
    }
}

fn attr_of(ino: u64, node: &Node) -> Attr {
    Attr {
        ino,
        kind: node.kind,
        size: node.size,
        modified: node.modified,
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('\0')
}

pub mod target;

#[cfg(all(feature = "mount", unix))]
pub mod fuse;
#[cfg(all(feature = "mount", windows))]
pub mod windows;

/// The adapter of this platform: FUSE on Linux and macOS, Dokan on Windows.
/// Both offer `mount(core, mountpoint, runtime) -> MountHandle` with
/// `mountpoint()`, `is_finished()` and `unmount()`.
#[cfg(all(feature = "mount", unix))]
pub use fuse as backend;
/// The adapter of this platform: FUSE on Linux and macOS, Dokan on Windows.
#[cfg(all(feature = "mount", windows))]
pub use windows as backend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_cannot_be_files_are_rejected() {
        assert!(valid_name("a b"));
        assert!(!valid_name(""));
        assert!(!valid_name("."));
        assert!(!valid_name(".."));
        assert!(!valid_name("a\0b"));
    }

    #[test]
    fn the_block_cache_evicts_the_least_recently_used_block() {
        let mut cache = BlockCache::default();
        cache.put(2, 0, Bytes::from_static(b"a"), 2);
        cache.put(2, 1, Bytes::from_static(b"b"), 2);
        assert!(cache.get(2, 0).is_some());
        cache.put(2, 2, Bytes::from_static(b"c"), 2);
        assert!(cache.get(2, 1).is_none());
        assert!(cache.get(2, 0).is_some());
        assert!(cache.get(2, 2).is_some());
        cache.forget(2);
        assert!(cache.get(2, 0).is_none());
    }

    #[test]
    fn support_follows_the_feature() {
        assert_eq!(
            supported(),
            cfg!(all(
                feature = "mount",
                any(
                    target_os = "linux",
                    target_os = "macos",
                    target_os = "windows"
                )
            ))
        );
        if !supported() {
            assert!(!fuse_installed());
        }
    }
}
