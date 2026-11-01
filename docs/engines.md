# Transfer engines

The S3 data movement behind the transfer queue:
upload a file or a folder, download, copy, move, rename and delete. The
code is `bucketbridge_core::engine` (`crates/core/src/engine/`). The
engines know nothing about the queue; `S3Runner` runs them as the queue's
task runners (see "Transfer queue runners" below).

| File | Contents |
|---|---|
| `mod.rs` | `TransferEngine`, `TransferOptions`, shared helpers |
| `control.rs` | the seams: `Throttle`, `Progress`, `StopSignal`, `Outcome`, `Hooks` |
| `source.rs` | `UploadSource` (positional reads), `FileSource`, `SourceFingerprint` |
| `body.rs` | retryable, throttled request bodies over a byte range |
| `checksum.rs` | `Checksummer` and helpers for S3 checksum values |
| `content_type.rs` | `Content-Type` guessed from the file extension |
| `upload.rs` | the upload engine, `UploadState` |
| `download.rs` | the download engine, `DownloadSink`, `FileSink`, `DownloadState` |
| `remote.rs` | copy, move and delete |
| `expand.rs` | selections to work items, rename planning |
| `runner.rs` | `S3Runner`: the task runners of the transfer queue, `S3Provider`, `RunnerState` |
| `enqueue.rs` | the enqueue commands: `enqueue_upload`, ..., `enqueue_rename` |

## Seams

`TransferEngine::new(s3, options)` validates `TransferOptions` and is cheap
to clone. `TransferOptions::from_settings(&settings)` (or `from_parts`
with the two pages) reads the `transfer` and `data_integrity` settings
pages: multipart threshold and part size (MiB
to bytes), parts in flight per task, upload checksum algorithm, download
validation, and copy concurrency.

Every engine call takes `&Hooks`:

* `throttle: Arc<dyn Throttle>`. `acquire(bytes)` returns a boxed future
  that resolves when `bytes` may be transferred. Engines ask for at most
  `max_grant()` bytes per call (default 64 KiB; `Unthrottled` returns
  `u64::MAX`). Uploads acquire before a piece is handed to the connection,
  downloads after a piece arrived and before the next one is read. Bytes the
  SDK sends again on a retry pass through the throttle again. A limiter
  should make `max_grant()` no larger than what it grants in about one
  second, so that a slow limit never keeps a stream silent for long (the
  SDK's stalled-stream protection watches request and response bodies).
  `open_stream()` returns a `StreamPermit` once one more body may stream and
  keeps its place until the permit is dropped; the engines call it around
  every request that streams a body (each part, a single `PutObject`, a
  `GetObject`), so that a limiter at a very low rate can keep only as many
  streams alive as one round of grants can serve in time (the default allows
  any number).
* `progress: Arc<dyn Progress>`: `set_total`, `set_done` (absolute: the
  bytes already present when a transfer resumes, or 0 when it starts over)
  and `add_done` (increments). Bytes, except for deletes, which count
  objects. Upload bodies report from HTTP connection tasks, so
  implementations must be thread-safe and must not block. Bytes of a retried
  request are not counted twice.
* `stop: StopSignal`: a `tokio_util` `CancellationToken` plus a reason.
  `pause()` stops and keeps everything needed to resume; `cancel()` stops
  and cleans up. Cancel wins over pause in either order. A token cancelled
  directly (for example a child of an application-wide shutdown token)
  counts as a pause, so nothing is deleted without an explicit cancel.

A run ends with `Ok(Outcome::Completed(result))`, `Ok(Outcome::Paused)`,
`Ok(Outcome::Cancelled)` or `Err(error)` (an `Error` with its `ErrorKind`,
see `docs/architecture.md`). A stop that is already requested when a run
starts returns at once (a cancel still cleans up the state passed in).

Upload and download also take the resume state of an earlier run
(`Option<UploadState>` / `Option<DownloadState>`) and a checkpoint callback
`&(dyn Fn(&State) + Send + Sync)`. The callback receives the current state
whenever it changes; it is called from the task driving the engine, must
not block, and may be called often (after every part). Keep the latest
state in memory and persist it at your own pace; the states serialize with
serde (snake_case JSON). The last checkpoint before `Paused` or an error is
the state to pass back.

## Upload

`upload_file(path, dst, hooks, resume, checkpoint)` uploads one local file;
`upload(source, dst, content_type, ...)` uploads any `UploadSource` (a
positional byte source; the 5 GiB test uses a generated one). A destination
key ending in `/` creates a folder marker instead and does not read the
local path.

1. The file is opened once (symbolic links followed); its fingerprint is
   the size and modification time. All reads are positional reads of the
   needed range on the blocking thread pool; no file is ever loaded whole.
2. Below `multipart_threshold` (default 8 MiB): one `PutObject`.
3. Otherwise a multipart upload: part size
   `ops::part_size_for(size, part_size)` (default 8 MiB, grown in whole MiB
   to stay within 10,000 parts), up to `max_parts_in_flight` parts at a time
   (default 4). `CreateMultipartUpload` is never raced with the stop signal,
   and its upload ID is checkpointed at once, so an upload the server knows
   about is always in the state. The state is checkpointed again after every
   finished part.
4. Before `CompleteMultipartUpload`, the open file is checked again (size
   and modification time); a file changed in place fails the run and the
   incomplete upload is aborted. `CompleteMultipartUpload` is never raced
   with the stop signal either: once sent, the object exists or will. A
   single `PutObject` with the checksum set to `none` is checked after the
   request instead (see "Checksums").
5. The content type is guessed from the extension
   (`application/octet-stream` when unknown). Every request carries
   `x-amz-request-payer: requester` when the account pays for requests.

Request bodies stream the range in 256 KiB reads and hand them out in
throttle grants. A body is rebuilt from the source for every SDK retry, so
retries re-read the file.

### Checksums

With an algorithm configured (`crc32` default, `crc32c`, `crc64nvme`,
`sha1`, `sha256`), the engine computes the checksum of each request body
(the whole file, or each part) from the source before sending, with the
SDK's own implementations, and sends it as the `x-amz-checksum-*` header.
The body is therefore sent as is (no `aws-chunked` encoding, no trailer),
and the server rejects a body that does not match, so a file that changes
while it is read is never stored silently. The multipart upload is created
with the algorithm (`x-amz-checksum-algorithm`) and every part's checksum is
sent again in `CompleteMultipartUpload`. No `x-amz-checksum-type` is sent,
so S3's default applies: full-object checksums for `crc64nvme`, composite
checksums (a checksum of the part checksums, reported with a `-N` suffix)
for the others. With `none`, no checksum is sent where the API does not
require one, whatever the client's default. Then nothing makes the server
notice a body that mixes old and new content, so a single `PutObject` checks
the source again after the request: if the file changed meanwhile, the run
fails (`io`, which the queue retries) and the retry uploads the file again,
replacing the object that was stored from the changing file. Until then that
object exists and may be torn; with a checksum the server refuses such a
body, and a multipart upload is never completed from a changed file.

### Resume (`UploadState`)

```json
{
  "bucket": "photos",
  "key": "2024/trip.mov",
  "upload_id": "VXBsb2FkIElE",
  "source": { "size": 734003200, "modified": "2026-10-29T08:15:00.123456789Z" },
  "part_size": 8388608,
  "checksum_algorithm": "crc32",
  "parts": [ { "number": 1, "size": 8388608, "etag": "\"9b2cf535f27731c974343645a3985328\"", "checksum": "DUoRhQ==" } ]
}
```

Only multipart uploads have a state; a paused single-request upload starts
over. On resume:

* a state for another bucket or key is ignored (its upload is left alone);
* a changed fingerprint (size or modification time) or an unusable part
  size: the old upload is aborted and a new one starts;
* `ListParts` (all pages) must find the upload; `NoSuchUpload` starts a new
  one. The server's list is the truth: a listed part with the expected size
  and a checksum (from the listing, or from the state when the ETag
  matches) is reused, anything else is uploaded again. The part size and
  checksum algorithm of the state are kept even if the settings changed.

### Cleanup guarantees

* Cancel during a multipart upload: the parts in flight are dropped and the
  upload is aborted (`AbortMultipartUpload`, then `ListParts` to verify,
  repeated up to 3 times because S3 may still store a part that was in
  flight). No object is created. If the abort itself fails, the run returns
  that error and the state stays valid for `discard_upload`.
* Cancel during a single `PutObject`: the request is dropped; S3 stores an
  object only once its whole body arrived. A cancel that arrives after the
  last byte was sent may still find the object stored (complete, never
  partial).
* Pause keeps the multipart upload. Errors keep it too, so that a retry
  continues; the engine cannot know whether the queue will retry.
* `discard_upload(&state)` aborts the upload of a saved state. An upload that
  no longer exists counts as discarded on S3 and MinIO (s3s-fs answers a
  second abort with `AccessDenied`).
* An upload created on the server whose response never arrived (the process
  died during `CreateMultipartUpload`) cannot be known; the uncompleted
  multipart uploads tool lists such leftovers.

## Download

`download_file(src, dest, hooks, resume, checkpoint)` downloads one object
to a local file; `download_to(src, sink, ...)` streams into any
`DownloadSink` (the 5 GiB test uses a sink that checks every byte). A source
key ending in `/` that is a folder marker (an empty object, or no object at
all) creates the folder `dest` instead. S3 allows data under such a key: an
object that holds data is refused with `usage` (and is a failed entry in the
expansion) instead of being turned into a folder, which would drop its
bytes.

1. Parent folders are created and the object is streamed into
   `<dest>.bbpart` (`PARTIAL_SUFFIX`; the sync scanner skips it), through the
   throttle, in batches of up to 1 MiB written on the blocking pool. Names
   ending in `.bbpart`, in any letter case, are reserved for these partial
   files: nothing is downloaded under such a name (`is_partial_name`), so a
   partial file can never be the final file of another object, and the
   engine may reuse, truncate or delete any file it finds under such a name.
   `download_file` refuses a destination like that with `usage`, and the
   expansion reports an object like that as a failed entry (the same rule as
   the sync planner, which never syncs such keys).
2. A fresh download sends a plain `GetObject` and records ETag, size,
   `Last-Modified` and, when validation is on, the object's full-object
   checksum from the answer; that state is checkpointed at once (offset 0).
   While data arrives, the partial file is flushed to disk (`fsync`) and the
   state checkpointed at most once a second, on pause, and before an error
   is returned.
3. When every byte has arrived: the size must match, the partial file is
   flushed, its modification time is set to the object's `Last-Modified`
   (the sync planner relies on it), and it is renamed over `dest` (replacing
   an existing file), then the folder entry is flushed where the OS supports
   it.
4. Zero-byte objects work like any other.

Validation: with `validate_downloads` (default) every request asks for
checksums (`x-amz-checksum-mode: ENABLED`) and the SDK validates a full
answer as it streams; a mismatch discards the partial data. Ranged answers
carry no checksum, so a resumed download is checked against the saved
full-object checksum (composite `-N` values cannot be checked) by reading
the partial file back before the rename, in chunks of 1 MiB that check the
stop signal, so a pause or cancel never waits for a large file (a pause
keeps the file and a checkpoint with every byte present, and the next run
verifies again); a mismatch discards it and the retry starts over. With
validation off, no response checksum is validated whatever the client's
default.

### Resume (`DownloadState`)

```json
{
  "bucket": "photos",
  "key": "2024/trip.mov",
  "etag": "\"9b2cf535f27731c974343645a3985328\"",
  "size": 734003200,
  "last_modified": "2026-10-29T08:15:00Z",
  "checksum": { "algorithm": "crc32", "value": "DUoRhQ==" },
  "offset": 52428800
}
```

`offset` is the number of bytes flushed to the partial file. A state with
offset 0, an empty ETag or another bucket or key means "start over". On
resume the partial file is cut to `offset` (a file shorter than `offset`
starts over) and the rest is requested with `Range: bytes=<offset>-` and
`If-Match: <etag>`. The answer must also carry the same ETag, the expected
`Content-Range` and length, because some servers ignore `If-Match`. A
failed precondition (412), a range past the end (416) or a different
answer starts the download over from byte 0 (`DownloadOutcome::restarted`).

### Cleanup guarantees

* While a download runs, `<dest>.bbpart` is locked exclusively (standard
  library file locking); a second download to the same destination, from
  this process or another, fails instead of mixing its bytes in. The error
  has the kind `io`, which the queue retries (the other download is usually
  about to finish). The partial file is renamed into place while it is still
  open and locked. File systems without locking go ahead unlocked.
* A link is never followed. A new partial file is created exclusively; a
  symbolic link under the name (dangling or not) or a file with other hard
  links is replaced by a file of its own, so a link planted there cannot
  make the download overwrite what it points to; and just before the
  rename the name must still be the ordinary file that was written (a link
  swapped in meanwhile fails the run with `io`).
* Pause keeps `<dest>.bbpart`; it holds exactly `offset` bytes of the
  checkpointed version.
* Cancel deletes `<dest>.bbpart`; `dest` is never touched before the final
  rename, so a cancelled or failed download never leaves a partial `dest`.
* An error keeps the partial file for the retry, unless it is empty or its
  data failed validation.
* `discard_download(dest)` deletes the partial file (a missing file is
  fine; a planted link is removed, never its target). A partial file that a
  download still holds open, typically a run the queue dropped after its
  grace period while a write or a flush was in progress, is waited for, up
  to 30 seconds; if it is still in use then, the call fails with an `io`
  error and leaves the file, so that the caller can try again.

### Downloading one version

`TransferEngine::with_download_version(Some(id))` is a copy of the engines whose downloads read
the version `id` of every object (the `GetObject` carries `versionId`; a resumed request adds
`Range` and `If-Match` as usual). The queue carries the version in the task's resume state:
`versions::enqueue_download_version` adds an ordinary download task with
`TransferQueue::add_task_with_resume` and the state
`{"type": "version_download", "version_id": "...", "state": null}`. `S3Runner` builds the
engine with that version and stores every checkpoint of the download beside it
(`"state": {...DownloadState...}`), so a retry, a resume and a restart of the app read the same
version. Cleanup is that of any download (the partial file). Tasks and events show no version:
`src` is the object.

## Copy, move, rename, delete

* `copy(src, dst, hooks)`: `HeadObject` for the total, then
  `ops::S3::copy_object_with` (single `CopyObject` up to 5 GiB, otherwise
  `UploadPartCopy` parts with the source pinned by version and ETag). A
  copy cannot pause half-way: pause or cancel stops it (an unfinished
  multipart copy is aborted) and a resumed task copies again. Copying an
  object onto itself is a usage error. Folder markers are recreated as
  markers: a key ending in `/` is a marker when `HeadObject` reports size 0
  or the key does not exist; an object that holds data under such a key is
  copied like any other object, so that a move never replaces it by an empty
  marker and then deletes it. If an unfinished multipart copy cannot be
  aborted (the abort is part of the copy's error, with the upload ID), the
  upload stays open on the server; a copy whose future is dropped
  mid-flight aborts in the background and can only log a failed abort (the
  uncompleted multipart uploads tool lists such leftovers).
* `move_object(src, dst, hooks)`: the copy, then `DeleteObject` of the
  source only after the copy succeeded. Once the copy exists the delete is
  always attempted, even if a stop arrives. If it fails, the error (with the
  delete's kind) says that the copy exists and the source is still there;
  running the move again is safe.
* `plan_rename(bucket, from, to)` turns the `rename` command into move items.
  An object: `to` must not end in `/`; `conflict` when `to` exists. A
  prefix: `to` must end in `/` and must not lie inside `from`; `conflict`
  when anything exists below `to`; `not_found` when `from` is empty. There
  is a window between this check and the moves in which another client
  could create the destination.
* `delete(target, hooks)`: one key, or every object under a prefix
  (including its folder marker), listed and deleted 1000 keys per request.
  Progress counts objects; the total grows while the prefix is listed.
  Stopping between requests is safe and a resumed delete lists again. Keys
  the server refuses fail the run after everything else was handled; the
  error names up to five of them and takes its kind from the first failure
  (`AccessDenied` is `auth`). A key made only of `/` is refused.

## Expansion

The folder semantics of the enqueue commands (`docs/desktop-api.md`): a key
ending in `/` expands recursively, and the parent part of each selected key
is stripped. Each function returns `Expansion { items, failures }`: entries
that cannot be added are failures (`ExpandFailure { entry, code, message }`,
the "folder-add" failure state), and everything else is still expanded. A
selected prefix whose listing fails is one failure without partial items.

Collisions between selected entries reject the whole request: when two
selected sources would land on the same destination (the same name in the
destination folder, for example `a/report.txt` and `b/report.txt`, or a file
and a folder of the same name), the function returns `Error::Conflict` and
no items, before anything is listed or walked, so the queue enqueues
nothing. For downloads, names are compared the way the local file system
compares them: without regard to case on Windows and macOS. S3 keys are
compared exactly. Selecting the same source twice counts once.

* `expand_upload(paths, prefix)` (and `expand_upload_blocking`): files map
  to `prefix + name`; folders keep relative paths
  (`prefix + folder/sub/file`, `/` separators); a folder without uploadable
  content becomes a folder marker (`UploadItem::is_folder`). Entries are
  sorted by name. Links to files are followed; links to folders are not
  (failure), nor are broken links, devices, sockets and pipes. Names that
  are not UTF-8, files that cannot be opened, folders that cannot be listed
  and keys longer than 1024 bytes are failures. Two selected paths with the
  same name are a conflict. Files whose names end in `.bbpart` are the
  temporary files of downloads: inside a selected folder they are left out
  (as the sync scanner does), and a selected file like that is a failure.
* `expand_download(bucket, keys, dest_dir)`: every local path comes from
  `path::strip_parent` and `path::safe_local_path`, so a key that could
  escape `dest_dir` (`..`, absolute, drive letters, backslashes, and on
  Windows reserved names) is a failure and nothing is ever written outside
  `dest_dir`. A selected prefix also yields its own folder item, and folder
  markers yield folder items (create the folder, nothing to download).
  Inside one selected prefix, objects that cannot coexist as local files
  (keys that differ only in case on Windows and macOS, or `a` next to
  `a/b`) are `conflict` failures for the later entry, as sync reports such
  rows, and the rest is downloaded. Unicode normalisation differences are
  not detected. Objects with a name ending in `.bbpart` and objects that hold
  data under a key ending in `/` are `usage` failures (see "Download").
* `expand_copy` / `expand_move(src_bucket, keys, dst_bucket, dst_prefix)`:
  `photos/2024/` to `backup/` gives `backup/2024/...`. Copying or moving an
  object onto itself, moving a folder into itself and destination keys that
  are too long are failures. For a move, a selected key inside another
  selected folder is a `usage` error for the request (an object can only be
  moved once); copying it twice is allowed. Copying a folder into its own
  sub-folder is allowed (the listing is taken first).
* `expand_delete(bucket, keys)`: one item per key; a prefix is one item.

Sizes come from listings (`None` for single selected keys, which the engines
look up themselves).

## Transfer queue runners

`S3Runner` (`runner.rs`) runs the engines as the task runners of the
transfer queue (`bucketbridge_core::transfer`, `docs/transfers.md`).

```rust
let runner = S3Runner::new(provider, settings.data_integrity.clone());
let queue = TransferQueue::open(settings.transfer.clone(), runner.runners(), options).await?;
// When the settings are saved:
queue.update_settings(settings.transfer.clone())?;
runner.set_data_integrity(settings.data_integrity.clone());
```

* `S3Provider` (`Arc<dyn Fn(String) -> BoxFuture<'static, Result<S3>> + Send + Sync>`)
  resolves the S3 transport of an account id: the desktop host passes
  `Session::s3_for`, tests a fixed server. It is called at the start of
  every run and cleanup, so the run uses the account's current credentials.
* `runners()` registers the same runner for every `TransferKind`; it
  dispatches on the task:

  | Task | Engine call |
  |---|---|
  | upload (`dst` key ending in `/`: folder marker) | `upload_file(path, dst, ...)` |
  | download (`src` key ending in `/`: folder) | `download_file(src, dest, ...)` |
  | copy | `copy(src, dst, ...)` |
  | move (also the moves of a rename) | `move_object(src, dst, ...)` |
  | delete (key or prefix) | `delete(target, ...)` |

* Every run builds an engine from the run's transfer settings snapshot and
  the runner's data-integrity settings (`TransferOptions::from_parts`), and
  `Hooks` from the run's context:
  * the queue's `BandwidthLimiter` as the `Throttle`. `max_grant` is one
    second of the current rate shared by the throttle requests waiting at
    that moment, between 256 B and 64 KiB (the limiter's chunk size). The
    limiter serves waiters first in first out, so one round of all waiting
    streams takes about a second and no throttled stream goes silent for
    anywhere near the 20 s stalled-stream grace period (nor S3's idle
    timeout). Far below 64 KiB/s the 256 B floor would make a round
    longer, so the runner also bounds the number of streams (`open_stream`,
    shared by all runs): at most `rate x 5 s / 256 B`, a quarter of the
    grace period, which is 20 streams at the minimum cap of 1 KiB/s and 80
    at 4 KiB/s (the settings allow up to 32 tasks of 32 parts). The others
    wait before their request is sent, never inside a body.
  * the run's `Progress` (set total, set done, add done; objects for
    deletes).
  * a stop signal that receives the queue's stop request. Pause and cancel
    both stop the engine as a pause, keeping its state; after a cancel the
    queue calls the runner's `cleanup`, which aborts the multipart upload
    or deletes the partial file exactly once.
* The engines' checkpoints become the task's `ResumeState`, tagged by
  `type` (`RunnerState`): `{"type": "upload", ...UploadState}`,
  `{"type": "download", ...DownloadState}`, or `{"type": "rejected",
  "code": ..., "message": ...}` for an entry that could not be added. A
  state that does not deserialize (or belongs to another kind) counts as
  none: the transfer starts over.
* A run returns `Ok(())` only for a completed transfer; after a stop it
  returns the `cancelled` error of the queue's stop signal; engine errors
  keep their kind, so `remote` and `io` failures are retried by the queue.
* `cleanup(task, resume)`: an upload with an upload state calls
  `discard_upload` (an upload that no longer exists counts as discarded);
  a download deletes `<dest>.bbpart` unless another download is writing it
  (waiting up to 30 seconds for a download that still holds the file's lock
  and failing if it does not let go); a rejected entry, a copy, a move
  and a delete leave nothing behind. A cleanup with no state or a stale
  state is harmless, so it may run again after a restart. A cleanup that
  fails keeps the task (see below).

**A failed upload keeps its multipart upload.** An error (for example the
network going away in the middle of a part) leaves the upload ID and the
finished parts in the resume state and the multipart upload on the server,
so that the queue's automatic retries and `retry_task` continue with the
parts that are already there. The upload is aborted when the task is
cancelled, whether it was running, stopped or failed (the queue keeps failed
tasks until they are retried or cancelled). The ID of a new multipart upload
is saved to the queue file at once (`ResumeState::persist_now`), not after
the persist delay, so that a crash right after `CreateMultipartUpload` does
not leave an upload that nothing names. If the abort fails (the network is
down), the task does not become `cancelled`: it stays `failed` with its
resume state, and cancelling it again, or opening the queue again, repeats
the abort; an upload that no longer exists counts as aborted, so aborting it
by hand with the multipart uploads tool lets the next cancel succeed. The SDK retries each request
(standard mode, `advanced.max_attempts`); the queue's retries
(`transfer.retry_attempts`) re-run the engine with the last checkpoint.

### Enqueue helpers

`enqueue.rs` implements the enqueue commands of `docs/desktop-api.md` on top
of the expansion; each returns the created `TransferTask`s:

```rust
pub async fn enqueue_upload(queue: &TransferQueue, account_id: &str, local_paths: &[PathBuf], bucket: &str, prefix: &str) -> Result<Vec<TransferTask>>;
pub async fn enqueue_download(queue: &TransferQueue, s3: &S3, account_id: &str, bucket: &str, keys: &[String], dest_dir: &Path) -> Result<Vec<TransferTask>>;
pub async fn enqueue_copy(queue: &TransferQueue, s3: &S3, account_id: &str, src_bucket: &str, keys: &[String], dst_bucket: &str, dst_prefix: &str) -> Result<Vec<TransferTask>>;
pub async fn enqueue_move(queue: &TransferQueue, s3: &S3, account_id: &str, src_bucket: &str, keys: &[String], dst_bucket: &str, dst_prefix: &str) -> Result<Vec<TransferTask>>;
pub async fn enqueue_delete(queue: &TransferQueue, account_id: &str, bucket: &str, keys: &[String]) -> Result<Vec<TransferTask>>;
pub async fn enqueue_rename(queue: &TransferQueue, s3: &S3, account_id: &str, bucket: &str, from_key: &str, to_key: &str) -> Result<Vec<TransferTask>>;
```

* All tasks of one call share a new `group_id`; sizes known from the
  listing (or the local file) become `bytes_total`.
* An error of the expansion, including a `conflict` between two selected
  entries that would land on the same destination, rejects the call and
  enqueues nothing.
* Each entry that could not be added (`Expansion::failures`: an unreadable
  file, an unsafe key, a folder that cannot be listed, ...) becomes a task
  in the `failed` state, added together with the others by
  `TransferQueue::add_tasks_with_failures`. Its error is the reason, its
  source the entry, its destination the selected destination folder or
  prefix, and its resume state `rejected`: a retry fails again with the
  same reason (the runner checks this before anything else, so nothing is
  read or written), and a cancel touches nothing. To try again after fixing
  the problem, select the entry again.
* `enqueue_rename` turns `plan_rename` into move tasks (`conflict` when
  the destination exists).

Every bucket-scoped SDK request the engines send themselves goes through one
helper (`bucket_client` in `mod.rs`, which calls `S3::client_for`, so the
bucket's region is used). Their metadata requests (`CreateMultipartUpload`,
`ListParts`, and `GetObject`, whose timeout ends with the response headers)
get the per-request read timeout of `S3::metadata_timeout`; requests that
send a body (`PutObject`, `UploadPart`) and `CompleteMultipartUpload` rely
on stalled-stream protection instead.

## Tests

`tests/tests/engines.rs` runs against s3s-fs (and MinIO with the `minio`
feature): sizes around the multipart threshold, every checksum algorithm,
pause and resume of uploads and downloads, cancel and discard, changed
sources and changed objects, corrupted partial data, folder round trips with
nested folders, spaces and Unicode names, expansion rules, copy, move,
rename and delete. `MockS3` covers request-level contracts (explicit
checksum headers, plain bodies, part checksums in the completion, the
request-payer header, `Range` and `If-Match` on resume, a 412 restart) and
behaviour s3s-fs lacks (per-key delete failures, a failing delete after a
copy). MinIO only: the round trip of empty folders (s3s-fs never lists
folder markers) and the 5 GiB round trip, which also needs
`BB_TEST_LARGE=1`:

```bash
BB_TEST_BACKEND=minio BB_TEST_LARGE=1 cargo test --release -p bucketbridge-tests --features minio --test engines five_gib
```

It generates and verifies the data as streams (nothing is stored locally),
pauses the upload after 1010 of 1024 parts so that resuming reads the part
list across two pages, and pauses the download half-way.

`tests/tests/queue_s3.rs` runs the queue with `S3Runner` against s3s-fs:
every task kind and the enqueue helpers, folders with their relative paths,
a paused multipart upload that resumes with its finished parts, cancels
that abort the multipart upload and delete the partial file, a restart in
the middle of a transfer, a failed upload (a relay between client and
server is cut in the middle of a part) whose retry continues the same
upload and whose cancel aborts it, the bandwidth cap across an upload and
a download, the concurrency cap, two accounts in one queue, and entries
that cannot be added (including unsafe keys from a scripted server).

`tests/tests/transfer_regressions.rs` pins the bugs an independent review
found: `.bbpart` names and planted links at the partial name, a cancel whose
cleanup fails, folder keys that hold data, a single `PutObject` of a file
that changes while it is read, stopping a verification, the cleanup of a
partial file a dropped run still holds, the bound on streams under a
bandwidth cap, and the multipart upload ID reaching the queue file at once.
