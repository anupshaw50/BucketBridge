//! Tools and export commands (docs/desktop-api.md, "Tools and export
//! (T12)"): diagnostics, uncompleted multipart uploads, saved filters and
//! the export of a listing.

use std::path::Path;

use bucketbridge_core::diagnostics::{self, Report, StepStatus};
use bucketbridge_core::export::{self, Format, Outcome, Request};
use bucketbridge_core::model::{AccountInput, FilterSpec, MultipartUpload, SavedFilter};
use bucketbridge_core::s3uri::{validate_bucket_name, validate_key};
use bucketbridge_core::{BbError, Error, Result, saved_filters};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{CommandResult, blocking, logged, require_absolute};
use crate::state::AppState;

/// Buckets whose uploads are listed at the same time.
const LIST_CONCURRENCY: usize = 4;

/// Probes a stored account (`account_id`) or an unsaved one (`input`), one
/// step at a time. A failing step is a result in the report, not a
/// rejection.
#[tauri::command(rename_all = "snake_case")]
pub async fn run_diagnostics(
    state: State<'_, AppState>,
    account_id: Option<String>,
    input: Option<AccountInput>,
    bucket: Option<String>,
) -> CommandResult<Report> {
    let core = state.core()?;
    logged("running diagnostics", async {
        let report = match (account_id, input) {
            (Some(id), None) => {
                diagnostics::probe_account(core.session(), &id, bucket.as_deref()).await?
            }
            (None, Some(input)) => {
                diagnostics::probe_input(core.session(), &input, bucket.as_deref()).await?
            }
            _ => {
                return Err(Error::Usage(
                    "give exactly one of account_id and input".into(),
                ));
            }
        };
        let failed: Vec<String> = report
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Fail)
            .map(|step| format!("{:?}", step.id).to_lowercase())
            .collect();
        if failed.is_empty() {
            tracing::info!(target: "bucketbridge::desktop", "diagnostics: every check passed");
        } else {
            tracing::warn!(
                target: "bucketbridge::desktop",
                "diagnostics: failed {}",
                failed.join(", ")
            );
        }
        Ok(report)
    })
    .await
}

/// One uncompleted upload and the bucket it belongs to.
#[derive(Debug, Serialize)]
pub struct UploadRow {
    bucket: String,
    #[serde(flatten)]
    upload: MultipartUpload,
}

/// A bucket whose uploads could not be listed.
#[derive(Debug, Serialize)]
pub struct BucketFailure {
    bucket: String,
    error: BbError,
}

/// Result of `list_multipart_uploads`.
#[derive(Debug, Serialize)]
pub struct UploadListing {
    uploads: Vec<UploadRow>,
    failures: Vec<BucketFailure>,
}

/// The uncompleted multipart uploads of the named buckets (four buckets at
/// a time); a bucket that cannot be listed goes to `failures`.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_multipart_uploads(
    state: State<'_, AppState>,
    account_id: String,
    buckets: Vec<String>,
) -> CommandResult<UploadListing> {
    let core = state.core()?;
    logged("listing uncompleted uploads", async {
        let mut names: Vec<String> = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            validate_bucket_name(&bucket)?;
            if !names.contains(&bucket) {
                names.push(bucket);
            }
        }
        if names.is_empty() {
            return Err(Error::Usage("name at least one bucket".into()));
        }
        let s3 = core.session.s3_for(&account_id).await?;
        let listed: Vec<(String, Result<Vec<MultipartUpload>>)> = futures::stream::iter(names)
            .map(|bucket| {
                let s3 = s3.clone();
                async move {
                    let result = s3.list_multipart_uploads(&bucket, "").await;
                    (bucket, result)
                }
            })
            .buffered(LIST_CONCURRENCY)
            .collect()
            .await;
        let mut uploads = Vec::new();
        let mut failures = Vec::new();
        for (bucket, result) in listed {
            match result {
                Ok(found) => uploads.extend(found.into_iter().map(|upload| UploadRow {
                    bucket: bucket.clone(),
                    upload,
                })),
                Err(error) => failures.push(BucketFailure {
                    bucket,
                    error: error.to_payload(),
                }),
            }
        }
        uploads.sort_by(|a, b| {
            (
                &a.bucket,
                &a.upload.key,
                a.upload.initiated,
                &a.upload.upload_id,
            )
                .cmp(&(
                    &b.bucket,
                    &b.upload.key,
                    b.upload.initiated,
                    &b.upload.upload_id,
                ))
        });
        Ok(UploadListing { uploads, failures })
    })
    .await
}

/// An upload to abort.
#[derive(Debug, Deserialize)]
pub struct UploadRef {
    bucket: String,
    key: String,
    upload_id: String,
}

/// What became of one upload of `abort_multipart_uploads`.
#[derive(Debug, Serialize)]
pub struct AbortResult {
    bucket: String,
    key: String,
    upload_id: String,
    aborted: bool,
    error: Option<BbError>,
}

/// Aborts exactly the named uploads, one after the other, and reports each
/// result; a failure never stops the rest.
#[tauri::command(rename_all = "snake_case")]
pub async fn abort_multipart_uploads(
    state: State<'_, AppState>,
    account_id: String,
    uploads: Vec<UploadRef>,
) -> CommandResult<Vec<AbortResult>> {
    let core = state.core()?;
    logged("aborting uncompleted uploads", async {
        if uploads.is_empty() {
            return Err(Error::Usage("name at least one upload".into()));
        }
        for upload in &uploads {
            validate_bucket_name(&upload.bucket)?;
            validate_key(&upload.key)?;
            if upload.upload_id.is_empty() {
                return Err(Error::Usage("an upload ID is empty".into()));
            }
        }
        let s3 = core.session.s3_for(&account_id).await?;
        let mut results = Vec::with_capacity(uploads.len());
        for upload in uploads {
            let outcome = s3
                .abort_multipart_upload(&upload.bucket, &upload.key, &upload.upload_id)
                .await;
            results.push(AbortResult {
                aborted: outcome.is_ok(),
                error: outcome.err().map(|error| error.to_payload()),
                bucket: upload.bucket,
                key: upload.key,
                upload_id: upload.upload_id,
            });
        }
        let aborted = results.iter().filter(|result| result.aborted).count();
        tracing::info!(
            target: "bucketbridge::desktop",
            "aborted {aborted} of {} uncompleted upload(s)",
            results.len()
        );
        Ok(results)
    })
    .await
}

/// The saved filters, in order.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_filters(state: State<'_, AppState>) -> CommandResult<Vec<SavedFilter>> {
    let core = state.core()?;
    logged("reading the saved filters", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || saved_filters::list(&store)).await
    })
    .await
}

/// Adds a filter (`replaces: null`) or replaces the one named `replaces`
/// (a rename included); returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_filter(
    state: State<'_, AppState>,
    filter: SavedFilter,
    replaces: Option<String>,
) -> CommandResult<Vec<SavedFilter>> {
    let core = state.core()?;
    logged("saving a filter", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || saved_filters::save(&store, filter, replaces.as_deref())).await
    })
    .await
}

/// Removes a filter; returns the stored list.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_filter(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<Vec<SavedFilter>> {
    let core = state.core()?;
    logged("deleting a filter", async {
        let store = core.session.accounts().config_store().clone();
        blocking(move || saved_filters::delete(&store, &name)).await
    })
    .await
}

/// Writes the listing of `s3://bucket/prefix` to `path` (absolute) as CSV,
/// XLSX or Parquet; the file is replaced only by a complete export.
// One argument per field of the IPC contract, as for every command.
#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
pub async fn export_listing(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    recursive: bool,
    format: Format,
    path: String,
    filter: Option<FilterSpec>,
) -> CommandResult<Outcome> {
    let core = state.core()?;
    logged("exporting a listing", async {
        require_absolute("the export file", &path)?;
        let s3 = core.session.s3_for(&account_id).await?;
        let request = Request {
            bucket,
            prefix,
            recursive,
            format,
            filter,
        };
        let outcome = export::export_listing(&s3, &request, Path::new(&path), None).await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "exported {} row(s) of s3://{}/{} to {}",
            outcome.rows,
            request.bucket,
            request.prefix,
            outcome.path
        );
        Ok(outcome)
    })
    .await
}
