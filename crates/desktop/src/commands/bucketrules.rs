//! Bucket rule commands (docs/desktop-api.md, "Bucket rules (T04/T07)"):
//! lifecycle, Intelligent-Tiering, CORS and replication of a bucket.

use bucketbridge_core::bucketrules::{
    self, BucketRules, CorsConfig, LifecycleConfig, ReplicationConfig, RuleIssue, TieringConfig,
};
use tauri::State;

use super::{CommandResult, logged};
use crate::state::AppState;

/// The four rule settings of a bucket. Each one is reported on its own
/// (`loaded`, `unsupported` or `failed`), so one that is refused or not
/// implemented never hides the others.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_rules(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BucketRules> {
    let core = state.core()?;
    logged("reading the bucket rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_rules(&bucket).await
    })
    .await
}

/// The lifecycle rules; `null` when the bucket has none.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_lifecycle(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<LifecycleConfig>> {
    let core = state.core()?;
    logged("reading the lifecycle rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_lifecycle(&bucket).await
    })
    .await
}

/// Replaces the lifecycle rules after validating them (`usage` lists every
/// problem with its path).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_lifecycle(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: LifecycleConfig,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the lifecycle rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_lifecycle(&bucket, &config).await
    })
    .await
}

/// Deletes every lifecycle rule; succeeds when there are none.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_lifecycle(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting the lifecycle rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_lifecycle(&bucket).await
    })
    .await
}

/// Every problem of a lifecycle document (JSON text), each with its path; an
/// empty list means it is valid. Needs no account.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_bucket_lifecycle(
    state: State<'_, AppState>,
    document: String,
) -> CommandResult<Vec<RuleIssue>> {
    state.core()?;
    Ok(bucketrules::check_lifecycle_json(&document))
}

/// The text of a rules document saved as a `.json` file (import).
#[tauri::command(rename_all = "snake_case")]
pub async fn read_rules_file(state: State<'_, AppState>, path: String) -> CommandResult<String> {
    state.core()?;
    logged("reading a rules file", async {
        super::require_absolute("the rules file path", &path)?;
        super::blocking(move || bucketrules::read_rules_file(std::path::Path::new(&path))).await
    })
    .await
}

/// Saves a rules document as a `.json` file (export).
#[tauri::command(rename_all = "snake_case")]
pub async fn write_rules_file(
    state: State<'_, AppState>,
    path: String,
    document: String,
) -> CommandResult<()> {
    state.core()?;
    logged("writing a rules file", async {
        super::require_absolute("the rules file path", &path)?;
        super::blocking(move || {
            bucketrules::write_rules_file(std::path::Path::new(&path), &document)
        })
        .await
    })
    .await
}

/// The Intelligent-Tiering configurations of the bucket, sorted by id.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_bucket_tiering(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Vec<TieringConfig>> {
    let core = state.core()?;
    logged("listing the Intelligent-Tiering configurations", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.list_bucket_tiering(&bucket).await
    })
    .await
}

/// Creates or replaces the Intelligent-Tiering configuration with the id of
/// `config`, after validating it.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_tiering(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: TieringConfig,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the Intelligent-Tiering configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_tiering(&bucket, &config).await
    })
    .await
}

/// Deletes one Intelligent-Tiering configuration; succeeds when the id does
/// not exist.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_tiering(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    id: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting the Intelligent-Tiering configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_tiering(&bucket, &id).await
    })
    .await
}

/// Every problem of an Intelligent-Tiering document (JSON text). Needs no
/// account.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_bucket_tiering(
    state: State<'_, AppState>,
    document: String,
) -> CommandResult<Vec<RuleIssue>> {
    state.core()?;
    Ok(bucketrules::check_tiering_json(&document))
}

/// The CORS rules; `null` when the bucket has none.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_cors(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<CorsConfig>> {
    let core = state.core()?;
    logged("reading the CORS rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_cors(&bucket).await
    })
    .await
}

/// Replaces the CORS rules after validating them.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_cors(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: CorsConfig,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the CORS rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_cors(&bucket, &config).await
    })
    .await
}

/// Deletes every CORS rule; succeeds when there are none.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_cors(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting the CORS rules", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_cors(&bucket).await
    })
    .await
}

/// Every problem of a CORS document (JSON text). Needs no account.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_bucket_cors(
    state: State<'_, AppState>,
    document: String,
) -> CommandResult<Vec<RuleIssue>> {
    state.core()?;
    Ok(bucketrules::check_cors_json(&document))
}

/// The replication configuration; `null` when the bucket does not replicate.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_replication(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<ReplicationConfig>> {
    let core = state.core()?;
    logged("reading the replication configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_replication(&bucket).await
    })
    .await
}

/// Replaces the replication configuration after validating it. Versioning
/// must be enabled on the bucket (`usage` with a clear message otherwise).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_replication(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: ReplicationConfig,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the replication configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_replication(&bucket, &config).await
    })
    .await
}

/// Stops replication; succeeds when the bucket did not replicate.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_replication(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting the replication configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_replication(&bucket).await
    })
    .await
}

/// Every problem of a replication document (JSON text). Needs no account;
/// the versioning of the source is checked when the document is saved.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_bucket_replication(
    state: State<'_, AppState>,
    document: String,
) -> CommandResult<Vec<RuleIssue>> {
    state.core()?;
    Ok(bucketrules::check_replication_json(&document))
}
