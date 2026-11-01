import type {
  CloudFrontDistribution,
  CloudFrontInvalidation,
  CloudFrontPage,
  CreatedDistribution,
  DistributionEdit,
  InvalidationPage,
  NewDistribution,
  PathSet,
  ArchiveState,
  BucketObjectLock,
  DefaultRetentionRule,
  ObjectLockState,
  ObjectOwnership,
  RestoreTier,
  RetentionChange,
  AccelerationStatus,
  AbortResult,
  Account,
  AccountInput,
  AccountList,
  AccountUpdate,
  AppInfo,
  Bucket,
  BucketConfig,
  BucketRules,
  BucketVersioning,
  CorsConfig,
  CrashReport,
  LifecycleConfig,
  ReplicationConfig,
  RuleIssue,
  Bookmark,
  WindowLocation,
  TieringConfig,
  IamAccessKey,
  IamDeleteOutcome,
  IamInlinePolicy,
  IamManagedPolicy,
  IamNewAccessKey,
  IamPolicyScope,
  IamStatus,
  IamUser,
  IamUserDetails,
  LoggingTarget,
  DiagnosticsReport,
  ExportOutcome,
  FilterSpec,
  ListingFormat,
  DeleteOutcome,
  CannedAcl,
  EncryptionChange,
  HeaderChanges,
  LogEntry,
  LogExportFormat,
  LogLevel,
  ObjectChange,
  CseCompression,
  ObjectCse,
  ObjectDetails,
  TransformOutcome,
  ObjectPage,
  PolicyIssue,
  PresignMethod,
  PublicAccessBlock,
  SavedFilter,
  ObjectPreview,
  RestoreOutcome,
  SecurityStatus,
  Settings,
  SyncDirection,
  SyncPlan,
  SyncProgress,
  StorageClass,
  SyncRowEvent,
  TagEdit,
  TransferRemovedEvent,
  TransferTask,
  VersioningStatus,
  WebsiteConfig,
  UploadListing,
  MountChangedEvent,
  MountInfo,
  MountStatus,
  ScanReport,
  ScanReportFormat,
  UploadRef,
  VersionFilter,
  VersionGroupPage,
  VersionRef,
} from "./types";

/**
 * Argument objects of every command that takes arguments, keyed by command
 * name. Keys are snake_case exactly as `invoke()` expects them, and optional
 * values are `null`, never omitted (see docs/desktop-api.md, Conventions).
 */
export interface CommandArgs {
  set_master_password: { current: string | null; new_password: string | null };
  unlock: { password: string };
  add_account: { input: AccountInput };
  update_account: { id: string; update: AccountUpdate };
  remove_account: { id: string };
  set_default_account: { id: string };
  mfa_login: { account_id: string; code: string };

  list_buckets: { account_id: string };
  list_objects: {
    account_id: string;
    bucket: string;
    prefix: string;
    flat: boolean;
    token: string | null;
  };
  create_folder: { account_id: string; bucket: string; key: string };
  create_bucket: { account_id: string; name: string; region: string | null };
  delete_bucket: { account_id: string; name: string };
  presign: {
    account_id: string;
    bucket: string;
    key: string;
    method: PresignMethod;
    expires_secs: number;
  };
  open_object: { account_id: string; bucket: string; key: string };
  open_object_with: { account_id: string; bucket: string; key: string; app: string };
  open_bucket_in_browser: { account_id: string; bucket: string; region: string | null };

  get_bucket_config: { account_id: string; bucket: string };
  get_bucket_policy: { account_id: string; bucket: string };
  put_bucket_policy: { account_id: string; bucket: string; policy: string };
  delete_bucket_policy: { account_id: string; bucket: string };
  validate_bucket_policy: { policy: string };
  format_bucket_policy: { policy: string };
  get_public_access_block: { account_id: string; bucket: string };
  put_public_access_block: { account_id: string; bucket: string; config: PublicAccessBlock };
  delete_public_access_block: { account_id: string; bucket: string };
  get_bucket_versioning: { account_id: string; bucket: string };
  put_bucket_versioning: { account_id: string; bucket: string; status: VersioningStatus };
  get_bucket_logging: { account_id: string; bucket: string };
  put_bucket_logging: { account_id: string; bucket: string; target: LoggingTarget };
  disable_bucket_logging: { account_id: string; bucket: string };
  get_bucket_website: { account_id: string; bucket: string };
  put_bucket_website: { account_id: string; bucket: string; website: WebsiteConfig };
  delete_bucket_website: { account_id: string; bucket: string };
  get_bucket_acceleration: { account_id: string; bucket: string };
  put_bucket_acceleration: { account_id: string; bucket: string; status: AccelerationStatus };
  list_external_buckets: { account_id: string };
  add_external_bucket: { account_id: string; name: string };
  remove_external_bucket: { account_id: string; name: string };
  add_bookmark: { account_id: string; bucket: string; prefix: string; label: string | null };
  rename_bookmark: { id: string; label: string };
  remove_bookmark: { id: string };
  open_new_window: { location: WindowLocation | null };
  get_bucket_rules: { account_id: string; bucket: string };
  get_bucket_lifecycle: { account_id: string; bucket: string };
  put_bucket_lifecycle: { account_id: string; bucket: string; config: LifecycleConfig };
  delete_bucket_lifecycle: { account_id: string; bucket: string };
  validate_bucket_lifecycle: { document: string };
  get_bucket_cors: { account_id: string; bucket: string };
  put_bucket_cors: { account_id: string; bucket: string; config: CorsConfig };
  delete_bucket_cors: { account_id: string; bucket: string };
  validate_bucket_cors: { document: string };
  get_bucket_replication: { account_id: string; bucket: string };
  put_bucket_replication: { account_id: string; bucket: string; config: ReplicationConfig };
  delete_bucket_replication: { account_id: string; bucket: string };
  validate_bucket_replication: { document: string };
  list_bucket_tiering: { account_id: string; bucket: string };
  put_bucket_tiering: { account_id: string; bucket: string; config: TieringConfig };
  delete_bucket_tiering: { account_id: string; bucket: string; id: string };
  validate_bucket_tiering: { document: string };

  enqueue_upload: { account_id: string; local_paths: string[]; bucket: string; prefix: string };
  enqueue_download: { account_id: string; bucket: string; keys: string[]; dest_dir: string };
  enqueue_copy: {
    account_id: string;
    src_bucket: string;
    keys: string[];
    dst_bucket: string;
    dst_prefix: string;
  };
  enqueue_move: {
    account_id: string;
    src_bucket: string;
    keys: string[];
    dst_bucket: string;
    dst_prefix: string;
  };
  enqueue_delete: { account_id: string; bucket: string; keys: string[] };
  rename: { account_id: string; bucket: string; from_key: string; to_key: string };
  pause_task: { id: string };
  resume_task: { id: string };
  cancel_task: { id: string };
  retry_task: { id: string };

  sync_analyze: {
    account_id: string;
    local_root: string;
    s3_prefix: string;
    direction: SyncDirection;
    delete: boolean;
  };
  sync_execute: { plan_id: string };
  sync_cancel: { plan_id: string };
  sync_get: { plan_id: string };

  list_cloudfront_distributions: { account_id: string; marker: string | null };
  get_cloudfront_distribution: { account_id: string; id: string };
  find_cloudfront_distributions: { account_id: string; bucket: string };
  create_cloudfront_distribution: { account_id: string; new: NewDistribution };
  update_cloudfront_distribution: { account_id: string; id: string; edit: DistributionEdit; expected_etag: string | null };
  set_cloudfront_enabled: { account_id: string; id: string; enabled: boolean; expected_etag: string | null };
  detach_cloudfront_bucket: { account_id: string; id: string; bucket: string; expected_etag: string | null };
  create_cloudfront_invalidation: { account_id: string; distribution_id: string; paths: string[] };
  list_cloudfront_invalidations: { account_id: string; distribution_id: string; marker: string | null };
  get_cloudfront_invalidation: { account_id: string; distribution_id: string; invalidation_id: string };
  validate_invalidation_paths: { paths: string[] };
  save_invalidation_path_set: { set: PathSet; replaces: string | null };
  delete_invalidation_path_set: { name: string };

  list_versions: {
    account_id: string;
    bucket: string;
    prefix: string;
    exact: boolean;
    filter: VersionFilter;
    token: string | null;
  };
  restore_version: { account_id: string; bucket: string; key: string; version_id: string };
  purge_versions: { account_id: string; bucket: string; versions: VersionRef[] };
  enqueue_download_version: {
    account_id: string;
    bucket: string;
    key: string;
    version_id: string;
    dest_path: string;
  };
  preview_object: {
    account_id: string;
    bucket: string;
    key: string;
    version_id: string | null;
    page: number;
    page_size: number | null;
  };

  save_settings: { settings: Settings };
  set_proxy_password: { password: string | null };
  shorten_url: { url: string };
  set_shortener_token: { token: string | null };
  get_log: { levels: LogLevel[] | null; text: string | null; limit: number | null };
  export_log: { path: string; format: LogExportFormat };
  dismiss_crash_report: { path: string };

  // Tools and export (T12). list_filters takes no arguments.
  run_diagnostics: { account_id: string | null; input: AccountInput | null; bucket: string | null };

  // IAM (T02)
  iam_status: { account_id: string };
  iam_list_users: { account_id: string };
  iam_create_user: { account_id: string; name: string; path: string | null };
  iam_update_user: { account_id: string; name: string; new_name: string | null; new_path: string | null };
  iam_delete_user: { account_id: string; name: string; confirm: string };
  iam_user_details: { account_id: string; name: string };
  iam_list_managed_policies: { account_id: string; scope: IamPolicyScope };
  iam_attach_policy: { account_id: string; user: string; policy_arn: string };
  iam_detach_policy: { account_id: string; user: string; policy_arn: string };
  iam_get_inline_policy: { account_id: string; user: string; name: string };
  iam_put_inline_policy: { account_id: string; user: string; name: string; document: string };
  iam_delete_inline_policy: { account_id: string; user: string; name: string; confirm: string };
  iam_validate_policy: { document: string };
  iam_format_policy: { document: string };
  iam_create_access_key: { account_id: string; user: string };
  iam_list_access_keys: { account_id: string; user: string };
  iam_set_access_key_active: { account_id: string; user: string; access_key_id: string; active: boolean };
  iam_delete_access_key: { account_id: string; user: string; access_key_id: string; confirm: string };
  iam_create_login_profile: { account_id: string; user: string; password: string; reset_required: boolean };
  iam_update_login_profile: { account_id: string; user: string; password: string | null; reset_required: boolean | null };
  iam_delete_login_profile: { account_id: string; user: string; confirm: string };
  list_multipart_uploads: { account_id: string; buckets: string[] };
  abort_multipart_uploads: { account_id: string; uploads: UploadRef[] };
  save_filter: { filter: SavedFilter; replaces: string | null };
  delete_filter: { name: string };
  // Mount as drive (T04); read-only, prefix null mounts the whole bucket.
  mount_bucket: { account_id: string; bucket: string; prefix: string | null; mount_point: string };
  unmount_bucket: { mount_point: string };
  // Security scan (T13). buckets: null scans every bucket of the account.
  run_security_scan: { account_id: string; buckets: string[] | null };
  export_scan_report: { report: ScanReport; format: ScanReportFormat; path: string };
  export_listing: {
    account_id: string;
    bucket: string;
    prefix: string;
    recursive: boolean;
    format: ListingFormat;
    path: string;
    filter: FilterSpec | null;
  };
  // Object metadata (T06)
  get_object_details: { account_id: string; bucket: string; key: string; sse_c_key: string | null };
  update_object_tags: { account_id: string; bucket: string; keys: string[]; edit: TagEdit };
  set_object_storage_class: {
    account_id: string;
    bucket: string;
    keys: string[];
    storage_class: StorageClass;
    sse_c_key: string | null;
  };
  set_object_encryption: {
    account_id: string;
    bucket: string;
    keys: string[];
    encryption: EncryptionChange;
    source_sse_c_key: string | null;
  };
  update_object_headers: {
    account_id: string;
    bucket: string;
    keys: string[];
    changes: HeaderChanges;
    sse_c_key: string | null;
  };
  set_object_acl: { account_id: string; bucket: string; keys: string[]; acl: CannedAcl };
  get_bucket_object_lock: { account_id: string; bucket: string };
  put_bucket_object_lock: { account_id: string; bucket: string; default_retention: DefaultRetentionRule | null };
  get_object_lock: { account_id: string; bucket: string; key: string; version_id: string | null };
  set_object_retention: {
    account_id: string;
    bucket: string;
    keys: string[];
    change: RetentionChange;
    version_id: string | null;
    bypass_governance: boolean;
  };
  set_object_legal_hold: {
    account_id: string;
    bucket: string;
    keys: string[];
    on: boolean;
    version_id: string | null;
  };
  get_object_ownership: { account_id: string; bucket: string };
  put_object_ownership: { account_id: string; bucket: string; ownership: ObjectOwnership };
  restore_objects: {
    account_id: string;
    bucket: string;
    keys: string[];
    days: number;
    tier: RestoreTier;
    version_id: string | null;
  };
  get_restore_state: { account_id: string; bucket: string; key: string; version_id: string | null };
  cse_probe_object: { account_id: string; bucket: string; key: string };
  cse_check_password: { account_id: string; bucket: string; key: string; password: string | null };
  cse_encrypt_object: {
    account_id: string;
    bucket: string;
    key: string;
    password: string;
    compression: CseCompression | null;
  };
  cse_decrypt_object: { account_id: string; bucket: string; key: string; password: string };
  enqueue_upload_encrypted: {
    account_id: string;
    local_paths: string[];
    bucket: string;
    prefix: string;
    password: string;
    compression: CseCompression | null;
  };
  enqueue_download_decrypted: {
    account_id: string;
    bucket: string;
    keys: string[];
    dest_dir: string;
    password: string | null;
  };
}

/** Payload type of every backend event, keyed by event name. */
export interface BackendEvents {
  "transfer://update": TransferTask;
  "transfer://removed": TransferRemovedEvent;
  "sync://progress": SyncProgress;
  "sync://row": SyncRowEvent;
  "log://entry": LogEntry;
  "security://changed": SecurityStatus;
  "bookmarks://changed": Bookmark[];
  "mount://changed": MountChangedEvent;
}

export type BackendEventName = keyof BackendEvents;
export type Unsubscribe = () => void;

export const BACKEND_EVENT_NAMES: readonly BackendEventName[] = [
  "transfer://update",
  "transfer://removed",
  "sync://progress",
  "sync://row",
  "log://entry",
  "security://changed",
  "bookmarks://changed",
  "mount://changed",
];

/**
 * One method per IPC command (named exactly like the command) plus typed
 * event subscriptions. Implemented by the Tauri backend and the mock backend.
 */
export interface Backend {
  // Security and accounts
  security_status: () => Promise<SecurityStatus>;
  set_master_password: (args: CommandArgs["set_master_password"]) => Promise<void>;
  unlock: (args: CommandArgs["unlock"]) => Promise<void>;
  lock: () => Promise<void>;
  list_accounts: () => Promise<AccountList>;
  add_account: (args: CommandArgs["add_account"]) => Promise<Account>;
  update_account: (args: CommandArgs["update_account"]) => Promise<Account>;
  remove_account: (args: CommandArgs["remove_account"]) => Promise<void>;
  set_default_account: (args: CommandArgs["set_default_account"]) => Promise<void>;
  mfa_login: (args: CommandArgs["mfa_login"]) => Promise<void>;

  // Browsing
  list_buckets: (args: CommandArgs["list_buckets"]) => Promise<Bucket[]>;
  list_objects: (args: CommandArgs["list_objects"]) => Promise<ObjectPage>;
  create_folder: (args: CommandArgs["create_folder"]) => Promise<void>;
  create_bucket: (args: CommandArgs["create_bucket"]) => Promise<void>;
  delete_bucket: (args: CommandArgs["delete_bucket"]) => Promise<void>;
  presign: (args: CommandArgs["presign"]) => Promise<string>;
  open_object: (args: CommandArgs["open_object"]) => Promise<void>;
  open_object_with: (args: CommandArgs["open_object_with"]) => Promise<void>;
  open_bucket_in_browser: (args: CommandArgs["open_bucket_in_browser"]) => Promise<string>;

  // Bucket configuration
  get_bucket_config: (args: CommandArgs["get_bucket_config"]) => Promise<BucketConfig>;
  get_bucket_policy: (args: CommandArgs["get_bucket_policy"]) => Promise<string | null>;
  put_bucket_policy: (args: CommandArgs["put_bucket_policy"]) => Promise<void>;
  delete_bucket_policy: (args: CommandArgs["delete_bucket_policy"]) => Promise<void>;
  validate_bucket_policy: (args: CommandArgs["validate_bucket_policy"]) => Promise<PolicyIssue[]>;
  format_bucket_policy: (args: CommandArgs["format_bucket_policy"]) => Promise<string>;
  get_public_access_block: (args: CommandArgs["get_public_access_block"]) => Promise<PublicAccessBlock | null>;
  put_public_access_block: (args: CommandArgs["put_public_access_block"]) => Promise<void>;
  delete_public_access_block: (args: CommandArgs["delete_public_access_block"]) => Promise<void>;
  get_bucket_versioning: (args: CommandArgs["get_bucket_versioning"]) => Promise<BucketVersioning>;
  put_bucket_versioning: (args: CommandArgs["put_bucket_versioning"]) => Promise<void>;
  get_bucket_logging: (args: CommandArgs["get_bucket_logging"]) => Promise<LoggingTarget | null>;
  put_bucket_logging: (args: CommandArgs["put_bucket_logging"]) => Promise<void>;
  disable_bucket_logging: (args: CommandArgs["disable_bucket_logging"]) => Promise<void>;
  get_bucket_website: (args: CommandArgs["get_bucket_website"]) => Promise<WebsiteConfig | null>;
  put_bucket_website: (args: CommandArgs["put_bucket_website"]) => Promise<void>;
  delete_bucket_website: (args: CommandArgs["delete_bucket_website"]) => Promise<void>;
  get_bucket_acceleration: (args: CommandArgs["get_bucket_acceleration"]) => Promise<AccelerationStatus>;
  put_bucket_acceleration: (args: CommandArgs["put_bucket_acceleration"]) => Promise<void>;
  list_external_buckets: (args: CommandArgs["list_external_buckets"]) => Promise<string[]>;
  add_external_bucket: (args: CommandArgs["add_external_bucket"]) => Promise<void>;
  remove_external_bucket: (args: CommandArgs["remove_external_bucket"]) => Promise<void>;
  list_bookmarks: () => Promise<Bookmark[]>;
  add_bookmark: (args: CommandArgs["add_bookmark"]) => Promise<Bookmark[]>;
  rename_bookmark: (args: CommandArgs["rename_bookmark"]) => Promise<Bookmark[]>;
  remove_bookmark: (args: CommandArgs["remove_bookmark"]) => Promise<Bookmark[]>;
  /** Opens another main window and returns its label. */
  open_new_window: (args: CommandArgs["open_new_window"]) => Promise<string>;
  get_bucket_rules: (args: CommandArgs["get_bucket_rules"]) => Promise<BucketRules>;
  get_bucket_lifecycle: (args: CommandArgs["get_bucket_lifecycle"]) => Promise<LifecycleConfig | null>;
  put_bucket_lifecycle: (args: CommandArgs["put_bucket_lifecycle"]) => Promise<void>;
  delete_bucket_lifecycle: (args: CommandArgs["delete_bucket_lifecycle"]) => Promise<void>;
  validate_bucket_lifecycle: (args: CommandArgs["validate_bucket_lifecycle"]) => Promise<RuleIssue[]>;
  get_bucket_cors: (args: CommandArgs["get_bucket_cors"]) => Promise<CorsConfig | null>;
  put_bucket_cors: (args: CommandArgs["put_bucket_cors"]) => Promise<void>;
  delete_bucket_cors: (args: CommandArgs["delete_bucket_cors"]) => Promise<void>;
  validate_bucket_cors: (args: CommandArgs["validate_bucket_cors"]) => Promise<RuleIssue[]>;
  get_bucket_replication: (args: CommandArgs["get_bucket_replication"]) => Promise<ReplicationConfig | null>;
  put_bucket_replication: (args: CommandArgs["put_bucket_replication"]) => Promise<void>;
  delete_bucket_replication: (args: CommandArgs["delete_bucket_replication"]) => Promise<void>;
  validate_bucket_replication: (args: CommandArgs["validate_bucket_replication"]) => Promise<RuleIssue[]>;
  list_bucket_tiering: (args: CommandArgs["list_bucket_tiering"]) => Promise<TieringConfig[]>;
  put_bucket_tiering: (args: CommandArgs["put_bucket_tiering"]) => Promise<void>;
  delete_bucket_tiering: (args: CommandArgs["delete_bucket_tiering"]) => Promise<void>;
  validate_bucket_tiering: (args: CommandArgs["validate_bucket_tiering"]) => Promise<RuleIssue[]>;

  // Transfer queue
  enqueue_upload: (args: CommandArgs["enqueue_upload"]) => Promise<TransferTask[]>;
  enqueue_download: (args: CommandArgs["enqueue_download"]) => Promise<TransferTask[]>;
  enqueue_copy: (args: CommandArgs["enqueue_copy"]) => Promise<TransferTask[]>;
  enqueue_move: (args: CommandArgs["enqueue_move"]) => Promise<TransferTask[]>;
  enqueue_delete: (args: CommandArgs["enqueue_delete"]) => Promise<TransferTask[]>;
  rename: (args: CommandArgs["rename"]) => Promise<TransferTask[]>;
  list_tasks: () => Promise<TransferTask[]>;
  start_all: () => Promise<void>;
  stop_all: () => Promise<void>;
  cancel_all: () => Promise<void>;
  clear_finished: () => Promise<void>;
  pause_task: (args: CommandArgs["pause_task"]) => Promise<void>;
  resume_task: (args: CommandArgs["resume_task"]) => Promise<void>;
  cancel_task: (args: CommandArgs["cancel_task"]) => Promise<void>;
  retry_task: (args: CommandArgs["retry_task"]) => Promise<void>;

  // Sync
  sync_analyze: (args: CommandArgs["sync_analyze"]) => Promise<SyncPlan>;
  sync_execute: (args: CommandArgs["sync_execute"]) => Promise<void>;
  sync_cancel: (args: CommandArgs["sync_cancel"]) => Promise<void>;
  sync_get: (args: CommandArgs["sync_get"]) => Promise<SyncPlan>;

  // CloudFront (T09)
  list_cloudfront_distributions: (args: CommandArgs["list_cloudfront_distributions"]) => Promise<CloudFrontPage>;
  get_cloudfront_distribution: (args: CommandArgs["get_cloudfront_distribution"]) => Promise<CloudFrontDistribution>;
  find_cloudfront_distributions: (args: CommandArgs["find_cloudfront_distributions"]) => Promise<CloudFrontDistribution[]>;
  create_cloudfront_distribution: (args: CommandArgs["create_cloudfront_distribution"]) => Promise<CreatedDistribution>;
  update_cloudfront_distribution: (args: CommandArgs["update_cloudfront_distribution"]) => Promise<CloudFrontDistribution>;
  set_cloudfront_enabled: (args: CommandArgs["set_cloudfront_enabled"]) => Promise<CloudFrontDistribution>;
  detach_cloudfront_bucket: (args: CommandArgs["detach_cloudfront_bucket"]) => Promise<CloudFrontDistribution>;
  create_cloudfront_invalidation: (args: CommandArgs["create_cloudfront_invalidation"]) => Promise<CloudFrontInvalidation>;
  list_cloudfront_invalidations: (args: CommandArgs["list_cloudfront_invalidations"]) => Promise<InvalidationPage>;
  get_cloudfront_invalidation: (args: CommandArgs["get_cloudfront_invalidation"]) => Promise<CloudFrontInvalidation>;
  validate_invalidation_paths: (args: CommandArgs["validate_invalidation_paths"]) => Promise<string[]>;
  list_invalidation_path_sets: () => Promise<PathSet[]>;
  save_invalidation_path_set: (args: CommandArgs["save_invalidation_path_set"]) => Promise<PathSet[]>;
  delete_invalidation_path_set: (args: CommandArgs["delete_invalidation_path_set"]) => Promise<PathSet[]>;

  // Versions and preview
  list_versions: (args: CommandArgs["list_versions"]) => Promise<VersionGroupPage>;
  restore_version: (args: CommandArgs["restore_version"]) => Promise<RestoreOutcome>;
  purge_versions: (args: CommandArgs["purge_versions"]) => Promise<DeleteOutcome>;
  enqueue_download_version: (args: CommandArgs["enqueue_download_version"]) => Promise<TransferTask[]>;
  preview_object: (args: CommandArgs["preview_object"]) => Promise<ObjectPreview>;

  // Settings, log, app
  get_settings: () => Promise<Settings>;
  save_settings: (args: CommandArgs["save_settings"]) => Promise<void>;
  set_proxy_password: (args: CommandArgs["set_proxy_password"]) => Promise<void>;
  shorten_url: (args: CommandArgs["shorten_url"]) => Promise<string>;
  set_shortener_token: (args: CommandArgs["set_shortener_token"]) => Promise<void>;
  get_log: (args: CommandArgs["get_log"]) => Promise<LogEntry[]>;
  export_log: (args: CommandArgs["export_log"]) => Promise<void>;
  clear_log: () => Promise<void>;
  app_info: () => Promise<AppInfo>;
  last_crash_report: () => Promise<CrashReport | null>;
  dismiss_crash_report: (args: CommandArgs["dismiss_crash_report"]) => Promise<void>;

  // Tools and export (T12)
  run_diagnostics: (args: CommandArgs["run_diagnostics"]) => Promise<DiagnosticsReport>;

  // IAM (T02)
  iam_status: (args: CommandArgs["iam_status"]) => Promise<IamStatus>;
  iam_list_users: (args: CommandArgs["iam_list_users"]) => Promise<IamUser[]>;
  iam_create_user: (args: CommandArgs["iam_create_user"]) => Promise<IamUser>;
  iam_update_user: (args: CommandArgs["iam_update_user"]) => Promise<IamUser>;
  iam_delete_user: (args: CommandArgs["iam_delete_user"]) => Promise<IamDeleteOutcome>;
  iam_user_details: (args: CommandArgs["iam_user_details"]) => Promise<IamUserDetails>;
  iam_list_managed_policies: (args: CommandArgs["iam_list_managed_policies"]) => Promise<IamManagedPolicy[]>;
  iam_attach_policy: (args: CommandArgs["iam_attach_policy"]) => Promise<void>;
  iam_detach_policy: (args: CommandArgs["iam_detach_policy"]) => Promise<void>;
  iam_get_inline_policy: (args: CommandArgs["iam_get_inline_policy"]) => Promise<IamInlinePolicy>;
  iam_put_inline_policy: (args: CommandArgs["iam_put_inline_policy"]) => Promise<void>;
  iam_delete_inline_policy: (args: CommandArgs["iam_delete_inline_policy"]) => Promise<void>;
  iam_validate_policy: (args: CommandArgs["iam_validate_policy"]) => Promise<PolicyIssue[]>;
  iam_format_policy: (args: CommandArgs["iam_format_policy"]) => Promise<string>;
  iam_create_access_key: (args: CommandArgs["iam_create_access_key"]) => Promise<IamNewAccessKey>;
  iam_list_access_keys: (args: CommandArgs["iam_list_access_keys"]) => Promise<IamAccessKey[]>;
  iam_set_access_key_active: (args: CommandArgs["iam_set_access_key_active"]) => Promise<void>;
  iam_delete_access_key: (args: CommandArgs["iam_delete_access_key"]) => Promise<void>;
  iam_create_login_profile: (args: CommandArgs["iam_create_login_profile"]) => Promise<void>;
  iam_update_login_profile: (args: CommandArgs["iam_update_login_profile"]) => Promise<void>;
  iam_delete_login_profile: (args: CommandArgs["iam_delete_login_profile"]) => Promise<void>;
  list_multipart_uploads: (args: CommandArgs["list_multipart_uploads"]) => Promise<UploadListing>;
  abort_multipart_uploads: (args: CommandArgs["abort_multipart_uploads"]) => Promise<AbortResult[]>;
  mount_status: () => Promise<MountStatus>;
  mount_bucket: (args: CommandArgs["mount_bucket"]) => Promise<MountInfo>;
  unmount_bucket: (args: CommandArgs["unmount_bucket"]) => Promise<void>;
  run_security_scan: (args: CommandArgs["run_security_scan"]) => Promise<ScanReport>;
  export_scan_report: (args: CommandArgs["export_scan_report"]) => Promise<void>;
  list_filters: () => Promise<SavedFilter[]>;
  save_filter: (args: CommandArgs["save_filter"]) => Promise<SavedFilter[]>;
  delete_filter: (args: CommandArgs["delete_filter"]) => Promise<SavedFilter[]>;
  export_listing: (args: CommandArgs["export_listing"]) => Promise<ExportOutcome>;
  // Object metadata (T06)
  get_object_details: (args: CommandArgs["get_object_details"]) => Promise<ObjectDetails>;
  update_object_tags: (args: CommandArgs["update_object_tags"]) => Promise<ObjectChange[]>;
  set_object_storage_class: (args: CommandArgs["set_object_storage_class"]) => Promise<ObjectChange[]>;
  set_object_encryption: (args: CommandArgs["set_object_encryption"]) => Promise<ObjectChange[]>;
  update_object_headers: (args: CommandArgs["update_object_headers"]) => Promise<ObjectChange[]>;
  set_object_acl: (args: CommandArgs["set_object_acl"]) => Promise<ObjectChange[]>;
  get_bucket_object_lock: (args: CommandArgs["get_bucket_object_lock"]) => Promise<BucketObjectLock>;
  put_bucket_object_lock: (args: CommandArgs["put_bucket_object_lock"]) => Promise<void>;
  get_object_lock: (args: CommandArgs["get_object_lock"]) => Promise<ObjectLockState>;
  set_object_retention: (args: CommandArgs["set_object_retention"]) => Promise<ObjectChange[]>;
  set_object_legal_hold: (args: CommandArgs["set_object_legal_hold"]) => Promise<ObjectChange[]>;
  get_object_ownership: (args: CommandArgs["get_object_ownership"]) => Promise<ObjectOwnership | null>;
  put_object_ownership: (args: CommandArgs["put_object_ownership"]) => Promise<void>;
  restore_objects: (args: CommandArgs["restore_objects"]) => Promise<ObjectChange[]>;
  get_restore_state: (args: CommandArgs["get_restore_state"]) => Promise<ArchiveState>;
  // Client-side encryption (T07)
  cse_probe_object: (args: CommandArgs["cse_probe_object"]) => Promise<ObjectCse>;
  cse_check_password: (args: CommandArgs["cse_check_password"]) => Promise<ObjectCse>;
  cse_encrypt_object: (args: CommandArgs["cse_encrypt_object"]) => Promise<TransformOutcome>;
  cse_decrypt_object: (args: CommandArgs["cse_decrypt_object"]) => Promise<TransformOutcome>;
  enqueue_upload_encrypted: (args: CommandArgs["enqueue_upload_encrypted"]) => Promise<TransferTask[]>;
  enqueue_download_decrypted: (args: CommandArgs["enqueue_download_decrypted"]) => Promise<TransferTask[]>;

  /** Subscribes to a backend event. The returned function unsubscribes. */
  listen: <E extends BackendEventName>(event: E, handler: (payload: BackendEvents[E]) => void) => Unsubscribe;
}

export type CommandName = Exclude<keyof Backend, "listen">;

/** Every command in docs/desktop-api.md; the record type keeps it exhaustive. */
const COMMAND_TABLE: Record<CommandName, true> = {
  security_status: true,
  set_master_password: true,
  unlock: true,
  lock: true,
  list_accounts: true,
  add_account: true,
  update_account: true,
  remove_account: true,
  set_default_account: true,
  mfa_login: true,
  list_buckets: true,
  list_objects: true,
  create_folder: true,
  create_bucket: true,
  delete_bucket: true,
  presign: true,
  open_object: true,
  open_object_with: true,
  open_bucket_in_browser: true,
  get_bucket_config: true,
  get_bucket_policy: true,
  put_bucket_policy: true,
  delete_bucket_policy: true,
  validate_bucket_policy: true,
  format_bucket_policy: true,
  get_public_access_block: true,
  put_public_access_block: true,
  delete_public_access_block: true,
  get_bucket_versioning: true,
  put_bucket_versioning: true,
  get_bucket_logging: true,
  put_bucket_logging: true,
  disable_bucket_logging: true,
  get_bucket_website: true,
  put_bucket_website: true,
  delete_bucket_website: true,
  get_bucket_acceleration: true,
  put_bucket_acceleration: true,
  list_external_buckets: true,
  add_external_bucket: true,
  remove_external_bucket: true,
  list_bookmarks: true,
  add_bookmark: true,
  rename_bookmark: true,
  remove_bookmark: true,
  open_new_window: true,
  get_bucket_rules: true,
  get_bucket_lifecycle: true,
  put_bucket_lifecycle: true,
  delete_bucket_lifecycle: true,
  validate_bucket_lifecycle: true,
  get_bucket_cors: true,
  put_bucket_cors: true,
  delete_bucket_cors: true,
  validate_bucket_cors: true,
  get_bucket_replication: true,
  put_bucket_replication: true,
  delete_bucket_replication: true,
  validate_bucket_replication: true,
  list_bucket_tiering: true,
  put_bucket_tiering: true,
  delete_bucket_tiering: true,
  validate_bucket_tiering: true,
  enqueue_upload: true,
  enqueue_download: true,
  enqueue_copy: true,
  enqueue_move: true,
  enqueue_delete: true,
  rename: true,
  list_tasks: true,
  start_all: true,
  stop_all: true,
  cancel_all: true,
  clear_finished: true,
  pause_task: true,
  resume_task: true,
  cancel_task: true,
  retry_task: true,
  sync_analyze: true,
  sync_execute: true,
  sync_cancel: true,
  sync_get: true,
  list_cloudfront_distributions: true,
  get_cloudfront_distribution: true,
  find_cloudfront_distributions: true,
  create_cloudfront_distribution: true,
  update_cloudfront_distribution: true,
  set_cloudfront_enabled: true,
  detach_cloudfront_bucket: true,
  create_cloudfront_invalidation: true,
  list_cloudfront_invalidations: true,
  get_cloudfront_invalidation: true,
  validate_invalidation_paths: true,
  list_invalidation_path_sets: true,
  save_invalidation_path_set: true,
  delete_invalidation_path_set: true,
  list_versions: true,
  restore_version: true,
  purge_versions: true,
  enqueue_download_version: true,
  preview_object: true,
  get_settings: true,
  save_settings: true,
  set_proxy_password: true,
  shorten_url: true,
  set_shortener_token: true,
  get_log: true,
  export_log: true,
  clear_log: true,
  app_info: true,
  last_crash_report: true,
  dismiss_crash_report: true,
  run_diagnostics: true,
  iam_status: true,
  iam_list_users: true,
  iam_create_user: true,
  iam_update_user: true,
  iam_delete_user: true,
  iam_user_details: true,
  iam_list_managed_policies: true,
  iam_attach_policy: true,
  iam_detach_policy: true,
  iam_get_inline_policy: true,
  iam_put_inline_policy: true,
  iam_delete_inline_policy: true,
  iam_validate_policy: true,
  iam_format_policy: true,
  iam_create_access_key: true,
  iam_list_access_keys: true,
  iam_set_access_key_active: true,
  iam_delete_access_key: true,
  iam_create_login_profile: true,
  iam_update_login_profile: true,
  iam_delete_login_profile: true,
  list_multipart_uploads: true,
  abort_multipart_uploads: true,
  list_filters: true,
  save_filter: true,
  delete_filter: true,
  mount_status: true,
  mount_bucket: true,
  unmount_bucket: true,
  run_security_scan: true,
  export_scan_report: true,
  export_listing: true,
  get_object_details: true,
  update_object_tags: true,
  set_object_storage_class: true,
  set_object_encryption: true,
  update_object_headers: true,
  set_object_acl: true,
  get_bucket_object_lock: true,
  put_bucket_object_lock: true,
  get_object_lock: true,
  set_object_retention: true,
  set_object_legal_hold: true,
  get_object_ownership: true,
  put_object_ownership: true,
  restore_objects: true,
  get_restore_state: true,
  cse_probe_object: true,
  cse_check_password: true,
  cse_encrypt_object: true,
  cse_decrypt_object: true,
  enqueue_upload_encrypted: true,
  enqueue_download_decrypted: true,
};

export const COMMAND_NAMES = Object.keys(COMMAND_TABLE) as CommandName[];
