//! Copy and move between two accounts.
//!
//! * Same service endpoint: the destination account first asks the service
//!   to copy (`CopyObject`, or `UploadPartCopy` parts), which moves no data
//!   through this machine. That needs the destination account to be allowed
//!   to read the source; when the service answers `AccessDenied` the copy
//!   falls back to streaming.
//! * Different endpoints, or after `AccessDenied`: the object is streamed
//!   through this machine. The source is read by ranges ([`RemoteSource`])
//!   and written by the upload engine, so the multipart upload, the part
//!   checksums, the resume state, the progress and the bandwidth limit are
//!   the ones of any upload. Memory stays bounded: a few parts at a time.
//! * A move deletes its source only after the destination was read back
//!   and compared with it ([`verify_copy`]).

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};

use super::control::{Hooks, Outcome};
use super::policy::{ItemInfo, Placed, object_time};
use super::source::{SourceFingerprint, UploadSource};
use super::upload::UploadState;
use super::{TransferEngine, UploadHeaders, require_object_key};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{CopyOutcome, ObjectHead, Sse, TransferKind};
use crate::ops::{S3, part_size_for};
use crate::s3uri::{S3Uri, validate_key};
use crate::settings::IfExists;

/// How a cross-account copy was done, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyRoute {
    /// The service copied the object.
    ServerSide,
    /// The object went through this machine.
    Streamed,
}

impl TransferEngine {
    /// Copies `src`, reached through these engines, to `dst`, reached through
    /// `dest` (another account, or the same one). See the module
    /// documentation for the route taken. `resume` and `checkpoint` are the
    /// upload state of a streamed copy; a copy that is stopped half-way
    /// through streaming keeps its multipart upload for the resume (the queue
    /// cleans it up on cancel, see [`TransferEngine::discard_upload`] on the
    /// destination engine).
    ///
    /// The overwrite policy of these engines applies to the destination, as
    /// in [`TransferEngine::copy`].
    pub async fn copy_to(
        &self,
        dest: &TransferEngine,
        src: &S3Uri,
        dst: &S3Uri,
        hooks: &Hooks,
        resume: Option<UploadState>,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<(CopyOutcome, CopyRoute)>> {
        validate_key(src.key()).map_err(|err| err.with_context("copy source"))?;
        validate_key(dst.key()).map_err(|err| err.with_context("copy destination"))?;
        if let Some(reason) = hooks.stop.reason() {
            return Ok(Outcome::stopped(Some(reason)));
        }
        if src.key().ends_with('/') {
            if !dst.key().ends_with('/') {
                return Err(Error::Usage(format!(
                    "{src} is a folder; its destination {dst} must end with '/'"
                )));
            }
            if self.is_folder_marker(src).await? {
                let outcome = dest.create_folder_marker(dst, hooks).await?;
                return Ok(outcome.map(|()| (CopyOutcome::default(), CopyRoute::ServerSide)));
            }
        }
        require_object_key(src)?;
        require_object_key(dst)?;
        let head = self.s3.head_object(src.bucket(), src.key(), None).await?;
        let mut target = dst.clone();
        let policy = self.options.if_exists;
        if policy != IfExists::Overwrite && (resume.is_none() || policy == IfExists::Rename) {
            let info = ItemInfo {
                size: Some(head.size),
                modified: object_time(&head),
            };
            match self
                .place_remote(
                    &dest.s3,
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
        let renamed = (target != *dst).then(|| target.key().to_owned());
        hooks.progress.set_total(head.size);
        hooks.progress.set_done(0);

        if resume.is_none() && self.s3.same_endpoint(&dest.s3) {
            match dest.server_side_between_accounts(src, &target, hooks).await {
                Ok(Outcome::Completed(mut done)) => {
                    done.renamed_to = renamed;
                    return Ok(Outcome::Completed((done, CopyRoute::ServerSide)));
                }
                Ok(other) => return Ok(other.map(|done| (done, CopyRoute::ServerSide))),
                // The destination account may not read the source: stream.
                Err(err) if err.kind() == ErrorKind::Auth => {
                    hooks.progress.set_done(0);
                }
                Err(err) => return Err(err),
            }
        }

        let block = part_size_for(head.size, dest.options.part_size)?;
        let source = RemoteSource::new(
            self.s3.clone(),
            src.clone(),
            &head,
            block,
            dest.options.max_parts_in_flight.saturating_mul(2).max(4),
        );
        // The source's own metadata is what the copy keeps; no local time is
        // involved, and the destination decided about existing objects above.
        let writer = dest
            .with_upload_headers(headers_of(&head))
            .with_preserve_mtime(false)
            .with_if_exists(IfExists::Overwrite);
        let content_type = head.meta.content_type.clone();
        let outcome = writer
            .upload(
                Arc::new(source),
                &target,
                content_type.as_deref(),
                hooks,
                resume,
                checkpoint,
            )
            .await?;
        Ok(match outcome {
            Outcome::Completed(done) => Outcome::Completed((
                CopyOutcome {
                    size: done.size,
                    etag: done.etag,
                    parts: done.parts,
                    renamed_to: renamed,
                },
                CopyRoute::Streamed,
            )),
            Outcome::Paused => Outcome::Paused,
            Outcome::Cancelled => Outcome::Cancelled,
            Outcome::Skipped(reason) => Outcome::Skipped(reason),
        })
    }

    /// Moves `src` to `dst` across accounts: [`TransferEngine::copy_to`],
    /// then the destination is compared with the source ([`verify_copy`]),
    /// and only then is the source deleted. Any failure before the delete
    /// keeps the source; if the delete itself fails, the error says that the
    /// copy exists and the source is still there.
    pub async fn move_to(
        &self,
        dest: &TransferEngine,
        src: &S3Uri,
        dst: &S3Uri,
        hooks: &Hooks,
        resume: Option<UploadState>,
        checkpoint: &(dyn Fn(&UploadState) + Send + Sync),
    ) -> Result<Outcome<(CopyOutcome, CopyRoute)>> {
        let (copied, route) = match self
            .copy_to(dest, src, dst, hooks, resume, checkpoint)
            .await?
        {
            Outcome::Completed(done) => done,
            other => return Ok(other),
        };
        let landed = match &copied.renamed_to {
            Some(key) => S3Uri::new(dst.bucket(), key.clone())?,
            None => dst.clone(),
        };
        if !src.key().ends_with('/') {
            // Not raced with the stop signal: the copy exists, and leaving the
            // source behind after a verified copy would only confuse.
            let before = self.s3.head_object(src.bucket(), src.key(), None).await?;
            let after = dest
                .s3
                .head_object(landed.bucket(), landed.key(), None)
                .await?;
            verify_copy(&before, &after).map_err(|err| {
                err.with_context(format!(
                    "moving {src} to {landed}: the source was kept because the copy could not be verified"
                ))
            })?;
        }
        self.s3
            .delete_object(src.bucket(), src.key(), None)
            .await
            .map_err(|err| {
                err.with_context(format!(
                    "moving {src} to {landed}: the copy exists, but the source could not be deleted and is still there"
                ))
            })?;
        Ok(Outcome::Completed((copied, route)))
    }

    /// `CopyObject` or `UploadPartCopy` issued by this (the destination)
    /// account for an object of another account's bucket.
    async fn server_side_between_accounts(
        &self,
        src: &S3Uri,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Outcome<CopyOutcome>> {
        let reported = std::sync::atomic::AtomicU64::new(0);
        let work = self
            .s3
            .copy_object_with(src, dst, &self.options.copy, |copied| {
                let before = reported.fetch_max(copied, std::sync::atomic::Ordering::AcqRel);
                if copied > before {
                    hooks.progress.add_done(copied - before);
                }
            });
        tokio::select! {
            biased;
            () = hooks.stop.stopped() => Ok(Outcome::stopped(hooks.stop.reason())),
            copied = work => {
                let done = copied?;
                hooks.progress.set_total(done.size);
                hooks.progress.set_done(done.size);
                Ok(Outcome::Completed(done))
            }
        }
    }
}

/// The headers a streamed copy gives the new object: the source's content
/// headers and user metadata (tags and ACLs are not copied) and its storage
/// class.
fn headers_of(head: &ObjectHead) -> UploadHeaders {
    UploadHeaders {
        metadata: head
            .meta
            .user_meta
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        cache_control: head.meta.cache_control.clone(),
        content_disposition: head.meta.content_disposition.clone(),
        content_encoding: head.meta.content_encoding.clone(),
        content_language: None,
        expires: None,
        storage_class: head
            .storage_class
            .clone()
            .filter(|class| class != "STANDARD"),
    }
}

/// Compares a copy with its source: the sizes must match, plain ETags (not
/// multipart, not KMS-encrypted) must match, and checksums of the same kind
/// must match. Values that cannot be compared (a multipart ETag against a
/// single-part one, composite checksums of different part counts) are left
/// out; the transfer itself already verified every byte it moved.
pub fn verify_copy(source: &ObjectHead, copy: &ObjectHead) -> Result<()> {
    let fail = |what: &str| {
        Err(Error::Remote(format!(
            "the copy differs from its source: {what}"
        )))
    };
    if source.size != copy.size {
        return fail(&format!(
            "the size is {} bytes, the source has {}",
            copy.size, source.size
        ));
    }
    let plain = |head: &ObjectHead| {
        head.etag
            .as_deref()
            .filter(|etag| {
                !etag.contains('-')
                    && !matches!(head.meta.sse, Sse::Kms { .. } | Sse::Customer { .. })
            })
            .map(str::to_owned)
    };
    if let (Some(a), Some(b)) = (plain(source), plain(copy))
        && !a.eq_ignore_ascii_case(&b)
    {
        return fail("the ETag differs");
    }
    let pairs = [
        ("CRC32", &source.checksums.crc32, &copy.checksums.crc32),
        ("CRC32C", &source.checksums.crc32c, &copy.checksums.crc32c),
        (
            "CRC64NVME",
            &source.checksums.crc64nvme,
            &copy.checksums.crc64nvme,
        ),
        ("SHA1", &source.checksums.sha1, &copy.checksums.sha1),
        ("SHA256", &source.checksums.sha256, &copy.checksums.sha256),
    ];
    for (name, a, b) in pairs {
        if let (Some(a), Some(b)) = (a, b) {
            // A composite value ends in `-<parts>`; it is comparable only
            // with a composite value of the same part count.
            let comparable = match (a.rsplit_once('-'), b.rsplit_once('-')) {
                (None, None) => true,
                (Some((_, x)), Some((_, y))) => x == y,
                _ => false,
            };
            if comparable && a != b {
                return fail(&format!("the {name} checksum differs"));
            }
        }
    }
    Ok(())
}

/// An object of another account, read by ranges as an upload source.
///
/// The upload engine reads each part twice (to checksum it, then to send it)
/// from several threads. A block cache of one part per block keeps that to
/// one `GET` per part; at most `cap` blocks are held, so memory is bounded
/// by `cap` times the part size. A block that was evicted is fetched again.
#[derive(Debug)]
pub struct RemoteSource {
    s3: S3,
    src: S3Uri,
    etag: Option<String>,
    size: u64,
    modified: Option<DateTime<Utc>>,
    block: u64,
    cap: usize,
    handle: tokio::runtime::Handle,
    cache: Mutex<Vec<(u64, Arc<Vec<u8>>)>>,
}

impl RemoteSource {
    /// Reads `src`, whose `head` was just taken, in blocks of `block` bytes
    /// (use the upload's part size), holding at most `cap` blocks. Must be
    /// called inside a Tokio runtime: the blocking reads run on its handle.
    pub fn new(s3: S3, src: S3Uri, head: &ObjectHead, block: u64, cap: usize) -> Self {
        RemoteSource {
            s3,
            src,
            etag: head.etag.clone(),
            size: head.size,
            modified: head.last_modified,
            block: block.max(1),
            cap: cap.max(1),
            handle: tokio::runtime::Handle::current(),
            cache: Mutex::new(Vec::new()),
        }
    }

    fn block_data(&self, index: u64) -> io::Result<Arc<Vec<u8>>> {
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((_, data)) = cache.iter().find(|(known, _)| *known == index) {
                return Ok(Arc::clone(data));
            }
        }
        let data = Arc::new(self.handle.block_on(self.fetch(index))?);
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= self.cap {
            cache.remove(0);
        }
        cache.push((index, Arc::clone(&data)));
        Ok(data)
    }

    async fn fetch(&self, index: u64) -> io::Result<Vec<u8>> {
        let start = index * self.block;
        let end = (start + self.block).min(self.size) - 1;
        let failed = |err: Error| io::Error::other(err.to_string());
        let client = self
            .s3
            .client_for(self.src.bucket())
            .await
            .map_err(failed)?;
        let response = client
            .get_object()
            .bucket(self.src.bucket())
            .key(self.src.key())
            .range(format!("bytes={start}-{end}"))
            .set_if_match(self.etag.as_ref().map(|etag| format!("\"{etag}\"")))
            .set_request_payer(self.s3.request_payer())
            .customize()
            .config_override(self.s3.metadata_timeout())
            .send()
            .await
            .map_err(|err| failed(Error::from(err).with_context(&self.src)))?;
        let bytes = response
            .body
            .collect()
            .await
            .map_err(|err| io::Error::other(format!("reading {}: {err}", self.src)))?
            .into_bytes();
        let expected = end - start + 1;
        if bytes.len() as u64 != expected {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "{} sent {} bytes of the {expected} asked for",
                    self.src,
                    bytes.len()
                ),
            ));
        }
        Ok(bytes.to_vec())
    }
}

impl UploadSource for RemoteSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let position = offset + done as u64;
            if position >= self.size {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let index = position / self.block;
            let data = self.block_data(index)?;
            let within = usize::try_from(position - index * self.block)
                .map_err(|_| io::Error::other("block offset too large"))?;
            let available = data.len().saturating_sub(within);
            if available == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let n = available.min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&data[within..within + n]);
            done += n;
        }
        Ok(())
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint {
            size: self.size,
            modified: self.modified,
        }
    }

    fn is_unchanged(&self) -> io::Result<bool> {
        let head = self
            .handle
            .block_on(self.s3.head_object(self.src.bucket(), self.src.key(), None))
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(head.size == self.size && head.etag == self.etag)
    }

    fn describe(&self) -> String {
        self.src.to_string()
    }
}
