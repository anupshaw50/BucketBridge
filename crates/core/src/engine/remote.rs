//! Server-side engines: copy, move and delete.

use std::sync::atomic::{AtomicU64, Ordering};

use futures::TryStreamExt;

use super::TransferEngine;
use super::control::{Hooks, Outcome};
use super::policy::{ItemInfo, Placed, object_time};
use crate::error::{Error, ErrorKind, Result, classify};
use crate::model::{CopyOutcome, DeleteOutcome, TransferKind};
use crate::ops::MAX_DELETE_BATCH;
use crate::s3uri::{S3Uri, validate_key};
use crate::settings::IfExists;

/// Failed keys spelled out in a delete error; the rest are counted.
const FAILURES_SHOWN: usize = 5;

impl TransferEngine {
    /// Copies one object on the server (`CopyObject`, or `UploadPartCopy`
    /// parts for large objects; see [`crate::ops::S3::copy_object_with`]),
    /// keeping its content headers and metadata. Progress counts bytes.
    ///
    /// A key ending in `/` that is a folder marker (an empty object, or no
    /// object at all) creates the destination marker. S3 allows data under
    /// such a key; an object that holds data is no marker and is copied like
    /// any other object, so that a move never replaces it with an empty
    /// marker. A copy cannot pause half-way: a pause or cancel stops it (an
    /// unfinished multipart copy is aborted) and a resumed task copies
    /// again.
    pub async fn copy(
        &self,
        src: &S3Uri,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Outcome<CopyOutcome>> {
        validate_key(src.key()).map_err(|err| err.with_context("copy source"))?;
        validate_key(dst.key()).map_err(|err| err.with_context("copy destination"))?;
        if src == dst {
            return Err(Error::Usage(format!("cannot copy {src} onto itself")));
        }
        if let Some(reason) = hooks.stop.reason() {
            return Ok(Outcome::stopped(Some(reason)));
        }
        if src.key().ends_with('/') {
            if !dst.key().ends_with('/') {
                return Err(Error::Usage(format!(
                    "{src} is a folder; its destination {dst} must end with '/'"
                )));
            }
            let marker = tokio::select! {
                biased;
                () = hooks.stop.stopped() => return Ok(Outcome::stopped(hooks.stop.reason())),
                marker = self.is_folder_marker(src) => marker?,
            };
            if marker {
                let outcome = self.create_folder_marker(dst, hooks).await?;
                return Ok(outcome.map(|()| CopyOutcome::default()));
            }
        }
        let work = async {
            let head = self.s3.head_object(src.bucket(), src.key(), None).await?;
            let mut target = dst.clone();
            if self.options.if_exists != IfExists::Overwrite {
                let info = ItemInfo {
                    size: Some(head.size),
                    modified: object_time(&head),
                };
                match self
                    .place_remote(
                        &self.s3,
                        TransferKind::Copy,
                        src.to_string(),
                        info,
                        dst,
                        hooks,
                    )
                    .await?
                {
                    Placed::Go(placed) => target = placed,
                    Placed::Skip(reason) => return Ok(Outcome::Skipped(reason)),
                    Placed::Stop(reason) => return Ok(Outcome::stopped(reason)),
                }
            }
            hooks.progress.set_total(head.size);
            hooks.progress.set_done(0);
            let reported = AtomicU64::new(0);
            let mut outcome = self
                .s3
                .copy_object_with(src, &target, &self.options.copy, |copied| {
                    let before = reported.fetch_max(copied, Ordering::AcqRel);
                    if copied > before {
                        hooks.progress.add_done(copied - before);
                    }
                })
                .await?;
            // The source may have changed between the HEAD and the copy.
            hooks.progress.set_total(outcome.size);
            hooks.progress.set_done(outcome.size);
            if target != *dst {
                outcome.renamed_to = Some(target.key().to_owned());
            }
            Ok::<_, Error>(Outcome::Completed(outcome))
        };
        tokio::select! {
            biased;
            () = hooks.stop.stopped() => Ok(Outcome::stopped(hooks.stop.reason())),
            copied = work => copied,
        }
    }

    /// Whether the object `src`, whose key ends in `/`, is a plain folder
    /// marker: empty, or not there at all (a folder that exists only as a
    /// prefix is recreated as a marker). Data under the key makes it an
    /// ordinary object.
    pub(super) async fn is_folder_marker(&self, src: &S3Uri) -> Result<bool> {
        match self.s3.head_object(src.bucket(), src.key(), None).await {
            Ok(head) => Ok(head.size == 0),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
            Err(err) => Err(err),
        }
    }

    /// Moves one object: copies it (see [`TransferEngine::copy`]) and
    /// deletes the source only after the copy succeeded.
    ///
    /// Once the copy exists the delete is always attempted, even if a stop
    /// arrives meanwhile. If it fails, the error says that the copy exists
    /// and the source was left in place; running the move again is safe.
    pub async fn move_object(
        &self,
        src: &S3Uri,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Outcome<CopyOutcome>> {
        let copied = match self.copy(src, dst, hooks).await? {
            Outcome::Completed(copied) => copied,
            Outcome::Paused => return Ok(Outcome::Paused),
            Outcome::Cancelled => return Ok(Outcome::Cancelled),
            // Nothing was copied: the source stays where it is.
            Outcome::Skipped(reason) => return Ok(Outcome::Skipped(reason)),
        };
        let landed = match &copied.renamed_to {
            Some(key) => S3Uri::new(dst.bucket(), key.clone())?,
            None => dst.clone(),
        };
        self.s3
            .delete_object(src.bucket(), src.key(), None)
            .await
            .map_err(|err| {
                err.with_context(format!(
                    "moving {src} to {landed}: the copy {landed} exists, but the source could not be deleted and is still there"
                ))
            })?;
        Ok(Outcome::Completed(copied))
    }

    /// Deletes one object, or every object under a prefix (a key ending in
    /// `/`, including its folder marker), 1000 keys per request. Progress
    /// counts objects; the total grows while the prefix is listed.
    ///
    /// Stopping between requests is safe: a resumed delete lists again and
    /// continues with what is left. Keys the server refuses to delete fail
    /// the run after every other key was handled; the error names them.
    pub async fn delete(&self, target: &S3Uri, hooks: &Hooks) -> Result<Outcome<DeleteOutcome>> {
        let key = target.key();
        if key.trim_end_matches('/').is_empty() {
            return Err(Error::Usage(format!(
                "refusing to delete {target}: it names no object or folder"
            )));
        }
        validate_key(key).map_err(|err| err.with_context(target))?;
        if let Some(reason) = hooks.stop.reason() {
            return Ok(Outcome::stopped(Some(reason)));
        }
        if !key.ends_with('/') {
            hooks.progress.set_total(1);
            hooks.progress.set_done(0);
            tokio::select! {
                biased;
                () = hooks.stop.stopped() => return Ok(Outcome::stopped(hooks.stop.reason())),
                deleted = self.s3.delete_object(target.bucket(), key, None) => deleted?,
            }
            hooks.progress.add_done(1);
            return Ok(Outcome::Completed(DeleteOutcome {
                deleted: 1,
                failures: Vec::new(),
            }));
        }

        hooks.progress.set_done(0);
        let mut outcome = DeleteOutcome::default();
        let mut listed = 0u64;
        let mut batch: Vec<String> = Vec::with_capacity(MAX_DELETE_BATCH);
        let mut entries = Box::pin(self.s3.list_all_stream(target.bucket(), key));
        loop {
            let next = tokio::select! {
                biased;
                () = hooks.stop.stopped() => return Ok(Outcome::stopped(hooks.stop.reason())),
                next = entries.try_next() => next?,
            };
            let finished = match next {
                Some(entry) => {
                    batch.push(entry.key);
                    listed += 1;
                    false
                }
                None => true,
            };
            if batch.len() == MAX_DELETE_BATCH || (finished && !batch.is_empty()) {
                hooks.progress.set_total(listed);
                let keys = std::mem::take(&mut batch);
                let done = tokio::select! {
                    biased;
                    () = hooks.stop.stopped() => return Ok(Outcome::stopped(hooks.stop.reason())),
                    done = self.s3.delete_objects(target.bucket(), &keys) => done?,
                };
                hooks.progress.add_done(done.deleted);
                outcome.deleted += done.deleted;
                outcome.failures.extend(done.failures);
            }
            if finished {
                break;
            }
        }
        hooks.progress.set_total(listed);
        if outcome.failures.is_empty() {
            Ok(Outcome::Completed(outcome))
        } else {
            Err(delete_failures(target, &outcome))
        }
    }
}

/// The error for keys a delete could not remove; its kind follows the
/// first failure's code (`AccessDenied` is an auth error, for example).
fn delete_failures(target: &S3Uri, outcome: &DeleteOutcome) -> Error {
    let failed = outcome.failures.len();
    let shown: Vec<String> = outcome
        .failures
        .iter()
        .take(FAILURES_SHOWN)
        .map(|failure| match &failure.code {
            Some(code) => format!("{} ({code}: {})", failure.key, failure.message),
            None => format!("{} ({})", failure.key, failure.message),
        })
        .collect();
    let mut message = format!(
        "deleting {target}: {failed} of {} objects could not be deleted: {}",
        outcome.deleted + failed as u64,
        shown.join("; ")
    );
    if failed > FAILURES_SHOWN {
        message.push_str(&format!("; and {} more", failed - FAILURES_SHOWN));
    }
    let code = outcome.failures.first().and_then(|f| f.code.as_deref());
    match classify(code, None) {
        ErrorKind::Auth => Error::Auth(message),
        ErrorKind::NotFound => Error::NotFound(message),
        ErrorKind::Conflict => Error::Conflict(message),
        _ => Error::Remote(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeleteFailure;

    #[test]
    fn delete_failures_are_summarised() {
        let failure = |key: &str, code: Option<&str>| DeleteFailure {
            key: key.into(),
            version_id: None,
            code: code.map(str::to_owned),
            message: "nope".into(),
        };
        let target = S3Uri::new("bucket", "logs/").unwrap();
        let outcome = DeleteOutcome {
            deleted: 3,
            failures: vec![
                failure("logs/a", Some("AccessDenied")),
                failure("logs/b", None),
            ],
        };
        let err = delete_failures(&target, &outcome);
        assert_eq!(err.kind(), ErrorKind::Auth);
        let text = err.to_string();
        assert!(text.contains("2 of 5 objects"), "{text}");
        assert!(text.contains("logs/a (AccessDenied: nope)"), "{text}");
        assert!(text.contains("logs/b (nope)"), "{text}");

        let many = DeleteOutcome {
            deleted: 0,
            failures: (0..8)
                .map(|i| failure(&format!("k{i}"), Some("InternalError")))
                .collect(),
        };
        let err = delete_failures(&target, &many);
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert!(err.to_string().ends_with("; and 3 more"), "{err}");
    }
}
