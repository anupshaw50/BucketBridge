//! Transfer queue commands (docs/desktop-api.md, "Transfer queue";
//! docs/transfers.md, docs/engines.md).
//!
//! The `enqueue_*` commands and `rename` expand the selection with the core's
//! enqueue helpers (folder semantics, one `group_id` per call, nothing queued
//! on a duplicate destination) and add the tasks to the queue; the queue runs
//! them, and its events reach the UI as `transfer://update` and
//! `transfer://removed` (see `events.rs`). The control commands call the
//! queue directly: it answers `not_found` for an unknown id and `conflict`
//! for a transition its state machine does not allow.

use std::path::PathBuf;

use bucketbridge_core::Result;
use bucketbridge_core::engine;
use bucketbridge_core::model::TransferTask;
use tauri::State;

use super::{CommandResult, logged, require_absolute};
use crate::state::AppState;

/// Uploads local files and folders to `prefix` in `bucket`. Folders keep
/// their relative paths and empty folders become folder markers. The paths
/// must be absolute.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_upload(
    state: State<'_, AppState>,
    account_id: String,
    local_paths: Vec<String>,
    bucket: String,
    prefix: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing an upload", async {
        let paths = absolute_paths("a local path", &local_paths)?;
        // Nothing is sent to S3 here, but a locked account, a missing MFA
        // session or an unknown account must reject the command, not the
        // tasks.
        core.session.s3_for(&account_id).await?;
        engine::enqueue_upload(&core.queue, &account_id, &paths, &bucket, &prefix).await
    })
    .await
}

/// Downloads keys and prefixes of `bucket` into `dest_dir` (absolute); the
/// parent part of every selected key is stripped.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_download(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
    dest_dir: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a download", async {
        require_absolute("the download folder", &dest_dir)?;
        let s3 = core.session.s3_for(&account_id).await?;
        engine::enqueue_download(
            &core.queue,
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

/// Copies keys and prefixes of `src_bucket` to `dst_prefix` in `dst_bucket`
/// on the server.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_copy(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    keys: Vec<String>,
    dst_bucket: String,
    dst_prefix: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a copy", async {
        let s3 = core.session.s3_for(&account_id).await?;
        engine::enqueue_copy(
            &core.queue,
            &s3,
            &account_id,
            &src_bucket,
            &keys,
            &dst_bucket,
            &dst_prefix,
        )
        .await
    })
    .await
}

/// Like `enqueue_copy`, deleting each source object once its copy exists.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_move(
    state: State<'_, AppState>,
    account_id: String,
    src_bucket: String,
    keys: Vec<String>,
    dst_bucket: String,
    dst_prefix: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a move", async {
        let s3 = core.session.s3_for(&account_id).await?;
        engine::enqueue_move(
            &core.queue,
            &s3,
            &account_id,
            &src_bucket,
            &keys,
            &dst_bucket,
            &dst_prefix,
        )
        .await
    })
    .await
}

/// Deletes keys: one task per key, and a prefix (ending in `/`) is one task
/// that deletes everything below it.
#[tauri::command(rename_all = "snake_case")]
pub async fn enqueue_delete(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    keys: Vec<String>,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a delete", async {
        // See `enqueue_upload`: the account is checked up front.
        core.session.s3_for(&account_id).await?;
        engine::enqueue_delete(&core.queue, &account_id, &bucket, &keys).await
    })
    .await
}

/// Renames an object, or a prefix with everything below it, as move tasks
/// (`conflict` when the destination exists).
#[tauri::command(rename_all = "snake_case")]
pub async fn rename(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    from_key: String,
    to_key: String,
) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    logged("queueing a rename", async {
        let s3 = core.session.s3_for(&account_id).await?;
        engine::enqueue_rename(&core.queue, &s3, &account_id, &bucket, &from_key, &to_key).await
    })
    .await
}

/// Every task in creation order, with its current state and progress.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_tasks(state: State<'_, AppState>) -> CommandResult<Vec<TransferTask>> {
    let core = state.core()?;
    Ok(core.queue.list())
}

/// Moves every `stopped` task back to `queued`.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_all(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.start_all()?)
}

/// Moves every queued task to `stopped` and asks running ones to pause.
#[tauri::command(rename_all = "snake_case")]
pub async fn stop_all(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.stop_all()?)
}

/// Cancels every task that is not `completed` or `cancelled`; each one is
/// cleaned up (no partial objects, open multipart uploads or partial files).
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_all(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.cancel_all()?)
}

/// Removes `completed` and `cancelled` tasks (`transfer://removed`). Failed
/// tasks stay until they are retried or cancelled.
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_finished(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    core.queue.clear_finished()?;
    Ok(())
}

/// Pauses a queued or running task (`conflict` in any other state).
#[tauri::command(rename_all = "snake_case")]
pub async fn pause_task(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.pause_task(&id)?)
}

/// Queues a `stopped` task again (`conflict` in any other state).
#[tauri::command(rename_all = "snake_case")]
pub async fn resume_task(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.resume_task(&id)?)
}

/// Cancels a queued, running, stopped or failed task and cleans up after it
/// (`conflict` for a completed or cancelled task).
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_task(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.cancel_task(&id)?)
}

/// Queues a `failed` task again (`conflict` in any other state).
#[tauri::command(rename_all = "snake_case")]
pub async fn retry_task(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    Ok(core.queue.retry_task(&id)?)
}

/// The paths as `PathBuf`s; `usage` unless every one is absolute (the UI
/// takes them from the native dialogs and the drag and drop of the OS).
fn absolute_paths(what: &str, paths: &[String]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            require_absolute(what, path)?;
            Ok(PathBuf::from(path))
        })
        .collect()
}
