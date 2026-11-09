//! Storage usage of a bucket or prefix, computed by listing.
//!
//! [`S3::compute_usage`] walks the keys under a prefix and adds up what it
//! finds. Nothing is read from a metric or an inventory, so the numbers are
//! exact for the moment of the listing but cost one request per 1000 keys.
//!
//! * Without versions, a plain listing counts the current objects: their
//!   number and total size. Zero-byte folder markers count as objects.
//! * With [`UsageOptions::versions`], the version listing is walked instead
//!   (use it when the bucket has versioning on or suspended). Current
//!   versions are counted as above; every older version adds to
//!   [`NoncurrentUsage::versions`] and [`NoncurrentUsage::bytes`], and delete
//!   markers are counted in [`NoncurrentUsage::delete_markers`] (they have no
//!   size).
//! * With [`UsageOptions::multipart`], unfinished multipart uploads are
//!   listed and the parts of each are added up ([`MultipartUsage`]). That
//!   needs one `ListParts` request per upload.
//!
//! The call reports progress after every page and stops with
//! [`Error::Cancelled`] when the token is cancelled (checked between
//! requests). A page that fails fails the whole call: a total with a hole in
//! it would be misleading.

use futures::StreamExt;
use serde::{Deserialize, Serialize};
pub use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// What to add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageOptions {
    /// Walk all versions and delete markers (for a bucket with versioning).
    pub versions: bool,
    /// Also size the unfinished multipart uploads.
    pub multipart: bool,
}

/// Current versions of objects plus, when asked for, the rest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of (current) objects.
    pub objects: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// Older versions and delete markers; `None` when versions were not
    /// listed.
    pub noncurrent: Option<NoncurrentUsage>,
    /// Unfinished multipart uploads; `None` when they were not sized.
    pub multipart: Option<MultipartUsage>,
}

/// Versions that are not the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NoncurrentUsage {
    /// Number of non-current versions (delete markers excluded).
    pub versions: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// Number of delete markers, current or not.
    pub delete_markers: u64,
}

/// Unfinished multipart uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MultipartUsage {
    /// Number of uploads.
    pub uploads: u64,
    /// Number of parts uploaded so far.
    pub parts: u64,
    /// Total size of those parts in bytes.
    pub bytes: u64,
}

/// What the computation is busy with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePhase {
    /// Listing objects or versions.
    Objects,
    /// Sizing multipart uploads.
    Multipart,
}

/// A progress report: the totals so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageProgress {
    /// Current phase.
    pub phase: UsagePhase,
    /// Totals counted so far.
    pub usage: Usage,
}

impl S3 {
    /// Adds up the objects of `bucket` under `prefix` (empty for the whole
    /// bucket). See the [module docs](self). `progress` is called after every
    /// page; `cancel` ends the call with [`Error::Cancelled`].
    pub async fn compute_usage(
        &self,
        bucket: &str,
        prefix: &str,
        options: UsageOptions,
        cancel: Option<&CancellationToken>,
        mut progress: impl FnMut(&UsageProgress) + Send,
    ) -> Result<Usage> {
        validate_bucket_name(bucket)?;
        let check = || match cancel {
            Some(token) if token.is_cancelled() => Err(Error::Cancelled),
            _ => Ok(()),
        };
        let mut usage = Usage {
            noncurrent: options.versions.then(NoncurrentUsage::default),
            multipart: options.multipart.then(MultipartUsage::default),
            ..Usage::default()
        };
        check()?;
        if options.versions {
            let mut token: Option<String> = None;
            loop {
                let page = self
                    .list_versions(bucket, prefix, true, token.as_deref(), 1000)
                    .await?;
                check()?;
                let noncurrent = usage.noncurrent.get_or_insert_with(Default::default);
                for entry in &page.entries {
                    if entry.is_delete_marker {
                        noncurrent.delete_markers += 1;
                    } else if entry.is_latest {
                        usage.objects += 1;
                        usage.bytes += entry.size;
                    } else {
                        noncurrent.versions += 1;
                        noncurrent.bytes += entry.size;
                    }
                }
                progress(&UsageProgress {
                    phase: UsagePhase::Objects,
                    usage: usage.clone(),
                });
                match page.next_token {
                    Some(next) => token = Some(next),
                    None => break,
                }
            }
        } else {
            let mut stream = Box::pin(self.list_all_stream(bucket, prefix));
            let mut since_report = 0u32;
            while let Some(entry) = stream.next().await {
                let entry = entry?;
                usage.objects += 1;
                usage.bytes += entry.size;
                since_report += 1;
                // The stream hands out whole pages of up to 1000 keys.
                if since_report == 1000 {
                    since_report = 0;
                    check()?;
                    progress(&UsageProgress {
                        phase: UsagePhase::Objects,
                        usage: usage.clone(),
                    });
                }
            }
            check()?;
            progress(&UsageProgress {
                phase: UsagePhase::Objects,
                usage: usage.clone(),
            });
        }
        if options.multipart {
            let uploads = self.list_multipart_uploads(bucket, prefix).await?;
            let mut total = MultipartUsage::default();
            for upload in &uploads {
                check()?;
                let (parts, bytes) = self
                    .multipart_upload_usage(bucket, &upload.key, &upload.upload_id)
                    .await?;
                total.uploads += 1;
                total.parts += parts;
                total.bytes += bytes;
                usage.multipart = Some(total);
                progress(&UsageProgress {
                    phase: UsagePhase::Multipart,
                    usage: usage.clone(),
                });
            }
            usage.multipart = Some(total);
            check()?;
        }
        Ok(usage)
    }
}
