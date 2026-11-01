//! BucketBridge desktop app: the Tauri 2 host of the React UI in `ui/`.
//!
//! - [`run`] starts the app (the `bucketbridge` binary calls it).
//! - [`handler`] is the IPC invoke handler with every command of
//!   docs/desktop-api.md.
//! - [`AppState`] / [`Core`] hold what the commands share: the core
//!   `Session`, the event log, the sync plan store and executor, the
//!   transfer queue, and the one bandwidth limiter.
//! - [`forward_events`] turns the core's broadcasts into the
//!   `security://changed` and `log://entry` events and the queue's events
//!   into `transfer://update` and `transfer://removed`; sync runs emit
//!   `sync://progress` and `sync://row`.
//!
//! See docs/desktop.md.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod commands;
mod error;
mod events;
mod logging;
#[cfg(target_os = "macos")]
mod menu;
mod open;
mod state;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, RunEvent, Runtime};

pub use commands::handler;
pub use error::{CommandError, Secret};
pub use events::{
    BOOKMARKS_CHANGED, LOG_ENTRY, SECURITY_CHANGED, SYNC_PROGRESS, SYNC_ROW, TRANSFER_REMOVED,
    TRANSFER_UPDATE, forward_events,
};
pub use open::OPEN_DIR_NAME;
pub use state::{AppState, Core, CoreParts, OpenWith, Opener, UrlOpener};

/// Label of the window declared in tauri.conf.json.
const MAIN_WINDOW: &str = "main";

/// How long the app waits for running sync runs to stop when it exits.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Starts the desktop app and returns when it fails to start (a normal exit
/// ends the process from the event loop).
pub fn run() -> ExitCode {
    let builder = tauri::Builder::default()
        // First: a second instance hands over to the first (which focuses
        // its window) and exits here, before anything else is opened.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main_window(app);
        }))
        // Second: opens the session and the stores. Plugins initialize in
        // this order while the app is built, before any window exists, so
        // the state is managed before the UI can call a command.
        .plugin(state_plugin())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(handler());

    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu::build);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("BucketBridge failed to start: {error}");
            return ExitCode::FAILURE;
        }
    };
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            shutdown(app);
        }
    });
    ExitCode::SUCCESS
}

/// The plugin that creates the [`AppState`] at startup. When the core cannot
/// start (for example `config.toml` is unreadable or another process holds
/// the data folder), the app still opens and every command rejects with the
/// startup error, so the UI can show it.
fn state_plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri::plugin::Builder::new("bucketbridge")
        .setup(|app, _api| {
            let state = match Core::start() {
                Ok(core) => {
                    forward_events(app, &core);
                    let purging = Arc::clone(&core);
                    tauri::async_runtime::spawn_blocking(move || purging.purge_opened_files());
                    AppState::ready(core)
                }
                Err(error) => {
                    eprintln!("BucketBridge could not open its data: {error}");
                    AppState::failed(CommandError::from(error))
                }
            };
            app.manage(state);
            Ok(())
        })
        .build()
}

/// Brings the main window to the front (a second launch of the app); when it
/// was closed, any other window of the app.
fn focus_main_window<R: Runtime>(app: &AppHandle<R>) {
    let window = app.get_webview_window(MAIN_WINDOW).or_else(|| {
        let mut windows: Vec<_> = app.webview_windows().into_iter().collect();
        windows.sort_by(|a, b| a.0.cmp(&b.0));
        windows.into_iter().next().map(|(_, window)| window)
    });
    if let Some(window) = window {
        // Best effort: the window may be closing.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Stops the transfer queue and running sync runs cleanly and flushes the log
/// file before exit.
fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>()
        && let Ok(core) = state.core()
    {
        core.shutdown(SHUTDOWN_GRACE);
    }
}
