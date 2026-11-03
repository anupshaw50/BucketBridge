//! Client-side encryption commands (docs/desktop-api.md, "Client-side
//! encryption (T07)").
//!
//! The encrypting upload and decrypting download are ordinary queue tasks (the
//! runner reads the mode from the task's group and the password from memory);
//! "Encrypt..." and "Decrypt..." on an existing object rewrite it in place
//! with `bucketbridge_core::cse::transform_object`. Passwords arrive as
//! strings, are wrapped at once and are never logged or stored.

use std::path::PathBuf;

use bucketbridge_core::cse::{
    self, Action, Compression, EncryptOptions, ObjectCse, TransformOutcome, TransformRequest,
};
use bucketbridge_core::engine::{self, Hooks, TransferEngine, TransferOptions};
use bucketbridge_core::model::TransferTask;
use bucketbridge_core::secrets::SecretString;
use tauri::State;

use super::{CommandResult, logged, require_absolute};
use crate::state::AppState;

/// Whether an object is client-side encrypted (its first bytes and its
/// metadata are read; no password is needed).
#[tauri::command(rename_all = "snake_case")]
pub async fn cse_probe_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> CommandResult<ObjectCse> {
    let core = state.core()?;
    logged("probing an object for encryption", async {
        let s3 = core.session.s3_for(&account_id).await?;
        cse::probe_object(&s3, &bucket, &key).await
    })
    .await
}

/// Checks a password against the header of an encrypted object. Rejects with
/// `auth` and a message that starts with `password not found` (empty
/// password) or `password not valid`, and with `usage` when the object is not
/// encrypted.
#[tauri::command(rename_all = "snake_case")]
pub async fn cse_check_password(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    password: Option<String>,
) -> CommandResult<ObjectCse> {
    let core = state.core()?;
    logged("checking an encryption password", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let password = password.map(SecretString::new);
        cse::check_object_password(&s3, &bucket, &key, password.as_ref()).await?;
        cse::probe_object(&s3, &bucket, &key).await
    })
    .await
}

/// Encrypts an object in place (download, compress and encrypt, upload under
/// the same key with its headers, conditional on the ETag that was read).
#[tauri::command(rename_all = "snake_case")]
pub async fn cse_encrypt_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    password: String,
    compression: Option<Compression>,
) -> CommandResult<TransformOutcome> {
    let core = state.core()?;
    logged("encrypting an object", async {
        let action = Action::Encrypt(EncryptOptions {
            compression: compression.unwrap_or_default(),
            ..EncryptOptions::default()
        });
        transform(&core, &account_id, bucket, key, password, action).await
    })
    .await
}

/// Decrypts an object in place. Rejects with `auth` (`password not found` or
/// `password not valid`) before anything is downloaded, and with `usage` when
/// the object is not encrypted.
#[tauri::command(rename_all = "snake_case")]
pub async fn cse_decrypt_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    password: String,
) -> CommandResult<TransformOutcome> {
    let core = state.core()?;
    logged("decrypting an object", async {
        transform(&core, &account_id, bucket, key, password, Action::Decrypt).await
    })
    .await
}

async fn transform(
    core: &crate::state::Core,
    account_id: &str,
    bucket: String,
    key: String,
    password: String,
    action: Action,
) -> bucketbridge_core::Result<TransformOutcome> {
    let settings = core.session.settings().await?;
    let s3 = core.session.s3_for(account_id).await?;
    let engine = TransferEngine::new(s3, TransferOptions::from_settings(&settings))?
        .with_if_exists(bucketbridge_core::settings::IfExists::Overwrite);
    let request = TransformRequest {
        bucket,
        key,
        action,
        password: SecretString::new(password),
    };
    cse::transform_object(&engine, &request, &Hooks::new()).await
}

/// Like `enqueue_upload`, encrypting every file (after compressing it with
/// `compression`, default none) with `password`. The password is kept in
/// memory for the tasks of this call only.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_upload_encrypted(
    state: State<'_, AppState>,
    account_id: String,
    local_paths: Vec<String>,
    bucket: String,
    prefix: String,
    password: String,
    compression: Option<Compression>,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing an encrypted upload", async {
        let mut paths = Vec::with_capacity(local_paths.len());
        for path in &local_paths {
            require_absolute("a local path", path)?;
            paths.push(PathBuf::from(path));
        }
        core.session.s3_for(&account_id).await?;
        engine::enqueue_upload_encrypted(
            &core.queue,
            &core.runner.cse_passwords(),
            SecretString::new(password),
            compression.unwrap_or_default(),
            &account_id,
            &paths,
            &bucket,
            &prefix,
        )
        .await
    })
    .await
}

/// Like `enqueue_download`, decrypting the objects that are encrypted with
/// `password` (`null`: an encrypted object fails with `password not found`;
/// other objects are saved as they are).
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_download_decrypted(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    dest_dir: String,
    password: Option<String>,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a decrypting download", async {
        require_absolute("the download folder", &dest_dir)?;
        let s3 = core.session.s3_for(&account_id).await?;
        engine::enqueue_download_decrypted(
            &core.queue,
            &core.runner.cse_passwords(),
            password.map(SecretString::new),
            &s3,
            &account_id,
            &bucket,
            &keys,
            &PathBuf::from(&dest_dir),
        )
        .await
    })
    .await
}
