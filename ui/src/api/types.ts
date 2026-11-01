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
}

export interface AccountInput {
  label: string;
  access_key_id: string;
  secret_access_key: string;
  session_token: string | null;
  region: string;
  endpoint: string | null;
  path_style: boolean;
  request_payer: boolean;
  mfa_serial: string | null;
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
}

export interface AccountList {
  accounts: Account[];
  default_account_id: string | null;
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

export type TransferState = "queued" | "running" | "stopped" | "completed" | "failed" | "cancelled";

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
export type SyncDirection = "upload" | "download" | "bidirectional";
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
  s3_prefix: string; // "s3://bucket/prefix/"
  direction: SyncDirection;
  delete: boolean;
  created_at: string;
  rows: SyncRow[];
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
export type ProxyKind = "http" | "socks5";
export type ShortenerProvider = "none" | "bitly" | "tinyurl";

export interface Settings {
  general: {
    confirm_delete: boolean; // true
    default_download_dir: string | null; // null = ask every time
    open_with_recent: string[]; // applications chosen in "Open with", most recent first (at most 8)
  };
  interface: {
    theme: ThemeSetting; // "system"
    flat_browsing: boolean; // false
    show_transfer_panel: boolean; // true
    show_log_panel: boolean; // false
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
  };
  logging: {
    level: LogLevel; // "information" (minimum level recorded)
    file_logging: boolean; // true
    max_file_mib: number; // 10
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
  };
  shortener: {
    provider: ShortenerProvider; // "none": share links are never sent to a third party
    token_ref: string | null; // SecretStore key, set via set_shortener_token
  };
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

export type PreviewKind = "text" | "json" | "image" | "binary" | "too_large" | "empty";

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

export interface ArchiveState {
  storage_class: string;
  archived: boolean;
  restore: RestoreStatus | null;
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
