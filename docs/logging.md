# Event log and crash reports

This document describes the event log shown in the log panel and the local crash
reporter (task T14). The code lives in `crates/core/src/eventlog.rs` and
`crates/core/src/crash.rs`.

## Privacy

BucketBridge has no telemetry. Neither the event log nor the crash reporter sends
anything anywhere: entries stay in memory and, if file logging is on, in the local log
file; crash reports stay in the local crash folder until the user deletes them. Log
entries and crash reports can contain local paths, bucket names and object keys, so the
log file and crash reports are created readable by the current user only (mode 0600 on
Unix). If a user wants to share a report, they copy it themselves.

## Event log

Each entry is a `LogEntry` (`id`, `ts`, `level`, `source`, `message`). Levels, from
lowest to highest: `information`, `warning`, `error`.

- **Memory**: the newest 10,000 entries are kept in a ring buffer. Ids start at 1 and
  only grow; `clear_log` empties the buffer, but ids keep counting and the log file is
  not touched.
- **Minimum level**: `settings.logging.level`. Entries below it are not recorded at
  all (not in memory, not in the file, not sent to the UI). It can be changed at any
  time and applies to the next entry (`EventLog::apply_settings`).
- **Messages** longer than 16 KiB are cut and end with `... [truncated, N bytes in
  total]`.
- **Live entries**: `EventLog::subscribe` returns a receiver of every new entry (the
  desktop host forwards them as `log://entry`). A receiver more than 1,024 entries
  behind is told how many it missed and continues with newer ones.

### Querying (`get_log`)

`query(levels, text, limit)` returns matching entries oldest first (newest last):

- `levels`: `null` means all levels; otherwise the level must be in the list (an empty
  list matches nothing).
- `text`: `null` or empty means no text filter; otherwise the text must occur in the
  message or the source, ignoring case.
- `limit`: return at most this many, the newest ones.

### Export (`export_log`)

`csv` writes a header `id,ts,level,source,message` and one record per entry, quoted as
needed (commas, quotes and line breaks survive a round trip). Because exported logs are
often opened in spreadsheet programs, a source or message starting with `=`, `+`, `-`,
`@`, a tab or a carriage return gets a leading `'` so it is not run as a formula. `json`
writes an array of `LogEntry` objects. The export replaces the target file atomically
(temporary file, fsync, rename) and covers all entries in memory.

## Log file

With file logging on (`settings.logging.file_logging`, default on) every recorded entry
is also appended to `log_dir/bucketbridge.log`, one line per entry:

```
2026-10-29T08:15:00.123Z information [bucketbridge::transfer] upload of a.jpg started
2026-10-29T08:15:03.456Z error       [sync] cannot read folder: permission denied
```

Backslashes, line breaks, tabs and other control characters in the source and message
are escaped (`\\`, `\n`, `\t`, `\u{0007}`), so each entry stays on one line.

**Rotation**: before a line would make the file larger than
`settings.logging.max_file_mib` (default 10, at least 1), the file is rotated:
`bucketbridge.log.2` becomes `.3`, `.1` becomes `.2`, the current file becomes `.1`, and
the previous `.3` is deleted. So at most four files exist, the current one and three
old ones, each at most the size limit (a single entry longer than the limit still gets
written, alone in its file).

**Never blocking on the disk**: callers only add the entry to memory and hand it to a
background writer thread through a bounded queue (4,096 entries), without waiting. If
the disk is so slow that the queue fills up, entries are left out of the file (they are
still in memory and reach subscribers) and the next line written says how many were
skipped. The only wait a caller can see is the short lock while the entry is added to
the ring buffer. `EventLog::flush(timeout)` waits, at most `timeout`, until everything
recorded so far has been handed to the OS; call it before the app exits.

**Write errors** (disk full, missing permission, the log folder replaced by a file) are
reported once as an error entry with source `eventlog`, in memory only, and again only
after writing has worked in between. The writer reopens the file on the next entry.

Turning file logging off closes the file; turning it on again appends to it.

## Forwarding `tracing` events

`EventLog::layer()` returns a `tracing_subscriber` layer. Add it to the subscriber that
the desktop host and the CLI install, next to any other layers:

```rust
use tracing_subscriber::layer::SubscriberExt;
let subscriber = tracing_subscriber::registry().with(event_log.layer());
tracing::subscriber::set_global_default(subscriber)?;
```

| `tracing` level | event log level |
|-----------------|-----------------|
| ERROR           | error           |
| WARN            | warning         |
| INFO            | information     |
| DEBUG, TRACE    | ignored         |

The event's target becomes the entry's source. The `message` field becomes the message
and other fields are appended as ` name=value` (strings in quotes), for example
`upload of a.jpg started bucket="photos"`.

## Crash reports

`crash::install(crash_dir, version)` is called once at startup. It puts a panic hook in
front of the existing one; the existing hook (normally the default one that prints the
panic) still runs afterwards. For each panic the hook writes
`crash_dir/crash-<UTC timestamp>-<process id>.txt`, for example
`crash-20261029T081500.123456Z-4242.txt` (a `-1`, `-2`, ... suffix is added if two
reports get the same name), and prints its path on standard error. A report contains:

- the time, the application version, the OS and CPU architecture, the process id,
- the thread name and id,
- the source location and the panic message,
- a backtrace (always captured, whatever `RUST_BACKTRACE` is set to).

At most 10 reports are written per process, so a panic loop cannot fill the disk. The
hook never panics itself (a panic inside a panic hook would abort the process).

**Showing reports** (`last_crash_report`): `crash::last_report(crash_dir)` returns the
newest report directly inside the crash folder (ignoring other files, folders and
symbolic links), or nothing. Reports larger than 1 MiB are cut when read.

**Dismissing** (`dismiss_crash_report`): `crash::dismiss(crash_dir, path)` moves the
report into `crash_dir/dismissed/` (keeping its name; a `-dupN` suffix avoids
overwriting an earlier dismissed report). The path comes from the UI and is treated as
untrusted: it must be absolute and resolve, after following symbolic links and `..`, to
a regular file directly inside the crash folder whose name has the report format.
Anything else (relative paths, files outside the folder or in its subfolders, links
pointing out of the folder, folders, other names) is a usage error and nothing is
moved. A well-formed report path that no longer exists (for example dismissed twice)
is `not_found`.
