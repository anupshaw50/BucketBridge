//! CloudFront commands (docs/desktop-api.md, "CloudFront (T09)"): the
//! distribution manager, invalidations and the saved invalidation path sets.
//!
//! Each command resolves the account with `Session::cloudfront_for` (which
//! refuses accounts with a custom endpoint with `usage`) and calls
//! `bucketbridge_core::cloudfront`. Changes that add or remove the origins
//! of a bucket forget the distributions the session keeps for the bucket
//! badges.

use bucketbridge_core::cloudfront::{
    self, CreatedDistribution, Distribution, DistributionEdit, DistributionPage, Invalidation,
    InvalidationPage, NewDistribution, PathSet,
};
use tauri::State;

use super::{CommandResult, blocking, logged};
use crate::state::AppState;

/// One page of the account's distributions; `marker` is the `next_marker` of
/// the previous page.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_cloudfront_distributions(
    state: State<'_, AppState>,
    account_id: String,
    marker: Option<String>,
) -> CommandResult<DistributionPage> {
    let core = state.core()?;
    logged("listing distributions", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront.list_distributions_page(marker.as_deref()).await
    })
    .await
}

/// One distribution with its ETag, default root object and the number of
/// invalidation batches in progress.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_cloudfront_distribution(
    state: State<'_, AppState>,
    account_id: String,
    id: String,
) -> CommandResult<Distribution> {
    let core = state.core()?;
    logged("reading a distribution", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront.get_distribution(&id).await
    })
    .await
}

/// The distributions that have an origin in `bucket`.
#[tauri::command(rename_all = "snake_case")]
pub async fn find_cloudfront_distributions(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> CommandResult<Vec<Distribution>> {
    let core = state.core()?;
    logged("finding the distributions of a bucket", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront.distributions_for_bucket(&bucket).await
    })
    .await
}

/// Creates a distribution for a bucket (origin access control, regional
/// bucket origin, basic default behaviour). The result carries the bucket
/// policy to add to the bucket; the bucket itself is not changed.
#[tauri::command(rename_all = "snake_case")]
pub async fn create_cloudfront_distribution(
    state: State<'_, AppState>,
    account_id: String,
    new: NewDistribution,
) -> CommandResult<CreatedDistribution> {
    let core = state.core()?;
    logged("creating a distribution", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        let created = cloudfront.create_distribution(&new).await;
        core.session.forget_cloudfront(&account_id);
        created
    })
    .await
}

/// Applies `edit` (comment, aliases, default root object, price class,
/// enabled; a `null` field stays as it is). `expected_etag` is the ETag the
/// caller showed; a distribution changed since fails with `conflict` and
/// nothing is sent.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_cloudfront_distribution(
    state: State<'_, AppState>,
    account_id: String,
    id: String,
    edit: DistributionEdit,
    expected_etag: Option<String>,
) -> CommandResult<Distribution> {
    let core = state.core()?;
    logged("updating a distribution", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront
            .update_distribution(&id, &edit, expected_etag.as_deref())
            .await
    })
    .await
}

/// Enables or disables a distribution.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_cloudfront_enabled(
    state: State<'_, AppState>,
    account_id: String,
    id: String,
    enabled: bool,
    expected_etag: Option<String>,
) -> CommandResult<Distribution> {
    let core = state.core()?;
    logged("enabling or disabling a distribution", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront
            .set_enabled(&id, enabled, expected_etag.as_deref())
            .await
    })
    .await
}

/// Removes the origins of `bucket` from a distribution.
#[tauri::command(rename_all = "snake_case")]
pub async fn detach_cloudfront_bucket(
    state: State<'_, AppState>,
    account_id: String,
    id: String,
    bucket: String,
    expected_etag: Option<String>,
) -> CommandResult<Distribution> {
    let core = state.core()?;
    logged("detaching a bucket from a distribution", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        let detached = cloudfront
            .detach_bucket(&id, &bucket, expected_etag.as_deref())
            .await;
        core.session.forget_cloudfront(&account_id);
        detached
    })
    .await
}

/// Creates an invalidation. Paths are checked first (`usage` lists every
/// problem).
#[tauri::command(rename_all = "snake_case")]
pub async fn create_cloudfront_invalidation(
    state: State<'_, AppState>,
    account_id: String,
    distribution_id: String,
    paths: Vec<String>,
) -> CommandResult<Invalidation> {
    let core = state.core()?;
    logged("creating an invalidation", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront
            .create_invalidation(&distribution_id, &paths)
            .await
    })
    .await
}

/// One page of the invalidation history, newest first.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_cloudfront_invalidations(
    state: State<'_, AppState>,
    account_id: String,
    distribution_id: String,
    marker: Option<String>,
) -> CommandResult<InvalidationPage> {
    let core = state.core()?;
    logged("listing invalidations", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront
            .list_invalidations(&distribution_id, marker.as_deref())
            .await
    })
    .await
}

/// One invalidation with its paths.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_cloudfront_invalidation(
    state: State<'_, AppState>,
    account_id: String,
    distribution_id: String,
    invalidation_id: String,
) -> CommandResult<Invalidation> {
    let core = state.core()?;
    logged("reading an invalidation", async {
        let cloudfront = core.session.cloudfront_for(&account_id).await?;
        cloudfront
            .get_invalidation(&distribution_id, &invalidation_id)
            .await
    })
    .await
}

/// Checks invalidation paths without sending anything and returns them as
/// they would be sent (trimmed, without repeats); `usage` lists every
/// problem.
#[tauri::command(rename_all = "snake_case")]
pub async fn validate_invalidation_paths(paths: Vec<String>) -> CommandResult<Vec<String>> {
    logged("checking invalidation paths", async {
        cloudfront::validate_paths(&paths)
    })
    .await
}

/// The saved invalidation path sets, in order of creation.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_invalidation_path_sets(
    state: State<'_, AppState>,
) -> CommandResult<Vec<PathSet>> {
    let core = state.core()?;
    let store = core.session.accounts().config_store().clone();
    logged("listing path sets", async {
        blocking(move || cloudfront::list_path_sets(&store)).await
    })
    .await
}

/// Adds a path set, or replaces the one named `replaces` (keeping its place);
/// returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_invalidation_path_set(
    state: State<'_, AppState>,
    set: PathSet,
    replaces: Option<String>,
) -> CommandResult<Vec<PathSet>> {
    let core = state.core()?;
    let store = core.session.accounts().config_store().clone();
    logged("saving a path set", async {
        blocking(move || cloudfront::save_path_set(&store, set, replaces.as_deref())).await
    })
    .await
}

/// Removes the path set named `name`; returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_invalidation_path_set(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<Vec<PathSet>> {
    let core = state.core()?;
    let store = core.session.accounts().config_store().clone();
    logged("deleting a path set", async {
        blocking(move || cloudfront::delete_path_set(&store, &name)).await
    })
    .await
}
