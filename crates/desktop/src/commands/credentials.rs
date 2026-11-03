//! Credential source commands (docs/desktop-api.md, "Credential sources
//! (v0.2.0)"): AWS profiles, the instance role and IAM Identity Center
//! sign-in. Adding and editing accounts of these kinds goes through
//! `add_account` and `update_account` (their `source` field).

use bucketbridge_core::creds::instance::InstanceRoleProbe;
use bucketbridge_core::creds::profiles::{AwsProfileEntry, ImportOutcome};
use bucketbridge_core::creds::sso::{SignInStatus, SsoAccount};
use bucketbridge_core::session::{SsoLoginPoll, SsoLoginStart};
use serde::Serialize;
use tauri::State;

use super::{CommandResult, logged};
use crate::state::AppState;

/// Result of `sso_start_login`: the sign-in to poll and whether the browser
/// was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SsoLoginStarted {
    /// What the user needs and the handle to poll with.
    #[serde(flatten)]
    pub login: SsoLoginStart,
    /// Whether the verification page was handed to the system browser. When
    /// `false` the UI shows the URL for the user to open.
    pub browser_opened: bool,
}

/// The profiles of the AWS config and credentials files (honouring
/// `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE`), each with the
/// account that is linked to it already. Never returns a secret.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_aws_profiles(state: State<'_, AppState>) -> CommandResult<Vec<AwsProfileEntry>> {
    let core = state.core()?;
    logged("listing the AWS profiles", core.session.list_aws_profiles()).await
}

/// Creates one account per named profile, linked to the AWS files (only the
/// profile name is stored). Profiles that cannot be imported are reported
/// in `skipped`.
#[tauri::command(rename_all = "snake_case")]
pub async fn import_aws_profiles(
    state: State<'_, AppState>,
    names: Vec<String>,
) -> CommandResult<ImportOutcome> {
    let core = state.core()?;
    logged(
        "importing AWS profiles",
        core.session.import_aws_profiles(&names),
    )
    .await
}

/// Checks that the machine has an EC2 instance or container role; fails with
/// `auth` and an explanation when it has none.
#[tauri::command(rename_all = "snake_case")]
pub async fn probe_instance_role(state: State<'_, AppState>) -> CommandResult<InstanceRoleProbe> {
    let core = state.core()?;
    logged(
        "checking the instance role",
        core.session.probe_instance_role(),
    )
    .await
}

/// Starts an IAM Identity Center sign-in: returns the user code and the
/// verification URL, and opens the page in the system browser unless
/// `open_browser` is `false`.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_start_login(
    state: State<'_, AppState>,
    start_url: String,
    sso_region: String,
    sso_session: Option<String>,
    open_browser: Option<bool>,
) -> CommandResult<SsoLoginStarted> {
    let core = state.core()?;
    let login = logged(
        "starting the SSO sign-in",
        core.session
            .sso_start_login(&start_url, &sso_region, sso_session.as_deref()),
    )
    .await?;
    let target = login
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&login.verification_uri);
    let browser_opened = open_browser.unwrap_or(true)
        && target.starts_with("https://")
        && (core.url_opener)(target).is_ok();
    Ok(SsoLoginStarted {
        login,
        browser_opened,
    })
}

/// Polls a sign-in once; call it every `interval_secs` until it reports
/// `complete` or fails (`auth` when denied or expired).
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_poll_login(
    state: State<'_, AppState>,
    login_id: String,
) -> CommandResult<SsoLoginPoll> {
    let core = state.core()?;
    logged(
        "polling the SSO sign-in",
        core.session.sso_poll_login(&login_id),
    )
    .await
}

/// Forgets a sign-in in progress.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_cancel_login(state: State<'_, AppState>, login_id: String) -> CommandResult<()> {
    let core = state.core()?;
    core.session.sso_cancel_login(&login_id);
    Ok(())
}

/// Whether a portal has a usable sign-in in the AWS CLI token cache.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_status(
    state: State<'_, AppState>,
    start_url: String,
    sso_region: String,
    sso_session: Option<String>,
) -> CommandResult<SignInStatus> {
    let core = state.core()?;
    logged(
        "reading the SSO sign-in",
        core.session
            .sso_status(&start_url, &sso_region, sso_session.as_deref()),
    )
    .await
}

/// Deletes the cached sign-in of a portal.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_sign_out(
    state: State<'_, AppState>,
    start_url: String,
    sso_region: String,
    sso_session: Option<String>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged(
        "signing out of SSO",
        core.session
            .sso_sign_out(&start_url, &sso_region, sso_session.as_deref()),
    )
    .await
}

/// The AWS accounts the signed-in user can reach through a portal.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_list_accounts(
    state: State<'_, AppState>,
    start_url: String,
    sso_region: String,
    sso_session: Option<String>,
) -> CommandResult<Vec<SsoAccount>> {
    let core = state.core()?;
    logged(
        "listing the SSO accounts",
        core.session
            .sso_list_accounts(&start_url, &sso_region, sso_session.as_deref()),
    )
    .await
}

/// The roles (permission sets) of one AWS account of a portal.
#[tauri::command(rename_all = "snake_case")]
pub async fn sso_list_roles(
    state: State<'_, AppState>,
    start_url: String,
    sso_region: String,
    sso_session: Option<String>,
    account_id: String,
) -> CommandResult<Vec<String>> {
    let core = state.core()?;
    logged(
        "listing the SSO roles",
        core.session
            .sso_list_roles(&start_url, &sso_region, sso_session.as_deref(), &account_id),
    )
    .await
}
