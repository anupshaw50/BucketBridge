//! Quitting while tasks are active, and the small amount of process-wide
//! state the everyday-use features share ([`Lifecycle`]).
//!
//! Every way to quit goes through [`request_quit`]: the close button of the
//! last window, the tray Quit item and the system quit (Cmd-Q).

use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};

use bucketbridge_core::Settings;
use bucketbridge_core::settings::QuitWithTasks;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::activity::Activity;
use crate::deeplink::PendingLink;
use crate::state::{AppState, Core};
use crate::window_state::SavedWindow;

/// Event asking the UI what to do.
pub const QUIT_REQUESTED: &str = "quit://requested";

/// What a quit request leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitAction {
    /// Quit at once.
    Quit,
    /// Let the active tasks finish, then quit.
    WaitThenQuit,
    /// Ask the user.
    Ask,
}

/// Decides from the setting and whether tasks are active. Without active
/// tasks the app always quits at once.
pub(crate) fn decide(setting: QuitWithTasks, busy: bool) -> QuitAction {
    if !busy {
        return QuitAction::Quit;
    }
    match setting {
        QuitWithTasks::Ask => QuitAction::Ask,
        QuitWithTasks::Wait => QuitAction::WaitThenQuit,
        QuitWithTasks::Quit => QuitAction::Quit,
    }
}

/// Payload of [`QUIT_REQUESTED`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct QuitRequest {
    active_transfers: usize,
    active_syncs: usize,
}

/// Settings the host reads often, copied from the saved settings.
#[derive(Debug, Clone, Copy)]
struct Prefs {
    keep_awake: bool,
    quit: QuitWithTasks,
}

/// State shared by the tray, the activity monitor, quitting, links and the
/// window state. Lives in the [`Core`].
pub(crate) struct Lifecycle {
    prefs: Mutex<Prefs>,
    quit_when_idle: AtomicBool,
    pending_link: Mutex<Option<PendingLink>>,
    /// The last known geometry of the main window.
    pub(crate) window: Mutex<Option<SavedWindow>>,
}

impl Lifecycle {
    pub(crate) fn new(settings: &Settings) -> Self {
        Lifecycle {
            prefs: Mutex::new(Prefs {
                keep_awake: settings.general.keep_awake,
                quit: settings.general.quit_with_tasks,
            }),
            quit_when_idle: AtomicBool::new(false),
            pending_link: Mutex::new(None),
            window: Mutex::new(None),
        }
    }

    /// Takes over saved settings.
    pub(crate) fn apply_settings(&self, settings: &Settings) {
        *self.prefs.lock().unwrap_or_else(PoisonError::into_inner) = Prefs {
            keep_awake: settings.general.keep_awake,
            quit: settings.general.quit_with_tasks,
        };
    }

    pub(crate) fn keep_awake(&self) -> bool {
        self.prefs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keep_awake
    }

    pub(crate) fn quit_behavior(&self) -> QuitWithTasks {
        self.prefs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .quit
    }

    /// Whether the app waits for the active tasks to quit.
    pub(crate) fn quit_when_idle(&self) -> bool {
        self.quit_when_idle.load(Ordering::SeqCst)
    }

    pub(crate) fn set_quit_when_idle(&self, wait: bool) {
        self.quit_when_idle.store(wait, Ordering::SeqCst);
    }

    /// Keeps `link` for its window; a newer link replaces an older one.
    pub(crate) fn set_pending_link(&self, link: PendingLink) {
        *self
            .pending_link
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(link);
    }

    /// Removes and returns the pending link when it is for `label`.
    pub(crate) fn take_pending_link(&self, label: &str) -> Option<crate::deeplink::LinkAction> {
        let mut pending = self
            .pending_link
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if pending.as_ref().is_some_and(|link| link.target == label) {
            pending.take().map(|link| link.action)
        } else {
            None
        }
    }
}

fn ready_core<R: Runtime>(app: &AppHandle<R>) -> Option<std::sync::Arc<Core>> {
    app.try_state::<AppState>()?.core().ok()
}

/// Whether leaving now would interrupt work. `false` when the core did not
/// start.
pub(crate) fn busy<R: Runtime>(app: &AppHandle<R>) -> bool {
    ready_core(app).is_some_and(|core| core.activity(None).busy())
}

/// Quits the app without asking.
pub(crate) fn exit_now<R: Runtime>(app: &AppHandle<R>) {
    app.exit(0);
}

/// Handles a request to quit, from any source.
pub(crate) fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    let Some(core) = ready_core(app) else {
        exit_now(app);
        return;
    };
    let activity = core.activity(None);
    match decide(core.lifecycle.quit_behavior(), activity.busy()) {
        QuitAction::Quit => exit_now(app),
        QuitAction::WaitThenQuit => wait_then_quit(app, &core),
        QuitAction::Ask => ask(app, activity),
    }
}

/// Hides the windows (when there is a tray to bring them back) and quits once
/// the active tasks are done; the activity monitor does the quitting.
pub(crate) fn wait_then_quit<R: Runtime>(app: &AppHandle<R>, core: &Core) {
    core.lifecycle.set_quit_when_idle(true);
    tracing::info!(
        target: "bucketbridge::app",
        "quit requested: waiting for the active tasks to finish"
    );
    if crate::tray::available() {
        for window in app.webview_windows().values() {
            let _ = window.hide();
        }
    }
}

fn ask<R: Runtime>(app: &AppHandle<R>, activity: Activity) {
    let target = crate::pick_window(app);
    crate::focus_main_window(app);
    let request = QuitRequest {
        active_transfers: activity.transfers(),
        active_syncs: activity.sync_runs,
    };
    match target {
        Some(window) => {
            let _ = app.emit_to(window.label(), QUIT_REQUESTED, request);
        }
        None => {
            let _ = app.emit(QUIT_REQUESTED, request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deeplink::LinkAction;

    #[test]
    fn no_active_tasks_always_quits() {
        for setting in [QuitWithTasks::Ask, QuitWithTasks::Wait, QuitWithTasks::Quit] {
            assert_eq!(decide(setting, false), QuitAction::Quit);
        }
    }

    #[test]
    fn active_tasks_follow_the_setting() {
        assert_eq!(decide(QuitWithTasks::Ask, true), QuitAction::Ask);
        assert_eq!(decide(QuitWithTasks::Wait, true), QuitAction::WaitThenQuit);
        assert_eq!(decide(QuitWithTasks::Quit, true), QuitAction::Quit);
    }

    #[test]
    fn the_request_serializes_for_the_ui() {
        let request = QuitRequest {
            active_transfers: 3,
            active_syncs: 1,
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({ "active_transfers": 3, "active_syncs": 1 })
        );
    }

    #[test]
    fn lifecycle_follows_the_settings() {
        let mut settings = Settings::default();
        let lifecycle = Lifecycle::new(&settings);
        assert!(lifecycle.keep_awake());
        assert_eq!(lifecycle.quit_behavior(), QuitWithTasks::Ask);
        settings.general.keep_awake = false;
        settings.general.quit_with_tasks = QuitWithTasks::Wait;
        lifecycle.apply_settings(&settings);
        assert!(!lifecycle.keep_awake());
        assert_eq!(lifecycle.quit_behavior(), QuitWithTasks::Wait);
        assert!(!lifecycle.quit_when_idle());
        lifecycle.set_quit_when_idle(true);
        assert!(lifecycle.quit_when_idle());
    }

    #[test]
    fn a_pending_link_goes_to_its_window_once() {
        let lifecycle = Lifecycle::new(&Settings::default());
        assert_eq!(lifecycle.take_pending_link("main"), None);
        let action = LinkAction::Error {
            message: "no".into(),
        };
        lifecycle.set_pending_link(PendingLink {
            target: "main".into(),
            action: action.clone(),
        });
        assert_eq!(lifecycle.take_pending_link("main-1"), None);
        assert_eq!(lifecycle.take_pending_link("main"), Some(action));
        assert_eq!(lifecycle.take_pending_link("main"), None);
    }
}
