//! Bucket configuration commands (docs/desktop-api.md, "Bucket configuration
//! (T03)"): policy, public access block, versioning, server access logging,
//! website hosting and transfer acceleration of a bucket, and the external
//! buckets of an account.

use bucketbridge_core::bucketcfg::{
    self, AccelerationStatus, BucketConfig, BucketVersioning, LoggingTarget, PolicyIssue,
    PublicAccessBlock, VersioningStatus, WebsiteConfig,
};
use tauri::State;

use super::{CommandResult, blocking, logged};
use crate::state::AppState;

/// Every configuration section of a bucket. Each section is reported on its
/// own (`loaded`, `unsupported` or `failed`), so one that is refused or not
/// implemented never hides the others.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_config(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BucketConfig> {
    let core = state.core()?;
    logged("reading the bucket configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_config(&bucket).await
    })
    .await
}

/// The bucket policy, pretty-printed; `null` when the bucket has none.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_policy(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<String>> {
    let core = state.core()?;
    logged("reading the bucket policy", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_policy(&bucket).await
    })
    .await
}

/// Replaces the bucket policy after validating it (`usage` lists every
/// problem).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_policy(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    policy: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the bucket policy", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_policy(&bucket, &policy).await
    })
    .await
}

/// Deletes the bucket policy; succeeds when there is none.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_policy(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting the bucket policy", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_policy(&bucket).await
    })
    .await
}

/// Every problem of a policy document, each with its path; an empty list
/// means it is valid. Needs no account.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_bucket_policy(
    state: State<'_, AppState>,
    policy: String,
) -> CommandResult<Vec<PolicyIssue>> {
    state.core()?;
    Ok(bucketcfg::check_policy(&policy))
}

/// A policy pretty-printed with its keys in the order written (`usage` when
/// the text is not JSON). Needs no account.
#[tauri::command(rename_all = "snake_case")]
pub async fn format_bucket_policy(
    state: State<'_, AppState>,
    policy: String,
) -> CommandResult<String> {
    state.core()?;
    Ok(bucketcfg::format_policy(&policy)?)
}

/// The public access block; `null` when none is configured.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_public_access_block(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<PublicAccessBlock>> {
    let core = state.core()?;
    logged("reading the public access block", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_public_access_block(&bucket).await
    })
    .await
}

/// Replaces the public access block (all four toggles are sent).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_public_access_block(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    config: PublicAccessBlock,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the public access block", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_public_access_block(&bucket, &config).await
    })
    .await
}

/// Removes the public access block; succeeds when there is none.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_public_access_block(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("removing the public access block", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_public_access_block(&bucket).await
    })
    .await
}

/// The versioning state and whether MFA delete is on.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_versioning(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<BucketVersioning> {
    let core = state.core()?;
    logged("reading the versioning state", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_versioning(&bucket).await
    })
    .await
}

/// Enables or suspends versioning (`unversioned` is `usage`: S3 cannot switch
/// versioning off again). MFA delete is never changed.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_versioning(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    status: VersioningStatus,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the versioning state", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_versioning(&bucket, status).await
    })
    .await
}

/// Where the bucket delivers its server access logs; `null` while logging is
/// off.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_logging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<LoggingTarget>> {
    let core = state.core()?;
    logged("reading the access logging", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_logging(&bucket).await
    })
    .await
}

/// Turns server access logging on or changes its target; the target grants
/// and the log key format the bucket already has are kept.
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_logging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    target: LoggingTarget,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the access logging", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_logging(&bucket, &target).await
    })
    .await
}

/// Turns server access logging off.
#[tauri::command(rename_all = "snake_case")]
pub async fn disable_bucket_logging(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("turning off the access logging", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.disable_bucket_logging(&bucket).await
    })
    .await
}

/// The website configuration; `null` when the bucket is not a website.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_website(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Option<WebsiteConfig>> {
    let core = state.core()?;
    logged("reading the website configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_website(&bucket).await
    })
    .await
}

/// Replaces the website configuration after checking it (`usage` lists every
/// problem).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_website(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    website: WebsiteConfig,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving the website configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_website(&bucket, &website).await
    })
    .await
}

/// Turns website hosting off; succeeds when it was off.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket_website(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("removing the website configuration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket_website(&bucket).await
    })
    .await
}

/// The transfer acceleration state. `usage` for an account with a custom
/// endpoint: acceleration exists on AWS only.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_bucket_acceleration(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<AccelerationStatus> {
    let core = state.core()?;
    logged("reading transfer acceleration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.get_bucket_acceleration(&bucket).await
    })
    .await
}

/// Enables or suspends transfer acceleration (`unset` is `usage`, as is a
/// custom endpoint or a bucket name with dots).
#[tauri::command(rename_all = "snake_case")]
pub async fn put_bucket_acceleration(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    status: AccelerationStatus,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("saving transfer acceleration", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.put_bucket_acceleration(&bucket, status).await
    })
    .await
}

/// The names of the external buckets pinned to the account, sorted. Reads the
/// configuration file only, so it works while the secrets are locked.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_external_buckets(
    state: State<'_, AppState>,
    account_id: String,
) -> CommandResult<Vec<String>> {
    let core = state.core()?;
    logged("listing the external buckets", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || bucketcfg::load_pinned_buckets(&store, &account_id)).await
    })
    .await
}

/// Pins a bucket to the account so that it shows in the bucket list although
/// the account cannot list it. The bucket must exist (`not_found`
/// otherwise); being refused access to it is fine. Pinning twice changes
/// nothing.
#[tauri::command(rename_all = "snake_case")]
pub async fn add_external_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("adding an external bucket", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.verify_bucket_exists(&name).await?;
        let store = core.session.accounts().config_store().clone();
        let (id, bucket) = (account_id.clone(), name.clone());
        blocking(move || bucketcfg::pin_bucket(&store, &id, &bucket)).await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "bucket {name} pinned to account {account_id} as an external bucket"
        );
        Ok(())
    })
    .await
}

/// Removes the pin of an external bucket (`not_found` when it is not pinned).
/// The bucket itself is untouched. Works while the secrets are locked.
#[tauri::command(rename_all = "snake_case")]
pub async fn remove_external_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("removing an external bucket", async {
        let store = core.session.accounts().config_store().clone();
        let (id, bucket) = (account_id.clone(), name.clone());
        blocking(move || bucketcfg::unpin_bucket(&store, &id, &bucket)).await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "external bucket {name} removed from account {account_id}"
        );
        Ok(())
    })
    .await
}
