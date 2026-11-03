//! Listing the remote side of a sync (v0.2.0): in parallel, and through a local cache.
//!
//! # Parallel listing
//!
//! A flat listing of a large prefix is a long chain of requests, because every page needs
//! the token of the one before. [`list_folder`] lists the prefix with the delimiter `/`
//! first (a short chain: one entry per sub-folder), then lists every sub-folder flat, at
//! most `parallelism` at a time, and joins the results. The result holds the same
//! objects as one flat listing, in no particular order.
//!
//! # The metadata cache
//!
//! The metadata of the last full listing of a prefix is kept in
//! `data_dir/sync-cache/`, one file per (account, bucket, prefix). A later listing of the
//! same prefix may be answered from it when all of these hold:
//!
//! - the cache is allowed (the option is on, and the sync deletes nothing: a stale entry
//!   must never decide a deletion, so a sync with `delete` always lists afresh),
//! - the snapshot is younger than [`CACHE_MAX_AGE`],
//! - the first page of the prefix, fetched with one request, is identical to the start of
//!   the snapshot (key, size, ETag, `Last-Modified` and storage class of every entry) and
//!   the snapshot ends where the page ends (it has no more entries when the page was the
//!   last one, and more when it was not).
//!
//! Otherwise the prefix is listed again and the snapshot replaced. A change deep inside a
//! long listing that leaves its first page unchanged is not noticed until the snapshot
//! expires or the cache is turned off (`--no-cache`); execution still refuses to replace
//! or delete anything that changed since the analysis (docs/sync.md).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Dirs;
use crate::error::{Error, Result};
use crate::model::ObjectEntry;
use crate::ops::S3;
use crate::s3uri::S3Uri;

/// Folder inside the data directory that holds the cache.
pub const CACHE_DIR_NAME: &str = "sync-cache";

/// Snapshots older than this are not used.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// Cache files kept; the least recently written are removed beyond this.
pub const MAX_CACHE_FILES: usize = 200;

/// Cache files not written for this long are removed.
pub const CACHE_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

/// Format version of the cache files.
const CACHE_VERSION: u32 = 1;

/// Keys requested by the validation page.
const VALIDATION_KEYS: u32 = 1000;

/// A snapshot of one listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedListing {
    /// Format version.
    pub version: u32,
    /// Account the listing was made with.
    pub account_id: String,
    /// Bucket.
    pub bucket: String,
    /// Prefix, as listed.
    pub prefix: String,
    /// When the listing was made.
    pub fetched_at: DateTime<Utc>,
    /// The objects, as the flat listing returned them.
    pub entries: Vec<ObjectEntry>,
}

/// The cache folder.
#[derive(Debug, Clone)]
pub struct ListingCache {
    dir: PathBuf,
}

impl ListingCache {
    /// The cache in `dirs.data_dir`.
    pub fn in_dirs(dirs: &Dirs) -> Self {
        ListingCache {
            dir: dirs.data_dir.join(CACHE_DIR_NAME),
        }
    }

    /// A cache in `dir` (the folder is created on the first write).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        ListingCache { dir: dir.into() }
    }

    /// The cache folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, account_id: &str, bucket: &str, prefix: &str) -> PathBuf {
        let mut hasher = Md5::new();
        for part in [account_id, bucket, prefix] {
            hasher.update(part.as_bytes());
            hasher.update([0u8]);
        }
        let name: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.dir.join(format!("{name}.json"))
    }

    /// The snapshot of a listing, when there is a readable one for exactly this
    /// (account, bucket, prefix). A damaged or foreign file counts as no snapshot.
    pub fn load(&self, account_id: &str, bucket: &str, prefix: &str) -> Option<CachedListing> {
        let bytes = fs::read(self.file(account_id, bucket, prefix)).ok()?;
        let cached: CachedListing = serde_json::from_slice(&bytes).ok()?;
        (cached.version == CACHE_VERSION
            && cached.account_id == account_id
            && cached.bucket == bucket
            && cached.prefix == prefix)
            .then_some(cached)
    }

    /// Replaces the snapshot of a listing (atomically: a temporary file in the same
    /// folder is renamed over the old one) and removes snapshots that are too old or too
    /// many.
    pub fn store(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
        entries: &[ObjectEntry],
    ) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .map_err(|err| Error::io(format!("cannot create {}", self.dir.display()), err))?;
        let snapshot = CachedListing {
            version: CACHE_VERSION,
            account_id: account_id.to_owned(),
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
            fetched_at: Utc::now(),
            entries: entries.to_vec(),
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|err| Error::Internal(format!("cannot encode the listing cache: {err}")))?;
        let target = self.file(account_id, bucket, prefix);
        let temporary = self.dir.join(format!(".{}.tmp", Uuid::new_v4().simple()));
        let written =
            write_private(&temporary, &bytes).and_then(|()| fs::rename(&temporary, &target));
        if let Err(err) = written {
            let _ = fs::remove_file(&temporary);
            return Err(Error::io(format!("cannot write {}", target.display()), err));
        }
        self.prune();
        Ok(())
    }

    /// Removes every snapshot; returns how many files went.
    pub fn clear(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| fs::remove_file(entry.path()).is_ok())
            .count()
    }

    /// Removes snapshots older than [`CACHE_RETENTION`] and, beyond
    /// [`MAX_CACHE_FILES`], the oldest ones. Best effort.
    fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        let mut files: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        files.sort();
        let excess = files.len().saturating_sub(MAX_CACHE_FILES);
        for (index, (modified, path)) in files.iter().enumerate() {
            let expired = now
                .duration_since(*modified)
                .is_ok_and(|age| age > CACHE_RETENTION);
            if expired || index < excess {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// How to list a prefix.
#[derive(Debug, Clone)]
pub struct ListOptions {
    /// Sub-folders listed at the same time; 1 lists flat.
    pub parallelism: usize,
    /// The cache and the account it is keyed with; `None` lists afresh.
    pub cache: Option<(ListingCache, String)>,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            parallelism: 1,
            cache: None,
        }
    }
}

/// Where a listing came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingSource {
    /// Listed from S3.
    Fresh,
    /// Answered from a valid snapshot after a one-page check.
    Cache,
}

/// A listing and where it came from.
#[derive(Debug, Clone)]
pub struct Listing {
    /// The objects under the prefix (folder markers included).
    pub entries: Vec<ObjectEntry>,
    /// Fresh or cached.
    pub source: ListingSource,
}

/// Lists every object under `folder` (see the [module docs](self)).
pub async fn list_folder(s3: &S3, folder: &S3Uri, options: &ListOptions) -> Result<Listing> {
    let bucket = folder.bucket();
    let prefix = folder.key();
    let Some((cache, account_id)) = &options.cache else {
        let entries = list_fresh(s3, bucket, prefix, options.parallelism).await?;
        return Ok(Listing {
            entries,
            source: ListingSource::Fresh,
        });
    };

    // The validation page: one request that is also the whole listing when the prefix
    // fits in a page.
    let page = s3
        .list_objects(bucket, prefix, true, None, VALIDATION_KEYS)
        .await?;
    let cached = {
        let (cache, account_id, bucket, prefix) = (
            cache.clone(),
            account_id.clone(),
            bucket.to_owned(),
            prefix.to_owned(),
        );
        tokio::task::spawn_blocking(move || cache.load(&account_id, &bucket, &prefix))
            .await
            .map_err(|err| Error::Internal(format!("a cache thread failed: {err}")))?
    };
    if let Some(snapshot) = cached
        && is_valid(&snapshot, &page.entries, page.next_token.is_some(), prefix)
    {
        return Ok(Listing {
            entries: snapshot.entries,
            source: ListingSource::Cache,
        });
    }
    let entries = if page.next_token.is_none() {
        page.entries
    } else {
        list_fresh(s3, bucket, prefix, options.parallelism).await?
    };
    // A cache that cannot be written only costs the next scan its speed.
    let (cache, account_id, bucket, prefix, snapshot) = (
        cache.clone(),
        account_id.clone(),
        bucket.to_owned(),
        prefix.to_owned(),
        entries.clone(),
    );
    let _ =
        tokio::task::spawn_blocking(move || cache.store(&account_id, &bucket, &prefix, &snapshot))
            .await;
    Ok(Listing {
        entries,
        source: ListingSource::Fresh,
    })
}

/// Whether a snapshot still describes the prefix, judged from its first page.
fn is_valid(
    snapshot: &CachedListing,
    page: &[ObjectEntry],
    more_pages: bool,
    prefix: &str,
) -> bool {
    let age = Utc::now().signed_duration_since(snapshot.fetched_at);
    let fresh_enough =
        age >= chrono::TimeDelta::zero() && age.to_std().is_ok_and(|age| age <= CACHE_MAX_AGE);
    if !fresh_enough {
        return false;
    }
    // The page never holds the prefix's own marker.
    let cached: Vec<&ObjectEntry> = snapshot
        .entries
        .iter()
        .filter(|entry| entry.key != prefix)
        .collect();
    let same_start = page.len() <= cached.len()
        && page
            .iter()
            .zip(&cached)
            .all(|(fresh, old)| same_metadata(fresh, old));
    let same_end = if more_pages {
        cached.len() > page.len()
    } else {
        cached.len() == page.len()
    };
    same_start && same_end
}

fn same_metadata(a: &ObjectEntry, b: &ObjectEntry) -> bool {
    a.key == b.key
        && a.size == b.size
        && a.etag == b.etag
        && a.last_modified == b.last_modified
        && a.storage_class == b.storage_class
}

async fn list_fresh(
    s3: &S3,
    bucket: &str,
    prefix: &str,
    parallelism: usize,
) -> Result<Vec<ObjectEntry>> {
    if parallelism <= 1 {
        return s3.list_all(bucket, prefix).await;
    }
    // The first level, with the delimiter: objects and sub-folders.
    let mut entries = Vec::new();
    let mut folders = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .list_objects(bucket, prefix, false, token.as_deref(), 1000)
            .await?;
        for entry in page.entries {
            if entry.is_prefix {
                folders.push(entry.key);
            } else {
                entries.push(entry);
            }
        }
        match page.next_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    let listed: Vec<Vec<ObjectEntry>> = collect_bounded(folders, parallelism, |folder| {
        let s3 = s3.clone();
        let bucket = bucket.to_owned();
        async move { s3.list_all(&bucket, &folder).await }
    })
    .await?;
    entries.extend(listed.into_iter().flatten());
    Ok(entries)
}

/// Runs `work` for every item, at most `limit` at the same time, and collects the results
/// (in no particular order). The first error ends the collection and drops the work that
/// is still running.
pub async fn collect_bounded<I, T, F, Fut>(items: I, limit: usize, work: F) -> Result<Vec<T>>
where
    I: IntoIterator,
    F: FnMut(I::Item) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    futures::stream::iter(items)
        .map(work)
        .buffer_unordered(limit.max(1))
        .try_collect()
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::model::Checksums;

    #[tokio::test]
    async fn work_runs_at_most_limit_at_a_time_and_all_of_it_runs() {
        for limit in [1usize, 3, 8] {
            let running = AtomicUsize::new(0);
            let peak = AtomicUsize::new(0);
            let results = collect_bounded(0..20u32, limit, |item| {
                let (running, peak) = (&running, &peak);
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(item)
                }
            })
            .await
            .unwrap();
            assert_eq!(results.len(), 20);
            let peak = peak.load(Ordering::SeqCst);
            assert!(peak <= limit, "limit {limit}, peak {peak}");
            if limit > 1 {
                assert!(peak > 1, "limit {limit} ran one at a time");
            }
        }
        // A limit of zero still makes progress.
        let one = collect_bounded(0..3u32, 0, |item| async move { Ok(item) })
            .await
            .unwrap();
        assert_eq!(one.len(), 3);
    }

    #[tokio::test]
    async fn the_first_error_ends_the_collection() {
        let err = collect_bounded(0..10u32, 2, |item| async move {
            if item == 3 {
                Err(Error::Remote("boom".into()))
            } else {
                Ok(item)
            }
        })
        .await
        .unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Remote);
    }

    fn entry(key: &str, size: u64) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: Some("abc".to_owned()),
            last_modified: DateTime::from_timestamp(1_700_000_000, 0),
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    fn snapshot(entries: Vec<ObjectEntry>, age: chrono::TimeDelta) -> CachedListing {
        CachedListing {
            version: CACHE_VERSION,
            account_id: "a".to_owned(),
            bucket: "b".to_owned(),
            prefix: "p/".to_owned(),
            fetched_at: Utc::now() - age,
            entries,
        }
    }

    #[test]
    fn snapshots_are_valid_while_young_and_unchanged_at_the_start() {
        let listed = vec![entry("p/a", 1), entry("p/b", 2), entry("p/c", 3)];
        let young = snapshot(listed.clone(), chrono::TimeDelta::seconds(10));
        // The whole listing fits in the page.
        assert!(is_valid(&young, &listed, false, "p/"));
        // The prefix's own marker is not part of a page.
        let mut with_marker = vec![entry("p/", 0)];
        with_marker.extend(listed.clone());
        assert!(is_valid(
            &snapshot(with_marker, chrono::TimeDelta::seconds(10)),
            &listed,
            false,
            "p/"
        ));
        // The page is the first of several: the snapshot must hold more.
        assert!(is_valid(&young, &listed[..2], true, "p/"));
        assert!(!is_valid(&young, &listed, true, "p/"));
        // A longer listing than the snapshot, a shorter one, a changed entry.
        assert!(!is_valid(&young, &listed[..2], false, "p/"));
        let grown = [listed.clone(), vec![entry("p/d", 4)]].concat();
        assert!(!is_valid(&young, &grown, false, "p/"));
        let mut changed = listed.clone();
        changed[1].size = 99;
        assert!(!is_valid(&young, &changed, false, "p/"));
        let mut retimed = listed.clone();
        retimed[0].last_modified = DateTime::from_timestamp(1_700_000_001, 0);
        assert!(!is_valid(&young, &retimed, false, "p/"));
        let mut other_etag = listed.clone();
        other_etag[2].etag = Some("different".to_owned());
        assert!(!is_valid(&young, &other_etag, false, "p/"));
    }

    #[test]
    fn old_snapshots_are_never_valid() {
        let listed = vec![entry("p/a", 1)];
        let old = snapshot(listed.clone(), chrono::TimeDelta::seconds(301));
        assert!(!is_valid(&old, &listed, false, "p/"));
        let future = snapshot(listed.clone(), chrono::TimeDelta::seconds(-60));
        assert!(!is_valid(&future, &listed, false, "p/"));
    }

    #[test]
    fn the_cache_round_trips_per_account_bucket_and_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ListingCache::at(dir.path().join("sync-cache"));
        assert!(cache.load("a", "b", "p/").is_none());
        let listed = vec![entry("p/a", 1), entry("p/b", 2)];
        cache.store("a", "b", "p/", &listed).unwrap();
        let loaded = cache.load("a", "b", "p/").unwrap();
        assert_eq!(loaded.entries, listed);
        assert_eq!(loaded.prefix, "p/");
        // Another account, bucket or prefix has no snapshot.
        assert!(cache.load("other", "b", "p/").is_none());
        assert!(cache.load("a", "other", "p/").is_none());
        assert!(cache.load("a", "b", "q/").is_none());
        // Storing again replaces it.
        cache.store("a", "b", "p/", &listed[..1]).unwrap();
        assert_eq!(cache.load("a", "b", "p/").unwrap().entries.len(), 1);
        // A damaged file is no snapshot.
        let file = cache.file("a", "b", "p/");
        fs::write(&file, b"{ not json").unwrap();
        assert!(cache.load("a", "b", "p/").is_none());
        assert!(cache.clear() >= 1);
        assert!(cache.load("a", "b", "p/").is_none());
    }

    #[test]
    fn an_identity_mismatch_inside_the_file_is_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ListingCache::at(dir.path());
        cache.store("a", "b", "p/", &[entry("p/a", 1)]).unwrap();
        let file = cache.file("a", "b", "p/");
        let forged = fs::read_to_string(&file)
            .unwrap()
            .replace("\"bucket\":\"b\"", "\"bucket\":\"x\"");
        fs::write(&file, forged).unwrap();
        assert!(cache.load("a", "b", "p/").is_none());
    }

    #[test]
    fn old_files_and_surplus_files_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ListingCache::at(dir.path());
        for number in 0..(MAX_CACHE_FILES + 5) {
            cache
                .store("a", "b", &format!("p{number}/"), &[entry("k", 1)])
                .unwrap();
        }
        let count = fs::read_dir(dir.path()).unwrap().count();
        assert!(count <= MAX_CACHE_FILES, "{count}");
    }

    #[cfg(unix)]
    #[test]
    fn cache_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache = ListingCache::at(dir.path());
        cache.store("a", "b", "p/", &[entry("k", 1)]).unwrap();
        let mode = fs::metadata(cache.file("a", "b", "p/"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}
