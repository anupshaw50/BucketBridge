//! Retry policy: which errors are retried, and capped exponential backoff with
//! full jitter.
//!
//! The delay before retry `n` (1-based) is drawn uniformly from
//! `[0, min(cap, base * 2^(n-1))]` ("full jitter"), which spreads retries of
//! many clients that failed together. Randomness is injected through
//! [`RandomSource`] so tests can make delays deterministic.
//!
//! Retryable error kinds are `remote` (S3 or network failures) and `io`
//! (local filesystem hiccups). Everything else needs a different input or a
//! user decision and is never retried: `usage`, `auth`, `locked`,
//! `mfa_required`, `not_found`, `conflict`, `cancelled` and `internal`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, ErrorKind};
use crate::settings::TransferSettings;

/// Whether a failure of this kind may succeed when simply tried again.
pub fn is_retryable(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Remote | ErrorKind::Io)
}

/// A source of uniformly distributed 64-bit values.
pub trait RandomSource {
    /// The next value, uniformly distributed over all of `u64`.
    fn next_u64(&mut self) -> u64;
}

impl<F: FnMut() -> u64> RandomSource for F {
    fn next_u64(&mut self) -> u64 {
        self()
    }
}

/// Small, fast, seedable generator (SplitMix64). Not for cryptographic use.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator with a fixed seed: the same seed yields the same sequence.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A generator seeded from the process' hash randomness, the clock and a
    /// counter, so every call yields a different sequence.
    pub fn from_entropy() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut hasher = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        hasher.write_u128(nanos);
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        Self::new(hasher.finish())
    }
}

impl RandomSource for SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Capped exponential backoff with full jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
}

impl Backoff {
    /// Default first-retry ceiling.
    pub const DEFAULT_BASE: Duration = Duration::from_secs(1);
    /// Default largest delay.
    pub const DEFAULT_CAP: Duration = Duration::from_secs(30);

    /// Backoff whose ceiling starts at `base` and doubles up to `cap`.
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// Ceiling of the delay before the first retry.
    pub fn base(&self) -> Duration {
        self.base
    }

    /// Largest delay before any retry.
    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// Largest delay before retry `retry` (1-based; 0 is treated as 1):
    /// `min(cap, base * 2^(retry - 1))`.
    pub fn ceiling(&self, retry: u32) -> Duration {
        let doublings = retry.saturating_sub(1);
        let base = self.base.as_nanos();
        let cap = self.cap.as_nanos();
        let ceiling = if doublings >= 128 {
            cap
        } else {
            base.checked_mul(1u128 << doublings)
                .map_or(cap, |c| c.min(cap))
        };
        let ceiling = if base == 0 { 0 } else { ceiling };
        Duration::from_nanos(u64::try_from(ceiling).unwrap_or(u64::MAX))
    }

    /// Delay before retry `retry`, uniform in `[0, ceiling(retry)]`.
    pub fn delay<R: RandomSource + ?Sized>(&self, retry: u32, random: &mut R) -> Duration {
        let ceiling = u128::from(u64::try_from(self.ceiling(retry).as_nanos()).unwrap_or(u64::MAX));
        // Multiply-shift maps a 64-bit value onto [0, ceiling] without bias
        // worth caring about and without division.
        let nanos = (u128::from(random.next_u64()) * (ceiling + 1)) >> 64;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BASE, Self::DEFAULT_CAP)
    }
}

/// How often and how patiently a failed operation is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt (so at most `max_retries + 1` attempts).
    pub max_retries: u32,
    /// Delay between attempts.
    pub backoff: Backoff,
}

impl RetryPolicy {
    /// At most `max_retries` retries, spaced by `backoff`.
    pub fn new(max_retries: u32, backoff: Backoff) -> Self {
        Self {
            max_retries,
            backoff,
        }
    }

    /// `retry_attempts` from the settings with the default backoff.
    pub fn from_settings(settings: &TransferSettings) -> Self {
        Self::new(settings.retry_attempts, Backoff::default())
    }

    /// The delay before the next retry, or `None` when `error` is not
    /// retryable or `retries_done` retries were already spent.
    pub fn next_delay<R: RandomSource + ?Sized>(
        &self,
        error: &Error,
        retries_done: u32,
        random: &mut R,
    ) -> Option<Duration> {
        if !is_retryable(error.kind()) || retries_done >= self.max_retries {
            return None;
        }
        Some(self.backoff.delay(retries_done.saturating_add(1), random))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [ErrorKind; 10] = [
        ErrorKind::Usage,
        ErrorKind::Auth,
        ErrorKind::Locked,
        ErrorKind::MfaRequired,
        ErrorKind::NotFound,
        ErrorKind::Conflict,
        ErrorKind::Remote,
        ErrorKind::Io,
        ErrorKind::Cancelled,
        ErrorKind::Internal,
    ];

    fn error(kind: ErrorKind) -> Error {
        super::super::new_error(kind, "test failure")
    }

    #[test]
    fn only_remote_and_io_are_retryable() {
        let retryable: Vec<ErrorKind> =
            ALL_KINDS.into_iter().filter(|k| is_retryable(*k)).collect();
        assert_eq!(retryable, [ErrorKind::Remote, ErrorKind::Io]);
    }

    #[test]
    fn ceiling_doubles_up_to_the_cap() {
        let backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        let ceilings: Vec<u128> = (0..=6).map(|n| backoff.ceiling(n).as_millis()).collect();
        assert_eq!(ceilings, [100, 100, 200, 400, 800, 1000, 1000]);
        assert_eq!(backoff.ceiling(u32::MAX), Duration::from_secs(1));
        assert_eq!(
            Backoff::new(Duration::ZERO, Duration::from_secs(5)).ceiling(9),
            Duration::ZERO
        );
        let huge = Backoff::new(Duration::MAX, Duration::MAX);
        assert_eq!(huge.ceiling(200), Duration::from_nanos(u64::MAX));
        let defaults = Backoff::default();
        assert_eq!(
            (defaults.base(), defaults.cap()),
            (Duration::from_secs(1), Duration::from_secs(30))
        );
    }

    #[test]
    fn delay_spans_zero_to_the_ceiling() {
        let backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut low = || 0u64;
        let mut high = || u64::MAX;
        let mut middle = || 1u64 << 63;
        assert_eq!(backoff.delay(3, &mut low), Duration::ZERO);
        assert_eq!(backoff.delay(3, &mut high), Duration::from_millis(400));
        assert_eq!(
            backoff.delay(3, &mut middle),
            Duration::from_nanos(200_000_000)
        );
        assert_eq!(backoff.delay(9, &mut high), Duration::from_secs(1));
    }

    #[test]
    fn jitter_stays_in_range_and_is_spread() {
        let backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        let mut random = SplitMix64::new(42);
        let delays: Vec<Duration> = (0..1000).map(|_| backoff.delay(4, &mut random)).collect();
        assert!(delays.iter().all(|d| *d <= Duration::from_millis(800)));
        let below_half = delays
            .iter()
            .filter(|d| **d < Duration::from_millis(400))
            .count();
        assert!(
            (400..600).contains(&below_half),
            "uneven spread: {below_half}"
        );
    }

    #[test]
    fn seeded_generator_is_deterministic() {
        let mut a = SplitMix64::new(7);
        let mut b = SplitMix64::new(7);
        let first: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let second: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        assert_eq!(first, second);
        // Known first output of SplitMix64 for seed 0.
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
        let mut c = SplitMix64::from_entropy();
        let mut d = SplitMix64::from_entropy();
        assert_ne!(c.next_u64(), d.next_u64());
    }

    #[test]
    fn policy_respects_kind_and_budget() {
        let policy = RetryPolicy::new(
            2,
            Backoff::new(Duration::from_secs(1), Duration::from_secs(8)),
        );
        let mut high = || u64::MAX;
        let remote = error(ErrorKind::Remote);
        assert_eq!(
            policy.next_delay(&remote, 0, &mut high),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            policy.next_delay(&remote, 1, &mut high),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.next_delay(&remote, 2, &mut high),
            None,
            "budget spent"
        );
        assert!(
            policy
                .next_delay(&error(ErrorKind::Io), 0, &mut high)
                .is_some()
        );
        for kind in ALL_KINDS.into_iter().filter(|k| !is_retryable(*k)) {
            assert_eq!(
                policy.next_delay(&error(kind), 0, &mut high),
                None,
                "{kind:?}"
            );
        }
        let none = RetryPolicy::new(0, Backoff::default());
        assert_eq!(none.next_delay(&remote, 0, &mut high), None);
        let from_settings = RetryPolicy::from_settings(&TransferSettings::default());
        assert_eq!(from_settings.max_retries, 3);
    }
}
