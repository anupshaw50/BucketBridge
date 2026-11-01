//! The seams between the engines and whoever drives them: bandwidth
//! throttling, progress reporting and the stop signal.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use tokio_util::sync::CancellationToken;

/// The future returned by [`Throttle::acquire`].
pub type ThrottleFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// The future returned by [`Throttle::open_stream`].
pub type StreamFuture<'a> = Pin<Box<dyn Future<Output = StreamPermit> + Send + 'a>>;

/// Largest grant the engines ask for by default (see [`Throttle::max_grant`]).
pub const DEFAULT_MAX_GRANT: u64 = 64 * 1024;

/// A place among the request and response bodies that stream at the same
/// time under a [`Throttle`] (see [`Throttle::open_stream`]). The place is
/// given up when the permit is dropped.
#[must_use = "the place is given up as soon as the permit is dropped"]
pub struct StreamPermit {
    _place: Option<Box<dyn Send + Sync>>,
}

impl StreamPermit {
    /// A permit that holds nothing, for a throttle that allows any number of
    /// streams.
    pub fn unlimited() -> Self {
        StreamPermit { _place: None }
    }

    /// A permit that keeps `place` alive until it is dropped; dropping
    /// `place` gives the place up.
    pub fn holding(place: impl Send + Sync + 'static) -> Self {
        StreamPermit {
            _place: Some(Box::new(place)),
        }
    }
}

impl fmt::Debug for StreamPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamPermit")
            .field("limited", &self._place.is_some())
            .finish()
    }
}

/// Bandwidth limiter seam.
///
/// Engines call [`Throttle::acquire`] for every piece of payload: before it
/// is handed to the connection (uploads) and after it was received, before
/// the next piece is read (downloads). One limiter shared by all tasks
/// therefore caps the total rate. Bytes that the SDK sends again when it
/// retries a request pass through the throttle again, because they use
/// bandwidth again.
pub trait Throttle: Send + Sync {
    /// Resolves once `bytes` more bytes may be transferred. `bytes` is never
    /// larger than [`Throttle::max_grant`].
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_>;

    /// The largest number of bytes the engines request in one
    /// [`Throttle::acquire`] call. A limiter should return at most what it
    /// grants in about one second (its rate in bytes per second, at least
    /// 1), so that no single wait keeps a request or response stream silent
    /// for long: the SDK's stalled-stream protection watches both. Values
    /// below 1 are treated as 1. Default [`DEFAULT_MAX_GRANT`].
    fn max_grant(&self) -> u64 {
        DEFAULT_MAX_GRANT
    }

    /// Waits until one more request or response body may stream, and keeps
    /// its place until the returned [`StreamPermit`] is dropped.
    ///
    /// [`Throttle::acquire`] serves the streams in turn, so one round takes
    /// about "streams x grant / rate" seconds, and a request body that gets
    /// no byte for [`STALL_GRACE_PERIOD`](crate::client::STALL_GRACE_PERIOD)
    /// fails (stalled-stream protection, and S3's own idle timeout). A
    /// limiter at a very low rate can therefore keep only so many streams
    /// alive; it holds the others back here, before their request is sent.
    /// Engines call this once per request that streams a body, around the
    /// request, and drop the future on a stop request. The default allows
    /// any number of streams.
    fn open_stream(&self) -> StreamFuture<'_> {
        Box::pin(std::future::ready(StreamPermit::unlimited()))
    }
}

/// A [`Throttle`] that never waits.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unthrottled;

impl Throttle for Unthrottled {
    fn acquire(&self, _bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(std::future::ready(()))
    }

    fn max_grant(&self) -> u64 {
        u64::MAX
    }
}

/// Progress seam. The unit is bytes, except for deletes, which count
/// objects.
///
/// Calls come from the task driving the engine and from HTTP connection
/// tasks (request bodies count their bytes as they hand them to the
/// connection), so implementations must be thread-safe, return quickly and
/// never block: store into atomics or `try_send` into a channel.
pub trait Progress: Send + Sync {
    /// The total amount of work. Called again when it changes: a download
    /// that restarts because the object changed, or a prefix delete whose
    /// listing is still growing.
    fn set_total(&self, total: u64);

    /// Sets the amount done to an absolute value: the work already done when
    /// a transfer resumes, or 0 when it restarts. Not a measure of speed.
    fn set_done(&self, done: u64);

    /// `amount` more units are done.
    fn add_done(&self, amount: u64);
}

/// A [`Progress`] that ignores every report.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn set_total(&self, _total: u64) {}

    fn set_done(&self, _done: u64) {}

    fn add_done(&self, _amount: u64) {}
}

/// A [`Progress`] that keeps the latest values in atomics, for callers that
/// poll (the CLI, tests).
#[derive(Debug)]
pub struct ProgressCounter {
    total: AtomicU64,
    done: AtomicU64,
}

/// Marks an unknown total.
const UNKNOWN: u64 = u64::MAX;

impl Default for ProgressCounter {
    fn default() -> Self {
        ProgressCounter {
            total: AtomicU64::new(UNKNOWN),
            done: AtomicU64::new(0),
        }
    }
}

impl ProgressCounter {
    /// A counter with an unknown total and nothing done.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last total reported, if any.
    pub fn total(&self) -> Option<u64> {
        let total = self.total.load(Ordering::Acquire);
        (total != UNKNOWN).then_some(total)
    }

    /// The amount done so far.
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Acquire)
    }
}

impl Progress for ProgressCounter {
    fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Release);
    }

    fn set_done(&self, done: u64) {
        self.done.store(done, Ordering::Release);
    }

    fn add_done(&self, amount: u64) {
        self.done.fetch_add(amount, Ordering::AcqRel);
    }
}

/// Why a running engine was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopReason {
    /// Stop and keep everything needed to resume: the resume state, the
    /// partial download file, the multipart upload.
    Pause,
    /// Stop and clean up: abort the multipart upload, delete the partial
    /// download file.
    Cancel,
}

const RUNNING: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

/// Stop seam: a [`CancellationToken`] plus the reason it was cancelled.
///
/// Clones share the same signal. [`StopSignal::cancel`] wins over
/// [`StopSignal::pause`] in either order. A token that is cancelled directly
/// (without `pause` or `cancel`, for example a child of an application-wide
/// shutdown token) counts as a pause, so that nothing is ever deleted
/// without an explicit cancel.
#[derive(Debug, Clone, Default)]
pub struct StopSignal {
    token: CancellationToken,
    reason: Arc<AtomicU8>,
}

impl StopSignal {
    /// A signal that has not been stopped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps an existing token (see the type documentation for tokens that
    /// are cancelled directly).
    pub fn from_token(token: CancellationToken) -> Self {
        StopSignal {
            token,
            reason: Arc::default(),
        }
    }

    /// Asks the engine to stop and keep its resume state.
    pub fn pause(&self) {
        let _ = self
            .reason
            .compare_exchange(RUNNING, PAUSE, Ordering::AcqRel, Ordering::Acquire);
        self.token.cancel();
    }

    /// Asks the engine to stop and clean up.
    pub fn cancel(&self) {
        self.reason.store(CANCEL, Ordering::Release);
        self.token.cancel();
    }

    /// True once a stop was requested.
    pub fn is_stopped(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The reason, once a stop was requested.
    pub fn reason(&self) -> Option<StopReason> {
        if !self.token.is_cancelled() {
            return None;
        }
        Some(match self.reason.load(Ordering::Acquire) {
            CANCEL => StopReason::Cancel,
            _ => StopReason::Pause,
        })
    }

    /// Resolves once a stop was requested.
    pub async fn stopped(&self) {
        self.token.cancelled().await;
    }

    /// The underlying token.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

/// How an engine run ended when it did not fail.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Outcome<T> {
    /// The work is done.
    Completed(T),
    /// Stopped by [`StopSignal::pause`]. The most recent checkpoint is the
    /// state to resume from; partial files and multipart uploads are kept.
    Paused,
    /// Stopped by [`StopSignal::cancel`] and cleaned up.
    Cancelled,
}

impl<T> Outcome<T> {
    /// The result of a completed run.
    pub fn into_completed(self) -> Option<T> {
        match self {
            Outcome::Completed(value) => Some(value),
            Outcome::Paused | Outcome::Cancelled => None,
        }
    }

    /// True for [`Outcome::Completed`].
    pub fn is_completed(&self) -> bool {
        matches!(self, Outcome::Completed(_))
    }

    /// Converts the result of a completed run.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Outcome::Completed(value) => Outcome::Completed(f(value)),
            Outcome::Paused => Outcome::Paused,
            Outcome::Cancelled => Outcome::Cancelled,
        }
    }

    /// The outcome for a stop with `reason` (`None`, a stop without a
    /// reason, counts as a pause).
    pub(crate) fn stopped(reason: Option<StopReason>) -> Self {
        match reason {
            Some(StopReason::Cancel) => Outcome::Cancelled,
            Some(StopReason::Pause) | None => Outcome::Paused,
        }
    }
}

/// The seams for one engine run.
#[derive(Clone)]
pub struct Hooks {
    /// Bandwidth limiter; [`Unthrottled`] by default.
    pub throttle: Arc<dyn Throttle>,
    /// Progress sink; [`NoProgress`] by default.
    pub progress: Arc<dyn Progress>,
    /// Stop signal; a fresh one by default.
    pub stop: StopSignal,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            throttle: Arc::new(Unthrottled),
            progress: Arc::new(NoProgress),
            stop: StopSignal::new(),
        }
    }
}

impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("max_grant", &self.throttle.max_grant())
            .field("stop", &self.stop.reason())
            .finish_non_exhaustive()
    }
}

impl Hooks {
    /// Hooks without throttling, progress or a stop request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the throttle.
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<dyn Throttle>) -> Self {
        self.throttle = throttle;
        self
    }

    /// Replaces the progress sink.
    #[must_use]
    pub fn with_progress(mut self, progress: Arc<dyn Progress>) -> Self {
        self.progress = progress;
        self
    }

    /// Replaces the stop signal.
    #[must_use]
    pub fn with_stop(mut self, stop: StopSignal) -> Self {
        self.stop = stop;
        self
    }

    /// The grant size to use: the throttle's hint, at least 1.
    pub(crate) fn grant_size(&self) -> u64 {
        self.throttle.max_grant().max(1)
    }

    /// Waits for a place among the streaming bodies (see
    /// [`Throttle::open_stream`]); keep the permit while the request runs.
    pub(crate) async fn open_stream(&self) -> StreamPermit {
        self.throttle.open_stream().await
    }

    /// Waits for permission to transfer `bytes`, in grants of at most
    /// [`Throttle::max_grant`] bytes.
    pub(crate) async fn throttle(&self, bytes: u64) {
        let grant = self.grant_size();
        let mut left = bytes;
        while left > 0 {
            let piece = left.min(grant);
            self.throttle.acquire(piece).await;
            left -= piece;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_wins_over_pause_in_either_order() {
        let stop = StopSignal::new();
        assert_eq!(stop.reason(), None);
        assert!(!stop.is_stopped());
        stop.pause();
        assert_eq!(stop.reason(), Some(StopReason::Pause));
        stop.cancel();
        assert_eq!(stop.reason(), Some(StopReason::Cancel));
        stop.pause();
        assert_eq!(stop.reason(), Some(StopReason::Cancel));

        let stop = StopSignal::new();
        stop.cancel();
        stop.pause();
        assert_eq!(stop.reason(), Some(StopReason::Cancel));
    }

    #[test]
    fn clones_share_the_signal() {
        let stop = StopSignal::new();
        let clone = stop.clone();
        clone.pause();
        assert!(stop.is_stopped());
        assert_eq!(stop.reason(), Some(StopReason::Pause));
    }

    #[test]
    fn a_token_cancelled_directly_counts_as_a_pause() {
        let token = CancellationToken::new();
        let stop = StopSignal::from_token(token.child_token());
        assert_eq!(stop.reason(), None);
        token.cancel();
        assert_eq!(stop.reason(), Some(StopReason::Pause));
        assert_eq!(Outcome::<()>::stopped(stop.reason()), Outcome::<()>::Paused);
        assert_eq!(
            Outcome::<()>::stopped(Some(StopReason::Cancel)),
            Outcome::<()>::Cancelled
        );
    }

    #[tokio::test]
    async fn stopped_resolves_after_a_stop() {
        let stop = StopSignal::new();
        let waiter = {
            let stop = stop.clone();
            tokio::spawn(async move { stop.stopped().await })
        };
        stop.cancel();
        waiter.await.unwrap();
    }

    #[test]
    fn progress_counter_tracks_reports() {
        let counter = ProgressCounter::new();
        assert_eq!(counter.total(), None);
        assert_eq!(counter.done(), 0);
        counter.set_total(10);
        counter.add_done(3);
        counter.add_done(4);
        assert_eq!(counter.total(), Some(10));
        assert_eq!(counter.done(), 7);
        counter.set_done(2);
        assert_eq!(counter.done(), 2);
    }

    #[test]
    fn outcome_helpers() {
        assert_eq!(Outcome::Completed(2).map(|v| v * 2), Outcome::Completed(4));
        assert_eq!(Outcome::<u8>::Paused.map(|v| v * 2), Outcome::Paused);
        assert!(Outcome::Completed(()).is_completed());
        assert!(!Outcome::<()>::Cancelled.is_completed());
        assert_eq!(Outcome::Completed(1).into_completed(), Some(1));
        assert_eq!(Outcome::<u8>::Cancelled.into_completed(), None);
    }

    struct Recorder(std::sync::Mutex<Vec<u64>>, u64);

    impl Throttle for Recorder {
        fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
            self.0.lock().unwrap().push(bytes);
            Box::pin(std::future::ready(()))
        }

        fn max_grant(&self) -> u64 {
            self.1
        }
    }

    #[tokio::test]
    async fn throttling_is_split_into_grants() {
        let recorder = Arc::new(Recorder(std::sync::Mutex::default(), 4));
        let hooks = Hooks::new().with_throttle(recorder.clone());
        hooks.throttle(10).await;
        hooks.throttle(0).await;
        assert_eq!(*recorder.0.lock().unwrap(), [4, 4, 2]);

        let zero = Arc::new(Recorder(std::sync::Mutex::default(), 0));
        let hooks = Hooks::new().with_throttle(zero.clone());
        hooks.throttle(2).await;
        assert_eq!(*zero.0.lock().unwrap(), [1, 1]);

        let hooks = Hooks::new();
        assert_eq!(hooks.grant_size(), u64::MAX);
        hooks.throttle(u64::MAX).await;
    }
}
