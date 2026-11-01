//! Object Lock, object ownership, Glacier restore and MFA delete (plan task
//! T07, the object-policy part).
//!
//! Everything is a method of [`S3`], implemented in this module and its
//! children:
//!
//! | Method | What it does |
//! |---|---|
//! | [`S3::get_bucket_object_lock`], [`put_bucket_object_lock`](S3::put_bucket_object_lock) | whether Object Lock is on for a bucket, and its default retention |
//! | [`S3::get_object_lock`] | the retention and legal hold of one object (or version) |
//! | [`S3::set_object_retention`] | a [`RetentionChange`] (set a mode and date, or clear) on many objects |
//! | [`S3::set_object_legal_hold`] | legal hold on or off for many objects |
//! | [`S3::get_object_ownership`], [`put_object_ownership`](S3::put_object_ownership) | the bucket's [`ObjectOwnership`] |
//! | [`S3::restore_objects`] | `RestoreObject` with a [`RestoreTier`] and days on many archived objects |
//! | [`S3::object_restore_state`] | the archive and restore state from `HeadObject` |
//! | [`S3::put_bucket_versioning_mfa`] | versioning and MFA delete with an [`MfaDeleteAuth`] |
//!
//! # Errors
//!
//! A change on a locked object that the server refuses (a retention that
//! cannot be shortened, a version under legal hold) is never reworded: the
//! server's code and message are in the [`ObjectChange::error`] of the key
//! (and in the error of the single-bucket calls), so the user reads exactly
//! what S3 said. Batches follow the rules of [`crate::objectmeta`]: a problem
//! with one object never stops the others.
//!
//! # Governance bypass
//!
//! `bypass_governance` sends `x-amz-bypass-governance-retention: true`, which
//! lets users with the `s3:BypassGovernanceRetention` permission shorten or
//! remove a governance retention. It never affects compliance mode.

use std::fmt;
use std::str::FromStr;

use aws_sdk_s3::types::{
    DefaultRetention, ObjectLockConfiguration, ObjectLockEnabled, ObjectLockLegalHold,
    ObjectLockLegalHoldStatus, ObjectLockRetention, ObjectLockRetentionMode, ObjectLockRule,
    ObjectOwnership as SdkOwnership, OwnershipControls, OwnershipControlsRule,
};
use aws_smithy_types::DateTime as SdkDateTime;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod mfa;
mod restore;

pub use mfa::MfaDeleteAuth;
pub use restore::{ArchiveState, MAX_RESTORE_DAYS, RestoreTier};

use crate::error::{Error, Result, ServiceError};
use crate::model::LockMode;
use crate::objectmeta::{BatchOptions, ObjectChange, run_batch};
use crate::ops::{S3, to_utc};
use crate::s3uri::{validate_bucket_name, validate_key};

/// The longest default retention S3 accepts, in years.
pub const MAX_RETENTION_YEARS: u32 = 100;

/// The longest default retention S3 accepts, in days.
pub const MAX_RETENTION_DAYS: u32 = 36_500;

fn sdk_mode(mode: LockMode) -> ObjectLockRetentionMode {
    match mode {
        LockMode::Governance => ObjectLockRetentionMode::Governance,
        LockMode::Compliance => ObjectLockRetentionMode::Compliance,
    }
}

fn model_mode(mode: &ObjectLockRetentionMode) -> Option<LockMode> {
    match mode {
        ObjectLockRetentionMode::Governance => Some(LockMode::Governance),
        ObjectLockRetentionMode::Compliance => Some(LockMode::Compliance),
        _ => None,
    }
}

/// The name S3 uses for a mode.
pub fn mode_name(mode: LockMode) -> &'static str {
    match mode {
        LockMode::Governance => "GOVERNANCE",
        LockMode::Compliance => "COMPLIANCE",
    }
}

/// Parses `governance` or `compliance` in any letter case.
pub fn parse_mode(text: &str) -> Result<LockMode> {
    match text.trim().to_ascii_lowercase().as_str() {
        "governance" => Ok(LockMode::Governance),
        "compliance" => Ok(LockMode::Compliance),
        _ => Err(Error::Usage(format!(
            "'{}' is not a retention mode; use GOVERNANCE or COMPLIANCE",
            text.escape_debug()
        ))),
    }
}

// ---- bucket configuration ----

/// How long new objects are retained by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultRetentionRule {
    /// The retention mode.
    pub mode: LockMode,
    /// The period in days (exactly one of `days` and `years` is set).
    pub days: Option<u32>,
    /// The period in years.
    pub years: Option<u32>,
}

impl DefaultRetentionRule {
    /// Checks what S3 would reject: one unit only, a positive period within
    /// the limits.
    pub fn validate(&self) -> Result<()> {
        match (self.days, self.years) {
            (Some(days), None) if (1..=MAX_RETENTION_DAYS).contains(&days) => Ok(()),
            (None, Some(years)) if (1..=MAX_RETENTION_YEARS).contains(&years) => Ok(()),
            (Some(_), None) => Err(Error::Usage(format!(
                "the default retention must be 1 to {MAX_RETENTION_DAYS} days"
            ))),
            (None, Some(_)) => Err(Error::Usage(format!(
                "the default retention must be 1 to {MAX_RETENTION_YEARS} years"
            ))),
            _ => Err(Error::Usage(
                "give the default retention in days or in years, not both and not neither".into(),
            )),
        }
    }
}

/// The Object Lock configuration of a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BucketObjectLock {
    /// Object Lock is enabled (it can never be switched off again).
    pub enabled: bool,
    /// The default retention of new objects, when one is set.
    pub default_retention: Option<DefaultRetentionRule>,
}

fn bucket_context(action: &str, bucket: &str) -> String {
    format!("{action} of bucket '{bucket}'")
}

fn object_context(action: &str, bucket: &str, key: &str) -> String {
    format!("{action} of s3://{bucket}/{key}")
}

impl S3 {
    /// Reads whether Object Lock is enabled on the bucket and its default
    /// retention. A bucket without Object Lock is `enabled: false`, not an
    /// error.
    pub async fn get_bucket_object_lock(&self, bucket: &str) -> Result<BucketObjectLock> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .get_object_lock_configuration()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = match reply {
            Ok(out) => out,
            Err(error) => {
                let error = Error::from(error);
                return if error.service().and_then(ServiceError::code)
                    == Some("ObjectLockConfigurationNotFoundError")
                {
                    Ok(BucketObjectLock::default())
                } else {
                    Err(error
                        .with_context(bucket_context("reading the Object Lock setting", bucket)))
                };
            }
        };
        let config = out.object_lock_configuration();
        let enabled = config
            .and_then(|c| c.object_lock_enabled())
            .is_some_and(|e| matches!(e, ObjectLockEnabled::Enabled));
        let default_retention = config
            .and_then(|c| c.rule())
            .and_then(|r| r.default_retention())
            .and_then(|d| {
                Some(DefaultRetentionRule {
                    mode: d.mode().and_then(model_mode)?,
                    days: d.days().and_then(|v| u32::try_from(v).ok()),
                    years: d.years().and_then(|v| u32::try_from(v).ok()),
                })
            });
        Ok(BucketObjectLock {
            enabled,
            default_retention,
        })
    }

    /// Enables Object Lock (when it is not yet) and sets or clears the
    /// default retention. `None` clears the rule and keeps Object Lock on.
    /// Enabling needs versioning on the bucket; S3 says so when it is not.
    pub async fn put_bucket_object_lock(
        &self,
        bucket: &str,
        default_retention: Option<&DefaultRetentionRule>,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        let rule = match default_retention {
            Some(rule) => {
                rule.validate()?;
                let retention = DefaultRetention::builder()
                    .mode(sdk_mode(rule.mode))
                    .set_days(rule.days.and_then(|d| i32::try_from(d).ok()))
                    .set_years(rule.years.and_then(|y| i32::try_from(y).ok()))
                    .build();
                Some(
                    ObjectLockRule::builder()
                        .default_retention(retention)
                        .build(),
                )
            }
            None => None,
        };
        let config = ObjectLockConfiguration::builder()
            .object_lock_enabled(ObjectLockEnabled::Enabled)
            .set_rule(rule)
            .build();
        self.client_for(bucket)
            .await?
            .put_object_lock_configuration()
            .bucket(bucket)
            .object_lock_configuration(config)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error)
                    .with_context(bucket_context("saving the Object Lock setting", bucket))
            })?;
        Ok(())
    }
}

// ---- object retention and legal hold ----

/// The lock state of one object (or version).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectLockState {
    /// The retention mode, when a retention is set.
    pub mode: Option<LockMode>,
    /// When the retention ends.
    pub retain_until: Option<DateTime<Utc>>,
    /// A legal hold is on.
    pub legal_hold: bool,
}

impl ObjectLockState {
    /// Short text for reports: `none`, `GOVERNANCE until ...`, with
    /// `+ legal hold`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let (Some(mode), Some(until)) = (self.mode, self.retain_until) {
            parts.push(format!(
                "{} until {}",
                mode_name(mode),
                until.format("%Y-%m-%dT%H:%M:%SZ")
            ));
        }
        if self.legal_hold {
            parts.push("legal hold".to_owned());
        }
        if parts.is_empty() {
            "none".to_owned()
        } else {
            parts.join(" + ")
        }
    }
}

/// A change to the retention of objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RetentionChange {
    /// Retain the objects in `mode` until `retain_until`.
    Set {
        /// The retention mode.
        mode: LockMode,
        /// When the retention ends; must be in the future.
        retain_until: DateTime<Utc>,
    },
    /// Remove the retention (a governance retention needs the bypass).
    Clear,
}

/// `NoSuchObjectLockConfiguration`, or the `InvalidRequest` S3 sends for a
/// bucket without Object Lock: the object has nothing to report.
fn is_no_lock(error: &Error) -> bool {
    error.service().is_some_and(|service| {
        service.code() == Some("NoSuchObjectLockConfiguration")
            || (service.code() == Some("InvalidRequest")
                && service
                    .message()
                    .is_some_and(|m| m.contains("Object Lock Configuration")))
    })
}

fn check_target(bucket: &str, keys: &[String], version_id: Option<&str>) -> Result<()> {
    validate_bucket_name(bucket)?;
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
    Ok(())
}

impl S3 {
    /// Reads the retention and legal hold of an object, or of one version.
    /// An object (or a bucket) without Object Lock reads as no retention and
    /// no hold.
    pub async fn get_object_lock(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> Result<ObjectLockState> {
        check_target(bucket, &[key.to_owned()], version_id)?;
        let client = self.client_for(bucket).await?;
        let retention = client
            .get_object_retention()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let (mode, retain_until) = match retention {
            Ok(out) => {
                let r = out.retention();
                (
                    r.and_then(|r| r.mode()).and_then(model_mode),
                    r.and_then(|r| r.retain_until_date()).and_then(to_utc),
                )
            }
            Err(error) => {
                let error = Error::from(error);
                if !is_no_lock(&error) {
                    return Err(error.with_context(object_context(
                        "reading the retention",
                        bucket,
                        key,
                    )));
                }
                (None, None)
            }
        };
        let hold = client
            .get_object_legal_hold()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let legal_hold = match hold {
            Ok(out) => out
                .legal_hold()
                .and_then(|h| h.status())
                .is_some_and(|s| matches!(s, ObjectLockLegalHoldStatus::On)),
            Err(error) => {
                let error = Error::from(error);
                if !is_no_lock(&error) {
                    return Err(error.with_context(object_context(
                        "reading the legal hold",
                        bucket,
                        key,
                    )));
                }
                false
            }
        };
        Ok(ObjectLockState {
            mode,
            retain_until,
            legal_hold,
        })
    }

    /// Sets or clears the retention of each object in `keys` (or of
    /// `version_id`, for one object). With `options.dry_run` nothing is
    /// written: the objects are read and reported with their current state in
    /// [`ObjectChange::previous`]. A refusal by the server (a shortened
    /// compliance retention, a missing bypass) is the key's
    /// [`ObjectChange::error`], with the server's code and message as sent.
    pub async fn set_object_retention(
        &self,
        bucket: &str,
        keys: &[String],
        change: RetentionChange,
        version_id: Option<&str>,
        bypass_governance: bool,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        check_target(bucket, keys, version_id)?;
        let retention = match change {
            RetentionChange::Set { mode, retain_until } => {
                if retain_until <= Utc::now() {
                    return Err(Error::Usage(
                        "the retain-until date must be in the future".into(),
                    ));
                }
                ObjectLockRetention::builder()
                    .mode(sdk_mode(mode))
                    .retain_until_date(SdkDateTime::from_secs_and_nanos(
                        retain_until.timestamp(),
                        0,
                    ))
                    .build()
            }
            RetentionChange::Clear => ObjectLockRetention::builder().build(),
        };
        let client = self.client_for(bucket).await?;
        run_batch(keys, options, |key| {
            let retention = retention.clone();
            let client = client.clone();
            async move {
                if options.dry_run {
                    let state = self.get_object_lock(bucket, &key, version_id).await?;
                    return Ok(ObjectChange::changed(&key, Some(state.summary())));
                }
                client
                    .put_object_retention()
                    .bucket(bucket)
                    .key(&key)
                    .set_version_id(version_id.map(str::to_owned))
                    .retention(retention)
                    .set_bypass_governance_retention(bypass_governance.then_some(true))
                    .set_request_payer(self.request_payer())
                    .customize()
                    .config_override(self.metadata_timeout())
                    .send()
                    .await
                    .map_err(|error| {
                        Error::from(error).with_context(object_context(
                            "changing the retention",
                            bucket,
                            &key,
                        ))
                    })?;
                Ok(ObjectChange::changed(&key, None))
            }
        })
        .await
    }

    /// Turns the legal hold of each object in `keys` (or of `version_id`)
    /// on or off. A hold that already is as wanted is reported unchanged
    /// and nothing is sent. Server refusals are the key's error, verbatim.
    pub async fn set_object_legal_hold(
        &self,
        bucket: &str,
        keys: &[String],
        on: bool,
        version_id: Option<&str>,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        check_target(bucket, keys, version_id)?;
        let status = if on {
            ObjectLockLegalHoldStatus::On
        } else {
            ObjectLockLegalHoldStatus::Off
        };
        let client = self.client_for(bucket).await?;
        run_batch(keys, options, |key| {
            let status = status.clone();
            let client = client.clone();
            async move {
                let current = self.get_object_lock(bucket, &key, version_id).await;
                let previous = current
                    .as_ref()
                    .ok()
                    .map(|state| if state.legal_hold { "on" } else { "off" }.to_owned());
                if current.as_ref().is_ok_and(|state| state.legal_hold == on) {
                    return Ok(ObjectChange::unchanged(&key, previous));
                }
                if options.dry_run {
                    current?;
                    return Ok(ObjectChange::changed(&key, previous));
                }
                client
                    .put_object_legal_hold()
                    .bucket(bucket)
                    .key(&key)
                    .set_version_id(version_id.map(str::to_owned))
                    .legal_hold(ObjectLockLegalHold::builder().status(status).build())
                    .set_request_payer(self.request_payer())
                    .customize()
                    .config_override(self.metadata_timeout())
                    .send()
                    .await
                    .map_err(|error| {
                        Error::from(error).with_context(object_context(
                            "changing the legal hold",
                            bucket,
                            &key,
                        ))
                    })?;
                Ok(ObjectChange::changed(&key, previous))
            }
        })
        .await
    }
}

// ---- object ownership ----

/// The Object Ownership setting of a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectOwnership {
    /// ACLs are disabled; the bucket owner owns every object.
    BucketOwnerEnforced,
    /// The uploading account owns its objects.
    ObjectWriter,
    /// The bucket owner owns objects uploaded with the
    /// `bucket-owner-full-control` ACL.
    BucketOwnerPreferred,
}

impl ObjectOwnership {
    /// Every setting.
    pub const ALL: [ObjectOwnership; 3] = [
        ObjectOwnership::BucketOwnerEnforced,
        ObjectOwnership::ObjectWriter,
        ObjectOwnership::BucketOwnerPreferred,
    ];

    /// The name S3 uses.
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectOwnership::BucketOwnerEnforced => "BucketOwnerEnforced",
            ObjectOwnership::ObjectWriter => "ObjectWriter",
            ObjectOwnership::BucketOwnerPreferred => "BucketOwnerPreferred",
        }
    }

    fn to_sdk(self) -> SdkOwnership {
        match self {
            ObjectOwnership::BucketOwnerEnforced => SdkOwnership::BucketOwnerEnforced,
            ObjectOwnership::ObjectWriter => SdkOwnership::ObjectWriter,
            ObjectOwnership::BucketOwnerPreferred => SdkOwnership::BucketOwnerPreferred,
        }
    }
}

impl fmt::Display for ObjectOwnership {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ObjectOwnership {
    type Err = Error;

    /// Accepts the S3 names in any letter case, with `-` or `_` between
    /// words.
    fn from_str(text: &str) -> Result<Self> {
        let wanted: String = text
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .collect::<String>()
            .to_ascii_lowercase();
        ObjectOwnership::ALL
            .into_iter()
            .find(|o| o.as_str().to_ascii_lowercase() == wanted)
            .ok_or_else(|| {
                Error::Usage(format!(
                    "'{}' is not an Object Ownership setting; use BucketOwnerEnforced, ObjectWriter or BucketOwnerPreferred",
                    text.escape_debug()
                ))
            })
    }
}

impl S3 {
    /// The Object Ownership of the bucket; `None` when none is configured
    /// (S3 then behaves as `ObjectWriter`).
    pub async fn get_object_ownership(&self, bucket: &str) -> Result<Option<ObjectOwnership>> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .get_bucket_ownership_controls()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        match reply {
            Ok(out) => Ok(out
                .ownership_controls()
                .and_then(|c| c.rules().first())
                .and_then(|rule| match rule.object_ownership() {
                    SdkOwnership::BucketOwnerEnforced => Some(ObjectOwnership::BucketOwnerEnforced),
                    SdkOwnership::ObjectWriter => Some(ObjectOwnership::ObjectWriter),
                    SdkOwnership::BucketOwnerPreferred => {
                        Some(ObjectOwnership::BucketOwnerPreferred)
                    }
                    _ => None,
                })),
            Err(error) => {
                let error = Error::from(error);
                if error.service().and_then(ServiceError::code)
                    == Some("OwnershipControlsNotFoundError")
                {
                    Ok(None)
                } else {
                    Err(error.with_context(bucket_context("reading the Object Ownership", bucket)))
                }
            }
        }
    }

    /// Sets the Object Ownership of the bucket.
    pub async fn put_object_ownership(
        &self,
        bucket: &str,
        ownership: ObjectOwnership,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        let rule = OwnershipControlsRule::builder()
            .object_ownership(ownership.to_sdk())
            .build()
            .map_err(|error| Error::Internal(format!("failed to build an S3 request: {error}")))?;
        let controls = OwnershipControls::builder()
            .rules(rule)
            .build()
            .map_err(|error| Error::Internal(format!("failed to build an S3 request: {error}")))?;
        self.client_for(bucket)
            .await?
            .put_bucket_ownership_controls()
            .bucket(bucket)
            .ownership_controls(controls)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error)
                    .with_context(bucket_context("saving the Object Ownership", bucket))
            })?;
        Ok(())
    }
}
