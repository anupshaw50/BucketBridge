//! The file watcher of an edit session and the fingerprint that tells a real
//! change from noise.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use md5::{Digest, Md5};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::error::{Error, Result};

/// What the watcher reports.
#[derive(Debug)]
pub(crate) enum FsMsg {
    /// Something in the session's folder changed.
    Changed,
    /// The watcher failed.
    Failed(String),
}

/// A running watcher on a session's folder. Dropping it stops watching.
///
/// The folder is watched, not the file: a save that writes a new file and
/// renames it over the old one replaces the file's inode, which a watch on
/// the file itself would lose.
pub(crate) struct Watching {
    /// Held for its `Drop`: the watcher stops with it.
    #[allow(dead_code)]
    pub(crate) guard: RecommendedWatcher,
    pub(crate) events: UnboundedReceiver<FsMsg>,
}

impl Watching {
    /// Starts watching `dir` (not its subfolders).
    pub(crate) fn start(dir: &Path) -> Result<Self> {
        let (tx, events) = unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |result| forward(&tx, result))
            .map_err(|err| watch_error(dir, &err))?;
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|err| watch_error(dir, &err))?;
        Ok(Watching {
            guard: watcher,
            events,
        })
    }
}

fn watch_error(dir: &Path, err: &notify::Error) -> Error {
    Error::io(
        format!("watching {} for saves", dir.display()),
        io::Error::other(err.to_string()),
    )
}

fn forward(tx: &UnboundedSender<FsMsg>, result: notify::Result<notify::Event>) {
    // The receiver is gone once the session ended.
    let _ = match result {
        Ok(event) => {
            if is_noise(&event.kind) {
                return;
            }
            tx.send(FsMsg::Changed)
        }
        Err(err) => tx.send(FsMsg::Failed(err.to_string())),
    };
}

/// Reads of the file (including the ones the session itself does to hash
/// it) must not start another round.
fn is_noise(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
        EventKind::Access(_) => true,
        _ => false,
    }
}

/// What identifies the content of the local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    size: u64,
    modified: Option<SystemTime>,
    md5: [u8; 16],
}

/// What a check of the copy found.
#[derive(Debug)]
pub(crate) enum Detected {
    /// The file does not exist (an editor between two steps of a save).
    Missing,
    /// The content is the one of the last fingerprint; carries the current
    /// fingerprint so that the next check can skip hashing.
    Unchanged(Fingerprint),
    /// The content differs.
    Changed(Fingerprint),
}

/// Hashes the file. Blocking.
pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let context = || format!("reading {}", path.display());
    let mut file = File::open(path).map_err(|err| Error::io(context(), err))?;
    let meta = file.metadata().map_err(|err| Error::io(context(), err))?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::io(context(), err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Fingerprint {
        size: meta.len(),
        modified: meta.modified().ok(),
        md5: hasher.finalize().into(),
    })
}

/// Compares the file with `last`: a file with the same size and modification
/// time is unchanged without being read; otherwise its content is hashed.
/// Blocking.
pub(crate) fn detect(path: &Path, last: &Fingerprint) -> Result<Detected> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Detected::Missing),
        Err(err) => {
            return Err(Error::io(format!("reading {}", path.display()), err));
        }
    };
    if meta.len() == last.size && meta.modified().ok() == last.modified && last.modified.is_some() {
        return Ok(Detected::Unchanged(*last));
    }
    let current = match fingerprint(path) {
        Ok(current) => current,
        // Removed between the two calls.
        Err(err) if err.kind() == crate::ErrorKind::Io && !path.exists() => {
            return Ok(Detected::Missing);
        }
        Err(err) => return Err(err),
    };
    if current.size == last.size && current.md5 == last.md5 {
        Ok(Detected::Unchanged(current))
    } else {
        Ok(Detected::Changed(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn detects_changes_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"one").unwrap();
        let first = fingerprint(&path).unwrap();
        assert!(matches!(
            detect(&path, &first).unwrap(),
            Detected::Unchanged(_)
        ));

        // Same content written again: a new modification time, no change.
        std::fs::write(&path, b"one").unwrap();
        assert!(matches!(
            detect(&path, &first).unwrap(),
            Detected::Unchanged(_)
        ));

        std::fs::write(&path, b"two").unwrap();
        assert!(matches!(
            detect(&path, &first).unwrap(),
            Detected::Changed(_)
        ));

        std::fs::remove_file(&path).unwrap();
        assert!(matches!(detect(&path, &first).unwrap(), Detected::Missing));
    }

    #[tokio::test]
    async fn the_watcher_sees_in_place_and_rename_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"one").unwrap();
        let mut watching = Watching::start(dir.path()).unwrap();

        std::fs::write(&path, b"two").unwrap();
        wait_for_event(&mut watching).await;

        let staged = dir.path().join(".a.txt.swp");
        std::fs::write(&staged, b"three").unwrap();
        std::fs::rename(&staged, &path).unwrap();
        wait_for_event(&mut watching).await;
    }

    async fn wait_for_event(watching: &mut Watching) {
        let message = tokio::time::timeout(Duration::from_secs(10), watching.events.recv())
            .await
            .expect("an event within ten seconds");
        assert!(matches!(message, Some(FsMsg::Changed)));
    }
}
