//! Sync commands (docs/desktop-api.md, "Sync"; docs/sync.md).

use std::path::PathBuf;
use std::sync::Arc;

use bucketbridge_core::engine::{TransferEngine, TransferOptions};
use bucketbridge_core::model::{SyncDirection, SyncPlan};
use bucketbridge_core::sync::{
    self, AnalysisRequest, ExecuteOptions, RunSummary, SyncEvents, SyncOptions, SyncProgress,
    SyncReport,
};
use bucketbridge_core::{Result, S3Uri};
use tauri::{AppHandle, Runtime, State};

use super::{CommandResult, blocking, logged, require_absolute};
use crate::events::SyncEmitter;
use crate::state::{AppState, Core};

/// Dry run: lists the remote folder, scans the local folder, plans the rows
/// and stores the plan for `sync_execute`. Writes nothing else. `options`
/// (filters, exclusion rules and analysis options) and `source_prefix` (the
/// source of an `s3_to_s3` sync, whose `local_root` is ignored) are optional.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn sync_analyze(
    state: State<'_, AppState>,
    account_id: String,
    local_root: String,
    s3_prefix: String,
    direction: SyncDirection,
    delete: bool,
    options: Option<SyncOptions>,
    source_prefix: Option<String>,
) -> CommandResult<SyncPlan> {
    let core = state.core()?;
    logged("analyzing a sync", async {
        let request = AnalysisRequest {
            account_id,
            direction,
            local_root: PathBuf::from(&local_root),
            s3_prefix: S3Uri::parse(&s3_prefix)?,
            source_prefix: source_prefix.as_deref().map(S3Uri::parse).transpose()?,
            delete,
            options: options.unwrap_or_default(),
            cache_dir: None,
        };
        analyze_and_store(&core, request).await
    })
    .await
}

/// Analyses a request and stores the plan (shared with `run_sync_job`).
pub(super) async fn analyze_and_store(
    core: &Arc<Core>,
    request: AnalysisRequest,
) -> Result<SyncPlan> {
    if request.direction != SyncDirection::S3ToS3 {
        require_absolute("the local folder", &request.local_root.to_string_lossy())?;
    }
    request.options.validate()?;
    let request = AnalysisRequest {
        cache_dir: Some(core.dirs.data_dir.join(sync::listing::CACHE_DIR_NAME)),
        ..request
    };
    let s3 = core.session.s3_for(&request.account_id).await?;
    let plan = sync::analyze_async(&s3, &request).await?;
    let store = Arc::clone(core.sync.store());
    let plan = blocking(move || {
        store.insert(&plan)?;
        Ok(plan)
    })
    .await?;
    tracing::info!(
        target: "bucketbridge::sync",
        "analyzed {} and {}: plan {} with {} row(s)",
        plan.source_prefix
            .as_ref()
            .map_or_else(|| plan.local_root.display().to_string(), ToString::to_string),
        plan.s3_prefix,
        plan.id,
        plan.rows.len()
    );
    Ok(plan)
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
        let engine = TransferEngine::new(s3, TransferOptions::from_settings(&settings))?
            .with_upload_rule_settings(&settings.upload_rules)?;
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

/// Builds the summary report of the finished run and sends it as `sync://report`.
/// A plan that cannot be read any more has no report.
async fn send_report<R: Runtime>(
    core: &Core,
    plan_id: &str,
    emitter: &SyncEmitter<R>,
    summary: &RunSummary,
) {
    let store = Arc::clone(core.sync.store());
    let id = plan_id.to_owned();
    let Ok(plan) = blocking(move || store.get(&id)).await else {
        return;
    };
    let report = SyncReport::from_rows(
        plan_id,
        plan.direction,
        &plan.rows,
        summary.duration,
        summary.cancelled,
    );
    emitter.report(&report);
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
            send_report(core, plan_id, emitter, &summary).await;
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
