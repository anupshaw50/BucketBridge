//! Transfer engines: the S3 data movement behind the transfer queue.
//!
//! Upload a file or a folder, download, copy, move, rename and delete.
//! Every engine is an async method of
//! [`TransferEngine`] and knows nothing about the queue; the queue wraps
//! each one as a task runner. They talk through small seams, bundled in
//! [`Hooks`]:
//!
//! * [`Throttle`]: the bandwidth limiter; every payload byte passes through
//!   it, in grants of at most [`Throttle::max_grant`] bytes.
//! * [`Progress`]: the total and the amount done (bytes; objects for
//!   deletes).
//! * [`StopSignal`]: a cancellation token plus the reason. A pause keeps the
//!   resume state, the partial download file and the multipart upload; a
//!   cancel cleans them up.
//!
//! Engines that can resume ([`TransferEngine::upload_file`],
//! [`TransferEngine::download_file`]) also take a checkpoint callback that
//! receives their typed resume state ([`UploadState`], [`DownloadState`])
//! while they run, and the last saved state when they start again.
//!
//! [`expand`] turns a selection (local paths, keys and prefixes) into one
//! work item per object, following the folder semantics of the transfer
//! commands in `docs/desktop-api.md`.
//!
//! [`S3Runner`] runs the engines as the task runners of the transfer queue
//! ([`crate::transfer`]), and the enqueue helpers ([`enqueue_upload`],
//! [`enqueue_download`], [`enqueue_copy`], [`enqueue_move`],
//! [`enqueue_delete`], [`enqueue_rename`]) implement the enqueue commands of
//! the desktop contract on top of the expansion.
//!
//! `docs/engines.md` describes the algorithms, the resume state formats, the
//! cleanup guarantees and the contract for wrapping an engine as a task
//! runner.

mod body;
pub mod checksum;
mod content_type;
mod control;
pub mod download;
mod enqueue;
pub mod expand;
mod remote;
mod runner;
pub mod source;
pub mod upload;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{RequestChecksumCalculation, ResponseChecksumValidation};
use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::ops::{CopyOptions, MAX_PART_SIZE, MIN_PART_SIZE, S3};
use crate::s3uri::{S3Uri, validate_key};
use crate::settings::{ChecksumAlgorithm, DataIntegritySettings, MIB, Settings, TransferSettings};

pub use checksum::{Checksummer, ObjectChecksum, checksum_of};
pub use content_type::{DEFAULT_CONTENT_TYPE, guess_content_type};
pub use control::{
    DEFAULT_MAX_GRANT, Hooks, NoProgress, Outcome, Progress, ProgressCounter, StopReason,
    StopSignal, StreamFuture, StreamPermit, Throttle, ThrottleFuture, Unthrottled,
};
pub use download::{
    DownloadOutcome, DownloadSink, DownloadState, FileSink, PARTIAL_SUFFIX, is_partial_name,
    partial_path,
};
pub use enqueue::{
    enqueue_copy, enqueue_delete, enqueue_download, enqueue_download_decrypted, enqueue_move,
    enqueue_rename, enqueue_upload, enqueue_upload_encrypted,
};
pub use expand::{
    CopyItem, DeleteItem, DownloadItem, ExpandFailure, Expansion, UploadItem, expand_delete,
    expand_upload, expand_upload_blocking,
};
pub use runner::{RunnerState, S3Provider, S3Runner};
pub use source::{FileSource, SourceFingerprint, UploadSource};
pub use upload::{UploadOutcome, UploadState, UploadedPart};

/// The largest object one `PutObject` may carry (5 GiB), and therefore the
/// largest multipart threshold.
pub const MAX_SINGLE_UPLOAD: u64 = MAX_PART_SIZE;

/// Most parts uploaded at the same time by one task.
pub const MAX_PARTS_IN_FLIGHT: usize = 64;

/// How transfers are split and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOptions {
    /// Uploads of at least this many bytes use a multipart upload; smaller
    /// ones a single `PutObject`. 1 to [`MAX_SINGLE_UPLOAD`].
    pub multipart_threshold: u64,
    /// Preferred part size, grown automatically so that an upload never
    /// needs more than 10,000 parts ([`crate::ops::part_size_for`]).
    /// [`MIN_PART_SIZE`] to [`MAX_PART_SIZE`].
    pub part_size: u64,
    /// Parts uploaded at the same time by one task, 1 to
    /// [`MAX_PARTS_IN_FLIGHT`].
    pub max_parts_in_flight: usize,
    /// Upload checksum; [`ChecksumAlgorithm::None`] sends checksums only
    /// where an API requires one.
    pub checksum: ChecksumAlgorithm,
    /// Validate downloads against the checksum the server stores.
    pub validate_downloads: bool,
    /// How server-side copies are split (validated by the copy itself).
    pub copy: CopyOptions,
}

impl Default for TransferOptions {
    fn default() -> Self {
        TransferOptions::from_settings(&Settings::default())
    }
}

impl TransferOptions {
    /// The options of the `transfer` and `data_integrity` settings pages.
    pub fn from_settings(settings: &Settings) -> Self {
        Self::from_parts(&settings.transfer, &settings.data_integrity)
    }

    /// The options of a `transfer` and a `data_integrity` settings page (the
    /// transfer queue hands runners a snapshot of the former).
    pub fn from_parts(transfer: &TransferSettings, data_integrity: &DataIntegritySettings) -> Self {
        let in_flight = usize::try_from(transfer.max_parts_per_task)
            .unwrap_or(MAX_PARTS_IN_FLIGHT)
            .clamp(1, MAX_PARTS_IN_FLIGHT);
        TransferOptions {
            multipart_threshold: transfer.multipart_threshold_mib.saturating_mul(MIB),
            part_size: transfer.part_size_mib.saturating_mul(MIB),
            max_parts_in_flight: in_flight,
            checksum: data_integrity.checksum_algorithm,
            validate_downloads: data_integrity.validate_downloads,
            copy: CopyOptions {
                concurrency: in_flight,
                ..CopyOptions::default()
            },
        }
    }

    /// Checks the documented ranges.
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_SINGLE_UPLOAD).contains(&self.multipart_threshold) {
            return Err(Error::Usage(format!(
                "the multipart threshold must be between 1 and {MAX_SINGLE_UPLOAD} bytes (got {})",
                self.multipart_threshold
            )));
        }
        if !(MIN_PART_SIZE..=MAX_PART_SIZE).contains(&self.part_size) {
            return Err(Error::Usage(format!(
                "the part size must be between {MIN_PART_SIZE} and {MAX_PART_SIZE} bytes (got {})",
                self.part_size
            )));
        }
        if !(1..=MAX_PARTS_IN_FLIGHT).contains(&self.max_parts_in_flight) {
            return Err(Error::Usage(format!(
                "the parts in flight per task must be between 1 and {MAX_PARTS_IN_FLIGHT} (got {})",
                self.max_parts_in_flight
            )));
        }
        Ok(())
    }
}

/// A precondition on the object an upload creates or replaces (S3 conditional writes: the
/// HTTP headers `If-None-Match` and `If-Match` of `PutObject` and
/// `CompleteMultipartUpload`). A server that checks it answers `412 Precondition Failed`
/// when it does not hold; one that does not know conditional writes answers `501` or
/// ignores the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCondition {
    /// The key must not exist yet (`If-None-Match: *`).
    Absent,
    /// The object must still have this ETag, given without quotes (`If-Match`).
    ETag(String),
}

impl WriteCondition {
    fn if_none_match(&self) -> Option<String> {
        matches!(self, WriteCondition::Absent).then(|| "*".to_owned())
    }

    fn if_match(&self) -> Option<String> {
        match self {
            WriteCondition::ETag(etag) => Some(format!("\"{}\"", unquote(etag))),
            WriteCondition::Absent => None,
        }
    }
}

/// The transfer engines for one account: cheap to clone, safe to share.
#[derive(Debug, Clone)]
pub struct TransferEngine {
    s3: S3,
    options: TransferOptions,
    write_condition: Option<WriteCondition>,
    download_version: Option<String>,
    upload_headers: Option<UploadHeaders>,
}

/// Object properties an upload sets besides the content type: user metadata
/// and the standard headers that survive a rewrite of an object (see
/// [`TransferEngine::with_upload_headers`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UploadHeaders {
    /// User metadata (keys without the `x-amz-meta-` prefix).
    pub metadata: std::collections::HashMap<String, String>,
    /// `Cache-Control`.
    pub cache_control: Option<String>,
    /// `Content-Disposition`.
    pub content_disposition: Option<String>,
    /// `Content-Encoding`.
    pub content_encoding: Option<String>,
    /// `Content-Language`.
    pub content_language: Option<String>,
    /// `Expires`.
    pub expires: Option<aws_smithy_types::DateTime>,
    /// Storage class (`STANDARD_IA`, ...); `None` is the bucket default.
    pub storage_class: Option<String>,
}

impl TransferEngine {
    /// Engines that use `s3` with `options` (validated).
    pub fn new(s3: S3, options: TransferOptions) -> Result<Self> {
        options.validate()?;
        Ok(TransferEngine {
            s3,
            options,
            write_condition: None,
            download_version: None,
            upload_headers: None,
        })
    }

    /// A copy of these engines whose uploads carry `condition` (single `PutObject`
    /// requests, and the `CompleteMultipartUpload` of multipart uploads). Used per file by
    /// callers that must not overwrite an object they did not see.
    #[must_use]
    pub fn with_write_condition(&self, condition: WriteCondition) -> Self {
        TransferEngine {
            write_condition: Some(condition),
            ..self.clone()
        }
    }

    /// A copy of these engines whose uploads set `headers` on the object (the
    /// `PutObject` request, or the `CreateMultipartUpload` of multipart
    /// uploads).
    #[must_use]
    pub fn with_upload_headers(&self, headers: UploadHeaders) -> Self {
        TransferEngine {
            upload_headers: Some(headers),
            ..self.clone()
        }
    }

    /// A copy of these engines whose downloads read the version `version_id`
    /// of every object (`None`: the current version, the default). The
    /// download state records the ETag of what was read, so a resumed
    /// download asks for the same version again.
    #[must_use]
    pub fn with_download_version(&self, version_id: Option<String>) -> Self {
        TransferEngine {
            download_version: version_id,
            ..self.clone()
        }
    }

    /// The transport.
    pub fn s3(&self) -> &S3 {
        &self.s3
    }

    /// The options.
    pub fn options(&self) -> &TransferOptions {
        &self.options
    }
}

/// The SDK client for requests on `bucket`, addressed to the bucket's region
/// ([`S3::client_for`]). Every bucket-scoped SDK call the engines make
/// themselves goes through this function.
async fn bucket_client(s3: &S3, bucket: &str) -> Result<Client> {
    s3.client_for(bucket).await
}

/// Runs blocking work on Tokio's blocking thread pool.
async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))
}

/// A per-request configuration that sends checksums only where an API
/// requires one (the `none` setting), whatever the client's default.
fn without_request_checksums() -> aws_sdk_s3::config::Builder {
    aws_sdk_s3::config::Builder::default()
        .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
}

/// `config` changed to validate response checksums only where an API
/// requires it (downloads with validation turned off).
fn without_response_validation(config: aws_sdk_s3::config::Builder) -> aws_sdk_s3::config::Builder {
    config.response_checksum_validation(ResponseChecksumValidation::WhenRequired)
}

/// Checks that `uri` names an object, not a folder or the whole bucket.
fn require_object_key(uri: &S3Uri) -> Result<()> {
    validate_key(uri.key()).map_err(|err| err.with_context(uri))?;
    if uri.key().ends_with('/') {
        return Err(Error::Usage(format!(
            "{uri} is a folder; this operation needs an object key"
        )));
    }
    Ok(())
}

/// An ETag without the double quotes S3 puts around it.
fn unquote(etag: &str) -> &str {
    etag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(etag)
}

fn to_utc(time: &aws_smithy_types::DateTime) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos())
}

fn to_i64(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Usage(format!("{what} is too large ({value} bytes)")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn options_follow_settings_and_validate() {
        let defaults = TransferOptions::default();
        assert_eq!(defaults.multipart_threshold, 8 * MIB);
        assert_eq!(defaults.part_size, 8 * MIB);
        assert_eq!(defaults.max_parts_in_flight, 4);
        assert_eq!(defaults.checksum, ChecksumAlgorithm::Crc32);
        assert!(defaults.validate_downloads);
        assert_eq!(defaults.copy.concurrency, 4);
        defaults.validate().unwrap();

        let mut settings = Settings::default();
        settings.transfer.multipart_threshold_mib = 5;
        settings.transfer.part_size_mib = 16;
        settings.transfer.max_parts_per_task = 32;
        settings.data_integrity.checksum_algorithm = ChecksumAlgorithm::Sha256;
        settings.data_integrity.validate_downloads = false;
        let options = TransferOptions::from_settings(&settings);
        assert_eq!(options.multipart_threshold, 5 * MIB);
        assert_eq!(options.part_size, 16 * MIB);
        assert_eq!(options.max_parts_in_flight, 32);
        assert_eq!(options.checksum, ChecksumAlgorithm::Sha256);
        assert!(!options.validate_downloads);
        options.validate().unwrap();

        for bad in [
            TransferOptions {
                multipart_threshold: 0,
                ..TransferOptions::default()
            },
            TransferOptions {
                multipart_threshold: MAX_SINGLE_UPLOAD + 1,
                ..TransferOptions::default()
            },
            TransferOptions {
                part_size: MIN_PART_SIZE - 1,
                ..TransferOptions::default()
            },
            TransferOptions {
                part_size: MAX_PART_SIZE + 1,
                ..TransferOptions::default()
            },
            TransferOptions {
                max_parts_in_flight: 0,
                ..TransferOptions::default()
            },
            TransferOptions {
                max_parts_in_flight: MAX_PARTS_IN_FLIGHT + 1,
                ..TransferOptions::default()
            },
        ] {
            assert_eq!(
                bad.validate().unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn object_keys_and_etags() {
        let uri = |key: &str| S3Uri::new("bucket", key).unwrap();
        require_object_key(&uri("a/b.txt")).unwrap();
        for bad in ["", "folder/"] {
            assert_eq!(
                require_object_key(&uri(bad)).unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad:?}"
            );
        }
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("abc"), "abc");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(to_i64(5, "x").unwrap(), 5);
        assert_eq!(to_i64(u64::MAX, "x").unwrap_err().kind(), ErrorKind::Usage);
    }
}
