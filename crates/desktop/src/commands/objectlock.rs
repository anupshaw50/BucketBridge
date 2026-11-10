//! Object Lock, ownership, Glacier restore and MFA delete commands
//! (docs/desktop-api.md, "Object Lock, ownership, restore and MFA delete
//! (T07)").
//!
//! Thin wrappers over `bucketbridge_core::objectlock`. MFA codes arrive as
//! [`Secret`] arguments (redacted in debug output, never logged); the serial
//! is the one stored with the account.

use std::str::FromStr;

use bucketbridge_core::bucketcfg::VersioningStatus;
use bucketbridge_core::model::DeleteOutcome;
use bucketbridge_core::objectlock::{
    ArchiveState, BucketObjectLock, DefaultRetentionRule, MfaDeleteAuth, ObjectLockState,
    ObjectOwnership, RestoreEstimate, RestoreTier, RetentionChange,
};
use bucketbridge_core::objectmeta::{BatchOptions, ObjectChange};
use bucketbridge_core::versions::{PurgeOptions, VersionRef};
use bucketbridge_core::{Error, Result};
use tauri::State;

use super::{CommandResult, logged};
use crate::error::Secret;
use crate::state::AppState;

/// The device of `account_id` with the one-time `code`; `usage` when the
/// account has no MFA serial.
async fn mfa_auth(
    core: &crate::state::Core,
    account_id: &str,
    code: &Secret,
) -> Result<MfaDeleteAuth> {
    let accounts = core.session.list_accounts().await?;
    let account = accounts
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| Error::NotFound(format!("account '{account_id}' not found")))?;
    let serial = account.mfa_serial.as_deref().ok_or_else(|| {
        Error::Usage("this account has no MFA serial; add one in the account settings".into())
    })?;
    MfaDeleteAuth::new(serial, code.expose())
}

/// One line in the event log per batch: counts only.
fn record(what: &str, bucket: &str, results: &[ObjectChange]) {
    let failed = results.iter().filter(|r| r.error.is_some()).count();
    let changed = results.iter().filter(|r| r.changed).count();
    if failed > 0 {
        tracing::warn!(
            target: "bucketbridge::desktop",
            "{what} in bucket {bucket}: {changed} changed, {failed} of {} failed",
            results.len()
        );
    } else {
        tracing::info!(
            target: "bucketbridge::desktop",
            "{what} in bucket {bucket}: {changed} of {} changed",
            results.len()
        );
    }
}

/// Whether Object Lock is on for the bucket, and its default retention.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_object_lock(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BucketObjectLock> {
    let core = state.core()?;
    logged("reading the Object Lock setting", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_object_lock(&bucket).await
    })
    .await
}

/// Enables Object Lock and sets (or, with `null`, clears) the default
/// retention.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_object_lock(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    default_retention: Option<DefaultRetentionRule>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the Object Lock setting", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_object_lock(&bucket, default_retention.as_ref())
            .await
    })
    .await
}

/// The retention and legal hold of one object or version.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_object_lock(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: Option<String>,
) -> CommandResult<ObjectLockState> {
    let core = state.core()?;
    logged("reading the object lock", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_object_lock(&bucket, &key, version_id.as_deref())
            .await
    })
    .await
}

/// Sets or clears the retention of the objects. A refusal by the server is
/// the object's `error`, worded by the server.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_object_retention(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    change: RetentionChange,
    version_id: Option<String>,
    bypass_governance: bool,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("changing the retention", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .set_object_retention(
                &bucket,
                &keys,
                change,
                version_id.as_deref(),
                bypass_governance,
                BatchOptions::default(),
            )
            .await?;
        record("changed the retention of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// Turns the legal hold of the objects on or off.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_object_legal_hold(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    on: bool,
    version_id: Option<String>,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("changing the legal hold", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .set_object_legal_hold(
                &bucket,
                &keys,
                on,
                version_id.as_deref(),
                BatchOptions::default(),
            )
            .await?;
        record("changed the legal hold of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// The Object Ownership of the bucket; `null` when none is configured.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_object_ownership(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<ObjectOwnership>> {
    let core = state.core()?;
    logged("reading the Object Ownership", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_object_ownership(&bucket).await
    })
    .await
}

/// Sets the Object Ownership of the bucket (`BucketOwnerEnforced`,
/// `ObjectWriter` or `BucketOwnerPreferred`).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_object_ownership(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    ownership: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the Object Ownership", async {
        let ownership = ObjectOwnership::from_str(&ownership)?;
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_object_ownership(&bucket, ownership).await
    })
    .await
}

/// Requests the restore of archived objects (`tier`: `expedited`, `standard`
/// or `bulk`).
#[tauri::command(rename_all = "snake_case")]
pub async fn restore_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    days: Option<u32>,
    tier: RestoreTier,
    version_id: Option<String>,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("requesting a restore", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .restore_archived(
                &bucket,
                &keys,
                days,
                tier,
                version_id.as_deref(),
                BatchOptions::default(),
            )
            .await?;
        record("requested the restore of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// Size, object count and the list-price cost of restoring `keys` per tier.
#[tauri::command(rename_all = "snake_case")]
pub async fn estimate_restore(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
) -> CommandResult<RestoreEstimate> {
    let core = state.core()?;
    logged("estimating a restore", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let summary = s3.restore_summary(&bucket, &keys).await?;
        Ok(bucketbridge_core::objectlock::estimate_restore(&summary))
    })
    .await
}

/// The storage class and restore state of one object or version.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_restore_state(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: Option<String>,
) -> CommandResult<ArchiveState> {
    let core = state.core()?;
    logged("reading the restore state", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.object_restore_state(&bucket, &key, version_id.as_deref())
            .await
    })
    .await
}

/// Changes versioning and MFA delete with the account's MFA device;
/// `mfa_code` is a current code from it.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_versioning_mfa(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    status: VersioningStatus,
    mfa_delete: bool,
    mfa_code: Secret,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the MFA delete state", async {
        let auth = mfa_auth(&core, &account_id, &mfa_code).await?;
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_versioning_mfa(&bucket, status, mfa_delete, &auth)
            .await
    })
    .await
}

/// Deletes versions for good like `purge_versions`, with the governance
/// bypass and, for a bucket with MFA delete, a code from the account's
/// device. Versions the server refuses (retained, on legal hold) are in the
/// outcome's `failures`, worded by the server.
#[tauri::command(rename_all = "snake_case")]
pub async fn purge_versions_with(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    versions: Vec<VersionRef>,
    bypass_governance: bool,
    mfa_code: Option<Secret>,
) -> CommandResult<DeleteOutcome> {
    let core = state.core()?;
    logged("purging versions", async {
        let mfa = match &mfa_code {
            Some(code) => Some(mfa_auth(&core, &account_id, code).await?),
            None => None,
        };
        let s3 = core.session.s3_for(&account_id).await?;
        s3.purge_versions_with(
            &bucket,
            &versions,
            &PurgeOptions {
                mfa,
                bypass_governance,
            },
        )
        .await
    })
    .await
}
