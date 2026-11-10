//! URL shortener commands (docs/desktop-api.md, "Sharing wizard and URL
//! shorteners").

use bucketbridge_core::settings::ShortenerProvider;
use tauri::State;

use super::{CommandResult, logged};
use crate::error::Secret;
use crate::state::AppState;

/// Sends `url` to a shortener and returns the short link: the one chosen in
/// the settings, or `provider` for this link when the settings have one.
/// Fails with `usage` while no shortener is chosen (nothing is sent).
#[tauri::command(rename_all = "snake_case")]
pub async fn shorten_url(
    state: State<'_, AppState>,
    url: Secret,
    provider: Option<ShortenerProvider>,
) -> CommandResult<String> {
    let core = state.core()?;
    logged(
        "shortening a link",
        core.session.shorten_url_as(provider, url.expose()),
    )
    .await
}

/// Stores (`token`) or deletes (`null`) the shortener's API token.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_shortener_token(
    state: State<'_, AppState>,
    token: Option<Secret>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged(
        "saving the shortener token",
        core.session
            .set_shortener_token(token.as_ref().map(Secret::expose)),
    )
    .await
}
