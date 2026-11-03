# Folder synchronisation

This document describes how BucketBridge compares a local folder with an S3 prefix
(task T10) and how a sync plan is stored so that executing it can be resumed. The code
lives in `crates/core/src/sync/` (`scan.rs`, `plan.rs`, `store.rs`, `execute.rs`, `mod.rs`); the
filter engine it uses is described in `crates/core/src/filter.rs`.

## Overview

A sync runs in two phases:

1. **Analysis** (`sync_analyze`, `bb sync --dry-run`). The local folder is scanned, the
   S3 prefix is listed (flat, without delimiter), and the planner produces one row per
   relative path that needs attention. Analysis writes nothing, neither locally nor to
   S3. The result is a `SyncPlan` with a new UUID v4 id.
2. **Execution** (`sync_execute`). The executor runs the rows that are still pending and
   records each result in the plan store, so a cancelled or crashed run continues where
   it stopped when it is started again (see "Executing a plan").

In code: build an `AnalyzeRequest` (account, local folder, `S3Uri` of the remote
folder, direction, `delete`), list the remote folder flat with
`S3::list_all(bucket, key)` (or `list_all_stream`), and call
`sync::analyze(&request, listing)`; `SyncStore::insert` stores the plan. Analysis
walks the file system, so call it from a blocking thread. The plan's `local_root` is
the absolute local path and its `s3_prefix` the folder `S3Uri`.

## Rows

Every row has a `rel_path`: the path relative to the local folder and to the S3 prefix,
with `/` as separator on every OS. Rows are sorted by `rel_path` (byte order).

| action   | target           | done at start | meaning                                          |
|----------|------------------|---------------|--------------------------------------------------|
| `copy`   | `local`/`remote` | false         | copy the file to the target side                 |
| `delete` | `local`/`remote` | false         | delete the file from the target side             |
| `equal`  | none             | true          | both sides already agree                         |
| `failed` | none             | false         | cannot be synced; `error` says why; never executed |

`local_meta` (size and modification time) and `s3_meta` (the listing entry) are filled
for each side on which the path exists (`local_meta` only when the local metadata could
be read). A `failed` row comes from the analysis; when an execution attempt fails, the
row keeps its `copy` or `delete` action, gets an `error` and stays `done: false`, so the
next run retries it (see "Plan store").

## Rules by direction

"Newer" always means newer by **more than the tolerance** (default 2 seconds, see below).

### upload (local to S3)

| local     | S3        | result                                             |
|-----------|-----------|----------------------------------------------------|
| present   | missing   | copy to remote                                     |
| present   | present   | copy to remote if the sizes differ or the local file is newer, otherwise equal |
| missing   | present   | delete from remote with `delete`, otherwise no row |

Example: `report.pdf` is 10 bytes locally and 12 bytes in S3: copy to remote, even if
the S3 object is newer (upload makes S3 match the folder).

### download (S3 to local)

The mirror image: copy to local when the sizes differ or the object is newer; local
files that do not exist in S3 are deleted with `delete`, otherwise they produce no row.

### bidirectional

| local     | S3        | result                                           |
|-----------|-----------|--------------------------------------------------|
| present   | missing   | copy to remote                                   |
| missing   | present   | copy to local                                    |
| present   | present   | the side newer by more than the tolerance is copied over the other |
| present   | present   | same size, times within the tolerance: equal     |
| present   | present   | different sizes, times within the tolerance: failed (conflict) |
| present   | present   | different sizes, a time missing: failed (conflict) |

Example: `notes.txt` is 120 bytes locally (modified 10:00:00) and 118 bytes in S3
(modified 10:00:01). Both changed and neither is clearly newer, so the row fails with
an explanation instead of guessing.

`delete` cannot be combined with bidirectional sync (usage error). Without a record of
earlier runs, a file that exists only locally may be new locally or deleted from S3,
and the two cases look exactly the same.

### Time tolerance and missing times

- The default tolerance is 2 seconds (`DEFAULT_TOLERANCE`), configurable per analysis.
  A difference of exactly the tolerance counts as equal; one nanosecond more counts as
  newer.
- File systems and S3 store times with different precision (S3 listings have whole
  seconds), which is why a tolerance is needed.
- A missing modification time (on either side) never counts as newer. With equal sizes
  such a pair is equal; in bidirectional mode with different sizes it is a conflict.
- Clock skew: S3 times come from the server's clock and local times from the local
  clock (or from wherever the file was copied). Skew larger than the tolerance makes
  time comparisons unreliable, mostly in bidirectional mode. Files written by the
  downloader carry the object's time (see the executor requirements), so they compare
  equal regardless of skew.

## Local scan

- Only files are synced. Folders are walked iteratively (no recursion depth limit) and
  empty folders are ignored, in both directions.
- **Symbolic links to folders** (and Windows junctions) are never followed, so link
  cycles are impossible. Each such link is a failed row, and remote keys below it are
  failed too (the local contents are unknown, so nothing below it may be copied into it
  or deleted remotely). On Unix a folder reached twice through bind mounts is reported
  as a loop.
- **Symbolic links to files are followed**: the link is synced as a regular file with
  its target's size and time. Uploading reads the target; a download replaces the link
  with a regular file (the downloader writes a temporary file and renames it over the
  link, it never writes through the link). A link whose target is missing is a failed
  row; a link whose target cannot be examined (no permission) is a failed row that is
  treated like an unreadable folder.
- **Unreadable entries are failed rows, never silently dropped**: a folder that cannot
  be listed (its subtree is treated as unknown, see above), a folder whose listing
  fails part way (reported as a whole), an entry whose type cannot be read (treated like
  an unreadable folder, since it may be one), a file whose metadata cannot be read, and,
  when the plan may upload (upload, bidirectional), a file that cannot be opened for
  reading.
- All these local problems (including the links, special files and names below) become
  failed rows whenever the direction would act on the entry: always for upload and
  bidirectional, and for download when S3 has the same path or `delete` is on. A
  download without `delete` leaves local-only entries alone, so, like readable
  local-only files, they produce no row. They still count as existing names for the
  case-collision and file/folder checks.
- Devices, sockets and pipes are failed rows; they are never opened.
- **Names that are not valid UTF-8** cannot become S3 keys. They are failed rows whose
  `rel_path` is a printable form with the invalid bytes written as `\xNN` (or unpaired
  UTF-16 surrogates as `\u{XXXX}` on Windows). Such folders are not entered. (macOS
  APFS does not allow such names at all; Linux file systems do.)
- Files whose name ends in `.bbpart` are the downloader's temporary files and are
  skipped. A symbolic link with such a name is skipped only when it leads to a file or
  to nothing; one that leads to a folder (or cannot be examined) is a failed folder entry
  like any other link, so remote keys below it are never copied or deleted. Real folders
  with such a name are scanned normally.
- A missing local folder is an error for upload (otherwise `delete` would delete the
  whole prefix) and an empty folder for download and bidirectional. A path that is not
  a folder is a usage error. A relative path is made absolute against the current
  directory, and the plan stores the absolute path.

## Remote side

- The listing must be flat (no delimiter). Folder markers (keys ending in `/`) and
  prefix entries are ignored; keys outside the prefix are ignored; if a key appears
  twice, the last entry wins.
- The prefix names a folder: `s3://bucket/photos` is analysed as
  `s3://bucket/photos/` (`sync::as_folder`), so `photos-old/x` is never part of it.
  The whole bucket is `s3://bucket/`.
- Keys that cannot be stored as a local file are failed rows **when the plan would
  write them locally** (download, bidirectional). The check is the path module's
  `safe_local_path`: it rejects `..`, absolute keys, backslashes, drive letters and
  NUL, empty or `.` segments (`a//b` and `a/./b` would land on the same file as
  `a/b`), and on Windows names Windows cannot store (`< > : " | ? *`, control
  characters, trailing dots or spaces, device names such as `CON` or `nul.txt`). In
  upload mode such keys are ordinary remote objects (and can be deleted with
  `delete`).
- Keys whose name ends in `.bbpart` are never acted on: because local scans skip such
  names, the local state of the path is unknown. Any row for them would be failed
  (upload without `delete` produces no row).

## Filters

A sync can be limited with a `FilterSpec` (see `filter.rs` for glob syntax and bounds).

- `prefix` and `glob` apply to `rel_path` and are applied to **both** sides before
  comparing, so a path is either in scope on both sides or on neither. Folders that
  cannot contain a match are not scanned.
- Size, date and storage-class criteria differ per side, so they are judged on the
  entry that an action reads or removes: the source of a copy, the file being deleted.
  Equal and conflict rows are kept when the side the direction treats as the source
  matches (either side for bidirectional). Local files count as storage class
  `STANDARD`.
- The path criteria hide local names from the analysis, so a copy to a local path the
  scan did not see is checked against the file system (see "Targets the analysis did not
  see").
- An entry that fails the metadata criteria is **never treated as absent**. Example:
  with `min_size = 100`, uploading with `delete`, local `a.bin` is 10 bytes and the S3
  copy is 500 bytes. If the filter removed the small local file from the comparison,
  the planner would see "only in S3" and delete the 500-byte object. Instead the path is
  simply out of scope: no copy, no delete.

## Case and normalization collisions

macOS (by default) and Windows ignore letter case in file names, so `Readme.md` and
`README.md` are one file there. macOS file systems also ignore the Unicode normalization
form: `café` written with a composed `é` and with `e` plus a combining accent
are one file, but two different S3 keys. When the plan writes locally (download,
bidirectional), the planner compares all local paths and all paths it would create,
segment by segment, after Unicode normalization (NFC) on every file system and, when the
local file system ignores case, after Unicode lowercasing. Every row that would create,
overwrite or delete a path whose spelling collides with another one is failed with an
explanation:

- S3 has `Readme.md` and `README.md`: both copies fail.
- The local folder is `Photos/`, S3 uses `photos/`: copying `photos/a.jpg` fails, and
  with `delete` deleting `Photos/a.jpg` fails too (both rows would hit the same file).
- Local `Docs/a.txt`, S3 `docs/b.txt`, bidirectional: copying `docs/b.txt` to local
  fails (it would end up in `Docs/`); uploading `Docs/a.txt` is fine.

- S3 has `café.txt` (composed) and `café.txt` (decomposed): both copies fail, on
  every file system, because a folder shared with macOS could not hold both. (Names
  are folded for normalization whatever the file system does, because that cannot be
  probed without writing.)

Equal rows and uploads are not affected. Whether the file system ignores case is
probed read-only: an existing name is looked up with its ASCII letters' case swapped
(same file: case-insensitive; missing or another file: case-sensitive). Windows is
always treated as case-insensitive, and so is a folder without a usable name (the
safe choice: it can only produce extra failed rows, never an overwrite).

Related: a file and a folder cannot have the same local name. A copy to local fails
when one of its parent segments is a local file that stays, or when its own name is
needed as a folder by another path (S3 can hold both `a` and `a/b`).

## Targets the analysis did not see

The analysis cannot see every name: a path filter hides local names, and letter case and
Unicode normalization are approximations of what the file system does. A copy whose
target the analysis saw nothing at is therefore checked against the real target, at three
moments, and **never overwrites anything it did not see** (in every direction):

1. In the analysis, for a copy to a local path with no local entry, when a path filter is
   set or the name is not plain ASCII: the path is looked up with `lstat`. A file,
   folder or link that exists there turns the row into a `failed` row ("never
   overwritten"). Example: filter prefix `docs/`, local `Docs/a.txt`, S3 `docs/a.txt`, on
   a case-insensitive file system.
2. Right before the download starts: the same check. Local entries that appeared since
   the analysis (a file, or a folder at the destination) fail the row. The one exception
   is the result of an earlier attempt of the same row: a regular file with the object's
   size and `Last-Modified` as its time counts as done (see "Rows whose result was
   lost").
3. Right before the downloaded file is moved into place (see "Executing a plan").

For a copy to S3 whose object the analysis did not see: the object is looked up with a
`HEAD` request before the upload; one that exists fails the row, and the upload carries
`If-None-Match: *` so that a server that supports conditional writes also refuses an
object created during the transfer.

## Key length

S3 keys are at most 1024 bytes (`s3uri::MAX_KEY_BYTES`). A copy to S3 whose key
(prefix plus `rel_path`) would be longer is failed.

## Plan store

Plans are kept in `data_dir/sync/` by `SyncStore`, two files per plan:

- `<id>.json`: the whole plan. It is only ever replaced atomically: written to a
  temporary file in the same folder, flushed with fsync, renamed over the old file.
- `<id>.journal`: an append-only log of row updates, one JSON line each,
  `{"i":12,"done":true}` or `{"i":12,"done":false,"error":"access denied"}`.

Recording a result (`mark_done`, `record_error`, or a batch of both with `update_rows`)
appends one line with a single write, so it costs the same for a 100,000-row plan as
for a small one (the tests record 100,000 results in well under a second). `get`
replays the journal over the snapshot; `compact` writes the result as a new snapshot
and removes the journal. `list` returns summaries (id, roots, direction, counts) newest
first; `counts` returns the progress counts of one plan; `delete` removes both files.
Plan ids from the UI are validated as UUID v4 before they are used in a file name.

Retention: the desktop stores one plan for every analysis, so `SyncStore::insert` prunes
the store afterwards, deciding from the files' modification times alone (a plan is never
read for it). The store keeps at most 50 plans (`MAX_PLANS`, the most recently touched),
removes plans that were not touched for 30 days (`MAX_PLAN_AGE`, finished or not: a plan
that old mostly fails as "changed since the analysis"), and never removes the plan just
inserted or one touched within the last hour (`ACTIVE_GRACE`, it may be running). A plan
that cannot be removed stays. `get` and `list` still read whole plan files, so keep the
number of stored plans small.

Durability and crashes: the journal is not fsynced per line. An application crash
loses nothing; a power failure may lose the last updates, which only means those rows
are executed again. Replay ignores a torn last line and skips damaged lines; a line is read with a length
limit (`MAX_JOURNAL_LINE`, 64 KiB; error texts are cut to 8 KiB when they are recorded), so
a long zero-filled tail cannot use unbounded memory; before the
first append, a torn last line (or a zero-filled tail) is cut off so it cannot merge
with the new line. Each line sets absolute values, so replaying a journal that is
already part of the snapshot (a crash in the middle of compaction) changes nothing.

Only `copy` and `delete` rows can be updated (anything else is a usage error, and a
batch is all or nothing), and the planned action is never changed. `mark_done` sets
`done: true` and clears `error`; `record_error` sets `error` and keeps `done: false`,
so the next run retries the row. Execution never stops at the first failure.

Progress (`SyncProgress`) is defined once, in `sync::RowCounts` (built by
`sync::row_counts(&plan.rows)` or `SyncStore::counts(id)`), always over the whole plan:

- `total`: the `copy` and `delete` rows,
- `done`: those of them with `done: true`,
- `failed`: the `failed` rows plus the rows that are not done and have an `error`.

`RowCounts::pending()` (`total - done`) is what the next run executes. The "failed
only" view shows the rows for which `sync::row_is_failed` holds (action `failed`, or an
`error` on a row that is not done; a row marked done never keeps an error), and
`sync::row_is_pending` selects the rows a run executes.

The store is thread-safe and serialises work per plan; share one `SyncStore` per folder
(for example in an `Arc`), and let only one process use a store folder at a time.

## Executing a plan

`sync::SyncExecutor` (`crates/core/src/sync/execute.rs`) executes a stored plan with
the transfer engines (`docs/engines.md`), not through the transfer queue: the plan
store's journal is what makes execution resumable.

```rust
// Once per process: one executor over the plan store.
let executor = SyncExecutor::new(Arc::new(SyncStore::open_in(&dirs)?));

// sync_execute(plan_id)
let summary = executor.store().summary(&plan_id)?;          // not_found, usage
let s3 = session.s3_for(&summary.account_id).await?;
let engine = TransferEngine::new(s3, TransferOptions::from_settings(&settings))?;
let options = ExecuteOptions::new(&settings, queue.limiter()); // CLI: ExecuteOptions::standalone(&settings)
let run = executor.start(&plan_id, engine, options, events)?;  // conflict while it runs
tokio::spawn(run.run());                                       // -> Result<RunSummary>

// sync_cancel(plan_id)
executor.cancel(&plan_id)?;                                    // false when not running
```

`events` implements `SyncEvents`: `progress(&SyncProgress)` and
`row(plan_id, index, &SyncRow)`, which the desktop host emits as `sync://progress` and
`sync://row`. `SyncProgress` serializes exactly as the contract's type.

How a run works:

1. The plan is loaded. Only pending rows run (`copy` and `delete` rows that are not
   done, including those whose last attempt failed); `equal` and `failed` rows are
   never touched.
2. Local deletions run first, then every folder that this run's deletions emptied is
   removed, deepest first. Only deletions done by this run count (a folder around a row
   done by an earlier run may have been used again), every path is checked like the
   deletions were (safe segments, no symbolic link or file among the folders on the
   way), and the sync root, symbolic links and folders that still hold anything are never
   touched. This is what lets a file become a folder of the same name, or the other way
   round.
3. Uploads, downloads and remote deletions then run concurrently,
   `settings.transfer.max_concurrent_tasks` rows at a time, through the bandwidth
   limiter (pass the queue's, so one cap covers both).
4. Every result is written to the plan store with `update_rows`, in batches (at most
   64 results or 200 ms), and only then reported with `row` and `progress`. A crash
   loses at most the rows in flight; they run again next time.
5. When the run ends (completed, with failures, or cancelled), the remaining batch is
   recorded, the plan is compacted and `progress` is reported once with
   `finished: true`. `run()` returns a `RunSummary` (rows attempted, errors, whether it
   was cancelled, final progress); row failures are not errors of the run.

Per row:

- A failure never stops the others. The error is recorded, the row keeps its action
  and stays `done: false`; running the plan again retries it, and success clears the
  error.
- Downloads go through the download engine, into a temporary file of their own next to
  the target (`.<name>.<random>.bbpart`, so scans skip it; the engine writes
  `<that name>.bbpart` first). The file gets the object's `Last-Modified` as its
  modification time. Right before it is moved into place the target is checked again:
  a target the analysis did not see must still not exist (the file is created with a
  hard link, which fails when the name exists, so nothing created meanwhile can be
  replaced; without hard links a check and a rename leave a window of microseconds), and
  in bidirectional mode the file must still be the one the analysis saw. A one-way
  copy over a file the analysis saw replaces it by design (a symbolic link at the file's
  place is replaced, never written through). The remaining window is the few
  microseconds between that check and the rename.
- A failed or cancelled row leaves no partial file and no open multipart upload: the
  executor discards the temporary download files, and aborts the multipart upload of a
  failed upload (it keeps the checkpoints of the engine for that), so a failed row
  does not leak data on either side. (After a crash or a dropped `run()` future a
  `*.bbpart` file or an incomplete multipart upload can remain; scans ignore the
  files, and the uncompleted multipart uploads tool lists the uploads.)
- Uploads of an object the analysis did not see, and every bidirectional upload, look
  at the object with a `HEAD` request first and then carry an S3 conditional write:
  `If-None-Match: *` for a new object, `If-Match: "<etag>"` for the object that was
  checked. A server that supports conditional writes (AWS S3, MinIO and others) then
  refuses (412) when the object changes during the transfer, the row fails and the
  object stays. The server evaluates the condition when it commits the object: S3 does
  so at the end of a single `PutObject` and of `CompleteMultipartUpload`; a server that
  checks it when the request starts (s3s-fs does, for single requests) or ignores the
  header leaves a window of the transfer's duration, which the `HEAD` request before
  it narrows to the time the transfer takes. A server that answers 501 (conditional
  writes not implemented) gets the upload without the header. A one-way upload over an
  object the analysis saw replaces it by design, without a check.
- After an upload the local file gets the new object's `Last-Modified` (from a HEAD
  request) as its modification time, unless the file changed during the upload or is a
  symbolic link (a link is never written through; its target lies outside the folder, so
  the next bidirectional run replaces the link with a regular copy once). In
  bidirectional mode this always happens; otherwise the next analysis would download the
  file back. In upload-only mode it happens only for a file dated later than the object
  by more than the tolerance (the local clock runs ahead of the server's, or the file
  is dated in the future), which would otherwise be uploaded again on every run; an
  ordinary file keeps its own time, and the extra `HEAD` request is only made for files
  modified within the last 30 minutes (requests are refused when the clocks differ by
  more than 15 minutes, so older files cannot be ahead). This is best effort: when it
  cannot be done, the next run does the work once more.
- Nothing is read, written or deleted through a folder that has become a symbolic
  link (or junction) since the analysis; such rows fail.
- A deletion removes only what the analysis saw. An object (size, `Last-Modified` to
  the second, ETag when both sides know it) or a file (size and modification time)
  that changed since is kept and the row fails ("analyze again"). A target that is
  already gone counts as deleted.
- A copy never overwrites a target the analysis did not see, and in bidirectional mode
  it also requires its target to be unchanged (or still missing), before the transfer and
  again at its end (see above), so a change made on the other side after the analysis
  is never overwritten. One-way copies replace a target the analysis saw by design.
- Rows whose result was lost. A crash inside the batch of results, or a cancelled
  request that was stored anyway, leaves a row pending whose work is done. When such a
  row runs again and its target already is the source, it counts as done instead of
  failing as "changed since the analysis": a download target that is a regular file with
  the object's size and `Last-Modified` as its time; an upload target with the local
  file's size and either the time adopted from it (within the tolerance) or, for an
  object uploaded in one request, an ETag equal to the MD5 of the file. A multipart
  object whose time was not adopted cannot be recognized and fails once.

Cancel and concurrency:

- `cancel` stops the run promptly: the transfers in flight are cancelled through the
  engines, which abort multipart uploads and delete partial downloads, and their rows
  stay pending. The plan resumes with the next `start`. (A `PutObject` whose last byte
  was already sent may still be stored, complete, while its row stays pending; the
  next run uploads it again.) Dropping the `run()` future instead skips that cleanup.
- One execution per plan at a time: `start` returns `conflict` while the plan runs
  (whatever the spelling of its id). The claim ends when the run ends or the
  `SyncRun` is dropped.

## Saved jobs, exclusion rules and options (v0.2.0)

`sync::SyncOptions` (`crates/core/src/sync/options.rs`) is everything a sync takes besides
the two locations, the direction and `delete`. A saved sync job (below) stores one, and
`bb sync` builds one from its flags. Every field has a default that keeps the behaviour
described in the rest of this document.

| Field | Default | Meaning |
|---|---|---|
| `filter` | none | the `FilterSpec` of "Filters" above |
| `include` | none | globs; when given, only matching paths are synced |
| `exclude` | none | gitignore-style rules, one per entry |
| `exclude_file` | none | a file of rules, read at every run; its rules come after `exclude` |
| `skip_empty_folders` | `true` | see "Empty folders" below |
| `skip_duplicate_content` | `false` | see "Duplicate content" below |
| `use_cache` | `true` | see "The listing cache" below |
| `parallelism` | `4` | remote folders listed at the same time, 1 to 32 |

`SyncOptions::compile_filter` turns the include globs and the rules into the path
criteria of the `Filter`, so they apply to both sides before the comparison, exactly like
`prefix` and `glob`: a path that is excluded is neither copied nor deleted on either
side. A missing or unreadable exclusion file is an error (`not_found` or `usage`), never
"no rules": a typo must not make a sync touch more than intended.

### Exclusion rules

The rules follow `.gitignore` (`crates/core/src/ignore.rs`):

- blank lines and lines that start with `#` are skipped (`\#` is a literal `#`),
- a leading `!` includes again what an earlier rule excluded (`\!` is a literal `!`); the
  last matching rule decides,
- a trailing `/` makes the rule match folders only,
- a pattern with a `/` at the start or in the middle is anchored to the synced root; one
  without matches at any depth,
- `*`, `?`, `[...]` and `**` have the meaning described in `filter.rs`,
- trailing spaces are dropped unless escaped with `\`.

Excluding a folder excludes everything below it, and a rule cannot include a file again
below an excluded folder (as in git). Folders that are excluded are not scanned.

```
# build output
build/
*.tmp
!keep.tmp
/notes.txt
logs/**/debug.log
```

### Saved jobs

`syncjobs` (`crates/core/src/syncjobs.rs`) keeps named jobs in `config.toml` under
`[[sync_jobs]]` (left out of the file while there are none). A job holds the account,
the direction, the local folder (or, for `s3_to_s3`, the source prefix), the destination
prefix, `delete` and the `SyncOptions`. Names are 1 to 100 characters without control
characters and unique without regard to letter case; at most 200 jobs can be stored;
deleting an account deletes its jobs. Jobs are created, replaced and deleted only through
`ConfigStore::update`, and saving validates everything that does not need the network
(the direction against the locations, `delete` with `bidirectional`, the options).

A job is run by analysing it into a stored plan: the desktop `run_sync_job` command does
that and the plan is reviewed before `sync_execute`; `bb sync --job NAME` analyses and
executes it like `bb sync` does. Nothing in a job executes by itself.

### Bucket to bucket sync

The direction `s3_to_s3` makes one prefix match another one, in the same bucket or in
another bucket of the account, with server-side copies (`TransferEngine::copy`, which
uses `UploadPartCopy` for large objects). The plan has the `source_prefix`, `s3_prefix`
is the destination and `local_root` is empty. The planner is the one of `upload` with the
source listing in the place of the local folder, so the rules are those of "upload (local
to S3)": a missing or different object (different size, or a source modified more than
the tolerance after the destination) is copied, and with `delete` objects that exist only
in the destination are deleted. In a row, `local_meta` is the source object's size and
`Last-Modified` and `s3_meta` is the destination object; `target` is always `remote`. The
two prefixes must not be the same or contain each other (usage error). Storage-class
criteria of a filter see the source as `STANDARD`. Before a copy the executor looks at
both objects: a source that changed or vanished, and a destination object that appeared
or changed since the analysis, fail the row ("analyze again"); nothing is overwritten that
the analysis did not see.

### Empty folders

By default empty folders are ignored and folder markers are not synced. With
`skip_empty_folders` off the plan also carries folder rows, whose `rel_path` ends in `/`:
a copy to the remote side creates the folder marker for an empty local folder (and, for
`s3_to_s3`, for a marker of the source) that has none, and a copy to the local side
creates the empty local folder for a marker whose folder does not exist. A folder that
holds files needs no row, an existing path (a file, a link) is left alone, folders are
never created through a link, and markers are never deleted by a sync.

### Duplicate content

With `skip_duplicate_content` a copy row whose two sides have the same size and the same
content becomes an `equal` row. The size must match first, so a file is only read when a
match is possible. The content is compared with what S3 stores, cheapest first: checksums
in the listing, then the ETag when it is the MD5 of a single-request upload, then one
`HEAD` request for the stored whole-object checksum (SHA-256, SHA-1, CRC-32C, CRC-32 or
CRC-64/NVME), which is computed over the local file. Whatever cannot be decided (a
multipart ETag without stored checksums, an ETag of an encrypted object, an unreadable
file) stays a copy row. For `s3_to_s3` the two objects' ETags and checksums are compared.
At most `parallelism` comparisons run at the same time.

### Parallel analysis and the listing cache

The local scan and the remote listing run at the same time. The remote prefix is listed
with the delimiter `/` first and every sub-folder is then listed flat, at most
`parallelism` at a time (`sync::listing::list_folder`); the result is the same set of
objects as one flat listing.

The metadata of a full listing is kept per (account, bucket, prefix) in
`data_dir/sync-cache/` (0600, replaced atomically, at most 200 files and 7 days). A later
listing is answered from it when the snapshot is younger than 5 minutes and the first
page of the prefix, fetched with one request, equals the start of the snapshot (key,
size, ETag, `Last-Modified`, storage class) and the snapshot ends where the page ends.
The cache is **never used when the sync deletes** (`delete`): a stale entry must not
decide a deletion, so such a sync always lists afresh, and it is not used when
`use_cache` is off (`bb sync --no-cache`). A change deep inside a long listing that leaves
its first page unchanged is not noticed for up to 5 minutes; execution still never
overwrites or deletes anything that changed since the analysis (see "Executing a plan").

### Run report

`sync::SyncReport` (`crates/core/src/sync/report.rs`) summarises the final state of a
plan's rows: per kind of action (`upload`, `download`, `copy`, `delete_local`,
`delete_remote`, `equal`) the files and bytes done, the rows that failed and the rows not
done, the rows that could not be planned, all failures (the first 100 are listed with
their reasons), the duration of the run and whether it was cancelled. The desktop host
sends it as `sync://report` when a run ends, and `bb sync --summary` prints it.

## Limitations

- Changes are detected by size and modification time only; content is not hashed, except
  for files that differ only in time when `skip_duplicate_content` is on. A change that
  keeps size and time is not seen.
- Unicode normalization is folded only to detect collisions of names that would be
  written locally (see "Case and normalization collisions"). Local and remote names are
  still matched exactly: on macOS a local `café` written decomposed and an S3 key
  written composed are two rows (an upload of the local name and a failed row for the
  key), never one file.
- Case folding uses Unicode lowercasing per character, an approximation of what each
  file system does (for example `ß` and `SS` are not considered equal).
- Windows folders with the per-folder case-sensitive flag are still treated as
  case-insensitive (only extra failed rows can result).
- Empty folders and S3 folder markers are not synced unless `skip_empty_folders` is off
  (see "Empty folders"); markers are never deleted.
- File name and path length limits of the local file system are not checked during
  analysis; such rows fail at execution.
- The whole scan is held in memory, roughly 150 to 300 bytes per file.
- The listing cache is not used for a sync that deletes, and it can hide a change deep
  inside a long listing for up to 5 minutes (see "Parallel analysis and the listing
  cache").
- Hard links are synced as independent files.
- An `s3_to_s3` sync copies on the server within one account; it does not copy between
  accounts, and the copy keeps no per-object conditions (a destination object that appears
  between the check and the copy is replaced).
- Execution resumes row by row, not inside a file: a cancelled, failed or interrupted
  transfer of a large file starts over on the next run (engine resume states are not
  kept in the plan, and partial files and multipart uploads are discarded).
- After an upload-only run, local modification times are the files' own (except for a
  file dated later than its object, see above); a later download or bidirectional
  analysis plans to download files whose upload happened more than the tolerance after
  their modification time (same content, downloaded once).
- Clock skew between the computer and the server matters for one-way uploads only for
  files modified within the last 30 minutes; a skew of more than 15 minutes is refused
  by the server anyway.
- A conditional write is only as good as the server's implementation (see "Executing a
  plan"); the window that remains is the duration of the transfer on servers that
  ignore it.
