//! Object metadata commands (docs/desktop-api.md, "Object metadata (T06)").
//!
//! Thin wrappers over `bucketbridge_core::objectmeta`: they resolve the
//! account, parse the arguments and return the per-object results. SSE-C keys
//! arrive as [`Secret`] arguments (zeroized when dropped, redacted in debug
//! output) and are never logged; the event log only gets counts.

use std::str::FromStr;

use bucketbridge_core::Result;
use bucketbridge_core::objectmeta::{
    BatchOptions, CannedAcl, CustomerKey, EncryptionChange, HeaderChanges, ObjectChange,
    ObjectDetails, StorageClass, TagEdit,
};
use serde::Deserialize;
use tauri::State;

use super::{CommandResult, logged};
use crate::error::Secret;
use crate::state::AppState;

/// The encryption a command changes objects to, as the UI sends it.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EncryptionArg {
    /// SSE-S3.
    S3,
    /// SSE-KMS.
    Kms {
        /// Key ID, ARN or alias; `null` is the AWS managed key.
        key_id: Option<String>,
        /// Whether an S3 Bucket Key is used.
        bucket_key: bool,
    },
    /// SSE-C with the key given here.
    #[serde(rename = "c")]
    Customer {
        /// 64 hexadecimal digits or the base64 of 32 bytes.
        key: Secret,
    },
}

impl EncryptionArg {
    fn parse(&self) -> Result<EncryptionChange> {
        Ok(match self {
            EncryptionArg::S3 => EncryptionChange::S3,
            EncryptionArg::Kms { key_id, bucket_key } => EncryptionChange::Kms {
                key_id: key_id.clone(),
                bucket_key: *bucket_key,
            },
            EncryptionArg::Customer { key } => {
                EncryptionChange::Customer(CustomerKey::parse(key.expose())?)
            }
        })
    }
}

fn parse_key(key: Option<&Secret>) -> Result<Option<CustomerKey>> {
    key.map(|secret| CustomerKey::parse(secret.expose()))
        .transpose()
}

/// One line in the event log per batch: counts only, never keys or secrets.
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

/// Key, size, headers, user metadata, encryption, tags and ACL of one object.
/// `sse_c_key` is the key of an object encrypted with SSE-C.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_object_details(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    sse_c_key: Option<Secret>,
) -> CommandResult<ObjectDetails> {
    let core = state.core()?;
    logged("reading object properties", async {
        let sse_c = parse_key(sse_c_key.as_ref())?;
        let s3 = core.session.s3_for(&account_id).await?;
        s3.object_details(&bucket, &key, sse_c.as_ref()).await
    })
    .await
}

/// Replaces, or adds to and removes from, the tags of the objects.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_object_tags(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    edit: TagEdit,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("editing tags", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .update_object_tags(&bucket, &keys, &edit, BatchOptions::default())
            .await?;
        record("edited the tags of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// Changes the storage class of the objects with an in-place copy.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_object_storage_class(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    storage_class: String,
    sse_c_key: Option<Secret>,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("changing the storage class", async {
        let class = StorageClass::from_str(&storage_class)?;
        let sse_c = parse_key(sse_c_key.as_ref())?;
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .set_object_storage_class(
                &bucket,
                &keys,
                class,
                sse_c.as_ref(),
                BatchOptions::default(),
            )
            .await?;
        record(
            &format!("set the storage class {class} on objects"),
            &bucket,
            &results,
        );
        Ok(results)
    })
    .await
}

/// Changes the server-side encryption of the objects with an in-place copy.
/// `source_sse_c_key` is the key the objects have now, when it is SSE-C.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_object_encryption(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    encryption: EncryptionArg,
    source_sse_c_key: Option<Secret>,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("changing the encryption", async {
        let target = encryption.parse()?;
        let source = parse_key(source_sse_c_key.as_ref())?;
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .set_object_encryption(
                &bucket,
                &keys,
                &target,
                source.as_ref(),
                BatchOptions::default(),
            )
            .await?;
        record("changed the encryption of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// Edits HTTP headers and user metadata of the objects with an in-place copy.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_object_headers(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    changes: HeaderChanges,
    sse_c_key: Option<Secret>,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("editing headers", async {
        let sse_c = parse_key(sse_c_key.as_ref())?;
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .update_object_headers(
                &bucket,
                &keys,
                &changes,
                sse_c.as_ref(),
                BatchOptions::default(),
            )
            .await?;
        record("edited the headers of objects", &bucket, &results);
        Ok(results)
    })
    .await
}

/// Applies a canned ACL to the objects.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_object_acl(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    acl: String,
) -> CommandResult<Vec<ObjectChange>> {
    let core = state.core()?;
    logged("setting ACLs", async {
        let canned = CannedAcl::from_str(&acl)?;
        let s3 = core.session.s3_for(&account_id).await?;
        let results = s3
            .set_object_acl(&bucket, &keys, canned, BatchOptions::default())
            .await?;
        record(
            &format!("set the ACL {canned} on objects"),
            &bucket,
            &results,
        );
        Ok(results)
    })
    .await
}
