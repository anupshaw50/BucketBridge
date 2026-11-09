# Architecture

BucketBridge is a cross-platform S3 client: a Rust core shared by a
command-line client (`bb`) and a Tauri desktop app with a React UI.
`docs/desktop-api.md` is the IPC
contract and the JSON shape of the data model.

## Workspace layout

```
BucketBridge/
├── Cargo.toml          virtual workspace manifest (resolver 3, edition 2024)
├── crates/
│   ├── core/           bucketbridge-core: model, config, errors, S3 transport
│   ├── cli/            bucketbridge-cli: the `bb` binary
│   └── desktop/        Tauri host (added later)
├── tests/              bucketbridge-tests: integration-test harness + tests
├── ui/                 React + TypeScript frontend
└── docs/
```

Dependencies point one way: `cli`, `desktop` and `tests` depend on `core`;
`core` depends on no other workspace crate. Shared dependency versions live in
`[workspace.dependencies]` of the root manifest; every crate opts into the
shared lints with `[lints] workspace = true` (`unsafe_code` is forbidden;
`unwrap`/`expect` warn outside tests, as do the debugging and placeholder
macros). CI builds with `-D warnings`, so warnings fail the build; crate
roots allow `unwrap`/`expect` only under `cfg(test)`. TLS is rustls
everywhere (the AWS SDK default HTTPS client, hyper 1 with rustls); OpenSSL
and native-tls are not used.

## Module map of `bucketbridge-core`

| Module | Contents | Owner area |
|---|---|---|
| `error` | `Error`, `ErrorKind`, `ServiceError`, `BbError`, SDK error conversion | foundation |
| `model::account` | `Account`, `AccountInput`, `AccountUpdate`, `AccountList`, `SecurityStatus` | accounts, secrets, proxy, MFA |
| `model::bucket` | `Bucket`, `BucketKind` | foundation |
| `model::object` | `ObjectEntry`, `ObjectPage`, `ObjectVersion`, `VersionPage`, `ObjectMeta`, `Sse`, `ObjectLock`, `ObjectHead`, `DeleteOutcome`, `MultipartUpload`, `CopyOutcome` | foundation; metadata and versions work extends it |
| `model::transfer` | `TransferTask`, `TransferKind`, `TransferState`, `Location` | transfers and queue |
| `model::sync` | `SyncPlan`, `SyncRow`, `SyncDirection`, `SyncAction`, `Side`, `FileMeta` | sync |
| `model::log` | `LogEntry`, `LogLevel` | event log |
| `model::filter` | `FilterSpec`, `SavedFilter` | filters |
| `settings` | `Settings` and its pages, defaults, `validate()` | foundation; each area reads its page |
| `config` | `AppConfig`, `ConfigStore`, `Dirs`, `SecurityConfig` (data only) | foundation; accounts work implements the crypto |
| `s3uri` | `S3Uri`, bucket and key naming rules | foundation |
| `path` | path normaliser and the download path safety check | foundation |
| `client` | `Credentials`, `ClientOptions`, `build_client`, `STALL_GRACE_PERIOD`, `redact_endpoint` | foundation; accounts work injects proxy-aware HTTP clients |
| `ops` | `S3` transport: buckets, listings, head, folders, deletes, server-side copy, presigned URLs, multipart-upload listing and abort; bucket regions (`client_for`) and the per-request read timeout (`metadata_timeout`) | foundation; streaming upload and download belong to transfers |
| `bucketcfg` | Bucket configuration on `S3`: policy (check, format, put), public access block, versioning, logging, website, acceleration, the combined `get_bucket_config`; the external buckets pinned to an account (`config.toml`, `pinned_buckets`) | bucket configuration (T03) |
| `objectmeta` | Object metadata (T06): `S3::object_details`, tags, storage class, server-side encryption (`CustomerKey` for SSE-C), HTTP headers and user metadata, canned ACLs; batch methods with per-object `ObjectChange` results; the in-place copy behind class, encryption and header changes | object metadata |
| `filter` | `Filter` (compiled `FilterSpec`), `Glob`, `IncludeExclude` (CLI `--include`/`--exclude`) | filters |
| `sync` | `analyze`, local and remote scans, the planner, `SyncStore` (resumable plan store), `RowCounts` | sync (see `docs/sync.md`) |
| `sync::{options, analysis, listing, duplicate, folders, report}`, `syncjobs`, `ignore` | `SyncOptions`, `analyze_async` and `analyze_s3`, parallel and cached listing, duplicate content, empty folders, `SyncReport`, saved `SyncJob`s, ignore-file style `IgnoreRules` | sync (see `docs/sync.md`, "Saved jobs, exclusion rules and options") |
| `accountfile` | Account folders and order (`AccountLayout`), the secret-free account export file and its import | accounts (see docs/desktop-api.md, "Account folders, order, export and import") |
| `usage` | Storage usage of a bucket or prefix (`S3::compute_usage`) | bucket properties, `bb du` |
| `eventlog` | `EventLog` (ring buffer, rotating log file, CSV/JSON export, `tracing` layer) | event log (see `docs/logging.md`) |
| `crash` | panic hook, `CrashReport`, newest report, dismiss | event log (see `docs/logging.md`) |
| `secrets` | `SecretStore` (OS keychain `KeyringStore`, `MemoryStore`), master-password `Vault`, `SecretRef`, `SecretString` | accounts and security (docs/security.md) |
| `accounts` | `AccountManager`: accounts, their secrets, the master password, crash-safe changes | accounts and security |
| `session` | `Session`: the process-wide entry point (accounts, unlock and lock, MFA login, cached S3 handles, security broadcasts) | accounts and security |
| `mfa` | STS `GetSessionToken`, in-memory `MfaSessions` | accounts and security |
| `creds` | credential sources other than static keys: AWS profiles, assumed roles, IAM Identity Center (SSO), instance and container roles; refreshing credentials providers, the AWS CLI token cache | accounts and security (see `docs/security.md`) |
| `versions` | `S3::list_version_groups` (versions grouped by key, filters, pages), `head_version`, `restore_version`, `purge_versions`, `enqueue_download_version` | versions and preview (T08) |
| `edit` | `EditManager`: edit sessions (local copy, `notify` watcher with debounce, `HeadObject` ETag check, upload with the object's headers and `If-Match`, conflict resolutions, cleanup) | edit in place |
| `preview` | `S3::preview_object`: ranged reads of an object or version, image, text and JSON detection, paging of text | versions and preview (T08) |
| `proxy` | the proxy-aware HTTP client (HTTP and SOCKS5 proxies, bypass list) shared by every SDK client | accounts and security |
| `diagnostics` | `run`, `probe_account`, `probe_input`: the step-by-step probe (DNS, TCP, proxy tunnel, TLS with the certificate, a signed request, the bucket region) and its `Report` | tools (T12) |
| `export` | `export_listing`: CSV, XLSX and Parquet from a streamed listing, written atomically | tools (T12) |
| `saved_filters` | the saved filters of `config.toml` (`list`, `save`, `delete`, validation by the filter engine) | tools (T12) |

Feature work adds modules beside these (for example `transfer`, `sync`,
`filter`, `eventlog`, `accounts`, `secrets`, `proxy`, `mfa`) and extends the
model file of its own area, so parallel work touches different files.

## Error model and exit codes

Every failure is an `Error`. `Error::kind()` returns an `ErrorKind` whose
serde form is the `BbErrorCode` of the IPC contract, and an `Error`
serializes to `{ "code": ..., "message": ... }`. The CLI maps kinds to exit
codes:

| Kind | Meaning | `bb` exit code |
|---|---|---|
| (success) | | 0 |
| `usage` | invalid input from the caller | 1 |
| `remote` | any other S3 or network failure | 2 |
| `not_found` | bucket, key, account, task or plan missing | 2 |
| `conflict` | bucket not empty, destination exists, ... | 2 |
| `io` | local file system failure, unusable `config.toml` | 2 |
| `internal` | a bug | 2 |
| `auth` | missing or invalid credentials, access denied | 3 |
| `locked` | secrets locked behind the master password | 3 |
| `mfa_required` | account needs an MFA session | 3 |
| `cancelled` | operation cancelled | 130 |

`internal` and `cancelled` are not listed in `docs/desktop-api.md`; they use
2 (a generic failure inside the stable set) and 130 (the shell convention for
an interrupted command).

Any AWS SDK error (`SdkError<E, R>` with `E: ProvideErrorMetadata`, from S3,
STS or any other `aws-sdk-*` crate) converts with `From` into
`Error::Service(ServiceError)`, which keeps the service error code, message,
HTTP status, request ID and S3 extended request ID. Classification (code
first, because S3 sends `ExpiredToken` with HTTP 400):

* `auth`: `InvalidAccessKeyId`, `SignatureDoesNotMatch`, `ExpiredToken`,
  `InvalidToken`, `AccessDenied` (plus the STS spellings such as
  `InvalidClientTokenId`), otherwise any 401 or 403 except the codes below;
* `not_found`: `NoSuchBucket`, `NoSuchKey`, `NoSuchUpload`, `NoSuchVersion`,
  `NotFound`, otherwise any 404;
* `conflict`: `BucketNotEmpty`, `BucketAlreadyExists`,
  `BucketAlreadyOwnedByYou`, `InvalidObjectState` and
  `ObjectNotInActiveTierError` (HTTP 403 for reading an archived object that
  is not restored), otherwise any 409;
* `usage`: the request could not be constructed;
* `remote`: dispatch failures, timeouts, unparseable responses,
  `RequestTimeTooSkewed` and `CrossLocationLoggingProhibited` (HTTP 403, but
  not about credentials) and everything else.

`RequestTimeTooSkewed` messages end with a hint to correct the computer's
date, time and time zone; `InvalidObjectState` messages with a hint to
restore the archived object first.

Messages read like `The specified bucket does not exist (NoSuchBucket, HTTP
404, request id 4442587FB7D0A2F9)`; the transport prefixes the bucket or
`s3://` URI involved. Local I/O errors always carry what was being done
(`Error::io(context, err)` or `IoContext::io_context`).

## Configuration and data directories

`Dirs` holds four directories:

| | Linux | macOS | Windows |
|---|---|---|---|
| `config_dir` (`config.toml`) | `~/.config/bucketbridge` | `~/Library/Application Support/BucketBridge` | `%APPDATA%\BucketBridge\config` |
| `data_dir` | `~/.local/share/bucketbridge` | `~/Library/Application Support/BucketBridge` | `%LOCALAPPDATA%\BucketBridge\data` |
| `log_dir` | `~/.local/state/bucketbridge/logs` | `~/Library/Application Support/BucketBridge/logs` | `%LOCALAPPDATA%\BucketBridge\data\logs` |
| `crash_dir` | `~/.local/state/bucketbridge/crash` | `~/Library/Application Support/BucketBridge/crash` | `%LOCALAPPDATA%\BucketBridge\data\crash` |

(XDG variables such as `XDG_CONFIG_HOME` are honoured on Linux.) Setting
`BB_CONFIG_DIR` puts everything under one folder instead: `config.toml`
directly in it, plus `data/`, `logs/` and `crash/`. A relative value is
resolved against the current directory. This is used for portable installs,
tests and CI. `Dirs::under(path)` builds the same layout explicitly.

`config.toml` (`AppConfig`) holds the format version, the installation id (a
random UUID created at the first write and kept, which names this installation
in the OS keychain, see docs/security.md), accounts, the default account,
settings, master-password parameters (`SecurityConfig`, no secrets) and saved
filters. It never holds secrets, only `*_ref` keys into the secret
store. `ConfigStore`:

* `load()`: a missing file gives the defaults and creates nothing. Every
  struct uses `#[serde(default)]`, so partial and older files load; unknown
  fields are ignored. The result is a read-only snapshot.
* `update(|cfg| ...)` is the only way to write. It takes an exclusive
  advisory lock on `config.toml.lock` (standard-library file locking),
  re-reads the file, runs the closure on the current contents, writes to a
  temporary file in the same directory, `fsync`s it, renames it over
  `config.toml` and `fsync`s the directory. On Unix the file is `0600` and
  newly created directories `0700`. Because every change is applied to what
  is on disk at that moment, the desktop app and the CLI never lose each
  other's changes; when the closure fails or leaves invalid settings,
  nothing is written. There is deliberately no call that writes back a
  snapshot from `load()`: it would silently drop changes made meanwhile.
* A file that is not valid UTF-8 or TOML, has invalid settings, or carries a
  newer (or zero) format version is `Error::Config` (kind `io`) naming the
  file. `update` fails with it before running the closure and never touches
  the file.
* These calls block; async callers use `spawn_blocking`.

## Retry and timeout policy

Open design decision in the plan; the choice:

* SDK **standard** retry mode, `advanced.max_attempts` attempts per request
  including the first (default **5**, range 1..=20). Standard mode retries
  throttling (`SlowDown`, 503), transient 5xx and connection errors with
  exponential backoff and jitter, and draws from a retry token bucket so a
  failing endpoint is not hammered. Adaptive mode was rejected: its
  client-side rate limiter would interfere with the transfer queue's own
  concurrency and bandwidth control.
* **Connect timeout** 10 s (`advanced.connect_timeout_secs`) on every
  request.
* **Read timeout** 60 s (`advanced.read_timeout_secs`): the time from
  sending a request until its response headers arrive. It is **not set on
  the clients**. The SDK starts it with the request, and S3 answers an
  upload only after the whole body has arrived, so on the client it would
  fail every PutObject or UploadPart whose body takes longer to send (an
  8 MiB part under a shared bandwidth limit, for example). `S3` applies it
  per request instead, with `S3::metadata_timeout()` (a configuration
  override for `customize().config_override(...)`), to every request that
  sends no large body: listings, HEAD (also the HeadBucket of region
  resolution), deletes, tagging, CreateBucket, DeleteBucket,
  CreateMultipartUpload, AbortMultipartUpload, ListParts and folder markers
  (an empty PutObject). The transfer engines apply it to their own such
  requests; GetObject may use it too, because the timeout ends when the
  response headers arrive.
* **No read timeout** for PutObject and UploadPart with a body, nor for the
  long server-side calls CopyObject, UploadPartCopy and
  CompleteMultipartUpload (some servers answer only when the copy or
  assembly is done; AWS answers at once and keeps the response alive with
  whitespace). Stalled-stream protection covers their bodies. A server that
  accepts such a request and then never answers at all is not detected;
  cancelling the task ends the wait.
* **Stalled-stream protection** on request and response bodies with a grace
  period of `client::STALL_GRACE_PERIOD` (20 s; `ClientOptions::stalled_stream`
  changes it). The SDK measures throughput over a 1 s window against a
  minimum of 1 byte per second and fails the stream when it stays below that
  for the grace period, so a stream fails after 20 to 21 s without a single
  byte, and a stream that gets bytes every few seconds is fine.
* **Constraint for the transfer queue and engines: an upload body must
  never go silent for 20 s or longer.** A bandwidth limiter has to hand every
  in-flight body small portions often (at least one portion every few
  seconds, also when many parts share a low limit), never large portions
  rarely. If a limit is so low that a body would wait 20 s or more for its
  next bytes, that request needs a longer grace period (a per-request
  `config_override` with `stalled_stream_protection(...)`). The queue's runner
  avoids that by bounding how many bodies stream at once at the current cap
  (`Throttle::open_stream`: 20 at 1 KiB/s), see `docs/engines.md`. Downloads are
  only considered stalled while they are being read: a reader that pauses to
  apply a bandwidth limit never trips the protection.
* **No operation or attempt timeout**: a transfer must never fail because it
  is large or the link is slow.
* The transfer queue adds `transfer.retry_attempts` (default 3) retries per
  part or object on top of the SDK's per-request retries.

Clients never read credentials, regions or endpoints from the environment,
AWS config files or instance metadata: `build_client` uses only the account,
explicit static credentials and `ClientOptions`. (The CLI's `BB_*`
variables are an explicit, documented input, not an implicit fallback.) The
SDK's default HTTPS client does honour `HTTP_PROXY`, `HTTPS_PROXY` and
`NO_PROXY`; the proxy settings of the app are applied by injecting an HTTP
client through `ClientOptions::http_client`. `Session` injects the client of
`proxy`, which keeps honouring those variables while the app's proxy
settings are disabled.

`Credentials` print as `<redacted>` in `Debug` output and are zeroed on drop.
Endpoint URLs in messages go through `client::redact_endpoint`, which
replaces user information (`user:password@`) with `<redacted>@`;
`validate_endpoint` rejects URLs with user information without echoing it,
so credentials typed into an endpoint never reach the UI or a log file.

## Bucket regions

AWS has one endpoint per region and the SDK does not follow S3's region
redirects, so a request about a bucket must be signed for and sent to the
bucket's region. `S3::client_for(bucket)` returns the client for it:

* Accounts without a custom endpoint resolve each bucket's region once and
  cache it in the `S3` value (shared by its clones): from `list_buckets`
  (every ListBuckets page sends `max-buckets=1000`, which makes S3 report
  each bucket's region and lets accounts with a raised bucket quota
  paginate), from `create_bucket`, or else with one `HeadBucket` request on
  the account-region client, whose `x-amz-bucket-region` header S3 sends with
  200, 301 and 403 answers. Concurrent callers share that one request. An
  answer without the header (a missing bucket, for example) means the
  account region for that call, and nothing is cached; a `HeadBucket` that
  gets no answer (timeout, connection failure) or names an invalid region is
  an error. `delete_bucket` forgets the bucket's region, and every
  `list_buckets` refreshes the cached regions it reports.
* Accounts with a custom endpoint always use the account region and send no
  extra request (`S3::with_region_resolution(true)` overrides this; tests use
  it to emulate AWS with `MockS3`).
* There is one client per region, built on first use from the account
  client's configuration with only the region changed: credentials, retry,
  timeouts, checksum settings, stalled-stream protection, interceptors and
  an injected HTTP client (`ClientOptions::http_client`) stay the same.
  Without an injected client, each regional client gets its own instance of
  the SDK's default HTTPS client; regional endpoints are different hosts, so
  no connection could be shared anyway.
* Every bucket-scoped operation of `S3` goes through `client_for`, presigned
  URLs included; `create_bucket` signs for its explicit region and
  `list_buckets` uses the account region. CopyObject and UploadPartCopy go to
  the destination's region (S3 reads the source across regions). The
  transfer engines do the same for raw SDK calls:
  `s3.client_for(bucket).await?` with `s3.metadata_timeout()` and
  `s3.request_payer()`. `S3::client()` stays the account-region client.

## Checksums

* Requests: `data_integrity.checksum_algorithm` defaults to `crc32`. Any
  value other than `none` sets the SDK to calculate request checksums
  whenever an API supports them (`RequestChecksumCalculation::WhenSupported`);
  the transfer engine chooses the algorithm per upload. `none` sends a
  checksum only when an API requires one (DeleteObjects, for example).
* `Content-MD5`: requests of the operations for which S3 requires an
  integrity header (DeleteObjects, PutObjectTagging, PutObjectRetention,
  PutObjectLegalHold, PutBucketLifecycleConfiguration and the other bucket
  configuration calls) also carry `Content-MD5`, in every checksum mode. The
  SDK sends only a CRC32 header there, which some S3-compatible servers
  reject; AWS accepts both. An interceptor installed by `build_client` adds
  it to in-memory bodies and keeps a value the caller set.
* Responses: `data_integrity.validate_downloads` (default on) validates
  response checksums whenever the server returns one; off means only when
  required.
* `head_object` requests checksum mode, so `ObjectHead::checksums` carries
  the stored values (base64, composite multipart values keep their `-N`
  suffix). Listings never carry checksum values.

## Multipart decision

Open design decision in the plan; the choice, implemented by the transfer
engine:

* **Uploads**: objects of at least **8 MiB** (`multipart_threshold_mib`) use
  multipart upload with **8 MiB parts** (`part_size_mib`), grown in whole
  MiB so that no upload needs more than 10,000 parts (`ops::part_size_for`
  computes this; part sizes stay within 5 MiB..=5 GiB). Up to 4 parts per
  task in flight (`max_parts_per_task`). Cancelling an upload aborts its
  multipart upload so no partial object or orphaned parts remain. A failed
  upload keeps its multipart upload and finished parts, so that an
  automatic or manual retry continues where it stopped; cancelling the
  failed task aborts it (`docs/engines.md`).
* **Server-side copy** (`S3::copy_object`): objects up to 5 GiB use one
  `CopyObject` with `MetadataDirective: COPY`; larger objects use
  `UploadPartCopy` with 256 MiB parts (grown to stay within 10,000 parts),
  4 in flight. Content headers, user metadata and (where the server supports
  tagging) tags are carried over; the source is pinned by version ID and
  `If-Match` ETag; any failure aborts the upload (a failed abort is part of
  the error), and dropping the future starts an abort in the background
  (a failed one is logged only).

## Listing semantics

* `list_objects(bucket, prefix, flat, token, max_keys)` returns one page:
  with `flat == false` the delimiter `/` turns sub-folders into entries with
  `is_prefix` set; prefixes come first, then objects. The entry whose key
  equals `prefix` (a folder marker) is never returned, and a page that would
  contain only that entry is skipped.
* `list_all` / `list_all_stream` list recursively, 1000 keys per request;
  the stream holds one page in memory. They include the folder marker equal
  to the prefix, because recursive deletes and downloads must see it.
* Zero-byte objects whose key ends in `/` are folder markers; they are
  objects (`is_prefix == false`) in recursive listings.
* Pagination is defensive: a server that reports more results without a
  token, or sends a continuation token (or version or upload marker) that it
  already sent during the same listing, produces a `remote` error instead of
  an endless or silently truncated listing. That covers a token repeated
  together with entries as well as cycles such as A, B, A; each listing keeps
  64-bit hashes of the tokens it has seen. `list_objects` returns one page
  per call and fails when the server answers with the token it was given;
  callers that follow `next_token` across calls (the CLI, the UI) must still
  detect longer cycles themselves.

## Requester pays and presigned URLs

When an account has `request_payer`, every operation that supports it sends
`x-amz-request-payer: requester`; `S3::request_payer()` exposes the value
for raw SDK calls. Presigned URLs carry it as the signed query parameter
`x-amz-request-payer=requester`, so the URL works on its own. Presigned
URLs are valid for 1 second to 7 days (the SigV4 maximum), signed for and
pointing to the bucket's region (see "Bucket regions"); presigned PUT URLs
require no checksum and accept any `Content-Type`.

## The path normaliser as a security boundary

Object keys come from the server and are untrusted. `path::safe_local_path`
maps a key (relative to the stripped selection prefix) to a path inside the
destination folder and rejects, on every OS: empty keys, NUL, backslashes,
absolute keys (`/x`, which also covers `//server/share`), empty segments
(`a//b`), `.` and `..` segments, and segments that start with a drive letter
(`C:`, `c:foo`). On Windows it also rejects characters that are invalid in
file names (`<>:"|?*` and control characters, so NTFS alternate data streams
too), names ending in a dot or space (Windows strips them, which would turn
`.. ` into `..`), and reserved device names (`CON`, `NUL`, `COM1`, ...). The
same rules are available as `validate_relative_key(key, PathStyle)` so the
Windows rules are tested on every OS. A key such as
`../../.ssh/authorized_keys` can therefore never make a download write
outside the folder the user picked.

`relative_path_to_key` goes the other way for uploads (OS separators to
`/`, `.` dropped, absolute paths, `..` and non-UTF-8 names rejected).
`strip_parent` implements the folder semantics of the transfer commands
(`photos/2024/` copied to `backup/` gives `backup/2024/...`) and `join_key`
joins prefixes. Names are never Unicode-normalised: keys and file names are
kept byte for byte so round trips are lossless and distinct keys never merge.
Keys that differ only in case or normalisation still map to one file on
case- or normalisation-insensitive file systems; bulk downloads should
detect such collisions.

## CLI (`bb`)

`bb` implements `account add|list|remove|default`, `ls`, `mb`, `rb`, `rm`,
`cp` and `mv` (local to S3, S3 to local, and S3 to S3, through the engines),
`sync` (through the sync executor, resumable with `--resume`), `presign`,
`mpu list|abort` and `export` (a listing as CSV, Parquet or XLSX; see
`docs/cli.md`). Account selection goes through one
`mpu list|abort`, `tag get|set|delete`, `storage-class`, `sse` and `acl get|set`
(see `docs/cli.md`). Account selection goes through one
function (`context::resolve_account`): `--account`, then the default stored
account, then an ephemeral account from the environment, a permanent
feature for scripts and CI:

| Variable | Meaning |
|---|---|
| `BB_ACCESS_KEY_ID`, `BB_SECRET_ACCESS_KEY` | credentials (required; missing is an `auth` error, exit 3) |
| `BB_SESSION_TOKEN` | session token for temporary credentials |
| `BB_REGION` | signing region, default `us-east-1` |
| `BB_ENDPOINT` | custom endpoint URL |
| `BB_PATH_STYLE` | `true`/`false` (also `1`/`0`, `yes`/`no`, `on`/`off`) |
| `BB_CONFIG_DIR` | where `config.toml` lives (retry, timeout, checksum and page-size settings) |

Data goes to standard output, errors to standard error as
`bb: error: <message>` (a `{code, message}` record with `--json`). `--json`
prints one JSON record per item (NDJSON); the schemas are in `docs/cli.md`.
Usage errors exit with 1 (clap's own default of 2 is overridden); `--help`
and `--version` exit with 0; Ctrl-C exits with 130. A closed standard output
(`bb ls | head`) ends the command quietly with 0.

## Concurrency notes

* `S3` is cheap to clone and safe to share; one per account, so several
  accounts work at the same time with independent clients. Clones share the
  bucket-region cache and the regional clients.
* Progress callbacks (`copy_object_with`) run on the task driving the
  operation and must not block.
* The configuration store is synchronous and blocking (see above).
