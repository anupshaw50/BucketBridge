//! Edit in place: open an object, edit a local copy, and upload every save.
//!
//! An [`EditManager`] keeps the edit sessions of one process. A session is
//! one object (account, bucket, key) with a private local copy under
//! `<root>/<uuid>/` and the ETag the object had when the copy was made (or
//! when it was last uploaded). A watcher on the copy's folder (the `notify`
//! crate; it sees saves that write in place and saves that write a new file
//! and rename it over the old one) waits for the file to be quiet for the
//! debounce interval and then uploads it through the transfer engine with
//! the object's content headers, user metadata, storage class and tags kept.
//!
//! **Conflicts.** Before an upload the object is read with `HeadObject`. When
//! its ETag is not the one the session last saw (somebody else changed or
//! deleted the object), nothing is uploaded: the session turns
//! [`EditState::Conflict`] and the user chooses with
//! [`EditManager::resolve`]: overwrite, save as a copy under a new key
//! (`name (edited).ext`), or discard the local changes. The upload itself
//! carries `If-Match` with that ETag where the server supports conditional
//! writes (a server that answers `501` is remembered and not asked again).
//!
//! **Ending.** A session ends with [`EditManager::stop`] (a pending save is
//! uploaded first, unless the changes are discarded) or at shutdown
//! ([`EditManager::shutdown`]). The local copy is deleted when a session ends
//! cleanly; one that could not upload its last save is kept on disk.
//!
//! Versioned buckets: the current version is always edited, and every upload
//! creates a new current version.

mod actor;
mod names;
mod watch;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::engine::{Hooks, Outcome, S3Provider, Throttle, TransferEngine, TransferOptions};
use crate::error::{Error, Result};
use crate::model::ObjectHead;
use crate::ops::S3;
use crate::path::safe_local_path;
use crate::s3uri::S3Uri;

use actor::{Actor, Command, Parts, header_parts};
pub use names::{copy_key, local_name};
use watch::Watching;

/// How long the copy must stay unchanged before a save is uploaded.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(1);

/// Copies of sessions that were never ended are deleted at startup when they
/// are this old ([`EditManager::purge_stale`]).
pub const KEEP_LEFTOVER_COPIES: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Capacity of the broadcast channel of session changes.
const EVENT_CAPACITY: usize = 256;

/// Where a session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditState {
    /// Waiting for the next save.
    Watching,
    /// A save is being checked and uploaded.
    Uploading,
    /// The last save was uploaded; still waiting for the next one.
    Uploaded,
    /// The object changed on the server since the session last saw it; the
    /// save is held until the user resolves it.
    Conflict,
    /// The last save could not be uploaded (see `error`); the next save, or
    /// [`EditResolution::Retry`], tries again.
    Error,
    /// The session ended. Sent once as the last change of a session.
    Stopped,
}

/// What a conflict is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditConflict {
    /// The object no longer exists.
    pub remote_deleted: bool,
    /// The ETag the object has now, without quotes.
    pub remote_etag: Option<String>,
    /// When the object was last modified.
    pub remote_modified: Option<DateTime<Utc>>,
}

/// One edit session, as listed and sent in `edit://changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditSession {
    /// Session id.
    pub id: String,
    /// Account of the object.
    pub account_id: String,
    /// Bucket of the object.
    pub bucket: String,
    /// Key of the object.
    pub key: String,
    /// Absolute path of the local copy.
    pub local_path: String,
    /// Where the session is.
    pub state: EditState,
    /// The ETag the next upload expects to find on the server (the one at
    /// open time until the first upload, the uploaded object's after that).
    pub etag: Option<String>,
    /// Version ID of the object at open time, on versioned buckets.
    pub version_id: Option<String>,
    /// When the session started.
    pub started_at: DateTime<Utc>,
    /// When the last save was uploaded.
    pub last_upload_at: Option<DateTime<Utc>>,
    /// Saves uploaded so far.
    pub uploads: u32,
    /// Why the last upload failed (`state` is `error`).
    pub error: Option<String>,
    /// The conflict (`state` is `conflict`).
    pub conflict: Option<EditConflict>,
}

/// How the user settles a conflict (or an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditResolution {
    /// Upload the local copy over the server's current object.
    Overwrite,
    /// Upload the local copy under a new key, `name (edited).ext`, and end
    /// the session. The object on the server is not touched.
    SaveCopy,
    /// Drop the local changes and end the session.
    Discard,
    /// Try the failed upload again (state `error` only).
    Retry,
}

/// What [`EditManager::resolve`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditResolved {
    /// The session afterwards; `None` when the resolution ended it.
    pub session: Option<EditSession>,
    /// The key of the copy, for [`EditResolution::SaveCopy`].
    pub copy_key: Option<String>,
}

/// What [`EditManager::start`] needs.
#[derive(Debug, Clone)]
pub struct StartEdit {
    /// Account of the object.
    pub account_id: String,
    /// Bucket of the object.
    pub bucket: String,
    /// Key of the object (not a folder).
    pub key: String,
    /// How the download and the uploads are split and checked.
    pub transfer: TransferOptions,
}

/// The answer of [`EditManager::start`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditStarted {
    /// The session.
    pub session: EditSession,
    /// The object was already being edited; the existing session is returned
    /// and nothing was downloaded.
    pub reused: bool,
}

/// Settings of an [`EditManager`].
#[derive(Debug, Clone)]
pub struct EditOptions {
    /// Quiet time before a save is uploaded.
    pub debounce: Duration,
}

impl Default for EditOptions {
    fn default() -> Self {
        EditOptions {
            debounce: DEFAULT_DEBOUNCE,
        }
    }
}

struct Handle {
    snapshot: Arc<Mutex<EditSession>>,
    commands: mpsc::UnboundedSender<Command>,
    dir: PathBuf,
}

#[derive(Default)]
pub(crate) struct Registry {
    sessions: Mutex<HashMap<String, Handle>>,
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Handle>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn remove(&self, id: &str) {
        self.lock().remove(id);
    }
}

/// The edit sessions of one process.
#[derive(Clone)]
pub struct EditManager {
    root: PathBuf,
    provider: S3Provider,
    options: EditOptions,
    throttle: Option<Arc<dyn Throttle>>,
    registry: Arc<Registry>,
    events: broadcast::Sender<EditSession>,
}

impl std::fmt::Debug for EditManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditManager")
            .field("root", &self.root)
            .field("sessions", &self.registry.lock().len())
            .finish_non_exhaustive()
    }
}

impl EditManager {
    /// A manager whose copies live under `root`; `provider` resolves an
    /// account to its S3 transport at every upload.
    pub fn new(root: PathBuf, provider: S3Provider, options: EditOptions) -> Self {
        EditManager {
            root,
            provider,
            options,
            throttle: None,
            registry: Arc::default(),
            events: broadcast::channel(EVENT_CAPACITY).0,
        }
    }

    /// A manager whose downloads and uploads go through `throttle` (the
    /// bandwidth cap).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<dyn Throttle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Every change of every session, in order. A receiver that falls behind
    /// skips to newer changes; [`EditManager::list`] has the full state.
    pub fn subscribe(&self) -> broadcast::Receiver<EditSession> {
        self.events.subscribe()
    }

    /// The active sessions, oldest first.
    pub fn list(&self) -> Vec<EditSession> {
        let mut sessions: Vec<EditSession> = self
            .registry
            .lock()
            .values()
            .map(|handle| lock(&handle.snapshot).clone())
            .collect();
        sessions.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        sessions
    }

    /// One active session.
    pub fn get(&self, id: &str) -> Option<EditSession> {
        self.registry
            .lock()
            .get(id)
            .map(|handle| lock(&handle.snapshot).clone())
    }

    /// Downloads the object into a new private folder and starts watching
    /// the copy. The caller opens `session.local_path` afterwards. An object
    /// that is already being edited returns its session (`reused`).
    ///
    /// Folders, and objects that are client-side encrypted (edit a decrypted
    /// copy instead), are refused.
    pub async fn start(&self, request: StartEdit) -> Result<EditStarted> {
        let StartEdit {
            account_id,
            bucket,
            key,
            transfer,
        } = request;
        if key.is_empty() || key.ends_with('/') {
            return Err(Error::Usage(format!(
                "'{}' is a folder; only objects can be edited",
                key.escape_debug()
            )));
        }
        let uri = S3Uri::new(&bucket, &key)?;
        if let Some(session) = self.find(&account_id, &bucket, &key) {
            return Ok(EditStarted {
                session,
                reused: true,
            });
        }
        let s3 = (self.provider)(account_id.clone()).await?;
        let mut head = s3.head_object(&bucket, &key, None).await?;
        if crate::cse::is_marked(head.meta.user_meta.iter()) {
            return Err(Error::Usage(format!(
                "{uri} is client-side encrypted; decrypt it before editing it"
            )));
        }

        let id = Uuid::new_v4().to_string();
        let dir = self.root.join(&id);
        let (dir_to_create, name) = (dir.clone(), local_name(&key));
        let dest = blocking(move || {
            names::create_private_dir(&dir_to_create)?;
            safe_local_path(&dir_to_create, &name)
        })
        .await?;

        let (fingerprint, watching) = match self
            .download_and_watch(&s3, &uri, &dest, &dir, &transfer, &mut head)
            .await
        {
            Ok(parts) => parts,
            Err(error) => {
                remove_dir(dir).await;
                return Err(error);
            }
        };

        let tags = s3.get_object_tags(&bucket, &key).await.ok();
        let (headers, content_type) = header_parts(&head);
        let session = EditSession {
            id: id.clone(),
            account_id,
            bucket,
            key,
            local_path: dest.to_string_lossy().into_owned(),
            state: EditState::Watching,
            etag: head.etag.clone(),
            version_id: head.version_id.clone(),
            started_at: Utc::now(),
            last_upload_at: None,
            uploads: 0,
            error: None,
            conflict: None,
        };
        let snapshot = Arc::new(Mutex::new(session.clone()));
        let (commands, command_rx) = mpsc::unbounded_channel();
        let raced = {
            let mut sessions = self.registry.lock();
            let existing = sessions.values().find_map(|handle| {
                let other = lock(&handle.snapshot);
                (other.account_id == session.account_id
                    && other.bucket == session.bucket
                    && other.key == session.key)
                    .then(|| other.clone())
            });
            if existing.is_none() {
                sessions.insert(
                    id,
                    Handle {
                        snapshot: Arc::clone(&snapshot),
                        commands,
                        dir: dir.clone(),
                    },
                );
            }
            existing
        };
        if let Some(existing) = raced {
            // Another start of the same object finished first.
            drop(watching);
            remove_dir(dir).await;
            return Ok(EditStarted {
                session: existing,
                reused: true,
            });
        }
        let actor = Actor::new(
            Parts {
                snapshot,
                uri,
                path: dest,
                dir,
                provider: Arc::clone(&self.provider),
                throttle: self.throttle.clone(),
                registry: Arc::downgrade(&self.registry),
                events: self.events.clone(),
                debounce: self.options.debounce,
                transfer,
                headers,
                content_type,
                tags,
                fingerprint,
            },
            watching,
        );
        tokio::spawn(actor.run(command_rx));
        // No subscriber is not an error.
        let _ = self.events.send(session.clone());
        Ok(EditStarted {
            session,
            reused: false,
        })
    }

    /// Ends a session. A save that was not uploaded yet is uploaded first;
    /// when that fails or conflicts, the session stays and the error is
    /// returned. With `discard`, local changes are dropped and nothing is
    /// uploaded. The copy is deleted.
    pub async fn stop(&self, id: &str, discard: bool) -> Result<()> {
        let (reply, answer) = oneshot::channel();
        self.send(id, Command::Stop { discard, reply })?;
        answer
            .await
            .map_err(|_| Error::NotFound(format!("edit session {id}")))?
    }

    /// Settles the conflict (or error) of a session.
    pub async fn resolve(&self, id: &str, resolution: EditResolution) -> Result<EditResolved> {
        let (reply, answer) = oneshot::channel();
        self.send(id, Command::Resolve { resolution, reply })?;
        answer
            .await
            .map_err(|_| Error::NotFound(format!("edit session {id}")))?
    }

    /// Stops every session, uploading pending saves first, each for at most
    /// `grace`. Returns the sessions that could not end (a conflict, or an
    /// upload that failed): their copies stay on disk.
    pub async fn shutdown(&self, grace: Duration) -> Vec<EditSession> {
        let ids: Vec<String> = self.list().into_iter().map(|session| session.id).collect();
        let stops = ids.iter().map(|id| async move {
            // A failed or late stop leaves the session in the list below.
            let _ = tokio::time::timeout(grace, self.stop(id, false)).await;
        });
        futures::future::join_all(stops).await;
        self.list()
    }

    /// Deletes copies under the root that no session uses and that were last
    /// changed more than `age` ago (left by a crash or by a session that
    /// could not upload its last save). Best effort, blocking.
    pub fn purge_stale(&self, age: Duration) {
        let active: Vec<PathBuf> = self
            .registry
            .lock()
            .values()
            .map(|handle| handle.dir.clone())
            .collect();
        purge(&self.root, age, &active);
    }

    fn find(&self, account_id: &str, bucket: &str, key: &str) -> Option<EditSession> {
        self.list().into_iter().find(|session| {
            session.account_id == account_id && session.bucket == bucket && session.key == key
        })
    }

    fn send(&self, id: &str, command: Command) -> Result<()> {
        let sessions = self.registry.lock();
        let handle = sessions
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("edit session {id}")))?;
        handle
            .commands
            .send(command)
            .map_err(|_| Error::NotFound(format!("edit session {id}")))
    }

    /// Downloads the object to `dest`, takes the fingerprint of the copy and
    /// starts the watcher. `head` is refreshed when the object changed
    /// between the head request and the download.
    async fn download_and_watch(
        &self,
        s3: &S3,
        uri: &S3Uri,
        dest: &Path,
        dir: &Path,
        transfer: &TransferOptions,
        head: &mut ObjectHead,
    ) -> Result<(watch::Fingerprint, Watching)> {
        let engine = TransferEngine::new(s3.clone(), transfer.clone())?;
        let hooks = match &self.throttle {
            Some(throttle) => Hooks::new().with_throttle(Arc::clone(throttle)),
            None => Hooks::new(),
        };
        let downloaded = engine
            .download_file(uri, dest, &hooks, None, &|_| {})
            .await?;
        let Outcome::Completed(downloaded) = downloaded else {
            return Err(Error::Cancelled);
        };
        if downloaded.etag.is_some() && downloaded.etag != head.etag {
            // The object changed after the head request: take the headers of
            // what was downloaded (best effort) and its ETag.
            if let Ok(fresh) = s3.head_object(uri.bucket(), uri.key(), None).await {
                *head = fresh;
            }
            head.etag.clone_from(&downloaded.etag);
        }
        let path = dest.to_path_buf();
        let fingerprint = blocking(move || watch::fingerprint(&path)).await?;
        let watching = Watching::start(dir)?;
        Ok((fingerprint, watching))
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))?
}

/// Deletes a session folder (best effort).
pub(crate) async fn remove_dir(dir: PathBuf) {
    let _ = blocking(move || {
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    })
    .await;
}

/// The newest modification time of a folder and its entries.
fn newest_change(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir)
        .and_then(|meta| meta.modified())
        .ok()?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                newest = newest.max(modified);
            }
        }
    }
    Some(newest)
}

fn purge(root: &Path, age: Duration, active: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if active.contains(&path) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let old = newest_change(&path)
            .and_then(|changed| now.duration_since(changed).ok())
            .is_some_and(|elapsed| elapsed > age);
        if old {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_removes_only_old_unused_folders() {
        let root = tempfile::tempdir().unwrap();
        let used = root.path().join("used");
        let stale = root.path().join("stale");
        for dir in [&used, &stale] {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("a.txt"), b"a").unwrap();
        }
        purge(root.path(), Duration::from_secs(3600), &[]);
        assert!(used.exists() && stale.exists());
        purge(root.path(), Duration::ZERO, std::slice::from_ref(&used));
        assert!(used.exists());
        assert!(!stale.exists());
        purge(&root.path().join("missing"), Duration::ZERO, &[]);
    }
}
