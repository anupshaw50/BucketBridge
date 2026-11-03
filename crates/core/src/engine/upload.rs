//! Upload engine: one local file (or any [`UploadSource`]) to one object.
//!
//! * Below [`TransferOptions::multipart_threshold`](super::TransferOptions)
//!   bytes: one `PutObject`.
//! * At or above it: a multipart upload with parts of
//!   [`part_size_for`]`(size, part_size)` bytes, up to
//!   `max_parts_in_flight` at a time. The upload ID is checkpointed as soon
//!   as it exists and the state again after every finished part.
//!
//! With a checksum algorithm configured, the checksum of every request body
//! (the whole file, or each part) is computed from the source before the
//! request is sent and sent as its `x-amz-checksum-*` header; the server
//! rejects a body that does not match, so a file that changes while it is
//! read can never be stored silently. The multipart upload is created with
//! the algorithm and every part's checksum goes into
//! `CompleteMultipartUpload`.
//!
//! With the checksum set to `none` nothing protects a single `PutObject`
//! that way, so the source is checked again after the request (a multipart
//! upload is checked before it is completed, whatever the checksum): if the
//! file changed meanwhile, the run fails and the retry uploads it again,
//! replacing the object that was stored from the changing file.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Arc;

use aws_sdk_s3::operation::complete_multipart_upload::CompleteMultipartUploadOutput;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::body::{SentCounter, range_body};
use super::checksum::{Checksummer, checksum_field, sdk_algorithm, set_checksum};
use super::content_type::guess_content_type;
use super::control::{Hooks, Outcome, StopReason};
use super::policy::{ItemInfo, MTIME_META_KEY, Placed, format_mtime};
use super::source::{FileSource, SourceFingerprint, UploadSource};
use super::{
    TransferEngine, WriteCondition, blocking, bucket_client, require_object_key, to_i64, unquote,
    without_request_checksums,
};
use crate::error::{Error, ErrorKind, Result};
use crate::model::TransferKind;
use crate::ops::{MAX_PART_SIZE, MAX_PARTS, MIN_PART_SIZE, part_size_for};
use crate::s3uri::S3Uri;
use crate::settings::{ChecksumAlgorithm, IfExists};

/// Bytes read at a time when computing a checksum before sending.
const HASH_CHUNK: u64 = 1024 * 1024;

/// How often an abort is repeated while parts that were in flight show up
/// after it.
const ABORT_ATTEMPTS: usize = 3;

/// Resume state of a multipart upload, emitted through the checkpoint
/// callback (stored by the queue as JSON). Single-request uploads have no
/// resume state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadState {
    /// Destination bucket.
    pub bucket: String,
    /// Destination key.
    pub key: String,
    /// The multipart upload.
    pub upload_id: String,
    /// The source content the parts were read from.
    pub source: SourceFingerprint,
    /// Part size in bytes (every part but the last).
    pub part_size: u64,
    /// The algorithm the multipart upload was created with.
    pub checksum_algorithm: ChecksumAlgorithm,
    /// Finished parts, by part number.
    pub parts: Vec<UploadedPart>,
}

/// A part the server has stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedPart {
    /// Part number, from 1.
    pub number: u32,
    /// Size in bytes.
    pub size: u64,
    /// ETag exactly as the server returned it.
    pub etag: String,
    /// Base64 checksum of the part, for the upload's algorithm.
    pub checksum: Option<String>,
}

/// Result of a finished upload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UploadOutcome {
    /// Bytes uploaded (the object size).
    pub size: u64,
    /// ETag of the new object, without quotes.
    pub etag: Option<String>,
    /// Number of parts; 0 for a single-request upload or a folder marker.
    pub parts: u32,
    /// Parts reused from an earlier run.
    pub reused_parts: u32,
    /// The object's checksum for the configured algorithm, as the server
    /// reported it (multipart objects may carry a composite `-N` value).
    pub checksum: Option<String>,
    /// The key the object was stored under when the destination existed and
    /// the policy renamed the upload (`name (1).ext`); `None` otherwise.
    #[serde(default)]
    pub renamed_to: Option<String>,
}

/// A part listed by `ListParts`.
struct ListedPart {
    etag: String,
    size: u64,
    checksum: Option<String>,
}

/// What every part of one multipart upload shares.
struct PartPlan<'a> {
    dst: &'a S3Uri,
    upload_id: &'a str,
    part_size: u64,
    algorithm: ChecksumAlgorithm,
}

enum HashError {
    Stopped,
    Io(io::Error),
}

impl TransferEngine {
    /// Uploads the local file `path` to `dst`.
    ///
    /// A key ending in `/` creates a folder marker instead (the local path
    /// is not read). The content type is guessed from the file extension.
    /// `resume` is the last checkpoint of an earlier run of the same task;
    /// it is used when the file is unchanged (same size and modification
    /// time) and the multipart upload still exists, and discarded otherwise.
    /// `checkpoint` must return quickly without blocking.
    pub async fn upload_file(
        &self,
        path: &Path,
        dst: &S3Uri,
        hooks: &Hooks,
        resume: Option<UploadState>,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<UploadOutcome>> {
        if dst.key().ends_with('/') {
            let outcome = self.create_folder_marker(dst, hooks).await?;
            return Ok(outcome.map(|()| UploadOutcome::default()));
        }
        require_object_key(dst)?;
        if let Some(outcome) = self.stop_before_start(hooks, resume.as_ref()).await? {
            return Ok(outcome);
        }
        let owned = path.to_path_buf();
        let source = blocking(move || FileSource::open(&owned)).await??;
        let content_type = guess_content_type(path);
        self.upload(
            Arc::new(source),
            dst,
            Some(content_type),
            hooks,
            resume,
            checkpoint,
        )
        .await
    }

    /// Uploads `source` to `dst` (see [`TransferEngine::upload_file`]).
    ///
    /// When `dst` exists the overwrite policy decides
    /// ([`TransferEngine::with_if_exists`]): a skipped upload ends with
    /// [`Outcome::Skipped`] and a renamed one reports the key it used in
    /// [`UploadOutcome::renamed_to`]. With `preserve_mtime` the source's
    /// modification time is stored in the `mtime` user metadata.
    pub async fn upload(
        &self,
        source: Arc<dyn UploadSource>,
        dst: &S3Uri,
        content_type: Option<&str>,
        hooks: &Hooks,
        resume: Option<UploadState>,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<UploadOutcome>> {
        require_object_key(dst)?;
        let mut target = dst.clone();
        let policy = self.options.if_exists;
        // A resumed upload was placed by the run that created it (a rename
        // is stable: the name it picked is still free).
        let decide = policy != IfExists::Overwrite
            && (resume.is_none() || policy == IfExists::Rename)
            && hooks.stop.reason().is_none();
        if decide {
            let info = ItemInfo {
                size: Some(source.size()),
                modified: source.fingerprint().modified,
            };
            match self
                .place_remote(
                    &self.s3,
                    TransferKind::Upload,
                    source.describe(),
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
        let engine = self.with_source_mtime(source.fingerprint().modified);
        let mut outcome = engine
            .upload_inner(source, &target, content_type, hooks, resume, checkpoint)
            .await?;
        if target != *dst
            && let Outcome::Completed(done) = &mut outcome
        {
            done.renamed_to = Some(target.key().to_owned());
        }
        Ok(outcome)
    }

    /// These engines with the `mtime` user metadata set to `modified`, when
    /// modification times are kept and the caller set none itself.
    fn with_source_mtime(&self, modified: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        let Some(modified) = modified.filter(|_| self.options.preserve_mtime) else {
            return self.clone();
        };
        let mut headers = self.upload_headers.clone().unwrap_or_default();
        headers
            .metadata
            .entry(MTIME_META_KEY.to_owned())
            .or_insert_with(|| format_mtime(modified));
        self.with_upload_headers(headers)
    }

    async fn upload_inner(
        &self,
        source: Arc<dyn UploadSource>,
        dst: &S3Uri,
        content_type: Option<&str>,
        hooks: &Hooks,
        resume: Option<UploadState>,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<UploadOutcome>> {
        if let Some(outcome) = self.stop_before_start(hooks, resume.as_ref()).await? {
            return Ok(outcome);
        }
        let size = source.size();
        hooks.progress.set_total(size);
        hooks.progress.set_done(0);
        if let Some(saved) = resume
            && let Some(state) = self.resumable(source.as_ref(), dst, saved).await?
        {
            return self
                .run_multipart(source, dst, hooks, state, checkpoint)
                .await;
        }
        if size < self.options.multipart_threshold {
            return self.put_single(source, dst, content_type, hooks).await;
        }
        let part_size = part_size_for(size, self.options.part_size)?;
        let algorithm = self.options.checksum;
        // Not raced with the stop signal: an upload created on the server
        // but unknown here could never be aborted.
        let upload_id = self.create_upload(dst, content_type, algorithm).await?;
        let state = UploadState {
            bucket: dst.bucket().to_owned(),
            key: dst.key().to_owned(),
            upload_id,
            source: source.fingerprint(),
            part_size,
            checksum_algorithm: algorithm,
            parts: Vec::new(),
        };
        checkpoint(&state);
        self.run_multipart(source, dst, hooks, state, checkpoint)
            .await
    }

    /// Aborts the multipart upload of a saved resume state: for a stopped
    /// task that is cancelled, or a failed task that is given up. An upload
    /// that no longer exists counts as discarded, so this may be repeated.
    pub async fn discard_upload(&self, state: &UploadState) -> Result<()> {
        let context = || {
            format!(
                "aborting the multipart upload to s3://{}/{}",
                state.bucket, state.key
            )
        };
        // S3 may still store parts whose upload was in progress during an
        // abort; abort again until none is left.
        for _ in 0..ABORT_ATTEMPTS {
            match self
                .s3
                .abort_multipart_upload(&state.bucket, &state.key, &state.upload_id)
                .await
            {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err.with_context(context())),
            }
            match self
                .list_parts(
                    &state.bucket,
                    &state.key,
                    &state.upload_id,
                    ChecksumAlgorithm::None,
                )
                .await
            {
                Ok(parts) if !parts.is_empty() => {}
                _ => return Ok(()),
            }
        }
        Err(Error::Remote(format!(
            "{}: parts were still listed after {ABORT_ATTEMPTS} aborts",
            context()
        )))
    }

    /// Creates a folder marker (a zero-byte object whose key ends in `/`).
    pub(crate) async fn create_folder_marker(
        &self,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Outcome<()>> {
        if let Some(reason) = hooks.stop.reason() {
            return Ok(Outcome::stopped(Some(reason)));
        }
        hooks.progress.set_total(0);
        hooks.progress.set_done(0);
        tokio::select! {
            biased;
            () = hooks.stop.stopped() => Ok(Outcome::stopped(hooks.stop.reason())),
            created = self.s3.create_folder(dst.bucket(), dst.key()) => {
                created.map(Outcome::Completed)
            }
        }
    }

    /// Handles a stop requested before the upload started.
    async fn stop_before_start(
        &self,
        hooks: &Hooks,
        resume: Option<&UploadState>,
    ) -> Result<Option<Outcome<UploadOutcome>>> {
        match hooks.stop.reason() {
            None => Ok(None),
            Some(StopReason::Pause) => Ok(Some(Outcome::Paused)),
            Some(StopReason::Cancel) => {
                if let Some(state) = resume {
                    self.discard_upload(state).await?;
                }
                Ok(Some(Outcome::Cancelled))
            }
        }
    }

    /// Decides whether a saved state can continue, and which of its parts
    /// the server really has. `None` means: start a new upload.
    async fn resumable(
        &self,
        source: &dyn UploadSource,
        dst: &S3Uri,
        saved: UploadState,
    ) -> Result<Option<UploadState>> {
        if saved.bucket != dst.bucket() || saved.key != dst.key() || saved.upload_id.is_empty() {
            // Not a state of this upload; leave its multipart upload alone.
            return Ok(None);
        }
        let size = source.size();
        if saved.source != source.fingerprint() || !valid_plan(size, saved.part_size) {
            // The parts can never form this content: free them.
            let _ = self.discard_upload(&saved).await;
            return Ok(None);
        }
        let listed = match self
            .list_parts(
                &saved.bucket,
                &saved.key,
                &saved.upload_id,
                saved.checksum_algorithm,
            )
            .await
        {
            Ok(listed) => listed,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        // The server's list is the truth: it may hold parts finished after
        // the last checkpoint, and it never lists a part that failed.
        let mut parts = Vec::with_capacity(listed.len());
        for (number, listed) in listed {
            let Some(expected) = part_len(size, saved.part_size, number) else {
                continue;
            };
            if listed.size != expected {
                continue;
            }
            let checksum = listed.checksum.or_else(|| {
                saved
                    .parts
                    .iter()
                    .find(|p| p.number == number && unquote(&p.etag) == unquote(&listed.etag))
                    .and_then(|p| p.checksum.clone())
            });
            if saved.checksum_algorithm != ChecksumAlgorithm::None && checksum.is_none() {
                // CompleteMultipartUpload needs it; upload the part again.
                continue;
            }
            parts.push(UploadedPart {
                number,
                size: expected,
                etag: listed.etag,
                checksum,
            });
        }
        Ok(Some(UploadState { parts, ..saved }))
    }

    async fn run_multipart(
        &self,
        source: Arc<dyn UploadSource>,
        dst: &S3Uri,
        hooks: &Hooks,
        mut state: UploadState,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<UploadOutcome>> {
        let size = source.size();
        let count = part_count(size, state.part_size)?;
        state.parts.retain(|p| (1..=count).contains(&p.number));
        state.parts.sort_by_key(|p| p.number);
        state.parts.dedup_by_key(|p| p.number);
        let reused = u32::try_from(state.parts.len()).unwrap_or(u32::MAX);
        hooks.progress.set_total(size);
        hooks
            .progress
            .set_done(state.parts.iter().map(|p| p.size).sum());
        checkpoint(&state);

        let finished: HashSet<u32> = state.parts.iter().map(|p| p.number).collect();
        let todo: Vec<u32> = (1..=count).filter(|n| !finished.contains(n)).collect();
        let upload_id = state.upload_id.clone();
        let plan = PartPlan {
            dst,
            upload_id: &upload_id,
            part_size: state.part_size,
            algorithm: state.checksum_algorithm,
        };
        let mut uploads = Box::pin(
            futures::stream::iter(todo)
                .map(|number| self.upload_part(Arc::clone(&source), &plan, number, hooks))
                .buffer_unordered(self.options.max_parts_in_flight),
        );
        loop {
            let next = tokio::select! {
                biased;
                () = hooks.stop.stopped() => None,
                next = uploads.next() => Some(next),
            };
            match next {
                None => {
                    // Dropping the stream cancels the parts in flight.
                    drop(uploads);
                    return self
                        .stop_multipart(hooks.stop.reason(), &state, checkpoint)
                        .await;
                }
                Some(Some(Ok(part))) => {
                    let at = state.parts.partition_point(|p| p.number < part.number);
                    state.parts.insert(at, part);
                    checkpoint(&state);
                }
                Some(Some(Err(err))) => {
                    drop(uploads);
                    if let Some(reason) = hooks.stop.reason() {
                        return self.stop_multipart(Some(reason), &state, checkpoint).await;
                    }
                    return Err(err);
                }
                Some(None) => break,
            }
        }
        drop(uploads);

        let description = source.describe();
        let unchanged = {
            let source = Arc::clone(&source);
            blocking(move || source.is_unchanged()).await?
        };
        match unchanged {
            Ok(true) => {}
            Ok(false) => {
                let _ = self.discard_upload(&state).await;
                return Err(Error::io(
                    format!("uploading {description} to {dst}"),
                    io::Error::other(
                        "the file changed while it was being uploaded; the incomplete upload was discarded",
                    ),
                ));
            }
            Err(err) => {
                return Err(Error::io(
                    format!("checking {description} after uploading it"),
                    err,
                ));
            }
        }

        // The last moment a stop is honoured. CompleteMultipartUpload is
        // never raced with the stop signal: once sent, the object exists or
        // will exist.
        if let Some(reason) = hooks.stop.reason() {
            return self.stop_multipart(Some(reason), &state, checkpoint).await;
        }
        let out = self.complete_upload(dst, &state).await?;
        Ok(Outcome::Completed(UploadOutcome {
            size,
            etag: out.e_tag().map(|e| unquote(e).to_owned()),
            parts: count,
            reused_parts: reused,
            checksum: checksum_field!(out, state.checksum_algorithm).map(str::to_owned),
            renamed_to: None,
        }))
    }

    async fn stop_multipart(
        &self,
        reason: Option<StopReason>,
        state: &UploadState,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<UploadOutcome>> {
        match reason {
            Some(StopReason::Cancel) => {
                self.discard_upload(state).await.map_err(|err| {
                    err.with_context(
                        "the upload was cancelled, but its multipart upload could not be aborted",
                    )
                })?;
                Ok(Outcome::Cancelled)
            }
            Some(StopReason::Pause) | None => {
                checkpoint(state);
                Ok(Outcome::Paused)
            }
        }
    }

    async fn upload_part(
        &self,
        source: Arc<dyn UploadSource>,
        plan: &PartPlan<'_>,
        number: u32,
        hooks: &Hooks,
    ) -> Result<UploadedPart> {
        let context = || format!("uploading part {number} of {}", plan.dst);
        let len = part_len(source.size(), plan.part_size, number)
            .ok_or_else(|| Error::Internal(format!("{}: no such part", context())))?;
        let offset = u64::from(number - 1) * plan.part_size;
        let checksum = checksum_range(
            Arc::clone(&source),
            offset,
            len,
            plan.algorithm,
            hooks.stop.token().clone(),
        )
        .await?;
        let counter = SentCounter::new(Arc::clone(&hooks.progress));
        let body = range_body(
            source,
            offset,
            len,
            Arc::clone(&hooks.throttle),
            Arc::clone(&counter),
        );
        let client = bucket_client(&self.s3, plan.dst.bucket()).await?;
        let request = client
            .upload_part()
            .bucket(plan.dst.bucket())
            .key(plan.dst.key())
            .upload_id(plan.upload_id)
            .part_number(part_number(number))
            .content_length(to_i64(len, "the part")?)
            .body(body)
            .set_request_payer(self.s3.request_payer());
        let request = set_checksum!(request, plan.algorithm, checksum.clone());
        // One of the few bodies a low bandwidth cap can keep alive at once
        // (see `Throttle::open_stream`); held until the request is answered.
        let _stream = hooks.open_stream().await;
        let sent = if plan.algorithm == ChecksumAlgorithm::None {
            request
                .customize()
                .config_override(without_request_checksums())
                .send()
                .await
        } else {
            request.send().await
        };
        let out = sent.map_err(|err| Error::from(err).with_context(context()))?;
        let etag = out
            .e_tag()
            .filter(|etag| !etag.is_empty())
            .ok_or_else(|| Error::Remote(format!("{}: the server sent no ETag", context())))?
            .to_owned();
        counter.finish(len);
        Ok(UploadedPart {
            number,
            size: len,
            etag,
            checksum,
        })
    }

    async fn put_single(
        &self,
        source: Arc<dyn UploadSource>,
        dst: &S3Uri,
        content_type: Option<&str>,
        hooks: &Hooks,
    ) -> Result<Outcome<UploadOutcome>> {
        let size = source.size();
        let algorithm = self.options.checksum;
        let description = source.describe();
        let work = async {
            let checksum = checksum_range(
                Arc::clone(&source),
                0,
                size,
                algorithm,
                hooks.stop.token().clone(),
            )
            .await?;
            let counter = SentCounter::new(Arc::clone(&hooks.progress));
            let body = range_body(
                Arc::clone(&source),
                0,
                size,
                Arc::clone(&hooks.throttle),
                Arc::clone(&counter),
            );
            let client = bucket_client(&self.s3, dst.bucket()).await?;
            let request = client
                .put_object()
                .bucket(dst.bucket())
                .key(dst.key())
                .content_length(to_i64(size, "the file")?)
                .body(body)
                .set_content_type(content_type.map(str::to_owned))
                .set_metadata(self.upload_headers.as_ref().map(|h| h.metadata.clone()))
                .set_cache_control(
                    self.upload_headers
                        .as_ref()
                        .and_then(|h| h.cache_control.clone()),
                )
                .set_content_disposition(
                    self.upload_headers
                        .as_ref()
                        .and_then(|h| h.content_disposition.clone()),
                )
                .set_content_language(
                    self.upload_headers
                        .as_ref()
                        .and_then(|h| h.content_language.clone()),
                )
                .set_content_encoding(
                    self.upload_headers
                        .as_ref()
                        .and_then(|h| h.content_encoding.clone()),
                )
                .set_expires(self.upload_headers.as_ref().and_then(|h| h.expires))
                .set_storage_class(self.storage_class())
                .set_if_match(
                    self.write_condition
                        .as_ref()
                        .and_then(WriteCondition::if_match),
                )
                .set_if_none_match(
                    self.write_condition
                        .as_ref()
                        .and_then(WriteCondition::if_none_match),
                )
                .set_request_payer(self.s3.request_payer());
            let request = set_checksum!(request, algorithm, checksum.clone());
            let _stream = hooks.open_stream().await;
            let sent = if algorithm == ChecksumAlgorithm::None {
                request
                    .customize()
                    .config_override(without_request_checksums())
                    .send()
                    .await
            } else {
                request.send().await
            };
            let out = sent.map_err(|err| {
                Error::from(err).with_context(format!("uploading {description} to {dst}"))
            })?;
            counter.finish(size);
            if algorithm == ChecksumAlgorithm::None {
                // Without a checksum, nothing makes the server notice a body
                // that mixes old and new content, and the object is stored
                // by now. Failing the run makes the queue upload the file
                // again, which replaces it. (With a checksum the server
                // refused such a body.)
                let unchanged = {
                    let source = Arc::clone(&source);
                    blocking(move || source.is_unchanged()).await?
                };
                match unchanged {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(Error::io(
                            format!("uploading {description} to {dst}"),
                            io::Error::other(
                                "the file changed while it was being uploaded, so the stored object may mix old and new content; uploading it again replaces it",
                            ),
                        ));
                    }
                    Err(err) => {
                        return Err(Error::io(
                            format!("checking {description} after uploading it"),
                            err,
                        ));
                    }
                }
            }
            Ok::<_, Error>(UploadOutcome {
                size,
                etag: out.e_tag().map(|e| unquote(e).to_owned()),
                parts: 0,
                reused_parts: 0,
                checksum: checksum_field!(out, algorithm)
                    .map(str::to_owned)
                    .or(checksum),
                renamed_to: None,
            })
        };
        // Dropping an unfinished PutObject leaves no object: S3 stores an
        // object only once its whole body has arrived.
        tokio::select! {
            biased;
            () = hooks.stop.stopped() => Ok(Outcome::stopped(hooks.stop.reason())),
            uploaded = work => uploaded.map(Outcome::Completed),
        }
    }

    /// The storage class of the upload headers, when they name one the SDK knows.
    fn storage_class(&self) -> Option<aws_sdk_s3::types::StorageClass> {
        self.upload_headers
            .as_ref()
            .and_then(|h| h.storage_class.as_deref())
            .map(aws_sdk_s3::types::StorageClass::from)
    }

    // ---- metadata requests ----

    async fn create_upload(
        &self,
        dst: &S3Uri,
        content_type: Option<&str>,
        algorithm: ChecksumAlgorithm,
    ) -> Result<String> {
        let context = || format!("starting a multipart upload to {dst}");
        let client = bucket_client(&self.s3, dst.bucket()).await?;
        let out = client
            .create_multipart_upload()
            .bucket(dst.bucket())
            .key(dst.key())
            .set_content_type(content_type.map(str::to_owned))
            .set_metadata(self.upload_headers.as_ref().map(|h| h.metadata.clone()))
            .set_cache_control(
                self.upload_headers
                    .as_ref()
                    .and_then(|h| h.cache_control.clone()),
            )
            .set_content_disposition(
                self.upload_headers
                    .as_ref()
                    .and_then(|h| h.content_disposition.clone()),
            )
            .set_content_language(
                self.upload_headers
                    .as_ref()
                    .and_then(|h| h.content_language.clone()),
            )
            .set_content_encoding(
                self.upload_headers
                    .as_ref()
                    .and_then(|h| h.content_encoding.clone()),
            )
            .set_expires(self.upload_headers.as_ref().and_then(|h| h.expires))
            .set_storage_class(self.storage_class())
            .set_checksum_algorithm(sdk_algorithm(algorithm))
            .set_request_payer(self.s3.request_payer())
            .customize()
            .config_override(self.s3.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(context()))?;
        out.upload_id()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::Remote(format!("{}: the server sent no upload ID", context())))
    }

    /// Every part of an upload, following pagination.
    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        algorithm: ChecksumAlgorithm,
    ) -> Result<BTreeMap<u32, ListedPart>> {
        let context = || format!("listing the uploaded parts of s3://{bucket}/{key}");
        let client = bucket_client(&self.s3, bucket).await?;
        let mut parts = BTreeMap::new();
        let mut marker: Option<String> = None;
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            let out = client
                .list_parts()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.clone())
                .set_request_payer(self.s3.request_payer())
                .customize()
                .config_override(self.s3.metadata_timeout())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(context()))?;
            for part in out.parts() {
                let (Some(number), Some(etag), Some(size)) =
                    (part.part_number(), part.e_tag(), part.size())
                else {
                    continue;
                };
                let (Ok(number), Ok(size)) = (u32::try_from(number), u64::try_from(size)) else {
                    continue;
                };
                if number == 0 || etag.is_empty() {
                    continue;
                }
                parts.insert(
                    number,
                    ListedPart {
                        etag: etag.to_owned(),
                        size,
                        checksum: checksum_field!(part, algorithm).map(str::to_owned),
                    },
                );
            }
            if out.is_truncated() != Some(true) {
                return Ok(parts);
            }
            let next = out
                .next_part_number_marker()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| {
                    Error::Remote(format!(
                        "{}: the server reported more parts but sent no marker",
                        context()
                    ))
                })?
                .to_owned();
            if !seen.insert(next.clone()) {
                return Err(Error::Remote(format!(
                    "{}: the server returned the part marker '{next}' twice; stopping to avoid an endless listing",
                    context()
                )));
            }
            marker = Some(next);
        }
    }

    async fn complete_upload(
        &self,
        dst: &S3Uri,
        state: &UploadState,
    ) -> Result<CompleteMultipartUploadOutput> {
        let parts: Vec<CompletedPart> = state
            .parts
            .iter()
            .map(|part| {
                let builder = CompletedPart::builder()
                    .part_number(part_number(part.number))
                    .e_tag(&part.etag);
                set_checksum!(builder, state.checksum_algorithm, part.checksum.clone()).build()
            })
            .collect();
        let client = bucket_client(&self.s3, dst.bucket()).await?;
        client
            .complete_multipart_upload()
            .bucket(dst.bucket())
            .key(dst.key())
            .upload_id(&state.upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .set_if_match(
                self.write_condition
                    .as_ref()
                    .and_then(WriteCondition::if_match),
            )
            .set_if_none_match(
                self.write_condition
                    .as_ref()
                    .and_then(WriteCondition::if_none_match),
            )
            .set_request_payer(self.s3.request_payer())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("completing the multipart upload to {dst}"))
            })
    }
}

/// The checksum of `len` bytes of `source` at `offset`, read on the blocking
/// pool; a stop request ends the reading early ([`Error::Cancelled`]).
async fn checksum_range(
    source: Arc<dyn UploadSource>,
    offset: u64,
    len: u64,
    algorithm: ChecksumAlgorithm,
    stop: CancellationToken,
) -> Result<Option<String>> {
    let Some(mut checksummer) = Checksummer::new(algorithm) else {
        return Ok(None);
    };
    let description = source.describe();
    let hashed = blocking(move || {
        let capacity = usize::try_from(len.min(HASH_CHUNK)).map_err(HashError::io)?;
        let mut buf = vec![0u8; capacity];
        let mut done = 0u64;
        while done < len {
            if stop.is_cancelled() {
                return Err(HashError::Stopped);
            }
            let step = (len - done).min(HASH_CHUNK);
            let chunk = &mut buf[..usize::try_from(step).map_err(HashError::io)?];
            source
                .read_exact_at(offset + done, chunk)
                .map_err(HashError::Io)?;
            checksummer.update(chunk);
            done += step;
        }
        Ok(checksummer.finish())
    })
    .await?;
    match hashed {
        Ok(value) => Ok(Some(value)),
        Err(HashError::Stopped) => Err(Error::Cancelled),
        Err(HashError::Io(err)) => Err(Error::io(format!("reading {description}"), err)),
    }
}

impl HashError {
    fn io(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        HashError::Io(io::Error::other(err))
    }
}

/// Whether `part_size` can split `size` bytes into at most 10,000 parts.
fn valid_plan(size: u64, part_size: u64) -> bool {
    (MIN_PART_SIZE..=MAX_PART_SIZE).contains(&part_size)
        && size > 0
        && size.div_ceil(part_size) <= MAX_PARTS
}

/// Number of parts of `size` bytes split into `part_size` byte parts.
fn part_count(size: u64, part_size: u64) -> Result<u32> {
    if !valid_plan(size, part_size) {
        return Err(Error::Internal(format!(
            "{size} bytes cannot be uploaded in parts of {part_size} bytes"
        )));
    }
    u32::try_from(size.div_ceil(part_size))
        .map_err(|_| Error::Internal(format!("too many parts for {size} bytes")))
}

/// Size of part `number` (from 1), or `None` when the object has no such part.
fn part_len(size: u64, part_size: u64, number: u32) -> Option<u64> {
    let offset = u64::from(number.checked_sub(1)?).checked_mul(part_size)?;
    (offset < size).then(|| part_size.min(size - offset))
}

/// The SDK's part number type; part numbers are at most 10,000.
fn part_number(number: u32) -> i32 {
    i32::try_from(number).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MIB;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn part_arithmetic() {
        let part = 5 * MIB;
        assert_eq!(part_count(part, part).unwrap(), 1);
        assert_eq!(part_count(part + 1, part).unwrap(), 2);
        assert_eq!(part_count(11 * MIB + 123, part).unwrap(), 3);
        assert_eq!(part_len(11 * MIB + 123, part, 1), Some(part));
        assert_eq!(part_len(11 * MIB + 123, part, 3), Some(MIB + 123));
        assert_eq!(part_len(11 * MIB + 123, part, 4), None);
        assert_eq!(part_len(11 * MIB, part, 0), None);
        assert_eq!(part_len(10 * MIB, part, 2), Some(part));
        assert_eq!(part_len(10 * MIB, part, 3), None);
        assert!(part_count(0, part).is_err());
        assert!(part_count(part, MIN_PART_SIZE - 1).is_err());
        assert!(part_count(MAX_PARTS * part + 1, part).is_err());
        assert!(valid_plan(MAX_PARTS * part, part));
        // A 5 TiB object in the largest parts.
        let huge = 5 * 1024 * 1024 * MIB;
        let size = part_size_for(huge, 8 * MIB).unwrap();
        assert!(part_count(huge, size).unwrap() <= 10_000);
        assert_eq!(part_number(10_000), 10_000);
    }

    #[test]
    fn state_shape_is_stable() {
        let state = UploadState {
            bucket: "b".into(),
            key: "k/file.bin".into(),
            upload_id: "u-1".into(),
            source: SourceFingerprint {
                size: 11,
                modified: Some(
                    chrono::Utc
                        .with_ymd_and_hms(2026, 10, 29, 8, 15, 0)
                        .unwrap(),
                ),
            },
            part_size: 5 * MIB,
            checksum_algorithm: ChecksumAlgorithm::Crc32c,
            parts: vec![UploadedPart {
                number: 1,
                size: 5 * MIB,
                etag: "\"e1\"".into(),
                checksum: Some("c1==".into()),
            }],
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value,
            json!({
                "bucket": "b",
                "key": "k/file.bin",
                "upload_id": "u-1",
                "source": { "size": 11, "modified": "2026-10-29T08:15:00Z" },
                "part_size": 5 * MIB,
                "checksum_algorithm": "crc32c",
                "parts": [{ "number": 1, "size": 5 * MIB, "etag": "\"e1\"", "checksum": "c1==" }]
            })
        );
        assert_eq!(serde_json::from_value::<UploadState>(value).unwrap(), state);
    }
}
