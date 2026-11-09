//! The system tray icon: a menu (Show window, Pause all, Resume all, Quit)
//! and a tooltip with the transfer progress (see `activity`).
//!
//! The menu texts are English, like the rest of the host's messages. A
//! desktop without tray support (some Linux sessions) runs the app without
//! one; [`available`] tells.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

const TRAY_ID: &str = "main";
const SHOW: &str = "tray-show";
const PAUSE: &str = "tray-pause";
const RESUME: &str = "tray-resume";
const QUIT: &str = "tray-quit";

static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the tray icon was created.
pub(crate) fn available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

/// Creates the tray icon. A failure is logged and the app goes on without it.
pub(crate) fn install<R: Runtime>(app: &AppHandle<R>) {
    match build(app) {
        Ok(_) => AVAILABLE.store(true, Ordering::SeqCst),
        Err(error) => eprintln!("BucketBridge: no tray icon: {error}"),
    }
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<TrayIcon<R>> {
    let show = MenuItem::with_id(app, SHOW, "Show window", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, PAUSE, "Pause all transfers", true, None::<&str>)?;
    let resume = MenuItem::with_id(app, RESUME, "Resume all transfers", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &resume,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("BucketBridge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        SHOW => show_window(app),
        PAUSE => queue_action(app, |core| core.queue.stop_all()),
        RESUME => queue_action(app, |core| core.queue.start_all()),
        QUIT => crate::quit::request_quit(app),
        _ => {}
    }
}

/// Brings the window back and cancels a pending "quit when idle".
fn show_window<R: Runtime>(app: &AppHandle<R>) {
    crate::focus_main_window(app);
}

fn queue_action<R: Runtime>(
    app: &AppHandle<R>,
    action: impl FnOnce(&crate::state::Core) -> bucketbridge_core::Result<()>,
) {
    let Some(Ok(core)) = app.try_state::<AppState>().map(|state| state.core()) else {
        return;
    };
    if let Err(error) = action(&core) {
        tracing::warn!(target: "bucketbridge::desktop", "tray queue action failed: {error}");
    }
}

/// Sets the tooltip text.
pub(crate) fn set_tooltip<R: Runtime>(app: &AppHandle<R>, text: &str) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // The tooltip is not supported on every desktop.
        let _ = tray.set_tooltip(Some(text));
    }
}
