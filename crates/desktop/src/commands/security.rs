//! Security and account commands (docs/desktop-api.md, "Security and accounts").

use bucketbridge_core::model::{Account, AccountInput, AccountList, AccountUpdate, SecurityStatus};
use tauri::State;

use super::{CommandResult, logged};
use crate::error::Secret;
use crate::state::AppState;

/// Whether a master password is set and whether the secrets are readable.
#[tauri::command(rename_all = "snake_case")]
pub async fn security_status(state: State<'_, AppState>) -> CommandResult<SecurityStatus> {
    let core = state.core()?;
    logged(
        "reading the security status",
        core.session.security_status(),
    )
    .await
}

/// Sets, changes (`current` required) or removes (`new_password: null`) the
/// master password; emits `security://changed`.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_master_password(
    state: State<'_, AppState>,
    current: Option<Secret>,
    new_password: Option<Secret>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("changing the master password", async {
        core.session
            .set_master_password(
                current.as_ref().map(Secret::expose),
                new_password.as_ref().map(Secret::expose),
            )
            .await?;
        Ok(())
    })
    .await
}

/// Unlocks the secrets (a wrong password is `auth` and changes nothing);
/// emits `security://changed`.
#[tauri::command(rename_all = "snake_case")]
pub async fn unlock(state: State<'_, AppState>, password: Secret) -> CommandResult<()> {
    let core = state.core()?;
    logged("unlocking the secrets", async {
        core.session.unlock(password.expose()).await?;
        Ok(())
    })
    .await
}

/// Locks the secrets and forgets every MFA session and cached client; emits
/// `security://changed`. Sync runs already executing keep their clients and
/// finish.
#[tauri::command(rename_all = "snake_case")]
pub async fn lock(state: State<'_, AppState>) -> CommandResult<()> {
    let core = state.core()?;
    core.session.lock();
    Ok(())
}

/// Every account and the default account; works while locked.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_accounts(state: State<'_, AppState>) -> CommandResult<AccountList> {
    let core = state.core()?;
    logged("listing the accounts", core.session.list_accounts()).await
}

/// Adds an account and stores its secrets.
#[tauri::command(rename_all = "snake_case")]
pub async fn add_account(
    state: State<'_, AppState>,
    input: AccountInput,
) -> CommandResult<Account> {
    let core = state.core()?;
    logged("adding an account", core.session.add_account(input)).await
}

/// Updates an account; `null` fields keep their value.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_account(
    state: State<'_, AppState>,
    id: String,
    update: AccountUpdate,
) -> CommandResult<Account> {
    let core = state.core()?;
    logged(
        "updating an account",
        core.session.update_account(&id, update),
    )
    .await
}

/// Removes an account and deletes its secrets.
#[tauri::command(rename_all = "snake_case")]
pub async fn remove_account(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    logged("removing an account", core.session.remove_account(&id)).await
}

/// Makes `id` the default account.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_default_account(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    let core = state.core()?;
    logged(
        "changing the default account",
        core.session.set_default_account(&id),
    )
    .await
}

/// Stores the arrangement of account folders and the order of accounts.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_account_layout(
    state: State<'_, AppState>,
    layout: bucketbridge_core::accountfile::AccountLayout,
) -> CommandResult<AccountList> {
    let core = state.core()?;
    logged(
        "arranging the accounts",
        core.session.set_account_layout(layout),
    )
    .await
}

/// What `export_accounts` wrote.
#[derive(Debug, serde::Serialize)]
pub struct ExportAccountsOutcome {
    path: String,
    accounts: usize,
    folders: usize,
}

/// Writes the account file (no secrets) for the named accounts, or all.
#[tauri::command(rename_all = "snake_case")]
pub async fn export_accounts(
    state: State<'_, AppState>,
    path: String,
    account_ids: Option<Vec<String>>,
) -> CommandResult<ExportAccountsOutcome> {
    let core = state.core()?;
    logged("exporting the accounts", async {
        super::require_absolute("the account file path", &path)?;
        let export = core.session.export_accounts(account_ids).await?;
        let outcome = ExportAccountsOutcome {
            path: path.clone(),
            accounts: export.accounts.len(),
            folders: export.folders.len(),
        };
        super::blocking(move || {
            bucketbridge_core::accountfile::write_export(&export, std::path::Path::new(&path))
        })
        .await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "exported {} account(s) without secrets",
            outcome.accounts
        );
        Ok(outcome)
    })
    .await
}

/// Adds the accounts of an account file; existing accounts are never changed.
#[tauri::command(rename_all = "snake_case")]
pub async fn import_accounts(
    state: State<'_, AppState>,
    path: String,
    duplicates: bucketbridge_core::accountfile::DuplicatePolicy,
) -> CommandResult<bucketbridge_core::accountfile::ImportReport> {
    let core = state.core()?;
    logged("importing accounts", async {
        super::require_absolute("the account file path", &path)?;
        let export = super::blocking(move || {
            bucketbridge_core::accountfile::read_export(std::path::Path::new(&path))
        })
        .await?;
        let report = core.session.import_accounts(export, duplicates).await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "imported {} account(s); {} duplicate(s), {} skipped",
            report.imported.len(),
            report.duplicates.len(),
            report.skipped.len()
        );
        Ok(report)
    })
    .await
}

/// Logs the account in with the current code of its MFA device; the STS
/// session is kept in memory until it expires.
#[tauri::command(rename_all = "snake_case")]
pub async fn mfa_login(
    state: State<'_, AppState>,
    account_id: String,
    code: Secret,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("signing in with MFA", async {
        core.session.mfa_login(&account_id, code.expose()).await?;
        Ok(())
    })
    .await
}
