//! The seam between the queue and the transports.
//!
//! A [`TaskRunner`] executes one run of a task. The queue picks the runner by
//! [`TransferKind`] from [`Runners`] and passes a [`TaskContext`] holding:
//!
//! * a snapshot of the task (`account_id` selects the S3 client) and of the
//!   transfer settings at the start of the run,
//! * a [`Progress`] reporter whose values the queue publishes at most every
//!   250 ms,
//! * the shared [`BandwidthLimiter`],
//! * a [`StopSignal`] that tells pause apart from cancel,
//! * the persisted [`ResumeState`] that survives retries, pauses and restarts.
//!
//! See `docs/transfers.md` for the full runner contract.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::watch;

use super::limiter::BandwidthLimiter;
use super::new_error;
use crate::error::{ErrorKind, Result};
use crate::model::{TransferKind, TransferTask};
use crate::settings::TransferSettings;

/// Executes tasks of one or more [`TransferKind`]s.
///
/// Implementations must be cheap to share (`Arc<dyn TaskRunner>`). Both
/// methods return boxed futures so the trait stays object safe.
pub trait TaskRunner: Send + Sync {
    /// Runs the task once.
    ///
    /// * `Ok(())` means the transfer is complete; the task becomes `completed`.
    /// * When [`TaskContext::stop`] fires, return promptly with any error
    ///   (preferably from [`StopSignal::check`]); the queue then marks the task
    ///   `stopped` or cancels it. Runners that do not return within the
    ///   queue's grace period are dropped.
    /// * Any other error fails the run; `remote` and `io` errors are retried
    ///   automatically with backoff, continuing from the [`ResumeState`].
    fn run(&self, ctx: TaskContext) -> BoxFuture<'_, Result<()>>;

    /// Releases what a cancelled task left behind (abort a multipart upload,
    /// delete a partial file). Called once per cancellation, after any run of
    /// the task has ended, with the last persisted resume state (`None` if the
    /// task never saved one). If a shutdown cuts the cleanup short, it is
    /// called again when the queue is next opened, so it must be idempotent.
    ///
    /// An error (or exceeding the queue's cleanup timeout) means that
    /// something may still be left behind: the task does not become
    /// `cancelled`. It ends `failed` with the error, keeps its resume state
    /// (the only record of what is left) and its cancellation is repeated,
    /// calling this method again, when the task is cancelled again and when
    /// the queue is next opened. Succeed (return `Ok`) for what no longer
    /// exists, and fail only when the cleanup could not be finished.
    fn cleanup(&self, task: TransferTask, resume: Option<Value>) -> BoxFuture<'_, Result<()>> {
        let _ = (task, resume);
        Box::pin(async { Ok(()) })
    }
}

const KIND_COUNT: usize = 5;
const ALL_KINDS: [TransferKind; KIND_COUNT] = [
    TransferKind::Upload,
    TransferKind::Download,
    TransferKind::Copy,
    TransferKind::Move,
    TransferKind::Delete,
];

fn slot(kind: TransferKind) -> usize {
    match kind {
        TransferKind::Upload => 0,
        TransferKind::Download => 1,
        TransferKind::Copy => 2,
        TransferKind::Move => 3,
        TransferKind::Delete => 4,
    }
}

/// The runner registered for each [`TransferKind`].
#[derive(Clone, Default)]
pub struct Runners {
    slots: [Option<Arc<dyn TaskRunner>>; KIND_COUNT],
}

impl Runners {
    /// No runners registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same runner for every kind (it can dispatch on `task().kind`).
    pub fn for_all(runner: Arc<dyn TaskRunner>) -> Self {
        Self {
            slots: std::array::from_fn(|_| Some(Arc::clone(&runner))),
        }
    }

    /// Registers `runner` for `kind`, replacing any previous one.
    pub fn with(mut self, kind: TransferKind, runner: Arc<dyn TaskRunner>) -> Self {
        self.set(kind, runner);
        self
    }

    /// Registers `runner` for `kind`, replacing any previous one.
    pub fn set(&mut self, kind: TransferKind, runner: Arc<dyn TaskRunner>) {
        self.slots[slot(kind)] = Some(runner);
    }

    /// The runner for `kind`, if any.
    pub fn get(&self, kind: TransferKind) -> Option<Arc<dyn TaskRunner>> {
        self.slots[slot(kind)].clone()
    }

    /// Whether a runner is registered for `kind`.
    pub fn contains(&self, kind: TransferKind) -> bool {
        self.slots[slot(kind)].is_some()
    }
}

impl fmt::Debug for Runners {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kinds: Vec<TransferKind> = ALL_KINDS
            .into_iter()
            .filter(|kind| self.contains(*kind))
            .collect();
        f.debug_struct("Runners").field("kinds", &kinds).finish()
    }
}

/// Everything a runner gets for one run of a task.
pub struct TaskContext {
    task: TransferTask,
    settings: TransferSettings,
    progress: Progress,
    limiter: BandwidthLimiter,
    stop: StopSignal,
    resume: ResumeState,
}

impl TaskContext {
    /// Assembles a context; the queue does this for every run, and runner
    /// tests can do it directly.
    pub fn new(
        task: TransferTask,
        settings: TransferSettings,
        progress: Progress,
        limiter: BandwidthLimiter,
        stop: StopSignal,
        resume: ResumeState,
    ) -> Self {
        Self {
            task,
            settings,
            progress,
            limiter,
            stop,
            resume,
        }
    }

    /// The task as it was when this run started (`state` is `running`,
    /// `attempts` counts this run).
    pub fn task(&self) -> &TransferTask {
        &self.task
    }

    /// Transfer settings at the start of this run.
    pub fn settings(&self) -> &TransferSettings {
        &self.settings
    }

    /// Where the run reports bytes (objects for delete tasks) done and total.
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// The limiter shared by all tasks of the queue.
    pub fn limiter(&self) -> &BandwidthLimiter {
        &self.limiter
    }

    /// Tells the run to pause or cancel; it must return promptly when set.
    pub fn stop(&self) -> &StopSignal {
        &self.stop
    }

    /// The task's persisted resume state, shared with later runs.
    pub fn resume(&self) -> &ResumeState {
        &self.resume
    }
}

impl fmt::Debug for TaskContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskContext")
            .field("task", &self.task.id)
            .field("progress", &self.progress)
            .field("stop", &self.stop.requested())
            .finish()
    }
}

/// Progress counters of a running task. Cloning shares the counters, so
/// parallel parts can report into the same task.
///
/// For delete tasks the unit is objects instead of bytes.
#[derive(Clone, Default)]
pub struct Progress {
    inner: Arc<ProgressInner>,
}

#[derive(Default)]
struct ProgressInner {
    done: AtomicU64,
    total: Mutex<Option<u64>>,
}

impl Progress {
    /// Counters starting at `done` of `total` (`None` = unknown total).
    pub fn new(done: u64, total: Option<u64>) -> Self {
        Self {
            inner: Arc::new(ProgressInner {
                done: AtomicU64::new(done),
                total: Mutex::new(total),
            }),
        }
    }

    /// Bytes (objects for delete tasks) done so far.
    pub fn done(&self) -> u64 {
        self.inner.done.load(Ordering::Relaxed)
    }

    /// Total size, when known.
    pub fn total(&self) -> Option<u64> {
        *lock(&self.inner.total)
    }

    /// Sets the total size (`None` = unknown).
    pub fn set_total(&self, total: Option<u64>) {
        *lock(&self.inner.total) = total;
    }

    /// Sets the amount done, e.g. at the start of a run from the resume state.
    pub fn set_done(&self, done: u64) {
        self.inner.done.store(done, Ordering::Relaxed);
    }

    /// Adds transferred bytes (saturating).
    pub fn add_done(&self, bytes: u64) {
        self.update_done(|done| done.saturating_add(bytes));
    }

    /// Takes back bytes, e.g. those of a part that failed and will be resent
    /// (saturating at 0).
    pub fn sub_done(&self, bytes: u64) {
        self.update_done(|done| done.saturating_sub(bytes));
    }

    fn update_done(&self, change: impl Fn(u64) -> u64) {
        let counter = &self.inner.done;
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            match counter.compare_exchange_weak(
                current,
                change(current),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Progress")
            .field("done", &self.done())
            .field("total", &self.total())
            .finish()
    }
}

/// Why a run is asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopReason {
    /// Pause, stop all, or shutdown: keep the resume state, the task resumes
    /// later.
    Pause,
    /// Cancel: the task ends and [`TaskRunner::cleanup`] runs afterwards.
    Cancel,
}

/// The sending side of a [`StopSignal`]. A cancel request replaces a pause
/// request; a pause request never replaces a cancel request.
#[derive(Debug)]
pub struct StopController {
    sender: watch::Sender<Option<StopReason>>,
}

impl StopController {
    /// A controller and the signal it drives.
    pub fn new() -> (Self, StopSignal) {
        let (sender, receiver) = watch::channel(None);
        (Self { sender }, StopSignal { receiver })
    }

    /// Requests a stop. Returns whether the request changed anything.
    pub fn request(&self, reason: StopReason) -> bool {
        self.sender.send_if_modified(|current| {
            // A pending cancel stays; a pending pause stays unless upgraded.
            let replace = !matches!(
                (*current, reason),
                (Some(StopReason::Cancel), _) | (Some(StopReason::Pause), StopReason::Pause)
            );
            if replace {
                *current = Some(reason);
            }
            replace
        })
    }

    /// The current request, if any.
    pub fn requested(&self) -> Option<StopReason> {
        *self.sender.borrow()
    }

    /// Another signal driven by this controller.
    pub fn signal(&self) -> StopSignal {
        StopSignal {
            receiver: self.sender.subscribe(),
        }
    }
}

/// Tells a runner whether, and why, it should stop.
#[derive(Debug, Clone)]
pub struct StopSignal {
    receiver: watch::Receiver<Option<StopReason>>,
}

impl StopSignal {
    /// The current request, if any (non-blocking). Once the controller is
    /// gone this reads as [`StopReason::Pause`] (unless a cancel was
    /// requested before), matching [`StopSignal::wait`].
    pub fn requested(&self) -> Option<StopReason> {
        let current = *self.receiver.borrow();
        match current {
            None if self.receiver.has_changed().is_err() => Some(StopReason::Pause),
            current => current,
        }
    }

    /// Whether a pause or cancel was requested (see [`StopSignal::requested`]).
    pub fn is_requested(&self) -> bool {
        self.requested().is_some()
    }

    /// Completes once a stop is requested (at once if one already was) and
    /// returns its reason. Cancellation safe, so it fits in `tokio::select!`.
    /// If the controller is gone, this completes with [`StopReason::Pause`].
    pub async fn wait(&self) -> StopReason {
        let mut receiver = self.receiver.clone();
        match receiver.wait_for(Option::is_some).await {
            Ok(value) => (*value).unwrap_or(StopReason::Pause),
            Err(_) => StopReason::Pause,
        }
    }

    /// `Err` with kind `cancelled` once a stop was requested, for use with `?`
    /// at convenient points of a runner.
    pub fn check(&self) -> Result<()> {
        match self.requested() {
            None => Ok(()),
            Some(StopReason::Pause) => {
                Err(new_error(ErrorKind::Cancelled, "the transfer was paused"))
            }
            Some(StopReason::Cancel) => Err(new_error(
                ErrorKind::Cancelled,
                "the transfer was cancelled",
            )),
        }
    }
}

/// Called after a change of a [`ResumeState`]; the queue uses it to schedule
/// a save. It must not block or take the queue lock.
pub(super) type ChangeHook = Arc<dyn Fn() + Send + Sync>;

/// How a [`ResumeState`] reaches the queue.
#[derive(Clone)]
pub(super) struct ChangeHooks {
    /// Called after every change: save after the persist delay.
    pub(super) changed: ChangeHook,
    /// Called by [`ResumeState::persist_now`]: save at once.
    pub(super) urgent: ChangeHook,
}

/// Opaque, persisted state a runner keeps so that a later run continues
/// instead of starting over (for example a multipart upload id and the
/// finished parts, or a temp file, offset and ETag of a download).
///
/// The queue stores it with the task, passes it to every later run of the
/// task (retries, resumes, after an app restart) and to
/// [`TaskRunner::cleanup`], and clears it when the task completes or is
/// cancelled. Updates are saved to disk shortly after they are made.
#[derive(Clone)]
pub struct ResumeState {
    inner: Arc<ResumeInner>,
}

struct ResumeInner {
    value: Mutex<Option<Value>>,
    hooks: Option<ChangeHooks>,
}

impl ResumeState {
    /// A detached state (not saved anywhere), for runner tests.
    pub fn new(initial: Option<Value>) -> Self {
        Self::with_hooks(initial, None)
    }

    pub(super) fn with_hooks(initial: Option<Value>, hooks: Option<ChangeHooks>) -> Self {
        Self {
            inner: Arc::new(ResumeInner {
                value: Mutex::new(initial),
                hooks,
            }),
        }
    }

    /// A copy of the current value.
    pub fn get(&self) -> Option<Value> {
        lock(&self.inner.value).clone()
    }

    /// The current value deserialized as `T`.
    pub fn get_as<T: DeserializeOwned>(&self) -> std::result::Result<Option<T>, serde_json::Error> {
        match lock(&self.inner.value).as_ref() {
            None => Ok(None),
            Some(value) => T::deserialize(value).map(Some),
        }
    }

    /// Replaces the value.
    pub fn set(&self, value: Value) {
        *lock(&self.inner.value) = Some(value);
        self.changed();
    }

    /// Replaces the value with `value` serialized to JSON.
    pub fn set_as<T: Serialize + ?Sized>(
        &self,
        value: &T,
    ) -> std::result::Result<(), serde_json::Error> {
        let value = serde_json::to_value(value)?;
        self.set(value);
        Ok(())
    }

    /// Changes the value in place, atomically with respect to other users of
    /// this state (useful when parallel parts record their completion).
    pub fn update<R>(&self, change: impl FnOnce(&mut Option<Value>) -> R) -> R {
        let result = change(&mut lock(&self.inner.value));
        self.changed();
        result
    }

    /// Removes the value.
    pub fn clear(&self) {
        let had_value = lock(&self.inner.value).take().is_some();
        if had_value {
            self.changed();
        }
    }

    /// Asks the queue to save its file at once, instead of after the
    /// persist delay (1 s by default), including the current value.
    ///
    /// Call it right after a change that must survive a crash because the
    /// transport holds something on the server that only this state names:
    /// the ID of a multipart upload that was just created. It returns
    /// immediately; the save runs in the background. Without a queue file it
    /// does nothing.
    pub fn persist_now(&self) {
        if let Some(hooks) = &self.inner.hooks {
            (hooks.urgent)();
        }
    }

    fn changed(&self) {
        if let Some(hooks) = &self.inner.hooks {
            (hooks.changed)();
        }
    }
}

impl fmt::Debug for ResumeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ResumeState").field(&self.get()).finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Checkpoint {
        upload_id: String,
        parts: Vec<u32>,
    }

    #[test]
    fn progress_counts_and_saturates() {
        let progress = Progress::new(10, None);
        let shared = progress.clone();
        shared.add_done(5);
        assert_eq!(progress.done(), 15);
        progress.sub_done(100);
        assert_eq!(progress.done(), 0);
        progress.set_done(u64::MAX - 1);
        progress.add_done(10);
        assert_eq!(progress.done(), u64::MAX);
        assert_eq!(progress.total(), None);
        shared.set_total(Some(42));
        assert_eq!(progress.total(), Some(42));
        assert_eq!(Progress::default().done(), 0);
    }

    #[tokio::test]
    async fn stop_signal_reports_pause_and_cancel() {
        let (controller, signal) = StopController::new();
        assert_eq!(signal.requested(), None);
        assert!(signal.check().is_ok());
        let waiter = {
            let signal = signal.clone();
            tokio::spawn(async move { signal.wait().await })
        };
        tokio::task::yield_now().await;
        assert!(controller.request(StopReason::Pause));
        assert_eq!(waiter.await.unwrap(), StopReason::Pause);
        assert!(
            !controller.request(StopReason::Pause),
            "repeated pause is a no-op"
        );
        assert_eq!(signal.check().unwrap_err().kind(), ErrorKind::Cancelled);
        assert!(
            controller.request(StopReason::Cancel),
            "cancel overrides pause"
        );
        assert!(
            !controller.request(StopReason::Pause),
            "pause never overrides cancel"
        );
        assert_eq!(signal.requested(), Some(StopReason::Cancel));
        assert_eq!(controller.signal().wait().await, StopReason::Cancel);
        assert_eq!(controller.requested(), Some(StopReason::Cancel));
    }

    #[tokio::test]
    async fn stop_signal_without_controller_reads_as_pause() {
        let (controller, signal) = StopController::new();
        drop(controller);
        assert_eq!(signal.wait().await, StopReason::Pause);
        assert_eq!(signal.requested(), Some(StopReason::Pause));
        assert!(signal.is_requested());
        assert_eq!(signal.check().unwrap_err().kind(), ErrorKind::Cancelled);
        // A cancel requested before the controller went away is kept.
        let (controller, signal) = StopController::new();
        controller.request(StopReason::Cancel);
        drop(controller);
        assert_eq!(signal.requested(), Some(StopReason::Cancel));
        assert_eq!(signal.wait().await, StopReason::Cancel);
    }

    #[test]
    fn resume_state_round_trips_typed_values_and_notifies() {
        let changes = Arc::new(AtomicUsize::new(0));
        let urgent = Arc::new(AtomicUsize::new(0));
        let counter = |counter: &Arc<AtomicUsize>| -> ChangeHook {
            let counter = Arc::clone(counter);
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        };
        let state = ResumeState::with_hooks(
            None,
            Some(ChangeHooks {
                changed: counter(&changes),
                urgent: counter(&urgent),
            }),
        );
        assert_eq!(state.get_as::<Checkpoint>().unwrap(), None);
        let checkpoint = Checkpoint {
            upload_id: "u-1".into(),
            parts: vec![1, 2],
        };
        state.set_as(&checkpoint).unwrap();
        assert_eq!(state.get_as::<Checkpoint>().unwrap(), Some(checkpoint));
        let added = state.update(|value| {
            if let Some(Value::Object(map)) = value
                && let Some(Value::Array(parts)) = map.get_mut("parts")
            {
                parts.push(json!(3));
            }
            3
        });
        assert_eq!(added, 3);
        assert_eq!(
            state.get(),
            Some(json!({"upload_id": "u-1", "parts": [1, 2, 3]}))
        );
        assert!(
            state.get_as::<Vec<u32>>().is_err(),
            "shape mismatch is an error"
        );
        state.clear();
        state.clear();
        assert_eq!(state.get(), None);
        assert_eq!(
            changes.load(Ordering::SeqCst),
            3,
            "set, update, one effective clear"
        );
        assert_eq!(urgent.load(Ordering::SeqCst), 0, "nothing was urgent");
        state.persist_now();
        assert_eq!(urgent.load(Ordering::SeqCst), 1);
        assert_eq!(
            changes.load(Ordering::SeqCst),
            3,
            "an urgent save is not a change"
        );
        let detached = ResumeState::new(Some(json!(1)));
        detached.set(json!(2));
        detached.persist_now();
        assert_eq!(detached.get(), Some(json!(2)));
    }

    struct Named;

    impl TaskRunner for Named {
        fn run(&self, _ctx: TaskContext) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn runners_are_registered_per_kind() {
        let runner: Arc<dyn TaskRunner> = Arc::new(Named);
        let runners = Runners::new().with(TransferKind::Upload, Arc::clone(&runner));
        assert!(runners.contains(TransferKind::Upload));
        assert!(!runners.contains(TransferKind::Delete));
        assert!(runners.get(TransferKind::Download).is_none());
        assert_eq!(format!("{runners:?}"), "Runners { kinds: [Upload] }");
        let all = Runners::for_all(runner);
        assert!(ALL_KINDS.into_iter().all(|kind| all.contains(kind)));
    }

    #[tokio::test]
    async fn default_cleanup_succeeds() {
        let task = TransferTask {
            id: "t".into(),
            account_id: "a".into(),
            group_id: None,
            kind: TransferKind::Delete,
            src: crate::model::Location::S3 {
                bucket: "b".into(),
                key: "k".into(),
            },
            dst: None,
            bytes_total: None,
            bytes_done: 0,
            speed: 0,
            eta: None,
            state: crate::model::TransferState::Cancelled,
            error: None,
            attempts: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        assert!(Named.cleanup(task, None).await.is_ok());
    }
}
