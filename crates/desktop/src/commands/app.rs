//! Settings, log and app commands (docs/desktop-api.md, "Settings, log, app").

use std::path::PathBuf;

use bucketbridge_core::model::{LogEntry, LogLevel};
use bucketbridge_core::{CrashReport, ExportFormat, Settings, crash};
use serde::Serialize;
use tauri::State;

use super::{CommandResult, blocking, logged, require_absolute};
use crate::error::Secret;
use crate::state::{AppState, VERSION};

/// The current settings.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<Settings> {
    let core = state.core()?;
    logged("reading the settings", core.session.settings()).await
}

/// Validates and saves the settings (every problem is reported in one
/// `usage` error), then applies them to the running app: the log level and
/// file logging at once, the bandwidth cap to running and future transfers.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the settings", async {
        core.session.save_settings(settings.clone()).await?;
        core.apply_settings(&settings);
        Ok(())
    })
    .await
}

/// Stores (`password`) or deletes (`null`) the proxy password.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_proxy_password(
    state: State<'_, AppState>,
    password: Option<Secret>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged(
        "saving the proxy password",
        core.session
            .set_proxy_password(password.as_ref().map(Secret::expose)),
    )
    .await
}

/// Event-log entries, newest last: `levels: null` is every level, `text` a
/// case-insensitive match on message and source, `limit` keeps the newest.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_log(
    state: State<'_, AppState>,
    levels: Option<Vec<LogLevel>>,
    text: Option<String>,
    limit: Option<u64>,
) -> CommandResult<Vec<LogEntry>> {
    let core = state.core()?;
    let limit = limit.map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(core.log.query(levels.as_deref(), text.as_deref(), limit))
}

/// Writes every entry in memory to `path` (absolute; replaced atomically).
#[tauri::command(rename_all = "snake_case")]
pub async fn export_log(
    state: State<'_, AppState>,
    path: String,
    format: ExportFormat,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("exporting the log", async {
        require_absolute("the export file", &path)?;
        let log = core.log.clone();
        blocking(move || log.export(&PathBuf::from(path), format)).await
    })
    .await
}

/// Empties the in-memory log (ids keep counting; the log file is kept).
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_log(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    core.log.clear();
    Ok(())
}

/// `AppInfo` of docs/desktop-api.md.
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    repository_url: &'static str,
    config_dir: String,
    data_dir: String,
    log_dir: String,
}

/// Name, version, license, repository and the application folders.
#[tauri::command(rename_all = "snake_case")]
pub async fn app_info(state: State<'_, AppState>) -> CommandResult<AppInfo> {
    let core = state.core()?;
    let dirs = &core.dirs;
    Ok(AppInfo {
        name: "BucketBridge",
        version: VERSION,
        license: env!("CARGO_PKG_LICENSE"),
        repository_url: env!("CARGO_PKG_REPOSITORY"),
        config_dir: dirs.config_dir.to_string_lossy().into_owned(),
        data_dir: dirs.data_dir.to_string_lossy().into_owned(),
        log_dir: dirs.log_dir.to_string_lossy().into_owned(),
    })
}

/// The newest crash report in the crash folder, or `null`.
#[tauri::command(rename_all = "snake_case")]
pub async fn last_crash_report(state: State<'_, AppState>) -> CommandResult<Option<CrashReport>> {
    let core = state.core()?;
    logged("reading the crash reports", async {
        let crash_dir = core.dirs.crash_dir.clone();
        blocking(move || crash::last_report(&crash_dir)).await
    })
    .await
}

/// Moves a crash report into `dismissed/`. The path is untrusted: it must be
/// a report directly inside the crash folder (`usage` otherwise,
/// `not_found` when it is already gone).
#[tauri::command(rename_all = "snake_case")]
pub async fn dismiss_crash_report(state: State<'_, AppState>, path: String) -> CommandResult<()> {
    let core = state.core()?;
    logged("dismissing a crash report", async {
        let crash_dir = core.dirs.crash_dir.clone();
        blocking(move || crash::dismiss(&crash_dir, &path)).await
    })
    .await
}
