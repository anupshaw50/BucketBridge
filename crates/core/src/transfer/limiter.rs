//! Shared bandwidth limiter (token bucket) and the throttling adapters the
//! runners wrap their byte streams in.
//!
//! One [`BandwidthLimiter`] is shared by every task of a queue, so the
//! configured rate caps the aggregate traffic. Properties:
//!
//! * The rate is in bytes per second; `None` means unlimited. It can be changed
//!   at any time with [`BandwidthLimiter::set_rate`]; current waiters wake up
//!   and continue at the new rate.
//! * Waiters are served strictly first-in, first-out: a small request never
//!   overtakes a large one that asked earlier.
//! * [`Acquire`] is cancellation safe: dropping it removes it from the queue,
//!   wakes the next waiter, and returns the tokens it had already collected.
//! * A request larger than the bucket is satisfied in several refills.
//!
//! The bucket holds at most `max(rate / 8, min(rate, 4 KiB))` bytes, so an idle
//! limiter allows a burst of 1/8 s of traffic (up to one second's worth for
//! rates below 32 KiB/s). Tokens are tracked as integers in units of 10^-9
//! bytes, which makes refills exact at any rate.
//!
//! The limiter uses the Tokio clock (`tokio::time`), so waits must happen
//! inside a Tokio runtime with the time driver enabled.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker, ready};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::time::{Instant, Sleep};

/// Largest piece the throttling adapters pass on at once (64 KiB).
pub const MAX_CHUNK: usize = 64 * 1024;

/// Token units per byte: one nanosecond at `r` bytes/s adds exactly `r` units.
const UNITS_PER_BYTE: u128 = 1_000_000_000;

/// Smallest bucket for rates of at least 4 KiB/s.
const MIN_BURST: u64 = 4 * 1024;

/// A token-bucket bandwidth limiter shared by all transfers.
///
/// Cloning is cheap and every clone controls the same bucket.
#[derive(Clone)]
pub struct BandwidthLimiter {
    bucket: Arc<Mutex<Bucket>>,
}

impl BandwidthLimiter {
    /// Creates a limiter with `rate` bytes per second (`None` = unlimited).
    pub fn new(rate: Option<NonZeroU64>) -> Self {
        let capacity = rate.map_or(0, |r| capacity_for(r.get()));
        let bucket = Bucket {
            rate,
            capacity,
            level: u128::from(capacity) * UNITS_PER_BYTE,
            refilled_at: Instant::now(),
            waiters: BTreeMap::new(),
            next_ticket: 0,
        };
        Self {
            bucket: Arc::new(Mutex::new(bucket)),
        }
    }

    /// Creates a limiter that never waits until a rate is set.
    pub fn unlimited() -> Self {
        Self::new(None)
    }

    /// The current rate in bytes per second (`None` = unlimited).
    pub fn rate(&self) -> Option<NonZeroU64> {
        lock(&self.bucket).rate
    }

    /// Changes the rate. Waiting requests wake up and continue at the new rate;
    /// with `None` they all complete immediately.
    pub fn set_rate(&self, rate: Option<NonZeroU64>) {
        let mut bucket = lock(&self.bucket);
        bucket.refill(Instant::now());
        let was_unlimited = bucket.rate.is_none();
        bucket.rate = rate;
        bucket.capacity = rate.map_or(0, |r| capacity_for(r.get()));
        let full = bucket.capacity_units();
        bucket.level = if was_unlimited {
            full
        } else {
            bucket.level.min(full)
        };
        bucket.wake_all();
    }

    /// Waits until `bytes` may be transferred. Requests are served in the
    /// order they first wait; dropping the future gives up the place in line
    /// and returns any tokens it had already collected.
    pub fn acquire(&self, bytes: u64) -> Acquire {
        Acquire {
            bucket: Arc::clone(&self.bucket),
            claim: Claim {
                remaining: bytes,
                granted: 0,
                ticket: None,
            },
            timer: None,
        }
    }

    /// Wraps a byte stream so that it yields at most [`MAX_CHUNK`] bytes per
    /// item and only after the limiter granted them.
    pub fn throttle_stream<S>(&self, inner: S) -> ThrottledStream<S> {
        ThrottledStream::new(inner, self.clone())
    }

    /// Wraps a reader so that every byte read passes through the limiter.
    pub fn throttle_read<R>(&self, inner: R) -> ThrottledRead<R> {
        ThrottledRead::new(inner, self.clone())
    }
}

impl Default for BandwidthLimiter {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl fmt::Debug for BandwidthLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BandwidthLimiter")
            .field("rate", &self.rate())
            .finish()
    }
}

/// Bucket size for a rate: 1/8 s of traffic, at least 4 KiB, at most one
/// second's worth.
fn capacity_for(rate: u64) -> u64 {
    (rate / 8).max(rate.min(MIN_BURST)).max(1)
}

fn lock(bucket: &Mutex<Bucket>) -> MutexGuard<'_, Bucket> {
    bucket.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Bucket {
    rate: Option<NonZeroU64>,
    /// Maximum number of whole bytes the bucket holds.
    capacity: u64,
    /// Current content in units of `1 / UNITS_PER_BYTE` bytes.
    level: u128,
    refilled_at: Instant,
    /// Waiting requests in arrival order; the first entry is the head.
    waiters: BTreeMap<u64, Option<Waker>>,
    next_ticket: u64,
}

impl Bucket {
    fn capacity_units(&self) -> u128 {
        u128::from(self.capacity) * UNITS_PER_BYTE
    }

    fn refill(&mut self, now: Instant) {
        if now <= self.refilled_at {
            return;
        }
        if let Some(rate) = self.rate {
            let elapsed = (now - self.refilled_at).as_nanos();
            let added = elapsed.saturating_mul(u128::from(rate.get()));
            self.level = self.level.saturating_add(added).min(self.capacity_units());
        }
        self.refilled_at = now;
    }

    /// Whole bytes currently available.
    fn available(&self) -> u64 {
        u64::try_from(self.level / UNITS_PER_BYTE).unwrap_or(u64::MAX)
    }

    fn take(&mut self, bytes: u64) {
        self.level = self
            .level
            .saturating_sub(u128::from(bytes) * UNITS_PER_BYTE);
    }

    fn give_back(&mut self, bytes: u64) {
        self.refill(Instant::now());
        self.level = self
            .level
            .saturating_add(u128::from(bytes) * UNITS_PER_BYTE)
            .min(self.capacity_units());
    }

    /// Time until `bytes` whole bytes are available at `rate`.
    fn time_until(&self, bytes: u64, rate: NonZeroU64) -> Duration {
        let missing = (u128::from(bytes) * UNITS_PER_BYTE).saturating_sub(self.level);
        let nanos = missing.div_ceil(u128::from(rate.get()));
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    fn head(&self) -> Option<u64> {
        self.waiters.first_key_value().map(|(ticket, _)| *ticket)
    }

    fn wake_head(&mut self) {
        if let Some(mut head) = self.waiters.first_entry()
            && let Some(waker) = head.get_mut().take()
        {
            waker.wake();
        }
    }

    fn wake_all(&mut self) {
        for slot in self.waiters.values_mut() {
            if let Some(waker) = slot.take() {
                waker.wake();
            }
        }
    }

    fn register_waker(&mut self, ticket: u64, waker: &Waker) {
        if let Some(slot) = self.waiters.get_mut(&ticket) {
            match slot {
                Some(current) if current.will_wake(waker) => {}
                _ => *slot = Some(waker.clone()),
            }
        }
    }
}

/// Future returned by [`BandwidthLimiter::acquire`].
#[must_use = "an `Acquire` does nothing unless it is awaited"]
pub struct Acquire {
    bucket: Arc<Mutex<Bucket>>,
    claim: Claim,
    timer: Option<Pin<Box<Sleep>>>,
}

/// Per-request bookkeeping, kept apart from the bucket handle so the bucket
/// lock and the claim can be borrowed at the same time.
struct Claim {
    /// Bytes still to be granted.
    remaining: u64,
    /// Bytes already taken from the bucket while at the head of the line;
    /// returned to the bucket if the request is dropped before completing.
    granted: u64,
    /// Place in line once the request had to wait.
    ticket: Option<u64>,
}

enum Step {
    Ready,
    Pending,
    SleepUntil(Instant),
}

impl Claim {
    fn step(&mut self, bucket: &mut Bucket, waker: &Waker) -> Step {
        let Some(rate) = bucket.rate else {
            self.leave(bucket);
            self.remaining = 0;
            return Step::Ready;
        };
        if self.remaining == 0 {
            self.leave(bucket);
            return Step::Ready;
        }
        let now = Instant::now();
        bucket.refill(now);
        let ticket = match self.ticket {
            Some(ticket) => ticket,
            None => {
                if bucket.waiters.is_empty() && bucket.available() >= self.remaining {
                    bucket.take(self.remaining);
                    self.remaining = 0;
                    return Step::Ready;
                }
                let ticket = bucket.next_ticket;
                bucket.next_ticket = bucket.next_ticket.wrapping_add(1);
                bucket.waiters.insert(ticket, None);
                self.ticket = Some(ticket);
                ticket
            }
        };
        bucket.register_waker(ticket, waker);
        if bucket.head() != Some(ticket) {
            return Step::Pending;
        }
        let grant = bucket.available().min(self.remaining);
        bucket.take(grant);
        self.remaining -= grant;
        self.granted += grant;
        if self.remaining == 0 {
            bucket.waiters.remove(&ticket);
            self.ticket = None;
            self.granted = 0;
            bucket.wake_head();
            return Step::Ready;
        }
        let target = self.remaining.min(bucket.capacity);
        Step::SleepUntil(now + bucket.time_until(target, rate))
    }

    /// Leaves the line (if in it), returning collected tokens and waking the
    /// next waiter when this request was the head.
    fn leave(&mut self, bucket: &mut Bucket) {
        if let Some(ticket) = self.ticket.take() {
            let was_head = bucket.head() == Some(ticket);
            bucket.waiters.remove(&ticket);
            if was_head {
                if self.granted > 0 {
                    bucket.give_back(self.granted);
                }
                bucket.wake_head();
            }
        }
        self.granted = 0;
    }
}

impl Future for Acquire {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        loop {
            let deadline = {
                let mut bucket = lock(&this.bucket);
                match this.claim.step(&mut bucket, cx.waker()) {
                    Step::Ready => return Poll::Ready(()),
                    Step::Pending => return Poll::Pending,
                    Step::SleepUntil(deadline) => deadline,
                }
            };
            let timer = match &mut this.timer {
                Some(timer) => {
                    timer.as_mut().reset(deadline);
                    timer
                }
                None => this
                    .timer
                    .insert(Box::pin(tokio::time::sleep_until(deadline))),
            };
            if timer.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
    }
}

impl Drop for Acquire {
    fn drop(&mut self) {
        if self.claim.ticket.is_some() {
            let mut bucket = lock(&self.bucket);
            self.claim.leave(&mut bucket);
        }
    }
}

impl fmt::Debug for Acquire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Acquire")
            .field("remaining", &self.claim.remaining)
            .field("waiting", &self.claim.ticket.is_some())
            .finish()
    }
}

/// A byte stream throttled by a [`BandwidthLimiter`].
///
/// Large chunks are split into pieces of at most [`MAX_CHUNK`] bytes; each
/// piece is yielded once the limiter granted it. Empty chunks are skipped and
/// errors are passed through unchanged.
pub struct ThrottledStream<S> {
    inner: Pin<Box<S>>,
    limiter: BandwidthLimiter,
    buffered: Bytes,
    pending: Option<(Acquire, usize)>,
    finished: bool,
}

impl<S> ThrottledStream<S> {
    /// Wraps `inner` so that every byte it yields first passes `limiter`.
    pub fn new(inner: S, limiter: BandwidthLimiter) -> Self {
        Self {
            inner: Box::pin(inner),
            limiter,
            buffered: Bytes::new(),
            pending: None,
            finished: false,
        }
    }
}

impl<S, E> Stream for ThrottledStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some((acquire, len)) = &mut this.pending {
                ready!(Pin::new(acquire).poll(cx));
                let len = *len;
                this.pending = None;
                return Poll::Ready(Some(Ok(this.buffered.split_to(len))));
            }
            if !this.buffered.is_empty() {
                let len = this.buffered.len().min(MAX_CHUNK);
                this.pending = Some((this.limiter.acquire(len as u64), len));
                continue;
            }
            if this.finished {
                return Poll::Ready(None);
            }
            match ready!(this.inner.as_mut().poll_next(cx)) {
                Some(Ok(bytes)) => this.buffered = bytes,
                Some(Err(error)) => return Poll::Ready(Some(Err(error))),
                None => {
                    this.finished = true;
                    return Poll::Ready(None);
                }
            }
        }
    }
}

impl<S> fmt::Debug for ThrottledStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThrottledStream")
            .field("limiter", &self.limiter)
            .field("buffered", &self.buffered.len())
            .finish()
    }
}

/// An [`AsyncRead`] throttled by a [`BandwidthLimiter`].
///
/// Data is read from the inner reader into an internal buffer of
/// [`MAX_CHUNK`] bytes; the bytes actually read are paid for before they are
/// handed out, so the limiter is charged exactly once per byte.
pub struct ThrottledRead<R> {
    inner: Pin<Box<R>>,
    limiter: BandwidthLimiter,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
    pending: Option<Acquire>,
}

impl<R> ThrottledRead<R> {
    /// Wraps `inner` so that every byte read from it first passes `limiter`.
    pub fn new(inner: R, limiter: BandwidthLimiter) -> Self {
        Self {
            inner: Box::pin(inner),
            limiter,
            buffer: vec![0; MAX_CHUNK].into_boxed_slice(),
            start: 0,
            end: 0,
            pending: None,
        }
    }
}

impl<R: AsyncRead> AsyncRead for ThrottledRead<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if let Some(acquire) = &mut this.pending {
                ready!(Pin::new(acquire).poll(cx));
                this.pending = None;
            }
            if this.start < this.end {
                let len = (this.end - this.start).min(out.remaining());
                out.put_slice(&this.buffer[this.start..this.start + len]);
                this.start += len;
                return Poll::Ready(Ok(()));
            }
            let mut chunk = ReadBuf::new(&mut this.buffer);
            ready!(this.inner.as_mut().poll_read(cx, &mut chunk))?;
            let len = chunk.filled().len();
            if len == 0 {
                return Poll::Ready(Ok(()));
            }
            this.start = 0;
            this.end = len;
            this.pending = Some(this.limiter.acquire(len as u64));
        }
    }
}

impl<R> fmt::Debug for ThrottledRead<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThrottledRead")
            .field("limiter", &self.limiter)
            .field("buffered", &(self.end - self.start))
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use futures::StreamExt;
    use futures::stream;
    use tokio::io::AsyncReadExt;
    use tokio::task::{JoinHandle, yield_now};

    use super::*;

    fn rate(bytes_per_sec: u64) -> Option<NonZeroU64> {
        NonZeroU64::new(bytes_per_sec)
    }

    fn assert_close(actual: Duration, expected: Duration, tolerance: Duration) {
        let diff = actual.abs_diff(expected);
        assert!(
            diff <= tolerance,
            "expected {expected:?} +/- {tolerance:?}, got {actual:?}"
        );
    }

    /// Spawns `acquire(bytes)` and yields so the request takes its place in
    /// line before the next one is spawned.
    async fn spawn_acquire(
        limiter: &BandwidthLimiter,
        bytes: u64,
        label: &'static str,
        done: &Arc<StdMutex<Vec<(&'static str, Duration)>>>,
        start: Instant,
    ) -> JoinHandle<()> {
        let acquire = limiter.acquire(bytes);
        let done = Arc::clone(done);
        let handle = tokio::spawn(async move {
            acquire.await;
            done.lock().unwrap().push((label, start.elapsed()));
        });
        yield_now().await;
        handle
    }

    #[test]
    fn capacity_scales_with_rate() {
        assert_eq!(capacity_for(1), 1);
        assert_eq!(capacity_for(1024), 1024);
        assert_eq!(capacity_for(4096), 4096);
        assert_eq!(capacity_for(8192), 4096);
        assert_eq!(capacity_for(32 * 1024), 4096);
        assert_eq!(capacity_for(1024 * 1024), 128 * 1024);
        assert_eq!(capacity_for(u64::MAX), u64::MAX / 8);
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_waits() {
        let limiter = BandwidthLimiter::unlimited();
        let start = Instant::now();
        limiter.acquire(10 * 1024 * 1024 * 1024).await;
        limiter.acquire(0).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(limiter.rate(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn burst_then_steady_rate() {
        let limiter = BandwidthLimiter::new(rate(8192));
        let start = Instant::now();
        limiter.acquire(4096).await;
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "a full bucket grants at once"
        );
        limiter.acquire(8192).await;
        assert_close(
            start.elapsed(),
            Duration::from_secs(1),
            Duration::from_millis(2),
        );
        limiter.acquire(0).await;
        assert_close(
            start.elapsed(),
            Duration::from_secs(1),
            Duration::from_millis(2),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn request_larger_than_bucket_is_split() {
        let limiter = BandwidthLimiter::new(rate(8192));
        let start = Instant::now();
        limiter.acquire(20_000).await;
        // 4096 bytes from the full bucket, the rest at 8192 B/s.
        let expected = Duration::from_secs_f64((20_000.0 - 4096.0) / 8192.0);
        assert_close(start.elapsed(), expected, Duration::from_millis(5));
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_are_served_first_in_first_out() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await; // drain the bucket
        let start = Instant::now();
        let done = Arc::new(StdMutex::new(Vec::new()));
        let a = spawn_acquire(&limiter, 500, "a", &done, start).await;
        let b = spawn_acquire(&limiter, 100, "b", &done, start).await;
        let c = spawn_acquire(&limiter, 100, "c", &done, start).await;
        for handle in [a, b, c] {
            handle.await.unwrap();
        }
        let done = done.lock().unwrap().clone();
        let order: Vec<_> = done.iter().map(|(label, _)| *label).collect();
        assert_eq!(order, ["a", "b", "c"], "small requests must not overtake");
        let tolerance = Duration::from_millis(3);
        assert_close(done[0].1, Duration::from_millis(500), tolerance);
        assert_close(done[1].1, Duration::from_millis(600), tolerance);
        assert_close(done[2].1, Duration::from_millis(700), tolerance);
    }

    #[tokio::test(start_paused = true)]
    async fn new_request_waits_behind_existing_waiters_even_with_tokens() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await;
        let start = Instant::now();
        let done = Arc::new(StdMutex::new(Vec::new()));
        let big = spawn_acquire(&limiter, 900, "big", &done, start).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        // 300 tokens are in the bucket, but the head needs 900: a 10 byte
        // request arriving now must still wait for it.
        let small = spawn_acquire(&limiter, 10, "small", &done, start).await;
        big.await.unwrap();
        small.await.unwrap();
        let done = done.lock().unwrap().clone();
        assert_eq!(done[0].0, "big");
        assert_eq!(done[1].0, "small");
        assert_close(
            done[0].1,
            Duration::from_millis(900),
            Duration::from_millis(3),
        );
        assert_close(
            done[1].1,
            Duration::from_millis(910),
            Duration::from_millis(3),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_head_returns_collected_tokens() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await;
        let head = tokio::spawn(limiter.acquire(5000));
        yield_now().await;
        // At 1.0 s the head takes a full bucket (1000) and keeps waiting.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        head.abort();
        assert!(head.await.unwrap_err().is_cancelled());
        // 500 tokens accumulated since 1.0 s plus the 1000 returned by the
        // dropped head, capped at the bucket size (1000).
        let start = Instant::now();
        limiter.acquire(1000).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire(100).await;
        assert_close(
            start.elapsed(),
            Duration::from_millis(100),
            Duration::from_millis(2),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_head_wakes_the_next_waiter() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await;
        let start = Instant::now();
        let done = Arc::new(StdMutex::new(Vec::new()));
        let head = spawn_acquire(&limiter, 5000, "head", &done, start).await;
        let next = spawn_acquire(&limiter, 100, "next", &done, start).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        head.abort();
        next.await.unwrap();
        let done = done.lock().unwrap().clone();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, "next");
        // 200 tokens were already in the bucket when the head left.
        assert_close(
            done[0].1,
            Duration::from_millis(200),
            Duration::from_millis(2),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_waiter_does_not_block_the_line() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await;
        let start = Instant::now();
        let done = Arc::new(StdMutex::new(Vec::new()));
        let a = spawn_acquire(&limiter, 500, "a", &done, start).await;
        let b = spawn_acquire(&limiter, 500, "b", &done, start).await;
        let c = spawn_acquire(&limiter, 100, "c", &done, start).await;
        b.abort();
        a.await.unwrap();
        c.await.unwrap();
        let done = done.lock().unwrap().clone();
        let order: Vec<_> = done.iter().map(|(label, _)| *label).collect();
        assert_eq!(order, ["a", "c"]);
        assert_close(
            done[1].1,
            Duration::from_millis(600),
            Duration::from_millis(3),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_an_unpolled_or_finished_request_is_harmless() {
        let limiter = BandwidthLimiter::new(rate(1000));
        drop(limiter.acquire(10_000));
        let start = Instant::now();
        limiter.acquire(1000).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        let mut finished = limiter.acquire(0);
        (&mut finished).await;
        drop(finished);
        assert!(lock(&limiter.bucket).waiters.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn rate_change_applies_to_current_waiters() {
        let limiter = BandwidthLimiter::new(rate(1000));
        limiter.acquire(1000).await;
        let start = Instant::now();
        let done = Arc::new(StdMutex::new(Vec::new()));
        let waiter = spawn_acquire(&limiter, 1000, "w", &done, start).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        // 100 bytes collected at 1000 B/s; the remaining 900 at 10,000 B/s.
        limiter.set_rate(rate(10_000));
        waiter.await.unwrap();
        let finished = done.lock().unwrap()[0].1;
        assert_close(
            finished,
            Duration::from_millis(190),
            Duration::from_millis(3),
        );

        // Lowering the rate slows waiters down the same way.
        limiter.acquire(4096).await; // drain the new, larger bucket
        let start = Instant::now();
        let slow = tokio::spawn(limiter.acquire(1000));
        yield_now().await;
        limiter.set_rate(rate(500));
        slow.await.unwrap();
        assert_close(
            start.elapsed(),
            Duration::from_secs(2),
            Duration::from_millis(3),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn switching_to_unlimited_releases_everyone() {
        let limiter = BandwidthLimiter::new(rate(10));
        limiter.acquire(10).await;
        let start = Instant::now();
        let a = tokio::spawn(limiter.acquire(1_000_000));
        let b = tokio::spawn(limiter.acquire(1_000_000));
        yield_now().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.set_rate(None);
        a.await.unwrap();
        b.await.unwrap();
        assert_close(
            start.elapsed(),
            Duration::from_secs(1),
            Duration::from_millis(2),
        );
        assert!(lock(&limiter.bucket).waiters.is_empty());

        // Back to a limited rate: the bucket starts full.
        limiter.set_rate(rate(8192));
        let start = Instant::now();
        limiter.acquire(4096).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn aggregate_rate_is_shared_by_concurrent_users() {
        let bytes_per_sec = 512 * 1024;
        let limiter = BandwidthLimiter::new(rate(bytes_per_sec));
        let start = Instant::now();
        let mut users = Vec::new();
        for _ in 0..4 {
            let limiter = limiter.clone();
            users.push(tokio::spawn(async move {
                for _ in 0..10 {
                    limiter.acquire(64 * 1024).await;
                }
            }));
        }
        for user in users {
            user.await.unwrap();
        }
        let total = 4.0 * 10.0 * 64.0 * 1024.0;
        let burst = capacity_for(bytes_per_sec) as f64;
        let expected = Duration::from_secs_f64((total - burst) / bytes_per_sec as f64);
        assert_close(start.elapsed(), expected, expected / 50);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_users_get_equal_shares() {
        let limiter = BandwidthLimiter::new(rate(64 * 1024));
        // Drain the initial burst so that both users start out waiting.
        limiter.acquire(capacity_for(64 * 1024)).await;
        let counters: Vec<_> = (0..2).map(|_| Arc::new(StdMutex::new(0u64))).collect();
        let mut users = Vec::new();
        for counter in &counters {
            let limiter = limiter.clone();
            let counter = Arc::clone(counter);
            users.push(tokio::spawn(async move {
                loop {
                    limiter.acquire(4096).await;
                    *counter.lock().unwrap() += 4096;
                }
            }));
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
        for user in &users {
            user.abort();
        }
        let a = *counters[0].lock().unwrap();
        let b = *counters[1].lock().unwrap();
        // Strict alternation: the users differ by at most the piece in flight.
        assert!(a.abs_diff(b) <= 4096, "unfair split: {a} vs {b}");
        let total = a + b;
        let expected = 4 * 64 * 1024;
        assert!(
            total.abs_diff(expected) <= 4096,
            "total {total}, expected {expected}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_stream_rechunks_and_preserves_content() {
        let first: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let last: Vec<u8> = (0..70 * 1024).map(|i| (i % 13) as u8).collect();
        let chunks = vec![
            Ok::<_, io::Error>(Bytes::from(first.clone())),
            Ok(Bytes::from_static(b"0123456789")),
            Ok(Bytes::new()),
            Ok(Bytes::from(last.clone())),
        ];
        let limiter = BandwidthLimiter::unlimited();
        let pieces: Vec<Bytes> = limiter
            .throttle_stream(stream::iter(chunks))
            .map(|piece| piece.unwrap())
            .collect()
            .await;
        assert!(pieces.iter().all(|p| !p.is_empty() && p.len() <= MAX_CHUNK));
        let sizes: Vec<usize> = pieces.iter().map(Bytes::len).collect();
        assert_eq!(
            sizes,
            [65536, 65536, 65536, 8192, 10, 65536, 6144],
            "chunks are split, never merged"
        );
        let joined: Vec<u8> = pieces.iter().flat_map(|p| p.iter().copied()).collect();
        let mut expected = first;
        expected.extend_from_slice(b"0123456789");
        expected.extend_from_slice(&last);
        assert_eq!(joined, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_stream_honours_the_rate() {
        let bytes_per_sec = 64 * 1024;
        let limiter = BandwidthLimiter::new(rate(bytes_per_sec));
        let data = Bytes::from(vec![7u8; 256 * 1024]);
        let start = Instant::now();
        let mut stream = limiter.throttle_stream(stream::iter([Ok::<_, io::Error>(data)]));
        let mut total = 0;
        while let Some(piece) = stream.next().await {
            total += piece.unwrap().len();
        }
        assert_eq!(total, 256 * 1024);
        let burst = capacity_for(bytes_per_sec) as f64;
        let expected = Duration::from_secs_f64((256.0 * 1024.0 - burst) / bytes_per_sec as f64);
        assert_close(start.elapsed(), expected, Duration::from_millis(10));
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_stream_passes_errors_through() {
        let items = vec![
            Ok(Bytes::from_static(b"abc")),
            Err("boom"),
            Ok(Bytes::from_static(b"def")),
        ];
        let limiter = BandwidthLimiter::new(rate(1000));
        let out: Vec<Result<Bytes, &str>> =
            limiter.throttle_stream(stream::iter(items)).collect().await;
        assert_eq!(
            out,
            vec![
                Ok(Bytes::from_static(b"abc")),
                Err("boom"),
                Ok(Bytes::from_static(b"def")),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_read_preserves_content_and_rate() {
        let data: Vec<u8> = (0..200 * 1024).map(|i| (i % 241) as u8).collect();
        let bytes_per_sec = 100 * 1024;
        let limiter = BandwidthLimiter::new(rate(bytes_per_sec));
        let start = Instant::now();
        let mut reader = limiter.throttle_read(&data[..]);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, data);
        let burst = capacity_for(bytes_per_sec) as f64;
        let expected = Duration::from_secs_f64((200.0 * 1024.0 - burst) / bytes_per_sec as f64);
        assert_close(start.elapsed(), expected, Duration::from_millis(10));
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_read_handles_small_caller_buffers() {
        let data: Vec<u8> = (0..10_000).map(|i| (i % 7) as u8).collect();
        let limiter = BandwidthLimiter::unlimited();
        let mut reader = limiter.throttle_read(&data[..]);
        let mut out = Vec::new();
        let mut small = [0u8; 333];
        loop {
            let n = reader.read(&mut small).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&small[..n]);
        }
        assert_eq!(out, data);
        assert_eq!(reader.read(&mut []).await.unwrap(), 0);
    }
}
