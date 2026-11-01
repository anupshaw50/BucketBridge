# Transfers: queue and engine building blocks

This document describes `bucketbridge_core::transfer`: the transport-agnostic
transfer queue (task T11) and the primitives the S3 runners of task T05 are
built on. The JSON shapes and command semantics follow `docs/desktop-api.md`
(types `TransferTask`, `Settings.transfer`, section "Queue rules").

## Components

| Item | File | Purpose |
|---|---|---|
| `TransferQueue` | `queue.rs` | Owns all tasks: scheduling, state machine, retries, events, persistence, shutdown |
| `TaskRunner`, `Runners`, `TaskContext` | `runner.rs` | Seam to the transports: one runner per `TransferKind` |
| `Progress`, `StopSignal`, `ResumeState` | `runner.rs` | What a runner reports, observes and persists |
| `BandwidthLimiter`, `ThrottledStream`, `ThrottledRead` | `limiter.rs` | One token bucket shared by all transfers |
| `plan_transfer`, `MultipartPlan`, `Part` | `parts.rs` | Single request or multipart, part size and byte ranges |
| `RetryPolicy`, `Backoff`, `is_retryable` | `retry.rs` | Error classification and capped exponential backoff with full jitter |
| `RateEstimator`, `eta_secs` | `rate.rs` | Smoothed speed and ETA |
| queue file | `store.rs` | On-disk format, loading, atomic saves |

## Task state machine

```text
  add ──> queued ──(slot free)──> running ──(runner Ok)───────────────> completed
            │  ^                   │  │ │
  pause,    │  │ resume,           │  │ └─(remote/io error, budget left:
  stop_all  │  │ start_all         │  │    back off, run again)
            v  │                   │  └─(other error or budget spent)──> failed
          stopped <──(pause, stop_all, shutdown)─┘                         │
                                                              retry_task   │
                                                              (queued) <───┘
  cancel from queued, stopped, failed or running ──(cleanup)──> cancelled
                                   (cleanup fails) ──> failed, kept
  clear_finished removes completed and cancelled tasks
```

`completed` and `cancelled` are terminal. Commands per state:

| Command | queued | running | stopped | failed | completed | cancelled |
|---|---|---|---|---|---|---|
| `pause_task` | stopped | pause requested, stopped when the run ends | conflict | conflict | conflict | conflict |
| `resume_task` | conflict | conflict | queued | conflict | conflict | conflict |
| `retry_task` | conflict | conflict | conflict | queued | conflict | conflict |
| `cancel_task` | cleanup, then cancelled | cancel requested, cleanup, then cancelled | cleanup, then cancelled | cleanup, then cancelled | conflict | conflict |
| `stop_all` | stopped | pause requested | skipped | skipped | skipped | skipped |
| `start_all` | skipped | skipped | queued | skipped | skipped | skipped |
| `cancel_all` | as `cancel_task` | as `cancel_task` | as `cancel_task` | as `cancel_task` | skipped | skipped |
| `clear_finished` | kept | kept | kept | kept | removed | removed |

This is exactly the "Queue rules" of `docs/desktop-api.md`: each command
accepts only the states listed there and rejects every other transition with
`conflict`. Cancelling a `failed` task also runs the cleanup, because a failed
upload may still own an open multipart upload. Bulk commands skip tasks they do
not apply to and never fail because of them. A cancellation whose cleanup
fails ends `failed`, not `cancelled` (see "Runs, stop requests and
cancellation").

Two transient conditions are not visible in `TransferState` but matter for
commands:

* **stopping**: `running` with a pause request. Another `pause_task` is
  accepted and changes nothing, `cancel_task` upgrades the request to a
  cancel, `resume_task` and `retry_task` fail with `conflict` (the task is
  still `running`).
* **being cancelled**: a cancel was requested (running task) or the cleanup of
  an idle task is in progress. The task keeps its visible state until the
  cleanup finished; another `cancel_task` is accepted and changes nothing (the
  cleanup is not repeated); every other command fails with `conflict`,
  including `pause_task`, since the task can no longer become `stopped`.

Error kinds: an unknown id is `not_found`; a command that does not apply to the
task's current state is `conflict` (the request is well formed but conflicts
with the state of the resource, which is how `conflict` is used elsewhere in
the contract). Every mutating command fails with `conflict` after `shutdown`.

## Scheduling

* At most `max_concurrent_tasks` tasks are `running`. A running task holds its
  slot until its run has completely ended, including backoff between automatic
  retries and the cleanup after a cancel, so the number of runner futures
  executing at once never exceeds the cap.
* Order: first in, first out per account (by creation order, so a resumed task
  keeps its place), round robin across accounts: each free slot goes to the
  oldest queued task of the next account after the one served last. With a
  single account this is plain FIFO; with several, a large batch for one
  account does not block the others.
* A paused, stopped, completed, failed or cancelled task frees its slot and the
  next queued task starts at once.
* `update_settings` applies immediately. A higher cap starts queued tasks; a
  lower cap never interrupts running tasks, new runs start only once fewer than
  the new cap are running. Bandwidth changes reach waiting transfers at once.
  The retry budget is read at the next failure; part settings apply to the next
  run (a run gets a settings snapshot).

## Runs, stop requests and cancellation

Each run gets a `TaskContext` and a `StopSignal`. Pause, `stop_all` and
shutdown send `StopReason::Pause`; cancel sends `StopReason::Cancel` (a cancel
replaces a pending pause, never the reverse). A stop that arrives before a run
has begun skips the runner entirely, so it causes no side effects (the start
still counts in `attempts`). The runner is expected to return promptly; if it
has not returned after `stop_grace` (default 5 s) its future is dropped.
Outcome of a run:

* `Ok(())` means the transfer finished: the task becomes `completed`, even if a
  stop was requested meanwhile (a finished transfer is not undone).
* Any result after a pause request: `stopped`, resume state kept.
* Any result after a cancel request: the runner's `cleanup` runs once, then the
  task becomes `cancelled` and its resume state is cleared. If the cleanup
  fails (an error, or the cleanup timeout), something may be left behind, such
  as an open multipart upload, and the resume state is the only record of it:
  the task becomes `failed` instead, with the error ("cleanup after cancelling
  failed: ... The task was kept so that cancelling it again repeats the
  cleanup"), keeps its resume state and its pending cancellation, and
  `cancel_task` is accepted again (it is a `failed` task). Cancelling again,
  and the next `open`, repeat the cleanup; `retry_task` starts the task again
  instead (and drops the pending cancellation).
* An error without a stop request: retried or `failed` (next section).
* A runner panic is caught and fails the task with an `internal` error.

Cancelling an idle task (`queued`, `stopped`, `failed`) runs its cleanup in the
background, at most 8 cleanups at a time; the task keeps its state until the
cleanup ends, then becomes `cancelled`. Every cancellation calls the cleanup
once, including for tasks that never ran (resume state `null`).

A cleanup gets `cleanup_timeout` (default 120 s); after that it counts as
failed ("cleanup after cancelling failed: the cleanup did not finish within
...", the task ends `failed` as described above) and its slot or cleanup
permit is free again, so a hanging cleanup cannot stall the queue.

A cancellation is recorded as pending until its cleanup has finished. If a
shutdown cuts it short (the grace period ran out), the task ends `stopped` in
memory but is saved with `cancel_pending: true`; the next `open` runs the
cleanup again and then marks the task `cancelled` (or `failed` again if it
fails again). The same flag is kept for a task whose cleanup failed. Until
then the task cannot be resumed. Cleanups must therefore be idempotent.

## Automatic retries

* Retryable kinds: `remote` and `io`. Never retried: `usage`, `auth`, `locked`,
  `mfa_required`, `not_found`, `conflict`, `cancelled`, `internal`.
* Budget: `retry_attempts` retries after the first run (default 3, so at most 4
  runs). The budget restarts whenever the user starts the task again
  (`resume_task`, `start_all`, `retry_task`).
* `attempts` counts runs, including automatic retries, across the task's life.
* Backoff: delay before retry `n` is uniform in `[0, min(30 s, 1 s * 2^(n-1))]`
  (full jitter). The task stays `running` and keeps its slot while waiting;
  `error` shows the failure that is being retried and is cleared when the next
  run starts. A pause or cancel during the wait takes effect immediately.
* Every retry passes the same `ResumeState`, so a retry continues where the
  failed run stopped instead of starting over.

Runners use `RetryPolicy::from_settings` for part-level retries inside a run.

## Events

`subscribe()` returns an unbounded channel of `QueueEvent`:

| Event | Desktop event | When |
|---|---|---|
| `Updated(TransferTask)` | `transfer://update` | task added; any state change; `error` set while backing off; progress |
| `Removed(Vec<String>)` | `transfer://removed` | `clear_finished` removed tasks |
| `PersistFailed(String)` | log entry (error) | a background save failed (once per distinct error) |

* State changes are sent before the command that caused them returns, in the
  order they happened.
* Progress (`bytes_done`, `bytes_total`, `speed`, `eta`) is sampled every
  `progress_interval` (250 ms) per running task and sent only when it changed,
  so a task produces at most one progress event per interval.
* Events are never dropped. Bulk commands produce one event per task (a
  10,000-task `start_all` sends 10,000 events); the host may batch them before
  forwarding to the UI. A dropped receiver is removed on the next event.

## Persistence

With `QueueOptions::persist_path` set, the queue lives in one JSON file
(suggested location: the app data dir, `transfers.json`):

```json
{
  "version": 1,
  "tasks": [
    {"task": { "id": "...", "state": "stopped", "...": "TransferTask fields" },
     "resume": {"upload_id": "...", "parts": [1, 2]}},
    {"task": { "id": "...", "state": "stopped" },
     "resume": null,
     "cancel_pending": true}
  ]
}
```

* Tasks are stored in creation order with the runner's resume state.
  `cancel_pending` appears only on tasks whose cancellation a shutdown cut
  short (see above); readers ignore fields they do not know.
* Saves are atomic: a temporary file in the same directory is written and
  flushed to disk, renamed over the queue file, and the directory is flushed
  (Unix). A crash leaves the old or the new file, never a mix. Saves are
  serialized, and a write keeps that order even if the caller of `flush` or
  `shutdown` stops waiting for it, so an older snapshot never replaces a newer
  one.
* Saves are delayed and coalesced: the first change schedules a save
  `persist_delay` (default 1 s) later; all changes until then share it. Changes
  that schedule a save: adding tasks, state changes, resume state updates,
  `clear_finished`. Progress ticks alone never do. A save rewrites the file, so
  its cost is linear in the number of tasks and paid at most once per delay.
  A runner can ask for a save at once with `ResumeState::persist_now`; the S3
  runners do it when a multipart upload was just created, so that its ID is on
  disk within milliseconds instead of after the delay.
* `flush()` saves pending changes at once; `shutdown()` ends with a save.
* Loading: a missing file is an empty queue. `queued` and `running` tasks come
  back as `stopped` (speed 0, no ETA, and without the error of a retry that was
  pending) with their resume state, and resume from it when started. A file that cannot be parsed, has duplicate ids or another
  format version makes `open` fail with an `io` error naming the file; the file
  is not modified (no queue is opened from it, so nothing can overwrite it).
  The host can offer to move it aside and start with an empty queue.
* Background save failures are reported as `PersistFailed` and retried with
  the next change, `flush` or `shutdown`.

## Bandwidth throttling

One `BandwidthLimiter` (a token bucket) is shared by every task of the queue,
so `bandwidth_limit_kibps` caps the total rate.

* Rate in bytes per second (`kibps * 1024`); `None` is unlimited. The bucket
  holds `max(rate / 8, min(rate, 4 KiB))` bytes: an idle limiter allows a burst
  of 1/8 s of traffic (up to one second's worth below 32 KiB/s, so that waits
  stay coarse enough at very low rates). Tokens are integers in units of
  10^-9 byte, so refills are exact at every rate.
* Waiters are served first in first out; a request arriving while others wait
  queues behind them even if tokens are available. A request larger than the
  bucket is granted over several refills.
* Cancellation safe: dropping an `acquire` future leaves the line, wakes the
  next waiter and returns tokens it had already collected (capped at the bucket
  size).
* `set_rate` wakes all waiters, which continue at the new rate; switching to
  unlimited releases them at once.
* Adapters: `ThrottledStream` wraps a `Stream<Item = Result<Bytes, E>>` and
  yields pieces of at most 64 KiB, each after it was granted (uploads, part
  bodies). `ThrottledRead` wraps an `AsyncRead`, reads up to 64 KiB into its own
  buffer and pays for exactly the bytes read before handing them out
  (downloads).

## Part sizing

`plan_transfer(size, threshold, part_size)` (or `plan_for_settings`):

* Multipart when `size >= threshold`, and always above 5 GiB (a single request
  has the same 5 GiB limit as a part). An empty object is a single request.
* Part size: the configured size, at least 5 MiB and at most 5 GiB. If that
  needs more than 10,000 parts, the size grows to `ceil(size / 10,000)` rounded
  up to a whole MiB.
* Above 10,000 x 5 GiB the result is a `usage` error; no provider-specific
  maximum object size is assumed.
* `MultipartPlan::parts()` yields `Part { number, range }` with numbers from 1
  and half-open byte ranges; `part(n)` gives random access for resuming;
  `Part::http_range()` formats a `Range` header for ranged downloads.

| Size | Settings | Plan |
|---|---|---|
| 1 MiB | defaults (8 MiB / 8 MiB) | single request |
| 8 MiB | defaults | 1 part of 8 MiB |
| 100 GiB | defaults | 9,310 parts of 11 MiB |
| 1 TiB | defaults | 9,987 parts of 105 MiB |
| 6 GiB | threshold 10 GiB | 768 parts of 8 MiB |
| 10,000 x 5 GiB + 1 byte | any | usage error |

## Speed and ETA

`speed` is an exponentially weighted moving average of bytes per second with a
2 s time constant, robust to irregular sampling. It reads exactly 0 after 2 s
without progress and outside of runs. `eta` is the remaining bytes divided by
the speed, rounded up to whole seconds; `null` when the total is unknown or
the speed is 0, `0` when done.

## Runner contract

A runner (`TaskRunner`) is registered per `TransferKind` in `Runners`. It must:

1. Select the S3 client by `ctx.task().account_id` and act on `src`/`dst`.
2. At the start of each run, read `ctx.resume()` and verify it (it can be
   stale: an aborted upload id, a temp file that vanished, an ETag that
   changed); start over when it does not hold. Then call
   `ctx.progress().set_total(..)` and `set_done(..)` to match what the resume
   state covers. Progress starts at the task's last saved values.
3. Report progress while bytes flow (per chunk, not per part), with
   `sub_done` to take back the bytes of a part that failed and will be resent.
   Delete tasks count objects.
4. Send every byte through `ctx.limiter()` (`throttle_stream`,
   `throttle_read`, or `acquire`).
5. Update the resume state at durable checkpoints (a part completed, a
   downloaded range written), not per chunk: each update schedules a save of
   the queue file. Record the plan (for example the part size) in it so a later
   run with changed settings continues with the same layout. Use
   `ResumeState::update` when parallel parts record completion.
6. Watch `ctx.stop()` (`select!` on `wait()`, or `check()?` between steps) and
   return promptly with an error: on a pause, leave the resume state usable; on
   a cancel, just return, the queue calls `cleanup` afterwards. A run that
   ignores the signal is dropped after the grace period.
7. Return `Ok(())` only when the transfer is complete. Return errors with
   accurate kinds: `remote` and `io` are retried by the queue, everything else
   fails the task.
8. Keep all work inside the run future (`FuturesUnordered`, or a `JoinSet`,
   which aborts its tasks when dropped). Never detach tasks: a run can be
   dropped at any await point.
9. Retry individual parts with `RetryPolicy::from_settings(ctx.settings())`
   and bound parallel parts by `ctx.settings().max_parts_per_task`.
10. Make `cleanup` idempotent: it receives the last persisted resume state,
    which may be `None` (the task never ran) or stale, runs after any run of
    the task ended, may run again after a restart if a shutdown interrupted
    it, and must remove partial results (abort the multipart upload, delete
    partial local files) within the cleanup timeout. Fail only when the
    cleanup could not be finished: the error keeps the task `failed` with its
    resume state so that the cleanup can be repeated. What no longer exists
    counts as cleaned up.
11. Not panic. A panic fails the task with an `internal` error.

`TaskContext::new`, `Progress::new`, `StopController::new` and
`ResumeState::new` let runner tests drive a runner without a queue.

## Adding tasks

* `add_tasks(tasks)` adds `queued` tasks (a `NewTask` per task).
* `add_tasks_with_failures(tasks, failed)` also adds tasks that failed
  before they could run, such as an entry of a selected folder that cannot
  be read: each `FailedTask { task, error, resume }` becomes a `failed` task
  with `error` and the given resume state, and `attempts` 0. It behaves like
  any failed task: `retry_task` runs its runner with that resume state, and
  `cancel_task` runs the cleanup with it. Both lists are validated first;
  either every task is added or none.

## S3 runners

`engine::S3Runner` implements the runner contract for S3 on the transfer
engines, and `engine::enqueue_upload`, `enqueue_download`, `enqueue_copy`,
`enqueue_move`, `enqueue_delete` and `enqueue_rename` implement the enqueue
commands (one `group_id` per call, nothing enqueued on a `conflict` between
selected entries, entries that cannot be added as failed tasks). The
wiring (provider of the S3 transport per account, throttle grants, progress,
stop requests, resume states and cleanups) is described in
`docs/engines.md`, section "Transfer queue runners".

## Errors returned by the queue

| Kind | When |
|---|---|
| `usage` | invalid settings (`max_concurrent_tasks` or `max_parts_per_task` of 0, `bandwidth_limit_kibps` of 0), a progress interval of zero or above one day, an invalid new task (empty account id, a delete with a destination, another kind without one, no runner for its kind), an object too large for multipart |
| `not_found` | unknown task id |
| `conflict` | a command that does not apply to the task's state; any mutating command after shutdown |
| `io` | the queue file cannot be read, is damaged or has another format version; a save failed |
| `internal` | `open` outside a Tokio runtime; a runner panicked (stored on the failed task) |

## Options

| Option | Default | Notes |
|---|---|---|
| `persist_path` | none | queue file; `None` keeps the queue in memory |
| `persist_delay` | 1 s | delay between a change and its save |
| `progress_interval` | 250 ms | must be more than zero and at most one day |
| `stop_grace` | 5 s | `Duration::MAX` means a run is never dropped |
| `cleanup_timeout` | 120 s | bound on each runner cleanup |
| `backoff` | 1 s base, 30 s cap | full jitter |
| `jitter_seed` | from entropy | fixed seeds make retry delays reproducible |

Very large durations are safe: deadlines that would overflow fall back to "far
in the future".

## Shutdown

`shutdown()` (idempotent): refuses further commands (`conflict`), turns queued
tasks into `stopped`, asks running tasks to pause, waits up to `stop_grace`
for runs and cleanups, then drops what is left (runs end `stopped` with their
resume state; a cancellation that did not finish stays pending and completes
on the next open) and saves the queue. Dropping a `TransferQueue` without
calling `shutdown` starts the same sequence in the background; a new queue on
the same file must only be opened after the previous one finished shutting
down.

## Limitations

* The queue file is rewritten as a whole on each save (at most once per
  `persist_delay`); there is no append-only journal.
* Resume granularity is the runner's checkpoints: bytes of an interrupted part
  are sent again.
* The queue file is not locked; the desktop app must run as a single instance.
* Event channels are unbounded per subscriber; a subscriber that stops reading
  without dropping its receiver accumulates events.
