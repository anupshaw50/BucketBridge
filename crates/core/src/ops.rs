//! The S3 transport: bucket and object operations returning core model types.
//!
//! [`S3`] wraps an `aws_sdk_s3::Client` for one account. It is cheap to
//! clone (the clients and caches are reference counted) and safe to share
//! between tasks. Every method validates its input, applies
//! `RequestPayer::Requester` when the account has `request_payer` set and the
//! operation supports it, and converts failures into [`Error`] with the
//! bucket or URI as context.
//!
//! Requests about a bucket go to the bucket's region ([`S3::client_for`]);
//! requests that neither send nor receive a large body carry the read
//! timeout ([`S3::metadata_timeout`]).
//!
//! Streaming uploads and downloads are not here; the transfer engine builds
//! them on top of [`S3::client_for`], [`S3::metadata_timeout`] and
//! [`S3::request_payer`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::error::BuildError;
use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    BucketLocationConstraint, CompletedMultipartUpload, CompletedPart, CreateBucketConfiguration,
    Delete, MetadataDirective, ObjectIdentifier, ObjectLockLegalHoldStatus, ObjectLockMode,
    RequestPayer, ServerSideEncryption,
};
use aws_smithy_runtime_api::client::orchestrator::HttpRequest;
use aws_smithy_types::error::display::DisplayErrorContext;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::client::{
    ClientOptions, Credentials, build_client, read_timeout_override, validate_region,
};
use crate::error::{Error, ErrorKind, Result, ServiceError};
use crate::model::{
    Account, Bucket, BucketKind, Checksums, CopyOutcome, DeleteFailure, DeleteOutcome, LockMode,
    MultipartUpload, ObjectEntry, ObjectHead, ObjectLock, ObjectMeta, ObjectPage, ObjectVersion,
    Sse, VersionPage,
};
use crate::objectmeta::{CopyChanges, HeaderSet, SSE_C_ALGORITHM};
use crate::s3uri::{S3Uri, validate_bucket_name, validate_key, validate_new_bucket_name};
use crate::settings::MIB;

/// Largest page S3 returns for listings.
pub const MAX_LIST_KEYS: u32 = 1000;

/// Buckets requested per `ListBuckets` page. Sending the parameter at all
/// makes S3 report each bucket's region, and accounts with a raised bucket
/// quota must paginate.
const MAX_BUCKETS_PER_PAGE: i32 = 1000;

/// Response header naming a bucket's region (S3 sends it with `HeadBucket`
/// answers 200, 301 and 403, among others).
const BUCKET_REGION_HEADER: &str = "x-amz-bucket-region";

/// Most keys one `DeleteObjects` request may carry.
pub const MAX_DELETE_BATCH: usize = 1000;

/// Shortest presigned URL lifetime accepted.
pub const MIN_PRESIGN_EXPIRY: Duration = Duration::from_secs(1);

/// Longest presigned URL lifetime SigV4 allows (7 days).
pub const MAX_PRESIGN_EXPIRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Largest object `CopyObject` can copy in one request (5 GiB).
pub const MAX_SINGLE_COPY_SIZE: u64 = 5 * 1024 * MIB;

/// Most parts a multipart upload may have.
pub const MAX_PARTS: u64 = 10_000;

/// Smallest part size (except for the last part).
pub const MIN_PART_SIZE: u64 = 5 * MIB;

/// Largest part size.
pub const MAX_PART_SIZE: u64 = 5 * 1024 * MIB;

/// How a server-side copy is split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOptions {
    /// Objects larger than this many bytes are copied with
    /// `UploadPartCopy`; at most [`MAX_SINGLE_COPY_SIZE`] (the default).
    pub multipart_threshold: u64,
    /// Part size for multipart copies, between [`MIN_PART_SIZE`] and
    /// [`MAX_PART_SIZE`]; grown automatically so that a copy never needs more
    /// than [`MAX_PARTS`] parts. Default 256 MiB.
    pub part_size: u64,
    /// Parts copied at the same time, 1..=64. Default 4.
    pub concurrency: usize,
}

impl Default for CopyOptions {
    fn default() -> Self {
        CopyOptions {
            multipart_threshold: MAX_SINGLE_COPY_SIZE,
            part_size: 256 * MIB,
            concurrency: 4,
        }
    }
}

impl CopyOptions {
    fn validate(&self) -> Result<()> {
        if !(1..=MAX_SINGLE_COPY_SIZE).contains(&self.multipart_threshold) {
            return Err(Error::Usage(format!(
                "the multipart copy threshold must be between 1 and {MAX_SINGLE_COPY_SIZE} bytes"
            )));
        }
        if !(MIN_PART_SIZE..=MAX_PART_SIZE).contains(&self.part_size) {
            return Err(Error::Usage(format!(
                "the copy part size must be between {MIN_PART_SIZE} and {MAX_PART_SIZE} bytes"
            )));
        }
        if !(1..=64).contains(&self.concurrency) {
            return Err(Error::Usage(
                "the copy concurrency must be between 1 and 64".into(),
            ));
        }
        Ok(())
    }
}

/// Picks the part size for an object of `size` bytes: at least `preferred`,
/// grown (in whole MiB) until the object fits in [`MAX_PARTS`] parts.
///
/// Fails when even [`MAX_PART_SIZE`] parts cannot hold the object.
pub fn part_size_for(size: u64, preferred: u64) -> Result<u64> {
    let minimum = size.div_ceil(MAX_PARTS).div_ceil(MIB) * MIB;
    let part_size = preferred.max(minimum).max(MIN_PART_SIZE);
    if part_size > MAX_PART_SIZE {
        return Err(Error::Usage(format!(
            "an object of {size} bytes cannot be split into at most {MAX_PARTS} parts of at most {MAX_PART_SIZE} bytes"
        )));
    }
    Ok(part_size)
}

/// Handle for running S3 operations as one account.
#[derive(Debug, Clone)]
pub struct S3 {
    client: Client,
    request_payer: bool,
    bucket_kind: BucketKind,
    read_timeout: Duration,
    resolve_regions: bool,
    regions: Arc<RegionCache>,
    /// The custom endpoint URL of the account, when it has one.
    endpoint: Option<String>,
}

impl S3 {
    /// Builds the client for `account` (see [`build_client`]). Requests that
    /// neither send nor receive a large body get `options.read_timeout`.
    pub fn new(
        account: &Account,
        credentials: &Credentials,
        options: &ClientOptions,
    ) -> Result<Self> {
        let client = build_client(account, credentials, options)?;
        let mut s3 = S3::from_client(client, account.request_payer, account.bucket_kind());
        s3.read_timeout = options.read_timeout;
        s3.endpoint = account.endpoint.clone();
        Ok(s3)
    }

    /// Builds the client for `account` with a refreshing credentials
    /// provider (see [`crate::client::build_client_with_provider`]).
    pub fn with_provider(
        account: &Account,
        provider: aws_credential_types::provider::SharedCredentialsProvider,
        options: &ClientOptions,
    ) -> Result<Self> {
        let client = crate::client::build_client_with_provider(account, provider, options)?;
        let mut s3 = S3::from_client(client, account.request_payer, account.bucket_kind());
        s3.read_timeout = options.read_timeout;
        Ok(s3)
    }

    /// Wraps an existing client, which should come from [`build_client`]
    /// (connect timeout, no read timeout, `Content-MD5`). Requests that
    /// neither send nor receive a large body get the default read timeout of
    /// [`ClientOptions`]; use [`S3::new`] to choose another.
    ///
    /// Bucket regions are resolved (see [`S3::client_for`]) unless
    /// `bucket_kind` is [`BucketKind::External`], which stands for a custom
    /// endpoint.
    pub fn from_client(client: Client, request_payer: bool, bucket_kind: BucketKind) -> Self {
        S3 {
            client,
            request_payer,
            bucket_kind,
            read_timeout: ClientOptions::default().read_timeout,
            resolve_regions: bucket_kind != BucketKind::External,
            regions: Arc::default(),
            endpoint: None,
        }
    }

    /// Turns bucket-region resolution on or off, overriding the default (on
    /// for AWS, off for accounts with a custom endpoint, which always use the
    /// account region). Tests use it to emulate AWS with a mock endpoint.
    #[must_use]
    pub fn with_region_resolution(mut self, enabled: bool) -> Self {
        self.resolve_regions = enabled;
        self
    }

    /// The SDK client for the account region. Requests about a bucket should
    /// use [`S3::client_for`] instead, which addresses the bucket's region.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The SDK client for requests about `bucket`, addressed to the bucket's
    /// region.
    ///
    /// For AWS accounts the region is resolved once per bucket and cached:
    /// from [`S3::list_buckets`] when it reported the bucket's region,
    /// otherwise with a `HeadBucket` request, whose `x-amz-bucket-region`
    /// header S3 sends with 200, 301 and 403 answers. When the answer has no
    /// such header (for example because the bucket does not exist) the
    /// account region is used and nothing is cached. Accounts with a custom
    /// endpoint always use the account region and send no extra request.
    ///
    /// The returned client shares credentials, retry, timeout, checksum and
    /// stalled-stream settings, interceptors and an injected HTTP client with
    /// [`S3::client`]; there is one per region, built on first use. Combine
    /// it with [`S3::metadata_timeout`] and [`S3::request_payer`] for raw SDK
    /// calls.
    ///
    /// Fails for an invalid bucket name, when the `HeadBucket` request gets
    /// no answer (timeout, connection failure), or when the server names an
    /// invalid region.
    pub async fn client_for(&self, bucket: &str) -> Result<Client> {
        validate_bucket_name(bucket)?;
        if !self.resolve_regions {
            return Ok(self.client.clone());
        }
        let cell = self.regions.cell(bucket);
        match cell.get_or_try_init(|| self.resolve_region(bucket)).await {
            Ok(region) => Ok(self.client_in(region)),
            Err(Unresolved::UseAccountRegion) => Ok(self.client.clone()),
            Err(Unresolved::Failed(err)) => Err(err),
        }
    }

    /// A per-request configuration override that applies the read timeout
    /// ([`ClientOptions::read_timeout`]) to one request. [`S3`] uses it for
    /// its own requests; raw SDK calls apply it like this:
    ///
    /// ```no_run
    /// # async fn example(s3: &bucketbridge_core::S3) -> bucketbridge_core::Result<()> {
    /// let client = s3.client_for("my-bucket").await?;
    /// let head = client
    ///     .head_object()
    ///     .bucket("my-bucket")
    ///     .key("photo.jpg")
    ///     .set_request_payer(s3.request_payer())
    ///     .customize()
    ///     .config_override(s3.metadata_timeout())
    ///     .send()
    ///     .await?;
    /// # let _ = head;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The clients have no read timeout of their own, because the SDK
    /// measures it from the start of a request until the response headers
    /// arrive, which includes sending the request body. Use this override for
    /// every request that sends no large body: listings, HEAD, deletes,
    /// tagging, CreateMultipartUpload, AbortMultipartUpload, ListParts and
    /// bucket operations. GetObject may use it too: the timeout ends when the
    /// response headers arrive, and stalled-stream protection covers the
    /// body. Never use it for PutObject or UploadPart with a body, nor for
    /// CopyObject, UploadPartCopy and CompleteMultipartUpload (a server may
    /// answer those only when the copy or assembly is done); they rely on
    /// stalled-stream protection alone.
    pub fn metadata_timeout(&self) -> aws_sdk_s3::config::Builder {
        read_timeout_override(self.read_timeout)
    }

    /// `Some(RequestPayer::Requester)` when the account pays for requests;
    /// pass it to `set_request_payer` on raw SDK calls.
    pub fn request_payer(&self) -> Option<RequestPayer> {
        self.request_payer.then_some(RequestPayer::Requester)
    }

    /// True when both handles talk to the same service endpoint (both AWS,
    /// or the same custom endpoint URL). Only then can one account ask the
    /// service to copy an object that another account's bucket holds.
    pub fn same_endpoint(&self, other: &S3) -> bool {
        let normal = |endpoint: &Option<String>| {
            endpoint
                .as_deref()
                .map(|url| url.trim_end_matches('/').to_ascii_lowercase())
        };
        match (normal(&self.endpoint), normal(&other.endpoint)) {
            (Some(a), Some(b)) => a == b,
            // Without a custom endpoint both are AWS, unless the handle was
            // wrapped around a client of unknown origin.
            (None, None) => {
                self.bucket_kind != BucketKind::External
                    && other.bucket_kind != BucketKind::External
            }
            _ => false,
        }
    }

    /// The badge reported in [`Bucket::kind`].
    pub fn bucket_kind(&self) -> BucketKind {
        self.bucket_kind
    }

    // ---- buckets ----

    /// Lists every bucket visible to the account, sorted by name, 1000 per
    /// request, following continuation tokens. Regions the server reports
    /// are remembered for [`S3::client_for`].
    pub async fn list_buckets(&self) -> Result<Vec<Bucket>> {
        let mut buckets = Vec::new();
        let mut token: Option<String> = None;
        let mut seen = SeenTokens::new();
        loop {
            let out = self
                .client
                .list_buckets()
                .max_buckets(MAX_BUCKETS_PER_PAGE)
                .set_continuation_token(token.clone())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context("listing buckets"))?;
            let owner = out
                .owner()
                .and_then(|owner| owner.display_name().or(owner.id()))
                .map(str::to_owned);
            for bucket in out.buckets() {
                let Some(name) = bucket.name() else { continue };
                let region = bucket.bucket_region().map(str::to_owned);
                if let Some(region) = &region {
                    self.learn_region(name, region);
                }
                buckets.push(Bucket {
                    name: name.to_owned(),
                    region,
                    creation_date: bucket.creation_date().and_then(to_utc),
                    owner: owner.clone(),
                    kind: self.bucket_kind,
                });
            }
            let next = out
                .continuation_token()
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            match next {
                None => {
                    buckets.sort_by(|a, b| a.name.cmp(&b.name));
                    return Ok(buckets);
                }
                Some(next) if !seen.first_time(next.as_str()) => {
                    return Err(repeated_token("listing buckets"));
                }
                Some(next) => token = Some(next),
            }
        }
    }

    /// Creates a bucket.
    ///
    /// `region` defaults to the account region. For AWS a location
    /// constraint is sent unless the region is `us-east-1`, and the request
    /// is signed for and sent to the target region. For custom endpoints a
    /// location constraint is only sent when `region` is given explicitly
    /// (and is not `us-east-1`), because many S3-compatible servers reject
    /// constraints they do not know.
    pub async fn create_bucket(&self, name: &str, region: Option<&str>) -> Result<()> {
        validate_new_bucket_name(name, self.bucket_kind)?;
        if let Some(region) = region {
            crate::client::validate_region(region)?;
        }
        let client_region = self.client.config().region().map(|r| r.as_ref().to_owned());
        let target = region.map(str::to_owned).or(client_region.clone());
        let send_constraint = match self.bucket_kind {
            BucketKind::Aws => true,
            BucketKind::External | BucketKind::CloudFront => region.is_some(),
        };
        let mut request = self.client.create_bucket().bucket(name);
        if send_constraint
            && let Some(target) = target.as_deref()
            && target != "us-east-1"
        {
            request = request.create_bucket_configuration(
                CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(target))
                    .build(),
            );
        }
        let ctx = || format!("creating bucket '{name}'");
        let mut config = self.metadata_timeout();
        if let Some(target) = &target
            && client_region.as_deref() != Some(target.as_str())
        {
            config = config.region(Region::new(target.clone()));
        }
        request
            .customize()
            .config_override(config)
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(ctx()))?;
        if let Some(target) = &target {
            self.learn_region(name, target);
        }
        Ok(())
    }

    /// Deletes an empty bucket.
    ///
    /// Fails with [`ErrorKind::Conflict`] while the bucket holds objects, or
    /// object versions or delete markers on servers that support
    /// versioning. The checks run before `DeleteBucket` so that servers that
    /// would delete a non-empty bucket never get the chance; a
    /// `BucketNotEmpty` reply is mapped to a conflict as well. The versions
    /// check is skipped when the server does not implement it or denies it.
    pub async fn delete_bucket(&self, name: &str) -> Result<()> {
        validate_bucket_name(name)?;
        let ctx = || format!("bucket '{name}'");
        let client = self.client_for(name).await?;
        let objects = client
            .list_objects_v2()
            .bucket(name)
            .max_keys(1)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(ctx()))?;
        // A truncated page without contents still means more keys follow
        // (S3 can return such pages when many delete markers are present).
        if !objects.contents().is_empty() || objects.is_truncated() == Some(true) {
            return Err(Error::Conflict(format!(
                "bucket '{name}' is not empty; delete its objects first"
            )));
        }
        let versions = client
            .list_object_versions()
            .bucket(name)
            .max_keys(1)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        match versions {
            Ok(out) => {
                if !out.versions().is_empty() || !out.delete_markers().is_empty() {
                    return Err(Error::Conflict(format!(
                        "bucket '{name}' still contains object versions or delete markers; delete them first"
                    )));
                }
            }
            Err(err) => {
                let err = Error::from(err);
                let unsupported = err
                    .service()
                    .is_some_and(|s| s.is_not_implemented() || s.status() == Some(405));
                if !(unsupported || err.kind() == ErrorKind::Auth) {
                    return Err(err.with_context(ctx()));
                }
            }
        }
        client
            .delete_bucket()
            .bucket(name)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(ctx()))?;
        // The name may be reused for a bucket in another region.
        self.regions.forget(name);
        Ok(())
    }

    // ---- listing ----

    /// Lists one page of objects under `prefix`.
    ///
    /// Unless `flat` is set, the listing uses the delimiter `/`: common
    /// prefixes become entries with `is_prefix` set and come first, followed
    /// by the objects directly under `prefix`. The entry whose key equals
    /// `prefix` (a folder marker) is never returned. `token` is the
    /// `next_token` of the previous page; `max_keys` must be in 1..=1000.
    ///
    /// A server that answers with the token it was given (or, while pages
    /// holding only the folder marker are skipped, with any token it sent
    /// before) is a `remote` error, so a caller following `next_token` never
    /// loops on the same page.
    pub async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        flat: bool,
        token: Option<&str>,
        max_keys: u32,
    ) -> Result<ObjectPage> {
        validate_bucket_name(bucket)?;
        check_max_keys(max_keys)?;
        let delimiter = (!flat).then_some("/");
        let mut seen = SeenTokens::new();
        if let Some(token) = token {
            seen.first_time(token);
        }
        let mut token = token.map(str::to_owned);
        loop {
            let page = self
                .fetch_objects(bucket, prefix, delimiter, token.as_deref(), max_keys)
                .await?;
            if let Some(next) = &page.next_token
                && !seen.first_time(next.as_str())
            {
                return Err(repeated_token(&format!("listing s3://{bucket}/{prefix}")));
            }
            let entries: Vec<ObjectEntry> = page
                .entries
                .into_iter()
                .filter(|e| e.key != prefix)
                .collect();
            // A page may consist of nothing but the folder marker; skip it
            // instead of handing out an empty page with a token.
            if !entries.is_empty() || page.next_token.is_none() {
                return Ok(ObjectPage {
                    entries,
                    next_token: page.next_token,
                });
            }
            token = page.next_token;
        }
    }

    /// Streams every object under `prefix` recursively, 1000 keys per
    /// request, without holding more than one page in memory.
    ///
    /// Unlike [`S3::list_objects`] this includes an object whose key equals
    /// `prefix` (a folder marker), because recursive operations such as
    /// [`S3::delete_prefix`] must see it. Common prefixes are never returned.
    /// A server that sends a continuation token twice (also in a cycle such
    /// as A, B, A) ends the stream with a `remote` error.
    pub fn list_all_stream(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> impl Stream<Item = Result<ObjectEntry>> + Send + 'static {
        let s3 = self.clone();
        let bucket = bucket.to_owned();
        let prefix = prefix.to_owned();
        // `None` = finished; otherwise the token of the next page (`None`
        // for the first) and the tokens seen so far.
        let start = Some((None::<String>, SeenTokens::new()));
        let pages = futures::stream::try_unfold(start, move |state| {
            let s3 = s3.clone();
            let bucket = bucket.clone();
            let prefix = prefix.clone();
            async move {
                let Some((token, mut seen)) = state else {
                    return Ok(None);
                };
                validate_bucket_name(&bucket)?;
                let page = s3
                    .fetch_objects(&bucket, &prefix, None, token.as_deref(), MAX_LIST_KEYS)
                    .await?;
                let next = match page.next_token {
                    None => None,
                    Some(next) if !seen.first_time(next.as_str()) => {
                        return Err(repeated_token(&format!("listing s3://{bucket}/{prefix}")));
                    }
                    Some(next) => Some((Some(next), seen)),
                };
                Ok(Some((page.entries, next)))
            }
        });
        pages
            .map_ok(|entries| futures::stream::iter(entries.into_iter().map(Ok)))
            .try_flatten()
    }

    /// Collects [`S3::list_all_stream`] into a vector. Memory grows with the
    /// number of objects (a few hundred bytes each); prefer the stream for
    /// prefixes that may hold millions of keys.
    pub async fn list_all(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectEntry>> {
        validate_bucket_name(bucket)?;
        self.list_all_stream(bucket, prefix).try_collect().await
    }

    /// Lists one page of object versions and delete markers under `prefix`.
    ///
    /// The delimiter, ordering and folder-marker rules are those of
    /// [`S3::list_objects`]; versions come by key, newest first within a key.
    /// `token` is an opaque value from a previous page.
    pub async fn list_versions(
        &self,
        bucket: &str,
        prefix: &str,
        flat: bool,
        token: Option<&str>,
        max_keys: u32,
    ) -> Result<VersionPage> {
        validate_bucket_name(bucket)?;
        check_max_keys(max_keys)?;
        let mut marker = token.map(VersionToken::decode).transpose()?;
        let mut seen = SeenTokens::new();
        if let Some(marker) = &marker {
            seen.first_time(marker);
        }
        let client = self.client_for(bucket).await?;
        loop {
            let out = client
                .list_object_versions()
                .bucket(bucket)
                .prefix(prefix)
                .set_delimiter((!flat).then(|| "/".to_owned()))
                .set_key_marker(marker.as_ref().map(|m| m.key.clone()))
                .set_version_id_marker(marker.as_ref().and_then(|m| m.version_id.clone()))
                .max_keys(max_keys_i32(max_keys))
                .set_request_payer(self.request_payer())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{prefix}")))?;

            let mut entries: Vec<ObjectVersion> = Vec::new();
            for v in out.versions() {
                let (Some(key), Some(version_id)) = (v.key(), v.version_id()) else {
                    continue;
                };
                entries.push(ObjectVersion {
                    key: key.to_owned(),
                    version_id: version_id.to_owned(),
                    is_latest: v.is_latest().unwrap_or(false),
                    is_delete_marker: false,
                    size: to_size(v.size()),
                    etag: v.e_tag().map(unquote),
                    last_modified: v.last_modified().and_then(to_utc),
                    storage_class: v.storage_class().map(|c| c.as_str().to_owned()),
                });
            }
            for m in out.delete_markers() {
                let (Some(key), Some(version_id)) = (m.key(), m.version_id()) else {
                    continue;
                };
                entries.push(ObjectVersion {
                    key: key.to_owned(),
                    version_id: version_id.to_owned(),
                    is_latest: m.is_latest().unwrap_or(false),
                    is_delete_marker: true,
                    size: 0,
                    etag: None,
                    last_modified: m.last_modified().and_then(to_utc),
                    storage_class: None,
                });
            }
            entries.retain(|e| e.key != prefix);
            // The SDK returns versions and delete markers as two lists; restore
            // S3's order: by key, newest first, the latest version first on ties.
            entries.sort_by(|a, b| {
                a.key
                    .cmp(&b.key)
                    .then(b.is_latest.cmp(&a.is_latest))
                    .then(b.last_modified.cmp(&a.last_modified))
            });
            let prefixes: Vec<String> = out
                .common_prefixes()
                .iter()
                .filter_map(|p| p.prefix())
                .filter(|p| *p != prefix)
                .map(str::to_owned)
                .collect();

            let next = if out.is_truncated() == Some(true) {
                let key = out.next_key_marker().filter(|k| !k.is_empty()).ok_or_else(|| {
                    Error::Remote(format!(
                        "listing versions of s3://{bucket}/{prefix}: the server reported more results but sent no marker"
                    ))
                })?;
                Some(VersionToken {
                    key: key.to_owned(),
                    version_id: out.next_version_id_marker().map(str::to_owned),
                })
            } else {
                None
            };
            if let Some(next) = &next
                && !seen.first_time(next)
            {
                return Err(repeated_token(&format!(
                    "listing versions of s3://{bucket}/{prefix}"
                )));
            }
            if !entries.is_empty() || !prefixes.is_empty() || next.is_none() {
                return Ok(VersionPage {
                    entries,
                    prefixes,
                    next_token: next.map(|t| t.encode()).transpose()?,
                });
            }
            marker = next;
        }
    }

    // ---- objects ----

    /// Returns size, ETag, modification time, storage class, checksum
    /// values (checksum mode enabled), version and metadata of an object.
    pub async fn head_object(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<ObjectHead> {
        let out = self.head_raw(bucket, key, version_id).await?;
        Ok(object_head(key, &out))
    }

    /// Creates a zero-byte folder marker. `key` must end with `/`.
    pub async fn create_folder(&self, bucket: &str, key: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        if !key.ends_with('/') {
            return Err(Error::Usage(format!(
                "folder key '{key}' must end with '/'"
            )));
        }
        if key.trim_end_matches('/').is_empty() {
            return Err(Error::Usage("the folder name is empty".into()));
        }
        // The body is empty, so the read timeout cannot cut off an upload.
        self.client_for(bucket)
            .await?
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from_static(b""))
            .content_length(0)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
        Ok(())
    }

    /// Deletes one object, or one version of it. Deleting a key that does
    /// not exist succeeds, as in S3. On a versioned bucket without
    /// `version_id` this creates a delete marker.
    pub async fn delete_object(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        self.client_for(bucket)
            .await?
            .delete_object()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
        Ok(())
    }

    /// Deletes keys in batches of [`MAX_DELETE_BATCH`]. Keys the server
    /// could not delete are reported in [`DeleteOutcome::failures`]; a
    /// request that fails as a whole returns an error (deleting is
    /// idempotent, so the call can simply be repeated).
    pub async fn delete_objects(&self, bucket: &str, keys: &[String]) -> Result<DeleteOutcome> {
        validate_bucket_name(bucket)?;
        for key in keys {
            validate_key(key)?;
        }
        let mut outcome = DeleteOutcome::default();
        for chunk in keys.chunks(MAX_DELETE_BATCH) {
            self.delete_batch(bucket, chunk, &mut outcome).await?;
        }
        Ok(outcome)
    }

    /// Deletes every object under `prefix`, including the folder marker
    /// `prefix` itself, listing and deleting 1000 keys at a time.
    ///
    /// `prefix` must end with `/` so that `photos/` never matches
    /// `photos2/`; an empty prefix (the whole bucket) is refused. On a
    /// versioned bucket this creates delete markers.
    pub async fn delete_prefix(&self, bucket: &str, prefix: &str) -> Result<DeleteOutcome> {
        validate_bucket_name(bucket)?;
        if prefix.trim_end_matches('/').is_empty() || !prefix.ends_with('/') {
            return Err(Error::Usage(format!(
                "'{prefix}' is not a folder prefix; it must be non-empty and end with '/'"
            )));
        }
        let mut outcome = DeleteOutcome::default();
        let mut batch: Vec<String> = Vec::with_capacity(MAX_DELETE_BATCH);
        let mut entries = std::pin::pin!(self.list_all_stream(bucket, prefix));
        while let Some(entry) = entries.try_next().await? {
            batch.push(entry.key);
            if batch.len() == MAX_DELETE_BATCH {
                self.delete_batch(bucket, &batch, &mut outcome).await?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.delete_batch(bucket, &batch, &mut outcome).await?;
        }
        Ok(outcome)
    }

    /// Server-side copy with default [`CopyOptions`] (see
    /// [`S3::copy_object_with`]).
    pub async fn copy_object(&self, src: &S3Uri, dst: &S3Uri) -> Result<CopyOutcome> {
        self.copy_object_with(src, dst, &CopyOptions::default(), |_| {})
            .await
    }

    /// Server-side copy of one object.
    ///
    /// Objects up to `options.multipart_threshold` bytes use `CopyObject`
    /// with `MetadataDirective: COPY`. Larger objects use a multipart upload
    /// with `UploadPartCopy`: content headers and user metadata are carried
    /// over from the source (and its tags, when the server supports
    /// tagging), the source is pinned by version ID and `If-Match` ETag so a
    /// concurrent overwrite cannot mix two versions, and the upload is
    /// aborted on any failure. If the returned future is dropped mid-copy,
    /// an abort is started in the background on the current Tokio runtime.
    ///
    /// `on_progress` receives the number of bytes copied so far after each
    /// part (once with the full size for a single-request copy). It runs on
    /// the task driving the copy and must return quickly without blocking.
    pub async fn copy_object_with<F>(
        &self,
        src: &S3Uri,
        dst: &S3Uri,
        options: &CopyOptions,
        on_progress: F,
    ) -> Result<CopyOutcome>
    where
        F: Fn(u64) + Send + Sync,
    {
        validate_key(src.key()).map_err(|err| err.with_context("copy source"))?;
        validate_key(dst.key()).map_err(|err| err.with_context("copy destination"))?;
        if src == dst {
            return Err(Error::Usage(format!("cannot copy {src} onto itself")));
        }
        options.validate()?;
        let head = self.head_raw(src.bucket(), src.key(), None).await?;
        let size = to_size(head.content_length());
        if size == 0 || size <= options.multipart_threshold {
            // Sent to the destination's region; S3 fetches the source itself.
            // No read timeout: a server may answer only when the copy is done.
            let out = self
                .client_for(dst.bucket())
                .await?
                .copy_object()
                .bucket(dst.bucket())
                .key(dst.key())
                .copy_source(copy_source(src.bucket(), src.key(), None))
                .metadata_directive(MetadataDirective::Copy)
                .set_request_payer(self.request_payer())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(format!("copying {src} to {dst}")))?;
            on_progress(size);
            return Ok(CopyOutcome {
                size,
                etag: out
                    .copy_object_result()
                    .and_then(|r| r.e_tag())
                    .map(unquote),
                parts: 0,
                renamed_to: None,
            });
        }
        self.multipart_copy(
            src,
            dst,
            &head,
            options,
            &CopyChanges::default(),
            &on_progress,
        )
        .await
        .map_err(|err| err.with_context(format!("copying {src} to {dst}")))
    }

    /// Creates a presigned `GET` URL valid for `expires` (1 s to 7 days),
    /// signed for and addressed to the bucket's region.
    pub async fn presign_get(&self, bucket: &str, key: &str, expires: Duration) -> Result<String> {
        let config = self.presign_config(bucket, key, expires)?;
        let payer = self.request_payer;
        let request = self
            .client_for(bucket)
            .await?
            .get_object()
            .bucket(bucket)
            .key(key)
            .customize()
            .map_request(move |req| add_payer_query(req, payer))
            .presigned(config)
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
        Ok(request.uri().to_owned())
    }

    /// Creates a presigned `PUT` URL valid for `expires` (1 s to 7 days),
    /// signed for and addressed to the bucket's region. The uploader may
    /// send any `Content-Type`; no checksum is required.
    pub async fn presign_put(&self, bucket: &str, key: &str, expires: Duration) -> Result<String> {
        let config = self.presign_config(bucket, key, expires)?;
        let payer = self.request_payer;
        let request = self
            .client_for(bucket)
            .await?
            .put_object()
            .bucket(bucket)
            .key(key)
            .customize()
            .map_request(move |req| add_payer_query(req, payer))
            .presigned(config)
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
        Ok(request.uri().to_owned())
    }

    // ---- multipart uploads ----

    /// Lists every uncompleted multipart upload under `prefix`, following
    /// pagination. Uploads are few in practice, so they are collected.
    pub async fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> Result<Vec<MultipartUpload>> {
        validate_bucket_name(bucket)?;
        let client = self.client_for(bucket).await?;
        let mut uploads = Vec::new();
        let mut markers: Option<(String, Option<String>)> = None;
        let mut seen = SeenTokens::new();
        loop {
            let out = client
                .list_multipart_uploads()
                .bucket(bucket)
                .prefix(prefix)
                .set_key_marker(markers.as_ref().map(|m| m.0.clone()))
                .set_upload_id_marker(markers.as_ref().and_then(|m| m.1.clone()))
                .set_request_payer(self.request_payer())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{prefix}")))?;
            for upload in out.uploads() {
                let (Some(key), Some(upload_id)) = (upload.key(), upload.upload_id()) else {
                    continue;
                };
                uploads.push(MultipartUpload {
                    key: key.to_owned(),
                    upload_id: upload_id.to_owned(),
                    initiated: upload.initiated().and_then(to_utc),
                    storage_class: upload.storage_class().map(|c| c.as_str().to_owned()),
                    initiator: upload
                        .initiator()
                        .and_then(|i| i.display_name().or(i.id()))
                        .map(str::to_owned),
                });
            }
            if out.is_truncated() != Some(true) {
                return Ok(uploads);
            }
            let next_key = out.next_key_marker().filter(|k| !k.is_empty()).ok_or_else(|| {
                Error::Remote(format!(
                    "listing uploads of s3://{bucket}/{prefix}: the server reported more results but sent no marker"
                ))
            })?;
            let next = (
                next_key.to_owned(),
                out.next_upload_id_marker().map(str::to_owned),
            );
            if !seen.first_time(&next) {
                return Err(repeated_token(&format!(
                    "listing uploads of s3://{bucket}/{prefix}"
                )));
            }
            markers = Some(next);
        }
    }

    /// Counts the parts uploaded so far to an uncompleted multipart upload
    /// and adds up their sizes: `(parts, bytes)`. Follows pagination.
    pub async fn multipart_upload_usage(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<(u64, u64)> {
        validate_bucket_name(bucket)?;
        let client = self.client_for(bucket).await?;
        let (mut parts, mut bytes) = (0u64, 0u64);
        let mut marker: Option<String> = None;
        let mut seen = SeenTokens::new();
        loop {
            let out = client
                .list_parts()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.clone())
                .set_request_payer(self.request_payer())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
            for part in out.parts() {
                parts += 1;
                bytes += to_size(part.size());
            }
            if out.is_truncated() != Some(true) {
                return Ok((parts, bytes));
            }
            let next = out
                .next_part_number_marker()
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::Remote(format!(
                        "listing parts of s3://{bucket}/{key}: the server reported more results but sent no marker"
                    ))
                })?;
            if !seen.first_time(&next) {
                return Err(repeated_token(&format!(
                    "listing parts of s3://{bucket}/{key}"
                )));
            }
            marker = Some(next);
        }
    }

    /// Aborts an uncompleted multipart upload and frees its parts.
    pub async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        if upload_id.is_empty() {
            return Err(Error::Usage("the upload ID is empty".into()));
        }
        self.client_for(bucket)
            .await?
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
        Ok(())
    }

    // ---- internals ----

    /// Asks the server for the region of `bucket` (`HeadBucket` on the
    /// account-region client).
    async fn resolve_region(&self, bucket: &str) -> std::result::Result<String, Unresolved> {
        let reply = self
            .client
            .head_bucket()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let region = match reply {
            Ok(out) => out.bucket_region().map(str::to_owned),
            // Redirects and refusals carry the header too.
            Err(err) => match err.raw_response() {
                Some(raw) => raw.headers().get(BUCKET_REGION_HEADER).map(str::to_owned),
                None => {
                    return Err(Unresolved::Failed(
                        Error::from(err)
                            .with_context(format!("finding the region of bucket '{bucket}'")),
                    ));
                }
            },
        };
        let Some(region) = region
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
        else {
            return Err(Unresolved::UseAccountRegion);
        };
        if validate_region(&region).is_err() {
            return Err(Unresolved::Failed(Error::Remote(format!(
                "finding the region of bucket '{bucket}': the server named an invalid region {region:?}"
            ))));
        }
        Ok(region)
    }

    /// Remembers the region of `bucket` reported by the server, when regions
    /// are resolved for this account and the name is a valid region.
    fn learn_region(&self, bucket: &str, region: &str) {
        if self.resolve_regions && validate_region(region).is_ok() {
            self.regions.remember(bucket, region);
        }
    }

    /// The client for `region`: [`S3::client`] for the account region,
    /// otherwise a cached copy of it with another region.
    fn client_in(&self, region: &str) -> Client {
        if self.client.config().region().map(Region::as_ref) == Some(region) {
            return self.client.clone();
        }
        let mut clients = self
            .regions
            .clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        clients
            .entry(region.to_owned())
            .or_insert_with(|| {
                let config = self
                    .client
                    .config()
                    .to_builder()
                    .region(Region::new(region.to_owned()))
                    .build();
                Client::from_conf(config)
            })
            .clone()
    }

    async fn fetch_objects(
        &self,
        bucket: &str,
        prefix: &str,
        delimiter: Option<&str>,
        token: Option<&str>,
        max_keys: u32,
    ) -> Result<ObjectPage> {
        let out = self
            .client_for(bucket)
            .await?
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .set_delimiter(delimiter.map(str::to_owned))
            .set_continuation_token(token.map(str::to_owned))
            .max_keys(max_keys_i32(max_keys))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{prefix}")))?;
        let mut entries = Vec::with_capacity(out.common_prefixes().len() + out.contents().len());
        entries.extend(
            out.common_prefixes()
                .iter()
                .filter_map(|p| p.prefix())
                .map(ObjectEntry::prefix),
        );
        for object in out.contents() {
            let Some(key) = object.key() else { continue };
            entries.push(ObjectEntry {
                key: key.to_owned(),
                size: to_size(object.size()),
                etag: object.e_tag().map(unquote),
                last_modified: object.last_modified().and_then(to_utc),
                storage_class: object.storage_class().map(|c| c.as_str().to_owned()),
                checksums: Checksums::default(),
                is_prefix: false,
            });
        }
        let next_token = if out.is_truncated() == Some(true) {
            let token = out.next_continuation_token().filter(|t| !t.is_empty()).ok_or_else(|| {
                Error::Remote(format!(
                    "listing s3://{bucket}/{prefix}: the server reported more results but sent no continuation token"
                ))
            })?;
            Some(token.to_owned())
        } else {
            None
        };
        Ok(ObjectPage {
            entries,
            next_token,
        })
    }

    async fn delete_batch(
        &self,
        bucket: &str,
        keys: &[String],
        outcome: &mut DeleteOutcome,
    ) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let objects = keys
            .iter()
            .map(|key| ObjectIdentifier::builder().key(key).build())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(build_error)?;
        let delete = Delete::builder()
            .set_objects(Some(objects))
            .quiet(true)
            .build()
            .map_err(build_error)?;
        let out = self
            .client_for(bucket)
            .await?
            .delete_objects()
            .bucket(bucket)
            .delete(delete)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("deleting objects in bucket '{bucket}'"))
            })?;
        let failures: Vec<DeleteFailure> = out
            .errors()
            .iter()
            .map(|e| DeleteFailure {
                key: e.key().unwrap_or_default().to_owned(),
                version_id: e.version_id().map(str::to_owned),
                code: e.code().map(str::to_owned),
                message: e
                    .message()
                    .unwrap_or("the object could not be deleted")
                    .to_owned(),
            })
            .collect();
        outcome.deleted += (keys.len().saturating_sub(failures.len())) as u64;
        outcome.failures.extend(failures);
        Ok(())
    }

    async fn head_raw(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<HeadObjectOutput> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        self.client_for(bucket)
            .await?
            .head_object()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))
    }

    fn presign_config(
        &self,
        bucket: &str,
        key: &str,
        expires: Duration,
    ) -> Result<PresigningConfig> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        if !(MIN_PRESIGN_EXPIRY..=MAX_PRESIGN_EXPIRY).contains(&expires) {
            return Err(Error::Usage(format!(
                "a presigned URL must be valid for 1 second to 7 days (got {} s)",
                expires.as_secs_f64()
            )));
        }
        PresigningConfig::expires_in(expires)
            .map_err(|err| Error::Usage(format!("invalid presigning expiry: {err}")))
    }

    async fn source_tagging(
        &self,
        src: &S3Uri,
        version_id: Option<&str>,
    ) -> Result<Option<String>> {
        let result = self
            .client_for(src.bucket())
            .await?
            .get_object_tagging()
            .bucket(src.bucket())
            .key(src.key())
            .set_version_id(version_id.map(str::to_owned))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        match result {
            Ok(out) if out.tag_set().is_empty() => Ok(None),
            Ok(out) => Ok(Some(
                out.tag_set()
                    .iter()
                    .map(|tag| {
                        format!(
                            "{}={}",
                            percent_encode(tag.key(), false),
                            percent_encode(tag.value(), false)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("&"),
            )),
            Err(err) => {
                let err = Error::from(err);
                if err.service().is_some_and(ServiceError::is_not_implemented) {
                    Ok(None)
                } else {
                    Err(err.with_context("reading the source tags"))
                }
            }
        }
    }

    /// Copies `src` to `dst` with `UploadPartCopy`.
    ///
    /// `changes` says how the new object differs from the source (see
    /// [`CopyChanges`]); the default carries over everything the source has,
    /// as a plain copy does. An in-place edit of an object larger than 5 GiB
    /// (`crate::objectmeta`) passes its storage class, encryption and headers.
    pub(crate) async fn multipart_copy(
        &self,
        src: &S3Uri,
        dst: &S3Uri,
        head: &HeadObjectOutput,
        options: &CopyOptions,
        changes: &CopyChanges,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<CopyOutcome> {
        let size = to_size(head.content_length());
        let part_size = part_size_for(size, options.part_size)?;
        let part_count = size.div_ceil(part_size);
        let version_id = head.version_id().filter(|v| *v != "null");
        let tagging = self.source_tagging(src, version_id).await?;
        let headers = changes
            .replace
            .clone()
            .unwrap_or_else(|| HeaderSet::from_head(head));
        let encryption = &changes.encryption;

        // Everything goes to the destination's region; UploadPartCopy and
        // CompleteMultipartUpload get no read timeout (see metadata_timeout).
        let client = self.client_for(dst.bucket()).await?;
        let mut create = client
            .create_multipart_upload()
            .bucket(dst.bucket())
            .key(dst.key())
            .set_content_type(headers.content_type)
            .set_cache_control(headers.cache_control)
            .set_content_disposition(headers.content_disposition)
            .set_content_encoding(headers.content_encoding)
            .set_content_language(headers.content_language)
            .set_website_redirect_location(if changes.replace.is_some() {
                // A header edit decided the redirect, which may be "none".
                changes.website_redirect.clone()
            } else {
                changes
                    .website_redirect
                    .clone()
                    .or_else(|| head.website_redirect_location().map(str::to_owned))
            })
            .set_expires(headers.expires)
            .set_metadata(Some(headers.user_meta.into_iter().collect()))
            .set_tagging(tagging)
            .set_storage_class(changes.storage_class.clone())
            .set_server_side_encryption(encryption.sse.clone())
            .set_ssekms_key_id(encryption.kms_key_id.clone())
            .set_bucket_key_enabled(encryption.bucket_key)
            .set_request_payer(self.request_payer());
        if let Some(key) = &encryption.customer_key {
            create = create
                .sse_customer_algorithm(SSE_C_ALGORITHM)
                .sse_customer_key(key.to_base64().as_str())
                .sse_customer_key_md5(key.md5_base64());
        }
        let created = create
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(Error::from)?;
        let upload_id = created
            .upload_id()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Error::Remote("the server started a multipart upload without an upload ID".into())
            })?
            .to_owned();

        let mut guard = AbortOnDrop {
            client: client.clone(),
            config: self.metadata_timeout(),
            bucket: dst.bucket().to_owned(),
            key: dst.key().to_owned(),
            upload_id: upload_id.clone(),
            request_payer: self.request_payer(),
            armed: true,
        };
        let plan = PartPlan {
            upload_id: &upload_id,
            size,
            part_size,
            concurrency: options.concurrency,
            changes,
        };
        let result = self
            .copy_parts(&client, src, dst, head, &plan, on_progress)
            .await;
        let result = match result {
            Ok(parts) => {
                let mut complete = client
                    .complete_multipart_upload()
                    .bucket(dst.bucket())
                    .key(dst.key())
                    .upload_id(&upload_id)
                    .multipart_upload(
                        CompletedMultipartUpload::builder()
                            .set_parts(Some(parts))
                            .build(),
                    )
                    .set_request_payer(self.request_payer());
                if let Some(key) = &encryption.customer_key {
                    complete = complete
                        .sse_customer_algorithm(SSE_C_ALGORITHM)
                        .sse_customer_key(key.to_base64().as_str())
                        .sse_customer_key_md5(key.md5_base64());
                }
                complete.send().await.map_err(Error::from)
            }
            Err(err) => Err(err),
        };
        match result {
            Ok(done) => {
                guard.armed = false;
                Ok(CopyOutcome {
                    size,
                    etag: done.e_tag().map(unquote),
                    parts: u32::try_from(part_count).unwrap_or(u32::MAX),
                    renamed_to: None,
                })
            }
            Err(err) => {
                // The original error is what the caller needs, but an upload
                // that could not be aborted stays open (and billed) on the
                // server, which the caller must hear about too; it also shows
                // up in list_multipart_uploads. The guard stays armed until
                // the abort has been attempted, so a cancellation during the
                // abort still triggers one.
                let aborted = self
                    .abort_multipart_upload(dst.bucket(), dst.key(), &upload_id)
                    .await;
                guard.armed = false;
                Err(match aborted {
                    Ok(()) => err,
                    Err(abort) => err.with_context(format!(
                        "the multipart upload {upload_id} to {dst} could not be aborted and stays open on the server ({abort}); the copy itself failed"
                    )),
                })
            }
        }
    }

    async fn copy_parts(
        &self,
        client: &Client,
        src: &S3Uri,
        dst: &S3Uri,
        head: &HeadObjectOutput,
        plan: &PartPlan<'_>,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<Vec<CompletedPart>> {
        let version_id = head.version_id().filter(|v| *v != "null");
        let source = copy_source(src.bucket(), src.key(), version_id);
        let if_match = head.e_tag().map(str::to_owned);
        let PartPlan {
            upload_id,
            size,
            part_size,
            concurrency,
            changes,
        } = *plan;
        let part_count = size.div_ceil(part_size);
        let requests = (0..part_count).map(|index| {
            let start = index * part_size;
            let end = (start + part_size).min(size) - 1;
            let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
            let mut request = client
                .upload_part_copy()
                .bucket(dst.bucket())
                .key(dst.key())
                .upload_id(upload_id)
                .part_number(number)
                .copy_source(source.clone())
                .copy_source_range(format!("bytes={start}-{end}"))
                .set_copy_source_if_match(if_match.clone())
                .set_request_payer(self.request_payer());
            // SSE-C needs the key of the source and, when the new object is
            // encrypted with one too, its key on every part.
            if let Some(key) = &changes.source_key {
                request = request
                    .copy_source_sse_customer_algorithm(SSE_C_ALGORITHM)
                    .copy_source_sse_customer_key(key.to_base64().as_str())
                    .copy_source_sse_customer_key_md5(key.md5_base64());
            }
            if let Some(key) = &changes.encryption.customer_key {
                request = request
                    .sse_customer_algorithm(SSE_C_ALGORITHM)
                    .sse_customer_key(key.to_base64().as_str())
                    .sse_customer_key_md5(key.md5_base64());
            }
            async move {
                let out = request.send().await.map_err(|err| {
                    Error::from(err).with_context(format!("copying part {number}"))
                })?;
                let etag = out
                    .copy_part_result()
                    .and_then(|r| r.e_tag())
                    .ok_or_else(|| {
                        Error::Remote(format!(
                            "the server returned no ETag for copied part {number}"
                        ))
                    })?;
                Ok::<_, Error>((number, etag.to_owned(), end - start + 1))
            }
        });
        let mut in_flight = futures::stream::iter(requests).buffer_unordered(concurrency);
        let mut parts = Vec::with_capacity(usize::try_from(part_count).unwrap_or(0));
        let mut copied = 0u64;
        while let Some(result) = in_flight.next().await {
            let (number, etag, len) = result?;
            parts.push(
                CompletedPart::builder()
                    .part_number(number)
                    .e_tag(etag)
                    .build(),
            );
            copied += len;
            on_progress(copied);
        }
        parts.sort_by_key(|part| part.part_number());
        Ok(parts)
    }
}

/// Shape of a multipart copy.
#[derive(Clone, Copy)]
struct PartPlan<'a> {
    upload_id: &'a str,
    size: u64,
    part_size: u64,
    concurrency: usize,
    /// How the new object differs from the source.
    changes: &'a CopyChanges,
}

/// Aborts a multipart upload if the copy future is dropped before finishing.
///
/// Dropping cannot report anything, so a failed abort is only logged (a
/// warning of the `bucketbridge::s3` target, which the event log shows), and
/// the upload stays open on the server until it is aborted with the
/// multipart uploads tool: the copy's upload ID is not recorded anywhere
/// else. A copy that fails while it is polled does better: it aborts in
/// line and puts a failed abort into its error.
struct AbortOnDrop {
    client: Client,
    /// Per-request override with the read timeout.
    config: aws_sdk_s3::config::Builder,
    bucket: String,
    key: String,
    upload_id: String,
    request_payer: Option<RequestPayer>,
    armed: bool,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let request = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(&self.key)
                .upload_id(&self.upload_id)
                .set_request_payer(self.request_payer.clone())
                .customize()
                .config_override(self.config.clone());
            let (bucket, key, upload_id) = (
                self.bucket.clone(),
                self.key.clone(),
                self.upload_id.clone(),
            );
            runtime.spawn(async move {
                if let Err(err) = request.send().await {
                    tracing::warn!(
                        target: "bucketbridge::s3",
                        "the multipart upload {upload_id} of a copy to s3://{bucket}/{key} could not be aborted and stays open on the server: {}",
                        DisplayErrorContext(&err)
                    );
                }
            });
        } else {
            tracing::warn!(
                target: "bucketbridge::s3",
                "the multipart upload {} of a copy to s3://{}/{} was not aborted (no runtime) and stays open on the server",
                self.upload_id, self.bucket, self.key
            );
        }
    }
}

/// Bucket regions and per-region clients, shared by the clones of one [`S3`].
#[derive(Debug, Default)]
struct RegionCache {
    /// Bucket name to its region; each cell is filled at most once, so
    /// concurrent callers share one `HeadBucket` request.
    buckets: Mutex<HashMap<String, Arc<OnceCell<String>>>>,
    /// Region to a client for it (the account region is not stored).
    clients: Mutex<HashMap<String, Client>>,
}

impl RegionCache {
    fn cell(&self, bucket: &str) -> Arc<OnceCell<String>> {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(bucket.to_owned())
            .or_default()
            .clone()
    }

    fn remember(&self, bucket: &str, region: &str) {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                bucket.to_owned(),
                Arc::new(OnceCell::new_with(Some(region.to_owned()))),
            );
    }

    fn forget(&self, bucket: &str) {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(bucket);
    }
}

/// Why a bucket region could not be cached.
enum Unresolved {
    /// The server answered without naming the region; use the account
    /// region this time.
    UseAccountRegion,
    /// The resolution request failed.
    Failed(Error),
}

/// Continuation tokens or markers the server sent during one listing. A
/// server that repeats one, directly or in a cycle such as A, B, A, would
/// otherwise be followed forever. Only keyed 64-bit hashes are kept, so
/// memory stays small even for listings of millions of pages.
pub(crate) struct SeenTokens {
    keys: RandomState,
    seen: HashSet<u64>,
}

impl SeenTokens {
    pub(crate) fn new() -> Self {
        SeenTokens {
            keys: RandomState::new(),
            seen: HashSet::new(),
        }
    }

    /// Records `token`; `false` when it was seen before in this listing.
    pub(crate) fn first_time<T: Hash + ?Sized>(&mut self, token: &T) -> bool {
        self.seen.insert(self.keys.hash_one(token))
    }
}

/// Opaque continuation token of [`S3::list_versions`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct VersionToken {
    #[serde(rename = "k")]
    key: String,
    #[serde(rename = "v")]
    version_id: Option<String>,
}

impl VersionToken {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|err| Error::Internal(format!("encoding a version token: {err}")))
    }

    fn decode(token: &str) -> Result<Self> {
        serde_json::from_str(token)
            .map_err(|_| Error::Usage("invalid continuation token for a version listing".into()))
    }
}

fn add_payer_query(
    mut request: HttpRequest,
    payer: bool,
) -> std::result::Result<HttpRequest, Error> {
    if payer {
        let separator = if request.uri().contains('?') {
            '&'
        } else {
            '?'
        };
        let uri = format!("{}{separator}x-amz-request-payer=requester", request.uri());
        request
            .set_uri(uri)
            .map_err(|err| Error::Internal(format!("adding the request-payer parameter: {err}")))?;
    }
    Ok(request)
}

pub(crate) fn build_error(err: BuildError) -> Error {
    Error::Internal(format!("failed to build an S3 request: {err}"))
}

fn repeated_token(what: &str) -> Error {
    Error::Remote(format!(
        "{what}: the server returned the same continuation token twice; stopping to avoid an endless listing"
    ))
}

fn check_max_keys(max_keys: u32) -> Result<()> {
    if (1..=MAX_LIST_KEYS).contains(&max_keys) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "max_keys must be between 1 and {MAX_LIST_KEYS} (got {max_keys})"
        )))
    }
}

fn max_keys_i32(max_keys: u32) -> i32 {
    i32::try_from(max_keys.min(MAX_LIST_KEYS)).unwrap_or(1000)
}

pub(crate) fn to_utc(time: &aws_smithy_types::DateTime) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos())
}

pub(crate) fn to_size(size: Option<i64>) -> u64 {
    size.and_then(|s| u64::try_from(s).ok()).unwrap_or(0)
}

/// Removes the double quotes S3 puts around ETags.
pub(crate) fn unquote(etag: &str) -> String {
    etag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(etag)
        .to_owned()
}

/// Percent-encodes everything except RFC 3986 unreserved characters (and
/// `/` when `keep_slash` is set).
fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/');
        if keep {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The `x-amz-copy-source` value: `bucket/key`, URL-encoded, with an
/// optional version.
pub(crate) fn copy_source(bucket: &str, key: &str, version_id: Option<&str>) -> String {
    let mut source = format!("{bucket}/{}", percent_encode(key, true));
    if let Some(version_id) = version_id {
        source.push_str("?versionId=");
        source.push_str(&percent_encode(version_id, false));
    }
    source
}

pub(crate) fn object_head(key: &str, out: &HeadObjectOutput) -> ObjectHead {
    let sse = match (out.sse_customer_algorithm(), out.server_side_encryption()) {
        (Some(_), _) => Sse::Customer {
            key_md5: out.sse_customer_key_md5().unwrap_or_default().to_owned(),
        },
        (None, Some(ServerSideEncryption::AwsKms | ServerSideEncryption::AwsKmsDsse)) => Sse::Kms {
            key_id: out.ssekms_key_id().map(str::to_owned),
            bucket_key: out.bucket_key_enabled().unwrap_or(false),
        },
        (None, Some(_)) => Sse::S3,
        (None, None) => Sse::None,
    };
    let lock_mode = out.object_lock_mode().and_then(|mode| match mode {
        ObjectLockMode::Governance => Some(LockMode::Governance),
        ObjectLockMode::Compliance => Some(LockMode::Compliance),
        _ => None,
    });
    let retain_until = out.object_lock_retain_until_date().and_then(to_utc);
    let legal_hold = out.object_lock_legal_hold_status();
    let object_lock = (lock_mode.is_some() || retain_until.is_some() || legal_hold.is_some())
        .then_some(ObjectLock {
            mode: lock_mode,
            retain_until,
            legal_hold: matches!(legal_hold, Some(ObjectLockLegalHoldStatus::On)),
        });
    ObjectHead {
        key: key.to_owned(),
        size: to_size(out.content_length()),
        etag: out.e_tag().map(unquote),
        last_modified: out.last_modified().and_then(to_utc),
        storage_class: out.storage_class().map(|c| c.as_str().to_owned()),
        version_id: out.version_id().map(str::to_owned),
        checksums: Checksums {
            crc32: out.checksum_crc32().map(str::to_owned),
            crc32c: out.checksum_crc32_c().map(str::to_owned),
            crc64nvme: out.checksum_crc64_nvme().map(str::to_owned),
            sha1: out.checksum_sha1().map(str::to_owned),
            sha256: out.checksum_sha256().map(str::to_owned),
        },
        meta: ObjectMeta {
            content_type: out.content_type().map(str::to_owned),
            cache_control: out.cache_control().map(str::to_owned),
            content_disposition: out.content_disposition().map(str::to_owned),
            content_encoding: out.content_encoding().map(str::to_owned),
            user_meta: out
                .metadata()
                .map(|meta| {
                    meta.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default(),
            tags: BTreeMap::new(),
            acl: None,
            sse,
            object_lock,
            website_redirect: out.website_redirect_location().map(str::to_owned),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_runtime_api::client::interceptors::Intercept;

    #[test]
    fn part_size_grows_to_stay_within_the_part_limit() {
        assert_eq!(part_size_for(10 * MIB, 8 * MIB).unwrap(), 8 * MIB);
        assert_eq!(part_size_for(1, MIN_PART_SIZE).unwrap(), MIN_PART_SIZE);
        // 100 GiB with 8 MiB parts would need 12,800 parts.
        let size = 100 * 1024 * MIB;
        let part = part_size_for(size, 8 * MIB).unwrap();
        assert!(size.div_ceil(part) <= MAX_PARTS);
        assert_eq!(part % MIB, 0);
        assert_eq!(part, 11 * MIB);
        // The largest S3 object (5 TiB) still fits.
        let max_object = 5 * 1024 * 1024 * MIB;
        let part = part_size_for(max_object, 8 * MIB).unwrap();
        assert!(max_object.div_ceil(part) <= MAX_PARTS);
        assert!(part <= MAX_PART_SIZE);
        // Exactly 10,000 parts of the preferred size is fine.
        assert_eq!(
            part_size_for(MAX_PARTS * 8 * MIB, 8 * MIB).unwrap(),
            8 * MIB
        );
        assert_eq!(
            part_size_for(MAX_PARTS * 8 * MIB + 1, 8 * MIB).unwrap(),
            9 * MIB
        );
        assert!(part_size_for(MAX_PARTS * MAX_PART_SIZE + 1, 8 * MIB).is_err());
        assert_eq!(part_size_for(0, MIN_PART_SIZE).unwrap(), MIN_PART_SIZE);
    }

    #[test]
    fn copy_options_validation() {
        CopyOptions::default().validate().unwrap();
        let bad = [
            CopyOptions {
                multipart_threshold: 0,
                ..CopyOptions::default()
            },
            CopyOptions {
                multipart_threshold: MAX_SINGLE_COPY_SIZE + 1,
                ..CopyOptions::default()
            },
            CopyOptions {
                part_size: MIN_PART_SIZE - 1,
                ..CopyOptions::default()
            },
            CopyOptions {
                part_size: MAX_PART_SIZE + 1,
                ..CopyOptions::default()
            },
            CopyOptions {
                concurrency: 0,
                ..CopyOptions::default()
            },
        ];
        for options in bad {
            assert_eq!(
                options.validate().unwrap_err().kind(),
                ErrorKind::Usage,
                "{options:?}"
            );
        }
    }

    #[test]
    fn copy_source_is_url_encoded() {
        assert_eq!(
            copy_source("b", "photos/a b+c.jpg", None),
            "b/photos/a%20b%2Bc.jpg"
        );
        assert_eq!(
            copy_source("b", "ü/ファ", None),
            "b/%C3%BC/%E3%83%95%E3%82%A1"
        );
        assert_eq!(
            copy_source("b", "k", Some("v+1/2")),
            "b/k?versionId=v%2B1%2F2"
        );
        assert_eq!(copy_source("b", "a?x=1#f%", None), "b/a%3Fx%3D1%23f%25");
        assert_eq!(percent_encode("k=v&x", false), "k%3Dv%26x");
        assert_eq!(percent_encode("A-z_0.9~", false), "A-z_0.9~");
    }

    #[test]
    fn etags_are_unquoted() {
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("abc"), "abc");
        assert_eq!(unquote("\"abc-3\""), "abc-3");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(unquote(""), "");
    }

    #[test]
    fn version_tokens_round_trip_and_reject_garbage() {
        let token = VersionToken {
            key: "a/b.txt".into(),
            version_id: Some("3HL4kqtJ".into()),
        };
        let text = token.encode().unwrap();
        assert_eq!(VersionToken::decode(&text).unwrap(), token);
        let err = VersionToken::decode("not a token").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn seen_tokens_detect_repeats_and_cycles() {
        let mut seen = SeenTokens::new();
        assert!(seen.first_time("A"));
        assert!(seen.first_time("B"));
        assert!(!seen.first_time("A"));
        assert!(!seen.first_time(&"B".to_owned()));
        let mut markers = SeenTokens::new();
        assert!(markers.first_time(&("k".to_owned(), Some("v1".to_owned()))));
        assert!(markers.first_time(&("k".to_owned(), None::<String>)));
        assert!(!markers.first_time(&("k".to_owned(), Some("v1".to_owned()))));
    }

    fn aws_s3(options: &ClientOptions) -> S3 {
        let account = Account {
            id: "aws".into(),
            label: "AWS".into(),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_ref: "aws/secret".into(),
            region: "us-east-1".into(),
            ..Account::default()
        };
        S3::new(
            &account,
            &Credentials::new("AKIDEXAMPLE", "secret", None),
            options,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn regional_clients_keep_the_account_configuration() {
        let options = ClientOptions {
            max_attempts: 3,
            connect_timeout: Duration::from_secs(7),
            read_timeout: Duration::from_secs(8),
            checksum_algorithm: crate::settings::ChecksumAlgorithm::None,
            validate_downloads: false,
            stalled_stream: aws_sdk_s3::config::StalledStreamProtectionConfig::enabled()
                .grace_period(Duration::from_secs(42))
                .build(),
            ..ClientOptions::default()
        };
        let s3 = aws_s3(&options);
        assert!(s3.resolve_regions);
        s3.regions.remember("eu-bucket", "eu-west-1");
        s3.regions.remember("home-bucket", "us-east-1");

        let regional = s3.client_for("eu-bucket").await.unwrap();
        let config = regional.config();
        assert_eq!(config.region().map(Region::as_ref), Some("eu-west-1"));
        assert_eq!(config.retry_config().map(|r| r.max_attempts()), Some(3));
        let timeouts = config.timeout_config().unwrap();
        assert_eq!(timeouts.connect_timeout(), Some(Duration::from_secs(7)));
        assert_eq!(timeouts.read_timeout(), None);
        assert_eq!(
            config.request_checksum_calculation(),
            Some(&aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired)
        );
        assert_eq!(
            config.response_checksum_validation(),
            Some(&aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired)
        );
        assert_eq!(
            config.stalled_stream_protection().map(|s| s.grace_period()),
            Some(Duration::from_secs(42))
        );
        let base: Vec<&str> = s3
            .client()
            .config()
            .interceptors()
            .map(|i| i.name())
            .collect();
        let copied: Vec<&str> = config.interceptors().map(|i| i.name()).collect();
        assert_eq!(base, copied);
        assert!(!copied.is_empty());

        // One client per region; the account region uses the base client.
        let again = s3.client_for("eu-bucket").await.unwrap();
        assert_eq!(s3.regions.clients.lock().unwrap().len(), 1);
        assert_eq!(
            again.config().region().map(Region::as_ref),
            Some("eu-west-1")
        );
        let home = s3.client_for("home-bucket").await.unwrap();
        assert_eq!(
            home.config().region().map(Region::as_ref),
            Some("us-east-1")
        );
        assert_eq!(s3.regions.clients.lock().unwrap().len(), 1);

        // The per-request override sets the read timeout only.
        let override_config = s3.metadata_timeout().build();
        let timeouts = override_config.timeout_config().unwrap();
        assert_eq!(timeouts.read_timeout(), Some(Duration::from_secs(8)));
        assert_eq!(timeouts.connect_timeout(), None);
    }

    #[tokio::test]
    async fn aws_presigned_urls_use_the_bucket_region() {
        let s3 = aws_s3(&ClientOptions::default());
        s3.regions.remember("eu-bucket", "eu-west-1");
        let url = s3
            .presign_get("eu-bucket", "k.txt", Duration::from_secs(60))
            .await
            .unwrap();
        assert!(
            url.starts_with("https://eu-bucket.s3.eu-west-1.amazonaws.com/k.txt?"),
            "{url}"
        );
        assert!(url.contains("%2Feu-west-1%2Fs3%2Faws4_request"), "{url}");
        let url = s3
            .presign_put("eu-bucket", "k.txt", Duration::from_secs(60))
            .await
            .unwrap();
        assert!(url.contains("%2Feu-west-1%2Fs3%2Faws4_request"), "{url}");

        // Learning a region needs a valid name and a resolving account.
        s3.learn_region("odd-bucket", "eu west 1");
        assert!(
            s3.regions
                .buckets
                .lock()
                .unwrap()
                .get("odd-bucket")
                .is_none()
        );
        let external = S3::from_client(s3.client().clone(), false, BucketKind::External);
        external.learn_region("eu-bucket-2", "eu-west-1");
        assert!(external.regions.buckets.lock().unwrap().is_empty());
        let resolving = external.with_region_resolution(true);
        resolving.learn_region("eu-bucket-2", "eu-west-1");
        assert_eq!(resolving.regions.buckets.lock().unwrap().len(), 1);
    }

    #[test]
    fn max_keys_bounds() {
        assert!(check_max_keys(0).is_err());
        assert!(check_max_keys(1).is_ok());
        assert!(check_max_keys(1000).is_ok());
        assert!(check_max_keys(1001).is_err());
        assert_eq!(max_keys_i32(1000), 1000);
    }

    #[test]
    fn timestamps_and_sizes_convert() {
        let time = aws_smithy_types::DateTime::from_secs_and_nanos(1_793_000_000, 5_000_000);
        let utc = to_utc(&time).unwrap();
        assert_eq!(utc.timestamp(), 1_793_000_000);
        assert_eq!(utc.timestamp_subsec_millis(), 5);
        assert_eq!(to_size(Some(-1)), 0);
        assert_eq!(to_size(None), 0);
        assert_eq!(to_size(Some(42)), 42);
    }

    #[test]
    fn payer_query_parameter() {
        let mut request = HttpRequest::empty();
        request
            .set_uri("https://b.s3.amazonaws.com/k?x-id=GetObject")
            .unwrap();
        let request = add_payer_query(request, true).unwrap();
        assert_eq!(
            request.uri(),
            "https://b.s3.amazonaws.com/k?x-id=GetObject&x-amz-request-payer=requester"
        );
        let mut request = HttpRequest::empty();
        request.set_uri("https://b.s3.amazonaws.com/k").unwrap();
        let request = add_payer_query(request, true).unwrap();
        assert_eq!(
            request.uri(),
            "https://b.s3.amazonaws.com/k?x-amz-request-payer=requester"
        );
        let mut request = HttpRequest::empty();
        request.set_uri("https://b.s3.amazonaws.com/k").unwrap();
        assert_eq!(
            add_payer_query(request, false).unwrap().uri(),
            "https://b.s3.amazonaws.com/k"
        );
    }

    #[test]
    fn head_output_maps_to_model() {
        let out = HeadObjectOutput::builder()
            .content_length(12)
            .e_tag("\"etag\"")
            .last_modified(aws_smithy_types::DateTime::from_secs(1_793_000_000))
            .storage_class(aws_sdk_s3::types::StorageClass::StandardIa)
            .version_id("v1")
            .checksum_crc32("crc==")
            .checksum_sha256("sha==")
            .content_type("text/plain")
            .cache_control("no-cache")
            .content_disposition("attachment")
            .content_encoding("gzip")
            .metadata("owner", "team")
            .server_side_encryption(ServerSideEncryption::AwsKms)
            .ssekms_key_id("key-1")
            .bucket_key_enabled(true)
            .object_lock_mode(ObjectLockMode::Governance)
            .object_lock_retain_until_date(aws_smithy_types::DateTime::from_secs(1_800_000_000))
            .object_lock_legal_hold_status(ObjectLockLegalHoldStatus::On)
            .build();
        let head = object_head("k", &out);
        assert_eq!(head.key, "k");
        assert_eq!(head.size, 12);
        assert_eq!(head.etag.as_deref(), Some("etag"));
        assert_eq!(head.storage_class.as_deref(), Some("STANDARD_IA"));
        assert_eq!(head.version_id.as_deref(), Some("v1"));
        assert_eq!(head.checksums.crc32.as_deref(), Some("crc=="));
        assert_eq!(head.checksums.sha256.as_deref(), Some("sha=="));
        assert_eq!(head.checksums.crc32c, None);
        assert_eq!(head.meta.content_type.as_deref(), Some("text/plain"));
        assert_eq!(
            head.meta.user_meta.get("owner").map(String::as_str),
            Some("team")
        );
        assert_eq!(
            head.meta.sse,
            Sse::Kms {
                key_id: Some("key-1".into()),
                bucket_key: true
            }
        );
        let lock = head.meta.object_lock.unwrap();
        assert_eq!(lock.mode, Some(LockMode::Governance));
        assert!(lock.legal_hold);
        assert_eq!(
            lock.retain_until.map(|t| t.timestamp()),
            Some(1_800_000_000)
        );

        let plain = object_head("p", &HeadObjectOutput::builder().build());
        assert_eq!(plain.meta.sse, Sse::None);
        assert_eq!(plain.meta.object_lock, None);

        let s3 = object_head(
            "s",
            &HeadObjectOutput::builder()
                .server_side_encryption(ServerSideEncryption::Aes256)
                .build(),
        );
        assert_eq!(s3.meta.sse, Sse::S3);

        let customer = object_head(
            "c",
            &HeadObjectOutput::builder()
                .sse_customer_algorithm("AES256")
                .sse_customer_key_md5("md5==")
                .build(),
        );
        assert_eq!(
            customer.meta.sse,
            Sse::Customer {
                key_md5: "md5==".into()
            }
        );
    }
}
