//! The transfer queue: scheduling, state machine, automatic retries, events
//! and persistence.
//!
//! State machine (see `docs/transfers.md` for the full table):
//!
//! ```text
//!   add ──> queued ──(slot free)──> running ──(Ok)──────────────> completed
//!             │  ^                   │  │ │
//!   pause,    │  │ resume,           │  │ └─(retryable error, budget left:
//!   stop_all  │  │ start_all         │  │    back off, run again)
//!             v  │                   │  └─(other error / budget spent)─> failed
//!           stopped <──(pause, stop_all, shutdown)─┘                       │
//!             │                                                retry_task │
//!             │                                                 (queued) <┘
//!   cancel from queued, stopped, failed or running ──(cleanup)──> cancelled
//!                                      (cleanup fails) ──> failed, kept
//! ```
//!
//! `completed` and `cancelled` are terminal. Commands apply only to the states
//! the contract lists: `pause_task` to queued or running tasks, `resume_task`
//! to stopped ones, `retry_task` to failed ones, `cancel_task` to queued,
//! running, stopped or failed ones. Anything else fails with
//! [`ErrorKind::Conflict`], including pausing a task that is being cancelled.
//! Repeating a pause or cancel on a task that is still running (or whose
//! cancellation is in progress) is accepted and changes nothing.
//!
//! A cancellation that cannot clean up (the cleanup fails or times out, for
//! example because the network is down) does not end in `cancelled`, which
//! would lose the resume state that names what is left behind: the task is
//! `failed` instead, with the error and its resume state, and cancelling it
//! again, or opening the queue again, repeats the cleanup.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::num::NonZeroU64;
use std::ops::Bound;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::{Notify, OnceCell, Semaphore, mpsc, watch};
use tokio::task::AbortHandle;
use tokio::time::{Instant, Interval, MissedTickBehavior};

use super::limiter::BandwidthLimiter;
use super::rate::{RateEstimator, eta_secs};
use super::retry::{Backoff, SplitMix64, is_retryable};
use super::runner::{
    ChangeHook, ChangeHooks, Progress, ResumeState, Runners, StopController, StopReason,
    StopSignal, TaskContext, TaskRunner,
};
use super::store::{self, StoredTask};
use super::{io_error, new_error};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{Location, TransferKind, TransferState, TransferTask};
use crate::settings::TransferSettings;

#[cfg(test)]
mod tests;

/// Default delay between a change and the save that includes it.
const DEFAULT_PERSIST_DELAY: Duration = Duration::from_secs(1);
/// Default (and contract) interval of progress events per running task.
const DEFAULT_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Default time a runner gets to return after a stop request.
const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(5);
/// Default time a runner's cleanup may take before it counts as failed.
const DEFAULT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest accepted progress interval (larger values could overflow the
/// timer arithmetic).
const MAX_PROGRESS_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Stand-in for "never" when a deadline would overflow (about 30 years).
const FAR_FUTURE: Duration = Duration::from_secs(30 * 365 * 24 * 60 * 60);
/// Cleanups of tasks that were not running at cancel time run at most this
/// many at a time.
const CLEANUP_CONCURRENCY: usize = 8;

/// A task to add to the queue. The queue assigns the id, timestamps and
/// state (`queued`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    /// Account whose credentials the task runs with (must not be empty).
    pub account_id: String,
    /// Shared by the tasks of one folder operation.
    pub group_id: Option<String>,
    /// What the task does; a runner must be registered for it.
    pub kind: TransferKind,
    /// What is transferred or deleted.
    pub src: Location,
    /// `None` exactly for delete tasks.
    pub dst: Option<Location>,
    /// Bytes (objects for delete tasks), when known in advance.
    pub bytes_total: Option<u64>,
}

/// A task to add in the `failed` state: it failed before it could run (for
/// example an entry of a selected folder that cannot be read). It behaves
/// like any failed task: `retry_task` queues it and its runner runs with
/// `resume` as the resume state, `cancel_task` runs the cleanup with it.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedTask {
    /// The task.
    pub task: NewTask,
    /// Why it failed; shown as the task's `error`.
    pub error: String,
    /// Its resume state.
    pub resume: Option<Value>,
}

/// Something observable happened in the queue.
// Nearly every event is `Updated`, so boxing the task would only add an
// allocation per event without saving memory.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum QueueEvent {
    /// A task was added or changed: state changes are sent immediately,
    /// progress (bytes, speed, ETA) at most every 250 ms per task.
    /// Maps to `transfer://update`.
    Updated(TransferTask),
    /// Tasks were removed by [`TransferQueue::clear_finished`].
    /// Maps to `transfer://removed`.
    Removed(Vec<String>),
    /// Saving the queue in the background failed (sent once per distinct
    /// error until a save succeeds again). The queue keeps working in memory
    /// and retries with the next change, [`TransferQueue::flush`] or
    /// [`TransferQueue::shutdown`].
    PersistFailed(String),
}

/// Construction options of a [`TransferQueue`].
#[derive(Debug, Clone)]
pub struct QueueOptions {
    /// Queue file; `None` keeps the queue in memory only.
    pub persist_path: Option<PathBuf>,
    /// Delay between a change and the save that includes it; changes made in
    /// the meantime share that save.
    pub persist_delay: Duration,
    /// Interval of progress events per running task (more than zero, at most
    /// one day).
    pub progress_interval: Duration,
    /// Time a runner gets to return after a stop request before it is dropped.
    /// Shutdown waits at most this long for runs and cleanups.
    pub stop_grace: Duration,
    /// Time a runner's cleanup may take; after that the cleanup counts as
    /// failed: the task ends `failed` with the timeout as its error (see
    /// [`TransferQueue::cancel_task`]).
    pub cleanup_timeout: Duration,
    /// Backoff between automatic retries.
    pub backoff: Backoff,
    /// Seed for the backoff jitter; `None` seeds from entropy.
    pub jitter_seed: Option<u64>,
}

impl Default for QueueOptions {
    fn default() -> Self {
        Self {
            persist_path: None,
            persist_delay: DEFAULT_PERSIST_DELAY,
            progress_interval: DEFAULT_PROGRESS_INTERVAL,
            stop_grace: DEFAULT_STOP_GRACE,
            cleanup_timeout: DEFAULT_CLEANUP_TIMEOUT,
            backoff: Backoff::default(),
            jitter_seed: None,
        }
    }
}

impl QueueOptions {
    /// Default options with the queue persisted at `path`.
    pub fn persistent(path: impl Into<PathBuf>) -> Self {
        Self {
            persist_path: Some(path.into()),
            ..Self::default()
        }
    }
}

/// The transfer queue. See the module documentation for the state machine and
/// `docs/transfers.md` for the design.
///
/// Commands are synchronous and cheap; work happens on the Tokio runtime the
/// queue was opened in. Call [`TransferQueue::shutdown`] before exiting: it
/// stops running tasks as `stopped`, keeps their resume state and saves the
/// queue. Dropping the queue starts the same shutdown in the background.
pub struct TransferQueue {
    shared: Arc<Shared>,
}

impl TransferQueue {
    /// Opens a queue, loading `options.persist_path` if it exists. Loaded
    /// tasks that were `queued` or `running` come back as `stopped`; tasks
    /// whose cancellation was interrupted by a shutdown get their cleanup run
    /// again and then become `cancelled`.
    ///
    /// Must be called inside a Tokio runtime. A damaged queue file is an `io`
    /// error and is left untouched.
    pub async fn open(
        settings: TransferSettings,
        runners: Runners,
        options: QueueOptions,
    ) -> Result<Self> {
        let rate = validate_settings(&settings)?;
        if options.progress_interval.is_zero() || options.progress_interval > MAX_PROGRESS_INTERVAL
        {
            return Err(new_error(
                ErrorKind::Usage,
                "the progress interval must be longer than zero and at most one day",
            ));
        }
        let runtime = Handle::try_current().map_err(|_| {
            new_error(
                ErrorKind::Internal,
                "the transfer queue must be opened inside a Tokio runtime",
            )
        })?;
        let stored = match &options.persist_path {
            Some(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || store::load(&path))
                    .await
                    .map_err(|e| {
                        new_error(
                            ErrorKind::Internal,
                            format!("loading the transfer queue failed: {e}"),
                        )
                    })??
            }
            None => Vec::new(),
        };
        let persist = options.persist_path.clone().map(|path| Persist {
            path,
            signal: Arc::new(PersistSignal::default()),
            serial: Arc::new(tokio::sync::Mutex::new(())),
            last_error: Mutex::new(None),
        });
        let signal = persist.as_ref().map(|p| Arc::clone(&p.signal));
        let resume_hooks = signal.clone().map(|signal| {
            let changed: ChangeHook = {
                let signal = Arc::clone(&signal);
                Arc::new(move || signal.mark())
            };
            let urgent: ChangeHook = Arc::new(move || signal.mark_urgent());
            ChangeHooks { changed, urgent }
        });
        let rng = options
            .jitter_seed
            .map_or_else(SplitMix64::from_entropy, SplitMix64::new);
        let mut state = State {
            settings,
            entries: BTreeMap::new(),
            ids: HashMap::new(),
            queued: BTreeMap::new(),
            last_account: None,
            running: 0,
            jobs: 0,
            next_seq: 0,
            next_job: 0,
            closed: false,
            subscribers: Vec::new(),
            rng,
            persist: signal.clone(),
            resume_hooks,
        };
        let mut interrupted_cancels = Vec::new();
        for StoredTask {
            mut task,
            resume,
            cancel_pending,
        } in stored
        {
            if matches!(task.state, TransferState::Queued | TransferState::Running) {
                // A `running` task may have been saved while backing off with
                // the error it was retrying; that error is stale now.
                task.state = TransferState::Stopped;
                task.error = None;
            }
            task.speed = 0;
            task.eta = None;
            let unfinished = !matches!(
                task.state,
                TransferState::Completed | TransferState::Cancelled | TransferState::Skipped
            );
            let seq = state.insert(task, resume);
            if cancel_pending && unfinished {
                interrupted_cancels.push(seq);
            }
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            runners,
            limiter: BandwidthLimiter::new(rate),
            options,
            runtime,
            persist,
            idle: Notify::new(),
            closing: watch::channel(false).0,
            cleanup_permits: Arc::new(Semaphore::new(CLEANUP_CONCURRENCY)),
            shutdown_outcome: OnceCell::new(),
        });
        if let Some(signal) = signal {
            shared
                .runtime
                .spawn(run_writer(Arc::clone(&shared), signal));
        }
        if !interrupted_cancels.is_empty() {
            let mut guard = shared.lock();
            for seq in interrupted_cancels {
                shared.start_cleanup(&mut guard, seq);
            }
        }
        Ok(Self { shared })
    }

    /// Adds tasks in order and returns them as created (state `queued`).
    /// Either all tasks are added or, on a validation error, none.
    pub fn add_tasks(&self, tasks: Vec<NewTask>) -> Result<Vec<TransferTask>> {
        self.add_tasks_with_failures(tasks, Vec::new())
    }

    /// Adds `tasks` as `queued` and then `failed` as `failed` tasks (with
    /// their error and resume state, `attempts` 0), in order, and returns
    /// them as created. Either all are added or, on a validation error, none.
    pub fn add_tasks_with_failures(
        &self,
        tasks: Vec<NewTask>,
        failed: Vec<FailedTask>,
    ) -> Result<Vec<TransferTask>> {
        let entries = tasks
            .into_iter()
            .map(|task| (task, None, None))
            .chain(
                failed
                    .into_iter()
                    .map(|failed| (failed.task, Some(failed.error), failed.resume)),
            )
            .collect();
        self.add_entries(entries)
    }

    /// Adds one task as `queued` whose first run, and every later one until
    /// the runner replaces it, gets `resume` as its resume state. A runner uses
    /// this to hand a request that is not part of [`NewTask`] to its own run
    /// (the S3 runner's download of one object version). See
    /// [`TransferQueue::add_tasks`] for the validation.
    pub fn add_task_with_resume(&self, task: NewTask, resume: Value) -> Result<TransferTask> {
        let mut added = self.add_entries(vec![(task, None, Some(resume))])?;
        added
            .pop()
            .ok_or_else(|| new_error(ErrorKind::Internal, "the added task went missing"))
    }

    /// Validates and adds `(task, error, resume)` entries in order: an entry
    /// with an error is added as `failed`, any other as `queued`.
    fn add_entries(
        &self,
        entries: Vec<(NewTask, Option<String>, Option<Value>)>,
    ) -> Result<Vec<TransferTask>> {
        for (task, _, _) in &entries {
            validate_new_task(task, &self.shared.runners)?;
        }
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let now = Utc::now();
        let mut added = Vec::with_capacity(entries.len());
        for (new, error, resume) in entries {
            let id = st.fresh_id();
            let state = if error.is_some() {
                TransferState::Failed
            } else {
                TransferState::Queued
            };
            let task = TransferTask {
                id,
                account_id: new.account_id,
                group_id: new.group_id,
                kind: new.kind,
                src: new.src,
                dst: new.dst,
                bytes_total: new.bytes_total,
                bytes_done: 0,
                speed: 0,
                eta: None,
                state,
                error,
                attempts: 0,
                created_at: now,
                updated_at: now,
            };
            added.push(task.clone());
            let seq = st.insert(task, resume);
            if state == TransferState::Queued {
                st.add_queued(seq);
            }
            if let Some(entry) = st.entries.get(&seq) {
                send(&mut st.subscribers, || {
                    QueueEvent::Updated(entry.task.clone())
                });
            }
        }
        st.mark_dirty();
        self.shared.schedule(st);
        Ok(added)
    }

    /// Adds one task. See [`TransferQueue::add_tasks`].
    pub fn add_task(&self, task: NewTask) -> Result<TransferTask> {
        let mut added = self.add_tasks(vec![task])?;
        added
            .pop()
            .ok_or_else(|| new_error(ErrorKind::Internal, "the added task went missing"))
    }

    /// All tasks in creation order.
    pub fn list(&self) -> Vec<TransferTask> {
        self.shared
            .lock()
            .entries
            .values()
            .map(|entry| entry.task.clone())
            .collect()
    }

    /// One task (`not_found` for an unknown id).
    pub fn get(&self, id: &str) -> Result<TransferTask> {
        let st = self.shared.lock();
        let seq = st.seq_of(id)?;
        st.entries
            .get(&seq)
            .map(|entry| entry.task.clone())
            .ok_or_else(|| missing_task(id))
    }

    /// The resume state a runner left for a task (diagnostics and tests).
    pub fn resume_state(&self, id: &str) -> Result<Option<Value>> {
        let st = self.shared.lock();
        let seq = st.seq_of(id)?;
        st.entries
            .get(&seq)
            .map(|entry| entry.resume.get())
            .ok_or_else(|| missing_task(id))
    }

    /// `queued` becomes `stopped`; a `running` task is asked to pause and
    /// becomes `stopped` when its runner returns. Any other state (including
    /// `stopped`, and a task being cancelled) is a `conflict`.
    pub fn pause_task(&self, id: &str) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let seq = st.seq_of(id)?;
        match st.status(seq, id)? {
            Status::Running(None) => {
                st.request_stop(seq, StopReason::Pause);
                Ok(())
            }
            // Still running, pause already requested.
            Status::Running(Some(StopReason::Pause)) => Ok(()),
            Status::Idle(TransferState::Queued) => {
                st.remove_queued(seq);
                st.set_state(seq, TransferState::Stopped, None);
                Ok(())
            }
            other => Err(invalid_transition("pause", other)),
        }
    }

    /// `stopped` becomes `queued`; any other state is a `conflict`.
    pub fn resume_task(&self, id: &str) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let seq = st.seq_of(id)?;
        match st.status(seq, id)? {
            Status::Idle(TransferState::Stopped) => {
                st.requeue(seq);
                self.shared.schedule(st);
                Ok(())
            }
            other => Err(invalid_transition("resume", other)),
        }
    }

    /// `failed` becomes `queued` with a fresh retry budget; the next run
    /// continues from the resume state. Any other state is a `conflict`.
    pub fn retry_task(&self, id: &str) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let seq = st.seq_of(id)?;
        match st.status(seq, id)? {
            Status::Idle(TransferState::Failed) => {
                st.requeue(seq);
                self.shared.schedule(st);
                Ok(())
            }
            other => Err(invalid_transition("retry", other)),
        }
    }

    /// Cancels a `queued`, `running`, `stopped` or `failed` task. Running
    /// tasks are asked to stop first; in every case the runner's cleanup runs
    /// once (also for failed tasks, which may still own an open multipart
    /// upload), then the task becomes `cancelled`. If the cleanup fails or
    /// times out, the task becomes `failed` instead and keeps its resume
    /// state, so that cancelling it again (or opening the queue again)
    /// repeats the cleanup. `completed` and `cancelled` tasks are a
    /// `conflict`.
    pub fn cancel_task(&self, id: &str) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let seq = st.seq_of(id)?;
        match st.status(seq, id)? {
            Status::Running(None | Some(StopReason::Pause)) => {
                st.request_cancel(seq);
                Ok(())
            }
            // Cancellation already under way; its cleanup runs only once.
            Status::Running(Some(StopReason::Cancel)) | Status::Cancelling => Ok(()),
            Status::Idle(
                TransferState::Queued | TransferState::Stopped | TransferState::Failed,
            ) => {
                self.shared.start_cleanup(st, seq);
                Ok(())
            }
            other => Err(invalid_transition("cancel", other)),
        }
    }

    /// Every `stopped` task becomes `queued`.
    pub fn start_all(&self) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        for seq in st.seqs_with(|status| status == Status::Idle(TransferState::Stopped)) {
            st.requeue(seq);
        }
        self.shared.schedule(st);
        Ok(())
    }

    /// Every `queued` task becomes `stopped`; running tasks are asked to pause.
    pub fn stop_all(&self) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        for seq in st.seqs_with(|status| {
            matches!(
                status,
                Status::Idle(TransferState::Queued) | Status::Running(None)
            )
        }) {
            if st.status_of(seq) == Some(Status::Running(None)) {
                st.request_stop(seq, StopReason::Pause);
            } else {
                st.remove_queued(seq);
                st.set_state(seq, TransferState::Stopped, None);
            }
        }
        Ok(())
    }

    /// Cancels every task that is not `completed` or `cancelled`.
    pub fn cancel_all(&self) -> Result<()> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        for seq in st.seqs_with(|status| {
            matches!(
                status,
                Status::Running(None | Some(StopReason::Pause))
                    | Status::Idle(
                        TransferState::Queued | TransferState::Stopped | TransferState::Failed
                    )
            )
        }) {
            if matches!(st.status_of(seq), Some(Status::Running(_))) {
                st.request_cancel(seq);
            } else {
                self.shared.start_cleanup(st, seq);
            }
        }
        Ok(())
    }

    /// Removes `completed` and `cancelled` tasks and returns their ids (also
    /// sent as one [`QueueEvent::Removed`]).
    pub fn clear_finished(&self) -> Result<Vec<String>> {
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        let finished = st.seqs_with(|status| {
            matches!(
                status,
                Status::Idle(
                    TransferState::Completed | TransferState::Cancelled | TransferState::Skipped
                )
            )
        });
        let mut removed = Vec::with_capacity(finished.len());
        for seq in finished {
            if let Some(entry) = st.entries.remove(&seq) {
                st.ids.remove(&entry.task.id);
                removed.push(entry.task.id);
            }
        }
        if !removed.is_empty() {
            send(&mut st.subscribers, || QueueEvent::Removed(removed.clone()));
            st.mark_dirty();
        }
        Ok(removed)
    }

    /// Current transfer settings.
    pub fn settings(&self) -> TransferSettings {
        self.shared.lock().settings.clone()
    }

    /// Applies new settings at once: concurrency (running tasks are never
    /// interrupted; new ones start while below the cap), bandwidth limit
    /// (waiters adopt the new rate), retry budget (used by the next failure)
    /// and the part settings passed to future runs.
    pub fn update_settings(&self, settings: TransferSettings) -> Result<()> {
        let rate = validate_settings(&settings)?;
        let mut guard = self.shared.lock();
        let st = &mut *guard;
        st.ensure_open()?;
        st.settings = settings;
        if self.shared.limiter.rate() != rate {
            self.shared.limiter.set_rate(rate);
        }
        self.shared.schedule(st);
        Ok(())
    }

    /// The bandwidth limiter shared by all tasks of this queue.
    pub fn limiter(&self) -> BandwidthLimiter {
        self.shared.limiter.clone()
    }

    /// A new event stream. Events are never dropped; drop the receiver when
    /// it is no longer read.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<QueueEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared.lock().subscribers.push(sender);
        receiver
    }

    /// Saves pending changes now (no-op without a queue file or changes).
    pub async fn flush(&self) -> Result<()> {
        self.shared.save().await
    }

    /// Stops the queue: no new runs start, queued tasks become `stopped`,
    /// running tasks are asked to pause (and dropped after the grace period),
    /// cleanups get the same grace period, then the queue is saved.
    ///
    /// Later commands fail with `conflict`; `list`, `get`, `subscribe` and
    /// `flush` keep working. Calling it again returns the first outcome.
    pub async fn shutdown(&self) -> Result<()> {
        self.shared.shutdown().await
    }

    #[cfg(test)]
    fn persisted_writes(&self) -> u64 {
        self.shared
            .persist
            .as_ref()
            .map_or(0, |p| p.signal.writes.load(Ordering::SeqCst))
    }
}

impl Drop for TransferQueue {
    fn drop(&mut self) {
        self.shared.begin_shutdown();
        let shared = Arc::clone(&self.shared);
        // Finishes in the background; with no runtime left this does nothing.
        drop(self.shared.runtime.spawn(async move {
            let _ = shared.shutdown().await;
        }));
    }
}

impl fmt::Debug for TransferQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.shared.lock();
        f.debug_struct("TransferQueue")
            .field("tasks", &st.entries.len())
            .field("running", &st.running)
            .field("closed", &st.closed)
            .finish()
    }
}

fn validate_settings(settings: &TransferSettings) -> Result<Option<NonZeroU64>> {
    if settings.max_concurrent_tasks == 0 {
        return Err(new_error(
            ErrorKind::Usage,
            "max_concurrent_tasks must be at least 1",
        ));
    }
    if settings.max_parts_per_task == 0 {
        return Err(new_error(
            ErrorKind::Usage,
            "max_parts_per_task must be at least 1",
        ));
    }
    match settings.bandwidth_limit_kibps {
        None => Ok(None),
        Some(0) => Err(new_error(
            ErrorKind::Usage,
            "bandwidth_limit_kibps must be at least 1 (use null for unlimited)",
        )),
        Some(kibps) => Ok(NonZeroU64::new(kibps.saturating_mul(1024))),
    }
}

fn validate_new_task(task: &NewTask, runners: &Runners) -> Result<()> {
    if task.account_id.trim().is_empty() {
        return Err(new_error(
            ErrorKind::Usage,
            "a transfer task needs an account id",
        ));
    }
    let kind = kind_name(task.kind);
    match (task.kind, &task.dst) {
        (TransferKind::Delete, Some(_)) => {
            return Err(new_error(
                ErrorKind::Usage,
                "delete tasks have no destination",
            ));
        }
        (TransferKind::Delete, None) | (_, Some(_)) => {}
        (_, None) => {
            return Err(new_error(
                ErrorKind::Usage,
                format!("{kind} tasks need a destination"),
            ));
        }
    }
    if !runners.contains(task.kind) {
        return Err(new_error(
            ErrorKind::Usage,
            format!("no runner is registered for {kind} tasks"),
        ));
    }
    Ok(())
}

fn kind_name(kind: TransferKind) -> &'static str {
    match kind {
        TransferKind::Upload => "upload",
        TransferKind::Download => "download",
        TransferKind::Copy => "copy",
        TransferKind::Move => "move",
        TransferKind::Delete => "delete",
    }
}

fn state_name(state: TransferState) -> &'static str {
    match state {
        TransferState::Queued => "queued",
        TransferState::Running => "running",
        TransferState::Stopped => "stopped",
        TransferState::Completed => "completed",
        TransferState::Failed => "failed",
        TransferState::Cancelled => "cancelled",
        TransferState::Skipped => "skipped",
    }
}

fn missing_task(id: &str) -> Error {
    new_error(
        ErrorKind::NotFound,
        format!("transfer task {id} does not exist"),
    )
}

fn invalid_transition(action: &str, status: Status) -> Error {
    let current = match status {
        Status::Idle(state) => state_name(state),
        Status::Running(None) => "running",
        Status::Running(Some(StopReason::Pause)) => "stopping",
        Status::Running(Some(StopReason::Cancel)) | Status::Cancelling => "being cancelled",
    };
    new_error(
        ErrorKind::Conflict,
        format!("cannot {action} a task that is {current}"),
    )
}

/// Sends an event to every live subscriber, dropping closed ones. The event
/// is only built when someone listens.
fn send(
    subscribers: &mut Vec<mpsc::UnboundedSender<QueueEvent>>,
    event: impl FnOnce() -> QueueEvent,
) {
    if subscribers.is_empty() {
        return;
    }
    let event = event();
    subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a task is doing, beyond its visible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// No run or cleanup in progress.
    Idle(TransferState),
    /// A run is in progress (state `running`), possibly asked to stop.
    Running(Option<StopReason>),
    /// A cleanup for a task that was not running is in progress; the task
    /// keeps its state until the cleanup ends.
    Cancelling,
}

enum Activity {
    Idle,
    Running(RunControl),
    Cancelling { job: u64, abort: AbortHandle },
}

struct RunControl {
    job: u64,
    stop: StopController,
    progress: Progress,
    abort: AbortHandle,
}

struct Entry {
    task: TransferTask,
    resume: ResumeState,
    activity: Activity,
    /// Automatic retries used since the task was last started by the user.
    retries_used: u32,
    /// A cancel was requested and its cleanup has not finished (it was cut
    /// short by a shutdown, or it failed). Saved with the task, so the next
    /// open runs the cleanup again instead of leaving a resumable task
    /// behind. A user action that starts the task again clears it.
    cancel_pending: bool,
}

impl Entry {
    fn status(&self) -> Status {
        match &self.activity {
            Activity::Idle => Status::Idle(self.task.state),
            Activity::Running(run) => Status::Running(run.stop.requested()),
            Activity::Cancelling { .. } => Status::Cancelling,
        }
    }

    /// The run controller, if `job` is the current run of this task.
    fn run(&self, job: u64) -> Option<&RunControl> {
        match &self.activity {
            Activity::Running(run) if run.job == job => Some(run),
            _ => None,
        }
    }

    /// Copies the final counters of the current run into the task.
    fn sync_progress(&mut self) {
        if let Activity::Running(run) = &self.activity {
            self.task.bytes_done = run.progress.done();
            self.task.bytes_total = run.progress.total();
        }
    }
}

#[derive(Default)]
struct PersistSignal {
    dirty: AtomicBool,
    notify: Notify,
    /// A change that must not wait for the persist delay is pending.
    urgent: AtomicBool,
    /// Wakes a writer that is waiting out the persist delay.
    rush: Notify,
    /// Completed writes (diagnostics and tests).
    writes: AtomicU64,
}

impl PersistSignal {
    fn mark(&self) {
        self.dirty.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Like [`PersistSignal::mark`], but the save does not wait for the
    /// persist delay.
    fn mark_urgent(&self) {
        self.urgent.store(true, Ordering::Release);
        self.mark();
        self.rush.notify_waiters();
    }
}

struct Persist {
    path: PathBuf,
    signal: Arc<PersistSignal>,
    /// Serializes saves so a newer snapshot is never overtaken by an older one.
    /// The blocking write itself holds the guard, so a caller that stops
    /// waiting (a dropped `flush`) cannot let the next save overtake it.
    serial: Arc<tokio::sync::Mutex<()>>,
    /// Last background save error, to report each distinct failure once.
    last_error: Mutex<Option<String>>,
}

struct State {
    settings: TransferSettings,
    /// Tasks by creation sequence (the listing order).
    entries: BTreeMap<u64, Entry>,
    ids: HashMap<String, u64>,
    /// Queued tasks by account, oldest first.
    queued: BTreeMap<String, BTreeSet<u64>>,
    /// Account that got the last free slot (round-robin cursor).
    last_account: Option<String>,
    /// Tasks holding a slot (state `running`).
    running: usize,
    /// Runs and cleanups that have not finished yet.
    jobs: usize,
    next_seq: u64,
    next_job: u64,
    closed: bool,
    subscribers: Vec<mpsc::UnboundedSender<QueueEvent>>,
    rng: SplitMix64,
    persist: Option<Arc<PersistSignal>>,
    resume_hooks: Option<ChangeHooks>,
}

impl State {
    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(new_error(
                ErrorKind::Conflict,
                "the transfer queue has been shut down",
            ))
        } else {
            Ok(())
        }
    }

    fn seq_of(&self, id: &str) -> Result<u64> {
        self.ids.get(id).copied().ok_or_else(|| missing_task(id))
    }

    fn status(&self, seq: u64, id: &str) -> Result<Status> {
        self.status_of(seq).ok_or_else(|| missing_task(id))
    }

    fn status_of(&self, seq: u64) -> Option<Status> {
        self.entries.get(&seq).map(Entry::status)
    }

    /// Sequence numbers of the tasks whose status matches, in order.
    fn seqs_with(&self, wanted: impl Fn(Status) -> bool) -> Vec<u64> {
        self.entries
            .iter()
            .filter(|(_, entry)| wanted(entry.status()))
            .map(|(seq, _)| *seq)
            .collect()
    }

    fn fresh_id(&self) -> String {
        loop {
            let id = uuid::Uuid::new_v4().to_string();
            if !self.ids.contains_key(&id) {
                return id;
            }
        }
    }

    fn next_job(&mut self) -> u64 {
        self.next_job = self.next_job.wrapping_add(1);
        self.next_job
    }

    fn insert(&mut self, task: TransferTask, resume: Option<Value>) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ids.insert(task.id.clone(), seq);
        let resume = ResumeState::with_hooks(resume, self.resume_hooks.clone());
        self.entries.insert(
            seq,
            Entry {
                task,
                resume,
                activity: Activity::Idle,
                retries_used: 0,
                cancel_pending: false,
            },
        );
        seq
    }

    fn mark_dirty(&self) {
        if let Some(signal) = &self.persist {
            signal.mark();
        }
    }

    /// Stamps the task, sends `Updated` and schedules a save.
    fn publish(&mut self, seq: u64) {
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.task.updated_at = Utc::now();
            send(&mut self.subscribers, || {
                QueueEvent::Updated(entry.task.clone())
            });
        }
        self.mark_dirty();
    }

    fn set_state(&mut self, seq: u64, state: TransferState, error: Option<String>) {
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.task.state = state;
            entry.task.error = error;
            entry.task.speed = 0;
            entry.task.eta = None;
        }
        self.publish(seq);
    }

    /// Back to `queued` by a user action: fresh retry budget, no error, and
    /// no leftover cancel intent.
    fn requeue(&mut self, seq: u64) {
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.retries_used = 0;
            entry.cancel_pending = false;
        }
        self.add_queued(seq);
        self.set_state(seq, TransferState::Queued, None);
    }

    fn add_queued(&mut self, seq: u64) {
        if let Some(entry) = self.entries.get(&seq) {
            self.queued
                .entry(entry.task.account_id.clone())
                .or_default()
                .insert(seq);
        }
    }

    fn remove_queued(&mut self, seq: u64) {
        let Some(entry) = self.entries.get(&seq) else {
            return;
        };
        let account = entry.task.account_id.as_str();
        if let Some(set) = self.queued.get_mut(account) {
            set.remove(&seq);
            if set.is_empty() {
                self.queued.remove(account);
            }
        }
    }

    /// The oldest queued task of the account after the last served one
    /// (round robin across accounts, first in first out within one).
    fn pick_next(&mut self) -> Option<u64> {
        let after_last = self.last_account.as_deref().and_then(|last| {
            self.queued
                .range::<str, _>((Bound::Excluded(last), Bound::Unbounded))
                .next()
                .map(|(account, _)| account.clone())
        });
        let account = after_last.or_else(|| self.queued.keys().next().cloned())?;
        let set = self.queued.get_mut(&account)?;
        let seq = set.pop_first()?;
        if set.is_empty() {
            self.queued.remove(&account);
        }
        self.last_account = Some(account);
        Some(seq)
    }

    fn request_stop(&self, seq: u64, reason: StopReason) {
        if let Some(Entry {
            activity: Activity::Running(run),
            ..
        }) = self.entries.get(&seq)
        {
            run.stop.request(reason);
        }
    }

    /// Asks a running task to cancel and records the intent (saved soon, so
    /// it survives a shutdown that cuts the cleanup short).
    fn request_cancel(&mut self, seq: u64) {
        self.request_stop(seq, StopReason::Cancel);
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.cancel_pending = true;
        }
        self.mark_dirty();
    }

    /// The pending stop request of run `job`, or `None` if that run is over.
    fn stop_request(&self, seq: u64, job: u64) -> Option<Option<StopReason>> {
        self.entries
            .get(&seq)
            .and_then(|entry| entry.run(job))
            .map(|run| run.stop.requested())
    }
}

struct Shared {
    state: Mutex<State>,
    runners: Runners,
    limiter: BandwidthLimiter,
    options: QueueOptions,
    runtime: Handle,
    persist: Option<Persist>,
    /// Notified when the last run or cleanup finishes.
    idle: Notify,
    /// Becomes `true` when shutdown starts.
    closing: watch::Sender<bool>,
    cleanup_permits: Arc<Semaphore>,
    shutdown_outcome: OnceCell<Option<(ErrorKind, String)>>,
}

enum AfterAttempt {
    Finished,
    Cleanup,
    Retry(Duration),
}

enum NextAttempt {
    /// Run again with this task snapshot and these settings.
    Run(Box<(TransferTask, TransferSettings)>),
    Finished,
    Cleanup,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Starts queued tasks while slots are free.
    fn schedule(self: &Arc<Self>, st: &mut State) {
        if st.closed {
            return;
        }
        let cap = usize::try_from(st.settings.max_concurrent_tasks).unwrap_or(usize::MAX);
        while st.running < cap {
            let Some(seq) = st.pick_next() else {
                break;
            };
            self.start_run(st, seq);
        }
    }

    fn start_run(self: &Arc<Self>, st: &mut State, seq: u64) {
        let job = st.next_job();
        let settings = st.settings.clone();
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        let Some(runner) = self.runners.get(entry.task.kind) else {
            let message = format!(
                "no runner is registered for {} tasks",
                kind_name(entry.task.kind)
            );
            st.set_state(seq, TransferState::Failed, Some(message));
            return;
        };
        entry.task.state = TransferState::Running;
        entry.task.attempts = entry.task.attempts.saturating_add(1);
        entry.task.error = None;
        entry.task.speed = 0;
        entry.task.eta = None;
        let (stop, signal) = StopController::new();
        let progress = Progress::new(entry.task.bytes_done, entry.task.bytes_total);
        let driver = Driver {
            shared: Arc::clone(self),
            seq,
            job,
            runner,
            stop: signal,
            progress: progress.clone(),
            resume: entry.resume.clone(),
        };
        let handle = self.runtime.spawn(driver.run(entry.task.clone(), settings));
        entry.activity = Activity::Running(RunControl {
            job,
            stop,
            progress,
            abort: handle.abort_handle(),
        });
        st.running += 1;
        st.jobs += 1;
        st.publish(seq);
    }

    /// Runs the cleanup of a task that is not running, then cancels it.
    fn start_cleanup(self: &Arc<Self>, st: &mut State, seq: u64) {
        st.remove_queued(seq);
        let job = st.next_job();
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        let runner = self.runners.get(entry.task.kind);
        let task = entry.task.clone();
        let resume = entry.resume.get();
        let shared = Arc::clone(self);
        let permits = Arc::clone(&self.cleanup_permits);
        let limit = self.options.cleanup_timeout;
        let handle = self.runtime.spawn(async move {
            let _guard = JobGuard {
                shared: Arc::clone(&shared),
                seq,
                job,
            };
            // The semaphore is never closed; without a permit the cleanup
            // simply runs unthrottled.
            let _permit = permits.acquire_owned().await.ok();
            let result = match runner {
                Some(runner) => run_cleanup(runner.as_ref(), task, resume, limit).await,
                None => Ok(()),
            };
            shared.finish_cancel(seq, job, result);
        });
        entry.activity = Activity::Cancelling {
            job,
            abort: handle.abort_handle(),
        };
        entry.cancel_pending = true;
        st.jobs += 1;
        st.mark_dirty();
    }

    /// Decides what follows a finished (or force-stopped, `None`) attempt.
    fn after_attempt(
        self: &Arc<Self>,
        seq: u64,
        job: u64,
        outcome: Option<Result<()>>,
        skipped: Option<String>,
    ) -> AfterAttempt {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(requested) = st.stop_request(seq, job) else {
            return AfterAttempt::Finished;
        };
        match (outcome, requested) {
            (Some(Ok(())), _) => {
                match skipped {
                    Some(reason) => self.finish_run(st, seq, TransferState::Skipped, Some(reason)),
                    None => self.finish_run(st, seq, TransferState::Completed, None),
                }
                AfterAttempt::Finished
            }
            (_, Some(StopReason::Cancel)) => AfterAttempt::Cleanup,
            (_, Some(StopReason::Pause)) | (None, None) => {
                self.finish_run(st, seq, TransferState::Stopped, None);
                AfterAttempt::Finished
            }
            (Some(Err(error)), None) => {
                let budget = st.settings.retry_attempts;
                let closed = st.closed;
                let message = error.to_string();
                let Some(entry) = st.entries.get_mut(&seq) else {
                    return AfterAttempt::Finished;
                };
                if !closed && is_retryable(error.kind()) && entry.retries_used < budget {
                    entry.retries_used += 1;
                    let delay = self.options.backoff.delay(entry.retries_used, &mut st.rng);
                    entry.sync_progress();
                    entry.task.error = Some(message);
                    entry.task.speed = 0;
                    entry.task.eta = None;
                    st.publish(seq);
                    AfterAttempt::Retry(delay)
                } else {
                    self.finish_run(st, seq, TransferState::Failed, Some(message));
                    AfterAttempt::Finished
                }
            }
        }
    }

    /// After a backoff: run again, or honour a stop that arrived meanwhile.
    fn next_attempt(self: &Arc<Self>, seq: u64, job: u64) -> NextAttempt {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(requested) = st.stop_request(seq, job) else {
            return NextAttempt::Finished;
        };
        match requested {
            Some(StopReason::Cancel) => NextAttempt::Cleanup,
            Some(StopReason::Pause) => {
                self.finish_run(st, seq, TransferState::Stopped, None);
                NextAttempt::Finished
            }
            None => {
                let settings = st.settings.clone();
                let Some(entry) = st.entries.get_mut(&seq) else {
                    return NextAttempt::Finished;
                };
                entry.task.attempts = entry.task.attempts.saturating_add(1);
                entry.task.error = None;
                let task = entry.task.clone();
                st.publish(seq);
                NextAttempt::Run(Box::new((task, settings)))
            }
        }
    }

    /// Ends the current run of a task with `state` and frees its slot.
    fn finish_run(
        self: &Arc<Self>,
        st: &mut State,
        seq: u64,
        state: TransferState,
        error: Option<String>,
    ) {
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        entry.sync_progress();
        let was_running = matches!(
            std::mem::replace(&mut entry.activity, Activity::Idle),
            Activity::Running(_)
        );
        if matches!(
            state,
            TransferState::Completed | TransferState::Cancelled | TransferState::Skipped
        ) {
            // A cancel that lost the race against completion is moot.
            entry.resume.clear();
            entry.cancel_pending = false;
        }
        st.set_state(seq, state, error);
        if was_running {
            self.release(st, true);
        }
        self.schedule(st);
    }

    /// Ends a cancellation (run `job` or cleanup job `job`): `cancelled` when
    /// the cleanup succeeded, otherwise `failed` with everything kept.
    ///
    /// A failed cleanup (an error, or the cleanup timeout) means that
    /// something may be left behind, such as an open multipart upload, and
    /// the resume state is the only record of it. So the state is kept and
    /// so is the cancel intent (saved with the task, which makes the next
    /// open repeat the cleanup), and the task stays `failed`, where
    /// `cancel_task` is accepted again.
    fn finish_cancel(self: &Arc<Self>, seq: u64, job: u64, result: Result<()>) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        let was_running = match &entry.activity {
            Activity::Running(run) if run.job == job => true,
            Activity::Cancelling { job: current, .. } if *current == job => false,
            _ => return,
        };
        entry.sync_progress();
        entry.activity = Activity::Idle;
        match result {
            Ok(()) => {
                entry.cancel_pending = false;
                entry.resume.clear();
                st.set_state(seq, TransferState::Cancelled, None);
            }
            Err(error) => {
                let message = format!(
                    "cleanup after cancelling failed: {error}. The task was kept so that cancelling it again repeats the cleanup; it is also repeated when the app starts"
                );
                st.set_state(seq, TransferState::Failed, Some(message));
            }
        }
        self.release(st, was_running);
        self.schedule(st);
    }

    /// A run or cleanup job ended without finishing normally (its task was
    /// dropped or panicked). Frees what it held; no-op if it already finished.
    fn abandon_job(self: &Arc<Self>, seq: u64, job: u64, panicked: bool) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        match &entry.activity {
            Activity::Running(run) if run.job == job => {
                let (state, error) = if panicked {
                    (
                        TransferState::Failed,
                        Some("internal error: the transfer stopped unexpectedly".to_owned()),
                    )
                } else {
                    (TransferState::Stopped, None)
                };
                self.finish_run(st, seq, state, error);
            }
            Activity::Cancelling { job: current, .. } if *current == job => {
                // The cancel intent stays recorded; a queued task left the
                // queued index when its cancel started, so it cannot stay
                // `queued`.
                entry.activity = Activity::Idle;
                let was_queued = entry.task.state == TransferState::Queued;
                self.release(st, false);
                if was_queued {
                    st.set_state(seq, TransferState::Stopped, None);
                }
            }
            _ => {}
        }
    }

    /// Bookkeeping when a run (`was_running`) or cleanup job ends.
    fn release(&self, st: &mut State, was_running: bool) {
        if was_running {
            st.running = st.running.saturating_sub(1);
        }
        st.jobs = st.jobs.saturating_sub(1);
        if st.jobs == 0 {
            self.idle.notify_waiters();
        }
    }

    /// Publishes the progress of run `job` if it changed.
    fn publish_progress(&self, seq: u64, job: u64, meter: &mut Meter) {
        let (done, total, speed, eta) = meter.sample();
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(entry) = st.entries.get_mut(&seq) else {
            return;
        };
        if entry.run(job).is_none() {
            return;
        }
        let task = &mut entry.task;
        if task.bytes_done == done
            && task.bytes_total == total
            && task.speed == speed
            && task.eta == eta
        {
            return;
        }
        task.bytes_done = done;
        task.bytes_total = total;
        task.speed = speed;
        task.eta = eta;
        task.updated_at = Utc::now();
        send(&mut st.subscribers, || QueueEvent::Updated(task.clone()));
    }

    fn task_snapshot(&self, seq: u64) -> Option<TransferTask> {
        self.lock()
            .entries
            .get(&seq)
            .map(|entry| entry.task.clone())
    }

    fn emit(&self, event: QueueEvent) {
        send(&mut self.lock().subscribers, || event);
    }

    /// Writes the queue file if anything changed since the last write.
    async fn save(&self) -> Result<()> {
        let Some(persist) = &self.persist else {
            return Ok(());
        };
        let serial = Arc::clone(&persist.serial).lock_owned().await;
        if !persist.signal.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let snapshot: Vec<StoredTask> = self
            .lock()
            .entries
            .values()
            .map(|entry| StoredTask {
                task: entry.task.clone(),
                resume: entry.resume.get(),
                cancel_pending: entry.cancel_pending,
            })
            .collect();
        let path = persist.path.clone();
        let signal = Arc::clone(&persist.signal);
        // The write owns the serial guard and records its own outcome, so it
        // stays ordered and accounted for even if this future is dropped.
        let write = tokio::task::spawn_blocking(move || {
            let _serial = serial;
            let result = store::save(&path, &snapshot);
            match &result {
                Ok(()) => {
                    signal.writes.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => signal.dirty.store(true, Ordering::Release),
            }
            result
        });
        let result = match write.await {
            Ok(result) => result,
            Err(join_error) => {
                persist.signal.dirty.store(true, Ordering::Release);
                Err(new_error(
                    ErrorKind::Internal,
                    format!("saving the transfer queue failed: {join_error}"),
                ))
            }
        };
        if result.is_ok() {
            *lock(&persist.last_error) = None;
        }
        result
    }

    /// A save started by the background writer; failures become events.
    async fn save_in_background(&self) {
        let Some(persist) = &self.persist else {
            return;
        };
        if let Err(error) = self.save().await {
            let message = error.to_string();
            let is_new = {
                let mut last = lock(&persist.last_error);
                let is_new = last.as_deref() != Some(message.as_str());
                *last = Some(message.clone());
                is_new
            };
            if is_new {
                self.emit(QueueEvent::PersistFailed(message));
            }
        }
    }

    /// First, synchronous part of shutdown: refuse new work and ask every run
    /// to pause. Idempotent.
    fn begin_shutdown(&self) {
        {
            let mut guard = self.lock();
            let st = &mut *guard;
            if st.closed {
                return;
            }
            st.closed = true;
            for seq in st.seqs_with(|status| {
                matches!(
                    status,
                    Status::Idle(TransferState::Queued) | Status::Running(None)
                )
            }) {
                if st.status_of(seq) == Some(Status::Running(None)) {
                    st.request_stop(seq, StopReason::Pause);
                } else {
                    st.set_state(seq, TransferState::Stopped, None);
                }
            }
            st.queued.clear();
        }
        self.closing.send_replace(true);
    }

    async fn shutdown(self: &Arc<Self>) -> Result<()> {
        let outcome = self
            .shutdown_outcome
            .get_or_init(|| async {
                self.begin_shutdown();
                let _ = tokio::time::timeout(self.options.stop_grace, self.wait_idle()).await;
                self.abort_leftovers();
                self.save()
                    .await
                    .err()
                    .map(|error| (error.kind(), error.to_string()))
            })
            .await;
        // Errors are not `Clone`, so every caller gets a rebuilt copy.
        match outcome {
            None => Ok(()),
            Some((ErrorKind::Io, message)) => Err(io_error(
                "the final save of the transfer queue failed",
                std::io::Error::other(message.clone()),
            )),
            Some((kind, message)) => Err(new_error(*kind, message.clone())),
        }
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock().jobs == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Ends runs and cleanups that outlived the shutdown grace period: runs
    /// end as `stopped` (resume state kept), unfinished cleanups leave their
    /// task as it was so it can be cancelled again later.
    fn abort_leftovers(&self) {
        let mut aborts = Vec::new();
        {
            let mut guard = self.lock();
            let st = &mut *guard;
            let active = st.seqs_with(|status| !matches!(status, Status::Idle(_)));
            for seq in active {
                let Some(entry) = st.entries.get_mut(&seq) else {
                    continue;
                };
                entry.sync_progress();
                match std::mem::replace(&mut entry.activity, Activity::Idle) {
                    Activity::Running(run) => {
                        aborts.push(run.abort);
                        st.set_state(seq, TransferState::Stopped, None);
                        self.release(st, true);
                    }
                    Activity::Cancelling { abort, .. } => {
                        aborts.push(abort);
                        self.release(st, false);
                        // Nothing runs after shutdown: a queued task whose
                        // cancellation did not finish is stopped, as it would
                        // be after a reload.
                        if st.status_of(seq) == Some(Status::Idle(TransferState::Queued)) {
                            st.set_state(seq, TransferState::Stopped, None);
                        }
                    }
                    Activity::Idle => {}
                }
            }
        }
        for abort in aborts {
            abort.abort();
        }
    }
}

/// Ends a job's bookkeeping when its task is dropped or panics before the job
/// finished normally (a no-op otherwise).
struct JobGuard {
    shared: Arc<Shared>,
    seq: u64,
    job: u64,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.shared
            .abandon_job(self.seq, self.job, std::thread::panicking());
    }
}

/// Runs a runner method, turning a panic into an `internal` error.
async fn guarded<'a>(start: impl FnOnce() -> BoxFuture<'a, Result<()>>) -> Result<()> {
    let future = match std::panic::catch_unwind(AssertUnwindSafe(start)) {
        Ok(future) => future,
        Err(panic) => return Err(panic_error(panic.as_ref())),
    };
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => Err(panic_error(panic.as_ref())),
    }
}

fn panic_error(panic: &(dyn std::any::Any + Send)) -> Error {
    let detail = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no details".to_owned());
    new_error(
        ErrorKind::Internal,
        format!("the transfer runner panicked: {detail}"),
    )
}

/// `start + wait`, or a far-future instant if that does not fit.
fn deadline(start: Instant, wait: Duration) -> Instant {
    start
        .checked_add(wait)
        .unwrap_or_else(|| start + FAR_FUTURE)
}

/// Runs a runner's cleanup within `limit`; panics and timeouts become errors
/// recorded on the cancelled task.
async fn run_cleanup(
    runner: &dyn TaskRunner,
    task: TransferTask,
    resume: Option<Value>,
    limit: Duration,
) -> Result<()> {
    match tokio::time::timeout(limit, guarded(|| runner.cleanup(task, resume))).await {
        Ok(result) => result,
        Err(_) => Err(new_error(
            ErrorKind::Remote,
            format!("the cleanup did not finish within {limit:?}"),
        )),
    }
}

/// Samples a run's progress counters for events.
struct Meter {
    progress: Progress,
    estimator: RateEstimator,
    interval: Interval,
}

impl Meter {
    fn new(progress: Progress, period: Duration) -> Self {
        let start = Instant::now();
        let mut interval = tokio::time::interval_at(deadline(start, period), period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut estimator = RateEstimator::new();
        estimator.record(start, progress.done());
        Self {
            progress,
            estimator,
            interval,
        }
    }

    async fn tick(&mut self) {
        self.interval.tick().await;
    }

    fn sample(&mut self) -> (u64, Option<u64>, u64, Option<u64>) {
        let now = Instant::now();
        let done = self.progress.done();
        let total = self.progress.total();
        self.estimator.record(now, done);
        let speed = self.estimator.bytes_per_sec(now);
        (done, total, speed, eta_secs(done, total, speed))
    }
}

/// Executes one task: attempts, backoff between them, cleanup on cancel.
struct Driver {
    shared: Arc<Shared>,
    seq: u64,
    job: u64,
    runner: Arc<dyn TaskRunner>,
    stop: StopSignal,
    progress: Progress,
    resume: ResumeState,
}

impl Driver {
    async fn run(self, mut task: TransferTask, mut settings: TransferSettings) {
        let _guard = JobGuard {
            shared: Arc::clone(&self.shared),
            seq: self.seq,
            job: self.job,
        };
        let mut meter = Meter::new(self.progress.clone(), self.shared.options.progress_interval);
        loop {
            let context = TaskContext::new(
                task,
                settings,
                self.progress.clone(),
                self.shared.limiter.clone(),
                self.stop.clone(),
                self.resume.clone(),
            );
            let skip = context.skip_slot();
            let outcome = self.attempt(context, &mut meter).await;
            let skipped = skip.take();
            let delay = match self
                .shared
                .after_attempt(self.seq, self.job, outcome, skipped)
            {
                AfterAttempt::Finished => return,
                AfterAttempt::Cleanup => return self.cleanup().await,
                AfterAttempt::Retry(delay) => delay,
            };
            self.backoff(delay, &mut meter).await;
            match self.shared.next_attempt(self.seq, self.job) {
                NextAttempt::Run(next) => (task, settings) = *next,
                NextAttempt::Finished => return,
                NextAttempt::Cleanup => return self.cleanup().await,
            }
        }
    }

    /// One run of the runner. `None` when it ignored a stop request for the
    /// whole grace period and was dropped.
    async fn attempt(&self, context: TaskContext, meter: &mut Meter) -> Option<Result<()>> {
        // A pause or cancel that arrived before the run began: do not start
        // the runner at all (no side effects such as a new multipart upload).
        if self.stop.is_requested() {
            return Some(self.stop.check());
        }
        let run = guarded(|| self.runner.run(context));
        tokio::pin!(run);
        let grace = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(grace);
        let mut stopping = false;
        loop {
            tokio::select! {
                biased;
                result = &mut run => return Some(result),
                _ = self.stop.wait(), if !stopping => {
                    stopping = true;
                    let grace_end = deadline(Instant::now(), self.shared.options.stop_grace);
                    grace.as_mut().reset(grace_end);
                }
                () = &mut grace, if stopping => return None,
                () = meter.tick() => self.shared.publish_progress(self.seq, self.job, meter),
            }
        }
    }

    /// Waits before the next attempt; returns early on a stop request.
    async fn backoff(&self, delay: Duration, meter: &mut Meter) {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                _ = self.stop.wait() => return,
                () = &mut sleep => return,
                () = meter.tick() => self.shared.publish_progress(self.seq, self.job, meter),
            }
        }
    }

    async fn cleanup(self) {
        let result = match self.shared.task_snapshot(self.seq) {
            Some(task) => {
                let limit = self.shared.options.cleanup_timeout;
                run_cleanup(self.runner.as_ref(), task, self.resume.get(), limit).await
            }
            None => Ok(()),
        };
        self.shared.finish_cancel(self.seq, self.job, result);
    }
}

/// Saves the queue shortly after changes until shutdown starts: after the
/// persist delay, so that changes share one save, or at once when a change
/// is urgent.
async fn run_writer(shared: Arc<Shared>, signal: Arc<PersistSignal>) {
    let mut closing = shared.closing.subscribe();
    loop {
        tokio::select! {
            () = signal.notify.notified() => {}
            () = closed(&mut closing) => return,
        }
        // Registered before the flag is read, so an urgent change cannot
        // slip in between and leave this waiting out the delay.
        let rushed = signal.rush.notified();
        tokio::pin!(rushed);
        rushed.as_mut().enable();
        if !signal.urgent.load(Ordering::Acquire) {
            tokio::select! {
                () = tokio::time::sleep(shared.options.persist_delay) => {}
                () = rushed => {}
                () = closed(&mut closing) => return,
            }
        }
        signal.urgent.store(false, Ordering::Release);
        shared.save_in_background().await;
    }
}

async fn closed(closing: &mut watch::Receiver<bool>) {
    let _ = closing.wait_for(|closed| *closed).await;
}
