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

mod activity;
mod commands;
mod deeplink;
mod error;
mod events;
mod logging;
#[cfg(target_os = "macos")]
mod menu;
mod open;
mod quit;
mod state;
mod tray;
mod window_state;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, RunEvent, Runtime, WebviewWindow, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;

pub use commands::handler;
pub use error::{CommandError, Secret};
pub use events::{
    BOOKMARKS_CHANGED, EDIT_CHANGED, LOG_ENTRY, SECURITY_CHANGED, SYNC_PROGRESS, SYNC_ROW,
    TRANSFER_CONFLICT, TRANSFER_REMOVED, TRANSFER_UPDATE, forward_events,
};
pub use events::{SYNC_JOBS_CHANGED, SYNC_REPORT};
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
            // A `bb://` link in the arguments reaches `on_open_url` through
            // the plugin's deep-link feature.
            focus_main_window(app);
        }))
        // Second: opens the session and the stores. Plugins initialize in
        // this order while the app is built, before any window exists, so
        // the state is managed before the UI can call a command.
        .plugin(state_plugin())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            setup_everyday(app.handle());
            Ok(())
        })
        .on_window_event(on_window_event)
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
    app.run(|app, event| match event {
        RunEvent::Ready => restore_main_window(app),
        // A quit the user can still reconsider: the close button of the last
        // window and the system quit (Cmd-Q) arrive here without a code; the
        // tray, the quit dialog and the activity monitor exit with one.
        RunEvent::ExitRequested {
            code: None, api, ..
        } if quit::busy(app) => {
            api.prevent_exit();
            quit::request_quit(app);
        }
        RunEvent::Exit => {
            save_window_state(app);
            shutdown(app);
        }
        _ => {}
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
                    tauri::async_runtime::spawn_blocking(move || {
                        purging.purge_opened_files();
                        purging.purge_edit_copies();
                    });
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

/// The window to bring forward: the main window; when it was closed, the
/// first other window by label.
pub(crate) fn pick_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window(MAIN_WINDOW).or_else(|| {
        let mut windows: Vec<_> = app.webview_windows().into_iter().collect();
        windows.sort_by(|a, b| a.0.cmp(&b.0));
        windows.into_iter().next().map(|(_, window)| window)
    })
}

/// Brings the main window to the front (a second launch of the app, the tray,
/// a link); when it was closed, any other window of the app. Showing the
/// window cancels a pending "quit when idle".
pub(crate) fn focus_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(Ok(core)) = app.try_state::<AppState>().map(|state| state.core()) {
        core.lifecycle.set_quit_when_idle(false);
    }
    if let Some(window) = pick_window(app) {
        // Best effort: the window may be closing.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Tray, activity monitor and `bb://` links; after the state exists.
fn setup_everyday<R: Runtime>(app: &AppHandle<R>) {
    let Some(Ok(core)) = app.try_state::<AppState>().map(|state| state.core()) else {
        return;
    };
    tray::install(app);
    activity::spawn_monitor(app.clone(), core);

    let links = app.deep_link();
    // A development build is not installed, so its scheme is registered at
    // run time (installed builds register it in the installer).
    #[cfg(all(debug_assertions, any(target_os = "linux", windows)))]
    if let Err(error) = links.register_all() {
        eprintln!("BucketBridge: could not register the bb:// scheme: {error}");
    }
    let handle = app.clone();
    links.on_open_url(move |event| {
        for url in event.urls() {
            deeplink::handle_link(&handle, url.as_str());
        }
    });
    // The link that started the app (the plugin keeps it for this call).
    if let Ok(Some(urls)) = links.get_current() {
        for url in urls {
            deeplink::handle_link(app, url.as_str());
        }
    }
}

/// Closing the last window while tasks are active asks first; the main
/// window's geometry is remembered.
fn on_window_event<R: Runtime>(window: &tauri::Window<R>, event: &WindowEvent) {
    let app = window.app_handle();
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if window.label() == MAIN_WINDOW {
                save_window_state(app);
            }
            if app.webview_windows().len() <= 1 && quit::busy(app) {
                api.prevent_close();
                quit::request_quit(app);
            }
        }
        WindowEvent::Moved(_) | WindowEvent::Resized(_) if window.label() == MAIN_WINDOW => {
            if let (Some(webview), Some(Ok(core))) = (
                app.get_webview_window(MAIN_WINDOW),
                app.try_state::<AppState>().map(|state| state.core()),
            ) {
                window_state::track(&webview, &core.lifecycle.window);
            }
        }
        _ => {}
    }
}

/// Puts the main window where it was at the last run, or back on screen.
fn restore_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let saved = app
        .try_state::<AppState>()
        .and_then(|state| state.core().ok())
        .and_then(|core| window_state::read(&core.dirs().data_dir.join(window_state::FILE_NAME)));
    match saved {
        Some(saved) => window_state::apply(&window, saved),
        None => window_state::ensure_on_screen(&window),
    }
}

/// Writes the last known geometry of the main window (best effort).
fn save_window_state<R: Runtime>(app: &AppHandle<R>) {
    let Some(Ok(core)) = app.try_state::<AppState>().map(|state| state.core()) else {
        return;
    };
    let saved = *core
        .lifecycle
        .window
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(saved) = saved
        && let Err(error) =
            window_state::write(&core.dirs().data_dir.join(window_state::FILE_NAME), &saved)
    {
        tracing::warn!(target: "bucketbridge::desktop", "window state not saved: {error}");
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
