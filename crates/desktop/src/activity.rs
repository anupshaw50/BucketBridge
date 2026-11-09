//! What the app is busy with (transfers and sync runs) and the things that
//! follow from it: the tray tooltip, the taskbar and Dock progress, keeping
//! the computer awake, and a pending "quit when idle".
//!
//! [`summarize`] and [`Activity`] are pure; [`spawn_monitor`] polls once a
//! second on its own thread (the awake guard of some platforms is not
//! `Send`, so it never leaves that thread).

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bucketbridge_core::model::{TransferState, TransferTask};
use chrono::{DateTime, Utc};
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::Core;

/// How often the monitor looks at the queue.
const POLL: Duration = Duration::from_secs(1);

/// Transfers and sync runs at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    /// Transfers in progress.
    pub running: usize,
    /// Transfers waiting for a slot.
    pub queued: usize,
    /// Sync runs executing.
    pub sync_runs: usize,
    /// Bytes done by the tasks counted for the progress.
    pub bytes_done: u64,
    /// Bytes of those tasks, when every one of them knows its size.
    pub bytes_total: Option<u64>,
}

impl Activity {
    /// Active transfers: running or about to run.
    pub fn transfers(&self) -> usize {
        self.running + self.queued
    }

    /// Whether anything is running or about to run.
    pub fn busy(&self) -> bool {
        self.transfers() > 0 || self.sync_runs > 0
    }

    /// Overall progress, 0 to 100, when the sizes are known.
    pub fn percent(&self) -> Option<u64> {
        let total = self.bytes_total.filter(|total| *total > 0)?;
        let percent = u128::from(self.bytes_done.min(total)) * 100 / u128::from(total);
        u64::try_from(percent).ok()
    }

    /// The tray tooltip.
    pub fn tooltip(&self, waiting_to_quit: bool) -> String {
        let mut parts = Vec::new();
        if self.transfers() > 0 {
            let noun = if self.transfers() == 1 {
                "transfer"
            } else {
                "transfers"
            };
            let mut text = format!("{} {noun}", self.transfers());
            if let Some(percent) = self.percent() {
                text.push_str(&format!(", {percent}%"));
            }
            parts.push(text);
        }
        if self.sync_runs > 0 {
            let noun = if self.sync_runs == 1 { "sync" } else { "syncs" };
            parts.push(format!("{} {noun} running", self.sync_runs));
        }
        let mut text = if parts.is_empty() {
            "BucketBridge".to_owned()
        } else {
            format!("BucketBridge: {}", parts.join(", "))
        };
        if waiting_to_quit {
            text.push_str(" (quits when done)");
        }
        text
    }

    /// The taskbar / Dock progress bar for this activity.
    pub fn progress_bar(&self) -> ProgressBarState {
        if !self.busy() {
            return ProgressBarState {
                status: Some(ProgressBarStatus::None),
                progress: None,
            };
        }
        match self.percent() {
            Some(percent) if self.transfers() > 0 => ProgressBarState {
                status: Some(ProgressBarStatus::Normal),
                progress: Some(percent),
            },
            _ => ProgressBarState {
                status: Some(ProgressBarStatus::Indeterminate),
                progress: None,
            },
        }
    }
}

/// Counts the queue's active tasks. Tasks that completed since `since` are
/// part of the progress, so the bar does not jump back each time one ends.
pub fn summarize(
    tasks: &[TransferTask],
    sync_runs: usize,
    since: Option<DateTime<Utc>>,
) -> Activity {
    let mut activity = Activity {
        sync_runs,
        ..Activity::default()
    };
    let mut done = 0u64;
    let mut total = 0u64;
    let mut known = true;
    for task in tasks {
        match task.state {
            TransferState::Running | TransferState::Queued => {
                if task.state == TransferState::Running {
                    activity.running += 1;
                } else {
                    activity.queued += 1;
                }
                done = done.saturating_add(task.bytes_done);
                match task.bytes_total {
                    Some(bytes) => total = total.saturating_add(bytes),
                    None => known = false,
                }
            }
            TransferState::Completed if since.is_some_and(|since| task.updated_at >= since) => {
                let bytes = task.bytes_total.unwrap_or(task.bytes_done);
                done = done.saturating_add(bytes);
                total = total.saturating_add(bytes);
            }
            _ => {}
        }
    }
    activity.bytes_done = done;
    activity.bytes_total = known.then_some(total);
    activity
}

/// Holds the computer awake while it is `set(true)`.
#[derive(Default)]
pub(crate) struct Awake {
    guard: Option<keepawake::KeepAwake>,
    failed: bool,
}

impl Awake {
    /// Takes or releases the hold. A platform refusal is logged once per
    /// busy period.
    pub(crate) fn set(&mut self, want: bool, app_id: &str) {
        match (want, self.guard.is_some()) {
            (true, false) if !self.failed => {
                let created = keepawake::Builder::default()
                    .idle(true)
                    .sleep(true)
                    .reason("Transfers or a sync are running")
                    .app_name("BucketBridge")
                    .app_reverse_domain(app_id)
                    .create();
                match created {
                    Ok(guard) => self.guard = Some(guard),
                    Err(error) => {
                        self.failed = true;
                        tracing::warn!(
                            target: "bucketbridge::desktop",
                            "the computer cannot be kept awake: {error}"
                        );
                    }
                }
            }
            (false, true) => self.guard = None,
            _ => {}
        }
        if !want {
            self.failed = false;
        }
    }

    #[cfg(test)]
    fn held(&self) -> bool {
        self.guard.is_some()
    }
}

/// Starts the monitor thread. It ends with the process.
pub(crate) fn spawn_monitor<R: Runtime>(app: AppHandle<R>, core: Arc<Core>) {
    let spawned = thread::Builder::new()
        .name("bb-activity".to_owned())
        .spawn(move || monitor(&app, &core));
    if let Err(error) = spawned {
        tracing::warn!(target: "bucketbridge::desktop", "activity monitor not started: {error}");
    }
}

fn monitor<R: Runtime>(app: &AppHandle<R>, core: &Core) {
    let app_id = app.config().identifier.clone();
    let mut awake = Awake::default();
    let mut batch_start: Option<DateTime<Utc>> = None;
    let mut last: Option<(Activity, bool, bool)> = None;
    loop {
        thread::sleep(POLL);
        let busy_before = last.is_some_and(|(activity, ..)| activity.busy());
        let since = if busy_before { batch_start } else { None };
        let activity = core.activity(since);
        if activity.busy() && !busy_before {
            batch_start = Some(Utc::now());
        }
        let keep_awake = core.lifecycle.keep_awake() && activity.busy();
        awake.set(keep_awake, &app_id);
        let waiting = core.lifecycle.quit_when_idle();
        if waiting && !activity.busy() {
            tracing::info!(target: "bucketbridge::app", "quitting: the active tasks have finished");
            crate::quit::exit_now(app);
            return;
        }
        let state = (activity, waiting, keep_awake);
        if last == Some(state) {
            continue;
        }
        last = Some(state);
        crate::tray::set_tooltip(app, &activity.tooltip(waiting));
        let bar = activity.progress_bar();
        for window in app.webview_windows().values() {
            // Best effort: not every desktop shows a progress bar.
            let _ = window.set_progress_bar(ProgressBarState {
                status: bar.status,
                progress: bar.progress,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::model::{Location, TransferKind};
    use chrono::TimeZone;

    fn task(state: TransferState, done: u64, total: Option<u64>, at: i64) -> TransferTask {
        let when = Utc.timestamp_opt(at, 0).unwrap();
        TransferTask {
            id: "t".into(),
            account_id: "a".into(),
            group_id: None,
            kind: TransferKind::Upload,
            src: Location::Local { path: "/x".into() },
            dst: None,
            bytes_total: total,
            bytes_done: done,
            speed: 0,
            eta: None,
            state,
            error: None,
            attempts: 0,
            created_at: when,
            updated_at: when,
        }
    }

    #[test]
    fn idle_queue_is_not_busy() {
        let tasks = [
            task(TransferState::Stopped, 5, Some(10), 0),
            task(TransferState::Failed, 5, Some(10), 0),
            task(TransferState::Completed, 10, Some(10), 0),
        ];
        let activity = summarize(&tasks, 0, None);
        assert!(!activity.busy());
        assert_eq!(activity.tooltip(false), "BucketBridge");
        assert!(matches!(
            activity.progress_bar().status,
            Some(ProgressBarStatus::None)
        ));
    }

    #[test]
    fn counts_and_percent_of_active_tasks() {
        let tasks = [
            task(TransferState::Running, 50, Some(100), 0),
            task(TransferState::Queued, 0, Some(100), 0),
            task(TransferState::Stopped, 99, Some(100), 0),
        ];
        let activity = summarize(&tasks, 0, None);
        assert_eq!((activity.running, activity.queued), (1, 1));
        assert_eq!(activity.percent(), Some(25));
        assert_eq!(activity.tooltip(false), "BucketBridge: 2 transfers, 25%");
        let bar = activity.progress_bar();
        assert!(matches!(bar.status, Some(ProgressBarStatus::Normal)));
        assert_eq!(bar.progress, Some(25));
    }

    #[test]
    fn completed_tasks_of_the_batch_keep_the_bar_from_jumping_back() {
        let tasks = [
            task(TransferState::Completed, 100, Some(100), 20),
            task(TransferState::Completed, 100, Some(100), 5),
            task(TransferState::Running, 0, Some(100), 20),
        ];
        let since = Utc.timestamp_opt(10, 0).unwrap();
        let activity = summarize(&tasks, 0, Some(since));
        assert_eq!(activity.percent(), Some(50));
        assert_eq!(activity.tooltip(false), "BucketBridge: 1 transfer, 50%");
    }

    #[test]
    fn unknown_sizes_and_sync_runs_are_indeterminate() {
        let tasks = [task(TransferState::Running, 5, None, 0)];
        let activity = summarize(&tasks, 1, None);
        assert_eq!(activity.percent(), None);
        assert_eq!(
            activity.tooltip(true),
            "BucketBridge: 1 transfer, 1 sync running (quits when done)"
        );
        assert!(matches!(
            activity.progress_bar().status,
            Some(ProgressBarStatus::Indeterminate)
        ));
        let sync_only = summarize(&[], 2, None);
        assert!(sync_only.busy());
        assert_eq!(sync_only.tooltip(false), "BucketBridge: 2 syncs running");
        assert!(matches!(
            sync_only.progress_bar().status,
            Some(ProgressBarStatus::Indeterminate)
        ));
    }

    #[test]
    fn done_never_exceeds_total() {
        let tasks = [task(TransferState::Running, 500, Some(100), 0)];
        assert_eq!(summarize(&tasks, 0, None).percent(), Some(100));
        let empty = [task(TransferState::Running, 0, Some(0), 0)];
        assert_eq!(summarize(&empty, 0, None).percent(), None);
    }

    #[test]
    fn releasing_without_a_hold_is_harmless() {
        let mut awake = Awake::default();
        awake.set(false, "io.example.app");
        assert!(!awake.held());
    }
}
