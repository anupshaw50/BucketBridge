//! The task that owns one edit session: it watches the copy, uploads saves
//! and answers commands, one thing at a time, so a save, a conflict
//! resolution and a stop can never race.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::Utc;
use notify::RecommendedWatcher;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{broadcast, oneshot};
use tokio::time::Instant;

use super::watch::{self, Detected, Fingerprint, FsMsg, Watching};
use super::{
    EditConflict, EditResolution, EditResolved, EditSession, EditState, Registry, blocking, lock,
    remove_dir,
};
use crate::engine::{
    FileSource, Hooks, Outcome, S3Provider, Throttle, TransferEngine, TransferOptions,
    UploadHeaders, UploadOutcome, UploadState, WriteCondition, guess_content_type,
};
use crate::error::{Error, ErrorKind, Result};
use crate::model::ObjectHead;
use crate::ops::S3;
use crate::s3uri::S3Uri;
use crate::settings::IfExists;

/// How many names `name (edited N).ext` are tried before giving up.
const MAX_COPY_NAMES: u32 = 50;

pub(crate) enum Command {
    Stop {
        discard: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    Resolve {
        resolution: EditResolution,
        reply: oneshot::Sender<Result<EditResolved>>,
    },
}

/// What an [`Actor`] starts from.
pub(crate) struct Parts {
    pub snapshot: Arc<Mutex<EditSession>>,
    pub uri: S3Uri,
    pub path: PathBuf,
    pub dir: PathBuf,
    pub provider: S3Provider,
    pub throttle: Option<Arc<dyn Throttle>>,
    pub registry: Weak<Registry>,
    pub events: broadcast::Sender<EditSession>,
    pub debounce: Duration,
    pub transfer: TransferOptions,
    pub headers: UploadHeaders,
    pub content_type: String,
    pub tags: Option<BTreeMap<String, String>>,
    pub fingerprint: Fingerprint,
}

/// The upload headers and content type an object keeps through a rewrite.
pub(crate) fn header_parts(head: &ObjectHead) -> (UploadHeaders, String) {
    let meta = &head.meta;
    let headers = UploadHeaders {
        metadata: meta
            .user_meta
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        cache_control: meta.cache_control.clone(),
        content_disposition: meta.content_disposition.clone(),
        content_encoding: meta.content_encoding.clone(),
        content_language: None,
        expires: None,
        storage_class: head
            .storage_class
            .clone()
            .filter(|class| class != "STANDARD"),
    };
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| guess_content_type(std::path::Path::new(&head.key)).to_owned());
    (headers, content_type)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Upload only when the object is the one the session last saw.
    Save,
    /// Upload over whatever is there now (the user chose to).
    Overwrite,
}

enum Attempt {
    Done(Option<String>),
    Conflict(EditConflict),
    Failed(Error),
}

enum Upload {
    Done,
    Conflict,
    Failed(Error),
}

pub(crate) struct Actor {
    snapshot: Arc<Mutex<EditSession>>,
    uri: S3Uri,
    path: PathBuf,
    dir: PathBuf,
    provider: S3Provider,
    throttle: Option<Arc<dyn Throttle>>,
    registry: Weak<Registry>,
    events: broadcast::Sender<EditSession>,
    debounce: Duration,
    transfer: TransferOptions,
    headers: UploadHeaders,
    content_type: String,
    tags: Option<BTreeMap<String, String>>,
    /// The content last uploaded (or downloaded).
    last: Fingerprint,
    /// The ETag the object must still have for a save to be uploaded.
    base_etag: Option<String>,
    /// False once the server refused `If-Match`.
    conditional: bool,
    guard: Option<RecommendedWatcher>,
    fs: UnboundedReceiver<FsMsg>,
}

impl Actor {
    pub(crate) fn new(parts: Parts, watching: Watching) -> Self {
        let base_etag = lock(&parts.snapshot).etag.clone();
        Actor {
            snapshot: parts.snapshot,
            uri: parts.uri,
            path: parts.path,
            dir: parts.dir,
            provider: parts.provider,
            throttle: parts.throttle,
            registry: parts.registry,
            events: parts.events,
            debounce: parts.debounce,
            transfer: parts.transfer,
            headers: parts.headers,
            content_type: parts.content_type,
            tags: parts.tags,
            last: parts.fingerprint,
            base_etag,
            conditional: true,
            guard: Some(watching.guard),
            fs: watching.events,
        }
    }

    pub(crate) async fn run(mut self, mut commands: UnboundedReceiver<Command>) {
        let mut deadline: Option<Instant> = None;
        let mut watching = true;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => {
                        if self.command(command).await {
                            break;
                        }
                    }
                    None => break,
                },
                message = self.fs.recv(), if watching => match message {
                    Some(FsMsg::Changed) => deadline = Some(Instant::now() + self.debounce),
                    Some(FsMsg::Failed(message)) => {
                        self.fail(format!("watching the file failed: {message}"));
                    }
                    None => watching = false,
                },
                () = sleep_until(deadline) => {
                    deadline = None;
                    self.save().await;
                }
            }
        }
    }

    // ---- state ----

    fn state(&self) -> EditState {
        lock(&self.snapshot).state
    }

    fn update(&self, change: impl FnOnce(&mut EditSession)) {
        let session = {
            let mut session = lock(&self.snapshot);
            change(&mut session);
            session.clone()
        };
        // No subscriber is not an error.
        let _ = self.events.send(session);
    }

    fn current(&self) -> EditSession {
        lock(&self.snapshot).clone()
    }

    fn fail(&self, message: String) {
        tracing::warn!(
            target: "bucketbridge::edit",
            "editing {}: {message}",
            self.uri
        );
        self.update(|session| {
            session.state = EditState::Error;
            session.error = Some(message);
            session.conflict = None;
        });
    }

    // ---- saves ----

    /// Checks the copy and uploads it when it changed.
    async fn save(&mut self) {
        if self.state() == EditState::Conflict {
            // Held until the user decides; the file is read again then.
            return;
        }
        match self.detect().await {
            Ok(Detected::Missing) => {}
            Ok(Detected::Unchanged(fingerprint)) => self.last = fingerprint,
            Ok(Detected::Changed(fingerprint)) => {
                let _ = self.upload(fingerprint, Mode::Save).await;
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    async fn detect(&self) -> Result<Detected> {
        let (path, last) = (self.path.clone(), self.last);
        blocking(move || watch::detect(&path, &last)).await
    }

    async fn fingerprint(&self) -> Result<Fingerprint> {
        let path = self.path.clone();
        blocking(move || watch::fingerprint(&path)).await
    }

    /// Uploads the copy (`fingerprint` is what it holds) and records the
    /// outcome in the session.
    async fn upload(&mut self, fingerprint: Fingerprint, mode: Mode) -> Upload {
        self.update(|session| {
            session.state = EditState::Uploading;
            session.error = None;
        });
        match self.attempt(mode).await {
            Attempt::Done(etag) => {
                self.last = fingerprint;
                self.base_etag.clone_from(&etag);
                self.update(|session| {
                    session.state = EditState::Uploaded;
                    session.etag = etag;
                    session.uploads = session.uploads.saturating_add(1);
                    session.last_upload_at = Some(Utc::now());
                    session.error = None;
                    session.conflict = None;
                });
                Upload::Done
            }
            Attempt::Conflict(conflict) => {
                self.update(|session| {
                    session.state = EditState::Conflict;
                    session.error = None;
                    session.conflict = Some(conflict);
                });
                Upload::Conflict
            }
            Attempt::Failed(error) => {
                self.fail(error.to_string());
                Upload::Failed(error)
            }
        }
    }

    async fn attempt(&mut self, mode: Mode) -> Attempt {
        let s3 = match (self.provider)(self.current().account_id).await {
            Ok(s3) => s3,
            Err(error) => return Attempt::Failed(error),
        };
        let (bucket, key) = (self.uri.bucket(), self.uri.key());
        let head = match s3.head_object(bucket, key, None).await {
            Ok(head) => Some(head),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Attempt::Failed(error),
        };
        let remote_etag = head.as_ref().and_then(|head| head.etag.clone());
        if mode == Mode::Save {
            match &head {
                None => {
                    return Attempt::Conflict(EditConflict {
                        remote_deleted: true,
                        remote_etag: None,
                        remote_modified: None,
                    });
                }
                Some(head) if changed_since(self.base_etag.as_deref(), head.etag.as_deref()) => {
                    return Attempt::Conflict(conflict_of(head));
                }
                Some(_) => {}
            }
        }
        let tags = match &head {
            Some(head) => {
                let (headers, content_type) = header_parts(head);
                self.headers = headers;
                self.content_type = content_type;
                s3.get_object_tags(bucket, key).await.ok()
            }
            None => self.tags.clone(),
        };
        if let Some(tags) = &tags {
            self.tags = Some(tags.clone());
        }
        let condition = match (&head, remote_etag) {
            (None, _) => Some(WriteCondition::Absent),
            (Some(_), Some(etag)) if self.conditional => Some(WriteCondition::ETag(etag)),
            _ => None,
        };

        let uri = self.uri.clone();
        let mut result = self.put(&s3, &uri, condition.clone()).await;
        if let (Err(error), Some(_)) = (&result, &condition)
            && is_not_implemented(error)
        {
            // The server does not know conditional writes; the head check
            // above is the guard from now on.
            self.conditional = false;
            result = self.put(&s3, &uri, None).await;
        }
        match result {
            Ok(outcome) => {
                if let Some(tags) = tags.filter(|tags| !tags.is_empty())
                    && let Err(error) = s3.put_object_tags(bucket, key, &tags).await
                {
                    tracing::warn!(
                        target: "bucketbridge::edit",
                        "the tags of {} were not restored after the upload: {error}",
                        self.uri
                    );
                }
                let etag = match outcome.etag {
                    Some(etag) => Some(etag),
                    None => s3
                        .head_object(bucket, key, None)
                        .await
                        .ok()
                        .and_then(|head| head.etag),
                };
                Attempt::Done(etag)
            }
            Err(error) if is_precondition_failed(&error) => {
                let conflict = match s3.head_object(bucket, key, None).await {
                    Ok(head) => conflict_of(&head),
                    Err(_) => EditConflict {
                        remote_deleted: false,
                        remote_etag: None,
                        remote_modified: None,
                    },
                };
                Attempt::Conflict(conflict)
            }
            Err(error) => Attempt::Failed(error),
        }
    }

    /// Uploads the copy to `target`; an upload that fails leaves no
    /// multipart upload behind.
    async fn put(
        &self,
        s3: &S3,
        target: &S3Uri,
        condition: Option<WriteCondition>,
    ) -> Result<UploadOutcome> {
        // An edit always replaces the object it was opened from: the conflict
        // check above already decided that, so the transfer overwrite policy
        // (which may say ask or skip) must not apply here.
        let mut engine = TransferEngine::new(s3.clone(), self.transfer.clone())?
            .with_upload_headers(self.headers.clone())
            .with_if_exists(IfExists::Overwrite);
        if let Some(condition) = condition {
            engine = engine.with_write_condition(condition);
        }
        let path = self.path.clone();
        let source = blocking(move || FileSource::open(&path)).await?;
        let saved: Mutex<Option<UploadState>> = Mutex::new(None);
        let remember = |state: &UploadState| *lock(&saved) = Some(state.clone());
        let hooks = match &self.throttle {
            Some(throttle) => Hooks::new().with_throttle(Arc::clone(throttle)),
            None => Hooks::new(),
        };
        let outcome = engine
            .upload(
                Arc::new(source),
                target,
                Some(&self.content_type),
                &hooks,
                None,
                &remember,
            )
            .await;
        let result = match outcome {
            Ok(Outcome::Completed(outcome)) => return Ok(outcome),
            Ok(Outcome::Paused | Outcome::Cancelled) => Err(Error::Cancelled),
            // Not reachable with the overwrite policy forced above.
            Ok(Outcome::Skipped(reason)) => Err(Error::Internal(format!(
                "the edited copy was not uploaded: {}",
                reason.describe()
            ))),
            Err(error) => Err(error),
        };
        let leftover = lock(&saved).take();
        if let Some(state) = leftover {
            // Best effort: a multipart upload that stays is listed by the
            // multipart tool.
            let _ = engine.discard_upload(&state).await;
        }
        result
    }

    // ---- commands ----

    /// Handles a command; true when the session ended.
    async fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Stop { discard, reply } => {
                let result = self.stop(discard).await;
                let ended = result.is_ok();
                // The caller may have stopped waiting.
                let _ = reply.send(result);
                ended
            }
            Command::Resolve { resolution, reply } => {
                let result = self.resolve(resolution).await;
                let ended = matches!(&result, Ok(resolved) if resolved.session.is_none());
                let _ = reply.send(result);
                ended
            }
        }
    }

    async fn stop(&mut self, discard: bool) -> Result<()> {
        if !discard {
            if self.state() == EditState::Conflict {
                return Err(self.unresolved());
            }
            match self.detect().await {
                Ok(Detected::Missing) => {}
                Ok(Detected::Unchanged(fingerprint)) => self.last = fingerprint,
                Ok(Detected::Changed(fingerprint)) => {
                    match self.upload(fingerprint, Mode::Save).await {
                        Upload::Done => {}
                        Upload::Conflict => return Err(self.unresolved()),
                        Upload::Failed(error) => return Err(error),
                    }
                }
                Err(error) => {
                    self.fail(error.to_string());
                    return Err(error);
                }
            }
        }
        self.finish().await;
        Ok(())
    }

    fn unresolved(&self) -> Error {
        Error::Conflict(format!(
            "{} changed on the server since it was opened; overwrite it, save a copy or discard your changes first",
            self.uri
        ))
    }

    async fn resolve(&mut self, resolution: EditResolution) -> Result<EditResolved> {
        let state = self.state();
        match resolution {
            EditResolution::Discard => {
                if !matches!(state, EditState::Conflict | EditState::Error) {
                    return Err(Error::Usage(
                        "there is nothing to discard: stop the session instead".to_owned(),
                    ));
                }
                self.finish().await;
                Ok(EditResolved {
                    session: None,
                    copy_key: None,
                })
            }
            EditResolution::Overwrite | EditResolution::Retry => {
                let (needed, mode) = if resolution == EditResolution::Overwrite {
                    (EditState::Conflict, Mode::Overwrite)
                } else {
                    (EditState::Error, Mode::Save)
                };
                if state != needed {
                    return Err(Error::Usage(format!(
                        "the session is not in the {} state",
                        if needed == EditState::Conflict {
                            "conflict"
                        } else {
                            "error"
                        }
                    )));
                }
                let fingerprint = self.fingerprint().await?;
                match self.upload(fingerprint, mode).await {
                    Upload::Done => Ok(EditResolved {
                        session: Some(self.current()),
                        copy_key: None,
                    }),
                    Upload::Conflict => Err(self.unresolved()),
                    Upload::Failed(error) => Err(error),
                }
            }
            EditResolution::SaveCopy => {
                if state != EditState::Conflict {
                    return Err(Error::Usage("there is no conflict to resolve".to_owned()));
                }
                let copy_key = self.save_copy().await?;
                self.finish().await;
                Ok(EditResolved {
                    session: None,
                    copy_key: Some(copy_key),
                })
            }
        }
    }

    /// Uploads the copy under `name (edited).ext`, the first such key that
    /// does not exist yet. Returns the key.
    async fn save_copy(&self) -> Result<String> {
        let s3 = (self.provider)(self.current().account_id).await?;
        let bucket = self.uri.bucket();
        for n in 1..=MAX_COPY_NAMES {
            let key = super::copy_key(self.uri.key(), n);
            match s3.head_object(bucket, &key, None).await {
                Ok(_) => continue,
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let target = S3Uri::new(bucket, &key)?;
            let mut result = self.put(&s3, &target, Some(WriteCondition::Absent)).await;
            if matches!(&result, Err(error) if is_not_implemented(error)) {
                result = self.put(&s3, &target, None).await;
            }
            match result {
                Ok(_) => return Ok(key),
                // Created by somebody meanwhile: the next name.
                Err(error) if is_precondition_failed(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::Conflict(format!(
            "no free name for a copy of {} (tried {MAX_COPY_NAMES})",
            self.uri
        )))
    }

    /// Ends the session: stops watching, deletes the copy, leaves the list.
    async fn finish(&mut self) {
        self.guard = None;
        remove_dir(self.dir.clone()).await;
        if let Some(registry) = self.registry.upgrade() {
            let id = self.current().id;
            registry.remove(&id);
        }
        self.update(|session| {
            session.state = EditState::Stopped;
            session.error = None;
            session.conflict = None;
        });
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The object is not the one the session saw: another ETag (a missing one on
/// either side cannot be compared).
fn changed_since(base: Option<&str>, remote: Option<&str>) -> bool {
    match (base, remote) {
        (Some(base), Some(remote)) => base.trim_matches('"') != remote.trim_matches('"'),
        _ => false,
    }
}

fn conflict_of(head: &ObjectHead) -> EditConflict {
    EditConflict {
        remote_deleted: false,
        remote_etag: head.etag.clone(),
        remote_modified: head.last_modified,
    }
}

/// The server refused a conditional write because the object is not what was
/// expected.
fn is_precondition_failed(error: &Error) -> bool {
    error.service().is_some_and(|service| {
        service.status() == Some(412)
            || matches!(
                service.code(),
                Some("PreconditionFailed" | "ConditionalRequestConflict")
            )
    })
}

fn is_not_implemented(error: &Error) -> bool {
    error
        .service()
        .is_some_and(|service| service.is_not_implemented())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etags_compare_without_quotes() {
        assert!(!changed_since(Some("abc"), Some("\"abc\"")));
        assert!(changed_since(Some("abc"), Some("def")));
        assert!(!changed_since(None, Some("def")));
        assert!(!changed_since(Some("abc"), None));
    }

    #[test]
    fn headers_of_an_object_are_kept() {
        let mut head = ObjectHead {
            key: "docs/a.txt".to_owned(),
            storage_class: Some("STANDARD_IA".to_owned()),
            ..ObjectHead::default()
        };
        head.meta.content_type = Some("text/x-custom".to_owned());
        head.meta.cache_control = Some("max-age=60".to_owned());
        head.meta
            .user_meta
            .insert("owner".to_owned(), "x".to_owned());
        let (headers, content_type) = header_parts(&head);
        assert_eq!(content_type, "text/x-custom");
        assert_eq!(headers.cache_control.as_deref(), Some("max-age=60"));
        assert_eq!(headers.metadata.get("owner").map(String::as_str), Some("x"));
        assert_eq!(headers.storage_class.as_deref(), Some("STANDARD_IA"));

        head.meta.content_type = None;
        head.storage_class = Some("STANDARD".to_owned());
        let (headers, content_type) = header_parts(&head);
        assert_eq!(content_type, "text/plain");
        assert_eq!(headers.storage_class, None);
    }
}
