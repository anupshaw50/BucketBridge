// TypeScript mirror of the JSON shapes in docs/desktop-api.md.
// Field names, optionality and unions must stay identical to the contract.

// ---- errors ----
export type BbErrorCode =
  | "usage" // invalid input from the caller
  | "auth" // missing or invalid credentials, access denied, wrong master password
  | "locked" // a master password is set and secrets are locked: call unlock()
  | "mfa_required" // account has mfa_serial and no MFA session: call mfa_login()
  | "not_found" // bucket, key, account, task or plan does not exist
  | "conflict" // bucket not empty, label already used, destination exists
  | "remote" // any other S3 or network failure
  | "io" // local filesystem failure
  | "cancelled"
  | "internal";

export interface BbError {
  code: BbErrorCode;
  message: string;
}

// ---- accounts and security ----
/** Where an account's credentials come from (docs/desktop-api.md, "Credential sources (v0.2.0)"). */
export type CredentialSource =
  | { type: "static_keys" }
  | { type: "aws_profile"; profile: string }
  | {
      type: "assume_role";
      source_account_id: string;
      role_arn: string;
      session_name: string;
      duration_secs: number; // 900 to 43200
      external_id?: string | null;
      mfa_serial?: string | null;
    }
  | {
      type: "sso";
      start_url: string;
      sso_region: string;
      account_id: string;
      role_name: string;
      sso_session?: string | null;
    }
  | { type: "instance_role" };

export interface Account {
  id: string;
  label: string; // unique, case-insensitive
  access_key_id: string;
  secret_ref: string; // SecretStore key of the secret access key
  session_token_ref: string | null; // SecretStore key of a static session token
  region: string; // default "us-east-1"
  endpoint: string | null; // custom endpoint URL; null = AWS
  path_style: boolean;
  request_payer: boolean;
  mfa_serial: string | null;
  /** Credential source; every account of version 0.1.0 has static keys. */
  source: CredentialSource;
  /** Network options; all off for accounts saved before version 0.4.0. */
  options: AccountOptions;
}

/** AWS endpoint variants; both need an account without a custom endpoint. */
export interface AccountOptions {
  /** Transfer Acceleration endpoint for transfers of buckets that have it enabled. */
  accelerate: boolean;
  /** Dual-stack (IPv4 and IPv6) endpoints. */
  dual_stack: boolean;
  /** Settings for S3-compatible providers. */
  compat: CompatOptions;
  /** Never send ListBuckets; show the known and pinned buckets instead. */
  cannot_list_buckets: boolean;
  /** `bucket` or `bucket/folder/`, shown when the server refuses to list buckets. */
  known_buckets: string[];
  /** How the account uses the proxy. */
  proxy: AccountProxy;
}

/** `global`: the app's proxy; `none`: connect directly; `custom`: this proxy for this account (no authentication). */
export type AccountProxy =
  | { type: "global" }
  | { type: "none" }
  | { type: "custom"; kind: "http" | "socks5"; host: string; port: number; bypass: string[] };

/** Settings for S3-compatible providers; the defaults are the AWS behaviour. */
export interface CompatOptions {
  /** Storage classes the provider offers; empty = the AWS list. */
  storage_classes: string[];
  /** Regions the provider has; empty = the AWS table. */
  regions: string[];
  /** Delete one object per request instead of DeleteObjects. */
  single_deletes: boolean;
  /** Copy with one CopyObject request (objects over 5 GiB fail). */
  no_multipart_copy: boolean;
  /** List buckets once per region in `regions` and merge. */
  list_all_regions: boolean;
}

export interface AccountInput {
  label: string;
  /** Static keys only; empty for the other sources. */
  access_key_id: string;
  secret_access_key: string;
  session_token: string | null;
  region: string;
  endpoint: string | null;
  path_style: boolean;
  request_payer: boolean;
  mfa_serial: string | null;
  /** Default `{ type: "static_keys" }`. */
  source?: CredentialSource;
  /** Default: all off. */
  options?: AccountOptions;
}

/** update_account payload: null = keep current value. */
export interface AccountUpdate {
  label: string | null;
  access_key_id: string | null;
  secret_access_key: string | null; // non-null rotates the secret
  session_token: string | null; // non-null replaces the session token
  clear_session_token: boolean;
  region: string | null;
  endpoint: string | null;
  clear_endpoint: boolean;
  path_style: boolean | null;
  request_payer: boolean | null;
  mfa_serial: string | null;
  clear_mfa_serial: boolean;
  /** New settings of an account that is not static keys (same kind of source). */
  source?: CredentialSource | null;
  /** Non-null replaces all network options. */
  options?: AccountOptions | null;
}

// ---- credential sources: AWS profiles, instance role, SSO ----
export type AwsProfileKind =
  | "static_keys"
  | "assume_role"
  | "sso"
  | "credential_process"
  | "web_identity"
  | "incomplete";

export interface AwsProfile {
  name: string;
  kind: AwsProfileKind;
  region: string | null;
  role_arn: string | null;
  source_profile: string | null;
  sso_start_url: string | null;
  sso_account_id: string | null;
  sso_role_name: string | null;
  importable: boolean;
  note: string | null;
}

/** A profile of the AWS files and the account that links it already. */
export interface AwsProfileEntry extends AwsProfile {
  account_id: string | null;
}

export interface SkippedProfile {
  profile: string;
  reason: string;
}

export interface ImportOutcome {
  imported: Account[];
  skipped: SkippedProfile[];
}

export interface InstanceRoleProbe {
  kind: "container" | "instance";
  access_key_id: string;
  expires_at: string | null;
}

export interface SsoPortal {
  start_url: string;
  sso_region: string;
  sso_session?: string | null;
}

export interface SsoLoginStarted {
  login_id: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string | null;
  expires_in_secs: number;
  interval_secs: number;
  browser_opened: boolean;
}

export type SsoLoginPoll = { state: "pending"; interval_secs: number } | { state: "complete"; expires_at: string };

export interface SsoSignInStatus {
  signed_in: boolean;
  expires_at: string | null;
  can_refresh: boolean;
}

export interface SsoAccount {
  account_id: string;
  account_name: string;
  email_address: string | null;
}

export interface AccountFolder {
  name: string; // unique, case-insensitively; 1 to 100 characters
  account_ids: string[]; // the accounts in the folder, in order
}

export interface AccountList {
  accounts: Account[]; // in their order; also the order of the accounts in no folder
  default_account_id: string | null;
  folders: AccountFolder[]; // in their order; [] before folders were used
}

export interface AccountLayout {
  folders: AccountFolder[]; // the folders in order, each with its accounts in order
  accounts: string[]; // every account id, in the order the accounts are listed
}

export interface ExportAccountsOutcome {
  path: string;
  accounts: number;
  folders: number;
}

export type DuplicatePolicy = "skip" | "rename";

export interface ImportedAccount {
  id: string;
  label: string;
  renamed_from: string | null;
  needs_secret: boolean; // a static-key account without a secret access key yet
}

export interface ImportDuplicate {
  label: string;
  existing: string;
  reason: "label" | "same_account";
}

export interface ImportSkipped {
  label: string;
  reason: string;
}

export interface ImportReport {
  imported: ImportedAccount[];
  duplicates: ImportDuplicate[];
  skipped: ImportSkipped[];
  folders_created: string[];
}

export interface SecurityStatus {
  master_password_set: boolean;
  unlocked: boolean;
}

// ---- buckets and objects ----
export type BucketKind = "aws" | "external" | "cloudfront";

export interface Bucket {
  name: string;
  region: string | null;
  creation_date: string | null;
  owner: string | null;
  kind: BucketKind; // "external" for accounts with a custom endpoint
}

/** Base64 values exactly as S3 returns them. */
export interface Checksums {
  crc32: string | null;
  crc32c: string | null;
  crc64nvme: string | null;
  sha1: string | null;
  sha256: string | null;
}

export interface ObjectEntry {
  key: string; // full key; prefixes end with "/"
  size: number; // 0 for prefixes
  etag: string | null;
  last_modified: string | null;
  storage_class: string | null;
  checksums: Checksums;
  is_prefix: boolean;
}

export interface ObjectPage {
  entries: ObjectEntry[];
  next_token: string | null;
}

export interface ObjectVersion {
  key: string;
  version_id: string;
  is_latest: boolean;
  is_delete_marker: boolean;
  size: number;
  etag: string | null;
  last_modified: string | null;
  storage_class: string | null;
}

export type Sse =
  | { type: "none" }
  | { type: "s3" }
  | { type: "kms"; key_id: string | null; bucket_key: boolean }
  | { type: "c"; key_md5: string }; // the customer key itself is never stored

export type LockMode = "governance" | "compliance";

export interface ObjectLock {
  mode: LockMode | null;
  retain_until: string | null;
  legal_hold: boolean;
}

export interface ObjectMeta {
  content_type: string | null;
  cache_control: string | null;
  content_disposition: string | null;
  content_encoding: string | null;
  user_meta: Record<string, string>;
  tags: Record<string, string>;
  acl: string | null;
  sse: Sse;
  object_lock: ObjectLock | null;
  website_redirect?: string | null; // x-amz-website-redirect-location
}

// ---- transfers ----
export type TransferKind = "upload" | "download" | "copy" | "move" | "delete";

export type TransferState = "queued" | "running" | "stopped" | "completed" | "failed" | "cancelled" | "skipped";

/** What a transfer does when its destination exists. */
export type IfExists = "ask" | "overwrite" | "skip" | "overwrite_if_newer" | "rename";

/** An answer to an overwrite question: `IfExists` without "ask". */
export type OverwriteChoice = "overwrite" | "skip" | "overwrite_if_newer" | "rename";

/** One side of an overwrite question. */
export interface ConflictItem {
  size: number | null;
  modified: string | null; // RFC 3339
}

/** What exists where, for a task that waits for an overwrite decision. */
export interface Conflict {
  kind: TransferKind;
  source: string; // a local path or s3:// URI
  destination: string;
  source_info: ConflictItem;
  destination_info: ConflictItem;
}

/** A task that waits for the user to decide what happens to an existing destination. */
export interface PendingConflict {
  task_id: string;
  group_id: string | null; // an "apply to all" answer covers the tasks of this group
  conflict: Conflict;
}

/** `transfer://conflict`: a task started or stopped waiting. */
export type ConflictEvent = ({ type: "asked" } & PendingConflict) | { type: "resolved"; task_id: string };

export type Location = { type: "local"; path: string } | { type: "s3"; bucket: string; key: string };

export interface TransferTask {
  id: string;
  account_id: string;
  group_id: string | null; // shared by tasks created from one folder operation
  kind: TransferKind;
  src: Location;
  dst: Location | null; // null for delete
  bytes_total: number | null; // delete tasks count objects instead of bytes
  bytes_done: number;
  speed: number; // bytes per second, smoothed; 0 when idle
  eta: number | null; // seconds
  state: TransferState;
  error: string | null;
  attempts: number;
  created_at: string;
  updated_at: string;
}

// ---- sync ----
export type SyncDirection = "upload" | "download" | "bidirectional" | "s3_to_s3";
export type SyncAction = "copy" | "delete" | "equal" | "failed";
export type Side = "local" | "remote";

export interface FileMeta {
  size: number;
  modified: string | null;
}

export interface SyncRow {
  rel_path: string; // "/"-separated, relative to local_root and s3_prefix
  action: SyncAction;
  target: Side | null; // side that a copy writes to or a delete removes from
  local_meta: FileMeta | null;
  s3_meta: ObjectEntry | null;
  error: string | null;
  done: boolean; // action applied (equal rows start as done)
}

export interface SyncPlan {
  id: string;
  account_id: string;
  local_root: string;
  s3_prefix: string; // "s3://bucket/prefix/"; the destination of an s3_to_s3 plan
  /** Source prefix of an s3_to_s3 plan; absent for every other direction. */
  source_prefix?: string;
  direction: SyncDirection;
  delete: boolean;
  created_at: string;
  rows: SyncRow[];
}

/** Filters and analysis options of a sync (docs/desktop-api.md, "Sync jobs and sync options (v0.2.0)"). */
export interface SyncOptions {
  filter: FilterSpec;
  /** Include globs: when not empty, only matching paths are synced. */
  include: string[];
  /** Gitignore-style exclusion rules. */
  exclude: string[];
  /** A file of exclusion rules, read at every run. */
  exclude_file: string | null;
  skip_empty_folders: boolean;
  skip_duplicate_content: boolean;
  use_cache: boolean;
  /** Remote prefixes listed at the same time, 1 to 32. */
  parallelism: number;
}

/** A named sync setup kept in the configuration. */
export interface SyncJob {
  /** "" in a job that is about to be created. */
  id: string;
  name: string;
  account_id: string;
  direction: SyncDirection;
  /** Absolute local folder; "" for s3_to_s3. */
  local_root: string;
  /** "s3://bucket/prefix/"; the destination of an s3_to_s3 job. */
  s3_prefix: string;
  /** Source of an s3_to_s3 job. */
  s3_source?: string;
  delete: boolean;
  options: SyncOptions;
}

export interface SyncKindTotals {
  files: number;
  bytes: number;
  failed: number;
  pending: number;
}

/** Summary report of a finished run, sent as `sync://report`. */
export interface SyncReport {
  plan_id: string;
  direction: SyncDirection;
  duration_ms: number;
  cancelled: boolean;
  upload: SyncKindTotals;
  download: SyncKindTotals;
  copy: SyncKindTotals;
  delete_local: SyncKindTotals;
  delete_remote: SyncKindTotals;
  equal: SyncKindTotals;
  unsyncable: number;
  failed: number;
  failures: { rel_path: string; error: string }[];
}

export interface SyncProgress {
  plan_id: string;
  total: number;
  done: number;
  failed: number;
  finished: boolean;
}

// ---- filters ----
export interface FilterSpec {
  prefix: string | null;
  glob: string | null; // matched against the key relative to the listing prefix
  min_size: number | null;
  max_size: number | null;
  modified_after: string | null;
  modified_before: string | null;
  storage_classes: string[]; // empty = any
}

export interface SavedFilter {
  name: string;
  filter: FilterSpec;
}

// ---- settings ----
export type LogLevel = "error" | "warning" | "information";
export type ChecksumAlgorithm = "crc32" | "crc32c" | "crc64nvme" | "sha1" | "sha256" | "none";
export type ThemeSetting = "system" | "light" | "dark";
export type QuitWithTasks = "ask" | "wait" | "quit";
export type ProxyKind = "http" | "socks5";
export type ShortenerProvider = "none" | "bitly" | "tinyurl";

export type UploadRuleSse = "AES256" | "aws:kms" | "aws:kms:dsse";

/** One upload rule (docs/desktop-api.md, "Upload rules"). */
export interface UploadRule {
  name: string; // ""
  enabled: boolean; // true
  pattern: string; // glob; a pattern without "/" matches the file name
  case_insensitive: boolean; // true
  content_type: string | null;
  cache_control: string | null;
  content_disposition: string | null;
  content_encoding: string | null;
  headers: Record<string, string>; // "Content-Language", "Expires", "x-amz-meta-<name>"
  storage_class: string | null;
  sse: UploadRuleSse | null;
  sse_kms_key_id: string | null;
}

export interface UploadRulesSettings {
  enabled: boolean; // true
  rules: UploadRule[]; // in order, the first match wins
}

export interface UploadRuleMatch {
  index: number | null;
  rule: UploadRule | null;
}

export interface Settings {
  general: {
    confirm_delete: boolean; // true
    default_download_dir: string | null; // null = ask every time
    open_with_recent: string[]; // applications chosen in "Open with", most recent first (at most 8)
    keep_awake: boolean; // true: keep the computer awake while transfers or a sync run work
    quit_with_tasks: QuitWithTasks; // "ask"
  };
  interface: {
    theme: ThemeSetting; // "system"
    flat_browsing: boolean; // false
    show_transfer_panel: boolean; // true
    show_log_panel: boolean; // false
    sort_column: "name" | "size" | "modified" | "class"; // "name": sort of the object list
    sort_direction: "asc" | "desc"; // "asc"
    column_widths: { size: number; modified: number; class: number }; // 96, 160, 140 (each 40..=600)
    bucket_page_size: number; // 1000 (10..=100000): buckets shown at once in the bucket list
  };
  advanced: {
    max_attempts: number; // 5 (SDK standard retry mode)
    connect_timeout_secs: number; // 10
    read_timeout_secs: number; // 60
    list_page_size: number; // 1000 (1..=1000)
  };
  data_integrity: {
    checksum_algorithm: ChecksumAlgorithm; // "crc32"; "none" = only when the API requires one
    validate_downloads: boolean; // true
    preserve_mtime: boolean; // true: downloads keep the object's time, uploads store the local time
  };
  logging: {
    level: LogLevel; // "information" (minimum level recorded)
    file_logging: boolean; // true
    max_file_mib: number; // 10
    keep_days: number; // 30, old log files older than this are deleted
    max_total_mib: number; // 100, old log files are deleted while the total is larger
  };
  proxy: {
    enabled: boolean; // false
    kind: ProxyKind; // "http"
    host: string; // ""
    port: number; // 8080
    username: string | null; // null
    password_ref: string | null; // SecretStore key, set via set_proxy_password
    bypass: string[]; // [] host names, "*.suffix", or CIDR ranges
  };
  transfer: {
    max_concurrent_tasks: number; // 4
    max_parts_per_task: number; // 4
    multipart_threshold_mib: number; // 8
    part_size_mib: number; // 8 (grown automatically to stay <= 10,000 parts)
    bandwidth_limit_kibps: number | null; // null = unlimited; shared by all tasks
    retry_attempts: number; // 3 per part or per object
    if_exists: IfExists; // "overwrite": what to do when a destination exists
  };
  shortener: {
    provider: ShortenerProvider; // "none": share links are never sent to a third party
    token_ref: string | null; // SecretStore key, set via set_shortener_token
  };
  upload_rules: UploadRulesSettings; // { enabled: true, rules: [] }
}

// ---- log, app, crash ----
export interface LogEntry {
  id: number;
  ts: string;
  level: LogLevel;
  source: string;
  message: string;
}

export interface AppInfo {
  name: string;
  version: string;
  license: string;
  repository_url: string;
  config_dir: string;
  data_dir: string;
  log_dir: string;
}

export type PartitionId = "aws" | "aws-cn" | "aws-us-gov" | "aws-eusc";

export interface RegionEntry {
  id: string;
  name: string;
  partition: PartitionId;
  dns_suffix: string;
  opt_in: boolean;
}

export interface CrashReport {
  path: string;
  created_at: string;
  contents: string;
}

// ---- command argument unions and event payloads ----
export type PresignMethod = "get" | "put";
export type LogExportFormat = "csv" | "json";

export interface TransferRemovedEvent {
  ids: string[];
}

export interface SyncRowEvent {
  plan_id: string;
  index: number;
  row: SyncRow;
}

// ---- bucket configuration (T03) ----

/** One independently read part of a bucket's configuration. */
export type Section<T> =
  | { type: "loaded"; value: T } // read; the value says what is configured
  | { type: "unsupported"; message: string } // the server does not implement this setting
  | { type: "failed"; error: BbError }; // refused, unreachable, ...

export interface PublicAccessBlock {
  block_public_acls: boolean; // reject requests that add a public ACL
  ignore_public_acls: boolean; // ignore every public ACL
  block_public_policy: boolean; // reject policies that grant public access
  restrict_public_buckets: boolean; // limit a public policy to the owner's account and AWS services
}

export type VersioningStatus = "unversioned" | "enabled" | "suspended"; // "unversioned" = never enabled

export interface BucketVersioning {
  status: VersioningStatus;
  mfa_delete: boolean; // read-only: it is never changed from here
}

export interface LoggingTarget {
  target_bucket: string;
  target_prefix: string; // may be ""
}

export type RedirectProtocol = "http" | "https";

export interface RoutingCondition {
  // at least one field is set
  key_prefix_equals: string | null;
  http_error_code_returned_equals: string | null; // a 4xx or 5xx code such as "404"
}

export interface RoutingRedirect {
  // at least one field is set
  protocol: RedirectProtocol | null;
  host_name: string | null;
  replace_key_prefix_with: string | null; // not together with replace_key_with
  replace_key_with: string | null;
  http_redirect_code: string | null; // a 3xx code such as "301"
}

export interface RoutingRule {
  condition: RoutingCondition | null;
  redirect: RoutingRedirect;
}

export type WebsiteConfig =
  | { type: "hosting"; index_document: string; error_document: string | null; routing_rules: RoutingRule[] }
  | { type: "redirect"; host_name: string; protocol: RedirectProtocol | null }; // redirect all requests

export type AccelerationStatus = "unset" | "enabled" | "suspended"; // "unset" = never configured

export interface BucketConfig {
  policy: Section<string | null>; // pretty-printed JSON; null = no policy
  public_access_block: Section<PublicAccessBlock | null>; // null = none configured
  versioning: Section<BucketVersioning>;
  logging: Section<LoggingTarget | null>; // null = logging is off
  website: Section<WebsiteConfig | null>; // null = the bucket is not a website
  acceleration: Section<AccelerationStatus>;
}

/** One problem of a policy document; an empty path means the document as a whole. */
export interface PolicyIssue {
  path: string;
  message: string;
}

// ---- tools and export (T12) ----
export type DiagnosticStepId = "dns" | "tcp" | "proxy" | "tls" | "credentials" | "region";
export type DiagnosticStatus = "pass" | "fail" | "skip";

export interface DiagnosticStep {
  id: DiagnosticStepId;
  status: DiagnosticStatus;
  elapsed_ms: number; // 0 for a skipped step
  summary: string; // one sentence: what was checked and how it ended
  detail: string | null; // addresses, certificate, clock offset, hint
  code: string; // stable outcome code, see docs/desktop-api.md "Diagnostics"
}

export interface DiagnosticsReport {
  target: string; // endpoint that was probed; never carries credentials
  region: string; // signing region of the account
  bucket: string | null; // bucket of the region probe
  started_at: string;
  elapsed_ms: number;
  ok: boolean; // no step failed (skipped steps do not count)
  steps: DiagnosticStep[]; // always the six steps, in the order of DiagnosticStepId
}

export interface NoncurrentUsage {
  versions: number; // non-current versions, delete markers excluded
  bytes: number; // their total size
  delete_markers: number; // all delete markers, current or not
}

export interface MultipartUsage {
  uploads: number; // unfinished multipart uploads
  parts: number; // parts uploaded so far
  bytes: number; // size of those parts
}

export interface BucketUsage {
  objects: number; // current objects (folder markers included)
  bytes: number; // their total size
  noncurrent: NoncurrentUsage | null; // null unless versions were asked for
  multipart: MultipartUsage | null; // null unless multipart uploads were asked for
}

export interface UsageProgressEvent {
  job_id: string;
  phase: "objects" | "multipart"; // listing keys, or sizing multipart uploads
  usage: BucketUsage; // the totals so far
}

export interface UploadRow {
  bucket: string;
  key: string;
  upload_id: string;
  initiated: string | null;
  storage_class: string | null;
  initiator: string | null;
}

export interface BucketFailure {
  bucket: string;
  error: BbError;
}

export interface UploadListing {
  uploads: UploadRow[];
  failures: BucketFailure[];
}

export interface UploadRef {
  bucket: string;
  key: string;
  upload_id: string;
}

export interface AbortResult {
  bucket: string;
  key: string;
  upload_id: string;
  aborted: boolean;
  error: BbError | null; // why this upload could not be aborted
}

// ---- clone a bucket (docs/desktop-api.md, "Clone a bucket") ----
export type CloneSetting = "versioning" | "encryption" | "public_access_block" | "policy" | "website" | "cors" | "tags" | "lifecycle";
export type CloneSettingState = "configured" | "not_configured" | "unsupported" | "failed";
export type ClonePreviewWarning = "policy_denies" | "kms_key" | "versions_not_copied";
export type CloneApplyState = "applied" | "skipped" | "failed";

export interface CloneSettingPreview {
  setting: CloneSetting;
  state: CloneSettingState;
  count: number | null; // policy statements, CORS rules, lifecycle rules, tags, blocked toggles
  detail: string | null; // "enabled" | "suspended", the encryption algorithm, the website index document
  error: BbError | null;
}

export interface ClonePreview {
  source_bucket: string;
  region: string | null;
  settings: CloneSettingPreview[];
  warnings: ClonePreviewWarning[];
}

export interface CloneSettingResult {
  setting: CloneSetting;
  state: CloneApplyState;
  error: BbError | null;
}

export interface CloneOutcome {
  bucket: string;
  region: string | null;
  results: CloneSettingResult[];
}

export interface CloneBucketResult {
  outcome: CloneOutcome;
  tasks: TransferTask[];
  copy_error: BbError | null;
}

export type PartVerification = "checksum" | "etag" | "skipped" | "mismatch";
export type ResumeProblemCode = "file_too_small" | "part_sizes" | "part_mismatch";

/** What a local file would do for an unfinished multipart upload (docs/desktop-api.md, "Resume an unfinished multipart upload"). */
export interface UploadResumeCheck {
  bucket: string;
  key: string;
  upload_id: string;
  file_size: number;
  part_size: number; // bytes of every part but the last
  part_count: number;
  checksum_algorithm: ChecksumAlgorithm; // "none" when the upload has none
  parts: { number: number; size: number; verified: PartVerification }[];
  missing_parts: number;
  bytes_uploaded: number;
  bytes_remaining: number;
  problem: { code: ResumeProblemCode; message: string; part: number | null } | null;
}

export type ListingFormat = "csv" | "xlsx" | "parquet";

export interface ExportOutcome {
  path: string; // the file that was written
  rows: number; // data rows (without the header)
  bytes: number; // size of the file
}

// ---- versions and preview (T08) ----
export type DeleteMarkerMode = "include" | "exclude" | "only";

export interface VersionFilter {
  show_versions: boolean; // false leaves only delete markers (when they are shown)
  delete_markers: DeleteMarkerMode;
  multiple_versions: boolean; // only keys with more than one entry left
  count_greater_than: number | null; // only keys with more entries left than this
  count_less_than: number | null; // only keys with fewer entries left than this
  modified_after: string | null; // inclusive
  modified_before: string | null; // exclusive
}

export interface VersionGroup {
  key: string;
  versions: ObjectVersion[]; // the entries the filter keeps, newest first
  total: number; // every entry of the key
  matching: number; // entries the filter keeps
  is_deleted: boolean; // the newest entry is a delete marker
  truncated: boolean; // the key has more entries than listed or counted
}

export interface VersionGroupPage {
  groups: VersionGroup[];
  next_token: string | null;
  scanned: number; // entries the server sent for this page, listed or not
}

export interface VersionRef {
  key: string;
  version_id: string;
}

export interface RestoreOutcome {
  key: string;
  restored_version_id: string;
  version_id: string | null; // the new current version
  etag: string | null;
  size: number;
}

export interface DeleteFailure {
  key: string;
  version_id: string | null;
  code: string | null;
  message: string;
}

export interface DeleteOutcome {
  deleted: number;
  failures: DeleteFailure[];
}

export type PreviewKind = "text" | "json" | "image" | "binary" | "too_large" | "empty" | "table";

/** One page of a CSV, TSV or Parquet table. */
export interface PreviewTable {
  columns: string[];
  rows: (string | null)[][];
  total_rows: number;
  truncated: boolean; // only the start of a large CSV/TSV was read
  columns_cut: boolean; // columns beyond the 200th are left out
}

export interface ObjectPreview {
  key: string;
  version_id: string | null;
  kind: PreviewKind;
  size: number;
  content_type: string | null;
  media_type: string | null; // what the preview recognized, e.g. "image/webp"
  last_modified: string | null;
  etag: string | null;
  page: number;
  page_count: number;
  page_size: number;
  offset: number;
  length: number;
  text: string | null; // text and JSON
  formatted: boolean; // JSON that was pretty-printed
  image_base64: string | null; // images
  table: PreviewTable | null; // tables: one page of rows
}

// ---- object metadata (T06) ----
export type StorageClass =
  | "STANDARD"
  | "STANDARD_IA"
  | "ONEZONE_IA"
  | "GLACIER_IR"
  | "GLACIER"
  | "DEEP_ARCHIVE"
  | "INTELLIGENT_TIERING"
  | "REDUCED_REDUNDANCY";

export type CannedAcl =
  | "private"
  | "public-read"
  | "public-read-write"
  | "authenticated-read"
  | "bucket-owner-read"
  | "bucket-owner-full-control";

export type GranteeType = "canonical_user" | "group" | "email";
export type AclPermission = "FULL_CONTROL" | "READ" | "WRITE" | "READ_ACP" | "WRITE_ACP";

export interface AclGrant {
  grantee_type: GranteeType;
  grantee: string; // canonical user ID (or display name), group URI, or e-mail address
  permission: AclPermission;
}

export interface ObjectAcl {
  owner: string | null;
  grants: AclGrant[];
  canned: CannedAcl | null; // the canned ACL the grants equal; null = custom grants
}

export interface RestoreStatus {
  in_progress: boolean;
  expires: string | null;
}

/** HeadObject, GetObjectTagging and GetObjectAcl in one value. */
export interface ObjectDetails {
  key: string;
  size: number;
  etag: string | null;
  last_modified: string | null;
  storage_class: string | null; // always set ("STANDARD" when S3 omits it)
  version_id: string | null;
  checksums: Checksums;
  meta: ObjectMeta; // tags and acl (canned name, or "custom") come from the other two calls
  restore: RestoreStatus | null;
  archive_status: "ARCHIVE_ACCESS" | "DEEP_ARCHIVE_ACCESS" | null;
  acl: ObjectAcl | null;
  tags_error: BbError | null; // GetObjectTagging failed: meta.tags is {}
  acl_error: BbError | null; // GetObjectAcl failed: acl and meta.acl are null
}

export type TagEdit =
  | { mode: "replace"; tags: Record<string, string> } // {} deletes every tag
  | { mode: "merge"; set: Record<string, string>; remove: string[] }; // other tags stay

export type HeaderChange = { action: "keep" } | { action: "set"; value: string } | { action: "remove" };

/** A patch: every object keeps what is not named. */
export interface HeaderChanges {
  content_type: HeaderChange;
  cache_control: HeaderChange;
  content_disposition: HeaderChange;
  content_encoding: HeaderChange;
  website_redirect?: HeaderChange; // "/path" or an http(s) URL
  user_meta_set: Record<string, string>;
  user_meta_remove: string[];
}

export type EncryptionChange =
  | { type: "s3" }
  | { type: "kms"; key_id: string | null; bucket_key: boolean } // key_id null = the AWS managed key
  | { type: "c"; key: string }; // SSE-C key: 64 hex digits or the base64 of 32 bytes; never stored

/** The result for one object of a batch command. */
export interface ObjectChange {
  key: string;
  changed: boolean; // false when already as wanted, and when it failed
  previous: string | null; // old storage class, encryption ("none", "s3", "kms", "c") or ACL
  error: BbError | null;
  warnings: string[];
}

// ---- Bucket rules (T04, T07) ----

export interface RuleIssue {
  path: string;
  message: string;
}

export type RuleStatus = "enabled" | "disabled";

export interface RuleTag {
  key: string;
  value: string;
}

export interface ObjectFilter {
  prefix: string | null;
  tags: RuleTag[];
}

export interface LifecycleRule {
  id: string | null;
  status: RuleStatus;
  filter: {
    prefix: string | null;
    tags: RuleTag[];
    object_size_greater_than: number | null;
    object_size_less_than: number | null;
  };
  transitions: { days: number | null; date: string | null; storage_class: string }[];
  expiration: { days: number | null; date: string | null; expired_object_delete_marker: boolean } | null;
  noncurrent_version_transitions: { noncurrent_days: number; newer_noncurrent_versions: number | null; storage_class: string }[];
  noncurrent_version_expiration: { noncurrent_days: number | null; newer_noncurrent_versions: number | null } | null;
  abort_incomplete_multipart_upload_days: number | null;
}

export interface LifecycleConfig {
  rules: LifecycleRule[];
}

export interface TieringConfig {
  id: string;
  status: RuleStatus;
  filter: ObjectFilter;
  tierings: { access_tier: "ARCHIVE_ACCESS" | "DEEP_ARCHIVE_ACCESS"; days: number }[];
}

export interface CorsRule {
  id: string | null;
  allowed_origins: string[];
  allowed_methods: string[];
  allowed_headers: string[];
  expose_headers: string[];
  max_age_seconds: number | null;
}

export interface CorsConfig {
  rules: CorsRule[];
}

export interface ReplicationRule {
  id: string | null;
  priority: number;
  status: RuleStatus;
  filter: ObjectFilter;
  destination_bucket: string;
  storage_class: string | null;
  delete_marker_replication: boolean;
}

export interface ReplicationConfig {
  role: string;
  rules: ReplicationRule[];
}

export interface BucketRules {
  cors: Section<CorsConfig | null>;
  lifecycle: Section<LifecycleConfig | null>;
  tiering: Section<TieringConfig[]>;
  replication: Section<ReplicationConfig | null>;
}

// ---- CloudFront (T09) ----

export interface CloudFrontOrigin {
  id: string;
  domain_name: string;
  origin_path: string;
  bucket: string | null; // the S3 bucket of an S3 domain name
  website: boolean;
  access_control: boolean;
}

export interface CloudFrontDistribution {
  id: string;
  arn: string;
  domain_name: string;
  status: string; // "Deployed" or "InProgress"
  enabled: boolean;
  comment: string;
  aliases: string[];
  price_class: string;
  last_modified: string | null;
  origins: CloudFrontOrigin[];
  default_root_object: string | null; // null in a listing
  in_progress_invalidations: number | null; // null in a listing
  etag: string | null; // null in a listing
}

export interface CloudFrontPage {
  items: CloudFrontDistribution[];
  next_marker: string | null;
}

export interface NewDistribution {
  bucket: string;
  region: string;
  comment: string;
  enabled: boolean;
  default_root_object: string;
  price_class: string | null;
}

export interface CreatedDistribution {
  distribution: CloudFrontDistribution;
  origin_access_control_id: string;
  bucket_policy: string;
}

export interface DistributionEdit {
  comment?: string | null;
  aliases?: string[] | null;
  default_root_object?: string | null;
  price_class?: string | null;
  enabled?: boolean | null;
}

export type InvalidationStatus = "InProgress" | "Completed";

export interface CloudFrontInvalidation {
  id: string;
  status: string; // "InProgress" or "Completed"
  create_time: string | null;
  paths: string[]; // empty in a history listing
}

export interface InvalidationPage {
  items: CloudFrontInvalidation[];
  next_marker: string | null;
}

export interface PathSet {
  name: string;
  paths: string[];
}

// ---- Object Lock, ownership, restore (T07) ----
export interface DefaultRetentionRule {
  mode: LockMode;
  days: number | null; // exactly one of days and years
  years: number | null;
}

export interface BucketObjectLock {
  enabled: boolean;
  default_retention: DefaultRetentionRule | null;
}

export interface ObjectLockState {
  mode: LockMode | null;
  retain_until: string | null;
  legal_hold: boolean;
}

export type RetentionChange = { action: "set"; mode: LockMode; retain_until: string } | { action: "clear" };

export type ObjectOwnership = "BucketOwnerEnforced" | "ObjectWriter" | "BucketOwnerPreferred";

export type RestoreTier = "expedited" | "standard" | "bulk";

export type ArchiveKind = "glacier" | "deep_archive" | "intelligent_archive" | "intelligent_deep_archive";

export interface ArchiveState {
  storage_class: string;
  archived: boolean;
  restore: RestoreStatus | null;
  /** The archive the object sits in (also while or after a restore); null for a class that is always readable. */
  kind: ArchiveKind | null;
}

export interface ArchiveGroup {
  kind: ArchiveKind;
  objects: number;
  bytes: number;
}

export interface RestoreSummary {
  groups: ArchiveGroup[];
  not_archived: number;
  unreadable: number;
  truncated: boolean;
}

export interface TierEstimate {
  tier: RestoreTier;
  total_usd: number;
  retrieval_usd: number;
  requests_usd: number;
  objects: number;
  /** Archived objects this tier cannot restore. */
  unsupported: number;
}

/** Size, object count and a list-price cost per tier for a restore (approximate, dated, never fetched). */
export interface RestoreEstimate {
  summary: RestoreSummary;
  objects: number;
  bytes: number;
  tiers: TierEstimate[];
  price_date: string;
  note: string;
}

// ---- IAM (T02) ----

export interface IamStatus {
  supported: boolean;
  message: string | null;
}

export interface IamUser {
  name: string;
  arn: string;
  user_id: string;
  path: string;
  created: string | null;
  password_last_used: string | null;
}

export interface IamManagedPolicy {
  name: string;
  arn: string;
  attachment_count: number | null;
  description: string | null;
}

export type IamPolicyScope = "local" | "aws" | "all";

export interface IamInlinePolicy {
  name: string;
  document: string;
}

export interface IamAccessKey {
  access_key_id: string;
  status: "active" | "inactive";
  created: string | null;
}

/** The only time the secret exists: show it once, never keep it. */
export interface IamNewAccessKey {
  user_name: string;
  access_key_id: string;
  secret_access_key: string;
  created: string | null;
}

export interface IamLoginProfile {
  created: string | null;
  password_reset_required: boolean;
}

export interface IamGroup {
  name: string;
  arn: string;
}

export type IamSection<T> =
  | { type: "loaded"; value: T }
  | { type: "unsupported"; message: string }
  | { type: "failed"; error: BbError };

export interface IamUserDetails {
  user: IamUser;
  managed_policies: IamSection<IamManagedPolicy[]>;
  inline_policies: IamSection<string[]>;
  access_keys: IamSection<IamAccessKey[]>;
  login_profile: IamSection<IamLoginProfile | null>;
  groups: IamSection<IamGroup[]>;
}

export interface IamDeleteStep {
  kind: string;
  target: string;
  ok: boolean;
  error: BbError | null;
}

export interface IamDeleteOutcome {
  user_name: string;
  steps: IamDeleteStep[];
  deleted: boolean;
}

// ---- Security scan (T13) ----

export type ScanSeverity = "low" | "medium" | "high" | "critical";
export type ScanFindingId =
  | "public_acl"
  | "public_policy"
  | "public_access_block_off"
  | "public_access_block_partial"
  | "no_default_encryption"
  | "versioning_off"
  | "versioning_suspended"
  | "acls_enabled";
export type ScanFixTab = "policy" | "public-access" | "versioning" | "encryption" | "ownership";
export type ScanCheck = "acl" | "policy" | "public_access_block" | "encryption" | "versioning" | "ownership";
export type ScanReportFormat = "csv" | "json";

export interface ScanFinding {
  id: ScanFindingId;
  severity: ScanSeverity;
  title: string;
  explanation: string;
  fix_tab: ScanFixTab;
}

export interface ScanSkippedCheck {
  check: ScanCheck;
  unsupported: boolean;
  message: string;
}

export interface BucketScan {
  bucket: string;
  findings: ScanFinding[]; // most severe first
  skipped: ScanSkippedCheck[];
  error: BbError | null; // set when the bucket could not be scanned at all
}

export interface ScanReport {
  generated_at: string;
  buckets: BucketScan[];
}

/** Compression applied before client-side encryption. */
export type CseCompression = "none" | "gzip" | "zstd";

/** What `cse_probe_object` found (docs/desktop-api.md, "Client-side encryption (T07)"). */
export interface ObjectCse {
  encrypted: boolean;
  marked: boolean;
  version: number | null;
  compression: CseCompression | null;
  chunk_size: number | null;
}

/** Result of an in-place encrypt or decrypt. */
export interface TransformOutcome {
  size_before: number;
  size_after: number;
  etag: string | null;
}

/** A saved location (docs/desktop-api.md, "Bookmarks and windows (T14)"). */
export interface Bookmark {
  id: string;
  account_id: string;
  bucket: string;
  /** Folder prefix or object key; "" for the bucket itself. */
  prefix: string;
  label: string;
}

/** Where a new window opens. */
export interface WindowLocation {
  account_id: string;
  bucket: string;
  prefix: string;
}

// ---- Mount as drive (T04) ----

/** One active mount. */
export interface MountInfo {
  account_id: string;
  bucket: string;
  prefix: string; // empty for the whole bucket
  mount_point: string; // absolute folder; on Windows also a drive letter such as "X:"
}

export interface MountStatus {
  supported: boolean; // this build can mount (the mount feature on Linux, macOS or Windows)
  fuse_installed: boolean; // the driver is installed: macFUSE, FUSE-T or fusermount; Dokan on Windows
  platform: string; // "linux", "macos", "windows", ...
  drive_letters: string[]; // Windows: free drive letters ("D", "E", ...); empty elsewhere
  mounts: MountInfo[];
}

/** Payload of mount://changed. */
export interface MountChangedEvent {
  mounts: MountInfo[];
}

// ---- Edit in place (v0.2.0) ----

export type EditState = "watching" | "uploading" | "uploaded" | "conflict" | "error" | "stopped";

/** What a conflict is about: the object changed or was deleted on the server since it was opened. */
export interface EditConflict {
  remote_deleted: boolean;
  remote_etag: string | null; // the ETag the object has now, without quotes
  remote_modified: string | null; // RFC 3339
}

/** The objects of a drag between windows (relay_drag). */
export interface DragItems {
  account_id: string;
  bucket: string;
  keys: string[];
}

/** Payload of drag://incoming: a drag of another window is over this one. Positions are CSS pixels of this window. */
export interface DragIncoming {
  phase: "over" | "drop" | "leave";
  x: number;
  y: number;
  items: DragItems | null;
  move: boolean;
}

/** One edit session; also the payload of edit://changed. */
export interface EditSession {
  id: string;
  account_id: string;
  bucket: string;
  key: string;
  local_path: string; // absolute path of the local copy
  state: EditState;
  etag: string | null; // the ETag the next upload expects to find
  version_id: string | null; // version at open time, on versioned buckets
  started_at: string; // RFC 3339
  last_upload_at: string | null; // RFC 3339
  uploads: number;
  error: string | null; // why the last upload failed (state "error")
  conflict: EditConflict | null; // set while state is "conflict"
}

/** overwrite: upload the local copy over the object; save_copy: upload it as `name (edited).ext` and end the session; discard: drop the local changes and end the session; retry: try a failed upload again. */
export type EditResolution = "overwrite" | "save_copy" | "discard" | "retry";

export interface EditResolved {
  session: EditSession | null; // null when the resolution ended the session
  copy_key: string | null; // key of the copy, for save_copy
}

/** What a `bb://` link asks the UI to do (the result of `take_pending_link`). */
export type LinkAction =
  | { type: "open"; account_id: string; bucket: string | null; prefix: string }
  | { type: "error"; message: string };

/** How `quit_app` quits: now, or once no transfer or sync run is active. */
export type QuitMode = "now" | "when_idle";

/** Payload of `quit://requested`. */
export interface QuitRequest {
  active_transfers: number;
  active_syncs: number;
}

// ---- bucket encryption, tags, ACLs and creation options (docs/desktop-api.md, "Priority 3") ----

export type BucketSse = "AES256" | "aws:kms" | "aws:kms:dsse"; // dsse is shown, never set

export interface BucketEncryption {
  algorithm: BucketSse;
  kms_key_id: string | null; // key ID, key ARN, alias name or alias ARN; null = SSE-S3, or the AWS managed key
  bucket_key_enabled: boolean; // S3 Bucket Key; SSE-KMS only
}

export interface BucketAcl {
  acl: ObjectAcl;
  ownership: ObjectOwnership | null; // null = none set, or it could not be read
  acls_disabled: boolean; // ownership is BucketOwnerEnforced: every ACL change is refused
}

/** Optional extras of `create_bucket`; null fields leave the setting to the server. */
export interface CreateBucketOptions {
  object_lock: boolean; // turn Object Lock on (S3 also turns versioning on); never reversible
  ownership: ObjectOwnership | null;
  public_access_block: PublicAccessBlock | null;
}

export interface CreateBucketResult {
  warnings: string[]; // what could not be applied although the bucket exists
}
