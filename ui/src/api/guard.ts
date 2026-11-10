import type { Backend } from "./backend";
import { toBbError } from "./errors";

/** UI hooks the session guard uses to recover from `locked` and `mfa_required`. */
export interface SessionPrompts {
  /** Shows the unlock dialog. Resolves `true` once secrets are unlocked, `false` if dismissed. */
  requestUnlock(): Promise<boolean>;
  /** Shows the MFA code dialog. Resolves `true` after a successful `mfa_login`, `false` if dismissed. */
  requestMfa(accountId: string): Promise<boolean>;
}

/** Upper bound on prompts per call, so a misbehaving backend cannot loop forever. */
export const MAX_PROMPTS_PER_CALL = 3;

/**
 * Runs `operation`; when it rejects with `locked` or `mfa_required`, asks the
 * user to unlock or to enter an MFA code and then retries the same call.
 * Any other failure (and a dismissed prompt) rejects with a normalised error.
 */
export async function runGuarded<T>(
  operation: () => Promise<T>,
  accountId: string | null,
  prompts: SessionPrompts,
): Promise<T> {
  let prompted = 0;
  for (;;) {
    try {
      return await operation();
    } catch (raw) {
      const error = toBbError(raw);
      if (prompted < MAX_PROMPTS_PER_CALL) {
        if (error.code === "locked") {
          prompted += 1;
          if (await prompts.requestUnlock()) {
            continue;
          }
        } else if (error.code === "mfa_required" && accountId !== null) {
          prompted += 1;
          if (await prompts.requestMfa(accountId)) {
            continue;
          }
        }
      }
      throw error;
    }
  }
}

/**
 * Wraps a backend so that commands transparently recover from `locked` and
 * `mfa_required`. Security commands (status, unlock, lock, master password)
 * are passed through untouched; they are what the prompts themselves use.
 */
export function withSessionGuard(base: Backend, prompts: SessionPrompts): Backend {
  const guard = <T>(operation: () => Promise<T>, accountId: string | null = null) =>
    runGuarded(operation, accountId, prompts);

  return {
    security_status: () => base.security_status(),
    set_master_password: (args) => base.set_master_password(args),
    unlock: (args) => base.unlock(args),
    lock: () => base.lock(),
    list_accounts: () => guard(() => base.list_accounts()),
    add_account: (args) => guard(() => base.add_account(args)),
    update_account: (args) => guard(() => base.update_account(args)),
    remove_account: (args) => guard(() => base.remove_account(args)),
    set_default_account: (args) => guard(() => base.set_default_account(args)),
    set_account_layout: (args) => guard(() => base.set_account_layout(args)),
    export_accounts: (args) => guard(() => base.export_accounts(args)),
    import_accounts: (args) => guard(() => base.import_accounts(args)),
    // An MFA prompt for mfa_login itself would recurse, so only `locked` is recovered here.
    mfa_login: (args) => guard(() => base.mfa_login(args)),

    list_buckets: (args) => guard(() => base.list_buckets(args), args.account_id),
    list_objects: (args) => guard(() => base.list_objects(args), args.account_id),
    create_folder: (args) => guard(() => base.create_folder(args), args.account_id),
    create_bucket: (args) => guard(() => base.create_bucket(args), args.account_id),
    preview_bucket_clone: (args) => guard(() => base.preview_bucket_clone(args), args.account_id),
    clone_bucket: (args) => guard(() => base.clone_bucket(args), args.account_id),
    delete_bucket: (args) => guard(() => base.delete_bucket(args), args.account_id),
    presign: (args) => guard(() => base.presign(args), args.account_id),
    open_object: (args) => guard(() => base.open_object(args), args.account_id),
    open_object_with: (args) => guard(() => base.open_object_with(args), args.account_id),
    open_bucket_in_browser: (args) => guard(() => base.open_bucket_in_browser(args), args.account_id),

    get_bucket_config: (args) => guard(() => base.get_bucket_config(args), args.account_id),
    get_bucket_policy: (args) => guard(() => base.get_bucket_policy(args), args.account_id),
    put_bucket_policy: (args) => guard(() => base.put_bucket_policy(args), args.account_id),
    delete_bucket_policy: (args) => guard(() => base.delete_bucket_policy(args), args.account_id),
    // Pure text commands: no account, nothing to unlock.
    validate_bucket_policy: (args) => guard(() => base.validate_bucket_policy(args)),
    format_bucket_policy: (args) => guard(() => base.format_bucket_policy(args)),
    get_public_access_block: (args) => guard(() => base.get_public_access_block(args), args.account_id),
    put_public_access_block: (args) => guard(() => base.put_public_access_block(args), args.account_id),
    delete_public_access_block: (args) => guard(() => base.delete_public_access_block(args), args.account_id),
    get_bucket_versioning: (args) => guard(() => base.get_bucket_versioning(args), args.account_id),
    put_bucket_versioning: (args) => guard(() => base.put_bucket_versioning(args), args.account_id),
    get_bucket_logging: (args) => guard(() => base.get_bucket_logging(args), args.account_id),
    put_bucket_logging: (args) => guard(() => base.put_bucket_logging(args), args.account_id),
    disable_bucket_logging: (args) => guard(() => base.disable_bucket_logging(args), args.account_id),
    get_bucket_website: (args) => guard(() => base.get_bucket_website(args), args.account_id),
    put_bucket_website: (args) => guard(() => base.put_bucket_website(args), args.account_id),
    delete_bucket_website: (args) => guard(() => base.delete_bucket_website(args), args.account_id),
    get_bucket_acceleration: (args) => guard(() => base.get_bucket_acceleration(args), args.account_id),
    put_bucket_acceleration: (args) => guard(() => base.put_bucket_acceleration(args), args.account_id),
    list_external_buckets: (args) => guard(() => base.list_external_buckets(args), args.account_id),
    add_external_bucket: (args) => guard(() => base.add_external_bucket(args), args.account_id),
    remove_external_bucket: (args) => guard(() => base.remove_external_bucket(args), args.account_id),
    put_object_acl_grants: (args) => guard(() => base.put_object_acl_grants(args), args.account_id),
    put_bucket_acl_grants: (args) => guard(() => base.put_bucket_acl_grants(args), args.account_id),
    set_bucket_acl: (args) => guard(() => base.set_bucket_acl(args), args.account_id),
    get_bucket_acl: (args) => guard(() => base.get_bucket_acl(args), args.account_id),
    put_bucket_tags: (args) => guard(() => base.put_bucket_tags(args), args.account_id),
    get_bucket_tags: (args) => guard(() => base.get_bucket_tags(args), args.account_id),
    delete_bucket_encryption: (args) => guard(() => base.delete_bucket_encryption(args), args.account_id),
    put_bucket_encryption: (args) => guard(() => base.put_bucket_encryption(args), args.account_id),
    get_bucket_encryption: (args) => guard(() => base.get_bucket_encryption(args), args.account_id),
    // Bookmarks and windows only touch the configuration file, never the secrets.
    list_bookmarks: () => base.list_bookmarks(),
    add_bookmark: (args) => base.add_bookmark(args),
    rename_bookmark: (args) => base.rename_bookmark(args),
    remove_bookmark: (args) => base.remove_bookmark(args),
    open_new_window: (args) => base.open_new_window(args),
    relay_drag: (args) => base.relay_drag(args),
    get_bucket_rules: (args) => guard(() => base.get_bucket_rules(args), args.account_id),
    get_bucket_lifecycle: (args) => guard(() => base.get_bucket_lifecycle(args), args.account_id),
    put_bucket_lifecycle: (args) => guard(() => base.put_bucket_lifecycle(args), args.account_id),
    delete_bucket_lifecycle: (args) => guard(() => base.delete_bucket_lifecycle(args), args.account_id),
    validate_bucket_lifecycle: (args) => base.validate_bucket_lifecycle(args),
    read_rules_file: (args) => base.read_rules_file(args),
    write_rules_file: (args) => base.write_rules_file(args),
    get_bucket_cors: (args) => guard(() => base.get_bucket_cors(args), args.account_id),
    put_bucket_cors: (args) => guard(() => base.put_bucket_cors(args), args.account_id),
    delete_bucket_cors: (args) => guard(() => base.delete_bucket_cors(args), args.account_id),
    validate_bucket_cors: (args) => base.validate_bucket_cors(args),
    get_bucket_replication: (args) => guard(() => base.get_bucket_replication(args), args.account_id),
    put_bucket_replication: (args) => guard(() => base.put_bucket_replication(args), args.account_id),
    delete_bucket_replication: (args) => guard(() => base.delete_bucket_replication(args), args.account_id),
    validate_bucket_replication: (args) => base.validate_bucket_replication(args),
    list_bucket_tiering: (args) => guard(() => base.list_bucket_tiering(args), args.account_id),
    put_bucket_tiering: (args) => guard(() => base.put_bucket_tiering(args), args.account_id),
    delete_bucket_tiering: (args) => guard(() => base.delete_bucket_tiering(args), args.account_id),
    validate_bucket_tiering: (args) => base.validate_bucket_tiering(args),

    enqueue_upload: (args) => guard(() => base.enqueue_upload(args), args.account_id),
    enqueue_download: (args) => guard(() => base.enqueue_download(args), args.account_id),
    enqueue_copy: (args) => guard(() => base.enqueue_copy(args), args.account_id),
    enqueue_move: (args) => guard(() => base.enqueue_move(args), args.account_id),
    enqueue_delete: (args) => guard(() => base.enqueue_delete(args), args.account_id),
    rename: (args) => guard(() => base.rename(args), args.account_id),
    list_tasks: () => guard(() => base.list_tasks()),
    list_conflicts: () => base.list_conflicts(),
    answer_conflict: (args) => base.answer_conflict(args),
    start_all: () => guard(() => base.start_all()),
    stop_all: () => guard(() => base.stop_all()),
    cancel_all: () => guard(() => base.cancel_all()),
    clear_finished: () => guard(() => base.clear_finished()),
    pause_task: (args) => guard(() => base.pause_task(args)),
    resume_task: (args) => guard(() => base.resume_task(args)),
    cancel_task: (args) => guard(() => base.cancel_task(args)),
    retry_task: (args) => guard(() => base.retry_task(args)),

    sync_analyze: (args) => guard(() => base.sync_analyze(args), args.account_id),
    sync_execute: (args) => guard(() => base.sync_execute(args)),
    sync_cancel: (args) => guard(() => base.sync_cancel(args)),
    sync_get: (args) => guard(() => base.sync_get(args)),
    // Saved jobs live in the configuration file; running one needs the account's secrets.
    list_sync_jobs: () => base.list_sync_jobs(),
    save_sync_job: (args) => base.save_sync_job(args),
    delete_sync_job: (args) => base.delete_sync_job(args),
    run_sync_job: (args) => guard(() => base.run_sync_job(args)),

    list_cloudfront_distributions: (args) => guard(() => base.list_cloudfront_distributions(args), args.account_id),
    get_cloudfront_distribution: (args) => guard(() => base.get_cloudfront_distribution(args), args.account_id),
    find_cloudfront_distributions: (args) => guard(() => base.find_cloudfront_distributions(args), args.account_id),
    create_cloudfront_distribution: (args) => guard(() => base.create_cloudfront_distribution(args), args.account_id),
    update_cloudfront_distribution: (args) => guard(() => base.update_cloudfront_distribution(args), args.account_id),
    set_cloudfront_enabled: (args) => guard(() => base.set_cloudfront_enabled(args), args.account_id),
    detach_cloudfront_bucket: (args) => guard(() => base.detach_cloudfront_bucket(args), args.account_id),
    create_cloudfront_invalidation: (args) => guard(() => base.create_cloudfront_invalidation(args), args.account_id),
    list_cloudfront_invalidations: (args) => guard(() => base.list_cloudfront_invalidations(args), args.account_id),
    get_cloudfront_invalidation: (args) => guard(() => base.get_cloudfront_invalidation(args), args.account_id),
    validate_invalidation_paths: (args) => guard(() => base.validate_invalidation_paths(args)),
    list_invalidation_path_sets: () => guard(() => base.list_invalidation_path_sets()),
    save_invalidation_path_set: (args) => guard(() => base.save_invalidation_path_set(args)),
    delete_invalidation_path_set: (args) => guard(() => base.delete_invalidation_path_set(args)),
    list_versions: (args) => guard(() => base.list_versions(args), args.account_id),
    restore_version: (args) => guard(() => base.restore_version(args), args.account_id),
    purge_versions: (args) => guard(() => base.purge_versions(args), args.account_id),
    enqueue_download_version: (args) => guard(() => base.enqueue_download_version(args), args.account_id),
    preview_object: (args) => guard(() => base.preview_object(args), args.account_id),

    get_settings: () => guard(() => base.get_settings()),
    save_settings: (args) => guard(() => base.save_settings(args)),
    set_proxy_password: (args) => guard(() => base.set_proxy_password(args)),
    shorten_url: (args) => guard(() => base.shorten_url(args)),
    set_shortener_token: (args) => guard(() => base.set_shortener_token(args)),
    get_log: (args) => guard(() => base.get_log(args)),
    export_log: (args) => guard(() => base.export_log(args)),
    clear_log: () => guard(() => base.clear_log()),
    app_info: () => guard(() => base.app_info()),
    list_regions: () => base.list_regions(),
    last_crash_report: () => guard(() => base.last_crash_report()),
    dismiss_crash_report: (args) => guard(() => base.dismiss_crash_report(args)),
    // Links and quitting need no secrets.
    take_pending_link: () => base.take_pending_link(),
    quit_app: (args) => base.quit_app(args),

    // Diagnostics of an unsaved account (account_id null) never needs the session.
    run_diagnostics: (args) => guard(() => base.run_diagnostics(args), args.account_id),
    iam_status: (args) => guard(() => base.iam_status(args), args.account_id),
    iam_list_users: (args) => guard(() => base.iam_list_users(args), args.account_id),
    iam_create_user: (args) => guard(() => base.iam_create_user(args), args.account_id),
    iam_update_user: (args) => guard(() => base.iam_update_user(args), args.account_id),
    iam_delete_user: (args) => guard(() => base.iam_delete_user(args), args.account_id),
    iam_user_details: (args) => guard(() => base.iam_user_details(args), args.account_id),
    iam_list_managed_policies: (args) => guard(() => base.iam_list_managed_policies(args), args.account_id),
    iam_attach_policy: (args) => guard(() => base.iam_attach_policy(args), args.account_id),
    iam_detach_policy: (args) => guard(() => base.iam_detach_policy(args), args.account_id),
    iam_get_inline_policy: (args) => guard(() => base.iam_get_inline_policy(args), args.account_id),
    iam_put_inline_policy: (args) => guard(() => base.iam_put_inline_policy(args), args.account_id),
    iam_delete_inline_policy: (args) => guard(() => base.iam_delete_inline_policy(args), args.account_id),
    iam_validate_policy: (args) => guard(() => base.iam_validate_policy(args)),
    iam_format_policy: (args) => guard(() => base.iam_format_policy(args)),
    iam_create_access_key: (args) => guard(() => base.iam_create_access_key(args), args.account_id),
    iam_list_access_keys: (args) => guard(() => base.iam_list_access_keys(args), args.account_id),
    iam_set_access_key_active: (args) => guard(() => base.iam_set_access_key_active(args), args.account_id),
    iam_delete_access_key: (args) => guard(() => base.iam_delete_access_key(args), args.account_id),
    iam_create_login_profile: (args) => guard(() => base.iam_create_login_profile(args), args.account_id),
    iam_update_login_profile: (args) => guard(() => base.iam_update_login_profile(args), args.account_id),
    iam_delete_login_profile: (args) => guard(() => base.iam_delete_login_profile(args), args.account_id),
    compute_bucket_usage: (args) => guard(() => base.compute_bucket_usage(args), args.account_id),
    cancel_bucket_usage: (args) => base.cancel_bucket_usage(args),
    list_multipart_uploads: (args) => guard(() => base.list_multipart_uploads(args), args.account_id),
    abort_multipart_uploads: (args) => guard(() => base.abort_multipart_uploads(args), args.account_id),
    check_multipart_resume: (args) => guard(() => base.check_multipart_resume(args), args.account_id),
    resume_multipart_upload: (args) => guard(() => base.resume_multipart_upload(args), args.account_id),
    list_filters: () => guard(() => base.list_filters()),
    save_filter: (args) => guard(() => base.save_filter(args)),
    delete_filter: (args) => guard(() => base.delete_filter(args)),
    test_upload_rules: (args) => base.test_upload_rules(args),
    mount_status: () => guard(() => base.mount_status()),
    // Credential sources: profile and SSO commands read files and call AWS, never the vault.
    list_aws_profiles: () => base.list_aws_profiles(),
    import_aws_profiles: (args) => guard(() => base.import_aws_profiles(args)),
    probe_instance_role: () => base.probe_instance_role(),
    sso_start_login: (args) => base.sso_start_login(args),
    sso_poll_login: (args) => base.sso_poll_login(args),
    sso_cancel_login: (args) => base.sso_cancel_login(args),
    sso_status: (args) => base.sso_status(args),
    sso_sign_out: (args) => base.sso_sign_out(args),
    sso_list_accounts: (args) => base.sso_list_accounts(args),
    sso_list_roles: (args) => base.sso_list_roles(args),
    mount_bucket: (args) => guard(() => base.mount_bucket(args), args.account_id),
    unmount_bucket: (args) => guard(() => base.unmount_bucket(args)),
    start_edit: (args) => guard(() => base.start_edit(args), args.account_id),
    list_edits: () => guard(() => base.list_edits()),
    stop_edit: (args) => guard(() => base.stop_edit(args)),
    resolve_edit_conflict: (args) => guard(() => base.resolve_edit_conflict(args)),
    run_security_scan: (args) => guard(() => base.run_security_scan(args), args.account_id),
    export_scan_report: (args) => guard(() => base.export_scan_report(args)),
    export_listing: (args) => guard(() => base.export_listing(args), args.account_id),
    get_object_details: (args) => guard(() => base.get_object_details(args), args.account_id),
    update_object_tags: (args) => guard(() => base.update_object_tags(args), args.account_id),
    set_object_storage_class: (args) => guard(() => base.set_object_storage_class(args), args.account_id),
    set_object_encryption: (args) => guard(() => base.set_object_encryption(args), args.account_id),
    update_object_headers: (args) => guard(() => base.update_object_headers(args), args.account_id),
    set_object_acl: (args) => guard(() => base.set_object_acl(args), args.account_id),
    get_bucket_object_lock: (args) => guard(() => base.get_bucket_object_lock(args), args.account_id),
    put_bucket_object_lock: (args) => guard(() => base.put_bucket_object_lock(args), args.account_id),
    get_object_lock: (args) => guard(() => base.get_object_lock(args), args.account_id),
    set_object_retention: (args) => guard(() => base.set_object_retention(args), args.account_id),
    set_object_legal_hold: (args) => guard(() => base.set_object_legal_hold(args), args.account_id),
    get_object_ownership: (args) => guard(() => base.get_object_ownership(args), args.account_id),
    put_object_ownership: (args) => guard(() => base.put_object_ownership(args), args.account_id),
    restore_objects: (args) => guard(() => base.restore_objects(args), args.account_id),
    get_restore_state: (args) => guard(() => base.get_restore_state(args), args.account_id),
    estimate_restore: (args) => guard(() => base.estimate_restore(args), args.account_id),
    cse_probe_object: (args) => guard(() => base.cse_probe_object(args), args.account_id),
    cse_check_password: (args) => guard(() => base.cse_check_password(args), args.account_id),
    cse_encrypt_object: (args) => guard(() => base.cse_encrypt_object(args), args.account_id),
    cse_decrypt_object: (args) => guard(() => base.cse_decrypt_object(args), args.account_id),
    enqueue_upload_encrypted: (args) => guard(() => base.enqueue_upload_encrypted(args), args.account_id),
    enqueue_download_decrypted: (args) => guard(() => base.enqueue_download_decrypted(args), args.account_id),

    listen: (event, handler) => base.listen(event, handler),
  };
}
