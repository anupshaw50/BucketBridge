//! The S3 task runners of the transfer queue: every [`TransferKind`] runs on
//! the engines of this module.
//!
//! * [`S3Runner`] implements [`TaskRunner`]; [`S3Runner::runners`] registers
//!   it for every kind and it dispatches on the task's kind.
//! * The queue's [`BandwidthLimiter`] becomes the engines' [`Throttle`], its
//!   [`QueueProgress`] their [`Progress`], and its stop signal their
//!   [`StopSignal`].
//! * The engines' resume states are stored in the queue's resume state, in
//!   a tagged envelope ([`RunnerState`]).
//!
//! See `docs/engines.md` (section "Transfer queue runners") for the wiring.

use std::future::Future;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::future::{BoxFuture, Either};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

use super::control::{
    Hooks, Progress, StopSignal, StreamFuture, StreamPermit, Throttle, ThrottleFuture,
};
use super::download::{DownloadState, discard_partial};
use super::upload::UploadState;
use super::{TransferEngine, TransferOptions};
use crate::client::STALL_GRACE_PERIOD;
use crate::cse::{self, CseMode, CsePasswords};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{Location, TransferKind, TransferTask};
use crate::ops::S3;
use crate::s3uri::S3Uri;
use crate::settings::{DataIntegritySettings, TransferSettings};
use crate::transfer::{
    BandwidthLimiter, MAX_CHUNK, Progress as QueueProgress, ResumeState, Runners,
    StopSignal as QueueStop, TaskContext, TaskRunner,
};

/// Resolves the S3 transport of an account by its id. The desktop host
/// passes `Session::s3_for`; tests pass a fixed server.
pub type S3Provider = Arc<dyn Fn(String) -> BoxFuture<'static, Result<S3>> + Send + Sync>;

/// Smallest throttle grant (bytes): below it, the per-request overhead would
/// dominate.
const MIN_GRANT: u64 = 256;

/// The longest one round of grants to all streaming bodies may take: a
/// quarter of the stalled-stream grace period, so that no body goes silent
/// for anywhere near the 20 s after which the SDK (and S3 itself) fail it.
const ROUND_BUDGET: Duration = Duration::from_secs(STALL_GRACE_PERIOD.as_secs() / 4);

/// How often a stream that waits for a place looks at the bandwidth cap
/// again (it can be raised while streams wait).
const GATE_RECHECK: Duration = Duration::from_secs(1);

/// The resume state a runner keeps in the queue, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerState {
    /// A multipart upload in progress.
    Upload(UploadState),
    /// A download in progress.
    Download(DownloadState),
    /// A download of one object version. The version belongs to the task, so
    /// every run, including a resumed one, asks for it; `state` is the
    /// engine's last checkpoint, `None` until the first one.
    VersionDownload {
        /// The version to download.
        version_id: String,
        /// The download engine's last checkpoint.
        state: Option<DownloadState>,
    },
    /// An entry that could not be added (see the enqueue helpers): every
    /// run fails with this error, and the cleanup does nothing.
    Rejected {
        /// Error category of the original failure.
        code: ErrorKind,
        /// Why the entry could not be added.
        message: String,
    },
}

/// Borrowing twin of [`RunnerState`], so that a checkpoint is serialized
/// without copying the engine state.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RunnerStateRef<'a> {
    Upload(&'a UploadState),
    Download(&'a DownloadState),
    VersionDownload {
        version_id: &'a str,
        state: Option<&'a DownloadState>,
    },
}

impl RunnerState {
    /// Reads a stored state; anything unreadable counts as no state (the
    /// transfer starts over).
    fn read(value: Option<Value>) -> Option<RunnerState> {
        value.and_then(|value| serde_json::from_value(value).ok())
    }
}

/// The S3 runners of a transfer queue.
///
/// Cheap to clone; clones share the data-integrity settings, which the queue
/// does not hand to runners (they are not part of the transfer settings).
#[derive(Clone)]
pub struct S3Runner {
    provider: S3Provider,
    data_integrity: Arc<Mutex<DataIntegritySettings>>,
    /// Throttle requests waiting right now, across all runs of this runner.
    waiting: Arc<AtomicUsize>,
    /// The bodies that stream right now, across all runs of this runner.
    streams: Arc<StreamGate>,
    /// The passwords of encrypting and decrypting task groups.
    passwords: CsePasswords,
}

impl std::fmt::Debug for S3Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Runner")
            .field("data_integrity", &self.data_integrity())
            .finish_non_exhaustive()
    }
}

impl S3Runner {
    /// Runners that reach each account through `provider`.
    pub fn new(provider: S3Provider, data_integrity: DataIntegritySettings) -> Self {
        S3Runner {
            provider,
            data_integrity: Arc::new(Mutex::new(data_integrity)),
            waiting: Arc::default(),
            streams: Arc::default(),
            passwords: CsePasswords::new(),
        }
    }

    /// These runners, reading the passwords of client-side encryption groups
    /// ([`CseMode`]) from `passwords`: uploads of an encrypting group are
    /// encrypted, downloads of a decrypting group are decrypted. A task of
    /// such a group whose password is unknown (after a restart) fails with
    /// *password not found*; it never runs in the clear.
    #[must_use]
    pub fn with_cse_passwords(mut self, passwords: CsePasswords) -> Self {
        self.passwords = passwords;
        self
    }

    /// Replaces the data-integrity settings used by runs that start from now
    /// on (call it when the settings are saved, next to
    /// `TransferQueue::update_settings`).
    pub fn set_data_integrity(&self, settings: DataIntegritySettings) {
        *self
            .data_integrity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = settings;
    }

    /// The passwords of the client-side encryption groups this runner reads
    /// (shared with its clones); pass them to [`enqueue_upload_encrypted`]
    /// and [`enqueue_download_decrypted`].
    ///
    /// [`enqueue_upload_encrypted`]: super::enqueue_upload_encrypted
    /// [`enqueue_download_decrypted`]: super::enqueue_download_decrypted
    pub fn cse_passwords(&self) -> CsePasswords {
        self.passwords.clone()
    }

    /// The current data-integrity settings.
    pub fn data_integrity(&self) -> DataIntegritySettings {
        self.data_integrity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// This runner registered for every [`TransferKind`], for
    /// `TransferQueue::open`.
    pub fn runners(&self) -> Runners {
        Runners::for_all(Arc::new(self.clone()))
    }

    async fn engine(
        &self,
        account_id: &str,
        settings: &TransferSettings,
    ) -> Result<TransferEngine> {
        let s3 = (self.provider)(account_id.to_owned()).await?;
        TransferEngine::new(
            s3,
            TransferOptions::from_parts(settings, &self.data_integrity()),
        )
    }

    async fn run_task(&self, ctx: TaskContext) -> Result<()> {
        let saved = RunnerState::read(ctx.resume().get());
        if let Some(RunnerState::Rejected { message, .. }) = &saved {
            return Err(Error::Usage(message.clone()));
        }
        let task = ctx.task().clone();
        let engine = tokio::select! {
            biased;
            _ = ctx.stop().wait() => return Err(stopped(ctx.stop())),
            engine = self.engine(&task.account_id, ctx.settings()) => engine?,
        };
        let stop = StopSignal::new();
        let hooks = Hooks::new()
            .with_throttle(Arc::new(LimiterThrottle {
                limiter: ctx.limiter().clone(),
                waiting: Arc::clone(&self.waiting),
                streams: Arc::clone(&self.streams),
            }))
            .with_progress(Arc::new(ToQueue(ctx.progress().clone())))
            .with_stop(stop.clone());
        let resume = ctx.resume();
        let completed = match task.kind {
            TransferKind::Upload => {
                let (path, dst) = local_to_s3(&task)?;
                let state = match saved {
                    Some(RunnerState::Upload(state)) => Some(state),
                    _ => None,
                };
                // The ID of a multipart upload that was just created is
                // saved at once: a crash within the persist delay would
                // otherwise leave an upload on the server that nothing
                // names, and that is billed for its parts.
                let known = Mutex::new(state.as_ref().map(|state| state.upload_id.clone()));
                let checkpoint = |state: &UploadState| {
                    save(resume, &RunnerStateRef::Upload(state));
                    if is_new_upload(&known, &state.upload_id) {
                        resume.persist_now();
                    }
                };
                match CseMode::of_group(task.group_id.as_deref()) {
                    Some(CseMode::Encrypt(compression)) => {
                        let password = self
                            .passwords
                            .get(task.group_id.as_deref())
                            .ok_or_else(|| cse::format_password_not_found(&dst))?;
                        let work = cse::upload_encrypted(
                            &engine,
                            &path,
                            &dst,
                            &hooks,
                            &password,
                            compression,
                            state,
                            &checkpoint,
                        );
                        forward_stop(ctx.stop(), &stop, work).await?.is_completed()
                    }
                    _ => {
                        let work = engine.upload_file(&path, &dst, &hooks, state, &checkpoint);
                        forward_stop(ctx.stop(), &stop, work).await?.is_completed()
                    }
                }
            }
            TransferKind::Download => {
                let (src, dest) = s3_to_local(&task)?;
                let (version, state) = match saved {
                    Some(RunnerState::Download(state)) => (None, Some(state)),
                    Some(RunnerState::VersionDownload { version_id, state }) => {
                        (Some(version_id), state)
                    }
                    _ => (None, None),
                };
                let engine = engine.with_download_version(version.clone());
                let checkpoint = |state: &DownloadState| match &version {
                    Some(version_id) => save(
                        resume,
                        &RunnerStateRef::VersionDownload {
                            version_id,
                            state: Some(state),
                        },
                    ),
                    None => save(resume, &RunnerStateRef::Download(state)),
                };
                if CseMode::of_group(task.group_id.as_deref()) == Some(CseMode::Decrypt) {
                    let password = self.passwords.get(task.group_id.as_deref());
                    let work = cse::download_decrypted(
                        &engine,
                        &src,
                        &dest,
                        &hooks,
                        password.as_ref(),
                        state,
                        &checkpoint,
                    );
                    forward_stop(ctx.stop(), &stop, work).await?.is_completed()
                } else {
                    let work = engine.download_file(&src, &dest, &hooks, state, &checkpoint);
                    forward_stop(ctx.stop(), &stop, work).await?.is_completed()
                }
            }
            TransferKind::Copy => {
                let (src, dst) = s3_to_s3(&task)?;
                forward_stop(ctx.stop(), &stop, engine.copy(&src, &dst, &hooks))
                    .await?
                    .is_completed()
            }
            TransferKind::Move => {
                let (src, dst) = s3_to_s3(&task)?;
                forward_stop(ctx.stop(), &stop, engine.move_object(&src, &dst, &hooks))
                    .await?
                    .is_completed()
            }
            TransferKind::Delete => {
                let target = s3_location(&task.src, "delete source")?;
                forward_stop(ctx.stop(), &stop, engine.delete(&target, &hooks))
                    .await?
                    .is_completed()
            }
        };
        if completed {
            Ok(())
        } else {
            Err(stopped(ctx.stop()))
        }
    }

    async fn cleanup_task(&self, task: TransferTask, resume: Option<Value>) -> Result<()> {
        match (task.kind, RunnerState::read(resume)) {
            // A rejected entry never ran; its locations may not be usable.
            (_, Some(RunnerState::Rejected { .. })) => Ok(()),
            (TransferKind::Upload, Some(RunnerState::Upload(state))) => {
                let s3 = (self.provider)(task.account_id.clone()).await?;
                TransferEngine::new(s3, TransferOptions::default())?
                    .discard_upload(&state)
                    .await
            }
            (TransferKind::Download, _) => match &task.dst {
                Some(Location::Local { path }) => discard_partial(path).await,
                _ => Ok(()),
            },
            // Nothing is left behind: a single-request upload, a copy, a
            // move or a delete has no partial state.
            _ => Ok(()),
        }
    }
}

impl TaskRunner for S3Runner {
    fn run(&self, ctx: TaskContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.run_task(ctx))
    }

    fn cleanup(&self, task: TransferTask, resume: Option<Value>) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.cleanup_task(task, resume))
    }
}

/// Stores a checkpoint in the queue's resume state.
fn save(resume: &ResumeState, state: &RunnerStateRef<'_>) {
    // The states are plain structs of strings and numbers; serializing them
    // cannot fail.
    let _ = resume.set_as(state);
}

/// Runs `work` while forwarding the queue's stop request to the engine's
/// stop signal. Pause and cancel both stop the engine as a pause, keeping
/// its state: after a cancel the queue runs the cleanup, which aborts the
/// multipart upload or deletes the partial file exactly once.
async fn forward_stop<F: Future>(queue: &QueueStop, engine: &StopSignal, work: F) -> F::Output {
    let work = std::pin::pin!(work);
    let forward = std::pin::pin!(async {
        queue.wait().await;
        engine.pause();
    });
    match futures::future::select(work, forward).await {
        Either::Left((output, _)) => output,
        Either::Right(((), work)) => work.await,
    }
}

/// The error a run returns after a stop request.
fn stopped(stop: &QueueStop) -> Error {
    stop.check().err().unwrap_or(Error::Cancelled)
}

/// The queue's limiter as the engines' throttle.
struct LimiterThrottle {
    limiter: BandwidthLimiter,
    waiting: Arc<AtomicUsize>,
    streams: Arc<StreamGate>,
}

impl Throttle for LimiterThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        let acquire = self.limiter.acquire(bytes);
        let waiting = Arc::clone(&self.waiting);
        Box::pin(async move {
            let _counted = Waiting::enter(waiting);
            acquire.await;
        })
    }

    /// One second of the current rate, shared by the requests waiting right
    /// now, between [`MIN_GRANT`] and [`MAX_CHUNK`] bytes: the limiter serves
    /// waiters first in first out, so a full round of all waiting streams
    /// takes about a second and no throttled request or response stream
    /// stays silent for long.
    fn max_grant(&self) -> u64 {
        match self.limiter.rate() {
            None => u64::MAX,
            Some(rate) => {
                let sharing = u64::try_from(self.waiting.load(Ordering::Relaxed))
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                grant(rate.get(), sharing)
            }
        }
    }

    /// A place among the bodies that stream: at the current rate only as
    /// many as one round of grants can serve in [`ROUND_BUDGET`], see
    /// [`stream_limit`]. The others wait here, before their request is sent.
    fn open_stream(&self) -> StreamFuture<'_> {
        Box::pin(self.streams.enter(&self.limiter))
    }
}

/// The grant for `sharing` streams at `rate` bytes per second.
fn grant(rate: u64, sharing: u64) -> u64 {
    (rate / sharing.max(1)).clamp(MIN_GRANT, MAX_CHUNK as u64)
}

/// How many bodies may stream at once at `rate` (`None` = unlimited).
///
/// The limiter serves its waiters in turn and grants at least [`MIN_GRANT`]
/// bytes each, so with `n` streams one round takes `n * MIN_GRANT / rate`
/// seconds when the rate is too low to give every stream more. A request
/// body that gets no byte for 20 s fails, so `n` is kept at what a round of
/// [`ROUND_BUDGET`] can serve: 20 streams at the lowest cap of 1 KiB/s,
/// 80 at 4 KiB/s, and no limit worth the name from 52 KiB/s on (the most
/// the settings allow is 32 tasks with 32 parts, 1024 streams). At least
/// one stream always runs.
fn stream_limit(rate: Option<NonZeroU64>) -> usize {
    let Some(rate) = rate else {
        return usize::MAX;
    };
    let streams = rate.get().saturating_mul(ROUND_BUDGET.as_secs()) / MIN_GRANT;
    usize::try_from(streams).unwrap_or(usize::MAX).max(1)
}

/// Counts the bodies that stream right now, across every run of a runner.
#[derive(Default)]
struct StreamGate {
    open: AtomicUsize,
    /// Notified when a stream ends.
    left: Notify,
}

impl StreamGate {
    /// Takes a place if fewer than `limit` streams are open.
    fn try_enter(&self, limit: usize) -> bool {
        let mut open = self.open.load(Ordering::Acquire);
        loop {
            if open >= limit {
                return false;
            }
            match self.open.compare_exchange_weak(
                open,
                open + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => open = actual,
            }
        }
    }

    /// Waits for a place under the cap of `limiter` as it is at the time.
    async fn enter(self: &Arc<Self>, limiter: &BandwidthLimiter) -> StreamPermit {
        loop {
            // Registered before the count is read, so a stream that ends in
            // between cannot be missed.
            let left = self.left.notified();
            tokio::pin!(left);
            left.as_mut().enable();
            if self.try_enter(stream_limit(limiter.rate())) {
                return StreamPermit::holding(Place(Arc::clone(self)));
            }
            // A raised cap makes room too, without a stream ending.
            tokio::select! {
                () = left => {}
                () = tokio::time::sleep(GATE_RECHECK) => {}
            }
        }
    }
}

/// A place in a [`StreamGate`], given up when dropped.
struct Place(Arc<StreamGate>);

impl Drop for Place {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
        self.0.left.notify_waiters();
    }
}

/// Whether `upload_id` differs from the one last seen (and remembers it).
fn is_new_upload(known: &Mutex<Option<String>>, upload_id: &str) -> bool {
    let mut known = known.lock().unwrap_or_else(PoisonError::into_inner);
    if known.as_deref() == Some(upload_id) {
        return false;
    }
    *known = Some(upload_id.to_owned());
    true
}

/// Counts a throttle request as waiting while it exists.
struct Waiting(Arc<AtomicUsize>);

impl Waiting {
    fn enter(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Waiting(counter)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The engines' progress reported to the queue.
struct ToQueue(QueueProgress);

impl Progress for ToQueue {
    fn set_total(&self, total: u64) {
        self.0.set_total(Some(total));
    }

    fn set_done(&self, done: u64) {
        self.0.set_done(done);
    }

    fn add_done(&self, amount: u64) {
        self.0.add_done(amount);
    }
}

fn s3_location(location: &Location, what: &str) -> Result<S3Uri> {
    match location {
        Location::S3 { bucket, key } => S3Uri::new(bucket.clone(), key.clone()),
        Location::Local { .. } => Err(Error::Usage(format!("the {what} must be an S3 location"))),
    }
}

fn local_location(location: &Location, what: &str) -> Result<PathBuf> {
    match location {
        Location::Local { path } => Ok(path.clone()),
        Location::S3 { .. } => Err(Error::Usage(format!("the {what} must be a local path"))),
    }
}

fn destination(task: &TransferTask) -> Result<&Location> {
    task.dst
        .as_ref()
        .ok_or_else(|| Error::Usage("the task has no destination".into()))
}

fn local_to_s3(task: &TransferTask) -> Result<(PathBuf, S3Uri)> {
    Ok((
        local_location(&task.src, "upload source")?,
        s3_location(destination(task)?, "upload destination")?,
    ))
}

fn s3_to_local(task: &TransferTask) -> Result<(S3Uri, PathBuf)> {
    Ok((
        s3_location(&task.src, "download source")?,
        local_location(destination(task)?, "download destination")?,
    ))
}

fn s3_to_s3(task: &TransferTask) -> Result<(S3Uri, S3Uri)> {
    Ok((
        s3_location(&task.src, "copy source")?,
        s3_location(destination(task)?, "copy destination")?,
    ))
}

/// The rejected state of an entry that could not be added.
pub(crate) fn rejected(code: ErrorKind, message: &str) -> Option<Value> {
    serde_json::to_value(RunnerState::Rejected {
        code,
        message: message.to_owned(),
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::source::SourceFingerprint;
    use crate::settings::ChecksumAlgorithm;
    use crate::transfer::{StopController, StopReason};
    use serde_json::json;
    use std::num::NonZeroU64;

    #[test]
    fn grants_follow_the_rate_and_the_waiting_streams() {
        assert_eq!(grant(1024 * 1024, 1), MAX_CHUNK as u64);
        assert_eq!(grant(32 * 1024, 1), 32 * 1024);
        assert_eq!(grant(32 * 1024, 4), 8 * 1024);
        assert_eq!(grant(1024, 1), 1024);
        assert_eq!(grant(1024, 100), MIN_GRANT);
        assert_eq!(grant(1, 0), MIN_GRANT);
    }

    #[tokio::test]
    async fn the_limiter_throttle_counts_waiters_and_adapts() {
        let limiter = BandwidthLimiter::unlimited();
        let throttle = LimiterThrottle {
            limiter: limiter.clone(),
            waiting: Arc::default(),
            streams: Arc::default(),
        };
        assert_eq!(throttle.max_grant(), u64::MAX);
        throttle.acquire(1 << 30).await;
        limiter.set_rate(NonZeroU64::new(16 * 1024));
        assert_eq!(throttle.max_grant(), 16 * 1024);
        throttle.waiting.store(3, Ordering::Relaxed);
        assert_eq!(throttle.max_grant(), 4 * 1024);
        throttle.waiting.store(0, Ordering::Relaxed);
        // A waiting request is counted until it is granted or dropped.
        let pending = throttle.acquire(1 << 20);
        let mut pending = Box::pin(pending);
        assert!(futures::poll!(pending.as_mut()).is_pending());
        assert_eq!(throttle.waiting.load(Ordering::Relaxed), 1);
        drop(pending);
        assert_eq!(throttle.waiting.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_stream_limit_keeps_a_round_of_grants_within_the_budget() {
        let rate = NonZeroU64::new;
        assert_eq!(stream_limit(None), usize::MAX);
        assert_eq!(stream_limit(rate(1024)), 20);
        assert_eq!(stream_limit(rate(4096)), 80);
        assert!(stream_limit(rate(52 * 1024)) >= 1024);
        assert_eq!(stream_limit(rate(1)), 1, "one stream always runs");
        assert_eq!(ROUND_BUDGET, Duration::from_secs(5));
        assert!(ROUND_BUDGET * 4 <= STALL_GRACE_PERIOD);

        // At every cap the settings allow, the streams the limit lets in
        // are served within the budget, one grant each, however many tasks
        // and parts ask (1024 at most).
        for kibps in [1u64, 2, 3, 5, 10, 33, 51, 100, 1000] {
            let rate = kibps * 1024;
            let streams = stream_limit(NonZeroU64::new(rate)).min(1024) as u64;
            let granted = streams * grant(rate, streams);
            assert!(
                granted <= rate * ROUND_BUDGET.as_secs(),
                "{streams} streams at {kibps} KiB/s need {granted} bytes per round"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn streams_beyond_the_limit_wait_for_a_place() {
        // 1 KiB/s: 20 streams.
        let limiter = BandwidthLimiter::new(NonZeroU64::new(1024));
        let gate: Arc<StreamGate> = Arc::default();
        let mut permits = Vec::new();
        for _ in 0..20 {
            permits.push(gate.enter(&limiter).await);
        }
        assert_eq!(gate.open.load(Ordering::SeqCst), 20);

        let waiting = tokio::spawn({
            let (gate, limiter) = (Arc::clone(&gate), limiter.clone());
            async move { gate.enter(&limiter).await }
        });
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert!(!waiting.is_finished(), "the 21st stream got in");

        // A stream that ends makes room.
        drop(permits.pop());
        let permit = waiting.await.unwrap();
        assert_eq!(gate.open.load(Ordering::SeqCst), 20);

        // A stream that gives up while it waits holds nothing.
        let mut giving_up = Box::pin(gate.enter(&limiter));
        assert!(futures::poll!(giving_up.as_mut()).is_pending());
        drop(giving_up);
        assert_eq!(gate.open.load(Ordering::SeqCst), 20);

        // A raised cap makes room too, without any stream ending.
        let waiting = tokio::spawn({
            let (gate, limiter) = (Arc::clone(&gate), limiter.clone());
            async move { gate.enter(&limiter).await }
        });
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!waiting.is_finished());
        limiter.set_rate(NonZeroU64::new(2048));
        let more = waiting.await.unwrap();
        assert_eq!(gate.open.load(Ordering::SeqCst), 21);
        drop((permit, more));
        drop(permits);
        assert_eq!(gate.open.load(Ordering::SeqCst), 0);

        // Unlimited: nothing waits.
        limiter.set_rate(None);
        let mut crowd = Vec::new();
        for _ in 0..2000 {
            crowd.push(gate.enter(&limiter).await);
        }
        assert_eq!(gate.open.load(Ordering::SeqCst), 2000);
    }

    #[tokio::test(start_paused = true)]
    async fn the_limiter_throttle_hands_out_places() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(1024));
        let throttle = LimiterThrottle {
            limiter: limiter.clone(),
            waiting: Arc::default(),
            streams: Arc::default(),
        };
        let mut places = Vec::new();
        for _ in 0..20 {
            places.push(throttle.open_stream().await);
        }
        let mut next = Box::pin(throttle.open_stream());
        assert!(futures::poll!(next.as_mut()).is_pending());
        drop(places.pop());
        let place = next.await;
        assert_eq!(throttle.streams.open.load(Ordering::SeqCst), 20);
        drop((place, places));
        assert_eq!(throttle.streams.open.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_new_upload_id_is_recognised_once() {
        let known = Mutex::new(Some("u-1".to_owned()));
        assert!(!is_new_upload(&known, "u-1"));
        assert!(is_new_upload(&known, "u-2"));
        assert!(!is_new_upload(&known, "u-2"));
        assert!(is_new_upload(&known, "u-1"));
        assert!(is_new_upload(&Mutex::new(None), "u-1"));
    }

    #[test]
    fn progress_is_forwarded() {
        let queue = QueueProgress::new(5, None);
        let adapter = ToQueue(queue.clone());
        adapter.set_total(100);
        adapter.set_done(10);
        adapter.add_done(15);
        assert_eq!((queue.done(), queue.total()), (25, Some(100)));
    }

    #[tokio::test]
    async fn queue_stops_reach_the_engine_as_pauses() {
        for reason in [StopReason::Pause, StopReason::Cancel] {
            let (controller, signal) = StopController::new();
            let engine = StopSignal::new();
            // The work ends only once the engine signal fires, and reports
            // the reason it saw.
            let forwarded = tokio::spawn(async move {
                let work = async {
                    engine.stopped().await;
                    engine.reason()
                };
                forward_stop(&signal, &engine, work).await
            });
            tokio::task::yield_now().await;
            controller.request(reason);
            assert_eq!(
                forwarded.await.unwrap(),
                Some(crate::engine::StopReason::Pause)
            );
        }
        // Work that finishes first is not stopped.
        let (_controller, signal) = StopController::new();
        let engine = StopSignal::new();
        assert_eq!(forward_stop(&signal, &engine, async { 7 }).await, 7);
        assert!(!engine.is_stopped());
    }

    #[test]
    fn a_version_download_keeps_its_version_in_its_state() {
        let seed = RunnerState::VersionDownload {
            version_id: "3HL4kq+rm".into(),
            state: None,
        };
        let value = serde_json::to_value(&seed).unwrap();
        assert_eq!(
            value,
            json!({"type": "version_download", "version_id": "3HL4kq+rm", "state": null})
        );
        assert_eq!(RunnerState::read(Some(value)), Some(seed));

        // A checkpoint of the engine is stored beside the version.
        let download = DownloadState {
            bucket: "b".into(),
            key: "k".into(),
            etag: "\"e\"".into(),
            size: 10,
            last_modified: None,
            checksum: None,
            offset: 4,
        };
        let value = serde_json::to_value(RunnerStateRef::VersionDownload {
            version_id: "v1",
            state: Some(&download),
        })
        .unwrap();
        assert_eq!(value["type"], "version_download");
        assert_eq!(value["version_id"], "v1");
        assert_eq!(value["state"]["offset"], 4);
        assert_eq!(
            RunnerState::read(Some(value)),
            Some(RunnerState::VersionDownload {
                version_id: "v1".into(),
                state: Some(download)
            })
        );
        // A plain download state is a different thing and has no version.
        assert_eq!(
            RunnerState::read(Some(json!({"type": "version_download"}))),
            None
        );
    }

    #[test]
    fn resume_states_are_tagged_and_unreadable_ones_are_ignored() {
        let upload = UploadState {
            bucket: "b".into(),
            key: "k".into(),
            upload_id: "u".into(),
            source: SourceFingerprint {
                size: 1,
                modified: None,
            },
            part_size: 5 << 20,
            checksum_algorithm: ChecksumAlgorithm::Crc32,
            parts: Vec::new(),
        };
        let value = serde_json::to_value(RunnerStateRef::Upload(&upload)).unwrap();
        assert_eq!(value["type"], "upload");
        assert_eq!(value["upload_id"], "u");
        assert_eq!(
            RunnerState::read(Some(value)),
            Some(RunnerState::Upload(upload))
        );
        let rejected = rejected(ErrorKind::Usage, "unsafe key").unwrap();
        assert_eq!(
            rejected,
            json!({"type": "rejected", "code": "usage", "message": "unsafe key"})
        );
        assert_eq!(RunnerState::read(Some(json!({"type": "other"}))), None);
        assert_eq!(RunnerState::read(Some(json!([1, 2]))), None);
        assert_eq!(RunnerState::read(None), None);
    }
}
