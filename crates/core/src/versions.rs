//! Object versions: listing a prefix by key with filters, restoring a
//! version, purging versions and downloading one version.
//!
//! The listing ([`S3::list_version_groups`]) follows `ListObjectVersions` with
//! its key and version-ID markers and hands out whole *groups*: every version
//! and delete marker of one key together, newest first. A group is judged
//! only once all of its entries have been seen, even when they arrive on
//! several S3 pages, so the filters see the real number of versions of a key.
//!
//! # Filters
//!
//! A [`VersionFilter`] applies in two steps. First, each entry is kept or
//! dropped by what it is (a version or a delete marker) and by its
//! modification time. Then each key is kept or dropped by how many entries
//! are left: at least two ("multiple versions"), more than a number, fewer
//! than a number. A key with no entry left is not listed. The date interval is
//! half open, as in [`crate::filter`] (`after <= modified < before`).
//!
//! # Pages and tokens
//!
//! A page holds at most `max_groups` groups (and about
//! [`PAGE_ENTRY_BUDGET`] entries); a call sends at most [`SCAN_REQUESTS`]
//! requests before it ends at the next key boundary, so that a filter that
//! rejects most keys returns an empty page with a token instead of scanning a
//! whole bucket. The token names the last key whose group was complete, so
//! the next call starts after it. A key with more than [`GROUP_SCAN_CAP`]
//! versions is counted up to that number, listed with [`VersionGroup::truncated`]
//! set, and skipped over on the server.
//!
//! # Changing versions
//!
//! * [`S3::restore_version`] copies a version over the key, which makes the
//!   copy the current version (an S3 copy keeps content headers, user
//!   metadata and tags; the storage class and server-side encryption of the
//!   version are sent again explicitly).
//! * [`S3::purge_versions`] deletes versions and delete markers for good.
//! * [`enqueue_download_version`] queues a download of one version; the
//!   version rides in the task's resume state ([`RunnerState::VersionDownload`]).

use std::collections::HashSet;
use std::hash::{BuildHasher, Hash, RandomState};
use std::path::Path;

use aws_sdk_s3::error::BuildError;
use aws_sdk_s3::operation::list_object_versions::ListObjectVersionsOutput;
use aws_sdk_s3::types::{
    Delete, MetadataDirective, ObjectIdentifier, ServerSideEncryption, StorageClass,
    TaggingDirective,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::engine::{RunnerState, is_partial_name};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{
    DeleteFailure, DeleteOutcome, Location, ObjectHead, ObjectVersion, Sse, TransferKind,
    TransferTask,
};
use crate::objectlock::MfaDeleteAuth;
use crate::ops::{MAX_DELETE_BATCH, MAX_LIST_KEYS, MAX_SINGLE_COPY_SIZE, S3};
use crate::s3uri::{validate_bucket_name, validate_key};
use crate::transfer::{NewTask, TransferQueue};

/// Entries (versions and delete markers) of one key that a page lists at
/// most; the others are counted ([`VersionGroup::matching`]) and flagged
/// ([`VersionGroup::truncated`]).
pub const MAX_GROUP_ENTRIES: usize = 1000;

/// Entries after which a page is complete, at the next key boundary.
pub const PAGE_ENTRY_BUDGET: usize = 5000;

/// Requests of one listing call after which it ends at the next key
/// boundary, with a token, whatever it has found.
pub const SCAN_REQUESTS: usize = 10;

/// Entries of one key that are counted; the rest of the key is skipped.
pub const GROUP_SCAN_CAP: u64 = 50_000;

/// Most groups a page may hold.
pub const MAX_GROUPS_PER_PAGE: u32 = 1000;

/// What the version list does with delete markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMarkers {
    /// Lists them next to the versions.
    #[default]
    Include,
    /// Leaves them out.
    Exclude,
    /// Lists nothing but delete markers.
    Only,
}

/// Which versions the version list shows (`VersionFilter` of
/// `docs/desktop-api.md`). The default shows everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionFilter {
    /// Show versions that hold data. Off leaves only delete markers (when
    /// they are shown).
    pub show_versions: bool,
    /// What to do with delete markers.
    pub delete_markers: DeleteMarkers,
    /// Only keys with more than one entry left.
    pub multiple_versions: bool,
    /// Only keys with more than this many entries left.
    pub count_greater_than: Option<u32>,
    /// Only keys with fewer than this many entries left.
    pub count_less_than: Option<u32>,
    /// Only entries modified at or after this time.
    pub modified_after: Option<DateTime<Utc>>,
    /// Only entries modified before this time.
    pub modified_before: Option<DateTime<Utc>>,
}

impl Default for VersionFilter {
    fn default() -> Self {
        VersionFilter {
            show_versions: true,
            delete_markers: DeleteMarkers::Include,
            multiple_versions: false,
            count_greater_than: None,
            count_less_than: None,
            modified_after: None,
            modified_before: None,
        }
    }
}

impl VersionFilter {
    /// Rejects filters that can never match anything, with a `usage` error
    /// that says why: nothing to show, an empty date interval, or counts that
    /// no key can satisfy.
    pub fn validate(&self) -> Result<()> {
        if !self.show_versions && self.delete_markers == DeleteMarkers::Exclude {
            return Err(Error::Usage(
                "nothing would be listed: turn on versions or delete markers".into(),
            ));
        }
        if let (Some(after), Some(before)) = (self.modified_after, self.modified_before)
            && after >= before
        {
            return Err(Error::Usage(
                "modified_after must be earlier than modified_before (the interval is empty)"
                    .into(),
            ));
        }
        let least = u64::from(self.count_greater_than.unwrap_or(0)) + 1;
        let least = if self.multiple_versions {
            least.max(2)
        } else {
            least
        };
        if let Some(fewer_than) = self.count_less_than
            && u64::from(fewer_than) <= least
        {
            return Err(Error::Usage(format!(
                "no key can have at least {least} and fewer than {fewer_than} versions"
            )));
        }
        Ok(())
    }

    /// Whether an entry is listed: its kind and its modification time.
    fn entry_matches(&self, entry: &ObjectVersion) -> bool {
        let kind = if entry.is_delete_marker {
            self.delete_markers != DeleteMarkers::Exclude
        } else {
            self.show_versions && self.delete_markers != DeleteMarkers::Only
        };
        if !kind {
            return false;
        }
        if self.modified_after.is_none() && self.modified_before.is_none() {
            return true;
        }
        let Some(modified) = entry.last_modified else {
            return false;
        };
        !self.modified_after.is_some_and(|after| modified < after)
            && !self
                .modified_before
                .is_some_and(|before| modified >= before)
    }

    /// Whether a key with `count` entries left is listed.
    fn key_matches(&self, count: u64) -> bool {
        (!self.multiple_versions || count >= 2)
            && self
                .count_greater_than
                .is_none_or(|greater| count > u64::from(greater))
            && self
                .count_less_than
                .is_none_or(|fewer| count < u64::from(fewer))
    }
}

/// One key with its versions and delete markers (`VersionGroup` of
/// `docs/desktop-api.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionGroup {
    /// Object key.
    pub key: String,
    /// The entries the filter keeps, newest first (at most
    /// [`MAX_GROUP_ENTRIES`]).
    pub versions: Vec<ObjectVersion>,
    /// Every entry the key has (counted up to [`GROUP_SCAN_CAP`]).
    pub total: u64,
    /// How many entries the filter keeps.
    pub matching: u64,
    /// The key is deleted: its newest entry is a delete marker.
    pub is_deleted: bool,
    /// The key has more entries than are listed or counted.
    pub truncated: bool,
}

/// One page of [`S3::list_version_groups`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VersionGroupPage {
    /// Keys in order.
    pub groups: Vec<VersionGroup>,
    /// Pass back to continue after the last key judged; `None` at the end.
    pub next_token: Option<String>,
    /// Entries the server sent for this page, listed or not.
    pub scanned: u64,
}

/// What [`S3::list_version_groups`] lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionQuery {
    /// Keys that start with this are listed.
    pub prefix: String,
    /// List only the key that equals `prefix`.
    pub exact: bool,
    /// Which entries and keys to keep.
    pub filter: VersionFilter,
    /// The `next_token` of the previous page.
    pub token: Option<String>,
    /// Most keys on the page, 1 to [`MAX_GROUPS_PER_PAGE`].
    pub max_groups: u32,
    /// Entries asked of S3 per request, 1 to 1000.
    pub request_keys: u32,
}

impl VersionQuery {
    /// Everything under `prefix`, 100 keys per page.
    pub fn new(prefix: impl Into<String>) -> Self {
        VersionQuery {
            prefix: prefix.into(),
            exact: false,
            filter: VersionFilter::default(),
            token: None,
            max_groups: 100,
            request_keys: MAX_LIST_KEYS,
        }
    }

    fn validate(&self) -> Result<()> {
        if !(1..=MAX_GROUPS_PER_PAGE).contains(&self.max_groups) {
            return Err(Error::Usage(format!(
                "a page of versions holds 1 to {MAX_GROUPS_PER_PAGE} keys (got {})",
                self.max_groups
            )));
        }
        if !(1..=MAX_LIST_KEYS).contains(&self.request_keys) {
            return Err(Error::Usage(format!(
                "a request asks for 1 to {MAX_LIST_KEYS} entries (got {})",
                self.request_keys
            )));
        }
        if self.exact {
            validate_key(&self.prefix)?;
        }
        self.filter.validate()
    }
}

/// Extra request settings for [`S3::purge_versions_with`].
#[derive(Debug, Clone, Default)]
pub struct PurgeOptions {
    /// The MFA device and code, for a bucket with MFA delete on.
    pub mfa: Option<MfaDeleteAuth>,
    /// Delete versions under a governance retention
    /// (`x-amz-bypass-governance-retention`).
    pub bypass_governance: bool,
}

/// One version of one key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionRef {
    /// Object key.
    pub key: String,
    /// Version ID (`null` for an object from before versioning).
    pub version_id: String,
}

/// The result of [`S3::restore_version`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOutcome {
    /// Object key.
    pub key: String,
    /// The version that was copied.
    pub restored_version_id: String,
    /// The new current version, when the server reports one.
    pub version_id: Option<String>,
    /// Entity tag of the new version, without quotes.
    pub etag: Option<String>,
    /// Size in bytes.
    pub size: u64,
}

/// What a version is, from `HeadObject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionHead {
    /// A version with data.
    Version(Box<ObjectHead>),
    /// A delete marker.
    DeleteMarker,
}

impl S3 {
    /// Lists the versions and delete markers under a prefix, grouped by key.
    /// See the [module documentation](self) for filters, pages and tokens.
    pub async fn list_version_groups(
        &self,
        bucket: &str,
        query: &VersionQuery,
    ) -> Result<VersionGroupPage> {
        validate_bucket_name(bucket)?;
        query.validate()?;
        let after = query
            .token
            .as_deref()
            .map(GroupToken::decode)
            .transpose()?
            .map(|token| token.after);
        let client = self.client_for(bucket).await?;
        let mut assembler = Assembler::new(
            &query.filter,
            query.max_groups as usize,
            query.exact.then_some(query.prefix.as_str()),
        );
        let mut marker: Option<(String, Option<String>)> = after.clone().map(|key| (key, None));
        let mut seen = SeenMarkers::new();
        if let Some(marker) = &marker {
            seen.first_time(marker);
        }
        let mut requests = 0usize;
        let mut scanned = 0u64;
        let mut next_token = None;
        loop {
            let out = client
                .list_object_versions()
                .bucket(bucket)
                .prefix(&query.prefix)
                .set_key_marker(marker.as_ref().map(|m| m.0.clone()))
                .set_version_id_marker(marker.as_ref().and_then(|m| m.1.clone()))
                .max_keys(i32::try_from(query.request_keys).unwrap_or(1000))
                .set_request_payer(self.request_payer())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| {
                    Error::from(err).with_context(format!("s3://{bucket}/{}", query.prefix))
                })?;
            requests += 1;
            let entries = page_entries(&out);
            scanned += entries.len() as u64;
            let mut flow = Flow::Continue;
            for entry in entries {
                // A server that restarts at the marker key itself repeats
                // groups that are already complete.
                if after
                    .as_deref()
                    .is_some_and(|after| entry.key.as_str() <= after)
                {
                    continue;
                }
                flow = assembler.push(entry, requests >= SCAN_REQUESTS);
                if flow != Flow::Continue {
                    break;
                }
            }
            match flow {
                Flow::Finished => break,
                Flow::Stop => {
                    next_token = assembler
                        .last_complete
                        .as_deref()
                        .map(GroupToken::encode)
                        .transpose()?;
                    break;
                }
                Flow::Continue => {}
            }
            if out.is_truncated() != Some(true) {
                assembler.finish();
                break;
            }
            let next = match assembler.capped_key() {
                // The rest of a huge key is skipped on the server.
                Some(key) => (key.to_owned(), None),
                None => {
                    let key = out
                        .next_key_marker()
                        .filter(|key| !key.is_empty())
                        .ok_or_else(|| {
                            Error::Remote(format!(
                                "listing versions of s3://{bucket}/{}: the server reported more results but sent no marker",
                                query.prefix
                            ))
                        })?;
                    (
                        key.to_owned(),
                        out.next_version_id_marker().map(str::to_owned),
                    )
                }
            };
            if !seen.first_time(&next) {
                return Err(Error::Remote(format!(
                    "listing versions of s3://{bucket}/{}: the server returned the same continuation marker twice; stopping to avoid an endless listing",
                    query.prefix
                )));
            }
            marker = Some(next);
        }
        Ok(VersionGroupPage {
            groups: assembler.groups,
            next_token,
            scanned,
        })
    }

    /// Whether a version is a delete marker or holds data, and the data
    /// version's details. A missing key or version is
    /// [`ErrorKind::NotFound`].
    pub async fn head_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> Result<VersionHead> {
        if version_id.is_empty() {
            return Err(Error::Usage("the version ID is empty".into()));
        }
        match self.head_object(bucket, key, Some(version_id)).await {
            Ok(head) => Ok(VersionHead::Version(Box::new(head))),
            // S3 answers a HEAD of a delete marker with 405.
            Err(err)
                if err
                    .service()
                    .is_some_and(|service| service.status() == Some(405)) =>
            {
                Ok(VersionHead::DeleteMarker)
            }
            Err(err) => Err(err),
        }
    }

    /// Restores a version: copies it over its key, so that the copy becomes
    /// the current version and every older version stays.
    ///
    /// Content headers, user metadata and tags come with the copy; the
    /// storage class and the server-side encryption (SSE-S3 or SSE-KMS with
    /// its key) of the version are requested again. Fails with
    /// [`ErrorKind::Usage`] for a delete marker (remove it instead, see
    /// [`S3::purge_versions`]), for a version encrypted with a customer key
    /// and for one above 5 GiB (one copy request cannot move it), and with
    /// [`ErrorKind::Conflict`] when the version already is the current one.
    pub async fn restore_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> Result<RestoreOutcome> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        let at = || format!("version {version_id} of s3://{bucket}/{key}");
        let head = match self.head_version(bucket, key, version_id).await? {
            VersionHead::Version(head) => *head,
            VersionHead::DeleteMarker => {
                return Err(Error::Usage(format!(
                    "{} is a delete marker, which has no content to restore; remove the delete marker to bring the previous version back",
                    at()
                )));
            }
        };
        if head.size > MAX_SINGLE_COPY_SIZE {
            return Err(Error::Usage(format!(
                "{} is {} bytes; one copy request moves at most {MAX_SINGLE_COPY_SIZE} bytes, so it cannot be restored here",
                at(),
                head.size
            )));
        }
        let sse = match &head.meta.sse {
            Sse::None => None,
            Sse::S3 => Some((ServerSideEncryption::Aes256, None, None)),
            Sse::Kms { key_id, bucket_key } => Some((
                ServerSideEncryption::AwsKms,
                key_id.clone(),
                Some(*bucket_key),
            )),
            Sse::Customer { .. } => {
                return Err(Error::Usage(format!(
                    "{} is encrypted with a customer-provided key (SSE-C) and cannot be restored without it",
                    at()
                )));
            }
        };
        match self.head_object(bucket, key, None).await {
            Ok(current) => {
                let is_current = match current.version_id.as_deref() {
                    Some(current) => current == version_id,
                    None => version_id == "null",
                };
                if is_current {
                    return Err(Error::Conflict(format!(
                        "{} already is the current version",
                        at()
                    )));
                }
            }
            // The key is deleted: its latest entry is a delete marker.
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let storage_class = head
            .storage_class
            .as_deref()
            .filter(|class| *class != "STANDARD")
            .map(StorageClass::from);
        let (encryption, kms_key, bucket_key) = match sse {
            Some((encryption, kms_key, bucket_key)) => (Some(encryption), kms_key, bucket_key),
            None => (None, None, None),
        };
        // A server-side copy gets no read timeout: some servers answer only
        // when it is done.
        let out = self
            .client_for(bucket)
            .await?
            .copy_object()
            .bucket(bucket)
            .key(key)
            .copy_source(format!(
                "{bucket}/{}?versionId={}",
                percent_encode(key, true),
                percent_encode(version_id, false)
            ))
            .metadata_directive(MetadataDirective::Copy)
            .tagging_directive(TaggingDirective::Copy)
            .set_storage_class(storage_class)
            .set_server_side_encryption(encryption)
            .set_ssekms_key_id(kms_key)
            .set_bucket_key_enabled(bucket_key)
            .set_request_payer(self.request_payer())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("restoring {}", at())))?;
        Ok(RestoreOutcome {
            key: key.to_owned(),
            restored_version_id: version_id.to_owned(),
            version_id: out.version_id().map(str::to_owned),
            etag: out
                .copy_object_result()
                .and_then(|result| result.e_tag())
                .map(unquote),
            size: head.size,
        })
    }

    /// Deletes versions and delete markers for good, 1000 per request. A
    /// version the server could not delete is in [`DeleteOutcome::failures`];
    /// a request that fails as a whole is an error (purging is idempotent, so
    /// the call can be repeated). The same version listed twice counts once.
    pub async fn purge_versions(
        &self,
        bucket: &str,
        versions: &[VersionRef],
    ) -> Result<DeleteOutcome> {
        self.purge_versions_with(bucket, versions, &PurgeOptions::default())
            .await
    }

    /// [`purge_versions`](S3::purge_versions) with an MFA device (for buckets
    /// with MFA delete on) and the governance bypass. A version that is
    /// retained or under legal hold is in [`DeleteOutcome::failures`] with
    /// the server's code and message.
    pub async fn purge_versions_with(
        &self,
        bucket: &str,
        versions: &[VersionRef],
        options: &PurgeOptions,
    ) -> Result<DeleteOutcome> {
        validate_bucket_name(bucket)?;
        let mut unique = HashSet::new();
        let mut wanted = Vec::with_capacity(versions.len());
        for version in versions {
            validate_key(&version.key)?;
            if version.version_id.is_empty() {
                return Err(Error::Usage(format!(
                    "the version ID of s3://{bucket}/{} is empty",
                    version.key
                )));
            }
            if unique.insert(version) {
                wanted.push(version);
            }
        }
        let mut outcome = DeleteOutcome::default();
        if wanted.is_empty() {
            return Ok(outcome);
        }
        let client = self.client_for(bucket).await?;
        for chunk in wanted.chunks(MAX_DELETE_BATCH) {
            let objects = chunk
                .iter()
                .map(|version| {
                    ObjectIdentifier::builder()
                        .key(&version.key)
                        .version_id(&version.version_id)
                        .build()
                })
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(build_error)?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .quiet(true)
                .build()
                .map_err(build_error)?;
            let out = client
                .delete_objects()
                .bucket(bucket)
                .delete(delete)
                .set_mfa(options.mfa.as_ref().map(MfaDeleteAuth::header))
                .set_bypass_governance_retention(options.bypass_governance.then_some(true))
                .set_request_payer(self.request_payer())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| {
                    Error::from(err).with_context(format!("purging versions in bucket '{bucket}'"))
                })?;
            let failures: Vec<DeleteFailure> = out
                .errors()
                .iter()
                .map(|failure| DeleteFailure {
                    key: failure.key().unwrap_or_default().to_owned(),
                    version_id: failure.version_id().map(str::to_owned),
                    code: failure.code().map(str::to_owned),
                    message: failure
                        .message()
                        .unwrap_or("the version could not be deleted")
                        .to_owned(),
                })
                .collect();
            outcome.deleted += chunk.len().saturating_sub(failures.len()) as u64;
            outcome.failures.extend(failures);
        }
        Ok(outcome)
    }
}

/// Queues the download of one version of an object to the local file `dest`
/// (an absolute path whose name is not reserved for partial files). The task
/// is an ordinary download of the queue (progress, pause, resume, retry, the
/// bandwidth cap); only its request names the version. `bytes_total` is the
/// version's size.
///
/// Fails with [`ErrorKind::Usage`] for a folder key, a relative `dest`, a
/// `dest` named like a partial file and a delete marker, and with
/// [`ErrorKind::NotFound`] when the version does not exist.
pub async fn enqueue_download_version(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    bucket: &str,
    key: &str,
    version_id: &str,
    dest: &Path,
) -> Result<TransferTask> {
    validate_bucket_name(bucket)?;
    validate_key(key)?;
    if key.ends_with('/') {
        return Err(Error::Usage(format!(
            "s3://{bucket}/{key} is a folder marker; only versions of objects can be downloaded"
        )));
    }
    if !dest.is_absolute() {
        return Err(Error::Usage(format!(
            "the download file must be an absolute path (got '{}')",
            dest.display()
        )));
    }
    match dest.file_name() {
        Some(name) if is_partial_name(name) => {
            return Err(Error::Usage(format!(
                "'{}' ends in '.bbpart', which is reserved for the temporary files of downloads; choose another name",
                dest.display()
            )));
        }
        Some(_) => {}
        None => {
            return Err(Error::Usage(format!(
                "'{}' does not name a file",
                dest.display()
            )));
        }
    }
    let head = match s3.head_version(bucket, key, version_id).await? {
        VersionHead::Version(head) => *head,
        VersionHead::DeleteMarker => {
            return Err(Error::Usage(format!(
                "version {version_id} of s3://{bucket}/{key} is a delete marker; it has no content to download"
            )));
        }
    };
    let resume = serde_json::to_value(RunnerState::VersionDownload {
        version_id: version_id.to_owned(),
        state: None,
    })
    .map_err(|err| Error::Internal(format!("encoding the download state: {err}")))?;
    queue.add_task_with_resume(
        NewTask {
            account_id: account_id.to_owned(),
            group_id: None,
            kind: TransferKind::Download,
            src: Location::S3 {
                bucket: bucket.to_owned(),
                key: key.to_owned(),
            },
            dst: Some(Location::Local {
                path: dest.to_path_buf(),
            }),
            bytes_total: Some(head.size),
        },
        resume,
    )
}

/// What the assembler wants the listing loop to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Feed the next entry.
    Continue,
    /// The page is complete: resume after `last_complete`.
    Stop,
    /// There is nothing more to list (the exact key's group is complete).
    Finished,
}

/// The entries of one key, while its group is being assembled.
struct Pending {
    key: String,
    total: u64,
    matching: u64,
    kept: Vec<ObjectVersion>,
    deleted: bool,
}

impl Pending {
    fn start(entry: ObjectVersion, filter: &VersionFilter) -> Self {
        let mut pending = Pending {
            key: entry.key.clone(),
            total: 0,
            matching: 0,
            kept: Vec::new(),
            // The newest entry of a key comes first.
            deleted: entry.is_delete_marker,
        };
        pending.add(entry, filter);
        pending
    }

    fn add(&mut self, entry: ObjectVersion, filter: &VersionFilter) {
        if self.capped() {
            return;
        }
        self.total += 1;
        if filter.entry_matches(&entry) {
            self.matching += 1;
            if self.kept.len() < MAX_GROUP_ENTRIES {
                self.kept.push(entry);
            }
        }
    }

    fn capped(&self) -> bool {
        self.total >= GROUP_SCAN_CAP
    }
}

/// Turns the ordered entries of a listing into filtered groups.
struct Assembler<'a> {
    filter: &'a VersionFilter,
    max_groups: usize,
    /// List only this key.
    exact: Option<&'a str>,
    current: Option<Pending>,
    groups: Vec<VersionGroup>,
    kept_entries: usize,
    /// The last key whose group was complete, listed or not.
    last_complete: Option<String>,
}

impl<'a> Assembler<'a> {
    fn new(filter: &'a VersionFilter, max_groups: usize, exact: Option<&'a str>) -> Self {
        Assembler {
            filter,
            max_groups,
            exact,
            current: None,
            groups: Vec::new(),
            kept_entries: 0,
            last_complete: None,
        }
    }

    /// Takes the next entry of the listing. `hurry` ends the page at the
    /// next key boundary.
    fn push(&mut self, entry: ObjectVersion, hurry: bool) -> Flow {
        let filter = self.filter;
        if self.exact.is_some_and(|exact| entry.key != exact) {
            self.finish();
            return Flow::Finished;
        }
        match &mut self.current {
            Some(pending) if pending.key == entry.key => {
                pending.add(entry, filter);
                Flow::Continue
            }
            _ => {
                // A page ends only after a group that was completed here, so
                // that the token always moves on.
                let completed = self.current.is_some();
                self.finish();
                if completed
                    && (self.groups.len() >= self.max_groups
                        || self.kept_entries >= PAGE_ENTRY_BUDGET
                        || hurry)
                {
                    return Flow::Stop;
                }
                self.current = Some(Pending::start(entry, filter));
                Flow::Continue
            }
        }
    }

    /// Completes the group being assembled, if any: lists it when it passes
    /// the key rules.
    fn finish(&mut self) {
        let Some(pending) = self.current.take() else {
            return;
        };
        let truncated = pending.capped() || pending.matching > pending.kept.len() as u64;
        self.last_complete = Some(pending.key.clone());
        if pending.matching == 0 || !self.filter.key_matches(pending.matching) {
            return;
        }
        self.kept_entries += pending.kept.len();
        self.groups.push(VersionGroup {
            key: pending.key,
            versions: pending.kept,
            total: pending.total,
            matching: pending.matching,
            is_deleted: pending.deleted,
            truncated,
        });
    }

    /// The key being assembled, when its entries are not counted any more.
    fn capped_key(&self) -> Option<&str> {
        self.current
            .as_ref()
            .filter(|pending| pending.capped())
            .map(|pending| pending.key.as_str())
    }
}

/// The versions and delete markers of one S3 page in S3's order: by key,
/// newest first (the SDK returns the two kinds as separate lists).
fn page_entries(out: &ListObjectVersionsOutput) -> Vec<ObjectVersion> {
    let mut entries = Vec::with_capacity(out.versions().len() + out.delete_markers().len());
    for version in out.versions() {
        let (Some(key), Some(version_id)) = (version.key(), version.version_id()) else {
            continue;
        };
        entries.push(ObjectVersion {
            key: key.to_owned(),
            version_id: version_id.to_owned(),
            is_latest: version.is_latest().unwrap_or(false),
            is_delete_marker: false,
            size: version
                .size()
                .and_then(|size| u64::try_from(size).ok())
                .unwrap_or(0),
            etag: version.e_tag().map(unquote),
            last_modified: version.last_modified().and_then(to_utc),
            storage_class: version
                .storage_class()
                .map(|class| class.as_str().to_owned()),
        });
    }
    for marker in out.delete_markers() {
        let (Some(key), Some(version_id)) = (marker.key(), marker.version_id()) else {
            continue;
        };
        entries.push(ObjectVersion {
            key: key.to_owned(),
            version_id: version_id.to_owned(),
            is_latest: marker.is_latest().unwrap_or(false),
            is_delete_marker: true,
            size: 0,
            etag: None,
            last_modified: marker.last_modified().and_then(to_utc),
            storage_class: None,
        });
    }
    entries.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then(b.is_latest.cmp(&a.is_latest))
            .then(b.last_modified.cmp(&a.last_modified))
    });
    entries
}

/// Opaque continuation token: the last key whose group is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GroupToken {
    #[serde(rename = "k")]
    after: String,
}

impl GroupToken {
    fn encode(after: &str) -> Result<String> {
        serde_json::to_string(&GroupToken {
            after: after.to_owned(),
        })
        .map_err(|err| Error::Internal(format!("encoding a version token: {err}")))
    }

    fn decode(token: &str) -> Result<Self> {
        serde_json::from_str(token)
            .map_err(|_| Error::Usage("invalid continuation token for a version listing".into()))
    }
}

/// Markers the server sent during one listing; a server that repeats one,
/// directly or in a cycle, would otherwise be followed forever. Only keyed
/// 64-bit hashes are kept.
struct SeenMarkers {
    keys: RandomState,
    seen: HashSet<u64>,
}

impl SeenMarkers {
    fn new() -> Self {
        SeenMarkers {
            keys: RandomState::new(),
            seen: HashSet::new(),
        }
    }

    /// Records `marker`; `false` when it was seen before.
    fn first_time<T: Hash + ?Sized>(&mut self, marker: &T) -> bool {
        self.seen.insert(self.keys.hash_one(marker))
    }
}

fn build_error(err: BuildError) -> Error {
    Error::Internal(format!("failed to build an S3 request: {err}"))
}

fn to_utc(time: &aws_smithy_types::DateTime) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos())
}

fn unquote(etag: &str) -> String {
    etag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(etag)
        .to_owned()
}

/// Percent-encodes everything except RFC 3986 unreserved characters (and `/`
/// when `keep_slash` is set).
fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap()
    }

    fn version(key: &str, id: &str, day: u32) -> ObjectVersion {
        ObjectVersion {
            key: key.into(),
            version_id: id.into(),
            is_latest: false,
            is_delete_marker: false,
            size: 10,
            etag: Some("e".into()),
            last_modified: Some(at(day)),
            storage_class: Some("STANDARD".into()),
        }
    }

    fn marker(key: &str, id: &str, day: u32) -> ObjectVersion {
        ObjectVersion {
            is_delete_marker: true,
            size: 0,
            etag: None,
            storage_class: None,
            ..version(key, id, day)
        }
    }

    /// Feeds `entries` to an assembler and returns the groups as
    /// `key: version ids`.
    fn assemble(filter: &VersionFilter, entries: Vec<ObjectVersion>) -> Vec<String> {
        let mut assembler = Assembler::new(filter, 100, None);
        for entry in entries {
            assert_eq!(assembler.push(entry, false), Flow::Continue);
        }
        assembler.finish();
        assembler
            .groups
            .iter()
            .map(|group| {
                let ids: Vec<&str> = group
                    .versions
                    .iter()
                    .map(|v| v.version_id.as_str())
                    .collect();
                format!("{}: {}", group.key, ids.join(" "))
            })
            .collect()
    }

    fn sample() -> Vec<ObjectVersion> {
        vec![
            version("a", "a3", 20),
            version("a", "a2", 10),
            version("a", "a1", 5),
            marker("b", "bm", 18),
            version("b", "b1", 9),
            version("c", "c1", 12),
            marker("d", "dm", 22),
        ]
    }

    #[test]
    fn the_default_filter_lists_every_key_with_every_entry() {
        assert_eq!(
            assemble(&VersionFilter::default(), sample()),
            ["a: a3 a2 a1", "b: bm b1", "c: c1", "d: dm"]
        );
    }

    #[test]
    fn delete_markers_and_versions_can_be_shown_or_hidden() {
        let only_markers = VersionFilter {
            delete_markers: DeleteMarkers::Only,
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&only_markers, sample()), ["b: bm", "d: dm"]);
        let no_markers = VersionFilter {
            delete_markers: DeleteMarkers::Exclude,
            ..VersionFilter::default()
        };
        assert_eq!(
            assemble(&no_markers, sample()),
            ["a: a3 a2 a1", "b: b1", "c: c1"]
        );
        // Versions off: only the markers that are included remain.
        let versions_off = VersionFilter {
            show_versions: false,
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&versions_off, sample()), ["b: bm", "d: dm"]);
        let both_off = VersionFilter {
            show_versions: false,
            delete_markers: DeleteMarkers::Exclude,
            ..VersionFilter::default()
        };
        assert_eq!(both_off.validate().unwrap_err().kind(), ErrorKind::Usage);
    }

    #[test]
    fn the_date_interval_is_half_open() {
        let filter = VersionFilter {
            modified_after: Some(at(9)),
            modified_before: Some(at(20)),
            ..VersionFilter::default()
        };
        // a3 (day 20) is excluded by the open end, b1 (day 9) is included.
        assert_eq!(assemble(&filter, sample()), ["a: a2", "b: bm b1", "c: c1"]);
        let after_only = VersionFilter {
            modified_after: Some(at(20)),
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&after_only, sample()), ["a: a3", "d: dm"]);
        // An entry without a time never satisfies a bound.
        let mut undated = version("u", "u1", 1);
        undated.last_modified = None;
        assert!(assemble(&after_only, vec![undated.clone()]).is_empty());
        assert_eq!(
            assemble(&VersionFilter::default(), vec![undated]),
            ["u: u1"]
        );
    }

    #[test]
    fn key_rules_count_the_entries_left_after_the_entry_rules() {
        let multiple = VersionFilter {
            multiple_versions: true,
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&multiple, sample()), ["a: a3 a2 a1", "b: bm b1"]);
        let more_than_two = VersionFilter {
            count_greater_than: Some(2),
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&more_than_two, sample()), ["a: a3 a2 a1"]);
        let fewer_than_two = VersionFilter {
            count_less_than: Some(2),
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&fewer_than_two, sample()), ["c: c1", "d: dm"]);
        let between = VersionFilter {
            count_greater_than: Some(1),
            count_less_than: Some(3),
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&between, sample()), ["b: bm b1"]);
        // The count is of what the entry rules keep: with delete markers
        // hidden, key b has one entry left.
        let hidden = VersionFilter {
            multiple_versions: true,
            delete_markers: DeleteMarkers::Exclude,
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&hidden, sample()), ["a: a3 a2 a1"]);
        // A key with nothing left is not listed even without key rules.
        let dated = VersionFilter {
            modified_after: Some(at(21)),
            ..VersionFilter::default()
        };
        assert_eq!(assemble(&dated, sample()), ["d: dm"]);
    }

    #[test]
    fn impossible_filters_are_usage_errors() {
        let bad = [
            VersionFilter {
                modified_after: Some(at(5)),
                modified_before: Some(at(5)),
                ..VersionFilter::default()
            },
            VersionFilter {
                modified_after: Some(at(6)),
                modified_before: Some(at(5)),
                ..VersionFilter::default()
            },
            VersionFilter {
                count_greater_than: Some(5),
                count_less_than: Some(6),
                ..VersionFilter::default()
            },
            VersionFilter {
                count_greater_than: Some(5),
                count_less_than: Some(3),
                ..VersionFilter::default()
            },
            VersionFilter {
                multiple_versions: true,
                count_less_than: Some(2),
                ..VersionFilter::default()
            },
            VersionFilter {
                count_less_than: Some(1),
                ..VersionFilter::default()
            },
            VersionFilter {
                count_less_than: Some(0),
                ..VersionFilter::default()
            },
        ];
        for filter in bad {
            let err = filter.validate().unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{filter:?}");
        }
        for filter in [
            VersionFilter::default(),
            VersionFilter {
                count_greater_than: Some(1),
                count_less_than: Some(3),
                ..VersionFilter::default()
            },
            VersionFilter {
                count_less_than: Some(2),
                ..VersionFilter::default()
            },
            VersionFilter {
                modified_after: Some(at(5)),
                modified_before: Some(at(6)),
                ..VersionFilter::default()
            },
        ] {
            filter.validate().unwrap();
        }
    }

    #[test]
    fn a_page_ends_at_a_key_boundary_and_names_the_last_complete_key() {
        let filter = VersionFilter::default();
        let mut assembler = Assembler::new(&filter, 2, None);
        let mut flows = Vec::new();
        for entry in sample() {
            flows.push(assembler.push(entry, false));
            if flows.last() == Some(&Flow::Stop) {
                break;
            }
        }
        // Keys a and b fill the page; the first entry of c ends it.
        assert_eq!(flows.last(), Some(&Flow::Stop));
        assert_eq!(assembler.last_complete.as_deref(), Some("b"));
        assert_eq!(assembler.groups.len(), 2);
        assert!(assembler.current.is_none());
    }

    #[test]
    fn hurry_ends_the_page_even_when_every_key_is_rejected() {
        let filter = VersionFilter {
            multiple_versions: true,
            ..VersionFilter::default()
        };
        let mut assembler = Assembler::new(&filter, 10, None);
        assert_eq!(assembler.push(version("a", "a1", 1), true), Flow::Continue);
        assert_eq!(assembler.push(version("a", "a0", 1), true), Flow::Continue);
        // A rejected key still moves the resume point.
        assert_eq!(assembler.push(version("b", "b1", 1), true), Flow::Stop);
        assert_eq!(assembler.last_complete.as_deref(), Some("a"));
        assert_eq!(assembler.groups.len(), 1);
        let mut assembler = Assembler::new(&filter, 10, None);
        assert_eq!(assembler.push(version("c", "c1", 1), true), Flow::Continue);
        assert_eq!(assembler.push(version("d", "d1", 1), true), Flow::Stop);
        assert_eq!(assembler.last_complete.as_deref(), Some("c"));
        assert!(assembler.groups.is_empty());
    }

    #[test]
    fn an_exact_listing_ends_at_the_first_other_key() {
        let filter = VersionFilter::default();
        let mut assembler = Assembler::new(&filter, 10, Some("a"));
        assert_eq!(assembler.push(version("a", "a2", 2), false), Flow::Continue);
        assert_eq!(assembler.push(version("a", "a1", 1), false), Flow::Continue);
        assert_eq!(
            assembler.push(version("a.bak", "x1", 1), false),
            Flow::Finished
        );
        assert_eq!(assembler.groups.len(), 1);
        assert_eq!(assembler.groups[0].versions.len(), 2);
    }

    #[test]
    fn groups_report_their_state_and_counts() {
        let filter = VersionFilter {
            delete_markers: DeleteMarkers::Exclude,
            ..VersionFilter::default()
        };
        let mut assembler = Assembler::new(&filter, 10, None);
        for entry in [
            marker("k", "m2", 30),
            version("k", "v2", 20),
            marker("k", "m1", 15),
            version("k", "v1", 10),
        ] {
            assembler.push(entry, false);
        }
        assembler.finish();
        let group = &assembler.groups[0];
        assert!(group.is_deleted);
        assert_eq!((group.total, group.matching), (4, 2));
        assert!(!group.truncated);
        assert_eq!(group.versions.len(), 2);
    }

    #[test]
    fn a_huge_key_is_capped_and_truncated() {
        let filter = VersionFilter::default();
        let mut assembler = Assembler::new(&filter, 10, None);
        for index in 0..(MAX_GROUP_ENTRIES + 5) {
            assembler.push(version("big", &format!("v{index}"), 1), false);
        }
        assert_eq!(assembler.capped_key(), None);
        assembler.finish();
        let group = &assembler.groups[0];
        assert_eq!(group.versions.len(), MAX_GROUP_ENTRIES);
        assert_eq!(group.matching, (MAX_GROUP_ENTRIES + 5) as u64);
        assert!(group.truncated);
        // The newest entries are the ones that are kept.
        assert_eq!(group.versions[0].version_id, "v0");

        let mut capped = Pending::start(version("huge", "v0", 1), &filter);
        capped.total = GROUP_SCAN_CAP;
        assert!(capped.capped());
        capped.add(version("huge", "late", 1), &filter);
        assert_eq!(capped.total, GROUP_SCAN_CAP);
        let mut assembler = Assembler::new(&filter, 10, None);
        assembler.current = Some(capped);
        assert_eq!(assembler.capped_key(), Some("huge"));
        assembler.finish();
        assert!(assembler.groups[0].truncated);
    }

    #[test]
    fn the_page_entry_budget_ends_a_page() {
        let filter = VersionFilter::default();
        let mut assembler = Assembler::new(&filter, 1000, None);
        let per_key = MAX_GROUP_ENTRIES;
        let mut stopped_at = None;
        for key in 0..10 {
            for index in 0..per_key {
                let flow =
                    assembler.push(version(&format!("k{key}"), &format!("v{index}"), 1), false);
                if flow == Flow::Stop {
                    stopped_at = Some(key);
                }
            }
            if stopped_at.is_some() {
                break;
            }
        }
        // Five keys of 1000 entries fill the budget; the sixth is not started.
        assert_eq!(stopped_at, Some(5));
        assert_eq!(assembler.groups.len(), 5);
        assert_eq!(assembler.last_complete.as_deref(), Some("k4"));
    }

    #[test]
    fn tokens_round_trip_and_reject_garbage() {
        let token = GroupToken::encode("dir/\u{e9}.txt").unwrap();
        assert_eq!(GroupToken::decode(&token).unwrap().after, "dir/\u{e9}.txt");
        let err = GroupToken::decode("not a token").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(GroupToken::decode("{\"v\":1}").is_err());
    }

    #[test]
    fn markers_that_repeat_are_noticed() {
        let mut seen = SeenMarkers::new();
        assert!(seen.first_time(&("a".to_owned(), Some("v1".to_owned()))));
        assert!(seen.first_time(&("a".to_owned(), None::<String>)));
        assert!(!seen.first_time(&("a".to_owned(), Some("v1".to_owned()))));
    }

    #[test]
    fn queries_are_validated() {
        VersionQuery::new("").validate().unwrap();
        let mut query = VersionQuery::new("a.txt");
        query.exact = true;
        query.validate().unwrap();
        query.prefix = String::new();
        assert_eq!(query.validate().unwrap_err().kind(), ErrorKind::Usage);
        for bad in [
            VersionQuery {
                max_groups: 0,
                ..VersionQuery::new("")
            },
            VersionQuery {
                max_groups: MAX_GROUPS_PER_PAGE + 1,
                ..VersionQuery::new("")
            },
            VersionQuery {
                request_keys: 0,
                ..VersionQuery::new("")
            },
            VersionQuery {
                request_keys: MAX_LIST_KEYS + 1,
                ..VersionQuery::new("")
            },
        ] {
            assert_eq!(bad.validate().unwrap_err().kind(), ErrorKind::Usage);
        }
    }

    #[test]
    fn copy_sources_are_percent_encoded() {
        assert_eq!(percent_encode("a b/c+d.txt", true), "a%20b/c%2Bd.txt");
        assert_eq!(percent_encode("3HL4kq+rm/S=", false), "3HL4kq%2Brm%2FS%3D");
        assert_eq!(percent_encode("null", false), "null");
    }

    #[test]
    fn filters_and_groups_have_a_stable_shape() {
        let filter: VersionFilter = serde_json::from_value(serde_json::json!({
            "show_versions": false,
            "delete_markers": "only",
            "multiple_versions": true,
            "count_greater_than": 2,
            "count_less_than": 9,
            "modified_after": "2026-10-01T00:00:00Z",
            "modified_before": "2026-10-29T08:15:00Z"
        }))
        .unwrap();
        assert!(!filter.show_versions);
        assert_eq!(filter.delete_markers, DeleteMarkers::Only);
        assert_eq!(filter.count_greater_than, Some(2));
        assert_eq!(
            filter.modified_before,
            Some(Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap())
        );
        // Missing fields take the defaults.
        let partial: VersionFilter =
            serde_json::from_value(serde_json::json!({ "multiple_versions": true })).unwrap();
        assert!(partial.show_versions && partial.multiple_versions);
        assert_eq!(partial.delete_markers, DeleteMarkers::Include);
        assert_eq!(
            serde_json::to_value(VersionFilter::default()).unwrap(),
            serde_json::json!({
                "show_versions": true,
                "delete_markers": "include",
                "multiple_versions": false,
                "count_greater_than": null,
                "count_less_than": null,
                "modified_after": null,
                "modified_before": null
            })
        );
        let group = VersionGroup {
            key: "k".into(),
            versions: vec![version("k", "v1", 1)],
            total: 3,
            matching: 1,
            is_deleted: true,
            truncated: false,
        };
        let value = serde_json::to_value(VersionGroupPage {
            groups: vec![group.clone()],
            next_token: Some("t".into()),
            scanned: 7,
        })
        .unwrap();
        assert_eq!(value["scanned"], 7);
        assert_eq!(value["next_token"], "t");
        assert_eq!(value["groups"][0], serde_json::to_value(&group).unwrap());
        let mut keys: Vec<&str> = value["groups"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "is_deleted",
                "key",
                "matching",
                "total",
                "truncated",
                "versions"
            ]
        );
        assert_eq!(
            serde_json::to_value(RestoreOutcome {
                key: "k".into(),
                restored_version_id: "v1".into(),
                version_id: Some("v9".into()),
                etag: None,
                size: 4,
            })
            .unwrap(),
            serde_json::json!({
                "key": "k",
                "restored_version_id": "v1",
                "version_id": "v9",
                "etag": null,
                "size": 4
            })
        );
        assert_eq!(
            serde_json::to_value(VersionRef {
                key: "k".into(),
                version_id: "v1".into()
            })
            .unwrap(),
            serde_json::json!({ "key": "k", "version_id": "v1" })
        );
    }
}
