//! Sync commands (docs/desktop-api.md, "Sync"; docs/sync.md).

use std::path::Path;
use std::sync::Arc;

use bucketbridge_core::engine::{TransferEngine, TransferOptions};
use bucketbridge_core::model::{SyncDirection, SyncPlan};
use bucketbridge_core::sync::{
    self, AnalyzeRequest, ExecuteOptions, RunSummary, SyncEvents, SyncProgress,
};
use bucketbridge_core::{Result, S3Uri};
use tauri::{AppHandle, Runtime, State};

use super::{CommandResult, blocking, logged, require_absolute};
use crate::events::SyncEmitter;
use crate::state::{AppState, Core};

/// Dry run: lists the remote folder, scans the local folder, plans the rows
/// and stores the plan for `sync_execute`. Writes nothing else.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_analyze(
    state: State<'_, AppState>,
    account_id: String,
    local_root: String,
    s3_prefix: String,
    direction: SyncDirection,
    delete: bool,
) -> CommandResult<SyncPlan> {
    let core = state.core()?;
    logged("analyzing a sync", async {
        require_absolute("the local folder", &local_root)?;
        let remote = sync::as_folder(&S3Uri::parse(&s3_prefix)?)?;
        let s3 = core.session.s3_for(&account_id).await?;
        let entries = s3.list_all(remote.bucket(), remote.key()).await?;
        let store = Arc::clone(core.sync.store());
        let plan = blocking(move || {
            let request = AnalyzeRequest {
                delete,
                ..AnalyzeRequest::new(&account_id, Path::new(&local_root), &remote, direction)
            };
            let plan = sync::analyze(&request, entries)?;
            store.insert(&plan)?;
            Ok(plan)
        })
        .await?;
        tracing::info!(
            target: "bucketbridge::sync",
            "analyzed {} and {}: plan {} with {} row(s)",
            plan.local_root.display(),
            plan.s3_prefix,
            plan.id,
            plan.rows.len()
        );
        Ok(plan)
    })
    .await
}

/// Starts executing the pending rows of a stored plan in the background and
/// returns. Progress arrives as `sync://progress` and `sync://row`, ending
/// with one `finished: true`. `conflict` while the plan is executing.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_execute<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    plan_id: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("starting a sync", async {
        let summary = {
            let store = Arc::clone(core.sync.store());
            let id = plan_id.clone();
            blocking(move || store.summary(&id)).await?
        };
        let s3 = core.session.s3_for(&summary.account_id).await?;
        let settings = core.session.settings().await?;
        let engine = TransferEngine::new(s3, TransferOptions::from_settings(&settings))?;
        let options = ExecuteOptions::new(&settings, core.limiter.clone());
        let emitter = Arc::new(SyncEmitter::new(app));
        let events: Arc<dyn SyncEvents> = emitter.clone();
        let run = core.sync.start(&plan_id, engine, options, events)?;
        let run_plan = run.plan_id().to_owned();
        let token = core.begin_run(&run_plan);
        let task_core = Arc::clone(&core);
        tauri::async_runtime::spawn(async move {
            let result = run.run().await;
            report_run(&task_core, &run_plan, &emitter, result).await;
            task_core.end_run(token);
        });
        Ok(())
    })
    .await
}

/// Records how a run ended in the event log. A run that stopped before it
/// could report `finished: true` (its plan could not be loaded) still sends
/// one, so the UI never waits for an end that does not come.
async fn report_run<R: Runtime>(
    core: &Core,
    plan_id: &str,
    emitter: &SyncEmitter<R>,
    result: Result<RunSummary>,
) {
    match result {
        Ok(summary) => {
            let progress = &summary.progress;
            if summary.cancelled {
                tracing::info!(
                    target: "bucketbridge::sync",
                    "sync plan {plan_id} cancelled: {} of {} row(s) done",
                    progress.done,
                    progress.total
                );
            } else if summary.errors > 0 {
                tracing::warn!(
                    target: "bucketbridge::sync",
                    "sync plan {plan_id} finished with {} failed row(s): {} of {} row(s) done",
                    summary.errors,
                    progress.done,
                    progress.total
                );
            } else {
                tracing::info!(
                    target: "bucketbridge::sync",
                    "sync plan {plan_id} finished: {} of {} row(s) done",
                    progress.done,
                    progress.total
                );
            }
        }
        Err(error) => {
            tracing::error!(target: "bucketbridge::sync", "sync plan {plan_id} stopped: {error}");
            if !emitter.finished() {
                let store = Arc::clone(core.sync.store());
                let id = plan_id.to_owned();
                let progress = match blocking(move || store.counts(&id)).await {
                    Ok(counts) => SyncProgress::from_counts(plan_id, counts, true),
                    Err(_) => SyncProgress {
                        plan_id: plan_id.to_owned(),
                        total: 0,
                        done: 0,
                        failed: 0,
                        finished: true,
                    },
                };
                emitter.progress(&progress);
            }
        }
    }
}

/// Stops a running plan; its unfinished rows stay pending. A plan that is
/// not running is left alone; an unknown plan is `not_found`.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_cancel(state: State<'_, AppState>, plan_id: String) -> CommandResult<()> {
    let core = state.core()?;
    logged("cancelling a sync", async {
        if !core.sync.cancel(&plan_id)? {
            let store = Arc::clone(core.sync.store());
            blocking(move || store.summary(&plan_id)).await?;
        }
        Ok(())
    })
    .await
}

/// The stored plan with the current state of every row.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_get(state: State<'_, AppState>, plan_id: String) -> CommandResult<SyncPlan> {
    let core = state.core()?;
    logged("reading a sync plan", async {
        let store = Arc::clone(core.sync.store());
        blocking(move || store.get(&plan_id)).await
    })
    .await
}
