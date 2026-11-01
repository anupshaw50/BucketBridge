# Testing

BucketBridge has three Rust test layers. All of them run with plain
`cargo test`; none needs Docker or network access beyond `127.0.0.1`.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace                 # everything below
cargo test -p bucketbridge-core        # unit tests (pure logic)
cargo test -p bucketbridge-tests       # integration tests (in-process S3 server)
cargo test -p bucketbridge-cli         # CLI tests (runs the real `bb` binary)
cargo test -p bucketbridge-tests --test ops -- list_all   # one test by name filter
```

## Unit tests (`crates/core`)

`#[cfg(test)]` modules next to the code. They cover the JSON shape of every
contract type (exact field names and enum spellings), settings defaults and
validation, the configuration store (atomic writes, locking, permissions,
corrupt and newer files, stale snapshots), `s3://` parsing and S3 naming
rules, error classification and exit codes, client construction (timeouts,
endpoint redaction), regional clients and presigned AWS URLs, and the path
normaliser. The Windows file
name rules of `path.rs` are tested on every OS through
`validate_relative_key(key, PathStyle::Windows)`; tests that need a real
Windows path parser are `#[cfg(windows)]`.

## Integration tests (`tests/`, package `bucketbridge-tests`)

The `tests` crate is both the harness (its library) and the integration
tests (`tests/tests/*.rs`). Later crates use the harness as a
dev-dependency (`bucketbridge-tests.workspace = true`).

### `TestS3`

```rust
let s3 = TestS3::start().await;          // isolated server for this test
let bucket = s3.new_bucket().await;      // unique name, created
s3.put(&bucket, "a/b.txt", "data").await;
let page = s3.ops().list_objects(&bucket, "a/", false, None, 1000).await?;
```

* Default backend: an in-process [s3s](https://crates.io/crates/s3s) server
  with the `s3s-fs` file-system backend (both Apache-2.0) on `127.0.0.1:0`,
  over a temporary directory, with SigV4 authentication enabled (random
  access key and secret per instance), so request signing is really
  exercised. The server runs on its own thread and Tokio runtime: a test may
  block (for example while waiting for the `bb` binary) without stalling it.
* Every instance has its own port, data directory and keys, so tests run in
  parallel. Dropping it stops the server and removes the data.
* Accessors: `endpoint()`, `region()`, `access_key()`, `secret_key()`,
  `account()` (custom endpoint, path-style), `credentials()`, `ops()` (the
  core `S3` transport), `client()` (the raw `aws_sdk_s3::Client`),
  `connect(&credentials, &options)` (a transport with other credentials or
  options), `root()` (the s3s-fs data directory), `backend()`.
* Helpers: `unique_bucket()`, `create_bucket(name)`, `new_bucket()`,
  `put(bucket, key, body)`, `get(bucket, key)`, `exists(bucket, key)`,
  `pending_uploads(bucket, key)` (multipart uploads neither completed nor
  aborted; with s3s-fs it counts every open upload on the isolated server,
  so start uploads only for the key you check).
* The harness panics with a descriptive message when the environment cannot
  be set up; it is test code.

### `MockS3`

A scripted HTTP endpoint for behaviour that no real backend produces on
demand. Every request is recorded; a closure answers it:

```rust
let mock = MockS3::start(|req| {
    if req.method == "POST" && req.query_param("delete").is_some() {
        MockResponse::xml("<DeleteResult>...</DeleteResult>")
    } else {
        MockResponse::error(500, "InternalError", "boom")
    }
});
let outcome = mock.ops().delete_objects("bucket", &keys).await?;
assert_eq!(mock.requests().len(), 1);
```

`tests/tests/mock.rs` uses it for per-key delete failures, batching,
repeated or missing continuation tokens, retries on throttling, version and
multipart-upload listings, request-payer headers and location constraints;
`pagination.rs` for token cycles (A, B, A) in every listing;
`content_md5.rs` for the `Content-MD5` header; `regions.rs` for bucket-region
resolution. Remember that the SDK addresses buckets as `/bucket/` in path
style.

To emulate AWS region handling, turn resolution on for the mock's custom
endpoint and answer like S3 does; the signing region of every request is in
the credential scope of its `Authorization` header
(`Credential=AKIDMOCK/<date>/<region>/s3/aws4_request`):

```rust
let mock = MockS3::start(|req| {
    if req.method == "HEAD" && req.path == "/far/" {
        MockResponse::empty(301).with_header("x-amz-bucket-region", "eu-west-1")
    } else {
        // ... check the signing region, answer or send PermanentRedirect
    }
});
let ops = mock.ops().with_region_resolution(true);
```

### `BucketConfigMock`

A `MockS3` that keeps bucket configuration, for the APIs s3s-fs does not
implement (policy, public access block, versioning, logging, website,
acceleration). It stores each document a client puts and answers the next read
with it, answers "nothing configured" with S3's error codes
(`NoSuchBucketPolicy`, `NoSuchPublicAccessBlockConfiguration`,
`NoSuchWebsiteConfiguration`) or an empty document, and can refuse a section
(`deny(bucket, ConfigKind::Policy)`: `AccessDenied`), leave one unimplemented
for every bucket (`not_implemented(ConfigKind::Website)`: 501) or refuse
`ListBuckets` (`deny_listing()`). Every request is recorded, so tests assert
the exact calls and payloads as well as the round trip. `ops()` is a transport
for a custom-endpoint account, `aws_ops(request_payer)` one that treats the
mock as AWS (transfer acceleration is only offered there).
`tests/tests/bucketcfg.rs` uses it for core, `crates/desktop/tests/ipc/bucketcfg.rs`
for the commands; the tests that need a real server (policy and versioning on
MinIO) are ignored without the `minio` feature.
### `ObjectStore`

A small in-memory S3 for the object-metadata tests (`tests/src/objstore.rs`),
served through a `MockS3` so every request is recorded. It keeps objects with
headers, user metadata, tags, storage class, encryption and ACL and answers
`HeadObject`, tagging, ACL, `CopyObject`, multipart copies and `ListObjectsV2`
the way S3 does, including the details that make in-place edits tricky: a copy
resets the storage class, the encryption and the ACL unless the request sends
them again and never copies the website redirect; copying an object onto itself
without a change is refused; `MetadataDirective: REPLACE` drops every header the
request leaves out; an SSE-C object needs its key (none is 400, a wrong one 403, a
key for an object without one 400, a key whose MD5 does not match 400);
`x-amz-copy-source-if-match` fails with 412. `disable_acls(bucket)` emulates
Object Ownership BucketOwnerEnforced, `intercept(...)` and `fail(...)` inject
errors, `put(...)`/`object(...)` seed and inspect objects.

```rust
let store = ObjectStore::new();
let mock = store.start();                       // a MockS3 served by the store
store.put("bucket", "a.txt", StoredObject::new(5).class("STANDARD_IA").tag("env", "prod"));
let results = mock.ops().set_object_storage_class("bucket", &keys, StorageClass::Glacier, None, BatchOptions::default()).await?;
assert_eq!(copies(&mock)[0].header("x-amz-storage-class"), Some("GLACIER"));
```

`tests/tests/objectmeta.rs` (core), `crates/desktop/tests/ipc/objectmeta.rs` and
`crates/cli/tests/cli/objectmeta.rs` use it. MinIO covers tags and header edits
(the gated tests there); MinIO refuses a copy onto itself that only changes the
storage class or the encryption, so those are covered by the store.

### `RawServer`

A raw HTTP/1.1 endpoint that is slow or silent on purpose, for read-timeout
and stalled-stream tests (`tests/tests/timeouts.rs`). Every connection gets
its own thread with blocking sockets:

* `RawBehavior::Silent` reads whatever arrives and never answers;
* `RawBehavior::SlowDrain { chunk, pause }` reads the request body `chunk`
  bytes at a time with `pause` after each read and answers `200 OK` once all
  of it has arrived, as S3 answers an upload;
* `RawBehavior::Trickle { chunk, chunks, pause }` answers `200 OK` at once
  and sends the body chunk by chunk with `pause` in between.

```rust
let server = RawServer::start(RawBehavior::SlowDrain { chunk: 64 * 1024, pause: Duration::from_millis(50) });
let s3 = server.ops_with(&ClientOptions { read_timeout: Duration::from_secs(1), ..ClientOptions::default() });
assert_eq!(server.requests(), 0);
```

Requests with a body must carry `Content-Length` (the SDK always sends
one). `body_bytes()` counts the request body bytes received.

### MinIO (opt-in, needs Docker)

With the `minio` feature and `BB_TEST_BACKEND=minio`, `TestS3::start()`
launches a MinIO container through `testcontainers-modules` instead (Docker
required; one container per `TestS3`):

```bash
BB_TEST_BACKEND=minio cargo test -p bucketbridge-tests --features minio
```

MinIO no longer publishes container images (`minio/minio` is gone from Docker
Hub and Quay), so the harness runs `pgsty/minio:RELEASE.2026-08-04T00-00-00Z`,
the last release of the community fork under the MinIO name. Set
`BB_TEST_MINIO_IMAGE` to `name` or `name:tag` to use another image, for
example one from a private registry.

Tests that need a feature s3s-fs lacks start MinIO explicitly and are
ignored without the feature:

```rust
#[tokio::test]
#[cfg_attr(not(feature = "minio"), ignore = "needs MinIO: s3s-fs does not implement versioning")]
async fn list_versions_shows_versions_and_delete_markers() {
    let s3 = TestS3::start_minio().await; // panics unless the MinIO backend is selected
    // ...
}
```

`BB_TEST_BACKEND=minio` without the feature, or the feature with
`TestS3::start_minio()` but without the variable, fails loudly instead of
silently testing something else. The feature always compiles on every OS:
`cargo clippy -p bucketbridge-tests --features minio --all-targets`.

With the MinIO backend every test in the crate runs against MinIO, so keep
generic tests backend-neutral. One MinIO difference to remember: it lists
uncompleted multipart uploads per object key rather than by arbitrary
prefix, so list uploads by exact key in tests that must pass on both.

### s3s-fs gaps

What s3s-fs 0.17 does not implement, or implements differently from S3.
Gate tests that depend on these with the `minio` pattern above, or cover the
client side with `MockS3`.

**Not implemented (HTTP 501 `NotImplemented`).** s3s-fs implements only:
CreateBucket, DeleteBucket, HeadBucket, GetBucketLocation, ListBuckets,
ListObjects (v1 and v2), PutObject, GetObject, HeadObject, CopyObject,
DeleteObject, DeleteObjects, CreateMultipartUpload, UploadPart,
UploadPartCopy, ListParts, CompleteMultipartUpload, AbortMultipartUpload.
Everything else is missing, notably:

* versioning: ListObjectVersions, Get/PutBucketVersioning, version IDs,
  delete markers, MFA delete;
* ListMultipartUploads;
* object and bucket tagging;
* ACLs, object ownership, bucket policy, public access block;
* Object Lock (retention, legal hold);
* server-side encryption configuration; SSE request headers are accepted and
  ignored, nothing is encrypted and no SSE response headers are returned;
* CORS, lifecycle, replication, logging, website, transfer acceleration,
  Intelligent-Tiering, RestoreObject (Glacier), GetObjectAttributes,
  SelectObjectContent.

**Behaves differently from S3.**

* Folder markers (keys ending in `/`) are stored as directories. PutObject
  with a body on such a key fails (`UnexpectedContent`); markers are not
  listed by ListObjects (neither as objects nor, when empty, as common
  prefixes); HeadObject and GetObject on them return `NoSuchKey`.
  `create_folder` can therefore only be verified on disk (`TestS3::root()`).
* A key cannot be both an object and a prefix: `a` and `a/b` collide (file
  versus directory). On case-insensitive file systems (macOS default) keys
  that differ only in case collide too.
* DeleteBucket removes the bucket directory recursively, even when it is not
  empty. The core's `delete_bucket` checks for objects first, which is why it
  is safe against this server.
* CreateBucket on an existing bucket returns `BucketAlreadyExists` (S3 says
  `BucketAlreadyOwnedByYou` for your own bucket); LocationConstraint is
  ignored; GetBucketLocation always answers with the empty (us-east-1)
  location.
* ListBuckets returns no owner and no bucket region, in directory order (the
  core sorts by name).
* ListObjectsV2 entries carry only key, size and last-modified time: no ETag,
  storage class or owner. The continuation token is the last key returned.
* DeleteObjects ignores `Quiet` (reports every key as deleted) and never
  reports per-key errors: any failure fails the whole request.
* PutObject accepts only the `STANDARD` and `REDUCED_REDUNDANCY` storage
  classes (others fail with `InvalidStorageClass`) and does not store or
  return a storage class.
* UploadPartCopy ignores the `x-amz-copy-source-if-*` conditions (CopyObject
  honours them), so a concurrent overwrite of the source is not detected.
* Multipart uploads: upload IDs must be UUIDs (anything else is
  `InvalidRequest`, HTTP 400, where S3 answers `NoSuchUpload`, HTTP 404);
  they are bound to the credentials that created them; CompleteMultipartUpload
  requires parts numbered 1..n in order; only the `FULL_OBJECT` checksum type
  is supported (composite multipart checksums are 501).
* HeadObject and GetObject return stored checksums whether or not checksum
  mode is enabled.
* Requester pays: `x-amz-request-payer` is accepted and ignored; no
  `x-amz-request-charged` in responses.
* `TestS3` uses path-style addressing only (no virtual-host domain is
  configured).

**Works as on S3** (and is covered against s3s-fs): SigV4 header signing
(wrong secret and unknown key are rejected), SigV4 presigned GET and PUT
URLs (up to 7 days; the server rejects unsigned `x-amz-*` headers, which is
stricter than S3 and catches signing mistakes), ranged GET, user metadata and
content headers, `MetadataDirective` on CopyObject, multipart upload and
UploadPartCopy, conditional PutObject (`If-None-Match: *`, `If-Match`), MD5
ETags and `md5-of-md5s-N` multipart ETags.

**Checksums.** The SDK's default integrity protection works against s3s-fs
unchanged: request checksums when supported (CRC32 by default, sent as a
header or as an aws-chunked trailer, which s3s decodes and verifies) and
response checksum validation. No client setting is relaxed for tests.

### Versions and previews

`tests/tests/versions.rs` runs a simulated versioned bucket on `MockS3` (`serve_versions`
answers `ListObjectVersions` with S3's prefix, `max-keys` and marker rules, with or without a
server that restarts at the key marker), so every page boundary can be put anywhere: the same
filters must give the same groups for every request size and page size. It also covers the
requests of restore (copy source, directives, storage class, encryption), purge (batches of
1000, per-version failures), the bounded scan and the huge-key cap, and the download of one
version through the queue. `tests/tests/preview.rs` checks the ranged requests of previews on
`MockS3` (ASCII bodies only, because `MockResponse` bodies are strings) and real bytes on the
in-process server: pages that tile text of several scripts, PNG, WebP and JPEG signatures,
binary, empty and JSON. MinIO-only tests (real versioning) are ignored without the `minio`
feature.

## CLI tests (`crates/cli/tests/cli/`)

One test binary (`main.rs` with a module per command) runs the real `bb`
binary through `assert_cmd`, against a `TestS3` or, for behaviour s3s-fs
lacks (version listings, multipart-upload listings, per-key delete failures,
endless pagination), a `MockS3` (`bb_mock`). Every invocation gets a private
`BB_CONFIG_DIR` and has inherited `BB_*` and proxy variables removed, so the
developer's own configuration never leaks in. The tests pin the exit codes
(0, 1, 2, 3 and 130 for Ctrl-C on Unix), the NDJSON records of `--json` and
the JSON error records on stderr, `--dry-run` writing nothing, the
include/exclude precedence, presign durations, the page size sent to the
server, and that a closed standard output is not an error. MinIO-only CLI
tests use the same `cfg_attr` pattern; the CLI's `minio` feature enables the
harness feature:

```bash
BB_TEST_BACKEND=minio cargo test -p bucketbridge-cli --features minio
```

## Writing tests

* Use `TestS3::start()` per test; do not share servers between tests.
* Prefer `ops()` for the operation under test and `client()` or the helpers
  for setup, so a bug in the code under test cannot hide in the setup.
* Do not block a Tokio worker thread inside async tests. When a test has to
  wait synchronously (for example inside a progress callback), use
  `tokio::task::block_in_place` on a multi-thread runtime or
  `spawn_blocking`; a blocked worker can stall tasks queued on it, including
  connection drivers of a shared SDK client.
* The SDK's default HTTP client honours `HTTP_PROXY`, `HTTPS_PROXY`,
  `ALL_PROXY` and `NO_PROXY`. If your shell sets a proxy, add `127.0.0.1` to
  `NO_PROXY` before running the integration tests.
