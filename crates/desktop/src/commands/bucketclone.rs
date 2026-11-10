//! Bucket cloning commands (docs/desktop-api.md, "Clone a bucket"): a preview
//! of what can be copied, and the clone itself (the new bucket with its
//! settings, then the objects through the transfer queue).

use bucketbridge_core::BbError;
use bucketbridge_core::bucketclone::{CloneOutcome, ClonePreview, CloneRequest, CloneSetting};
use bucketbridge_core::engine::enqueue_clone_copy;
use bucketbridge_core::model::TransferTask;
use bucketbridge_core::s3uri::validate_bucket_name;
use serde::Serialize;
use tauri::State;

use super::{CommandResult, logged};
use crate::state::AppState;

/// What a clone of `bucket` would copy: each setting and whether the bucket
/// has it. Nothing is changed.
#[tauri::command(rename_all = "snake_case")]
pub async fn preview_bucket_clone(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<ClonePreview> {
    let core = state.core()?;
    logged("previewing a bucket clone", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.preview_bucket_clone(&bucket).await
    })
    .await
}

/// The answer of `clone_bucket`.
#[derive(Debug, Serialize)]
pub struct CloneBucketResult {
    /// The new bucket and what became of each setting.
    outcome: CloneOutcome,
    /// The copy tasks added to the transfer queue (none without
    /// `copy_objects`, or for an empty bucket).
    tasks: Vec<TransferTask>,
    /// Why the objects could not be queued; the bucket exists anyway.
    copy_error: Option<BbError>,
}

/// Creates `new_bucket` (in `region`, else the region of `source_bucket`),
/// copies the chosen `settings`, and with `copy_objects` queues the
/// server-side copy of the objects (all of them, or those under `prefix`).
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn clone_bucket(
    state: State<'_, AppState>,
    account_id: String,
    source_bucket: String,
    new_bucket: String,
    region: Option<String>,
    prefix: String,
    copy_objects: bool,
    settings: Vec<CloneSetting>,
) -> CommandResult<CloneBucketResult> {
    let core = state.core()?;
    logged("cloning a bucket", async {
        validate_bucket_name(&source_bucket)?;
        let s3 = core.session.s3_for(&account_id).await?;
        let outcome = s3
            .create_bucket_clone(&CloneRequest {
                source_bucket: source_bucket.clone(),
                new_bucket: new_bucket.clone(),
                region,
                settings,
            })
            .await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "cloned the settings of {source_bucket} into the new bucket {new_bucket}"
        );
        let (tasks, copy_error) = if copy_objects {
            match enqueue_clone_copy(
                &core.queue,
                &s3,
                &account_id,
                &source_bucket,
                &new_bucket,
                &prefix,
            )
            .await
            {
                Ok(tasks) => (tasks, None),
                Err(error) => (Vec::new(), Some(error.to_payload())),
            }
        } else {
            (Vec::new(), None)
        };
        Ok(CloneBucketResult {
            outcome,
            tasks,
            copy_error,
        })
    })
    .await
}
