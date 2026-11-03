//! Browsing commands (docs/desktop-api.md, "Browsing").

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bucketbridge_core::bucketcfg;
use bucketbridge_core::engine::{DownloadState, Hooks, Outcome, TransferEngine, TransferOptions};
use bucketbridge_core::model::{Bucket, ObjectPage};
use bucketbridge_core::path::safe_local_path;
use bucketbridge_core::{Error, S3Uri};
use serde::Deserialize;
use tauri::State;

use super::{CommandResult, blocking, logged};
use crate::open::{self, LimiterThrottle};
use crate::state::AppState;

/// Every bucket of the account, sorted by name, with the external buckets
/// pinned to the account (kind `external`). When the account is refused the
/// bucket list but has pinned buckets, those are returned alone.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_buckets(
    state: State<'_, AppState>,
    account_id: String,
) -> CommandResult<Vec<Bucket>> {
    let core = state.core()?;
    logged("listing buckets", async {
        let s3 = core.session.s3_for(&account_id).await?;
        let store = core.session.accounts().config_store().clone();
        let id = account_id.clone();
        let pinned = blocking(move || bucketcfg::load_pinned_buckets(&store, &id)).await?;
        let mut buckets = s3.list_buckets_with_pinned(&pinned).await?;
        // The badge costs one listing of distributions per session.
        core.session
            .mark_cloudfront_buckets(&account_id, &mut buckets)
            .await;
        Ok(buckets)
    })
    .await
}

/// One page of the listing of `prefix`, `settings.advanced.list_page_size`
/// entries at most; `token` is the `next_token` of the previous page.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_objects(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: String,
    flat: bool,
    token: Option<String>,
) -> CommandResult<ObjectPage> {
    let core = state.core()?;
    logged("listing objects", async {
        let page_size = core.session.settings().await?.advanced.list_page_size;
        let s3 = core.session.s3_for(&account_id).await?;
        s3.list_objects(&bucket, &prefix, flat, token.as_deref(), page_size)
            .await
    })
    .await
}

/// Creates a folder marker; `key` ends with `/`.
#[tauri::command(rename_all = "snake_case")]
pub async fn create_folder(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("creating a folder", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.create_folder(&bucket, &key).await
    })
    .await
}

/// Creates a bucket, in `region` or the account's region.
#[tauri::command(rename_all = "snake_case")]
pub async fn create_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
    region: Option<String>,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("creating a bucket", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.create_bucket(&name, region.as_deref()).await
    })
    .await
}

/// Deletes an empty bucket (`conflict` while it holds objects).
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_bucket(
    state: State<'_, AppState>,
    account_id: String,
    name: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("deleting a bucket", async {
        let s3 = core.session.s3_for(&account_id).await?;
        s3.delete_bucket(&name).await
    })
    .await
}

/// HTTP method of a presigned URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresignMethod {
    /// Download URL.
    Get,
    /// Upload URL.
    Put,
}

/// A presigned URL valid for `expires_secs` (1 s to 7 days).
#[tauri::command(rename_all = "snake_case")]
pub async fn presign(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    method: PresignMethod,
    expires_secs: u64,
) -> CommandResult<String> {
    let core = state.core()?;
    logged("creating a presigned URL", async {
        let expires = Duration::from_secs(expires_secs);
        let s3 = core.session.s3_for(&account_id).await?;
        match method {
            PresignMethod::Get => s3.presign_get(&bucket, &key, expires).await,
            PresignMethod::Put => s3.presign_put(&bucket, &key, expires).await,
        }
    })
    .await
}

/// Downloads the object through the download engine (bandwidth cap and
/// download validation per the settings) into a new private folder
/// `data_dir/open/<uuid>/`, then opens it with the OS default application
/// through the opener plugin's Rust API. A failed or cancelled download
/// leaves nothing behind. Copies are deleted at a later start once they are a
/// day old.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_object(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("opening an object", async {
        let (src, dest) = download_for_open(&core, &account_id, &bucket, &key).await?;
        let opener = Arc::clone(&core.opener);
        let path = dest.clone();
        blocking(move || {
            opener(&path).map_err(|message| {
                Error::io(
                    format!("opening {} with the default application", path.display()),
                    io::Error::other(message),
                )
            })
        })
        .await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "opened {src} from {}",
            dest.display()
        );
        Ok(())
    })
    .await
}

/// Like `open_object`, but opens the downloaded copy with `app` (an `.app`
/// bundle on macOS, an executable on Windows and Linux). The application is
/// added to `general.open_with_recent` once it has been started.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_object_with(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    key: String,
    app: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("opening an object with an application", async {
        if app.trim().is_empty() {
            return Err(Error::Usage("choose an application".to_owned()));
        }
        let (src, dest) = download_for_open(&core, &account_id, &bucket, &key).await?;
        let open_with = Arc::clone(&core.open_with);
        let (path, program) = (dest.clone(), app.clone());
        blocking(move || {
            open_with(&path, &program).map_err(|message| {
                Error::io(
                    format!("opening {} with {program}", path.display()),
                    io::Error::other(message),
                )
            })
        })
        .await?;
        let mut settings = core.session.settings().await?;
        settings.general.remember_open_with(&app);
        core.session.save_settings(settings).await?;
        tracing::info!(
            target: "bucketbridge::desktop",
            "opened {src} from {} with {app}",
            dest.display()
        );
        Ok(())
    })
    .await
}

/// Downloads `key` into a new private folder under `data_dir/open` and
/// returns its address and the local copy. Folders are refused.
async fn download_for_open(
    core: &crate::state::Core,
    account_id: &str,
    bucket: &str,
    key: &str,
) -> bucketbridge_core::Result<(S3Uri, PathBuf)> {
    if key.is_empty() || key.ends_with('/') {
        return Err(Error::Usage(format!(
            "'{}' is a folder; only objects can be opened",
            key.escape_debug()
        )));
    }
    let src = S3Uri::new(bucket, key)?;
    let s3 = core.session.s3_for(account_id).await?;
    let settings = core.session.settings().await?;
    let engine = TransferEngine::new(s3, TransferOptions::from_settings(&settings))?
        .with_if_exists(bucketbridge_core::settings::IfExists::Overwrite);

    let root = open::open_root(&core.dirs);
    let name = open::local_name(key);
    let (dir, dest) = blocking(move || {
        let dir = open::new_open_dir(&root)?;
        let dest = safe_local_path(&dir, &name)?;
        Ok((dir, dest))
    })
    .await?;

    let hooks = Hooks::new().with_throttle(Arc::new(LimiterThrottle(core.limiter.clone())));
    let downloaded = engine
        .download_file(&src, &dest, &hooks, None, &|_: &DownloadState| {})
        .await;
    match downloaded {
        Ok(Outcome::Completed(_)) => Ok((src, dest)),
        Ok(Outcome::Paused | Outcome::Cancelled | Outcome::Skipped(_)) => {
            discard(dir).await;
            Err(Error::Cancelled)
        }
        Err(error) => {
            discard(dir).await;
            Err(error)
        }
    }
}

/// The https address of a bucket: `https://<bucket>.s3.<region>.amazonaws.com/`
/// on AWS (path-style when the name has a dot, which the wildcard
/// certificate would not cover), the endpoint with the bucket in the path or
/// in the host name for other providers.
pub(crate) fn bucket_url(
    endpoint: Option<&str>,
    path_style: bool,
    bucket: &str,
    region: Option<&str>,
) -> String {
    let Some(endpoint) = endpoint else {
        let region = region.filter(|r| !r.is_empty()).unwrap_or("us-east-1");
        return if bucket.contains('.') {
            format!("https://s3.{region}.amazonaws.com/{bucket}")
        } else {
            format!("https://{bucket}.s3.{region}.amazonaws.com/")
        };
    };
    let base = endpoint.trim_end_matches('/');
    if path_style {
        return format!("{base}/{bucket}");
    }
    let (scheme, host) = match base.strip_prefix("http://") {
        Some(host) => ("http", host),
        None => ("https", base.strip_prefix("https://").unwrap_or(base)),
    };
    format!("{scheme}://{bucket}.{host}/")
}

/// Opens the bucket's address (see `bucket_url`) in the system browser.
/// `region` is the bucket's region when the list knows it.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_bucket_in_browser(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    region: Option<String>,
) -> CommandResult<String> {
    let core = state.core()?;
    logged("opening a bucket in the browser", async {
        let account = core
            .session
            .list_accounts()
            .await?
            .accounts
            .into_iter()
            .find(|account| account.id == account_id)
            .ok_or_else(|| Error::NotFound(format!("account {account_id}")))?;
        let url = bucket_url(
            account.endpoint.as_deref(),
            account.path_style,
            &bucket,
            region.as_deref(),
        );
        let opener = Arc::clone(&core.url_opener);
        let target = url.clone();
        blocking(move || {
            opener(&target).map_err(|message| {
                Error::io(
                    format!("opening {target} in the browser"),
                    io::Error::other(message),
                )
            })
        })
        .await?;
        Ok(url)
    })
    .await
}

/// Deletes a folder created by `open_object` whose download did not finish.
async fn discard(dir: PathBuf) {
    // Best effort: a leftover folder is removed by the startup cleanup.
    let _ = blocking(move || {
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::bucket_url;

    #[test]
    fn bucket_urls() {
        assert_eq!(
            bucket_url(None, false, "data", Some("eu-west-1")),
            "https://data.s3.eu-west-1.amazonaws.com/"
        );
        assert_eq!(
            bucket_url(None, false, "data", None),
            "https://data.s3.us-east-1.amazonaws.com/"
        );
        assert_eq!(
            bucket_url(None, false, "my.data", Some("eu-west-1")),
            "https://s3.eu-west-1.amazonaws.com/my.data"
        );
        assert_eq!(
            bucket_url(Some("https://minio.example:9000/"), true, "b", None),
            "https://minio.example:9000/b"
        );
        assert_eq!(
            bucket_url(Some("https://objects.example"), false, "b", None),
            "https://b.objects.example/"
        );
        assert_eq!(
            bucket_url(Some("http://localhost:9000"), false, "b", None),
            "http://b.localhost:9000/"
        );
    }
}
