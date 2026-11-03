//! Support for `open_object`: the private folder that receives the downloaded
//! copies, their file names, cleanup, and the bandwidth limiter as the
//! engines' throttle.

use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bucketbridge_core::engine::{Throttle, ThrottleFuture};
use bucketbridge_core::transfer::BandwidthLimiter;
use bucketbridge_core::{Dirs, Error, Result};
use uuid::Uuid;

/// Folder inside the data folder that holds the copies made by `open_object`.
pub const OPEN_DIR_NAME: &str = "open";

/// Copies older than this are deleted at startup. They are not deleted
/// sooner because the application showing one may still have it open.
pub(crate) const KEEP_OPENED_FILES: Duration = Duration::from_secs(24 * 60 * 60);

/// `data_dir/open`.
pub(crate) fn open_root(dirs: &Dirs) -> PathBuf {
    dirs.data_dir.join(OPEN_DIR_NAME)
}

/// Creates a new folder `root/<uuid>` (and `root`), readable by the current
/// user only on Unix, so two opens never share a folder. Blocking.
pub(crate) fn new_open_dir(root: &Path) -> Result<PathBuf> {
    create_private_dir(root)?;
    let dir = root.join(Uuid::new_v4().to_string());
    create_private_dir(&dir)?;
    Ok(dir)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|err| Error::io(format!("creating {}", dir.display()), err))
}

pub(crate) use bucketbridge_core::edit::local_name;

/// Deletes the entries of `root` that were last modified more than `age`
/// ago. Best effort and blocking: a copy that cannot be deleted (on Windows,
/// one that is still open) is kept for the next start. Symbolic links are
/// removed, never followed.
pub(crate) fn purge(root: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|elapsed| elapsed > age);
        if old {
            let path = entry.path();
            let _ = if file_type.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// The app's [`BandwidthLimiter`] as the engines' [`Throttle`].
pub(crate) struct LimiterThrottle(pub(crate) BandwidthLimiter);

impl Throttle for LimiterThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(self.0.acquire(bytes))
    }

    /// At most one second's worth of the current rate, as the engines ask.
    fn max_grant(&self) -> u64 {
        self.0.rate().map_or(u64::MAX, NonZeroU64::get)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_removes_only_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh");
        fs::create_dir(&fresh).unwrap();
        fs::write(fresh.join("a.txt"), b"a").unwrap();
        purge(dir.path(), Duration::from_secs(3600));
        assert!(fresh.exists());
        purge(dir.path(), Duration::ZERO);
        assert!(!fresh.exists());
        purge(&dir.path().join("missing"), Duration::ZERO);
    }

    #[test]
    fn open_dirs_are_new_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("open");
        let first = new_open_dir(&root).unwrap();
        let second = new_open_dir(&root).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&root) && first.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&first).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
