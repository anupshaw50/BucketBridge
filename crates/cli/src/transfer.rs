//! Engine hooks for `cp` and `mv`: the shared bandwidth limit, progress
//! counters and the progress line on a terminal.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bucketbridge_core::engine::{
    DEFAULT_MAX_GRANT, Progress, Throttle, ThrottleFuture, Unthrottled,
};
use bucketbridge_core::settings::TransferSettings;
use bucketbridge_core::transfer::BandwidthLimiter;
use tokio::task::JoinHandle;

use crate::i18n::trf;
use crate::output::Stderr;

/// How often the progress line is redrawn.
const REDRAW_INTERVAL: Duration = Duration::from_millis(250);

/// The bandwidth limit of the settings, shared by every transfer of the
/// invocation.
pub fn throttle(settings: &TransferSettings) -> Arc<dyn Throttle> {
    let rate = settings
        .bandwidth_limit_kibps
        .and_then(|kibps| kibps.checked_mul(1024))
        .and_then(NonZeroU64::new);
    match rate {
        Some(rate) => Arc::new(LimiterThrottle {
            limiter: BandwidthLimiter::new(Some(rate)),
            max_grant: DEFAULT_MAX_GRANT.min(rate.get()),
        }),
        None => Arc::new(Unthrottled),
    }
}

/// A [`BandwidthLimiter`] as an engine [`Throttle`]. Grants are at most one
/// second's worth of bytes, so a slow limit never keeps a stream silent long
/// enough to trip the SDK's stalled-stream protection.
struct LimiterThrottle {
    limiter: BandwidthLimiter,
    max_grant: u64,
}

impl Throttle for LimiterThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(self.limiter.acquire(bytes))
    }

    fn max_grant(&self) -> u64 {
        self.max_grant
    }
}

/// Byte and item counters over all transfers of an invocation.
#[derive(Debug, Default)]
pub struct Totals {
    total: AtomicU64,
    done: AtomicU64,
    items: AtomicUsize,
    items_done: AtomicUsize,
}

impl Totals {
    /// Counters for `items` transfers of `bytes` bytes (as far as known
    /// before they start; each transfer corrects its own share).
    pub fn new(items: usize, bytes: u64) -> Arc<Self> {
        Arc::new(Totals {
            total: AtomicU64::new(bytes),
            items: AtomicUsize::new(items),
            ..Totals::default()
        })
    }

    /// One transfer finished (successfully or not).
    pub fn item_finished(&self) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
    }

    /// The current progress as one line of text.
    pub fn line(&self, elapsed: Duration) -> String {
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed).min(total);
        let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0);
        let seconds = elapsed.as_secs_f64().max(0.001);
        // Display only: rounding to whole bytes per second is fine.
        let rate = (done as f64 / seconds) as u64;
        trf(
            "{done} of {total} ({percent}%), {rate}/s, {items_done} of {items} done",
            &[
                ("done", human_bytes(done).as_str()),
                ("total", human_bytes(total).as_str()),
                ("percent", percent.to_string().as_str()),
                ("rate", human_bytes(rate).as_str()),
                ("items_done", self.items_done.load(Ordering::Relaxed).to_string().as_str()),
                ("items", self.items.load(Ordering::Relaxed).to_string().as_str()),
            ],
        )
    }
}

/// The progress of one transfer, added into the shared [`Totals`].
pub struct ItemProgress {
    totals: Arc<Totals>,
    total: AtomicU64,
    done: AtomicU64,
}

impl ItemProgress {
    /// Progress feeding `totals`, whose byte total already counts
    /// `expected` bytes for this transfer.
    pub fn new(totals: Arc<Totals>, expected: u64) -> Arc<Self> {
        Arc::new(ItemProgress {
            totals,
            total: AtomicU64::new(expected),
            done: AtomicU64::new(0),
        })
    }
}

/// Moves `counter` by the difference between `new` and `old`.
fn shift(counter: &AtomicU64, old: u64, new: u64) {
    if new >= old {
        counter.fetch_add(new - old, Ordering::Relaxed);
    } else {
        counter.fetch_sub(old - new, Ordering::Relaxed);
    }
}

impl Progress for ItemProgress {
    fn set_total(&self, total: u64) {
        let old = self.total.swap(total, Ordering::Relaxed);
        shift(&self.totals.total, old, total);
    }

    fn set_done(&self, done: u64) {
        let old = self.done.swap(done, Ordering::Relaxed);
        shift(&self.totals.done, old, done);
    }

    fn add_done(&self, amount: u64) {
        self.done.fetch_add(amount, Ordering::Relaxed);
        self.totals.done.fetch_add(amount, Ordering::Relaxed);
    }
}

/// `12.3 MiB` style sizes.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // Display only: precision loss above 2^53 bytes is irrelevant.
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// What is on the terminal line right now.
#[derive(Debug, Default)]
struct LineState {
    /// Characters drawn (0 when the line is clear).
    width: usize,
    /// Set once the line is stopped; nothing is drawn after that.
    stopped: bool,
}

/// A progress line redrawn on standard error four times a second. Output
/// printed through [`ProgressLine::print_above`] appears above it; stopping
/// (or dropping) it clears the line.
pub struct ProgressLine {
    stderr: Stderr,
    state: Arc<Mutex<LineState>>,
    task: JoinHandle<()>,
}

fn lock(state: &Mutex<LineState>) -> MutexGuard<'_, LineState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProgressLine {
    /// Starts drawing `totals` on `stderr`.
    pub fn start(totals: Arc<Totals>, stderr: Stderr) -> Self {
        let state = Arc::new(Mutex::new(LineState::default()));
        let started = Instant::now();
        let task = tokio::spawn({
            let stderr = stderr.clone();
            let state = Arc::clone(&state);
            async move {
                let mut ticker = tokio::time::interval(REDRAW_INTERVAL);
                loop {
                    ticker.tick().await;
                    let line = totals.line(started.elapsed());
                    let width = line.chars().count();
                    let mut state = lock(&state);
                    if state.stopped {
                        return;
                    }
                    let padding = state.width.saturating_sub(width);
                    stderr.raw(&format!("\r{line}{}", " ".repeat(padding)));
                    state.width = width;
                }
            }
        });
        ProgressLine {
            stderr,
            state,
            task,
        }
    }

    /// Runs `print` while the line is cleared; the next redraw puts the line
    /// back below whatever `print` wrote.
    pub fn print_above<T>(&self, print: impl FnOnce() -> T) -> T {
        let mut state = lock(&self.state);
        self.clear(&mut state);
        print()
    }

    fn clear(&self, state: &mut LineState) {
        if state.width > 0 {
            self.stderr.raw(&format!("\r{}\r", " ".repeat(state.width)));
            state.width = 0;
        }
    }
}

impl Drop for ProgressLine {
    fn drop(&mut self) {
        self.task.abort();
        let mut state = lock(&self.state);
        state.stopped = true;
        self.clear(&mut state);
    }
}

/// Runs `print` above the progress line, if there is one.
pub fn print_above<T>(line: Option<&ProgressLine>, print: impl FnOnce() -> T) -> T {
    match line {
        Some(line) => line.print_above(print),
        None => print(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::settings::Settings;
    use std::io::{self, Write};

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
        assert_eq!(human_bytes(u64::MAX), "16384.0 PiB");
    }

    #[test]
    fn item_progress_adds_up() {
        let totals = Totals::new(2, 150);
        let a = ItemProgress::new(Arc::clone(&totals), 100);
        let b = ItemProgress::new(Arc::clone(&totals), 50);
        a.add_done(30);
        b.set_done(20);
        // A transfer that starts over resets its share; a size that turned
        // out different corrects the total.
        a.set_done(10);
        b.set_total(40);
        totals.item_finished();
        assert_eq!(totals.total.load(Ordering::Relaxed), 140);
        assert_eq!(totals.done.load(Ordering::Relaxed), 30);
        let line = totals.line(Duration::from_secs(1));
        assert_eq!(
            line,
            trf(
                "{done} of {total} ({percent}%), {rate}/s, {items_done} of {items} done",
                &[
                    ("done", "30 B"),
                    ("total", "140 B"),
                    ("percent", "21"),
                    ("rate", "30 B"),
                    ("items_done", "1"),
                    ("items", "2"),
                ],
            )
        );
        assert!(
            Totals::new(0, 0)
                .line(Duration::ZERO)
                .starts_with(&trf(
                    "{done} of {total} ({percent}%), {rate}/s, {items_done} of {items} done",
                    &[
                        ("done", "0 B"),
                        ("total", "0 B"),
                        ("percent", "0"),
                        ("rate", "0 B"),
                        ("items_done", "0"),
                        ("items", "0"),
                    ],
                ))
        );
    }
    #[test]
    fn unlimited_settings_do_not_throttle() {
        let settings = Settings::default();
        assert_eq!(throttle(&settings.transfer).max_grant(), u64::MAX);
        let mut limited = settings.transfer.clone();
        limited.bandwidth_limit_kibps = Some(16);
        assert_eq!(throttle(&limited).max_grant(), 16 * 1024);
        limited.bandwidth_limit_kibps = Some(1024 * 1024);
        assert_eq!(throttle(&limited).max_grant(), DEFAULT_MAX_GRANT);
    }
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_line_is_cleared_for_output_and_when_stopped() {
        let captured = Captured::default();
        let totals = Totals::new(1, 2048);
        let line =
            ProgressLine::start(Arc::clone(&totals), Stderr::new(Box::new(captured.clone())));
        tokio::time::sleep(REDRAW_INTERVAL / 2).await;
        let text = || String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(
            text().starts_with('\r')
                && text().contains(&trf(
                    "{done} of {total}",
                    &[("done", "0 B"), ("total", "2.0 KiB")]
                )),
            "{:?}",
            text()
        );
        let width = text().chars().count() - 1;
        line.print_above(|| captured.clone().write_all(b"record\n").unwrap());
        assert!(
            text().ends_with(&format!("\r{}\rrecord\n", " ".repeat(width))),
            "{:?}",
            text()
        );
        tokio::time::sleep(REDRAW_INTERVAL).await;
        assert!(
            text().ends_with(&crate::i18n::trf(
                "{items_done} of {items} done",
                &[("items_done", "0"), ("items", "1")]
            )),
            "{:?}",
            text()
        );
        drop(line);
        let after = text();
        assert!(
            after.ends_with(&format!("\r{}\r", " ".repeat(width))),
            "{after:?}"
        );
        tokio::time::sleep(REDRAW_INTERVAL * 4).await;
        assert_eq!(text(), after, "nothing is drawn after stop");
    }
}
