//! `take_pending_link` and `quit_app` (docs/desktop-api.md, "Everyday use").

use serde::Deserialize;
use tauri::{AppHandle, Runtime, State, WebviewWindow};

use super::CommandResult;
use crate::deeplink::LinkAction;
use crate::state::AppState;

/// How `quit_app` quits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitMode {
    /// At once.
    Now,
    /// When no transfer or sync run is active any more.
    WhenIdle,
}

/// The `bb://` link waiting for the calling window, which is removed.
#[tauri::command(rename_all = "snake_case")]
pub async fn take_pending_link<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<LinkAction>> {
    let core = state.core()?;
    Ok(core.lifecycle.take_pending_link(window.label()))
}

/// Quits the app, now or once the active tasks have finished.
#[tauri::command(rename_all = "snake_case")]
pub async fn quit_app<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    mode: QuitMode,
) -> CommandResult<()> {
    let core = state.core()?;
    match mode {
        QuitMode::WhenIdle if core.activity(None).busy() => {
            crate::quit::wait_then_quit(&app, &core);
        }
        _ => crate::quit::exit_now(&app),
    }
    Ok(())
}
