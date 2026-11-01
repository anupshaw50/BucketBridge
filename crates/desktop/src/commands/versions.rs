//! Version and preview commands (docs/desktop-api.md, "Versions and preview
//! (T08)").
//!
//! Each command resolves the account with `Session::s3_for` and calls the
//! core (`bucketbridge_core::versions` and `bucketbridge_core::preview`);
//! the transfer of one version is an ordinary download task of the queue.

use std::path::Path;

use bucketbridge_core::model::{DeleteOutcome, TransferTask};
use bucketbridge_core::ops::MAX_LIST_KEYS;
use bucketbridge_core::preview::{ObjectPreview, PreviewOptions};
use bucketbridge_core::versions::{
    RestoreOutcome, VersionFilter, VersionGroupPage, VersionQuery, VersionRef,
    enqueue_download_version as queue_version_download,
};
use tauri::State;

use super::{CommandResult, logged, require_absolute};
use crate::state::AppState;

/// One page of the versions and delete markers under `prefix` (or of the key
/// `prefix` alone with `exact`), grouped by key and filtered by `filter`. A
/// page holds `settings.advanced.list_page_size` keys at most; `token` is the
/// `next_token` of the previous page.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_versions(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    exact: bool,
    filter: VersionFilter,
    token: Option<String>,
) -> CommandResult<VersionGroupPage> {
    let core = state.core()?;
    logged("listing versions", async {
        let max_groups = core.session.settings().await?.advanced.list_page_size;
        let s3 = core.session.s3_for(&account_id).await?;
        let query = VersionQuery {
            prefix,
            exact,
            filter,
            token,
            max_groups,
            request_keys: MAX_LIST_KEYS,
        };
        s3.list_version_groups(&bucket, &query).await
    })
    .await
}

/// Copies a version over its key, which makes the copy the current version.
#[tauri::command(rename_all = "snake_case")]
pub async fn restore_version(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: String,
) -> CommandResult<RestoreOutcome> {
    let core = state.core()?;
    logged("restoring a version", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.restore_version(&bucket, &key, &version_id).await
    })
    .await
}

/// Deletes versions and delete markers for good. Versions the server refuses
/// are in the outcome's `failures`.
#[tauri::command(rename_all = "snake_case")]
pub async fn purge_versions(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    versions: Vec<VersionRef>,
) -> CommandResult<DeleteOutcome> {
    let core = state.core()?;
    logged("purging versions", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.purge_versions(&bucket, &versions).await
    })
    .await
}

/// Queues the download of one version to the file `dest_path` (absolute);
/// the result holds that one task.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_download_version(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: String,
    dest_path: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a version download", async {
        require_absolute("the download file", &dest_path)?;
        let s3 = core.session.s3_for(&account_id).await?;
        let task = queue_version_download(
            &core.queue,
            &s3,
            &account_id,
            &bucket,
            &key,
            &version_id,
            Path::new(&dest_path),
        )
        .await?;
        Ok(vec![task])
    })
    .await
}

/// One page of the preview of an object (its latest version, or
/// `version_id`): an image, text, JSON, or one of the states binary, too
/// large and empty. `page_size` is the bytes per page of text (16 to
/// 1,048,576; `null` is 131,072).
#[tauri::command(rename_all = "snake_case")]
pub async fn preview_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    version_id: Option<String>,
    page: u64,
    page_size: Option<u64>,
) -> CommandResult<ObjectPreview> {
    let core = state.core()?;
    logged("previewing an object", async {
        let options = PreviewOptions::with_page_bytes(page_size);
        let s3 = core.session.s3_for(&account_id).await?;
        s3.preview_object(&bucket, &key, version_id.as_deref(), page, &options)
            .await
    })
    .await
}
