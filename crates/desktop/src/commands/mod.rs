//! The IPC commands of docs/desktop-api.md.
//!
//! Every command is `async` (it runs on the async runtime, never on the UI
//! thread), takes `snake_case` arguments and rejects with [`CommandError`],
//! the contract's `BbError`. Blocking core calls (configuration file, plan
//! store, log export, crash reports) run on the blocking thread pool.

mod app;
mod bookmarks;
mod browse;
mod bucketcfg;
mod bucketrules;
mod cloudfront;
mod credentials;
mod cse;
mod edit;
mod iam;
pub(crate) mod mount;
mod objectlock;
mod objectmeta;
mod scan;
mod security;
mod share;
mod sync;
mod syncjobs;
mod tools;
mod transfer;
mod versions;
mod window;

use std::future::Future;

use bucketbridge_core::{Error, ErrorKind, Result};
use tauri::Runtime;
use tauri::ipc::Invoke;

use crate::error::CommandError;

type CommandResult<T> = std::result::Result<T, CommandError>;

/// The invoke handler with every command.
pub fn handler<R: Runtime>() -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        security::security_status,
        security::set_master_password,
        security::unlock,
        security::lock,
        security::list_accounts,
        security::add_account,
        security::update_account,
        security::remove_account,
        security::set_default_account,
        security::mfa_login,
        browse::list_buckets,
        browse::list_objects,
        browse::create_folder,
        browse::create_bucket,
        browse::delete_bucket,
        browse::presign,
        share::shorten_url,
        share::set_shortener_token,
        browse::open_object,
        browse::open_object_with,
        browse::open_bucket_in_browser,
        bucketcfg::get_bucket_config,
        bucketcfg::get_bucket_policy,
        bucketcfg::put_bucket_policy,
        bucketcfg::delete_bucket_policy,
        bucketcfg::validate_bucket_policy,
        bucketcfg::format_bucket_policy,
        bucketcfg::get_public_access_block,
        bucketcfg::put_public_access_block,
        bucketcfg::delete_public_access_block,
        bucketcfg::get_bucket_versioning,
        bucketcfg::put_bucket_versioning,
        bucketcfg::get_bucket_logging,
        bucketcfg::put_bucket_logging,
        bucketcfg::disable_bucket_logging,
        bucketcfg::get_bucket_website,
        bucketcfg::put_bucket_website,
        bucketcfg::delete_bucket_website,
        bucketcfg::get_bucket_acceleration,
        bucketcfg::put_bucket_acceleration,
        bucketcfg::list_external_buckets,
        bucketcfg::add_external_bucket,
        bucketcfg::remove_external_bucket,
        bucketrules::get_bucket_rules,
        bucketrules::get_bucket_lifecycle,
        bucketrules::put_bucket_lifecycle,
        bucketrules::delete_bucket_lifecycle,
        bucketrules::validate_bucket_lifecycle,
        bucketrules::list_bucket_tiering,
        bucketrules::put_bucket_tiering,
        bucketrules::delete_bucket_tiering,
        bucketrules::validate_bucket_tiering,
        bucketrules::get_bucket_cors,
        bucketrules::put_bucket_cors,
        bucketrules::delete_bucket_cors,
        bucketrules::validate_bucket_cors,
        bucketrules::get_bucket_replication,
        bucketrules::put_bucket_replication,
        bucketrules::delete_bucket_replication,
        bucketrules::validate_bucket_replication,
        cloudfront::list_cloudfront_distributions,
        cloudfront::get_cloudfront_distribution,
        cloudfront::find_cloudfront_distributions,
        cloudfront::create_cloudfront_distribution,
        cloudfront::update_cloudfront_distribution,
        cloudfront::set_cloudfront_enabled,
        cloudfront::detach_cloudfront_bucket,
        cloudfront::create_cloudfront_invalidation,
        cloudfront::list_cloudfront_invalidations,
        cloudfront::get_cloudfront_invalidation,
        cloudfront::validate_invalidation_paths,
        cloudfront::list_invalidation_path_sets,
        cloudfront::save_invalidation_path_set,
        cloudfront::delete_invalidation_path_set,
        transfer::enqueue_upload,
        transfer::enqueue_download,
        transfer::enqueue_copy,
        transfer::enqueue_move,
        transfer::enqueue_delete,
        transfer::rename,
        transfer::list_tasks,
        transfer::start_all,
        transfer::stop_all,
        transfer::cancel_all,
        transfer::clear_finished,
        transfer::list_conflicts,
        transfer::answer_conflict,
        transfer::pause_task,
        transfer::resume_task,
        transfer::cancel_task,
        transfer::retry_task,
        sync::sync_analyze,
        sync::sync_execute,
        sync::sync_cancel,
        sync::sync_get,
        syncjobs::list_sync_jobs,
        syncjobs::save_sync_job,
        syncjobs::delete_sync_job,
        syncjobs::run_sync_job,
        versions::list_versions,
        versions::restore_version,
        versions::purge_versions,
        versions::enqueue_download_version,
        versions::preview_object,
        bookmarks::list_bookmarks,
        bookmarks::add_bookmark,
        bookmarks::rename_bookmark,
        bookmarks::remove_bookmark,
        window::open_new_window,
        app::get_settings,
        app::save_settings,
        app::set_proxy_password,
        app::get_log,
        app::export_log,
        app::clear_log,
        app::app_info,
        app::last_crash_report,
        app::dismiss_crash_report,
        tools::run_diagnostics,
        tools::list_multipart_uploads,
        tools::abort_multipart_uploads,
        tools::list_filters,
        tools::save_filter,
        tools::delete_filter,
        tools::export_listing,
        scan::run_security_scan,
        scan::export_scan_report,
        objectmeta::get_object_details,
        objectmeta::update_object_tags,
        objectmeta::set_object_storage_class,
        objectmeta::set_object_encryption,
        objectmeta::update_object_headers,
        objectmeta::set_object_acl,
        objectlock::get_bucket_object_lock,
        objectlock::put_bucket_object_lock,
        objectlock::get_object_lock,
        objectlock::set_object_retention,
        objectlock::set_object_legal_hold,
        objectlock::get_object_ownership,
        objectlock::put_object_ownership,
        objectlock::restore_objects,
        objectlock::get_restore_state,
        objectlock::put_bucket_versioning_mfa,
        objectlock::purge_versions_with,
        iam::iam_status,
        iam::iam_list_users,
        iam::iam_create_user,
        iam::iam_update_user,
        iam::iam_delete_user,
        iam::iam_user_details,
        iam::iam_list_managed_policies,
        iam::iam_attach_policy,
        iam::iam_detach_policy,
        iam::iam_get_inline_policy,
        iam::iam_put_inline_policy,
        iam::iam_delete_inline_policy,
        iam::iam_validate_policy,
        iam::iam_format_policy,
        iam::iam_create_access_key,
        iam::iam_list_access_keys,
        iam::iam_set_access_key_active,
        iam::iam_delete_access_key,
        iam::iam_create_login_profile,
        iam::iam_update_login_profile,
        iam::iam_delete_login_profile,
        cse::cse_probe_object,
        cse::cse_check_password,
        cse::cse_encrypt_object,
        cse::cse_decrypt_object,
        cse::enqueue_upload_encrypted,
        cse::enqueue_download_decrypted,
        mount::mount_status,
        mount::mount_bucket,
        mount::unmount_bucket,
        edit::start_edit,
        edit::list_edits,
        edit::stop_edit,
        edit::resolve_edit_conflict,
        credentials::list_aws_profiles,
        credentials::import_aws_profiles,
        credentials::probe_instance_role,
        credentials::sso_start_login,
        credentials::sso_poll_login,
        credentials::sso_cancel_login,
        credentials::sso_status,
        credentials::sso_sign_out,
        credentials::sso_list_accounts,
        credentials::sso_list_roles,
    ]
}

/// Awaits a command's work and turns its error into the IPC error.
///
/// Failures that point at something broken (`remote`, `io`, `internal`) are
/// recorded in the event log as errors and authentication failures as
/// warnings, so the log panel explains what went wrong. Usage errors,
/// missing items, conflicts, locked secrets, missing MFA sessions and
/// cancellations are ordinary answers and are not recorded.
async fn logged<T>(operation: &str, work: impl Future<Output = Result<T>>) -> CommandResult<T> {
    work.await.map_err(|error| {
        match error.kind() {
            ErrorKind::Remote | ErrorKind::Io | ErrorKind::Internal => {
                tracing::error!(target: "bucketbridge::desktop", "{operation} failed: {error}");
            }
            ErrorKind::Auth => {
                tracing::warn!(target: "bucketbridge::desktop", "{operation} failed: {error}");
            }
            _ => {}
        }
        CommandError::from(error)
    })
}

/// Runs blocking work on the blocking thread pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))?
}

/// Rejects paths that are not absolute: the UI passes paths from the native
/// file dialogs, and a relative path would depend on the app's working
/// directory.
fn require_absolute(what: &str, path: &str) -> Result<()> {
    if std::path::Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{what} must be an absolute path (got '{}')",
            path.escape_debug()
        )))
    }
}
