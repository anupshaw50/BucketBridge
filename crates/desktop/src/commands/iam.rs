//! IAM manager commands (docs/desktop-api.md, "IAM (T02)"): users, their
//! policies, access keys and login profiles.
//!
//! Every command resolves the account through `Session::iam_for`, which
//! fails with `locked` or `mfa_required` first and with `usage` ("IAM is not
//! available for this endpoint") for an account with a custom endpoint.
//! Destructive commands take the typed `confirm` text of the dialog and
//! the core refuses without it. The secret of a new access key is in the
//! answer of `iam_create_access_key` and nowhere else.

use bucketbridge_core::Error;
use bucketbridge_core::bucketcfg::PolicyIssue;
use bucketbridge_core::iam::{
    self, AccessKeyInfo, DeleteUserOutcome, IamUser, InlinePolicy, ManagedPolicy, NewAccessKey,
    PolicyScope, UserDetails,
};
use bucketbridge_core::secrets::SecretString;
use serde::Serialize;
use tauri::State;

use super::{CommandResult, logged};
use crate::state::AppState;

/// Whether IAM can be used with an account.
#[derive(Debug, Serialize)]
pub struct IamStatus {
    supported: bool,
    /// Why not, when `supported` is false.
    message: Option<String>,
}

/// Whether the account can use IAM (no network call): AWS accounts can,
/// accounts with a custom endpoint cannot.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_status(
    state: State<'_, AppState>,
    account_id: String,
) -> CommandResult<IamStatus> {
    let core = state.core()?;
    logged("checking IAM support", async {
        let account = core
            .session
            .list_accounts()
            .await?
            .accounts
            .into_iter()
            .find(|account| account.id == account_id)
            .ok_or_else(|| Error::NotFound("no such account".to_owned()))?;
        let supported = iam::iam_supported(&account);
        Ok(IamStatus {
            supported,
            message: (!supported).then(|| iam::NOT_AVAILABLE.to_owned()),
        })
    })
    .await
}

/// The users of the account.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_list_users(
    state: State<'_, AppState>,
    account_id: String,
) -> CommandResult<Vec<IamUser>> {
    let core = state.core()?;
    logged("listing IAM users", async {
        core.session
            .iam_for(&account_id)
            .await?
            .list_users(None)
            .await
    })
    .await
}

/// Creates a user.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_create_user(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    path: Option<String>,
) -> CommandResult<IamUser> {
    let core = state.core()?;
    logged("creating an IAM user", async {
        core.session
            .iam_for(&account_id)
            .await?
            .create_user(&name, path.as_deref())
            .await
    })
    .await
}

/// Renames a user and/or changes its path.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_update_user(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    new_name: Option<String>,
    new_path: Option<String>,
) -> CommandResult<IamUser> {
    let core = state.core()?;
    logged("updating an IAM user", async {
        core.session
            .iam_for(&account_id)
            .await?
            .update_user(&name, new_name.as_deref(), new_path.as_deref())
            .await
    })
    .await
}

/// Deletes a user and everything it owns; `confirm` must be the user name.
/// Every step is reported in the answer.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_delete_user(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    confirm: String,
) -> CommandResult<DeleteUserOutcome> {
    let core = state.core()?;
    logged("deleting an IAM user", async {
        let outcome = core
            .session
            .iam_for(&account_id)
            .await?
            .delete_user(&name, &confirm)
            .await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "IAM user {name}: {}",
            if outcome.deleted { "deleted" } else { "not deleted, a step failed" }
        );
        Ok(outcome)
    })
    .await
}

/// A user with its policies, access keys, login profile and groups, each
/// part on its own.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_user_details(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> CommandResult<UserDetails> {
    let core = state.core()?;
    logged("reading an IAM user", async {
        core.session
            .iam_for(&account_id)
            .await?
            .user_details(&name)
            .await
    })
    .await
}

/// Managed policies that can be attached (`scope`: `local`, `aws`, `all`).
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_list_managed_policies(
    state: State<'_, AppState>,
    account_id: String,
    scope: PolicyScope,
) -> CommandResult<Vec<ManagedPolicy>> {
    let core = state.core()?;
    logged("listing managed policies", async {
        core.session
            .iam_for(&account_id)
            .await?
            .list_managed_policies(scope)
            .await
    })
    .await
}

/// Attaches a managed policy to a user.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_attach_policy(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    policy_arn: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("attaching a policy", async {
        core.session
            .iam_for(&account_id)
            .await?
            .attach_user_policy(&user, &policy_arn)
            .await
    })
    .await
}

/// Detaches a managed policy from a user.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_detach_policy(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    policy_arn: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("detaching a policy", async {
        core.session
            .iam_for(&account_id)
            .await?
            .detach_user_policy(&user, &policy_arn)
            .await
    })
    .await
}

/// Reads an inline policy (pretty-printed).
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_get_inline_policy(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    name: String,
) -> CommandResult<InlinePolicy> {
    let core = state.core()?;
    logged("reading an inline policy", async {
        core.session
            .iam_for(&account_id)
            .await?
            .get_inline_policy(&user, &name)
            .await
    })
    .await
}

/// Creates or replaces an inline policy after validating the document
/// (`usage` lists every problem).
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_put_inline_policy(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    name: String,
    document: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving an inline policy", async {
        core.session
            .iam_for(&account_id)
            .await?
            .put_inline_policy(&user, &name, &document)
            .await
    })
    .await
}

/// Deletes an inline policy; `confirm` must be the policy name.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_delete_inline_policy(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    name: String,
    confirm: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting an inline policy", async {
        core.session
            .iam_for(&account_id)
            .await?
            .delete_inline_policy(&user, &name, &confirm)
            .await
    })
    .await
}

/// Every problem of a user policy document (empty when it is fine); no
/// account and no network involved.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_validate_policy(document: String) -> CommandResult<Vec<PolicyIssue>> {
    Ok(iam::check_user_policy(&document))
}

/// Pretty-prints a policy document (`usage` when it is not valid JSON).
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_format_policy(document: String) -> CommandResult<String> {
    Ok(iam::format_user_policy(&document)?)
}

/// Creates an access key. The answer holds the secret: it is shown once and
/// never stored or logged.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_create_access_key(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
) -> CommandResult<NewAccessKey> {
    let core = state.core()?;
    logged("creating an access key", async {
        let key = core
            .session
            .iam_for(&account_id)
            .await?
            .create_access_key(&user)
            .await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "created access key {} for IAM user {user}",
            key.access_key_id
        );
        Ok(key)
    })
    .await
}

/// The access keys of a user (no secrets).
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_list_access_keys(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
) -> CommandResult<Vec<AccessKeyInfo>> {
    let core = state.core()?;
    logged("listing access keys", async {
        core.session
            .iam_for(&account_id)
            .await?
            .list_access_keys(&user)
            .await
    })
    .await
}

/// Activates or deactivates an access key.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_set_access_key_active(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    access_key_id: String,
    active: bool,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("changing an access key", async {
        core.session
            .iam_for(&account_id)
            .await?
            .set_access_key_active(&user, &access_key_id, active)
            .await
    })
    .await
}

/// Deletes an access key; `confirm` must be the access key ID.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_delete_access_key(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    access_key_id: String,
    confirm: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting an access key", async {
        core.session
            .iam_for(&account_id)
            .await?
            .delete_access_key(&user, &access_key_id, &confirm)
            .await
    })
    .await
}

/// Gives a user a console password. The password is never logged.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_create_login_profile(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    password: String,
    reset_required: bool,
) -> CommandResult<()> {
    let core = state.core()?;
    let password = SecretString::new(password);
    logged("creating a login profile", async {
        core.session
            .iam_for(&account_id)
            .await?
            .create_login_profile(&user, &password, reset_required)
            .await
    })
    .await
}

/// Changes the console password and/or the reset requirement.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_update_login_profile(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    password: Option<String>,
    reset_required: Option<bool>,
) -> CommandResult<()> {
    let core = state.core()?;
    let password = password.map(SecretString::new);
    logged("updating a login profile", async {
        core.session
            .iam_for(&account_id)
            .await?
            .update_login_profile(&user, password.as_ref(), reset_required)
            .await
    })
    .await
}

/// Removes the console password; `confirm` must be the user name.
#[tauri::command(rename_all = "snake_case")]
pub async fn iam_delete_login_profile(
    state: State<'_, AppState>,
    account_id: String,
    user: String,
    confirm: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting a login profile", async {
        core.session
            .iam_for(&account_id)
            .await?
            .delete_login_profile(&user, &confirm)
            .await
    })
    .await
}
