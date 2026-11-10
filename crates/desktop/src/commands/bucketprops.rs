//! Default encryption and tags of a bucket (docs/desktop-api.md, "Bucket
//! encryption and tags (P3)"). Thin wrappers over
//! `bucketbridge_core::bucketprops`.

use std::collections::BTreeMap;

use bucketbridge_core::bucketprops::BucketEncryption;
use bucketbridge_core::objectmeta::{AclGrant, BucketAcl, CannedAcl};
use tauri::State;

use super::{CommandResult, logged};
use crate::state::AppState;

/// The default encryption of a bucket; `null` when none is configured.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<BucketEncryption>> {
    let core = state.core()?;
    logged("reading the default encryption", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_encryption(&bucket).await
    })
    .await
}

/// Sets the default encryption (SSE-S3 or SSE-KMS); `usage` for a key with
/// SSE-S3 or a malformed key.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: BucketEncryption,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the default encryption", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_encryption(&bucket, &config).await
    })
    .await
}

/// Removes the default encryption setting; succeeds when there is none.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("removing the default encryption", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_encryption(&bucket).await
    })
    .await
}

/// The tags of a bucket (empty when it has none).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_tags(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BTreeMap<String, String>> {
    let core = state.core()?;
    logged("reading the bucket tags", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_tags(&bucket).await
    })
    .await
}

/// Replaces the tags of a bucket after validating them (at most 50; an empty
/// set removes every tag).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_tags(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    tags: BTreeMap<String, String>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the bucket tags", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_tags(&bucket, &tags).await
    })
    .await
}

/// The ACL of a bucket and whether ACLs are disabled on it.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_acl(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BucketAcl> {
    let core = state.core()?;
    logged("reading the bucket ACL", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_acl(&bucket).await
    })
    .await
}

/// Applies a canned ACL to a bucket (`usage` for the two bucket-owner ACLs;
/// `conflict` when ACLs are disabled).
#[tauri::command(rename_all = "snake_case")]
pub async fn set_bucket_acl(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    acl: CannedAcl,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("setting the bucket ACL", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_acl(&bucket, acl).await
    })
    .await
}

/// Replaces every grant of a bucket's ACL.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_acl_grants(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    grants: Vec<AclGrant>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("setting the bucket ACL grants", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_acl_grants(&bucket, &grants).await
    })
    .await
}

/// Replaces every grant of an object's ACL.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_object_acl_grants(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    grants: Vec<AclGrant>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("setting the object ACL grants", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_object_acl_grants(&bucket, &key, &grants).await
    })
    .await
}
