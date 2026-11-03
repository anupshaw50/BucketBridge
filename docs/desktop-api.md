# Desktop IPC contract

Contract between the Tauri host (`crates/desktop`) and the React UI (`ui/`).
The JSON shapes below are produced by the Rust types in `bucketbridge-core`
(modules `model`, `settings`, `error`) and must stay identical on both sides.
When a shape has to change, change this file first, then both sides.

## Conventions

- JSON field names are `snake_case`. Every Tauri command is declared with
  `#[tauri::command(rename_all = "snake_case")]`, so `invoke()` argument keys
  are `snake_case` too.
- Unit enum variants serialize as `snake_case` strings (`"queued"`). Tagged
  unions carry a `type` field.
- Timestamps are RFC 3339 strings in UTC (`2026-10-29T08:15:00Z`). Sizes are
  integer byte counts. IDs are UUID v4 strings.
- `null` means "not set". Optional command arguments are passed as `null`,
  never omitted.
- A failed command rejects with a `BbError` object, never a bare string.
- S3 keys ending in `/` denote prefixes ("folders").

## Errors

```ts
type BbErrorCode =
  | "usage"        // invalid input from the caller
  | "auth"         // missing or invalid credentials, access denied, wrong master password
  | "locked"       // a master password is set and secrets are locked: call unlock()
  | "mfa_required" // account has mfa_serial and no MFA session: call mfa_login()
  | "not_found"    // bucket, key, account, task or plan does not exist
  | "conflict"     // bucket not empty, label already used, destination exists
  | "remote"       // any other S3 or network failure
  | "io"           // local filesystem failure
  | "cancelled"
  | "internal";

interface BbError { code: BbErrorCode; message: string }
```

The CLI maps the same kinds to exit codes: `usage` 1; `remote`, `not_found`,
`conflict`, `io`, `internal` 2; `auth`, `locked`, `mfa_required` 3;
`cancelled` 130; success 0.

## Types

```ts
// ---- accounts and security ----
interface Account {
  id: string;
  label: string;                    // unique, case-insensitive
  access_key_id: string;
  secret_ref: string;               // SecretStore key of the secret access key
  session_token_ref: string | null; // SecretStore key of a static session token
  region: string;                   // default "us-east-1"
  endpoint: string | null;          // custom endpoint URL; null = AWS
  path_style: boolean;
  request_payer: boolean;
  mfa_serial: string | null;
}

interface AccountInput {            // add_account
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

interface AccountUpdate {           // update_account: null = keep current value
  label: string | null;
  access_key_id: string | null;
  secret_access_key: string | null; // non-null rotates the secret
  session_token: string | null;     // non-null replaces the session token
  clear_session_token: boolean;
  region: string | null;
  endpoint: string | null;
  clear_endpoint: boolean;
  path_style: boolean | null;
  request_payer: boolean | null;
  mfa_serial: string | null;
  clear_mfa_serial: boolean;
}

interface AccountList { accounts: Account[]; default_account_id: string | null }
interface SecurityStatus { master_password_set: boolean; unlocked: boolean }

// ---- buckets and objects ----
type BucketKind = "aws" | "external" | "cloudfront";

interface Bucket {
  name: string;
  region: string | null;
  creation_date: string | null;
  owner: string | null;
  kind: BucketKind;                 // "external" for accounts with a custom endpoint
}

interface Checksums {               // base64 values exactly as S3 returns them
  crc32: string | null;
  crc32c: string | null;
  crc64nvme: string | null;
  sha1: string | null;
  sha256: string | null;
}

interface ObjectEntry {
  key: string;                      // full key; prefixes end with "/"
  size: number;                     // 0 for prefixes
  etag: string | null;              // without surrounding quotes
  // checksums are only filled by HEAD-style calls; listings leave them all null
  last_modified: string | null;
  storage_class: string | null;
  checksums: Checksums;
  is_prefix: boolean;
}

interface ObjectPage { entries: ObjectEntry[]; next_token: string | null }

interface ObjectVersion {
  key: string;
  version_id: string;
  is_latest: boolean;
  is_delete_marker: boolean;
  size: number;
  etag: string | null;
  last_modified: string | null;
  storage_class: string | null;
}

type Sse =
  | { type: "none" }
  | { type: "s3" }
  | { type: "kms"; key_id: string | null; bucket_key: boolean }
  | { type: "c"; key_md5: string };  // the customer key itself is never stored

type LockMode = "governance" | "compliance";
interface ObjectLock { mode: LockMode | null; retain_until: string | null; legal_hold: boolean }

interface ObjectMeta {
  content_type: string | null;
  cache_control: string | null;
  content_disposition: string | null;
  content_encoding: string | null;
  user_meta: Record<string, string>;
  tags: Record<string, string>;
  acl: string | null;
  sse: Sse;
  object_lock: ObjectLock | null;
}

// ---- transfers ----
type TransferKind = "upload" | "download" | "copy" | "move" | "delete";
type TransferState = "queued" | "running" | "stopped" | "completed" | "failed" | "cancelled";
type Location =
  | { type: "local"; path: string }
  | { type: "s3"; bucket: string; key: string };

interface TransferTask {
  id: string;
  account_id: string;
  group_id: string | null;          // shared by tasks created from one folder operation
  kind: TransferKind;
  src: Location;
  dst: Location | null;             // null for delete
  bytes_total: number | null;       // delete tasks count objects instead of bytes
  bytes_done: number;
  speed: number;                    // bytes per second, smoothed; 0 when idle
  eta: number | null;               // seconds
  state: TransferState;
  error: string | null;
  attempts: number;
  created_at: string;
  updated_at: string;
}

// ---- sync ----
type SyncDirection = "upload" | "download" | "bidirectional";
type SyncAction = "copy" | "delete" | "equal" | "failed";
type Side = "local" | "remote";
interface FileMeta { size: number; modified: string | null }

interface SyncRow {
  rel_path: string;                 // "/"-separated, relative to local_root and s3_prefix
  action: SyncAction;
  target: Side | null;              // side that a copy writes to or a delete removes from
  local_meta: FileMeta | null;
  s3_meta: ObjectEntry | null;
  error: string | null;
  done: boolean;                    // action applied (equal rows start as done)
}

interface SyncPlan {
  id: string;
  account_id: string;
  local_root: string;
  s3_prefix: string;                // "s3://bucket/prefix/"
  direction: SyncDirection;
  delete: boolean;
  created_at: string;
  rows: SyncRow[];
}

interface SyncProgress { plan_id: string; total: number; done: number; failed: number; finished: boolean }

// ---- filters ----
interface FilterSpec {
  prefix: string | null;
  glob: string | null;              // matched against the key relative to the listing prefix
  min_size: number | null;
  max_size: number | null;
  modified_after: string | null;
  modified_before: string | null;
  storage_classes: string[];        // empty = any
}
interface SavedFilter { name: string; filter: FilterSpec }

// ---- settings (default, then the valid range enforced by Settings::validate) ----
type LogLevel = "error" | "warning" | "information";
type ChecksumAlgorithm = "crc32" | "crc32c" | "crc64nvme" | "sha1" | "sha256" | "none";

interface Settings {
  general: {
    confirm_delete: boolean;              // true
    default_download_dir: string | null;  // null = ask every time
    open_with_recent: string[];           // [] ; "Open with" applications, newest first, at most 8
  };
  interface: {
    theme: "system" | "light" | "dark";   // "system"
    flat_browsing: boolean;               // false
    show_transfer_panel: boolean;         // true
    show_log_panel: boolean;              // false
  };
  advanced: {
    max_attempts: number;                 // 5, 1..=20 (SDK standard retry mode)
    connect_timeout_secs: number;         // 10, 1..=300
    read_timeout_secs: number;            // 60, 1..=3600
    list_page_size: number;               // 1000, 1..=1000
  };
  data_integrity: {
    checksum_algorithm: ChecksumAlgorithm; // "crc32"; "none" = only when the API requires one
    validate_downloads: boolean;          // true
  };
  logging: {
    level: LogLevel;                      // "information" (minimum level recorded)
    file_logging: boolean;                // true
    max_file_mib: number;                 // 10, 1..=1024
  };
  proxy: {
    enabled: boolean;                     // false
    kind: "http" | "socks5";              // "http"
    host: string;                         // ""; required when enabled; no whitespace, no "@" (user name and password have their own fields)
    port: number;                         // 8080, 1..=65535
    username: string | null;              // null; when set, non-empty
    password_ref: string | null;          // SecretStore key, set via set_proxy_password; save_settings keeps it, and only the proxy password of this installation is ever read through it
    bypass: string[];                     // []; non-empty host names, "*.suffix", IPs or CIDR ranges
  };
  transfer: {
    max_concurrent_tasks: number;         // 4, 1..=32
    max_parts_per_task: number;           // 4, 1..=32
    multipart_threshold_mib: number;      // 8, 5..=5120
    part_size_mib: number;                // 8, 5..=5120 (grown automatically to stay <= 10,000 parts)
    bandwidth_limit_kibps: number | null; // null = unlimited, else 1..=104857600; shared by all tasks
    retry_attempts: number;               // 3, 0..=20 automatic retries of a failed task
  };
  shortener?: {                           // optional in what the UI sends (the host fills in the default); see "URL shorteners and mount"
    provider: "none" | "bitly" | "tinyurl"; // "none"
    token_ref: string | null;             // SecretStore key of the API token; save_settings keeps it
  };
}
// All numeric settings are whole numbers. save_settings rejects any value
// outside its range with a single `usage` error that lists every problem.

// ---- log, app, crash ----
interface LogEntry { id: number; ts: string; level: LogLevel; source: string; message: string }

interface AppInfo {
  name: string; version: string; license: string; repository_url: string;
  config_dir: string; data_dir: string; log_dir: string;
}

interface CrashReport { path: string; created_at: string; contents: string }
```

## Commands

All commands are async and reject with `BbError`. `account_id` must name an
existing account; account-bound commands reject with `locked` while a master
password is set and not yet unlocked, and with `mfa_required` when the account
has an `mfa_serial` but no MFA session.

### Security and accounts

| Command | Arguments | Returns |
|---|---|---|
| `security_status` | | `SecurityStatus` |
| `set_master_password` | `current: string \| null, new_password: string \| null` | `void` (`new_password: null` removes it; `current` required when one is set) |
| `unlock` | `password: string` | `void` (wrong password: `auth`, nothing is modified) |
| `lock` | | `void` |
| `list_accounts` | | `AccountList` (works while locked) |
| `add_account` | `input: AccountInput` | `Account` |
| `update_account` | `id: string, update: AccountUpdate` | `Account` |
| `remove_account` | `id: string` | `void` (also deletes its secrets) |
| `set_default_account` | `id: string` | `void` |
| `mfa_login` | `account_id: string, code: string` | `void` (STS session kept in memory until expiry) |

### Browsing

| Command | Arguments | Returns |
|---|---|---|
| `list_buckets` | `account_id` | `Bucket[]` (sorted by name) |
| `list_objects` | `account_id, bucket, prefix: string, flat: boolean, token: string \| null` | `ObjectPage` (page size from settings; prefixes first, then objects; never contains an entry whose key equals `prefix`; in flat listings nested folder markers, zero-byte keys ending in `/`, are ordinary entries with `is_prefix: false`, so clients treat any key ending in `/` as a folder) |
| `create_folder` | `account_id, bucket, key` | `void` (`key` ends with `/`) |
| `create_bucket` | `account_id, name, region: string \| null` | `void` |
| `delete_bucket` | `account_id, name` | `void` (`conflict` while not empty) |
| `presign` | `account_id, bucket, key, method: "get" \| "put", expires_secs: number` | `string` URL (1 s to 7 days) |
| `open_object` | `account_id, bucket, key` | `void` (downloads to a temp dir and opens with the OS default app) |

### Transfer queue

Folder semantics for every enqueue command: a key ending in `/` is a prefix and
expands recursively; the parent part of each selected key is stripped, so the
selected name lands directly under the destination. Example: copying
`photos/2024/` to prefix `backup/` yields `backup/2024/...`; downloading
`photos/2024/a.jpg` to `/tmp/x` yields `/tmp/x/a.jpg`. Local folders upload with
their relative paths preserved and empty folders become folder markers. If two
selected sources would land on the same destination (for example
`a/report.txt` and `b/report.txt` copied, moved, or downloaded into one
folder), the command rejects with `conflict` and enqueues nothing. Names ending
in `.bbpart` (any letter case) are reserved for the temporary files of
downloads: such an object is a failed task and is never downloaded under that
name, and such local files are left out of an upload. An object that holds data
under a key ending in `/` is a failed download task too (a folder cannot hold
data), while a copy or move transfers it like any object.

| Command | Arguments | Returns |
|---|---|---|
| `enqueue_upload` | `account_id, local_paths: string[], bucket, prefix` | `TransferTask[]` |
| `enqueue_download` | `account_id, bucket, keys: string[], dest_dir: string` | `TransferTask[]` |
| `enqueue_copy` | `account_id, src_bucket, keys: string[], dst_bucket, dst_prefix` | `TransferTask[]` |
| `enqueue_move` | `account_id, src_bucket, keys: string[], dst_bucket, dst_prefix` | `TransferTask[]` |
| `enqueue_delete` | `account_id, bucket, keys: string[]` | `TransferTask[]` (one task per key; a prefix is one task) |
| `rename` | `account_id, bucket, from_key, to_key` | `TransferTask[]` (`conflict` if the destination exists) |
| `list_tasks` | | `TransferTask[]` |
| `start_all` / `stop_all` / `cancel_all` / `clear_finished` | | `void` |
| `pause_task` / `resume_task` / `cancel_task` / `retry_task` | `id: string` | `void` |

Queue rules: `pause_task` (queued or running) and `stop_all` move tasks to
`stopped`; `resume_task` and `start_all` move `stopped` tasks back to `queued`;
`retry_task` re-queues a `failed` task; `cancel_task` (queued, running, stopped
or failed) and `cancel_all` abort and clean up (no partial objects, no open
multipart uploads, no partial local files). If the cleanup cannot finish (for
example the network is down while a multipart upload is aborted), the task
becomes `failed` instead of `cancelled` and keeps its progress state, so that
`cancel_task` (or restarting the app) repeats the cleanup. Any other
transition, for example pausing a completed task, rejects with `conflict`. `clear_finished` removes
`completed` and `cancelled` tasks; `failed` tasks stay until retried or
cancelled. The queue is persisted in the data dir; after an app restart
unfinished tasks come back as `stopped` and resume from their saved progress.

### Sync

| Command | Arguments | Returns |
|---|---|---|
| `sync_analyze` | `account_id, local_root, s3_prefix, direction: SyncDirection, delete: boolean` (and optionally `options`, `source_prefix`: see "Sync jobs and sync options (v0.2.0)") | `SyncPlan` (dry run, writes nothing) |
| `sync_execute` | `plan_id` | `void`; runs in the background, rows with `done: false` only, so calling it again resumes |
| `sync_cancel` | `plan_id` | `void` |
| `sync_get` | `plan_id` | `SyncPlan` |

Sync rules:
- Analysis: `copy` and `delete` rows always have a `target`; `equal` rows start
  with `done: true`; a `failed` row cannot be executed and its `error` says why
  (unreadable file, unsafe key, case collision, ambiguous bidirectional
  change). Remote-only files in an upload without `delete` (and local-only
  files in a download without `delete`) produce no row.
- `delete: true` with `direction: "bidirectional"` is rejected with `usage`.
- Execution never stops at the first failure. A row that fails while executing
  keeps its planned action, stays `done: false`, and gets an `error`. Calling
  `sync_execute` again re-runs every `copy` or `delete` row that is not done,
  including previously failed ones, and clears `error` on success.
- `SyncProgress` always covers the whole plan: `total` counts `copy` and
  `delete` rows, `done` counts those with `done: true`, `failed` counts
  `failed` rows plus not-done rows with an `error`. `finished: true` is sent
  once per run, after success, failures, or `sync_cancel`.
- A "failed only" view shows rows whose action is `failed` or whose `error` is
  set.

### Settings, log, app

| Command | Arguments | Returns |
|---|---|---|
| `get_settings` | | `Settings` |
| `save_settings` | `settings: Settings` | `void` |
| `set_proxy_password` | `password: string \| null` | `void` |
| `get_log` | `levels: LogLevel[] \| null, text: string \| null, limit: number \| null` | `LogEntry[]` (newest last; `levels: null` = all; `text` is a case-insensitive substring match on `message` and `source`; `limit` keeps the newest entries; memory holds the newest 10,000) |
| `export_log` | `path: string, format: "csv" \| "json"` | `void` |
| `clear_log` | | `void` |
| `app_info` | | `AppInfo` |
| `last_crash_report` | | `CrashReport \| null` |
| `dismiss_crash_report` | `path: string` | `void` |

## Events

| Event | Payload | When |
|---|---|---|
| `transfer://update` | `TransferTask` | any state change, and at most every 250 ms per running task |
| `transfer://removed` | `{ ids: string[] }` | tasks removed by `clear_finished` |
| `sync://progress` | `SyncProgress` | while a plan executes, and once with `finished: true` |
| `sync://row` | `{ plan_id: string, index: number, row: SyncRow }` | a row finished or failed |
| `log://entry` | `LogEntry` | each new event-log entry at or above the configured level |
| `security://changed` | `SecurityStatus` | after unlock, lock, or a master-password change, and when a change is found in `config.toml` (another process changed the master password, or its section disappeared: the session locks) |

Native file and folder pickers, opening URLs, revealing files, and clipboard
access are done in the UI through the official Tauri plugins (dialog, opener,
clipboard-manager); they are not commands.

## Bucket configuration (T03)

The settings of the "Bucket properties" dialog (policy, public access block,
versioning, server access logging, static website hosting, transfer
acceleration) and the external buckets of an account. Commands that take an
`account_id` follow the rules of "Commands" above (`not_found` for an unknown
account, `locked`, `mfa_required`) and address the bucket in its own region.
They work on AWS and on S3-compatible servers; a server that does not
implement a setting says so (see "Sections").

### Types

```ts
// One independently read part of BucketConfig.
type Section<T> =
  | { type: "loaded"; value: T }              // read; the value says what is configured
  | { type: "unsupported"; message: string }  // the server does not implement this setting
  | { type: "failed"; error: BbError };       // refused, unreachable, ...

interface BucketConfig {
  policy: Section<string | null>;                          // pretty-printed JSON; null = no policy
  public_access_block: Section<PublicAccessBlock | null>;  // null = none configured
  versioning: Section<BucketVersioning>;
  logging: Section<LoggingTarget | null>;                  // null = logging is off
  website: Section<WebsiteConfig | null>;                  // null = the bucket is not a website
  acceleration: Section<AccelerationStatus>;
}

interface PublicAccessBlock {
  block_public_acls: boolean;       // reject requests that add a public ACL
  ignore_public_acls: boolean;      // ignore every public ACL
  block_public_policy: boolean;     // reject policies that grant public access
  restrict_public_buckets: boolean; // limit a public policy to the owner's account and AWS services
}

type VersioningStatus = "unversioned" | "enabled" | "suspended";
interface BucketVersioning {
  status: VersioningStatus;         // "unversioned" = never enabled
  mfa_delete: boolean;              // read-only: it is never changed from here
}

interface LoggingTarget { target_bucket: string; target_prefix: string }  // the prefix may be ""

type RedirectProtocol = "http" | "https";
type WebsiteConfig =
  | { type: "hosting"; index_document: string; error_document: string | null;
      routing_rules: RoutingRule[] }
  | { type: "redirect"; host_name: string; protocol: RedirectProtocol | null }; // redirect all requests
interface RoutingRule { condition: RoutingCondition | null; redirect: RoutingRedirect }
interface RoutingCondition {        // at least one field is set
  key_prefix_equals: string | null;
  http_error_code_returned_equals: string | null;   // "404", a 4xx or 5xx code
}
interface RoutingRedirect {         // at least one field is set
  protocol: RedirectProtocol | null;
  host_name: string | null;
  replace_key_prefix_with: string | null;           // not together with replace_key_with
  replace_key_with: string | null;
  http_redirect_code: string | null;                // "301", a 3xx code
}

type AccelerationStatus = "unset" | "enabled" | "suspended";   // "unset" = never configured

interface PolicyIssue { path: string; message: string }  // path "" = the document as a whole
```

### Commands

| Command | Arguments | Returns |
|---|---|---|
| `get_bucket_config` | `account_id, bucket` | `BucketConfig` (never rejects for one failing section) |
| `get_bucket_policy` | `account_id, bucket` | `string \| null` |
| `put_bucket_policy` | `account_id, bucket, policy: string` | `void` |
| `delete_bucket_policy` | `account_id, bucket` | `void` (succeeds when there is none) |
| `validate_bucket_policy` | `policy: string` | `PolicyIssue[]` (empty = valid; needs no account) |
| `format_bucket_policy` | `policy: string` | `string` (pretty-printed; `usage` when the text is not JSON; needs no account) |
| `get_public_access_block` | `account_id, bucket` | `PublicAccessBlock \| null` |
| `put_public_access_block` | `account_id, bucket, config: PublicAccessBlock` | `void` |
| `delete_public_access_block` | `account_id, bucket` | `void` (succeeds when there is none) |
| `get_bucket_versioning` | `account_id, bucket` | `BucketVersioning` |
| `put_bucket_versioning` | `account_id, bucket, status: VersioningStatus` | `void` (`"unversioned"` is `usage`) |
| `get_bucket_logging` | `account_id, bucket` | `LoggingTarget \| null` |
| `put_bucket_logging` | `account_id, bucket, target: LoggingTarget` | `void` |
| `disable_bucket_logging` | `account_id, bucket` | `void` |
| `get_bucket_website` | `account_id, bucket` | `WebsiteConfig \| null` |
| `put_bucket_website` | `account_id, bucket, website: WebsiteConfig` | `void` |
| `delete_bucket_website` | `account_id, bucket` | `void` (succeeds when it is off) |
| `get_bucket_acceleration` | `account_id, bucket` | `AccelerationStatus` |
| `put_bucket_acceleration` | `account_id, bucket, status: AccelerationStatus` | `void` (`"unset"` is `usage`) |
| `list_external_buckets` | `account_id` | `string[]` (sorted names; works while locked) |
| `add_external_bucket` | `account_id, name` | `void` |
| `remove_external_bucket` | `account_id, name` | `void` (works while locked) |

### Sections

Each setting is read and written on its own, so the dialog loads one tab and
re-reads it after saving. `get_bucket_config` reads all six in parallel and
reports each as a `Section`, so a refusal or a missing feature of one never
hides the others:

- `loaded`: the setting was read. Nothing configured is an answer, not an
  error: `null` (no policy, no public access block, logging off, not a
  website), `"unversioned"` and `"unset"`. The single getters return the same
  values.
- `unsupported`: the server does not implement the setting (HTTP 501 or 405,
  or the codes `NotImplemented`, `XNotImplemented`, `MethodNotAllowed`; common
  on S3-compatible servers). The message names the setting and the server's
  answer. Transfer acceleration is `unsupported` for every account with a
  custom endpoint, without a request.
- `failed`: any other failure, with the `BbError` of the single getter
  (`auth` for `AccessDenied`, `not_found` for a missing bucket, `remote`, ...).

`get_bucket_config` itself rejects only when nothing can be read: an invalid
bucket name (`usage`) or a server that does not answer at all (`remote`).
The single getters and writers reject with the `BbError` of their failure,
the message prefixed with what was being done and for which bucket, for
example `reading the policy of bucket 'site': Access Denied (AccessDenied,
HTTP 403, request id ...)`.

### Policy

`validate_bucket_policy` reports every problem with its path; `put_bucket_policy`
runs the same check and rejects with one `usage` error that lists them all
(`invalid bucket policy: Statement[0].Effect: must be "Allow" or "Deny" (found
"allow"); ...`), before any request. The rules:

- the text is JSON, an object, and at most 20 KiB (20,480 bytes) long without
  whitespace; the policy is sent without whitespace, so indentation never
  counts against the limit;
- the document has `Version` (`2012-10-17` or `2008-10-17`) and `Statement`,
  and optionally `Id`; no other element, and no element twice in any object;
- `Statement` is a non-empty list of objects, or one object;
- a statement has `Effect` (`Allow` or `Deny`), exactly one of `Principal` and
  `NotPrincipal`, exactly one of `Action` and `NotAction`, exactly one of
  `Resource` and `NotResource`, and optionally `Sid` and `Condition`; no other
  element;
- `Action` and `Resource` (and their `Not` forms) are a string or a non-empty
  list of non-empty strings; a principal is `"*"` or an object that maps a
  principal type (`AWS`, `Service`, ...) to such a value; a `Condition` maps
  operators to objects that map condition keys to a string, number, boolean or
  a list of them; a `Sid` is a string that no other statement uses.

`get_bucket_policy` and the `policy` section return the policy pretty-printed
with two-space indentation and the keys in the order S3 returned them (text
that is not JSON is returned as it is). `format_bucket_policy` needs only
valid JSON, so a policy that is still being written can be tidied up; it keeps
the order of the keys.

### Other settings

- **Public access block**: `put_public_access_block` always sends all four
  toggles. Reading a configuration with a missing toggle gives `false` for it.
- **Versioning**: only `"enabled"` and `"suspended"` can be written; S3 cannot
  return to `"unversioned"`. `MFADelete` is never sent, so MFA delete keeps
  its state, and it is reported read-only as `mfa_delete`.
- **Logging**: the target bucket name is validated and the prefix is at most
  1,024 bytes. Changing the target keeps the target grants and the log key
  format the bucket already has: the current configuration is read first, and
  when that read fails nothing is written. S3 itself checks that the target
  exists, is in the same region and allows log delivery.
- **Website**: a hosting configuration needs an index document without a
  slash; an error document is a non-empty key; at most 50 routing rules; a
  condition names a key prefix or a 4xx/5xx error code; a redirect sets at
  least one of protocol, host, replacement key or prefix, and redirect code
  (3xx), and never both a replacement key and a replacement prefix; a
  redirect of all requests needs a host (no scheme, path or whitespace). All
  problems are reported in one `usage` error (`invalid website configuration:
  routing rule 2: ...`).
- **Acceleration**: AWS accounts only. For an account with a custom endpoint
  the getter and the setter reject with `usage` ("Transfer acceleration is
  available on AWS S3 only; this account uses a custom endpoint.") without a
  request. Enabling it for a bucket name with dots is `usage` too (S3 needs a
  DNS-compatible name without periods). `"unset"` cannot be written.

All writes of a section carry the integrity header S3 requires (`Content-MD5`
for policy, public access block, versioning, logging and website).

### External buckets

A bucket that an account can use but not list (`ListBuckets` is denied, or the
bucket belongs to another account) is pinned to the account. The pins are
stored in `config.toml` per account id (`pinned_buckets`), so they survive
restarts, are shared with the CLI, and are removed with the account.

- `add_external_bucket` checks that the bucket exists with `HeadBucket`
  (`not_found` when it does not, `usage` for an invalid name). Being refused
  access, a bucket in another region and a server without `HeadBucket` all
  count as existing. Pinning twice changes nothing.
- `remove_external_bucket` removes only the pin (`not_found` when the bucket
  is not pinned); the bucket is untouched.
- `list_external_buckets` and `remove_external_bucket` read only the
  configuration file, so they work while the secrets are locked.

`list_buckets` (see "Browsing") changed with this task: it merges the pinned
buckets into its result as `Bucket` entries with `kind: "external"` and
`null` region, creation date and owner, a bucket that the server lists keeps
its own entry, and the result stays sorted by name. When the server refuses
to list buckets (`AccessDenied`) and the account has pinned buckets, the
pinned buckets are returned alone; without pins the refusal is the error, as
before.

## Tools and export (T12)

Diagnostics, uncompleted multipart uploads, saved filters and the export of a
listing. Implemented by `crates/core/src/{diagnostics,export,saved_filters}.rs`
and `crates/desktop/src/commands/tools.rs`; the UI lives in
`ui/src/features/tools/`.

### Types

```ts
// ---- diagnostics ----
type DiagnosticStepId = "dns" | "tcp" | "proxy" | "tls" | "credentials" | "region";
type DiagnosticStatus = "pass" | "fail" | "skip";

interface DiagnosticStep {
  id: DiagnosticStepId;
  status: DiagnosticStatus;
  elapsed_ms: number;               // 0 for a skipped step
  summary: string;                  // one sentence: what was checked and how it ended
  detail: string | null;            // addresses, certificate, clock offset, hint
  code: string;                     // stable outcome code, see "Diagnostics" below
}

interface DiagnosticsReport {
  target: string;                   // endpoint that was probed; never carries credentials
  region: string;                   // signing region of the account
  bucket: string | null;            // bucket of the region probe
  started_at: string;
  elapsed_ms: number;
  ok: boolean;                      // no step failed (skipped steps do not count)
  steps: DiagnosticStep[];          // always the six steps, in the order of DiagnosticStepId
}

// ---- uncompleted multipart uploads ----
interface UploadRow {
  bucket: string;
  key: string;
  upload_id: string;
  initiated: string | null;
  storage_class: string | null;
  initiator: string | null;
}
interface BucketFailure { bucket: string; error: BbError }
interface UploadListing { uploads: UploadRow[]; failures: BucketFailure[] }
interface UploadRef { bucket: string; key: string; upload_id: string }
interface AbortResult {
  bucket: string;
  key: string;
  upload_id: string;
  aborted: boolean;
  error: BbError | null;            // why this upload could not be aborted
}

// ---- export ----
type ListingFormat = "csv" | "xlsx" | "parquet";
interface ExportOutcome {
  path: string;                     // the file that was written
  rows: number;                     // data rows (without the header)
  bytes: number;                    // size of the file
}
```

`FilterSpec` and `SavedFilter` are the types of "Types" above.

### Commands

| Command | Arguments | Returns |
|---|---|---|
| `run_diagnostics` | `account_id: string \| null, input: AccountInput \| null, bucket: string \| null` | `DiagnosticsReport` |
| `list_multipart_uploads` | `account_id: string, buckets: string[]` | `UploadListing` |
| `abort_multipart_uploads` | `account_id: string, uploads: UploadRef[]` | `AbortResult[]` |
| `list_filters` | | `SavedFilter[]` |
| `save_filter` | `filter: SavedFilter, replaces: string \| null` | `SavedFilter[]` (the stored list) |
| `delete_filter` | `name: string` | `SavedFilter[]` (the stored list) |
| `export_listing` | `account_id: string, bucket: string, prefix: string, recursive: boolean, format: ListingFormat, path: string, filter: FilterSpec \| null` | `ExportOutcome` |

### Diagnostics

`run_diagnostics` probes one account, step by step, and always answers with a
report: a failing step is a result (`ok: false`), not a rejection. Give exactly
one of `account_id` (a stored account) and `input` (an unsaved account, for
example from the add-account wizard; nothing is stored and `mfa_serial` is
ignored), otherwise `usage`. A stored account rejects with `not_found`,
`locked` or `mfa_required` like any account-bound command; an `input` with an
invalid region, endpoint or empty keys, and an invalid `bucket` name, reject
with `usage`. `bucket` (or `null`) is the bucket of the region probe.

The steps, in this order, with the `code` each can report. Every step has a
`code` whatever its status.

| Step | What it does | Codes |
|---|---|---|
| `dns` | Resolves the host name of the endpoint (of the proxy when one carries the traffic) | `resolved`; skip: `ip_address`; fail: `not_found`, `timeout`, `failed` |
| `tcp` | Opens a TCP connection to the endpoint (to the proxy) and tries every address in turn | `connected`; skip: `blocked`; fail: `refused`, `timeout`, `unreachable`, `failed` |
| `proxy` | Asks the proxy to open a tunnel to the endpoint (HTTP `CONNECT` or SOCKS5 `CONNECT`) | `tunnel_open`, `forwarded` (plain-HTTP endpoint through an HTTP proxy); skip: `none`, `direct`, `blocked`; fail: `auth_required`, `rejected`, `protocol`, `timeout`, `failed` |
| `tls` | TLS handshake with rustls and the OS trust roots, then the certificate subject, issuer and expiry | `handshake`; skip: `plain_http`, `blocked`; fail: `untrusted_issuer`, `expired`, `not_yet_valid`, `name_mismatch`, `bad_certificate`, `protocol`, `timeout`, `failed` |
| `credentials` | One signed `ListBuckets` request (`max-buckets=1`), measured on a single attempt | `valid`, `access_denied` (pass: the key is accepted, it may not list buckets); skip: `blocked`; fail: `invalid_access_key`, `bad_signature`, `clock_skew`, `expired_token`, `invalid_token`, `wrong_region`, `proxy_auth`, `redirect`, `timeout`, `failed` |
| `region` | `HeadBucket` on the named bucket and its `x-amz-bucket-region` header | `found`, `no_region_header` (the server reports no region, as custom endpoints do); skip: `no_bucket`, `blocked`; fail: `bucket_not_found`, `access_denied`, `timeout`, `failed` |

A proxy that carries the traffic (the app's proxy settings, or the proxy
variables of the environment while the settings are disabled, unless the
endpoint is on the bypass list) turns `dns` and `tcp` into probes of the proxy
host, `proxy` into the tunnel to the endpoint and `tls` into a handshake
inside that tunnel (so a TLS-inspecting proxy shows its own certificate).
Without a proxy `proxy` is skipped with `none` (or `direct` for a bypassed
host). A failed network step (`dns`, `tcp`, `proxy`, `tls`) skips the later
steps with `blocked`; a `credentials` step that got no answer at all (`timeout`,
`failed`) skips `region`. Timings are wall-clock milliseconds. The report holds
no secret: not the secret key, the session token, the proxy password or user
information of the endpoint URL.

### Multipart uploads

`list_multipart_uploads` lists the uncompleted uploads of each named bucket
(each in its own region; at most four buckets are listed at the same time).
`buckets` must hold at least one valid name (`usage`); names are listed once.
A bucket that cannot be listed is reported in `failures` and does not fail the
call. `uploads` is sorted by bucket, key and initiation time. Some servers,
MinIO among them, list uploads only for an exact object key.

`abort_multipart_uploads` aborts exactly the named uploads, one after the
other, and reports each result in request order: it never stops at the first
failure and aborts nothing else. An empty list, an invalid bucket name or
key and an empty upload ID are `usage` (nothing is aborted); an upload that is already gone has `error.code` `not_found`.

### Saved filters

Filters live in `config.toml` (`saved_filters`) and are read and written
through `ConfigStore::update`, so the CLI and the app never lose each other's
changes. A name is trimmed, 1 to 100 characters, without control characters,
and unique ignoring case. The spec is compiled by the core filter engine before
it is stored (`usage` with the engine's message for a glob that does not parse,
reversed size bounds, an empty date interval or an empty storage class). At
most 200 filters (`conflict` beyond that).

- `list_filters` returns the stored list in order. The three filter commands
  only touch `config.toml`, so they work while the secrets are locked.
- `save_filter` with `replaces: null` adds the filter (`conflict` when the
  name is taken). With `replaces: "name"` it replaces the filter of that name
  in place, renames included (`not_found` when there is none, `conflict` when
  the new name belongs to another filter). It returns the whole list.
- `delete_filter` removes one (`not_found` when there is none) and returns the
  list.

The object list applies a filter in the UI, over the entries it has loaded,
with the semantics of the core engine (see `crates/core/src/filter.rs`): paths
are relative to the listing prefix, `min_size` and `max_size` are inclusive,
`modified_after` is inclusive and `modified_before` exclusive.

### Export

`export_listing` writes the listing of `s3://bucket/prefix` to `path` (absolute,
else `usage`) and returns what it wrote. With `recursive: true` every object
below the prefix is a row; without it the rows are what the folder view shows
(folders, then objects, one level). `filter`, when set, keeps only the rows the
filter accepts (relative to `prefix`, folders as in the filter engine).

Columns, in every format: `key`, `size`, `last_modified`, `etag`,
`storage_class`. Folder rows (`key` ends in `/`, from a non-recursive listing)
have no size, date, etag or class. `last_modified` is UTC.

| Format | File |
|---|---|
| `csv` | UTF-8 with a header row, RFC 3339 UTC times (`2026-10-29T08:15:00Z`, with milliseconds when S3 sent them). A text field that starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a leading `'`, so that spreadsheet programs never run an object key as a formula. |
| `xlsx` | One sheet `Objects` (more sheets, `Objects 2` and so on, when a listing exceeds Excel's 1,048,576 rows): bold header, frozen header row, sizes as numbers, times as dates, text cells only |
| `parquet` | One row group per 50,000 rows, Snappy pages, `key` required UTF-8, `size` INT64, `last_modified` TIMESTAMP(MILLIS, UTC), `etag` and `storage_class` UTF-8; unknown values are null |

The file is written to a temporary file in the same folder and renamed over
`path` when complete, so a failure (`not_found` bucket, `auth`, `remote`,
`io`) leaves no partial file and an existing file untouched. CSV streams the
listing page by page; XLSX and Parquet hold at most one sheet row or one row
group in memory, so a listing of any size needs bounded memory.

## Versions and preview (T08)

The version list of a prefix, restoring and purging versions, the download of one
version, and the preview of an object or of one of its versions. The code is
`bucketbridge_core::versions` and `bucketbridge_core::preview`; the commands are
in `crates/desktop/src/commands/versions.rs`.

### Types

```ts
// ---- versions ----
type DeleteMarkerMode = "include" | "exclude" | "only";

interface VersionFilter {
  show_versions: boolean;          // false leaves only delete markers (when they are shown)
  delete_markers: DeleteMarkerMode;
  multiple_versions: boolean;      // only keys with more than one entry left
  count_greater_than: number | null; // only keys with more entries left than this
  count_less_than: number | null;  // only keys with fewer entries left than this
  modified_after: string | null;   // inclusive
  modified_before: string | null;  // exclusive
}

interface VersionGroup {           // one key
  key: string;
  versions: ObjectVersion[];       // the entries the filter keeps, newest first, 1000 at most
  total: number;                   // every entry of the key (counted up to 50,000)
  matching: number;                // entries the filter keeps
  is_deleted: boolean;             // the newest entry is a delete marker
  truncated: boolean;              // the key has more entries than are listed or counted
}

interface VersionGroupPage { groups: VersionGroup[]; next_token: string | null; scanned: number }

interface VersionRef { key: string; version_id: string }

interface RestoreOutcome {
  key: string;
  restored_version_id: string;     // the version that was copied
  version_id: string | null;       // the new current version, when the server reports one
  etag: string | null;
  size: number;
}

interface DeleteFailure { key: string; version_id: string | null; code: string | null; message: string }
interface DeleteOutcome { deleted: number; failures: DeleteFailure[] }

// ---- preview ----
type PreviewKind = "text" | "json" | "image" | "binary" | "too_large" | "empty";

interface ObjectPreview {
  key: string;
  version_id: string | null;       // the version that was read, when the bucket reports one
  kind: PreviewKind;
  size: number;                    // of the whole object
  content_type: string | null;     // as stored
  media_type: string | null;       // as recognized: "image/webp", "application/json", "text/plain"
  last_modified: string | null;
  etag: string | null;             // without quotes
  page: number;                    // the page this answer holds, from 0
  page_count: number;              // 1 unless the object is long text
  page_size: number;               // bytes per page
  offset: number;                  // first byte of the object that is shown
  length: number;                  // bytes of the object that are shown
  text: string | null;             // text and JSON
  formatted: boolean;              // JSON that was pretty-printed (its text is not the object's bytes)
  image_base64: string | null;     // images: the whole image
}
```

### Commands

| Command | Arguments | Returns |
|---|---|---|
| `list_versions` | `account_id, bucket, prefix: string, exact: boolean, filter: VersionFilter, token: string \| null` | `VersionGroupPage` |
| `restore_version` | `account_id, bucket, key, version_id` | `RestoreOutcome` |
| `purge_versions` | `account_id, bucket, versions: VersionRef[]` | `DeleteOutcome` |
| `enqueue_download_version` | `account_id, bucket, key, version_id, dest_path: string` | `TransferTask[]` (exactly one task) |
| `preview_object` | `account_id, bucket, key, version_id: string \| null, page: number, page_size: number \| null` | `ObjectPreview` |

### Version list

- `list_versions` lists every key that starts with `prefix` (with `exact: true`, only the key
  `prefix` itself, which must be an object key). Entries are grouped by key; a group is judged
  only when all of its entries have been seen, even when they arrive on several S3 pages. A
  folder marker is a key like any other.
- The filter works in two steps. Each entry is kept or dropped by what it is (a version or a
  delete marker: `show_versions`, `delete_markers`) and by `last_modified`
  (`modified_after <= t < modified_before`; an entry without a time never satisfies a bound).
  Then each key is kept or dropped by how many entries are left: `multiple_versions` (at least
  two), `count_greater_than`, `count_less_than`. A key with no entry left is not listed.
- A page holds `settings.advanced.list_page_size` keys at most (and about 5,000 entries). A call
  sends at most 10 requests to the server and then ends at the next key boundary, so a filter
  that rejects most keys can return an empty page with a `next_token`; `scanned` says how many
  entries the call looked at. `token` continues after the last key that was judged. A key with
  more than 50,000 entries is counted up to that number, listed with `truncated: true`, and
  skipped over on the server.
- Rejected with `usage` (nothing is sent): `show_versions: false` with `delete_markers:
  "exclude"`, an empty date interval (`modified_after >= modified_before`), counts no key can
  satisfy (for example more than 4 and fewer than 5, or several versions and fewer than 2), a
  token that is not one of the server's, `exact` with a prefix that is empty or ends in `/`.

### Restore, purge, download

- `restore_version` copies the version over its key (a server-side copy), so that the copy is
  the new current version and every older version stays. Content headers, user metadata and
  tags come with the copy; the storage class and the SSE-S3 or SSE-KMS encryption (with its key)
  of the version are requested again. `usage`: a delete marker (remove it with `purge_versions`
  instead), an SSE-C version, a version above 5 GiB. `conflict`: the version already is the
  current one, or the server refuses (an archived object that is not restored).
  `not_found`: no such key or version.
- `purge_versions` deletes versions and delete markers for good, 1000 per request; one version
  listed twice counts once. Versions the server refuses (object lock, a missing permission) are
  in `failures` and the rest is deleted; a request that fails as a whole rejects the command.
  Removing a delete marker brings back the previous version. The UI asks for a typed
  confirmation first. MFA delete is not supported.
- `enqueue_download_version` queues one `download` task of the transfer queue: `src` is the
  object (the task has no version field; the version is part of the task's resume state, so
  retries and resumes read the same version), `dst` is the local file `dest_path` (absolute, not
  named `*.bbpart`, replaced when it exists), `bytes_total` is the version's size. All queue
  rules and events apply. `usage`: relative path, folder key, empty version ID, delete marker;
  `not_found`: no such version.

### Preview

- One ranged read per page. Page `n` is the bytes from `n * page_size` on; `page_size` is 16 to
  1,048,576 bytes (`null` is 131,072). Pages after the first are cut on character boundaries so
  that the pages tile the object exactly. A page past the end is `usage`.
- Page 0 decides what the object is, by its content, not its label: an image (PNG, JPEG, GIF,
  WebP, BMP, ICO, AVIF, and SVG when the content type or the key says so) up to 8 MiB comes back
  whole in `image_base64` with its `media_type`; a larger one is `too_large` (with the
  `media_type`, no image). UTF-8 text (no NUL, few control characters) is `text`, and `json`
  when the content type or the key says JSON and it starts with `{` or `[`. A JSON document that
  fits in the first page is validated and pretty-printed with its key order and number spellings
  untouched (`formatted: true`); one that is not valid JSON is `text`; a longer one is paged as it is
  (`formatted: false`). Everything else is `binary`, and a zero-byte object is `empty`
  (`page_count` 1, `length` 0). A byte order mark is left out of the text but counted in
  `length`.
- A client shows `text` as text only, never as markup, and an image from a `blob:` or `data:`
  URL (the content security policy allows both); WebP needs no conversion, the web view draws it.
- `version_id: null` is the current version. `not_found`: no such key or version, or the key's
  current entry is a delete marker; `usage`: a folder key, an empty version ID, a delete marker
  version, a page size out of range; `conflict`: an archived object (restore it first).

## Object metadata (T06)

The properties editor of an object: tags, storage class, server-side encryption,
HTTP headers, user metadata and canned ACLs. Implemented by
`bucketbridge_core::objectmeta` and `crates/desktop/src/commands/objectmeta.rs`.

### Types

```ts
type StorageClass =
  | "STANDARD" | "STANDARD_IA" | "ONEZONE_IA" | "GLACIER_IR"
  | "GLACIER" | "DEEP_ARCHIVE" | "INTELLIGENT_TIERING" | "REDUCED_REDUNDANCY";

type CannedAcl =
  | "private" | "public-read" | "public-read-write" | "authenticated-read"
  | "bucket-owner-read" | "bucket-owner-full-control";

interface AclGrant {
  grantee_type: "canonical_user" | "group" | "email";
  grantee: string;                  // canonical user ID (or display name), group URI, or e-mail address
  permission: "FULL_CONTROL" | "READ" | "WRITE" | "READ_ACP" | "WRITE_ACP";
}

interface ObjectAcl {
  owner: string | null;
  grants: AclGrant[];
  canned: CannedAcl | null;         // the canned ACL the grants equal; null = custom grants
}

interface RestoreStatus { in_progress: boolean; expires: string | null }

// HeadObject + GetObjectTagging + GetObjectAcl in one value.
interface ObjectDetails {
  key: string;
  size: number;
  etag: string | null;
  last_modified: string | null;
  storage_class: string | null;     // always set ("STANDARD" when S3 omits it)
  version_id: string | null;
  checksums: Checksums;
  meta: ObjectMeta;                 // headers, user metadata, sse and object lock from HEAD;
                                    // tags and acl (canned name, or "custom") from the other two calls
  restore: RestoreStatus | null;    // an archived object with a restore request
  archive_status: "ARCHIVE_ACCESS" | "DEEP_ARCHIVE_ACCESS" | null;   // Intelligent-Tiering archive tiers
  acl: ObjectAcl | null;
  tags_error: BbError | null;       // GetObjectTagging failed: meta.tags is {}
  acl_error: BbError | null;        // GetObjectAcl failed: acl and meta.acl are null
}

type TagEdit =
  | { mode: "replace"; tags: Record<string, string> }                     // {} deletes every tag
  | { mode: "merge"; set: Record<string, string>; remove: string[] };      // other tags stay

type HeaderChange = { action: "keep" } | { action: "set"; value: string } | { action: "remove" };

interface HeaderChanges {           // a patch: each object keeps what is not named
  content_type: HeaderChange;
  cache_control: HeaderChange;
  content_disposition: HeaderChange;
  content_encoding: HeaderChange;
  user_meta_set: Record<string, string>;    // keys are not case sensitive; S3 stores them in lowercase
  user_meta_remove: string[];
}

type EncryptionChange =
  | { type: "s3" }
  | { type: "kms"; key_id: string | null; bucket_key: boolean }    // key_id null = the AWS managed key
  | { type: "c"; key: string };     // SSE-C key: 64 hex digits or the base64 of 32 bytes; never stored

interface ObjectChange {            // one per distinct key, in the order of `keys`
  key: string;
  changed: boolean;                 // false when the object was already as wanted, and when it failed
  previous: string | null;          // the old storage class, encryption ("none", "s3", "kms", "c") or ACL
  error: BbError | null;
  warnings: string[];               // trouble after the change, for example an ACL that could not be applied again
}
```

### Commands

| Command | Arguments | Returns |
|---|---|---|
| `get_object_details` | `account_id, bucket, key, sse_c_key: string \| null` | `ObjectDetails` |
| `update_object_tags` | `account_id, bucket, keys: string[], edit: TagEdit` | `ObjectChange[]` |
| `set_object_storage_class` | `account_id, bucket, keys: string[], storage_class: StorageClass, sse_c_key: string \| null` | `ObjectChange[]` |
| `set_object_encryption` | `account_id, bucket, keys: string[], encryption: EncryptionChange, source_sse_c_key: string \| null` | `ObjectChange[]` |
| `update_object_headers` | `account_id, bucket, keys: string[], changes: HeaderChanges, sse_c_key: string \| null` | `ObjectChange[]` |
| `set_object_acl` | `account_id, bucket, keys: string[], acl: CannedAcl` | `ObjectChange[]` |

Rules:

- The batch commands work on up to 10,000 distinct keys, 8 at a time. A problem
  with one object (missing, archived, access denied, a wrong SSE-C key) is that
  object's `error` and never stops the others; the command itself rejects only
  when nothing could be attempted: an empty or oversized `keys`, an invalid tag,
  header, class, ACL or SSE-C key (`usage`), an unknown account, locked secrets.
  A key ending in `/` is a folder: it fails with `usage` and is not expanded.
  An object that already is as wanted is `changed: false` and nothing is sent.
- Tags: S3's rules apply (at most 10 tags, keys of 1 to 128 and values of at most
  256 characters made of letters, digits, spaces and `+ - = . _ : / @`, no `aws:`
  prefix). A merge that would leave an object with more than 10 tags fails for
  that object only.
- Headers: values are printable ASCII; user metadata is at most 2048 bytes in
  total (keys and values, UTF-8) after the patch, checked per object. Removing
  `content_type` leaves S3's default, `binary/octet-stream`.
- Storage class, encryption and header changes copy the object onto itself (the
  only way S3 offers). The copy keeps content headers, user metadata, tags, the
  storage class and encryption that were not changed, the website redirect and
  the ACL (read first and applied again when it is not the default one; a failure
  there is a `warnings` entry). The object gets a new modification time and, on a
  versioned bucket, a new version. The source is pinned by ETag: an object that
  is overwritten meanwhile fails with `conflict` and nothing is replaced. Objects
  above 5 GiB are copied in parts. An archived object that is not restored
  (`GLACIER`, `DEEP_ARCHIVE`, an Intelligent-Tiering archive tier) fails with
  `conflict` without any copy being sent.
- SSE-C: the key travels with the command (`sse_c_key` is the key an SSE-C object
  has now; for `set_object_encryption` it is `source_sse_c_key`, and the new key
  is part of `encryption`). It is never stored, logged or returned; the host
  zeroizes it. Reading an SSE-C object needs its key: without it
  `get_object_details` rejects with `remote` (HTTP 400) and a message about
  SSE-C, and a wrong key is refused by S3 (HTTP 403, `auth`) with a message that
  names SSE-C. Edits report the same as that object's `error`; nothing is
  changed. Leave the key `null` for objects that are not encrypted with one: S3
  rejects a key for them too.
- ACLs: `set_object_acl` on a bucket with ACLs disabled (Object Ownership
  BucketOwnerEnforced) fails per object with `conflict` and a message that says
  so; setting `private` there is `changed: false`.

Errors: `usage` (invalid input), `not_found` (bucket or object), `conflict`
(archived object, concurrent change, ACLs disabled), `auth` (access denied, SSE-C
key refused), `remote`, plus the account errors of the other commands (`locked`,
`mfa_required`, `not_found` for the account).

## Bucket rules (T04/T07)

Lifecycle, Intelligent-Tiering, CORS and replication of a bucket
(`crates/core/src/bucketrules.rs`, `crates/desktop/src/commands/bucketrules.rs`).
Every command takes `account_id` and `bucket`, except the `validate_*` commands,
which need no account.

| Command | Arguments | Result |
| --- | --- | --- |
| `get_bucket_rules` | | `BucketRules` |
| `get_bucket_lifecycle` / `get_bucket_cors` / `get_bucket_replication` | | the config, or `null` when none |
| `put_bucket_lifecycle` / `put_bucket_cors` / `put_bucket_replication` | `config` | `null` |
| `delete_bucket_lifecycle` / `delete_bucket_cors` / `delete_bucket_replication` | | `null` (also when none) |
| `list_bucket_tiering` | | `TieringConfig[]`, sorted by id |
| `put_bucket_tiering` | `config` | `null` (creates or replaces the id) |
| `delete_bucket_tiering` | `id` | `null` (also when the id is unknown) |
| `validate_bucket_lifecycle` / `_cors` / `_tiering` / `_replication` | `document` (JSON text) | `RuleIssue[]`, empty when valid |

`BucketRules` has `cors`, `lifecycle`, `tiering` and `replication`, each a
`Section` (`loaded`, `unsupported` or `failed`, as in `BucketConfig`), so a
server that does not implement one setting never hides the others.
`RuleIssue` is `{ path, message }`; `path` is such as
`rules[1].transitions[0].days`, empty for the document as a whole (not JSON, an
unknown field, a wrong type).

Documents (snake_case, unknown fields are rejected, omitted fields take their
defaults):

- `LifecycleConfig { rules: LifecycleRule[] }`; a rule has `id`, `status`
  (`enabled`/`disabled`), `filter { prefix, tags[{key,value}],
  object_size_greater_than, object_size_less_than }`, `transitions[{ days | date
  (YYYY-MM-DD), storage_class }]`, `expiration { days | date |
  expired_object_delete_marker }`, `noncurrent_version_transitions[{
  noncurrent_days, newer_noncurrent_versions, storage_class }]`,
  `noncurrent_version_expiration { noncurrent_days, newer_noncurrent_versions }`
  and `abort_incomplete_multipart_upload_days`.
- `TieringConfig { id, status, filter { prefix, tags }, tierings[{ access_tier:
  "ARCHIVE_ACCESS" | "DEEP_ARCHIVE_ACCESS", days }] }`.
- `CorsConfig { rules[{ id, allowed_origins, allowed_methods, allowed_headers,
  expose_headers, max_age_seconds }] }`.
- `ReplicationConfig { role, rules[{ id, priority, status, filter { prefix,
  tags }, destination_bucket (ARN), storage_class, delete_marker_replication }] }`.

Every `put_*` validates first and rejects with `usage` listing all problems
(`invalid lifecycle configuration: rules[0]...; ...`); nothing is sent then.
`put_bucket_replication` also reads the versioning of the source and rejects
with `usage` ("replication needs versioning on the source bucket, and
versioning of 'b' is not enabled; enable versioning in the bucket properties
first") unless it is enabled. Replication settings the model does not carry
(replica modification sync, KMS source selection, encryption of replicas,
replication time control, metrics, ACL translation, existing object
replication) are copied from the rule with the same id that the bucket has now.
Errors: `usage`, `not_found` (bucket), `auth`, `remote` (including servers
without the API), plus the account errors of the other commands.

## CloudFront (T09)

CloudFront is a global AWS service. Every command below needs an account without a custom
endpoint (otherwise it rejects with `usage` and nothing is sent). Requests are signed with the
account's credentials (the MFA session's when it has one) and go through the app's proxy.

### Types

```ts
interface CloudFrontOrigin {
  id: string;
  domain_name: string;
  origin_path: string;              // "" for none
  bucket: string | null;            // the S3 bucket of an S3 domain name, lowercase
  website: boolean;                 // an S3 website endpoint
  access_control: boolean;          // signed with an origin access control
}

interface CloudFrontDistribution {
  id: string;
  arn: string;
  domain_name: string;              // *.cloudfront.net
  status: string;                   // "Deployed" or "InProgress"
  enabled: boolean;
  comment: string;
  aliases: string[];
  price_class: string;              // PriceClass_All | PriceClass_200 | PriceClass_100
  last_modified: string | null;
  origins: CloudFrontOrigin[];
  default_root_object: string | null;        // null in a listing
  in_progress_invalidations: number | null;  // null in a listing
  etag: string | null;                       // null in a listing; pass back as expected_etag
}

interface CloudFrontPage { items: CloudFrontDistribution[]; next_marker: string | null; }

interface NewDistribution {
  bucket: string; region: string;   // region of the bucket: the origin is its regional endpoint
  comment: string;                  // at most 128 characters
  enabled: boolean;
  default_root_object: string;      // no leading "/"; "" for none
  price_class: string | null;       // null = PriceClass_All
}
interface CreatedDistribution {
  distribution: CloudFrontDistribution;
  origin_access_control_id: string;
  bucket_policy: string;            // add to the bucket's policy; the bucket is not changed
}
interface DistributionEdit {        // null / missing = unchanged
  comment?: string | null; aliases?: string[] | null; default_root_object?: string | null;
  price_class?: string | null; enabled?: boolean | null;
}

interface CloudFrontInvalidation {
  id: string;
  status: string;                   // "InProgress" or "Completed"
  create_time: string | null;
  paths: string[];                  // empty in a history listing
}
interface InvalidationPage { items: CloudFrontInvalidation[]; next_marker: string | null; }
interface PathSet { name: string; paths: string[]; }
```

### Commands

| Command | Arguments | Returns |
|---|---|---|
| `list_cloudfront_distributions` | `account_id, marker: string \| null` | `CloudFrontPage` |
| `get_cloudfront_distribution` | `account_id, id` | `CloudFrontDistribution` |
| `find_cloudfront_distributions` | `account_id, bucket` | `CloudFrontDistribution[]` |
| `create_cloudfront_distribution` | `account_id, new: NewDistribution` | `CreatedDistribution` |
| `update_cloudfront_distribution` | `account_id, id, edit: DistributionEdit, expected_etag: string \| null` | `CloudFrontDistribution` |
| `set_cloudfront_enabled` | `account_id, id, enabled: boolean, expected_etag: string \| null` | `CloudFrontDistribution` |
| `detach_cloudfront_bucket` | `account_id, id, bucket, expected_etag: string \| null` | `CloudFrontDistribution` |
| `create_cloudfront_invalidation` | `account_id, distribution_id, paths: string[]` | `CloudFrontInvalidation` |
| `list_cloudfront_invalidations` | `account_id, distribution_id, marker: string \| null` | `InvalidationPage` |
| `get_cloudfront_invalidation` | `account_id, distribution_id, invalidation_id` | `CloudFrontInvalidation` |
| `validate_invalidation_paths` | `paths: string[]` | `string[]` |
| `list_invalidation_path_sets` | none | `PathSet[]` |
| `save_invalidation_path_set` | `set: PathSet, replaces: string \| null` | `PathSet[]` |
| `delete_invalidation_path_set` | `name` | `PathSet[]` |

Rules:

- Edits are read-modify-write of the distribution configuration with `If-Match`. When
  `expected_etag` is given and the distribution has changed since, the command rejects with
  `conflict` and sends nothing; a `PreconditionFailed` answer from the service is a `conflict` too.
  Everything the editor does not manage is sent back unchanged.
- Invalidation paths: each starts with `/`, `*` is allowed only as the last character, no blank or
  control character (use `%20`), at most 4000 bytes each and 3000 paths. Paths are trimmed and
  repeats dropped. `usage` lists every problem. `TooManyInvalidationsInProgress` is `conflict`.
- `detach_cloudfront_bucket` removes the origins of the bucket; it rejects with `conflict` when it
  would leave no origin or a behaviour or origin group still uses one, and with `not_found` when the
  bucket is not an origin.
- UI use: the bucket menu entries `cloudfront.attach` (create), `cloudfront.edit` (update) and
  `cloudfront.detach` (detach) all open the CloudFront manager on the distributions of the bucket.
  Attach calls `create_cloudfront_distribution` and shows the returned `bucket_policy`; edit reads the
  distribution with `get_cloudfront_distribution` and passes its `etag` to `update_cloudfront_distribution`;
  detach (typed confirmation of the bucket name) removes the bucket's origins with
  `detach_cloudfront_bucket` and never deletes the distribution.
- `create_cloudfront_distribution` makes an origin access control, the regional bucket endpoint as
  the only origin and a default behaviour (HTTPS redirect, compression, managed CachingOptimized).
- Path sets live in `config.toml` (names unique ignoring case, at most 100); `conflict` for a
  duplicate name or too many, `not_found` for an unknown name.
- `list_buckets` marks AWS buckets that have a distribution with `kind: "cloudfront"`: the account's
  distributions are listed once per session on the first listing (never when a menu opens), a failure
  leaves the kinds as they are, and an account that may not list distributions is remembered as
  having none.

Errors: `usage`, `not_found` (distribution, invalidation, path set), `conflict`, `auth`, `remote`,
plus the account errors of the other commands.

## Object Lock, ownership, restore and MFA delete (T07)

Core: `bucketbridge_core::objectlock`. Batch commands return one `ObjectChange` per key (`key`, `changed`, `previous`, `error`, `warnings`); a refusal by the server (a locked object) is that key's `error`, with the server's code and message as sent.

Shapes: `LockMode` is `"governance"` or `"compliance"`. `DefaultRetentionRule` is `{ mode, days, years }` (exactly one of `days`, `years`). `BucketObjectLock` is `{ enabled, default_retention }`. `ObjectLockState` is `{ mode, retain_until, legal_hold }`. `RetentionChange` is `{ "action": "set", "mode", "retain_until" }` (RFC 3339, in the future) or `{ "action": "clear" }`. `ObjectOwnership` is `"BucketOwnerEnforced"`, `"ObjectWriter"` or `"BucketOwnerPreferred"`. `RestoreTier` is `"expedited"`, `"standard"` or `"bulk"`. `ArchiveState` is `{ storage_class, archived, restore }` with `restore` a `RestoreStatus` or `null`.

| Command | Arguments | Result |
|---|---|---|
| `get_bucket_object_lock` | `account_id`, `bucket` | `BucketObjectLock` (`enabled: false` for a bucket without Object Lock) |
| `put_bucket_object_lock` | `account_id`, `bucket`, `default_retention: DefaultRetentionRule \| null` | `null`; turns Object Lock on, `null` clears the default retention |
| `get_object_lock` | `account_id`, `bucket`, `key`, `version_id` | `ObjectLockState` |
| `set_object_retention` | `account_id`, `bucket`, `keys`, `change`, `version_id`, `bypass_governance` | `ObjectChange[]` (`x-amz-bypass-governance-retention` when `bypass_governance`) |
| `set_object_legal_hold` | `account_id`, `bucket`, `keys`, `on`, `version_id` | `ObjectChange[]` (`previous` is `"on"` or `"off"`) |
| `get_object_ownership` | `account_id`, `bucket` | `ObjectOwnership` or `null` when none is set |
| `put_object_ownership` | `account_id`, `bucket`, `ownership` | `null` |
| `restore_objects` | `account_id`, `bucket`, `keys`, `days` (1 to 3650), `tier`, `version_id` | `ObjectChange[]` (accepted means requested; a running restore is `conflict`) |
| `get_restore_state` | `account_id`, `bucket`, `key`, `version_id` | `ArchiveState` from `HeadObject` (`x-amz-restore`) |
| `put_bucket_versioning_mfa` | `account_id`, `bucket`, `status`, `mfa_delete`, `mfa_code` | `null`; sends `x-amz-mfa` with the account's `mfa_serial` and the six-digit code (`usage` without a serial) |
| `purge_versions_with` | `account_id`, `bucket`, `versions`, `bypass_governance`, `mfa_code` | `DeleteOutcome` like `purge_versions`; retained versions are in `failures` |

The website redirect is `meta.website_redirect` of the object details and `website_redirect: HeaderChange` of `HeaderChanges` (`/path` or an `http(s)://` URL; others are `usage`).

## IAM (T02)

The IAM manager of the active AWS account. Every command takes `account_id` and
fails first with `locked` or `mfa_required`. An account with a custom endpoint has
no IAM: `iam_status` answers `{ supported: false, message: "IAM is not available for
this endpoint" }` and every other command rejects with `usage` and that message.
Calls use the account's credentials (or its MFA session), the proxy-aware HTTP
client and the read timeout of the settings. Every list follows the IAM `Marker`
until the list is complete (a repeated marker fails with `remote`).

| Command | Arguments | Result |
|---|---|---|
| `iam_status` | `account_id` | `{ supported, message }` (no network call) |
| `iam_list_users` | `account_id` | `IamUser[]` |
| `iam_create_user` | `account_id, name, path?` | `IamUser` |
| `iam_update_user` | `account_id, name, new_name?, new_path?` | `IamUser` |
| `iam_delete_user` | `account_id, name, confirm` | `{ user_name, steps: [{ kind, target, ok, error }], deleted }` |
| `iam_user_details` | `account_id, name` | `{ user, managed_policies, inline_policies, access_keys, login_profile, groups }`, each part a `{ type: loaded \| unsupported \| failed, ... }` section |
| `iam_list_managed_policies` | `account_id, scope` (`local`, `aws`, `all`) | `{ name, arn, attachment_count, description }[]` |
| `iam_attach_policy`, `iam_detach_policy` | `account_id, user, policy_arn` | `null` |
| `iam_get_inline_policy` | `account_id, user, name` | `{ name, document }` (pretty-printed) |
| `iam_put_inline_policy` | `account_id, user, name, document` | `null` (validated first, `usage` lists every problem) |
| `iam_delete_inline_policy` | `account_id, user, name, confirm` | `null` |
| `iam_validate_policy`, `iam_format_policy` | `document` | `PolicyIssue[]`, pretty text (no account, no network) |
| `iam_create_access_key` | `account_id, user` | `{ user_name, access_key_id, secret_access_key, created }` |
| `iam_list_access_keys` | `account_id, user` | `{ access_key_id, status: active \| inactive, created }[]` |
| `iam_set_access_key_active` | `account_id, user, access_key_id, active` | `null` |
| `iam_delete_access_key` | `account_id, user, access_key_id, confirm` | `null` |
| `iam_create_login_profile` | `account_id, user, password, reset_required` | `null` |
| `iam_update_login_profile` | `account_id, user, password?, reset_required?` | `null` |
| `iam_delete_login_profile` | `account_id, user, confirm` | `null` |

`IamUser` is `{ name, arn, user_id, path, created, password_last_used }`.

- Typed confirmation: `confirm` of `iam_delete_user` and `iam_delete_login_profile`
  must equal the user name, of `iam_delete_inline_policy` the policy name, of
  `iam_delete_access_key` the access key ID. The host refuses with `usage`
  otherwise, so no client can skip the dialog's confirmation.
- `iam_delete_user` removes the access keys, inline policies, attached managed
  policies, login profile and group memberships first and reports each step; a
  failed step does not stop the others, but the user is only deleted
  (`deleted: true`) when every step worked.
- The secret of `iam_create_access_key` exists only in that answer. It is never
  stored, logged or persisted, the core zeroizes its copy, and the UI shows it
  once (copy goes through the clipboard `writeSecret`). Passwords are never
  logged either.

Errors: `usage` (invalid input, wrong confirmation, no IAM for the endpoint),
`not_found` (`NoSuchEntity`), `conflict` (`EntityAlreadyExists`, `DeleteConflict`,
quota), `auth`, `remote`, plus the account errors of the other commands.
## Security scan (T13)

| Command | Arguments | Returns |
|---|---|---|
| `run_security_scan` | `account_id: string`, `buckets: string[] \| null` | `ScanReport` |
| `export_scan_report` | `report: ScanReport`, `format: "csv" \| "json"`, `path: string` (absolute) | `void` |

```ts
type ScanSeverity = "low" | "medium" | "high" | "critical";
type ScanFixTab = "policy" | "public-access" | "versioning" | "encryption" | "ownership";
interface ScanFinding {
  id: "public_acl" | "public_policy" | "public_access_block_off" | "public_access_block_partial"
    | "no_default_encryption" | "versioning_off" | "versioning_suspended" | "acls_enabled";
  severity: ScanSeverity;
  title: string;
  explanation: string;          // what is wrong, why it matters, what to do
  fix_tab: ScanFixTab;          // the bucket Properties tab that fixes it
}
interface ScanSkippedCheck { check: "acl" | "policy" | "public_access_block" | "encryption" | "versioning" | "ownership"; unsupported: boolean; message: string }
interface BucketScan { bucket: string; findings: ScanFinding[]; skipped: ScanSkippedCheck[]; error: BbError | null }
interface ScanReport { generated_at: string; buckets: BucketScan[] }   // RFC 3339, UTC
```

- `buckets: null` scans every bucket of the account; names are checked
  (`usage`), duplicates are scanned once, and an empty list is `usage`. Up to
  four buckets are scanned at a time.
- The scan only reads (`GetBucketAcl`, `GetBucketPolicyStatus`, `GetBucketPolicy`,
  `GetPublicAccessBlock`, `GetBucketEncryption`, `GetBucketVersioning`,
  `GetBucketOwnershipControls`). A check that fails (access denied, an
  unimplemented API) is a `skipped` entry of that bucket and never hides the other
  findings; a bucket that cannot be reached at all has `error`. The command rejects
  only for the account errors (`not_found`, `locked`, `mfa_required`), `usage`, and
  when `buckets` is `null` and the bucket list cannot be read.
- Severities: a public ACL is critical (write access for everyone), high (read for
  everyone, write for any AWS account) or medium (read for any AWS account); a public
  bucket policy is critical; public access block off is high, partly on medium;
  missing default encryption is medium; versioning off or suspended and ACLs enabled
  are low. `fix_tab` values `policy`, `public-access` and `versioning` are tabs of the
  bucket properties dialog; `encryption` and `ownership` have no editor yet.
- `export_scan_report` writes a CSV (`bucket,severity,finding,title,fix_tab,explanation`,
  then one row per skipped check or failed bucket) or the report as JSON, replacing
  the file atomically; a relative path is `usage`.

### URL shorteners and mount (T13, T04)

The settings carry `shortener: { provider: "none" | "bitly" | "tinyurl", token_ref }`
(`none` by default: nothing is ever sent to a third party). The token is kept in the
secret store like the proxy password (`AccountManager::set_shortener_token`, owner
`shortener-<installation id>`) and `save_settings` keeps `token_ref`. The core can
shorten a link (`bucketbridge_core::shorten`); see "Sharing wizard and URL shorteners"
at the end of this document for the commands and the share-dialog button. Mounting a
bucket as a drive is an optional build feature on Linux, macOS and Windows; see
"Mount as drive (T04)" at the end of this document.
## Client-side encryption (T07)

Objects are compressed (optional) and encrypted on this machine with AES-256-GCM
under an Argon2id key; the server only sees ciphertext. The envelope format is in
`docs/security.md`. Encrypted objects carry the user metadata `x-amz-meta-bb-cse`
(for example `bbcse1;aes-256-gcm;zstd`); the authoritative test is the envelope
header at the start of the data. Passwords are never logged or stored; they cross
IPC once per command and the host keeps them in memory only, for the tasks of one
enqueue call.

Types: `CseCompression = "none" | "gzip" | "zstd"`;
`ObjectCse { encrypted, marked, version, compression, chunk_size }` (the last
three `null` when not encrypted); `TransformOutcome { size_before, size_after,
etag }`.

| Command | Arguments | Result |
|---|---|---|
| `cse_probe_object` | `account_id, bucket, key` | `ObjectCse` (reads the first 57 bytes and the metadata) |
| `cse_check_password` | `account_id, bucket, key, password: string \| null` | `ObjectCse`; `auth` and `usage` as below |
| `cse_encrypt_object` | `account_id, bucket, key, password, compression: CseCompression \| null` | `TransformOutcome` |
| `cse_decrypt_object` | `account_id, bucket, key, password` | `TransformOutcome` |
| `enqueue_upload_encrypted` | as `enqueue_upload` plus `password`, `compression` | `TransferTask[]` |
| `enqueue_download_decrypted` | as `enqueue_download` plus `password: string \| null` | `TransferTask[]` |

- The two password states are `auth` errors told apart by the message prefix:
  `password not found` (the object is encrypted and the password is `null` or
  empty) and `password not valid` (the password does not open it). Both are
  detected from the header before any body is downloaded. Modified, truncated or
  reordered data fails with `io` and a message that mentions the integrity check.
  Not encrypted (decrypt) is `usage`; already encrypted (encrypt) is `conflict`.
- `cse_encrypt_object` and `cse_decrypt_object` download the object, transform
  the file in a temporary folder (streaming, one extra copy on disk) and upload
  it under the same key, conditional on the ETag that was downloaded. Content
  type, `Cache-Control`, `Content-Disposition`, `Content-Encoding` (kept in
  `bb-cse-content-encoding` while encrypted), user metadata and storage class are
  kept; tags, ACL, `Content-Language` and `Expires` are not. Folders are `usage`.
- Encrypting tasks have the group ID `cse-enc-<compression>-<uuid>`, decrypting
  ones `cse-dec-<uuid>`. A task of such a group whose password is unknown (after a
  restart) fails with `password not found` and never runs in the clear. An
  encrypted upload is not resumed (a new run has new salt and nonces): a saved
  multipart upload is aborted and the file is encrypted again. A decrypting
  download of an object that is not encrypted saves it as it is; if decryption
  fails after the download the file is removed.

Errors: `usage`, `conflict`, `not_found`, `auth`, `io`, `remote`, plus the account
errors of the other commands.

## Destination picker (T05)

No new commands. The picker dialog (`ui/src/features/picker/`) chooses a bucket and
folder with `list_buckets`, `list_objects` (`flat: false`) and `create_folder`, then
queues the work with the existing `enqueue_copy` or `enqueue_move`. These back the
menu actions `file.copy-to`, `file.move-to`, `folder.copy-all`, `folder.move-all`
(picker) and `bucket.paste` (pastes the clipboard into the bucket root).

- The copy engine works inside one account, so the picker is fixed to the account
  of the source and cannot pick a destination in another account.
- A folder cannot be copied or moved into itself or one of its subfolders, and
  sources that would land on the same destination name are refused. The picker
  disables its confirm button and shows the reason; the commands themselves answer
  with `usage` or `conflict` as before.
## Bookmarks and windows (T14)

Bookmarks are saved locations kept in `config.toml` (`[[bookmarks]]`). They are
read and written without the secrets, so they work while locked.

```ts
interface Bookmark {
  id: string;          // random, stable across renames
  account_id: string;
  bucket: string;
  prefix: string;      // "" for the bucket, a folder prefix, or an object key
  label: string;       // 1..100 characters, no control characters
}
```

| Command | Arguments | Result |
|---|---|---|
| `list_bookmarks` | none | `Bookmark[]`, in the order they were added |
| `add_bookmark` | `account_id, bucket, prefix, label: string \| null` | the stored `Bookmark[]` |
| `rename_bookmark` | `id, label` | the stored `Bookmark[]` |
| `remove_bookmark` | `id` | the stored `Bookmark[]` |
| `open_new_window` | `location: { account_id, bucket, prefix } \| null` | the label of the new window (`main-1`, `main-2`, ...) |

- `label: null` (or blank) on `add_bookmark` uses `bucket` or `bucket/prefix`.
  The label is trimmed. The same location can be bookmarked once per account;
  at most 500 bookmarks are stored.
- Errors: `usage` (empty bucket, empty or invalid label), `not_found` (unknown
  account or bookmark id), `conflict` (location already bookmarked, or the
  limit reached).
- Removing an account (`remove_account`) removes its bookmarks.
- Event `bookmarks://changed`, payload the full `Bookmark[]`, is emitted after
  every successful add, rename or remove, to every window.
- `open_new_window` opens another main window of the same process: the session,
  transfer queue, stores and events are shared, and every event reaches every
  window. The new window has the size of the calling window (at least
  960 x 600) and sits 32 points below and to the right of it. With a
  `location` its URL carries `bb_account`, `bb_bucket` and `bb_prefix`
  (percent-encoded) and the UI opens that account and folder once it has
  loaded; an unknown account falls back to a normal start. A `location` without
  `account_id` or `bucket` is rejected by the IPC layer. Closing a window does
  not stop the app; closing the last one does. Window size and position are not
  restored between runs.
## Mount as drive (T04)

A read-only mount of a bucket (or one prefix): FUSE on Linux and macOS, Dokan on Windows
(a drive letter or a folder). It exists only in
builds made with the cargo feature `mount` (`cargo build --features mount`, see
`docs/mount.md`). Every build has the three commands; a build without the feature
answers `mount_status` with `supported: false` and rejects `mount_bucket` with `usage`.

- `mount_status() -> MountStatus` with
  `{ supported, fuse_installed, platform, drive_letters, mounts }`.
  `supported` is a property of the build and the OS; `fuse_installed` is a run-time
  check of the filesystem driver (the FUSE device and `fusermount` on Linux, macFUSE or
  FUSE-T on macOS, the Dokan 2 driver and `dokan2.dll` on Windows) and is
  always `false` when `supported` is `false`. `platform` is `"linux"`, `"macos"`,
  `"windows"` (Rust `std::env::consts::OS`). `drive_letters` lists the free drive
  letters (`["D", "E", ...]`) on Windows and is `[]` elsewhere. Mounts whose session ended (unmounted
  from outside) are dropped from the list.
- `mount_bucket({ account_id, bucket, prefix: string | null, mount_point }) -> MountInfo`
  with `MountInfo = { account_id, bucket, prefix, mount_point }` (`prefix` is `""` for
  the whole bucket). `mount_point` must be an absolute path of an existing folder; on
  Windows it may also be a free drive letter (`"X"`, `"x:"` or `"X:\"`), which is
  returned normalised as `"X:"` in `MountInfo.mount_point` (`usage` otherwise, also for
  a bad bucket name, an unsupported build, a missing driver installation or a drive
  letter that is in use); mounting the same mount point twice is `conflict`. The mount
  is read-only: writes fail with `EROFS` (`STATUS_MEDIA_WRITE_PROTECTED` on Windows), reads are ranged requests with a small block
  cache, directories are listed lazily and cached for 30 seconds.
- `unmount_bucket({ mount_point })` unmounts; `not_found` when the mount point is not
  one of the app's mounts (pass `MountInfo.mount_point`). All mounts end when the app exits.

Event `mount://changed` with `{ mounts: MountInfo[] }` after every mount and unmount.

The UI offers "Mount as drive" in the bucket menu only when `mount_status().supported`
is `true` (asked once at startup).

Errors: `usage`, `conflict`, `not_found`, plus the account errors of the other commands.
## Sharing wizard and URL shorteners (T10, T13)

The bucket menu entry "Sharing wizard..." opens a three-step dialog (object, access,
link) for one bucket. It adds no command of its own: it lists the bucket
(`list_objects`, flat, first page) to suggest keys and creates the link with `presign`
(GET or PUT, 1 second to 7 days). The same dialog step and the existing "Share link"
dialog offer a **Shorten** button next to the link.

Shortening is off by default and sends the link to an outside service, so the UI states
that the whole link, including the signature, leaves the machine and names the service.
The service is chosen on the Sharing page of the settings: `settings.shortener` is
`{ provider: "none" | "bitly" | "tinyurl", token_ref: string | null }` (`none` by
default). Nothing is sent until Shorten is pressed. When the provider is `none` the
dialogs show no button, only a link to the Sharing page.

| Command | Arguments | Result |
|---|---|---|
| `shorten_url` | `url: string` | the short link (`string`) |
| `set_shortener_token` | `token: string \| null` | `void` (stores or deletes the API token in the secret store; `save_settings` keeps `token_ref`) |

`shorten_url` errors: `usage` (no provider chosen, Bitly without a token, a link that is
not `http(s)` or longer than 8192 bytes; nothing is sent), `auth` (the service rejected
the token or the proxy rejected its credentials), `remote` (rate limit, outage, an
unusable answer), `locked` (the stored token is behind the master password). The token
and the link are never logged and no error echoes either. Requests use the proxy-aware
HTTP client of the app. The token crosses IPC once per command and `url` is zeroized
after the call. The short link of a service keeps redirecting after a presigned link
expires, but the target then fails.
## Opening objects and buckets (T14)

| Command | Args | Result |
|---|---|---|
| `open_object_with` | `account_id, bucket, key, app: string` | `void`. Downloads like `open_object`, then opens the copy with `app` (an `.app` bundle, `.exe` or other executable chosen by the user) and records `app` in `settings.general.open_with_recent` |
| `open_bucket_in_browser` | `account_id, bucket, region: string \| null` | `string`, the URL that was opened in the system browser (virtual-hosted style on AWS, path-style aware for a custom endpoint) |

- `open_object_with`: a folder key or a blank `app` is `usage`; an application
  that cannot be started is `io` and is not recorded; download errors as in
  `open_object`.
- `open_bucket_in_browser`: an unknown account is `not_found`. `region` is the
  bucket's region from the bucket list; `null` means `us-east-1`.
- "Website redirect" needs no new command: the dialog reads
  `ObjectDetails.meta.website_redirect` from `get_object_details` and writes it
  with `update_object_headers` (`changes.website_redirect`: `set` with a `/path`
  or `http(s)://` URL, or `remove`).
- Native picker for the application: the dialog plugin's `open` (filters: `app`
  on macOS, `exe`, `bat`, `cmd`, `com` on Windows, none on Linux).

Errors: `usage`, `not_found`, `io`, plus the account errors of the other commands.

## Edit in place (v0.2.0)

Open an object, edit a local copy and upload every save. The core module
`bucketbridge_core::edit` does the work (see "Edit in place" in desktop.md); the
commands and the event are its contract.

### Types

```ts
type EditState = "watching" | "uploading" | "uploaded" | "conflict" | "error" | "stopped";

interface EditConflict {
  remote_deleted: boolean;         // the object no longer exists
  remote_etag: string | null;      // the ETag it has now, without quotes
  remote_modified: string | null;  // RFC 3339
}

interface EditSession {
  id: string;
  account_id: string;
  bucket: string;
  key: string;
  local_path: string;              // absolute path of the local copy
  state: EditState;
  etag: string | null;             // the ETag the next upload expects to find
  version_id: string | null;       // version at open time, on versioned buckets
  started_at: string;              // RFC 3339
  last_upload_at: string | null;
  uploads: number;                 // saves uploaded so far
  error: string | null;            // why the last upload failed (state "error")
  conflict: EditConflict | null;   // set while state is "conflict"
}

type EditResolution = "overwrite" | "save_copy" | "discard" | "retry";

interface EditResolved {
  session: EditSession | null;     // null when the resolution ended the session
  copy_key: string | null;         // the key of the copy, for "save_copy"
}
```

### Commands

| Command | Args | Result |
|---|---|---|
| `start_edit` | `account_id, bucket, key, app: string \| null` | `EditSession`. Downloads the current version of the object into a private folder under the data folder, starts watching the copy and opens it with the default application, or with `app` (as `open_object_with`; the application is added to `settings.general.open_with_recent`). An object that already has a session returns that session and opens its copy again. |
| `list_edits` | none | `EditSession[]`, oldest first |
| `stop_edit` | `id, discard: boolean \| null` | `void`. Uploads a save that is still pending, then ends the session and deletes the copy. With `discard: true` local changes are dropped and nothing is uploaded. |
| `resolve_edit_conflict` | `id, resolution` | `EditResolved` |

Errors:

- `start_edit`: `usage` for a folder key or a client-side encrypted object (a
  blank `app` means the default application); `not_found` for a missing object or bucket; `io` when the application cannot
  be started (the session is dropped and nothing stays on disk); download errors
  as in `open_object`; the account errors of the other commands.
- `stop_edit`: `not_found` for an unknown id; `conflict` when the session is in
  conflict (resolve it, or pass `discard: true`); the upload's own error when the
  pending save cannot be uploaded (the session stays). A conflict found while
  uploading the pending save is also `conflict`.
- `resolve_edit_conflict`: `not_found` for an unknown id; `usage` when the
  session is not in the state the resolution needs (`overwrite` and `save_copy`
  need `conflict`, `retry` needs `error`, `discard` needs `conflict` or `error`);
  `conflict` when the object changed again during an overwrite.

### Behaviour

- **Saves.** The copy's folder is watched, so saves that write in place and
  saves that rename a new file over the old one are both seen. A save is
  uploaded when the file has been quiet for one second and its content differs
  from what was last uploaded.
- **Conflict check.** Before every upload the object is read with `HeadObject`.
  When its ETag is not `EditSession.etag`, or the object is gone, nothing is
  uploaded and the session is `conflict` (`conflict` is set) until the user
  resolves it. The upload carries `If-Match` with that ETag where the server
  supports conditional writes (`501` is remembered for the session and the
  upload is repeated without it; `412` is a conflict). The object is never
  overwritten without `overwrite`.
- **What an upload keeps:** content type, `Cache-Control`,
  `Content-Disposition`, `Content-Encoding`, user metadata, storage class and
  tags. ACLs, the SSE-KMS key, Object Lock settings, `Expires` and
  `Content-Language` are not carried over.
- **Resolutions.** `overwrite` uploads over the object (a deleted object is
  created again, with `If-None-Match: *`). `save_copy`
  uploads to `name (edited).ext`, `name (edited 2).ext`, ... (the first key that
  does not exist; an existing key is never overwritten) and ends the session.
  `discard` ends the session and deletes the copy. `retry` repeats a failed
  upload.
- **Versioned buckets** always edit the current version; every upload creates a
  new current version.
- **Ending.** `stop_edit`, `resolve_edit_conflict` with `save_copy` or
  `discard`, and app exit end a session. At exit pending saves are uploaded
  first; a session that cannot upload (conflict or error) keeps its copy on disk
  and is logged. The copy of a session that ended cleanly is deleted. Folders
  of sessions that never ended are deleted at a later start once nothing in
  them was changed for a week.

### Event

| Event | Payload | When |
|---|---|---|
| `edit://changed` | `EditSession` | Every change of a session: started (`watching`), `uploading`, `uploaded`, `conflict`, `error`; and once with `state: "stopped"` when it ends (it is no longer listed by `list_edits`). |

## Transfers: overwrite policy, modification times, cross-account copy (v0.2.0)

### Settings

```ts
settings.transfer.if_exists: "ask" | "overwrite" | "skip" | "overwrite_if_newer" | "rename"; // "overwrite"
settings.data_integrity.preserve_mtime: boolean;                                            // true
```

`preserve_mtime`: uploads store the local modification time as the user
metadata `x-amz-meta-mtime` (Unix seconds, with a fraction when there is one,
`1700000000.123`); downloads set the file's time to that value, else to the
object's `Last-Modified`. Old files without these fields load with the defaults.

### Changed commands

All new parameters are optional (`null` or absent means the setting).

| Command | New parameters |
|---|---|
| `enqueue_upload`, `enqueue_download` | `if_exists`, `preserve_mtime` |
| `enqueue_copy`, `enqueue_move` | `dst_account_id`: the account that owns `dst_bucket` (default `account_id`); `if_exists` |

* `dst_account_id` makes a copy between accounts: the destination account asks
  the service to copy when both accounts share an endpoint and it may read the
  source, and the objects stream through this machine otherwise. A move deletes
  each source only after the copy was read back and compared with it. An
  unknown or locked destination account rejects the command (`not_found`,
  `locked`, `mfa_required`) as for `account_id`. Tags and ACLs are not copied
  when the data streams.
* The options of a call are carried by the `group_id` of its tasks (a suffix
  after `~`); clients treat `group_id` as opaque.
* `TransferState` gains `"skipped"`: the destination existed and the policy kept
  it (`skip`, or `overwrite_if_newer` with a source that is not newer). `error`
  holds the reason ("the destination already exists"). `skipped` is terminal
  like `completed`: no command applies to it except `clear_finished`, which
  removes it, and a move never deletes the source of a skipped item.

### Overwrite questions (`ask`)

A task that finds its destination under the policy `ask` stays `running` and
sends no byte until it is answered.

| Command | Args | Result |
|---|---|---|
| `list_conflicts` | none | `PendingConflict[]`, oldest task first |
| `answer_conflict` | `task_id`, `choice: "overwrite" \| "skip" \| "overwrite_if_newer" \| "rename"`, `apply_to_batch: boolean` | `void`; `not_found` when the task is not waiting |

```ts
PendingConflict { task_id: string; group_id: string | null; conflict: Conflict }
Conflict { kind: TransferKind; source: string; destination: string;  // a path or an s3:// URI
           source_info: { size: number | null; modified: string | null };
           destination_info: { size: number | null; modified: string | null } }
```

`apply_to_batch` also settles every other waiting task of the same
`group_id` and every later question of that group. Pausing or cancelling a
waiting task withdraws its question. A `rename` answer writes to
`name (1).ext`, `name (2).ext`, ...; the task keeps its original `dst`.

Event `transfer://conflict`: `{ type: "asked", task_id, group_id, conflict }`
when a task starts to wait, `{ type: "resolved", task_id }` when it no longer
does.

Errors: `not_found` (`answer_conflict`), plus those of the enqueue commands.

## Credential sources (v0.2.0)

Until version 0.1.0 every account held static keys. An account now has a
`source` that says where its credentials come from. Only `static_keys` keeps
a secret in the secret store; every other source asks for temporary
credentials when they are needed and refreshes them before they expire. No
source ever writes a secret to `config.toml` or sends one over IPC.

### Types

```ts
type CredentialSource =
  | { type: "static_keys" }                                   // the default
  | { type: "aws_profile"; profile: string }
  | { type: "assume_role"; source_account_id: string; role_arn: string;
      session_name: string;              // default "bucketbridge", 2 to 64 of [A-Za-z0-9_+=,.@-]
      duration_secs: number;             // 900 to 43200, default 3600
      external_id?: string | null;       // 2 to 1224 characters
      mfa_serial?: string | null }
  | { type: "sso"; start_url: string; sso_region: string; account_id: string;
      role_name: string; sso_session?: string | null }
  | { type: "instance_role" };
```

- `Account.source` is always present in answers. A `config.toml` written by
  version 0.1.0 has no `source`; such accounts load as `static_keys`.
- Accounts that are not `static_keys` have `access_key_id: ""` and
  `secret_ref: ""`. For `assume_role`, `Account.mfa_serial` mirrors the role's
  `mfa_serial`, so the existing MFA prompt flow applies (`mfa_required`, then
  `mfa_login`).
- `AccountInput.source` (default `static_keys`) and `AccountUpdate.source`
  (`null` keeps it). `add_account` with another source takes no access key,
  secret key or session token (`usage` when any is given); `mfa_serial` is
  accepted for `static_keys` and, through the source, for `assume_role`.
- `update_account` with `source` changes the settings of an account that
  already has that kind of source (for example the role session length); the
  kind of source cannot change (`usage`), and a changed source drops the
  account's MFA session. Keys, secrets and session tokens cannot be set on such
  an account (`usage`).
- Validation (`usage`): the role ARN must look like
  `arn:<partition>:iam::<12 digits>:role/<name>`; the session length is 900 to
  43200 seconds; an SSO start URL starts with `https://`, the account id is 12
  digits. An `assume_role` whose `source_account_id` does not exist is
  `not_found`; a source chain that loops or an account that is its own source is
  `usage`. `remove_account` of an account that is the source of a role is
  `conflict`.

```ts
type AwsProfileKind = "static_keys" | "assume_role" | "sso" | "credential_process" | "web_identity" | "incomplete";
interface AwsProfileEntry {            // never carries a key or secret
  name: string; kind: AwsProfileKind; region: string | null;
  role_arn: string | null; source_profile: string | null;
  sso_start_url: string | null; sso_account_id: string | null; sso_role_name: string | null;
  importable: boolean; note: string | null;   // why it cannot be used
  account_id: string | null;                  // the account that links it already
}
interface ImportOutcome { imported: Account[]; skipped: { profile: string; reason: string }[] }
interface InstanceRoleProbe { kind: "instance" | "container"; access_key_id: string; expires_at: string | null }
interface SsoLoginStarted {
  login_id: string; user_code: string; verification_uri: string;
  verification_uri_complete: string | null; expires_in_secs: number;
  interval_secs: number; browser_opened: boolean;
}
type SsoLoginPoll = { state: "pending"; interval_secs: number } | { state: "complete"; expires_at: string };
interface SsoSignInStatus { signed_in: boolean; expires_at: string | null; can_refresh: boolean }
interface SsoAccount { account_id: string; account_name: string; email_address: string | null }
```

### Commands

| Command | Args | Result |
|---|---|---|
| `list_aws_profiles` | none | `AwsProfileEntry[]`, sorted by name |
| `import_aws_profiles` | `names: string[]` | `ImportOutcome` |
| `probe_instance_role` | none | `InstanceRoleProbe` |
| `sso_start_login` | `start_url, sso_region, sso_session?, open_browser?` | `SsoLoginStarted` |
| `sso_poll_login` | `login_id` | `SsoLoginPoll` |
| `sso_cancel_login` | `login_id` | `void` |
| `sso_status` | `start_url, sso_region, sso_session?` | `SsoSignInStatus` |
| `sso_sign_out` | `start_url, sso_region, sso_session?` | `void` |
| `sso_list_accounts` | `start_url, sso_region, sso_session?` | `SsoAccount[]`, sorted by name |
| `sso_list_roles` | `start_url, sso_region, sso_session?, account_id` | `string[]`, sorted |

- **AWS shared config.** `list_aws_profiles` reads `~/.aws/config` and
  `~/.aws/credentials`, or the files named by `AWS_CONFIG_FILE` and
  `AWS_SHARED_CREDENTIALS_FILE`. Missing files give an empty list; a malformed
  file is `usage`. `import_aws_profiles` creates one `aws_profile` account per
  usable profile, labelled with the profile name (`name (AWS)`, `name (AWS 2)`,
  ... when the label is taken) and with the profile's region (default
  `us-east-1`). Only the profile name is stored. Profiles that do not exist, are
  linked already or are not `importable` come back in `skipped` with a reason;
  the call itself succeeds. The profile is read again whenever credentials are
  needed, so a rotated key or a new role in the files applies without
  re-importing. A profile that has disappeared fails the account's requests with
  `auth`.
- **Instance role.** `probe_instance_role` asks for credentials once: the
  container credentials endpoint when `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`
  or `AWS_CONTAINER_CREDENTIALS_FULL_URI` is set, otherwise the EC2 instance
  metadata service with a session token (IMDSv2). Off EC2 or without a role it
  fails with `auth` and a message that says so (it waits up to 8 seconds).
- **SSO sign-in** (OIDC device authorization). `sso_start_login` registers a
  public OIDC client and asks for a device code. Unless `open_browser` is
  `false`, the host opens `verification_uri_complete` (else `verification_uri`)
  in the system browser; `browser_opened` says whether that worked, and the UI
  shows `user_code` and the URL either way. The UI then calls `sso_poll_login`
  every `interval_secs` seconds. `pending` carries the interval to use next (it
  grows after a `slow_down` answer); `complete` means the token is in the AWS CLI
  cache. A denied sign-in or an expired code is `auth` and ends the sign-in; an
  unknown or finished `login_id` is `not_found`; other failures (network) keep
  the sign-in, so polling can continue. At most 4 sign-ins wait at once and each
  is forgotten after 20 minutes. `sso_cancel_login` forgets one.
- `sso_list_accounts` and `sso_list_roles` use the cached token (refreshed with
  its refresh token when it is about to expire). Without a usable sign-in they
  fail with `auth`. The portal region decides the partition (commercial,
  GovCloud, China).
- The same commands exist for the sign-in of an account that is already stored:
  pass its `start_url`, `sso_region` and `sso_session`.

Errors: `usage`, `not_found`, `conflict`, `auth`, plus the account errors of
the other commands (`locked` comes before everything else, then `mfa_required`).

## Sync jobs and sync options (v0.2.0)

Saved sync jobs, exclusion rules, bucket-to-bucket sync, empty folders, duplicate
content, the listing cache, parallel analysis and the run report. Rules and algorithms are
in `docs/sync.md`; the CLI side is in `docs/cli.md`.

### Types

```ts
type SyncDirection = "upload" | "download" | "bidirectional" | "s3_to_s3";

interface SyncOptions {
  filter: FilterSpec;              // prefix, glob, sizes, dates, storage classes
  include: string[];               // globs; when not empty only matching paths are synced
  exclude: string[];               // gitignore-style rules, one per entry
  exclude_file: string | null;     // file of rules, read at every run
  skip_empty_folders: boolean;     // default true
  skip_duplicate_content: boolean; // default false
  use_cache: boolean;              // default true; never used when delete is set
  parallelism: number;             // 1..32, default 4
}

interface SyncJob {
  id: string;                      // "" when creating
  name: string;                    // 1..100 characters, unique without regard to case
  account_id: string;
  direction: SyncDirection;
  local_root: string;              // absolute; "" for s3_to_s3
  s3_prefix: string;               // "s3://bucket/prefix/"; the destination for s3_to_s3
  s3_source?: string;              // s3_to_s3 only
  delete: boolean;
  options: SyncOptions;            // every field optional on input, defaults as above
}
```

`SyncPlan` gains `source_prefix?: string` (present only for `s3_to_s3`, where `local_root`
is `""`, `s3_prefix` is the destination, `local_meta` of a row is the source object and
`s3_meta` the destination object, and every copy or delete has `target: "remote"`). A
row whose `rel_path` ends in `/` is a folder row (`skip_empty_folders: false`): a copy to
`remote` creates the folder marker, a copy to `local` creates the empty local folder.

### Commands

| Command | Args | Result |
|---|---|---|
| `sync_analyze` | the existing arguments plus `options?: SyncOptions` and `source_prefix?: string` | `SyncPlan`. `direction: "s3_to_s3"` needs `source_prefix` (`local_root` is ignored); `usage` when it is missing or the prefixes overlap. The exclusion file named in `options` must exist (`not_found`) |
| `list_sync_jobs` | none | `SyncJob[]` in creation order |
| `save_sync_job` | `job: SyncJob` | `SyncJob` as stored. An empty `id` creates, an `id` replaces. `usage` (invalid name, locations that do not fit the direction, `delete` with `bidirectional`, invalid options), `not_found` (account or id), `conflict` (name in use, more than 200 jobs) |
| `delete_sync_job` | `id` | `SyncJob[]` left; `not_found` for an unknown id |
| `run_sync_job` | `id` (id or name) | `SyncPlan`: the job analysed like `sync_analyze` and stored. Nothing is copied until `sync_execute`; `not_found` for an unknown job |

The job commands only read and write `config.toml`, so listing, saving and deleting work
while the secrets are locked; `run_sync_job` needs the account like `sync_analyze`.

### Events

| Event | Payload |
|---|---|
| `sync_jobs://changed` | the full `SyncJob[]` after a save or delete, sent to every window |
| `sync://report` | `SyncReport`, once after `sync://progress` with `finished: true` |

```ts
interface SyncKindTotals { files: number; bytes: number; failed: number; pending: number }

interface SyncReport {
  plan_id: string;
  direction: SyncDirection;
  duration_ms: number;
  cancelled: boolean;
  upload: SyncKindTotals;          // local files copied to S3
  download: SyncKindTotals;
  copy: SyncKindTotals;            // s3_to_s3
  delete_local: SyncKindTotals;
  delete_remote: SyncKindTotals;
  equal: SyncKindTotals;           // files and bytes only
  unsyncable: number;              // rows the analysis could not plan
  failed: number;                  // unsyncable plus rows whose attempt failed
  failures: { rel_path: string; error: string }[]; // at most 100
}
```

The report describes the final state of the plan (rows done by an earlier run count as
done); `duration_ms` is the time of this run. `files` and `bytes` count done rows, `failed`
rows whose last attempt failed and `pending` rows not attempted (a cancelled run).

### Settings and storage

No setting was added. Jobs are stored in `config.toml` as `[[sync_jobs]]` (omitted while
empty, so older files load unchanged). The listing cache lives in `data_dir/sync-cache/`.
