//! Restoring archived objects (`RestoreObject`) and reading their state.

use std::fmt;
use std::str::FromStr;

use aws_sdk_s3::types::{GlacierJobParameters, RestoreRequest, Tier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::objectmeta::{
    BatchOptions, ObjectChange, RestoreStatus, class_name, parse_restore, run_batch,
    unrestored_archive,
};
use crate::ops::S3;
use crate::s3uri::{validate_bucket_name, validate_key};

/// The most days a restored copy can be asked to stay.
pub const MAX_RESTORE_DAYS: u32 = 3650;

/// How fast a restore runs (and what it costs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreTier {
    /// Minutes; not for `DEEP_ARCHIVE`.
    Expedited,
    /// Hours.
    Standard,
    /// The cheapest and slowest.
    Bulk,
}

impl RestoreTier {
    /// The name S3 uses.
    pub fn as_str(self) -> &'static str {
        match self {
            RestoreTier::Expedited => "Expedited",
            RestoreTier::Standard => "Standard",
            RestoreTier::Bulk => "Bulk",
        }
    }

    fn to_sdk(self) -> Tier {
        match self {
            RestoreTier::Expedited => Tier::Expedited,
            RestoreTier::Standard => Tier::Standard,
            RestoreTier::Bulk => Tier::Bulk,
        }
    }
}

impl fmt::Display for RestoreTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RestoreTier {
    type Err = Error;

    /// Accepts the S3 names in any letter case.
    fn from_str(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "expedited" => Ok(RestoreTier::Expedited),
            "standard" => Ok(RestoreTier::Standard),
            "bulk" => Ok(RestoreTier::Bulk),
            _ => Err(Error::Usage(format!(
                "'{}' is not a restore tier; use Expedited, Standard or Bulk",
                text.escape_debug()
            ))),
        }
    }
}

/// The archive and restore state of one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveState {
    /// The storage class (`STANDARD` when S3 omits it).
    pub storage_class: String,
    /// The object's data is archived and cannot be read until it is
    /// restored: `GLACIER`, `DEEP_ARCHIVE` or an Intelligent-Tiering archive
    /// tier, with no finished restore.
    pub archived: bool,
    /// A restore ran or is running; `None` when none was requested.
    pub restore: Option<RestoreStatus>,
}

impl S3 {
    /// Reads the storage class and restore state of an object (or version)
    /// with `HeadObject`.
    pub async fn object_restore_state(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<ArchiveState> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        let head = self
            .client_for(bucket)
            .await?
            .head_object()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(format!("reading s3://{bucket}/{key}"))
            })?;
        Ok(ArchiveState {
            storage_class: class_name(&head),
            archived: unrestored_archive(&head).is_some(),
            restore: head.restore().and_then(parse_restore),
        })
    }

    /// Asks S3 to restore each archived object in `keys` (or `version_id`,
    /// for one object) for `days` days with `tier`. Restoring is
    /// asynchronous: success means the request was accepted, and
    /// [`object_restore_state`](S3::object_restore_state) shows when the
    /// copy is ready. An object that is already restored gets a new expiry; one whose
    /// restore is running, or that is not archived, is that key's error, as
    /// the server worded it. With `options.dry_run` nothing is sent.
    pub async fn restore_objects(
        &self,
        bucket: &str,
        keys: &[String],
        days: u32,
        tier: RestoreTier,
        version_id: Option<&str>,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        if !(1..=MAX_RESTORE_DAYS).contains(&days) {
            return Err(Error::Usage(format!(
                "restore for 1 to {MAX_RESTORE_DAYS} days"
            )));
        }
        for key in keys {
            validate_key(key)?;
            if key.ends_with('/') {
                return Err(Error::Usage(format!(
                    "'{}' is a folder; select the objects in it instead",
                    key.escape_debug()
                )));
            }
        }
        if version_id.is_some() && keys.len() > 1 {
            return Err(Error::Usage(
                "a version ID applies to one object; select one object".into(),
            ));
        }
        let request = RestoreRequest::builder()
            .days(i32::try_from(days).unwrap_or(i32::MAX))
            .glacier_job_parameters(
                GlacierJobParameters::builder()
                    .tier(tier.to_sdk())
                    .build()
                    .map_err(|error| {
                        Error::Internal(format!("failed to build an S3 request: {error}"))
                    })?,
            )
            .build();
        let client = self.client_for(bucket).await?;
        run_batch(keys, options, |key| {
            let request = request.clone();
            let client = client.clone();
            async move {
                if options.dry_run {
                    return Ok(ObjectChange::changed(&key, None));
                }
                client
                    .restore_object()
                    .bucket(bucket)
                    .key(&key)
                    .set_version_id(version_id.map(str::to_owned))
                    .restore_request(request)
                    .set_request_payer(self.request_payer())
                    .customize()
                    .config_override(self.metadata_timeout())
                    .send()
                    .await
                    .map_err(|error| {
                        Error::from(error).with_context(format!("restoring s3://{bucket}/{key}"))
                    })?;
                Ok(ObjectChange::changed(&key, None))
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn tiers_parse_in_any_case() {
        assert_eq!("bulk".parse::<RestoreTier>().unwrap(), RestoreTier::Bulk);
        assert_eq!(
            " Expedited ".parse::<RestoreTier>().unwrap(),
            RestoreTier::Expedited
        );
        assert_eq!(
            "fast".parse::<RestoreTier>().unwrap_err().kind(),
            ErrorKind::Usage
        );
        assert_eq!(RestoreTier::Standard.to_string(), "Standard");
    }
}
