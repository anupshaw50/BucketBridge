//! Saved sync job commands (docs/desktop-api.md, "Sync jobs and sync options
//! (v0.2.0)"). Listing, saving and deleting read and write `config.toml` only, so
//! they work while the secrets are locked. Every change emits `sync_jobs://changed`
//! with the stored list, to every window. `run_sync_job` analyses a job into a stored
//! plan, which the UI shows for review before `sync_execute`.

use std::path::PathBuf;

use bucketbridge_core::S3Uri;
use bucketbridge_core::model::{SyncDirection, SyncPlan};
use bucketbridge_core::sync::AnalysisRequest;
use bucketbridge_core::syncjobs::{self, SyncJob};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::sync::analyze_and_store;
use super::{CommandResult, blocking, logged};
use crate::events::SYNC_JOBS_CHANGED;
use crate::state::AppState;

/// Tells every window about the new list. An emit only fails while the app
/// shuts down.
fn announce<R: Runtime, T: Serialize + Clone>(app: &AppHandle<R>, list: &T) {
    let _ = app.emit(SYNC_JOBS_CHANGED, list.clone());
}

/// The saved sync jobs, in the order they were created.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_sync_jobs(state: State<'_, AppState>) -> CommandResult<Vec<SyncJob>> {
    let core = state.core()?;
    logged("listing the sync jobs", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || syncjobs::list(&store)).await
    })
    .await
}

/// Creates a job (empty `id`) or replaces the job with that `id`; returns the job as
/// stored. `usage` for an invalid job, `not_found` for an unknown account or id,
/// `conflict` for a name another job uses.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_sync_job<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    job: SyncJob,
) -> CommandResult<SyncJob> {
    let core = state.core()?;
    let (saved, list) = logged("saving a sync job", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || {
            let saved = syncjobs::save(&store, job)?;
            let list = syncjobs::list(&store)?;
            Ok((saved, list))
        })
        .await
    })
    .await?;
    announce(&app, &list);
    Ok(saved)
}

/// Deletes a job and returns the stored list; `not_found` for an unknown id.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_sync_job<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Vec<SyncJob>> {
    let core = state.core()?;
    let list = logged("deleting a sync job", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || syncjobs::delete(&store, &id)).await
    })
    .await?;
    announce(&app, &list);
    Ok(list)
}

/// Analyses the job (like `sync_analyze` with the job's settings) and returns the
/// stored plan. Nothing is copied or deleted until `sync_execute` runs the plan.
#[tauri::command(rename_all = "snake_case")]
pub async fn run_sync_job(state: State<'_, AppState>, id: String) -> CommandResult<SyncPlan> {
    let core = state.core()?;
    logged("running a sync job", async {
        let job = {
            let store = core.session.accounts().config_store().clone();
            blocking(move || syncjobs::find(&store, &id)).await?
        };
        let source_prefix = job.s3_source.as_deref().map(S3Uri::parse).transpose()?;
        let request = AnalysisRequest {
            account_id: job.account_id,
            direction: job.direction,
            local_root: if job.direction == SyncDirection::S3ToS3 {
                PathBuf::new()
            } else {
                PathBuf::from(job.local_root)
            },
            s3_prefix: S3Uri::parse(&job.s3_prefix)?,
            source_prefix,
            delete: job.delete,
            options: job.options,
            cache_dir: None,
        };
        analyze_and_store(&core, request).await
    })
    .await
}
