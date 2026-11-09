//! Bucket usage commands (docs/desktop-api.md, "Bucket usage").

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use bucketbridge_core::Error;
use bucketbridge_core::usage::{CancellationToken, Usage, UsageOptions};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::{CommandResult, logged};
use crate::state::AppState;

/// `UsageProgressEvent` after every listed page and every sized upload.
pub const USAGE_PROGRESS: &str = "usage://progress";

const MAX_JOB_ID_LEN: usize = 64;

/// Payload of `usage://progress`.
#[derive(Debug, Clone, Serialize)]
struct UsageProgressEvent {
    job_id: String,
    phase: bucketbridge_core::usage::UsagePhase,
    usage: Usage,
}

fn jobs() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static JOBS: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

/// Removes the job from the registry when the command ends, however it ends.
struct JobGuard(String);

impl Drop for JobGuard {
    fn drop(&mut self) {
        jobs()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Adds up the objects of `bucket` under `prefix`; progress arrives as
/// `usage://progress`, and `cancel_bucket_usage` ends the run with `cancelled`.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn compute_bucket_usage<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    versions: bool,
    multipart: bool,
    job_id: String,
) -> CommandResult<Usage> {
    let core = state.core()?;
    logged("computing the bucket usage", async {
        if job_id.is_empty() || job_id.len() > MAX_JOB_ID_LEN {
            return Err(Error::Usage(
                "the job id must be 1 to 64 characters long".into(),
            ));
        }
        let token = CancellationToken::new();
        {
            let mut running = jobs().lock().unwrap_or_else(PoisonError::into_inner);
            if running.contains_key(&job_id) {
                return Err(Error::Conflict(format!("job {job_id} is already running")));
            }
            running.insert(job_id.clone(), token.clone());
        }
        let _guard = JobGuard(job_id.clone());
        let s3 = core.session.s3_for(&account_id).await?;
        let events_job = job_id.clone();
        s3.compute_usage(
            &bucket,
            &prefix,
            UsageOptions {
                versions,
                multipart,
            },
            Some(&token),
            move |progress| {
                let _ = app.emit(
                    USAGE_PROGRESS,
                    UsageProgressEvent {
                        job_id: events_job.clone(),
                        phase: progress.phase,
                        usage: progress.usage.clone(),
                    },
                );
            },
        )
        .await
    })
    .await
}

/// Stops a running computation; an unknown or finished job is not an error.
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_bucket_usage(job_id: String) -> CommandResult<()> {
    if let Some(token) = jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&job_id)
    {
        token.cancel();
    }
    Ok(())
}
