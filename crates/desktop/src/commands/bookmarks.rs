//! Bookmark commands (docs/desktop-api.md, "Bookmarks and windows"). They
//! read and write `config.toml` only, so they work while the secrets are
//! locked. Every change emits `bookmarks://changed` with the stored list, to
//! every window.

use bucketbridge_core::bookmarks::{self, Bookmark};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::{CommandResult, blocking, logged};
use crate::events::BOOKMARKS_CHANGED;
use crate::state::AppState;

/// Tells every window about the new list. An emit only fails while the app
/// shuts down.
fn announce<R: Runtime, T: Serialize + Clone>(app: &AppHandle<R>, list: &T) {
    let _ = app.emit(BOOKMARKS_CHANGED, list.clone());
}

/// The saved bookmarks, in the order they were added.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_bookmarks(state: State<'_, AppState>) -> CommandResult<Vec<Bookmark>> {
    let core = state.core()?;
    logged("listing the bookmarks", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || bookmarks::list(&store)).await
    })
    .await
}

/// Bookmarks a location (`prefix` is `""` for a bucket, a folder prefix or an
/// object key). `label: null` uses the location. Returns the stored list;
/// `conflict` when the location is already bookmarked, `not_found` for an
/// unknown account, `usage` for an invalid label.
#[tauri::command(rename_all = "snake_case")]
pub async fn add_bookmark<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    label: Option<String>,
) -> CommandResult<Vec<Bookmark>> {
    let core = state.core()?;
    let list = logged("adding a bookmark", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || bookmarks::add(&store, &account_id, &bucket, &prefix, label.as_deref()))
            .await
    })
    .await?;
    announce(&app, &list);
    Ok(list)
}

/// Gives a bookmark a new label and returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn rename_bookmark<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: String,
    label: String,
) -> CommandResult<Vec<Bookmark>> {
    let core = state.core()?;
    let list = logged("renaming a bookmark", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || bookmarks::rename(&store, &id, &label)).await
    })
    .await?;
    announce(&app, &list);
    Ok(list)
}

/// Removes a bookmark and returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn remove_bookmark<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<Vec<Bookmark>> {
    let core = state.core()?;
    let list = logged("removing a bookmark", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || bookmarks::remove(&store, &id)).await
    })
    .await?;
    announce(&app, &list);
    Ok(list)
}
