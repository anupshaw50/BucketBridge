//! Edit-in-place commands (docs/desktop-api.md, "Edit in place (v0.2.0)").
//!
//! A thin layer over `bucketbridge_core::edit`: `start_edit` downloads the
//! object into a private folder and opens the copy with the OS default
//! application (or a chosen one) through the same openers as `open_object`;
//! the core uploads every save and reports through `edit://changed`.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use bucketbridge_core::Error;
use bucketbridge_core::edit::{EditResolution, EditResolved, EditSession, StartEdit};
use bucketbridge_core::engine::TransferOptions;
use tauri::State;

use super::{CommandResult, blocking, logged};
use crate::state::{AppState, Core};

/// Downloads the object, opens the copy (with `app` when given) and starts
/// uploading its saves. An object that is already being edited has its
/// existing copy opened again. When the application cannot be started the
/// session is dropped again.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_edit(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    app: Option<String>,
) -> CommandResult<EditSession> {
    let core = state.core()?;
    logged("starting to edit an object", async {
        let app = app.filter(|app| !app.trim().is_empty());
        let settings = core.session.settings().await?;
        let started = core
            .edits
            .start(StartEdit {
                account_id,
                bucket,
                key,
                transfer: TransferOptions::from_settings(&settings),
            })
            .await?;
        let session = started.session;
        let path = PathBuf::from(&session.local_path);
        if let Err(error) = open_copy(&core, path, app.clone()).await {
            if !started.reused {
                // Nothing was edited yet; a failed stop leaves a folder that
                // the startup cleanup removes.
                let _ = core.edits.stop(&session.id, true).await;
            }
            return Err(error);
        }
        if let Some(program) = app {
            let mut settings = core.session.settings().await?;
            settings.general.remember_open_with(&program);
            core.session.save_settings(settings).await?;
        }
        tracing::info!(
            target: "bucketbridge::desktop",
            "editing s3://{}/{} from {}",
            session.bucket,
            session.key,
            session.local_path
        );
        Ok(session)
    })
    .await
}

/// Hands the copy to the default application, or to `app`.
async fn open_copy(
    core: &Core,
    path: PathBuf,
    app: Option<String>,
) -> bucketbridge_core::Result<()> {
    match app {
        Some(program) => {
            let open_with = Arc::clone(&core.open_with);
            blocking(move || {
                open_with(&path, &program).map_err(|message| {
                    Error::io(
                        format!("opening {} with {program}", path.display()),
                        io::Error::other(message),
                    )
                })
            })
            .await
        }
        None => {
            let opener = Arc::clone(&core.opener);
            blocking(move || {
                opener(&path).map_err(|message| {
                    Error::io(
                        format!("opening {} with the default application", path.display()),
                        io::Error::other(message),
                    )
                })
            })
            .await
        }
    }
}

/// The active edit sessions, oldest first.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_edits(state: State<'_, AppState>) -> CommandResult<Vec<EditSession>> {
    let core = state.core()?;
    Ok(core.edits.list())
}

/// Ends a session: a save that is not uploaded yet is uploaded first (the
/// command rejects with `conflict`, or with the upload's error, and the
/// session stays); with `discard` the local changes are dropped. The local
/// copy is deleted.
#[tauri::command(rename_all = "snake_case")]
pub async fn stop_edit(
    state: State<'_, AppState>,
    id: String,
    discard: Option<bool>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("stopping an edit session", async {
        core.edits.stop(&id, discard.unwrap_or(false)).await
    })
    .await
}

/// Settles a conflict (`overwrite`, `save_copy`, `discard`) or retries a
/// failed upload (`retry`).
#[tauri::command(rename_all = "snake_case")]
pub async fn resolve_edit_conflict(
    state: State<'_, AppState>,
    id: String,
    resolution: EditResolution,
) -> CommandResult<EditResolved> {
    let core = state.core()?;
    logged("resolving an edit conflict", async {
        core.edits.resolve(&id, resolution).await
    })
    .await
}
