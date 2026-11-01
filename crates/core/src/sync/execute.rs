//! Executing stored sync plans (task T10, execution). See docs/sync.md, "Executing a
//! plan".
//!
//! [`SyncExecutor`] runs the pending rows of a plan kept in a [`SyncStore`] with the
//! transfer engines, records every result in the store (so a crash, a cancel or an
//! error loses at most the rows in flight) and reports through [`SyncEvents`]:
//!
//! - Only pending rows run: `copy` and `delete` rows that are not done, including rows
//!   whose last attempt failed. `equal` and `failed` rows are never touched.
//! - A failing row never stops the others. Its error is recorded, it keeps its action
//!   and stays not done, so running the plan again retries it; success clears the error.
//! - Local deletions run first; afterwards the folders they emptied are removed (never
//!   the sync root). Then copies and remote deletions run concurrently, at most
//!   [`ExecuteOptions::concurrency`] rows at a time, through the shared bandwidth
//!   limiter.
//! - Downloads use the download engine: data goes to a `.bbpart` file that is renamed
//!   into place, and the file gets the object's `Last-Modified` as modification time.
//!   Nothing is read, written or deleted through a folder that has become a symbolic
//!   link since the analysis.
//! - Bidirectional uploads: afterwards the local file's modification time is set to the
//!   new object's `Last-Modified` (from a HEAD request), unless the file changed during
//!   the upload or is a link, so that the next analysis sees the pair as equal. One-way
//!   uploads do the same only for a file dated later than the object (clock skew).
//! - A delete only removes what the analysis saw: an object or file that changed or
//!   appeared since is kept and the row fails, and one that is already gone counts as
//!   deleted. A copy never overwrites a target the analysis did not see (a local entry or
//!   an object that appeared since), and in bidirectional mode it also requires its
//!   target to be unchanged. Downloads are staged in a temporary file and checked again
//!   right before they are moved into place; uploads carry an S3 conditional write
//!   (`If-None-Match: *` or `If-Match`) where the server supports it.
//! - A failed or cancelled row leaves no partial file and no open multipart upload.
//! - [`SyncExecutor::cancel`] stops promptly: transfers in flight are aborted and
//!   cleaned up (no partial objects or files) and their rows stay pending, so the next
//!   run resumes the plan.
//! - At most one execution per plan at a time: starting a plan that runs is a conflict.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use uuid::Uuid;

use super::plan::DEFAULT_TOLERANCE;
use super::scan::system_time_to_utc;
use super::store::{RowOutcome, RowUpdate, SyncStore, parse_plan_id};
use super::{RowCounts, local_path_from_segments, row_counts, row_is_pending, row_local_path};
use crate::engine::{
    FileSink, Hooks, Outcome, StopSignal, Throttle, ThrottleFuture, TransferEngine, UploadOutcome,
    UploadState, WriteCondition,
};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{
    FileMeta, ObjectEntry, ObjectHead, Side, SyncAction, SyncDirection, SyncPlan, SyncRow,
};
use crate::path::join_key;
use crate::s3uri::S3Uri;
use crate::settings::Settings;
use crate::transfer::BandwidthLimiter;

/// Results recorded in the store with one write, at most.
const BATCH_SIZE: usize = 64;
/// Longest time a finished row waits before its result is recorded and reported.
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);
/// Local deletions handled per trip to the blocking thread pool.
const LOCAL_DELETE_CHUNK: usize = 256;
/// Error text the store uses for an empty message.
const UNKNOWN_ERROR: &str = "unknown error";
/// A one-way upload looks at a file's modification time after the upload only when the
/// file is dated within this long before the upload or later (HTTP requests are refused
/// when the clocks differ by more than 15 minutes, so older files cannot be ahead of the
/// server's clock).
const SKEW_WINDOW: Duration = Duration::from_secs(30 * 60);

fn conflict(message: impl Into<String>) -> Error {
    Error::Conflict(message.into())
}

fn internal(message: impl Into<String>) -> Error {
    Error::Internal(message.into())
}

/// Progress of a plan: `SyncProgress` in docs/desktop-api.md, always over the whole
/// plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncProgress {
    /// The plan.
    pub plan_id: String,
    /// `copy` and `delete` rows.
    pub total: usize,
    /// Those of them that are done.
    pub done: usize,
    /// `failed` rows plus rows that are not done and have an error.
    pub failed: usize,
    /// True on the last report of a run, sent exactly once per run.
    pub finished: bool,
}

impl SyncProgress {
    /// Progress of `plan_id` with `counts`.
    pub fn from_counts(plan_id: &str, counts: RowCounts, finished: bool) -> Self {
        SyncProgress {
            plan_id: plan_id.to_owned(),
            total: counts.total,
            done: counts.done,
            failed: counts.failed,
            finished,
        }
    }
}

/// Receives the events of an execution; the desktop host forwards them as
/// `sync://progress` and `sync://row`. Called from the task that drives the run:
/// implementations must return quickly and never block.
pub trait SyncEvents: Send + Sync {
    /// Progress over the whole plan: when the run starts, after every batch of recorded
    /// results, and once with `finished: true` when the run ends (completed, with
    /// failures, cancelled, or stopped by an error after the plan was loaded).
    fn progress(&self, progress: &SyncProgress);

    /// Row `index` of the plan finished (`done: true`) or failed (`error` set). `row`
    /// is its new state, already recorded in the store.
    fn row(&self, plan_id: &str, index: usize, row: &SyncRow);
}

/// [`SyncEvents`] that ignores everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSyncEvents;

impl SyncEvents for NoSyncEvents {
    fn progress(&self, _progress: &SyncProgress) {}

    fn row(&self, _plan_id: &str, _index: usize, _row: &SyncRow) {}
}

/// Settings of an execution.
#[derive(Debug, Clone)]
pub struct ExecuteOptions {
    /// Rows transferred at the same time; 0 counts as 1.
    pub concurrency: usize,
    /// Bandwidth limiter. Pass the transfer queue's limiter so that one cap covers the
    /// queue and the sync together.
    pub limiter: BandwidthLimiter,
}

impl ExecuteOptions {
    /// `settings.transfer.max_concurrent_tasks` rows at a time, through `limiter`.
    pub fn new(settings: &Settings, limiter: BandwidthLimiter) -> Self {
        ExecuteOptions {
            concurrency: usize::try_from(settings.transfer.max_concurrent_tasks)
                .unwrap_or(usize::MAX),
            limiter,
        }
    }

    /// Like [`ExecuteOptions::new`], with a limiter of its own at
    /// `settings.transfer.bandwidth_limit_kibps` (for a process without a transfer
    /// queue, such as the CLI).
    pub fn standalone(settings: &Settings) -> Self {
        let rate = settings
            .transfer
            .bandwidth_limit_kibps
            .and_then(|kibps| NonZeroU64::new(kibps.saturating_mul(1024)));
        Self::new(settings, BandwidthLimiter::new(rate))
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// Progress after the run (`finished: true`).
    pub progress: SyncProgress,
    /// Rows executed by this run (done or failed).
    pub attempted: usize,
    /// Of those, the rows that failed.
    pub errors: usize,
    /// True when the run was cancelled; rows it did not finish stay pending.
    pub cancelled: bool,
}

/// Runs stored plans, at most one execution per plan at a time. Cheap to clone; the
/// clones share the store and the set of running plans.
#[derive(Debug, Clone)]
pub struct SyncExecutor {
    store: Arc<SyncStore>,
    running: Arc<Mutex<HashMap<Uuid, StopSignal>>>,
}

impl SyncExecutor {
    /// An executor for the plans in `store`.
    pub fn new(store: Arc<SyncStore>) -> Self {
        SyncExecutor {
            store,
            running: Arc::default(),
        }
    }

    /// The plan store.
    pub fn store(&self) -> &Arc<SyncStore> {
        &self.store
    }

    /// Claims `plan_id` for one run and returns it; start it with [`SyncRun::run`]
    /// (for example `tokio::spawn(run.run())`). `engine` must use the plan's account
    /// (see [`SyncStore::summary`]). Errors: `usage` for an invalid id, `conflict` while
    /// the plan is executing. A missing plan is reported by [`SyncRun::run`].
    pub fn start(
        &self,
        plan_id: &str,
        engine: TransferEngine,
        options: ExecuteOptions,
        events: Arc<dyn SyncEvents>,
    ) -> Result<SyncRun> {
        let key = parse_plan_id(plan_id)?;
        let stop = StopSignal::new();
        {
            let mut running = lock(&self.running);
            if running.contains_key(&key) {
                return Err(conflict(format!("sync plan {key} is already executing")));
            }
            running.insert(key, stop.clone());
        }
        Ok(SyncRun {
            plan_id: key.to_string(),
            key,
            store: Arc::clone(&self.store),
            running: Arc::clone(&self.running),
            engine,
            options,
            events,
            stop,
        })
    }

    /// Stops the run of `plan_id`. Returns whether one was running. The run ends
    /// promptly with `cancelled: true`; the plan stays resumable.
    pub fn cancel(&self, plan_id: &str) -> Result<bool> {
        let key = parse_plan_id(plan_id)?;
        Ok(match lock(&self.running).get(&key) {
            Some(stop) => {
                stop.cancel();
                true
            }
            None => false,
        })
    }

    /// Whether `plan_id` is executing.
    pub fn is_running(&self, plan_id: &str) -> Result<bool> {
        let key = parse_plan_id(plan_id)?;
        Ok(lock(&self.running).contains_key(&key))
    }
}

/// A claimed run (see [`SyncExecutor::start`]). The claim ends when the run ends or the
/// value is dropped. Prefer [`SyncExecutor::cancel`] over dropping a running future:
/// cancel lets the engines clean up.
pub struct SyncRun {
    plan_id: String,
    key: Uuid,
    store: Arc<SyncStore>,
    running: Arc<Mutex<HashMap<Uuid, StopSignal>>>,
    engine: TransferEngine,
    options: ExecuteOptions,
    events: Arc<dyn SyncEvents>,
    stop: StopSignal,
}

impl fmt::Debug for SyncRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncRun")
            .field("plan_id", &self.plan_id)
            .field("options", &self.options)
            .field("stopped", &self.stop.is_stopped())
            .finish_non_exhaustive()
    }
}

impl Drop for SyncRun {
    fn drop(&mut self) {
        lock(&self.running).remove(&self.key);
    }
}

impl SyncRun {
    /// The plan id (lowercase, hyphenated).
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// Executes the pending rows of the plan (see the [module documentation](self)).
    ///
    /// Errors: `not_found` and `io` when the plan cannot be loaded; afterwards only
    /// failures of the plan store end a run with an error (the rows recorded so far
    /// stay recorded). Row failures are not errors of the run: they are recorded in the
    /// rows and counted in [`RunSummary::errors`].
    pub async fn run(self) -> Result<RunSummary> {
        let plan = {
            let store = Arc::clone(&self.store);
            let plan_id = self.plan_id.clone();
            blocking(move || store.get(&plan_id)).await?
        };
        let context = Arc::new(RowContext::new(&plan, &self));
        let mut state = RunState::new(self.plan_id.clone(), plan.rows);
        self.events.progress(&state.progress(false));

        let executed = self.execute(&context, &mut state).await;
        let flushed = self.flush(&mut state).await;
        let compacted = {
            let store = Arc::clone(&self.store);
            let plan_id = self.plan_id.clone();
            blocking(move || store.compact(&plan_id)).await
        };
        let summary = RunSummary {
            progress: state.progress(true),
            attempted: state.attempted,
            errors: state.errors,
            cancelled: self.stop.is_stopped(),
        };
        self.events.progress(&summary.progress);
        executed?;
        flushed?;
        compacted?;
        Ok(summary)
    }

    async fn execute(&self, context: &Arc<RowContext>, state: &mut RunState) -> Result<()> {
        self.delete_local(context, state).await?;
        if self.stop.is_stopped() {
            return Ok(());
        }
        let deleted = state.deleted_by_this_run();
        let root = context.local_root.clone();
        blocking(move || {
            remove_emptied_folders(&root, &deleted);
            Ok(())
        })
        .await?;
        self.transfer(context, state).await
    }

    /// Phase 1: local deletions, on the blocking pool, before any copy to local.
    async fn delete_local(&self, context: &Arc<RowContext>, state: &mut RunState) -> Result<()> {
        let indices = state.pending(is_local_delete);
        for chunk in indices.chunks(LOCAL_DELETE_CHUNK) {
            if self.stop.is_stopped() {
                return Ok(());
            }
            let rows: Vec<(usize, SyncRow)> = chunk
                .iter()
                .map(|&index| (index, state.rows[index].clone()))
                .collect();
            let context = Arc::clone(context);
            let stop = self.stop.clone();
            let results = blocking(move || {
                Ok(rows
                    .into_iter()
                    .map_while(|(index, row)| {
                        (!stop.is_stopped())
                            .then(|| (index, delete_local_row(&context.local_root, &row)))
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
            for (index, result) in results {
                state.record(index, result);
            }
            self.flush(state).await?;
        }
        Ok(())
    }

    /// Phase 2: copies and remote deletions, concurrently.
    async fn transfer(&self, context: &Arc<RowContext>, state: &mut RunState) -> Result<()> {
        let mut queue = state.pending(|row| !is_local_delete(row)).into_iter();
        let concurrency = self.options.concurrency.max(1);
        let mut tasks = JoinSet::new();
        let mut in_flight: HashMap<tokio::task::Id, usize> = HashMap::new();
        loop {
            while tasks.len() < concurrency && !self.stop.is_stopped() {
                let Some(index) = queue.next() else {
                    break;
                };
                let row = state.rows[index].clone();
                let context = Arc::clone(context);
                let handle = tasks.spawn(async move { execute_row(&context, &row).await });
                in_flight.insert(handle.id(), index);
            }
            if tasks.is_empty() {
                return Ok(());
            }
            let joined = match state.flush_delay() {
                Some(delay) => tokio::select! {
                    joined = tasks.join_next_with_id() => joined,
                    () = tokio::time::sleep(delay) => {
                        self.flush_or_stop(state, &mut tasks).await?;
                        continue;
                    }
                },
                None => tasks.join_next_with_id().await,
            };
            match joined {
                Some(Ok((id, result))) => {
                    if let Some(index) = in_flight.remove(&id) {
                        state.record(index, result);
                    }
                }
                Some(Err(err)) => {
                    if let Some(index) = in_flight.remove(&err.id()) {
                        state.record(
                            index,
                            RowResult::Failed(format!(
                                "internal error while executing the row: {err}"
                            )),
                        );
                    }
                }
                None => return Ok(()),
            }
            if state.flush_due() {
                self.flush_or_stop(state, &mut tasks).await?;
            }
        }
    }

    /// Records the pending results; when the store fails, stops the run (in-flight
    /// transfers are cancelled and cleaned up) before returning the error.
    async fn flush_or_stop(
        &self,
        state: &mut RunState,
        tasks: &mut JoinSet<RowResult>,
    ) -> Result<()> {
        if let Err(err) = self.flush(state).await {
            self.stop.cancel();
            while tasks.join_next().await.is_some() {}
            return Err(err);
        }
        Ok(())
    }

    /// Writes the batch to the store, then reports the rows and the progress.
    async fn flush(&self, state: &mut RunState) -> Result<()> {
        if state.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut state.batch);
        state.batch_since = None;
        let indices: Vec<usize> = batch.iter().map(|update| update.index).collect();
        let store = Arc::clone(&self.store);
        let plan_id = self.plan_id.clone();
        blocking(move || store.update_rows(&plan_id, &batch)).await?;
        for index in indices {
            self.events.row(&self.plan_id, index, &state.rows[index]);
        }
        self.events.progress(&state.progress(false));
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| internal(format!("a sync worker thread failed: {err}")))?
}

fn is_local_delete(row: &SyncRow) -> bool {
    row.action == SyncAction::Delete && row.target == Some(Side::Local)
}

/// The in-memory rows of a run and the results not yet written to the store.
struct RunState {
    plan_id: String,
    rows: Vec<SyncRow>,
    batch: Vec<RowUpdate>,
    batch_since: Option<Instant>,
    attempted: usize,
    errors: usize,
    /// Local files deleted by this run (not by an earlier one).
    local_deletes_done: Vec<usize>,
}

impl RunState {
    fn new(plan_id: String, rows: Vec<SyncRow>) -> Self {
        RunState {
            plan_id,
            rows,
            batch: Vec::new(),
            batch_since: None,
            attempted: 0,
            errors: 0,
            local_deletes_done: Vec::new(),
        }
    }

    fn progress(&self, finished: bool) -> SyncProgress {
        SyncProgress::from_counts(&self.plan_id, row_counts(&self.rows), finished)
    }

    fn pending(&self, wanted: impl Fn(&SyncRow) -> bool) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row_is_pending(row) && wanted(row))
            .map(|(index, _)| index)
            .collect()
    }

    fn record(&mut self, index: usize, result: RowResult) {
        let outcome = match result {
            RowResult::Done => RowOutcome::Done,
            RowResult::Failed(message) if message.trim().is_empty() => {
                RowOutcome::Error(UNKNOWN_ERROR.to_owned())
            }
            RowResult::Failed(message) => RowOutcome::Error(message),
            RowResult::Stopped => return,
        };
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        self.attempted += 1;
        match &outcome {
            RowOutcome::Done => {
                row.done = true;
                row.error = None;
                if row.action == SyncAction::Delete && row.target == Some(Side::Local) {
                    self.local_deletes_done.push(index);
                }
            }
            RowOutcome::Error(message) => {
                self.errors += 1;
                row.done = false;
                row.error = Some(message.clone());
            }
        }
        self.batch.push(RowUpdate { index, outcome });
        self.batch_since.get_or_insert_with(Instant::now);
    }

    fn flush_due(&self) -> bool {
        self.batch.len() >= BATCH_SIZE
            || self
                .batch_since
                .is_some_and(|since| since.elapsed() >= FLUSH_INTERVAL)
    }

    /// How long until the pending batch is due, if there is one.
    fn flush_delay(&self) -> Option<Duration> {
        self.batch_since
            .map(|since| FLUSH_INTERVAL.saturating_sub(since.elapsed()))
    }

    /// Paths of the local files this run deleted (rows done by an earlier run do not
    /// count: the folders around them may have been used again since).
    fn deleted_by_this_run(&self) -> Vec<String> {
        self.local_deletes_done
            .iter()
            .filter_map(|&index| self.rows.get(index))
            .map(|row| row.rel_path.clone())
            .collect()
    }
}

/// Result of executing one row.
enum RowResult {
    Done,
    Failed(String),
    /// The run was stopped before the row finished; it stays pending.
    Stopped,
}

/// What every row of a run needs.
struct RowContext {
    engine: TransferEngine,
    local_root: PathBuf,
    bucket: String,
    key_prefix: String,
    direction: SyncDirection,
    hooks: Hooks,
}

impl RowContext {
    fn new(plan: &SyncPlan, run: &SyncRun) -> Self {
        let throttle: Arc<dyn Throttle> = Arc::new(LimiterThrottle(run.options.limiter.clone()));
        RowContext {
            engine: run.engine.clone(),
            local_root: plan.local_root.clone(),
            bucket: plan.s3_prefix.bucket().to_owned(),
            key_prefix: plan.s3_prefix.key().to_owned(),
            direction: plan.direction,
            hooks: Hooks::new()
                .with_throttle(throttle)
                .with_stop(run.stop.clone()),
        }
    }

    fn uri(&self, rel_path: &str) -> std::result::Result<S3Uri, String> {
        S3Uri::new(&self.bucket, join_key(&self.key_prefix, rel_path))
            .map_err(|err| err.to_string())
    }

    fn bidirectional(&self) -> bool {
        self.direction == SyncDirection::Bidirectional
    }
}

/// The transfer queue's [`BandwidthLimiter`] as the engines' [`Throttle`].
struct LimiterThrottle(BandwidthLimiter);

impl Throttle for LimiterThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(self.0.acquire(bytes))
    }

    /// At most one second's worth of the current rate, as the engines ask.
    fn max_grant(&self) -> u64 {
        self.0.rate().map_or(u64::MAX, NonZeroU64::get)
    }
}

async fn execute_row(context: &RowContext, row: &SyncRow) -> RowResult {
    match (row.action, row.target) {
        (SyncAction::Copy, Some(Side::Remote)) => upload_row(context, row).await,
        (SyncAction::Copy, Some(Side::Local)) => download_row(context, row).await,
        (SyncAction::Delete, Some(Side::Remote)) => delete_remote_row(context, row).await,
        _ => RowResult::Failed(format!(
            "row '{}' cannot be executed by this phase",
            row.rel_path
        )),
    }
}

async fn upload_row(context: &RowContext, row: &SyncRow) -> RowResult {
    let path = match local_path(&context.local_root, row) {
        Ok(path) => path,
        Err(message) => return RowResult::Failed(message),
    };
    let target = match context.uri(&row.rel_path) {
        Ok(target) => target,
        Err(message) => return RowResult::Failed(message),
    };
    let guard = RemoteGuard::new(context.bidirectional(), row.s3_meta.as_ref());
    let condition = match check_remote_target(context, &target, &path, &guard).await {
        Ok(RemoteCheck::Proceed(condition)) => condition,
        Ok(RemoteCheck::AlreadyDone) => return RowResult::Done,
        Err(message) => return RowResult::Failed(message),
    };
    // The file's time before the upload, to give it the object's time afterwards.
    let before = {
        let file = path.clone();
        let bidirectional = context.bidirectional();
        tokio::task::spawn_blocking(move || {
            stamp(&file).filter(|stamp| bidirectional || is_recent(stamp.modified))
        })
        .await
        .ok()
        .flatten()
    };

    let saved: Mutex<Option<UploadState>> = Mutex::new(None);
    let mut result = upload_once(context, &path, &target, condition.as_ref(), &saved, None).await;
    if condition.is_some()
        && matches!(&result, Err(err) if err.service().is_some_and(|service| service.is_not_implemented()))
    {
        // The server does not know conditional writes; the HEAD request above stood in
        // for it. A multipart upload continues with the parts it already has.
        let resume = lock(&saved).take();
        result = upload_once(context, &path, &target, None, &saved, resume).await;
    }
    match result {
        Ok(Outcome::Completed(_)) => {}
        Ok(Outcome::Paused | Outcome::Cancelled) => {
            let _ = discard_upload(context, &saved).await;
            return RowResult::Stopped;
        }
        Err(err) => {
            let leftover = discard_upload(context, &saved).await;
            if err.kind() == ErrorKind::Cancelled {
                return RowResult::Stopped;
            }
            if is_precondition_failed(&err) {
                // Our own earlier attempt may be what the server found there.
                if remote_matches_source(context, &target, &path).await {
                    return RowResult::Done;
                }
                return RowResult::Failed(
                    "not uploaded: the object in S3 changed since the analysis; analyze again"
                        .to_owned(),
                );
            }
            return RowResult::Failed(with_leftover(err.to_string(), leftover));
        }
    }
    if let Some(before) = before {
        adopt_remote_time(context, &target, path, before, !context.bidirectional()).await;
    }
    RowResult::Done
}

/// One upload attempt; the checkpoints of a multipart upload are kept in `saved`.
async fn upload_once(
    context: &RowContext,
    path: &Path,
    target: &S3Uri,
    condition: Option<&WriteCondition>,
    saved: &Mutex<Option<UploadState>>,
    resume: Option<UploadState>,
) -> Result<Outcome<UploadOutcome>> {
    let engine = match condition {
        Some(condition) => context.engine.with_write_condition(condition.clone()),
        None => context.engine.clone(),
    };
    let remember = |state: &UploadState| *lock(saved) = Some(state.clone());
    engine
        .upload_file(path, target, &context.hooks, resume, &remember)
        .await
}

/// Aborts the multipart upload an attempt left behind. Returns why that failed, if it did.
async fn discard_upload(
    context: &RowContext,
    saved: &Mutex<Option<UploadState>>,
) -> Option<String> {
    let state = lock(saved).take()?;
    context
        .engine
        .discard_upload(&state)
        .await
        .err()
        .map(|err| format!("the incomplete multipart upload could not be aborted: {err}"))
}

fn with_leftover(message: String, leftover: Option<String>) -> String {
    match leftover {
        Some(leftover) => format!("{message}; {leftover}"),
        None => message,
    }
}

/// The server refused a conditional write because the object is not what was expected.
fn is_precondition_failed(err: &Error) -> bool {
    err.service().is_some_and(|service| {
        matches!(service.status(), Some(412))
            || matches!(
                service.code(),
                Some("PreconditionFailed" | "ConditionalRequestConflict")
            )
    })
}

async fn download_row(context: &RowContext, row: &SyncRow) -> RowResult {
    let dest = match local_path(&context.local_root, row) {
        Ok(dest) => dest,
        Err(message) => return RowResult::Failed(message),
    };
    let source = match context.uri(&row.rel_path) {
        Ok(source) => source,
        Err(message) => return RowResult::Failed(message),
    };
    let guard = LocalGuard::new(context.bidirectional(), row.local_meta.as_ref());
    let checked = {
        let dest = dest.clone();
        let guard = guard.clone();
        let object = row.s3_meta.clone();
        let rel_path = row.rel_path.clone();
        tokio::task::spawn_blocking(move || {
            check_local_target(&dest, &rel_path, &guard, object.as_ref())
        })
        .await
        .unwrap_or_else(|err| Err(format!("internal error: {err}")))
    };
    match checked {
        Ok(LocalCheck::Proceed) => {}
        Ok(LocalCheck::AlreadyDone) => return RowResult::Done,
        Err(message) => return RowResult::Failed(message),
    }
    // The data goes to a file of its own, so that the target is checked again right
    // before the download replaces it, however long the transfer took. The staging
    // file is the engine's partial file itself (its name ends in ".bbpart", so scans
    // skip it and the engine opens it without ever following a link).
    let staging = staging_path(&dest);
    let sink = {
        let staging = staging.clone();
        let stop = context.hooks.stop.token().clone();
        tokio::task::spawn_blocking(move || {
            FileSink::open(&staging).map(|sink| sink.with_stop(stop))
        })
        .await
        .unwrap_or_else(|err| Err(Error::Internal(format!("internal error: {err}"))))
    };
    let sink = match sink {
        Ok(sink) => sink,
        Err(err) => {
            discard_staging(context, &staging).await;
            return RowResult::Failed(err.to_string());
        }
    };
    match context
        .engine
        .download_to(&source, sink, &context.hooks, None, &|_| {})
        .await
    {
        Ok((Outcome::Completed(done), sink)) => {
            let modified = done.last_modified;
            let installed = tokio::task::spawn_blocking(move || {
                let staging = sink.seal(modified).map_err(|err| err.to_string())?;
                install_download(&staging, &dest, &guard)
            })
            .await
            .unwrap_or_else(|err| Err(format!("internal error: {err}")));
            match installed {
                Ok(()) => RowResult::Done,
                Err(message) => {
                    discard_staging(context, &staging).await;
                    RowResult::Failed(message)
                }
            }
        }
        Ok((Outcome::Paused | Outcome::Cancelled, sink)) => {
            drop(sink);
            discard_staging(context, &staging).await;
            RowResult::Stopped
        }
        Err(err) => {
            discard_staging(context, &staging).await;
            if err.kind() == ErrorKind::Cancelled {
                RowResult::Stopped
            } else {
                RowResult::Failed(err.to_string())
            }
        }
    }
}

/// Removes what a download that did not finish left next to its target.
async fn discard_staging(context: &RowContext, staging: &Path) {
    let _ = context.engine.discard_download(staging).await;
    let staging = staging.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || fs::remove_file(staging)).await;
}

/// A new, unpredictable name next to `dest` for the download to be written under. It ends
/// in `.bbpart`, so scans skip it, and keeps the name short enough for any file system.
fn staging_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut short = String::new();
    for c in name.chars() {
        if short.len() + c.len_utf8() > 64 {
            break;
        }
        short.push(c);
    }
    let unique = Uuid::new_v4().simple().to_string();
    dest.with_file_name(format!(".{short}.{}.bbpart", &unique[..12]))
}

async fn delete_remote_row(context: &RowContext, row: &SyncRow) -> RowResult {
    let target = match context.uri(&row.rel_path) {
        Ok(target) => target,
        Err(message) => return RowResult::Failed(message),
    };
    match context
        .engine
        .s3()
        .head_object(target.bucket(), target.key(), None)
        .await
    {
        Ok(head) => {
            if !row
                .s3_meta
                .as_ref()
                .is_some_and(|planned| same_object(planned, &head))
            {
                return RowResult::Failed(
                    "not deleted: the object in S3 changed since the analysis; analyze again"
                        .to_owned(),
                );
            }
        }
        // Already gone: that is what the row asked for.
        Err(err) if err.kind() == ErrorKind::NotFound => return RowResult::Done,
        Err(err) => return RowResult::Failed(err.to_string()),
    }
    match context.engine.delete(&target, &context.hooks).await {
        Ok(Outcome::Completed(_)) => RowResult::Done,
        Ok(Outcome::Paused | Outcome::Cancelled) => RowResult::Stopped,
        Err(err) if err.kind() == ErrorKind::NotFound => RowResult::Done,
        Err(err) => RowResult::Failed(err.to_string()),
    }
}

/// Runs on the blocking pool.
fn delete_local_row(local_root: &Path, row: &SyncRow) -> RowResult {
    let path = match local_path(local_root, row) {
        Ok(path) => path,
        Err(message) => return RowResult::Failed(message),
    };
    let entry = match fs::symlink_metadata(&path) {
        Ok(entry) => entry,
        // Already gone: that is what the row asked for.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return RowResult::Done,
        Err(err) => return RowResult::Failed(format!("cannot read {}: {err}", path.display())),
    };
    if entry.is_dir() {
        return RowResult::Failed("not deleted: the path is a folder now".to_owned());
    }
    // A link to a file was scanned as its target; compare the target, delete the link.
    let unchanged = match fs::metadata(&path) {
        Ok(meta) => row
            .local_meta
            .as_ref()
            .is_some_and(|planned| same_file(planned, &meta)),
        Err(_) => false,
    };
    if !unchanged {
        return RowResult::Failed(
            "not deleted: the local file changed since the analysis; analyze again".to_owned(),
        );
    }
    match fs::remove_file(&path) {
        Ok(()) => RowResult::Done,
        Err(err) if err.kind() == io::ErrorKind::NotFound => RowResult::Done,
        Err(err) => RowResult::Failed(format!("cannot delete {}: {err}", path.display())),
    }
}

/// The row's local path, refusing a path whose folders lead through a symbolic link.
fn local_path(local_root: &Path, row: &SyncRow) -> std::result::Result<PathBuf, String> {
    let path = row_local_path(local_root, row).map_err(|err| err.to_string())?;
    check_folders(local_root, &row.rel_path)?;
    Ok(path)
}

/// Fails when a folder between `root` and the file `rel_path` is a symbolic link (or a
/// junction) or not a folder, so that nothing is read, written or deleted outside
/// `root`. Folders that do not exist yet are fine: they are created as real folders.
fn check_folders(root: &Path, rel_path: &str) -> std::result::Result<(), String> {
    let Some((parents, _)) = rel_path.rsplit_once('/') else {
        return Ok(());
    };
    let mut folder = root.to_path_buf();
    for segment in parents.split('/') {
        folder.push(segment);
        match fs::symlink_metadata(&folder) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "'{}' is a symbolic link; sync never works through links to folders",
                    folder.display()
                ));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("'{}' is a file, not a folder", folder.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(format!("cannot read {}: {err}", folder.display())),
        }
    }
    Ok(())
}

/// Whether a local file still has the size and modification time the analysis saw.
fn same_file(planned: &FileMeta, meta: &Metadata) -> bool {
    meta.len() == planned.size
        && planned.modified.is_none_or(|modified| {
            meta.modified().ok().and_then(system_time_to_utc) == Some(modified)
        })
}

/// Whether an object still is the one the listing showed: same size, and same ETag and
/// `Last-Modified` (to the second, HEAD answers have no fractions) where both are known.
fn same_object(planned: &ObjectEntry, current: &ObjectHead) -> bool {
    planned.size == current.size
        && match (planned.last_modified, current.last_modified) {
            (Some(planned), Some(current)) => planned.timestamp() == current.timestamp(),
            _ => true,
        }
        && match (planned.etag.as_deref(), current.etag.as_deref()) {
            (Some(planned), Some(current)) => planned == current,
            _ => true,
        }
}

/// What a copy to S3 requires of its target.
enum RemoteGuard<'a> {
    /// The analysis saw no object: none may exist now.
    Absent,
    /// Bidirectional: the object must still be what the analysis saw.
    Unchanged(&'a ObjectEntry),
    /// A one-way copy over an object the analysis saw replaces it, by design.
    Replace,
}

impl<'a> RemoteGuard<'a> {
    fn new(bidirectional: bool, planned: Option<&'a ObjectEntry>) -> Self {
        match planned {
            None => RemoteGuard::Absent,
            Some(planned) if bidirectional => RemoteGuard::Unchanged(planned),
            Some(_) => RemoteGuard::Replace,
        }
    }
}

enum RemoteCheck {
    /// Upload, carrying this precondition where the server supports conditional writes.
    Proceed(Option<WriteCondition>),
    /// The object already is the file: an earlier attempt whose result was lost.
    AlreadyDone,
}

/// Looks at the object before it is written. The precondition returned with
/// [`RemoteCheck::Proceed`] makes the server refuse the write when the object changes
/// between this check and the upload; servers without conditional writes only get the check.
async fn check_remote_target(
    context: &RowContext,
    target: &S3Uri,
    path: &Path,
    guard: &RemoteGuard<'_>,
) -> std::result::Result<RemoteCheck, String> {
    if matches!(guard, RemoteGuard::Replace) {
        return Ok(RemoteCheck::Proceed(None));
    }
    let head = match context
        .engine
        .s3()
        .head_object(target.bucket(), target.key(), None)
        .await
    {
        Ok(head) => Some(head),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => return Err(err.to_string()),
    };
    match (guard, head) {
        (RemoteGuard::Absent, None) => Ok(RemoteCheck::Proceed(Some(WriteCondition::Absent))),
        (RemoteGuard::Unchanged(planned), Some(head)) if same_object(planned, &head) => {
            Ok(RemoteCheck::Proceed(head.etag.map(WriteCondition::ETag)))
        }
        (RemoteGuard::Unchanged(_), None) => {
            Err("the object was deleted from S3 since the analysis; analyze again".to_owned())
        }
        (RemoteGuard::Replace, _) => Ok(RemoteCheck::Proceed(None)),
        (guard, Some(head)) => {
            if object_is_file(path, head).await {
                return Ok(RemoteCheck::AlreadyDone);
            }
            Err(match guard {
                RemoteGuard::Absent => {
                    "an object with this name appeared in S3 since the analysis; it is never overwritten; analyze again"
                }
                _ => "the object in S3 changed since the analysis; analyze again",
            }
            .to_owned())
        }
    }
}

/// Whether the object in S3 is the local file (after an upload whose result was lost).
async fn remote_matches_source(context: &RowContext, target: &S3Uri, path: &Path) -> bool {
    match context
        .engine
        .s3()
        .head_object(target.bucket(), target.key(), None)
        .await
    {
        Ok(head) => object_is_file(path, head).await,
        Err(_) => false,
    }
}

async fn object_is_file(path: &Path, head: ObjectHead) -> bool {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || local_matches_object(&path, &head))
        .await
        .unwrap_or(false)
}

/// Whether the local file is what `head` describes: the same size and either the time
/// the upload adopted (the pair counts as equal to the planner) or the same content (the
/// ETag of a single-request upload is the MD5 of the data).
fn local_matches_object(path: &Path, head: &ObjectHead) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != head.size {
        return false;
    }
    let local = meta.modified().ok().and_then(system_time_to_utc);
    if let (Some(local), Some(remote)) = (local, head.last_modified)
        && (local - remote).abs() <= DEFAULT_TOLERANCE
    {
        return true;
    }
    match head.etag.as_deref() {
        Some(etag) if etag.len() == 32 && etag.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            md5_of(path).is_some_and(|md5| md5.eq_ignore_ascii_case(etag))
        }
        _ => false,
    }
}

fn md5_of(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// What a copy to a local file requires of its target.
#[derive(Clone)]
enum LocalGuard {
    /// The analysis saw nothing there: nothing may exist now.
    Absent,
    /// Bidirectional: the file must still be what the analysis saw.
    Unchanged(FileMeta),
    /// A one-way copy over a file the analysis saw replaces it, by design.
    Replace,
}

impl LocalGuard {
    fn new(bidirectional: bool, planned: Option<&FileMeta>) -> Self {
        match planned {
            None => LocalGuard::Absent,
            Some(planned) if bidirectional => LocalGuard::Unchanged(planned.clone()),
            Some(_) => LocalGuard::Replace,
        }
    }
}

enum LocalCheck {
    Proceed,
    /// The file already is the object: an earlier attempt whose result was lost.
    AlreadyDone,
}

/// The message for a local entry that exists where the analysis saw nothing.
pub(crate) fn unseen_entry_message(entry: &Metadata, rel_path: &str) -> String {
    let kind = if entry.file_type().is_symlink() {
        "link"
    } else if entry.is_dir() {
        "folder"
    } else {
        "file"
    };
    format!(
        "not written: a local {kind} named '{rel_path}' exists, but the analysis did not see it (it appeared since, or a filter or a differently spelled name hid it); it is never overwritten"
    )
}

/// Checks the local target of a download before it starts (see [`LocalGuard`]).
fn check_local_target(
    path: &Path,
    rel_path: &str,
    guard: &LocalGuard,
    object: Option<&ObjectEntry>,
) -> std::result::Result<LocalCheck, String> {
    if matches!(guard, LocalGuard::Replace) {
        return Ok(LocalCheck::Proceed);
    }
    let entry = match fs::symlink_metadata(path) {
        Ok(entry) => entry,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return match guard {
                LocalGuard::Unchanged(_) => {
                    Err("the local file was deleted since the analysis; analyze again".to_owned())
                }
                _ => Ok(LocalCheck::Proceed),
            };
        }
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    if is_downloaded_copy(&entry, object) {
        return Ok(LocalCheck::AlreadyDone);
    }
    match guard {
        // The scan followed a link, so the comparison does too.
        LocalGuard::Unchanged(planned) => match fs::metadata(path) {
            Ok(meta) if meta.is_file() && same_file(planned, &meta) => Ok(LocalCheck::Proceed),
            Ok(_) => Err("the local file changed since the analysis; analyze again".to_owned()),
            Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        },
        _ => Err(unseen_entry_message(&entry, rel_path)),
    }
}

/// Whether `entry` is a regular file the downloader wrote for `object`: its size and its
/// time, which the downloader sets to the object's `Last-Modified`.
fn is_downloaded_copy(entry: &Metadata, object: Option<&ObjectEntry>) -> bool {
    let Some(object) = object else {
        return false;
    };
    entry.is_file()
        && entry.len() == object.size
        && object.last_modified.is_some_and(|remote| {
            entry
                .modified()
                .ok()
                .and_then(system_time_to_utc)
                .is_some_and(|local| local.timestamp() == remote.timestamp())
        })
}

/// Moves a finished download into place, checking the target one last time. A target the
/// analysis did not see is created with a hard link, which fails when the name exists, so
/// nothing that appeared in the meantime can be replaced; without hard links the check and
/// the rename leave a window of a few microseconds.
fn install_download(
    staging: &Path,
    dest: &Path,
    guard: &LocalGuard,
) -> std::result::Result<(), String> {
    let moved = |err: io::Error| {
        format!(
            "cannot move the downloaded file to {}: {err}",
            dest.display()
        )
    };
    let unreadable = |err: io::Error| format!("cannot read {}: {err}", dest.display());
    match guard {
        LocalGuard::Replace => fs::rename(staging, dest).map_err(moved),
        LocalGuard::Unchanged(planned) => {
            match fs::metadata(dest) {
                Ok(meta) if meta.is_file() && same_file(planned, &meta) => {}
                Ok(_) => {
                    return Err(
                        "the local file changed while it was being downloaded; it was not replaced; analyze again"
                            .to_owned(),
                    );
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(
                        "the local file was deleted while it was being downloaded; analyze again"
                            .to_owned(),
                    );
                }
                Err(err) => return Err(unreadable(err)),
            }
            fs::rename(staging, dest).map_err(moved)
        }
        LocalGuard::Absent => {
            let appeared = || {
                format!(
                    "not written: {} appeared while the file was being downloaded; it is never overwritten; analyze again",
                    dest.display()
                )
            };
            match fs::hard_link(staging, dest) {
                Ok(()) => {
                    let _ = fs::remove_file(staging);
                    Ok(())
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(appeared()),
                Err(_) => match fs::symlink_metadata(dest) {
                    Ok(_) => Err(appeared()),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        fs::rename(staging, dest).map_err(moved)
                    }
                    Err(err) => Err(unreadable(err)),
                },
            }
        }
    }
}

/// Size and modification time of a local file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: SystemTime,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some(Stamp {
        size: meta.len(),
        modified: meta.modified().ok()?,
    })
}

/// Whether a file's time is recent enough that it could be ahead of the server's clock.
fn is_recent(time: SystemTime) -> bool {
    SystemTime::now()
        .checked_sub(SKEW_WINDOW)
        .is_none_or(|limit| time >= limit)
}

/// After an upload: gives the local file the new object's `Last-Modified`, so that the next
/// analysis sees the pair as equal. Bidirectional runs always do this (the upload time
/// makes the object newer than the file, which would be downloaded back). One-way runs only
/// do it for a file dated later than the object (`only_if_ahead`, a clock that runs ahead),
/// which would otherwise be uploaded again on every run. Best effort: when it cannot be
/// done, the next run does the work once more.
async fn adopt_remote_time(
    context: &RowContext,
    target: &S3Uri,
    path: PathBuf,
    before: Stamp,
    only_if_ahead: bool,
) {
    let Ok(head) = context
        .engine
        .s3()
        .head_object(target.bucket(), target.key(), None)
        .await
    else {
        return;
    };
    let Some(last_modified) = head.last_modified else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        set_time_if_unchanged(&path, before, last_modified, only_if_ahead)
    })
    .await;
}

fn set_time_if_unchanged(
    path: &Path,
    before: Stamp,
    time: DateTime<Utc>,
    only_if_ahead: bool,
) -> io::Result<()> {
    // Never through a link: its target lies outside the sync folder.
    let entry = fs::symlink_metadata(path)?;
    if entry.file_type().is_symlink() || !entry.is_file() {
        return Ok(());
    }
    // A file changed during the upload keeps its time, so the next run uploads it again.
    if stamp(path) != Some(before) {
        return Ok(());
    }
    if only_if_ahead
        && !system_time_to_utc(before.modified)
            .is_some_and(|local| local - time > DEFAULT_TOLERANCE)
    {
        return Ok(());
    }
    let file = File::options()
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))?;
    // The file that was opened must be the one that was inspected, not a link put in its place.
    if !same_inode(&file.metadata()?, &entry) {
        return Ok(());
    }
    file.set_modified(SystemTime::from(time))
}

#[cfg(unix)]
fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_inode(_a: &Metadata, _b: &Metadata) -> bool {
    true
}

/// Removes the folders below `root` that this run's deletions emptied: the parents of the
/// files it deleted and their ancestors, deepest first. Nothing the run did not empty is
/// touched, every path is checked like the deletions were (safe segments, no link or file
/// among the folders on the way), and `root` itself and anything that is not a real, empty
/// folder are left alone.
fn remove_emptied_folders(root: &Path, deleted: &[String]) {
    let mut folders: BTreeSet<String> = BTreeSet::new();
    for rel_path in deleted {
        if local_path_from_segments(root, rel_path).is_err() {
            continue;
        }
        let mut current = rel_path.rsplit_once('/').map(|(parent, _)| parent);
        while let Some(folder) = current {
            if !folders.insert(folder.to_owned()) {
                break;
            }
            current = folder.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    let mut ordered: Vec<&String> = folders.iter().collect();
    ordered.sort_by(|a, b| {
        b.matches('/')
            .count()
            .cmp(&a.matches('/').count())
            .then_with(|| b.cmp(a))
    });
    for rel in ordered {
        if check_folders(root, &format!("{rel}/.")).is_err() {
            continue;
        }
        let mut folder = root.to_path_buf();
        folder.extend(rel.split('/'));
        let is_real_folder = fs::symlink_metadata(&folder)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
        if is_real_folder {
            // Fails, harmlessly, when the folder is not empty.
            let _ = fs::remove_dir(&folder);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::model::Checksums;

    fn local_delete(rel_path: &str, meta: Option<FileMeta>) -> SyncRow {
        SyncRow {
            rel_path: rel_path.to_owned(),
            action: SyncAction::Delete,
            target: Some(Side::Local),
            local_meta: meta,
            s3_meta: None,
            error: None,
            done: false,
        }
    }

    fn meta_of(path: &Path) -> FileMeta {
        let meta = fs::metadata(path).unwrap();
        FileMeta {
            size: meta.len(),
            modified: meta.modified().ok().and_then(system_time_to_utc),
        }
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn local_deletions_check_the_file_and_tolerate_its_absence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a/doomed.txt"), "x");
        let planned = meta_of(&root.join("a/doomed.txt"));
        let row = local_delete("a/doomed.txt", Some(planned.clone()));
        assert!(matches!(delete_local_row(root, &row), RowResult::Done));
        assert!(!root.join("a/doomed.txt").exists());
        // Gone already: still done.
        assert!(matches!(delete_local_row(root, &row), RowResult::Done));

        // Changed since the analysis: kept.
        write(&root.join("a/edited.txt"), "before");
        let planned = meta_of(&root.join("a/edited.txt"));
        write(&root.join("a/edited.txt"), "after the analysis");
        let row = local_delete("a/edited.txt", Some(planned));
        assert!(
            matches!(delete_local_row(root, &row), RowResult::Failed(message) if message.contains("changed"))
        );
        assert!(root.join("a/edited.txt").exists());

        // A folder now: kept.
        fs::create_dir_all(root.join("a/now-a-folder")).unwrap();
        let row = local_delete(
            "a/now-a-folder",
            Some(FileMeta {
                size: 0,
                modified: None,
            }),
        );
        assert!(matches!(delete_local_row(root, &row), RowResult::Failed(_)));
        assert!(root.join("a/now-a-folder").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_deleted_through_a_folder_that_became_a_link() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        write(&outside.join("precious.txt"), "keep me");
        fs::create_dir_all(&root).unwrap();
        symlink(&outside, root.join("folder")).unwrap();
        let planned = meta_of(&outside.join("precious.txt"));
        let row = local_delete("folder/precious.txt", Some(planned));
        assert!(
            matches!(delete_local_row(&root, &row), RowResult::Failed(message) if message.contains("symbolic link"))
        );
        assert!(outside.join("precious.txt").exists());
        assert!(check_folders(&root, "folder/precious.txt").is_err());
    }

    #[test]
    fn folder_checks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("real/file.txt"), "x");
        assert!(check_folders(root, "top.txt").is_ok());
        assert!(check_folders(root, "real/new.txt").is_ok());
        assert!(check_folders(root, "real/missing/deeper/new.txt").is_ok());
        assert!(check_folders(root, "missing/new.txt").is_ok());
        let err = check_folders(root, "real/file.txt/below.txt").unwrap_err();
        assert!(err.contains("not a folder"), "{err}");
    }

    #[test]
    fn emptied_folders_are_removed_deepest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir_all(root.join("a/c/d")).unwrap();
        write(&root.join("keep/file.txt"), "x");
        fs::create_dir_all(root.join("keep/empty")).unwrap();
        let deleted: Vec<String> = ["a/b/f", "a/c/d/f", "keep/empty/f", "missing/x/f"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        remove_emptied_folders(&root, &deleted);
        assert!(!root.join("a").exists());
        assert!(!root.join("keep/empty").exists());
        assert!(root.join("keep/file.txt").exists());
        assert!(root.is_dir());
        // Emptying everything below the root keeps the root itself.
        fs::remove_file(root.join("keep/file.txt")).unwrap();
        remove_emptied_folders(&root, &["keep/f".to_owned()]);
        assert!(!root.join("keep").exists());
        assert!(root.is_dir());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn links_to_folders_are_never_removed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside-empty");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        remove_emptied_folders(&root, &["link/f".to_owned()]);
        assert!(fs::symlink_metadata(root.join("link")).is_ok());
        assert!(outside.is_dir());
    }

    fn planned_file(size: u64) -> LocalGuard {
        LocalGuard::Unchanged(FileMeta {
            size,
            modified: None,
        })
    }

    #[test]
    fn staging_files_are_hidden_unique_and_short() {
        let dest = Path::new("/sync/root/sub").join("a".repeat(250));
        let first = staging_path(&dest);
        let second = staging_path(&dest);
        assert_ne!(first, second);
        assert_eq!(first.parent(), dest.parent());
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with('.') && name.ends_with(".bbpart"), "{name}");
        // The engine adds its own ".bbpart" to this name for the partial file.
        assert!(name.len() + ".bbpart".len() < 100, "{}", name.len());
    }

    #[test]
    fn a_target_the_analysis_did_not_see_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("new.txt");
        let object = ObjectEntry {
            key: "k".to_owned(),
            size: 3,
            last_modified: Some(Utc.with_ymd_and_hms(2026, 10, 29, 8, 0, 7).unwrap()),
            ..ObjectEntry::default()
        };
        let check = |guard: &LocalGuard| check_local_target(&dest, "new.txt", guard, Some(&object));
        assert!(matches!(
            check(&LocalGuard::Absent),
            Ok(LocalCheck::Proceed)
        ));

        write(&dest, "mine");
        let err = check(&LocalGuard::Absent).err().unwrap();
        assert!(err.contains("never overwritten"), "{err}");
        assert!(err.contains("new.txt"), "{err}");
        // A one-way copy over a file the analysis saw replaces it.
        assert!(matches!(
            check(&LocalGuard::Replace),
            Ok(LocalCheck::Proceed)
        ));
        // A folder counts as something too.
        fs::remove_file(&dest).unwrap();
        fs::create_dir(&dest).unwrap();
        let err = check(&LocalGuard::Absent).err().unwrap();
        assert!(err.contains("local folder"), "{err}");
        fs::remove_dir(&dest).unwrap();

        // What an earlier attempt of this very row left (the object's size and time) is
        // that row's result, not a foreign file.
        write(&dest, "abc");
        File::options()
            .write(true)
            .open(&dest)
            .unwrap()
            .set_modified(SystemTime::from(object.last_modified.unwrap()))
            .unwrap();
        assert!(matches!(
            check(&LocalGuard::Absent),
            Ok(LocalCheck::AlreadyDone)
        ));
        assert!(matches!(
            check(&planned_file(1)),
            Ok(LocalCheck::AlreadyDone)
        ));
        // Another size is another file.
        fs::write(&dest, "abcd").unwrap();
        assert!(check(&LocalGuard::Absent).is_err());
        let err = check(&planned_file(3)).err().unwrap();
        assert!(err.contains("changed"), "{err}");
    }

    #[test]
    fn downloads_are_installed_without_replacing_what_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let stage = |name: &str| {
            let path = dir.path().join(name);
            fs::write(&path, "downloaded").unwrap();
            path
        };
        // Nothing there: the file is created and the staging name is gone.
        let dest = dir.path().join("fresh.txt");
        let staging = stage(".fresh.bbpart");
        install_download(&staging, &dest, &LocalGuard::Absent).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "downloaded");
        assert!(!staging.exists());

        // Something appeared during the transfer: it stays.
        let dest = dir.path().join("raced.txt");
        fs::write(&dest, "created meanwhile").unwrap();
        let staging = stage(".raced.bbpart");
        let err = install_download(&staging, &dest, &LocalGuard::Absent).unwrap_err();
        assert!(err.contains("never overwritten"), "{err}");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "created meanwhile");

        // Bidirectional: an edit during the transfer stays; an untouched file is replaced.
        let dest = dir.path().join("edited.txt");
        fs::write(&dest, "before").unwrap();
        let planned = meta_of(&dest);
        fs::write(&dest, "edited during the download").unwrap();
        let staging = stage(".edited.bbpart");
        let guard = LocalGuard::Unchanged(planned);
        let err = install_download(&staging, &dest, &guard).unwrap_err();
        assert!(err.contains("changed while"), "{err}");
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            "edited during the download"
        );
        let planned = meta_of(&dest);
        let guard = LocalGuard::Unchanged(planned);
        install_download(&staging, &dest, &guard).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "downloaded");

        // One-way over a seen file: replaced.
        let dest = dir.path().join("mirror.txt");
        fs::write(&dest, "old").unwrap();
        let staging = stage(".mirror.bbpart");
        install_download(&staging, &dest, &LocalGuard::Replace).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "downloaded");
    }

    #[test]
    fn an_object_is_the_local_file_when_size_and_time_or_content_agree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, "hello").unwrap();
        let local = meta_of(&path).modified.unwrap();
        let head = |size: u64, time: Option<DateTime<Utc>>, etag: Option<&str>| ObjectHead {
            key: "k".to_owned(),
            size,
            etag: etag.map(str::to_owned),
            last_modified: time,
            ..ObjectHead::default()
        };
        // The upload gave the file the object's time.
        assert!(local_matches_object(&path, &head(5, Some(local), None)));
        let skewed = local + chrono::TimeDelta::hours(1);
        assert!(!local_matches_object(&path, &head(5, Some(skewed), None)));
        assert!(!local_matches_object(&path, &head(6, Some(local), None)));
        // Otherwise the ETag of a single-request upload is the MD5 of the data.
        let md5 = "5d41402abc4b2a76b9719d911017c592";
        assert!(local_matches_object(
            &path,
            &head(5, Some(skewed), Some(md5))
        ));
        assert!(!local_matches_object(
            &path,
            &head(5, Some(skewed), Some("00000000000000000000000000000000"))
        ));
        // A multipart ETag cannot be compared with a local hash.
        assert!(!local_matches_object(
            &path,
            &head(5, Some(skewed), Some("5d41402abc4b2a76b9719d911017c592-2"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn adopted_times_are_never_written_through_a_link() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside.txt");
        let link = dir.path().join("link.txt");
        write(&target, "content");
        symlink(&target, &link).unwrap();
        let before = stamp(&link).unwrap();
        let time = Utc.with_ymd_and_hms(2001, 2, 3, 4, 5, 6).unwrap();
        set_time_if_unchanged(&link, before, time, false).unwrap();
        assert_eq!(
            stamp(&target),
            Some(before),
            "the link's target was touched"
        );
        // A regular file gets the time, unless it changed since the stamp.
        let file = dir.path().join("file.txt");
        write(&file, "content");
        let before = stamp(&file).unwrap();
        set_time_if_unchanged(&file, before, time, false).unwrap();
        assert_eq!(meta_of(&file).modified, Some(time));
        write(&file, "changed");
        let stale = stamp(&file).unwrap();
        fs::write(&file, "changed again!").unwrap();
        let other = Utc.with_ymd_and_hms(2002, 2, 3, 4, 5, 6).unwrap();
        set_time_if_unchanged(&file, stale, other, false).unwrap();
        assert_ne!(meta_of(&file).modified, Some(other));
        // One-way runs only move a time that is ahead of the object's.
        let ahead = Utc::now() + chrono::TimeDelta::hours(1);
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(SystemTime::from(ahead))
            .unwrap();
        let before = stamp(&file).unwrap();
        let object_time = Utc::now();
        set_time_if_unchanged(&file, before, object_time, true).unwrap();
        let adopted = meta_of(&file).modified.unwrap();
        assert!((adopted - object_time).num_seconds().abs() <= 1);
        let before = stamp(&file).unwrap();
        let later = object_time + chrono::TimeDelta::hours(2);
        set_time_if_unchanged(&file, before, later, true).unwrap();
        assert_eq!(stamp(&file), Some(before), "an older file keeps its time");
    }

    #[test]
    fn emptied_folders_are_only_those_this_runs_deletions_emptied_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside-empty");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(root.join("emptied/deep")).unwrap();
        fs::create_dir_all(root.join("untouched")).unwrap();
        // A damaged plan names a folder outside the root; segments are validated.
        remove_emptied_folders(
            &root,
            &["../outside-empty/f".to_owned(), "emptied/deep/f".to_owned()],
        );
        assert!(outside.is_dir(), "a folder outside the root was removed");
        assert!(!root.join("emptied").exists());
        // Folders no deletion of this run emptied stay, even when they are empty.
        assert!(root.join("untouched").is_dir());
        assert!(root.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn emptied_folders_are_not_removed_behind_a_link_on_the_way() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let external = dir.path().join("external");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(external.join("b")).unwrap();
        symlink(&external, root.join("a")).unwrap();
        remove_emptied_folders(&root, &["a/b/f.txt".to_owned()]);
        assert!(
            external.join("b").is_dir(),
            "removed through an ancestor link"
        );
        assert!(fs::symlink_metadata(root.join("a")).is_ok());
    }

    #[test]
    fn identity_of_objects_and_files() {
        let at = |secs: u32, millis: u32| {
            Utc.with_ymd_and_hms(2026, 10, 29, 8, 0, secs).unwrap()
                + chrono::TimeDelta::milliseconds(i64::from(millis))
        };
        let listed = ObjectEntry {
            key: "k".to_owned(),
            size: 5,
            etag: None,
            last_modified: Some(at(7, 250)),
            storage_class: None,
            checksums: Checksums::default(),
            is_prefix: false,
        };
        let head = ObjectHead {
            key: "k".to_owned(),
            size: 5,
            etag: Some("abc".to_owned()),
            last_modified: Some(at(7, 0)),
            ..ObjectHead::default()
        };
        // HEAD answers have whole seconds; a listing without ETag compares size and time.
        assert!(same_object(&listed, &head));
        assert!(!same_object(
            &listed,
            &ObjectHead {
                size: 6,
                ..head.clone()
            }
        ));
        assert!(!same_object(
            &listed,
            &ObjectHead {
                last_modified: Some(at(8, 0)),
                ..head.clone()
            }
        ));
        let with_etag = ObjectEntry {
            etag: Some("other".to_owned()),
            ..listed.clone()
        };
        assert!(!same_object(&with_etag, &head));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, "12345").unwrap();
        let planned = meta_of(&path);
        assert!(same_file(&planned, &fs::metadata(&path).unwrap()));
        assert!(same_file(
            &FileMeta {
                size: 5,
                modified: None
            },
            &fs::metadata(&path).unwrap()
        ));
        fs::write(&path, "123456").unwrap();
        assert!(!same_file(&planned, &fs::metadata(&path).unwrap()));
    }

    #[test]
    fn limiter_grants_at_most_one_second_of_its_rate() {
        let limiter = BandwidthLimiter::unlimited();
        let throttle = LimiterThrottle(limiter.clone());
        assert_eq!(throttle.max_grant(), u64::MAX);
        limiter.set_rate(NonZeroU64::new(1500));
        assert_eq!(throttle.max_grant(), 1500);
        limiter.set_rate(None);
        assert_eq!(throttle.max_grant(), u64::MAX);
    }

    #[test]
    fn results_are_batched_and_stops_are_not_recorded() {
        let rows = (0..BATCH_SIZE + 2)
            .map(|n| SyncRow {
                rel_path: format!("f{n}"),
                action: SyncAction::Copy,
                target: Some(Side::Remote),
                local_meta: None,
                s3_meta: None,
                error: None,
                done: false,
            })
            .collect();
        let mut state = RunState::new("plan".to_owned(), rows);
        assert!(state.flush_delay().is_none());
        state.record(0, RowResult::Stopped);
        assert!(state.batch.is_empty() && state.flush_delay().is_none());
        state.record(0, RowResult::Failed("  ".to_owned()));
        assert_eq!(state.rows[0].error.as_deref(), Some(UNKNOWN_ERROR));
        assert!(state.flush_delay().is_some());
        assert!(!state.flush_due());
        for index in 1..BATCH_SIZE {
            state.record(index, RowResult::Done);
        }
        assert!(state.flush_due());
        assert_eq!((state.attempted, state.errors), (BATCH_SIZE, 1));
        let progress = state.progress(false);
        assert_eq!(
            (progress.total, progress.done, progress.failed),
            (BATCH_SIZE + 2, BATCH_SIZE - 1, 1)
        );
    }

    #[test]
    fn options_follow_the_settings() {
        let mut settings = Settings::default();
        settings.transfer.max_concurrent_tasks = 7;
        settings.transfer.bandwidth_limit_kibps = Some(2);
        let options = ExecuteOptions::standalone(&settings);
        assert_eq!(options.concurrency, 7);
        assert_eq!(options.limiter.rate(), NonZeroU64::new(2048));
        settings.transfer.bandwidth_limit_kibps = None;
        assert_eq!(ExecuteOptions::standalone(&settings).limiter.rate(), None);
    }

    #[test]
    fn progress_serializes_as_in_the_contract() {
        let progress = SyncProgress::from_counts(
            "p",
            RowCounts {
                total: 3,
                done: 1,
                failed: 1,
            },
            true,
        );
        assert_eq!(
            serde_json::to_value(&progress).unwrap(),
            serde_json::json!({ "plan_id": "p", "total": 3, "done": 1, "failed": 1, "finished": true })
        );
    }
}
