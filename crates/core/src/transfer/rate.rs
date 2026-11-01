//! Transfer speed estimation and ETA.
//!
//! [`RateEstimator`] turns samples of a cumulative byte counter into a
//! smoothed bytes-per-second figure. Smoothing is an exponentially weighted
//! moving average whose weight depends on the time between samples (time
//! constant `window`), so irregular sampling does not skew the result. When the
//! counter has not moved for `idle_after`, the speed is reported as exactly 0.

use std::time::Duration;

use tokio::time::Instant;

/// Smoothed transfer speed from samples of a cumulative byte counter.
#[derive(Debug, Clone)]
pub struct RateEstimator {
    window: Duration,
    idle_after: Duration,
    last: Option<Sample>,
    last_progress: Option<Instant>,
    rate: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: Instant,
    done: u64,
}

impl RateEstimator {
    /// Default smoothing time constant.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(2);
    /// Default time without progress after which the speed reads 0.
    pub const DEFAULT_IDLE_AFTER: Duration = Duration::from_secs(2);

    /// An estimator with the default window and idle timeout.
    pub fn new() -> Self {
        Self::with_window(Self::DEFAULT_WINDOW, Self::DEFAULT_IDLE_AFTER)
    }

    /// An estimator with a custom smoothing time constant and idle timeout.
    pub fn with_window(window: Duration, idle_after: Duration) -> Self {
        Self {
            window,
            idle_after,
            last: None,
            last_progress: None,
            rate: None,
        }
    }

    /// Records the cumulative number of bytes done at `now`.
    ///
    /// A counter that moves backwards (a runner rolled progress back) starts a
    /// new baseline without affecting the current estimate.
    pub fn record(&mut self, now: Instant, done: u64) {
        let Some(previous) = self.last else {
            self.last = Some(Sample { at: now, done });
            return;
        };
        if done < previous.done {
            self.last = Some(Sample { at: now, done });
            return;
        }
        let elapsed = now.saturating_duration_since(previous.at);
        if elapsed.is_zero() {
            // Nothing to divide by yet; the bytes count towards the next sample.
            return;
        }
        let delta = done - previous.done;
        let seconds = elapsed.as_secs_f64();
        let instant = delta as f64 / seconds;
        if delta > 0 {
            let was_idle = self.rate.is_none() || self.is_idle(previous.at);
            self.rate = Some(match self.rate {
                Some(rate) if !was_idle => rate + self.weight(seconds) * (instant - rate),
                _ => instant,
            });
            self.last_progress = Some(now);
        } else if let Some(rate) = self.rate {
            self.rate = Some(rate - self.weight(seconds) * rate);
        }
        self.last = Some(Sample { at: now, done });
    }

    /// Smoothed speed in bytes per second at `now`; 0 when nothing moved for
    /// the idle timeout or no progress was seen yet.
    pub fn bytes_per_sec(&self, now: Instant) -> u64 {
        match self.rate {
            Some(rate) if !self.is_idle(now) => rate.round() as u64,
            _ => 0,
        }
    }

    /// Forgets all samples.
    pub fn reset(&mut self) {
        self.last = None;
        self.last_progress = None;
        self.rate = None;
    }

    fn is_idle(&self, now: Instant) -> bool {
        self.last_progress
            .is_none_or(|at| now.saturating_duration_since(at) >= self.idle_after)
    }

    fn weight(&self, seconds: f64) -> f64 {
        let window = self.window.as_secs_f64();
        if window <= 0.0 {
            1.0
        } else {
            1.0 - (-seconds / window).exp()
        }
    }
}

impl Default for RateEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Seconds left at `bytes_per_sec`, rounded up.
///
/// `None` when the total is unknown or the speed is 0 (and work remains);
/// `Some(0)` once `done` reached `total`.
pub fn eta_secs(done: u64, total: Option<u64>, bytes_per_sec: u64) -> Option<u64> {
    let remaining = total?.saturating_sub(done);
    if remaining == 0 {
        return Some(0);
    }
    if bytes_per_sec == 0 {
        return None;
    }
    Some(remaining.div_ceil(bytes_per_sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn steady_rate_is_reported_exactly() {
        let start = Instant::now();
        let mut estimator = RateEstimator::new();
        estimator.record(start, 0);
        assert_eq!(estimator.bytes_per_sec(start), 0, "no progress seen yet");
        for step in 1..=20u64 {
            estimator.record(at(start, step * 250), step * 250_000);
        }
        // 250,000 bytes every 250 ms.
        assert_eq!(estimator.bytes_per_sec(at(start, 5000)), 1_000_000);
    }

    #[test]
    fn rate_changes_are_smoothed() {
        let start = Instant::now();
        let mut estimator =
            RateEstimator::with_window(Duration::from_secs(1), Duration::from_secs(5));
        estimator.record(start, 0);
        estimator.record(at(start, 1000), 1000);
        assert_eq!(estimator.bytes_per_sec(at(start, 1000)), 1000);
        // One second at 3000 B/s moves the estimate by 1 - e^-1 of the gap.
        estimator.record(at(start, 2000), 4000);
        let expected = 1000.0 + (1.0 - (-1.0f64).exp()) * 2000.0;
        assert_eq!(
            estimator.bytes_per_sec(at(start, 2000)),
            expected.round() as u64
        );
        // Many more seconds at 3000 B/s converge on it.
        for second in 3..=20u64 {
            estimator.record(at(start, second * 1000), 4000 + (second - 2) * 3000);
        }
        assert_eq!(estimator.bytes_per_sec(at(start, 20_000)), 3000);
    }

    #[test]
    fn speed_drops_to_zero_when_idle() {
        let start = Instant::now();
        let mut estimator =
            RateEstimator::with_window(Duration::from_secs(2), Duration::from_secs(2));
        estimator.record(start, 0);
        estimator.record(at(start, 500), 50_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 500)), 100_000);
        // No new bytes: the estimate decays...
        estimator.record(at(start, 1500), 50_000);
        let decayed = estimator.bytes_per_sec(at(start, 1500));
        assert!(decayed > 0 && decayed < 100_000, "decayed to {decayed}");
        // ...and reads exactly zero once idle for two seconds.
        assert_eq!(estimator.bytes_per_sec(at(start, 2500)), 0);
        estimator.record(at(start, 3000), 50_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 3000)), 0);
        // Progress after an idle period restarts from the observed rate.
        estimator.record(at(start, 3500), 60_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 3500)), 20_000);
    }

    #[test]
    fn irregular_samples_and_duplicates_are_handled() {
        let start = Instant::now();
        let mut estimator = RateEstimator::new();
        estimator.record(start, 0);
        // Two samples at the same instant: the bytes count towards the next one.
        estimator.record(start, 1000);
        estimator.record(at(start, 100), 2000);
        assert_eq!(estimator.bytes_per_sec(at(start, 100)), 20_000);
        // An earlier timestamp never panics or divides by zero.
        estimator.record(start, 3000);
        assert_eq!(estimator.bytes_per_sec(at(start, 100)), 20_000);
    }

    #[test]
    fn counter_going_backwards_rebaselines() {
        let start = Instant::now();
        let mut estimator = RateEstimator::new();
        estimator.record(start, 0);
        estimator.record(at(start, 1000), 10_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 1000)), 10_000);
        // A runner rolls progress back (part retried): no negative rate.
        estimator.record(at(start, 1100), 5_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 1100)), 10_000);
        estimator.record(at(start, 2100), 15_000);
        assert_eq!(estimator.bytes_per_sec(at(start, 2100)), 10_000);
    }

    #[test]
    fn reset_forgets_everything() {
        let start = Instant::now();
        let mut estimator = RateEstimator::new();
        estimator.record(start, 0);
        estimator.record(at(start, 1000), 5000);
        estimator.reset();
        assert_eq!(estimator.bytes_per_sec(at(start, 1000)), 0);
        estimator.record(at(start, 1000), 5000);
        estimator.record(at(start, 2000), 6000);
        assert_eq!(estimator.bytes_per_sec(at(start, 2000)), 1000);
    }

    #[test]
    fn zero_window_follows_the_latest_sample() {
        let start = Instant::now();
        let mut estimator = RateEstimator::with_window(Duration::ZERO, Duration::from_secs(1));
        estimator.record(start, 0);
        estimator.record(at(start, 1000), 1000);
        estimator.record(at(start, 1500), 3000);
        assert_eq!(estimator.bytes_per_sec(at(start, 1500)), 4000);
    }

    #[test]
    fn eta_rules() {
        assert_eq!(eta_secs(0, None, 100), None, "unknown total");
        assert_eq!(eta_secs(0, Some(1000), 0), None, "no speed");
        assert_eq!(eta_secs(1000, Some(1000), 0), Some(0), "done");
        assert_eq!(eta_secs(2000, Some(1000), 0), Some(0), "overshoot");
        assert_eq!(eta_secs(0, Some(1000), 100), Some(10));
        assert_eq!(eta_secs(1, Some(1000), 100), Some(10), "rounded up");
        assert_eq!(eta_secs(0, Some(1001), 100), Some(11));
        assert_eq!(eta_secs(0, Some(u64::MAX), 1), Some(u64::MAX));
    }
}
