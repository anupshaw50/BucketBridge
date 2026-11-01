import { invoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Backend, BackendEventName, BackendEvents, CommandArgs, Unsubscribe } from "./backend";
import { type BackendError, toBbError } from "./errors";

async function call<T>(command: string, args?: CommandArgs[keyof CommandArgs]): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toBbError(error);
  }
}

export interface TauriBackendOptions {
  /** Called when an event subscription cannot be registered. */
  onListenError?: (error: BackendError) => void;
}

/** Backend that talks to the Rust host through Tauri `invoke` and events. */
export function createTauriBackend(options: TauriBackendOptions = {}): Backend {
  function listen<E extends BackendEventName>(
    event: E,
    handler: (payload: BackendEvents[E]) => void,
  ): Unsubscribe {
    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    tauriListen<BackendEvents[E]>(event, (message) => {
      if (!disposed) {
        handler(message.payload);
      }
    }).then(
      (fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      },
      (error: unknown) => {
        options.onListenError?.(toBbError(error));
      },
    );
    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
        unlisten = null;
      }
    };
  }

  return {
    security_status: () => call("security_status"),
    set_master_password: (args) => call("set_master_password", args),
    unlock: (args) => call("unlock", args),
    lock: () => call("lock"),
    list_accounts: () => call("list_accounts"),
    add_account: (args) => call("add_account", args),
    update_account: (args) => call("update_account", args),
    remove_account: (args) => call("remove_account", args),
    set_default_account: (args) => call("set_default_account", args),
    mfa_login: (args) => call("mfa_login", args),

    list_buckets: (args) => call("list_buckets", args),
    list_objects: (args) => call("list_objects", args),
    create_folder: (args) => call("create_folder", args),
    create_bucket: (args) => call("create_bucket", args),
    delete_bucket: (args) => call("delete_bucket", args),
    presign: (args) => call("presign", args),
    open_object: (args) => call("open_object", args),
    open_object_with: (args) => call("open_object_with", args),
    open_bucket_in_browser: (args) => call("open_bucket_in_browser", args),

    get_bucket_config: (args) => call("get_bucket_config", args),
    get_bucket_policy: (args) => call("get_bucket_policy", args),
    put_bucket_policy: (args) => call("put_bucket_policy", args),
    delete_bucket_policy: (args) => call("delete_bucket_policy", args),
    validate_bucket_policy: (args) => call("validate_bucket_policy", args),
    format_bucket_policy: (args) => call("format_bucket_policy", args),
    get_public_access_block: (args) => call("get_public_access_block", args),
    put_public_access_block: (args) => call("put_public_access_block", args),
    delete_public_access_block: (args) => call("delete_public_access_block", args),
    get_bucket_versioning: (args) => call("get_bucket_versioning", args),
    put_bucket_versioning: (args) => call("put_bucket_versioning", args),
    get_bucket_logging: (args) => call("get_bucket_logging", args),
    put_bucket_logging: (args) => call("put_bucket_logging", args),
    disable_bucket_logging: (args) => call("disable_bucket_logging", args),
    get_bucket_website: (args) => call("get_bucket_website", args),
    put_bucket_website: (args) => call("put_bucket_website", args),
    delete_bucket_website: (args) => call("delete_bucket_website", args),
    get_bucket_acceleration: (args) => call("get_bucket_acceleration", args),
    put_bucket_acceleration: (args) => call("put_bucket_acceleration", args),
    list_external_buckets: (args) => call("list_external_buckets", args),
    add_external_bucket: (args) => call("add_external_bucket", args),
    remove_external_bucket: (args) => call("remove_external_bucket", args),
    list_bookmarks: () => call("list_bookmarks"),
    add_bookmark: (args) => call("add_bookmark", args),
    rename_bookmark: (args) => call("rename_bookmark", args),
    remove_bookmark: (args) => call("remove_bookmark", args),
    open_new_window: (args) => call("open_new_window", args),
    get_bucket_rules: (args) => call("get_bucket_rules", args),
    get_bucket_lifecycle: (args) => call("get_bucket_lifecycle", args),
    put_bucket_lifecycle: (args) => call("put_bucket_lifecycle", args),
    delete_bucket_lifecycle: (args) => call("delete_bucket_lifecycle", args),
    validate_bucket_lifecycle: (args) => call("validate_bucket_lifecycle", args),
    get_bucket_cors: (args) => call("get_bucket_cors", args),
    put_bucket_cors: (args) => call("put_bucket_cors", args),
    delete_bucket_cors: (args) => call("delete_bucket_cors", args),
    validate_bucket_cors: (args) => call("validate_bucket_cors", args),
    get_bucket_replication: (args) => call("get_bucket_replication", args),
    put_bucket_replication: (args) => call("put_bucket_replication", args),
    delete_bucket_replication: (args) => call("delete_bucket_replication", args),
    validate_bucket_replication: (args) => call("validate_bucket_replication", args),
    list_bucket_tiering: (args) => call("list_bucket_tiering", args),
    put_bucket_tiering: (args) => call("put_bucket_tiering", args),
    delete_bucket_tiering: (args) => call("delete_bucket_tiering", args),
    validate_bucket_tiering: (args) => call("validate_bucket_tiering", args),

    enqueue_upload: (args) => call("enqueue_upload", args),
    enqueue_download: (args) => call("enqueue_download", args),
    enqueue_copy: (args) => call("enqueue_copy", args),
    enqueue_move: (args) => call("enqueue_move", args),
    enqueue_delete: (args) => call("enqueue_delete", args),
    rename: (args) => call("rename", args),
    list_tasks: () => call("list_tasks"),
    start_all: () => call("start_all"),
    stop_all: () => call("stop_all"),
    cancel_all: () => call("cancel_all"),
    clear_finished: () => call("clear_finished"),
    pause_task: (args) => call("pause_task", args),
    resume_task: (args) => call("resume_task", args),
    cancel_task: (args) => call("cancel_task", args),
    retry_task: (args) => call("retry_task", args),

    sync_analyze: (args) => call("sync_analyze", args),
    sync_execute: (args) => call("sync_execute", args),
    sync_cancel: (args) => call("sync_cancel", args),
    sync_get: (args) => call("sync_get", args),

    list_cloudfront_distributions: (args) => call("list_cloudfront_distributions", args),
    get_cloudfront_distribution: (args) => call("get_cloudfront_distribution", args),
    find_cloudfront_distributions: (args) => call("find_cloudfront_distributions", args),
    create_cloudfront_distribution: (args) => call("create_cloudfront_distribution", args),
    update_cloudfront_distribution: (args) => call("update_cloudfront_distribution", args),
    set_cloudfront_enabled: (args) => call("set_cloudfront_enabled", args),
    detach_cloudfront_bucket: (args) => call("detach_cloudfront_bucket", args),
    create_cloudfront_invalidation: (args) => call("create_cloudfront_invalidation", args),
    list_cloudfront_invalidations: (args) => call("list_cloudfront_invalidations", args),
    get_cloudfront_invalidation: (args) => call("get_cloudfront_invalidation", args),
    validate_invalidation_paths: (args) => call("validate_invalidation_paths", args),
    list_invalidation_path_sets: () => call("list_invalidation_path_sets"),
    save_invalidation_path_set: (args) => call("save_invalidation_path_set", args),
    delete_invalidation_path_set: (args) => call("delete_invalidation_path_set", args),
    list_versions: (args) => call("list_versions", args),
    restore_version: (args) => call("restore_version", args),
    purge_versions: (args) => call("purge_versions", args),
    enqueue_download_version: (args) => call("enqueue_download_version", args),
    preview_object: (args) => call("preview_object", args),

    get_settings: () => call("get_settings"),
    save_settings: (args) => call("save_settings", args),
    set_proxy_password: (args) => call("set_proxy_password", args),
    shorten_url: (args) => call("shorten_url", args),
    set_shortener_token: (args) => call("set_shortener_token", args),
    get_log: (args) => call("get_log", args),
    export_log: (args) => call("export_log", args),
    clear_log: () => call("clear_log"),
    app_info: () => call("app_info"),
    last_crash_report: () => call("last_crash_report"),
    dismiss_crash_report: (args) => call("dismiss_crash_report", args),

    run_diagnostics: (args) => call("run_diagnostics", args),
    iam_status: (args) => call("iam_status", args),
    iam_list_users: (args) => call("iam_list_users", args),
    iam_create_user: (args) => call("iam_create_user", args),
    iam_update_user: (args) => call("iam_update_user", args),
    iam_delete_user: (args) => call("iam_delete_user", args),
    iam_user_details: (args) => call("iam_user_details", args),
    iam_list_managed_policies: (args) => call("iam_list_managed_policies", args),
    iam_attach_policy: (args) => call("iam_attach_policy", args),
    iam_detach_policy: (args) => call("iam_detach_policy", args),
    iam_get_inline_policy: (args) => call("iam_get_inline_policy", args),
    iam_put_inline_policy: (args) => call("iam_put_inline_policy", args),
    iam_delete_inline_policy: (args) => call("iam_delete_inline_policy", args),
    iam_validate_policy: (args) => call("iam_validate_policy", args),
    iam_format_policy: (args) => call("iam_format_policy", args),
    iam_create_access_key: (args) => call("iam_create_access_key", args),
    iam_list_access_keys: (args) => call("iam_list_access_keys", args),
    iam_set_access_key_active: (args) => call("iam_set_access_key_active", args),
    iam_delete_access_key: (args) => call("iam_delete_access_key", args),
    iam_create_login_profile: (args) => call("iam_create_login_profile", args),
    iam_update_login_profile: (args) => call("iam_update_login_profile", args),
    iam_delete_login_profile: (args) => call("iam_delete_login_profile", args),
    list_multipart_uploads: (args) => call("list_multipart_uploads", args),
    abort_multipart_uploads: (args) => call("abort_multipart_uploads", args),
    list_filters: () => call("list_filters"),
    save_filter: (args) => call("save_filter", args),
    delete_filter: (args) => call("delete_filter", args),
    mount_status: () => call("mount_status"),
    mount_bucket: (args) => call("mount_bucket", args),
    unmount_bucket: (args) => call("unmount_bucket", args),
    run_security_scan: (args) => call("run_security_scan", args),
    export_scan_report: (args) => call("export_scan_report", args),
    export_listing: (args) => call("export_listing", args),
    get_object_details: (args) => call("get_object_details", args),
    update_object_tags: (args) => call("update_object_tags", args),
    set_object_storage_class: (args) => call("set_object_storage_class", args),
    set_object_encryption: (args) => call("set_object_encryption", args),
    update_object_headers: (args) => call("update_object_headers", args),
    set_object_acl: (args) => call("set_object_acl", args),
    get_bucket_object_lock: (args) => call("get_bucket_object_lock", args),
    put_bucket_object_lock: (args) => call("put_bucket_object_lock", args),
    get_object_lock: (args) => call("get_object_lock", args),
    set_object_retention: (args) => call("set_object_retention", args),
    set_object_legal_hold: (args) => call("set_object_legal_hold", args),
    get_object_ownership: (args) => call("get_object_ownership", args),
    put_object_ownership: (args) => call("put_object_ownership", args),
    restore_objects: (args) => call("restore_objects", args),
    get_restore_state: (args) => call("get_restore_state", args),
    cse_probe_object: (args) => call("cse_probe_object", args),
    cse_check_password: (args) => call("cse_check_password", args),
    cse_encrypt_object: (args) => call("cse_encrypt_object", args),
    cse_decrypt_object: (args) => call("cse_decrypt_object", args),
    enqueue_upload_encrypted: (args) => call("enqueue_upload_encrypted", args),
    enqueue_download_decrypted: (args) => call("enqueue_download_decrypted", args),

    listen,
  };
}
