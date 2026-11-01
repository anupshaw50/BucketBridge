//! Event log (task T14): the error / warning / information entries shown in the log
//! panel, optionally mirrored to a rotating file. See docs/logging.md.
//!
//! - Entries live in an in-memory ring buffer of [`CAPACITY`] entries; ids are
//!   monotonic for the lifetime of the log (also across [`EventLog::clear`]).
//! - Entries below the minimum level ([`LoggingSettings::level`]) are not recorded at
//!   all. The level and the file settings can be changed at any time with
//!   [`EventLog::apply_settings`].
//! - With a log folder and `file_logging` on, entries are also appended to
//!   `log_dir/bucketbridge.log`. When the next line would make the file larger than
//!   `max_file_mib`, it is rotated: `.log.2` becomes `.log.3`, `.log.1` becomes `.log.2`,
//!   the current file becomes `.log.1`, and the oldest (`.log.3`) is deleted, so at most
//!   [`ROTATED_FILES`] old files are kept.
//! - Recording never waits for the disk. File writes happen on a background thread fed
//!   by a bounded queue; when the disk cannot keep up and the queue is full, entries
//!   are left out of the file (they stay in memory and reach subscribers) and the file
//!   gets a line saying how many were skipped. The only waiting a caller can see is a
//!   short mutex hold while an entry is added to the buffer.
//! - Problems writing the file are reported once as an error entry from source
//!   `eventlog` (in memory only) until a write succeeds again.
//! - [`EventLogLayer`] forwards `tracing` events: ERROR becomes error, WARN warning,
//!   INFO information; DEBUG and TRACE are ignored; the event's target is the source.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::error::{Error, Result};
use crate::model::{LogEntry, LogLevel};
use crate::settings::{LoggingSettings, MIB};

/// Number of entries kept in memory.
pub const CAPACITY: usize = 10_000;
/// Name of the log file inside the log folder.
pub const LOG_FILE_NAME: &str = "bucketbridge.log";
/// Number of rotated files kept next to the log file (`.1` is the newest).
pub const ROTATED_FILES: u32 = 3;
/// Longer messages are cut to this many bytes (plus a note).
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SOURCE_BYTES: usize = 256;
/// Source of the entries the log writes about itself.
const OWN_SOURCE: &str = "eventlog";
const FILE_QUEUE_CAPACITY: usize = 4096;
const LIVE_CAPACITY: usize = 1024;

fn io_error(context: impl Into<String>, err: io::Error) -> Error {
    Error::io(context, err)
}

/// Format of [`EventLog::export`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Comma-separated values with a header row (`"csv"`).
    Csv,
    /// A JSON array of `LogEntry` (`"json"`).
    Json,
}

/// The application's event log. Cheap to clone; all clones share the same log.
#[derive(Clone)]
pub struct EventLog {
    inner: Arc<Inner>,
}

struct Inner {
    core: Arc<Core>,
    sink: Option<FileSink>,
}

/// The in-memory part, shared with the file writer thread (weakly) for its own errors.
struct Core {
    ring: Mutex<Ring>,
    min_rank: AtomicU8,
    live: broadcast::Sender<LogEntry>,
}

struct Ring {
    entries: VecDeque<LogEntry>,
    next_id: u64,
}

impl fmt::Debug for EventLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventLog")
            .field("level", &self.level())
            .field("entries", &lock(&self.inner.core.ring).entries.len())
            .field("file_logging", &self.inner.sink.is_some())
            .finish()
    }
}

impl EventLog {
    /// Creates a log. With `log_dir`, a background thread writes the log file while
    /// `settings.file_logging` is on.
    pub fn new(settings: &LoggingSettings, log_dir: Option<PathBuf>) -> Self {
        let (live, _) = broadcast::channel(LIVE_CAPACITY);
        let core = Arc::new(Core {
            ring: Mutex::new(Ring {
                entries: VecDeque::new(),
                next_id: 1,
            }),
            min_rank: AtomicU8::new(rank(settings.level)),
            live,
        });
        let sink = log_dir.and_then(|dir| match FileSink::start(dir, settings, &core) {
            Ok(sink) => Some(sink),
            Err(err) => {
                core.push(
                    LogLevel::Error,
                    OWN_SOURCE,
                    &format!("cannot start the log file writer, file logging is off: {err}"),
                    None,
                );
                None
            }
        });
        Self {
            inner: Arc::new(Inner { core, sink }),
        }
    }

    /// Applies changed logging settings (level, file logging, file size) immediately.
    pub fn apply_settings(&self, settings: &LoggingSettings) {
        self.set_level(settings.level);
        if let Some(sink) = &self.inner.sink {
            sink.shared
                .max_bytes
                .store(max_file_bytes(settings.max_file_mib), Ordering::Relaxed);
            let was_on = sink
                .shared
                .enabled
                .swap(settings.file_logging, Ordering::SeqCst);
            if was_on && !settings.file_logging {
                // Best effort: if the queue is full the file is closed later.
                let _ = sink.queue.try_send(FileMsg::Close);
            }
        }
    }

    /// Sets the minimum level that is recorded.
    pub fn set_level(&self, level: LogLevel) {
        self.inner
            .core
            .min_rank
            .store(rank(level), Ordering::Relaxed);
    }

    /// The minimum level that is recorded.
    pub fn level(&self) -> LogLevel {
        level_of_rank(self.inner.core.min_rank.load(Ordering::Relaxed))
    }

    /// Whether an entry of `level` would be recorded.
    pub fn would_record(&self, level: LogLevel) -> bool {
        rank(level) >= self.inner.core.min_rank.load(Ordering::Relaxed)
    }

    /// Records an entry; returns it, or `None` when its level is below the minimum.
    /// Messages longer than [`MAX_MESSAGE_BYTES`] are shortened.
    pub fn record(&self, level: LogLevel, source: &str, message: &str) -> Option<LogEntry> {
        self.inner
            .core
            .push(level, source, message, self.inner.sink.as_ref())
    }

    /// Records an error entry.
    pub fn error(&self, source: &str, message: &str) -> Option<LogEntry> {
        self.record(LogLevel::Error, source, message)
    }

    /// Records a warning entry.
    pub fn warning(&self, source: &str, message: &str) -> Option<LogEntry> {
        self.record(LogLevel::Warning, source, message)
    }

    /// Records an information entry.
    pub fn information(&self, source: &str, message: &str) -> Option<LogEntry> {
        self.record(LogLevel::Information, source, message)
    }

    /// Entries matching all given criteria, oldest first (newest last).
    ///
    /// - `levels`: `None` accepts every level; otherwise the entry's level must be in
    ///   the list (so an empty list matches nothing).
    /// - `text`: `None` or empty accepts everything; otherwise the text must occur in
    ///   the message or the source, ignoring case.
    /// - `limit`: at most this many entries, the newest ones.
    pub fn query(
        &self,
        levels: Option<&[LogLevel]>,
        text: Option<&str>,
        limit: Option<usize>,
    ) -> Vec<LogEntry> {
        let needle = text.filter(|text| !text.is_empty()).map(str::to_lowercase);
        let ring = lock(&self.inner.core.ring);
        let mut found: Vec<LogEntry> = ring
            .entries
            .iter()
            .rev()
            .filter(|entry| levels.is_none_or(|levels| levels.contains(&entry.level)))
            .filter(|entry| {
                needle.as_deref().is_none_or(|needle| {
                    entry.message.to_lowercase().contains(needle)
                        || entry.source.to_lowercase().contains(needle)
                })
            })
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect();
        drop(ring);
        found.reverse();
        found
    }

    /// All entries in memory, oldest first.
    pub fn entries(&self) -> Vec<LogEntry> {
        lock(&self.inner.core.ring)
            .entries
            .iter()
            .cloned()
            .collect()
    }

    /// Removes all entries from memory (ids keep counting; the log file is untouched).
    pub fn clear(&self) {
        lock(&self.inner.core.ring).entries.clear();
    }

    /// Receives every entry recorded from now on. A receiver that falls more than 1024
    /// entries behind gets `RecvError::Lagged` and continues with newer entries.
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.inner.core.live.subscribe()
    }

    /// Writes all entries in memory to `path` (replaced atomically). CSV has the columns
    /// `id,ts,level,source,message`; JSON is an array of `LogEntry`.
    pub fn export(&self, path: &Path, format: ExportFormat) -> Result<()> {
        let entries = self.entries();
        let failed =
            |err: io::Error| io_error(format!("cannot export the log to {}", path.display()), err);
        let folder = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let mut temp = tempfile::Builder::new()
            .prefix(".bb-log-export-")
            .tempfile_in(folder)
            .map_err(failed)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            match format {
                ExportFormat::Csv => write_csv(&mut writer, &entries)?,
                ExportFormat::Json => write_json(&mut writer, &entries)?,
            }
            writer.flush().map_err(failed)?;
        }
        temp.as_file().sync_all().map_err(failed)?;
        temp.persist(path).map_err(|err| failed(err.error))?;
        Ok(())
    }

    /// Waits until every entry recorded so far has been handed to the OS by the file
    /// writer, at most `timeout`. Returns false on timeout or when the writer is gone;
    /// true at once without file logging.
    pub fn flush(&self, timeout: Duration) -> bool {
        let Some(sink) = &self.inner.sink else {
            return true;
        };
        let target = sink.shared.accepted.load(Ordering::SeqCst);
        let deadline = Instant::now().checked_add(timeout);
        let mut written = lock(&sink.shared.written);
        loop {
            if *written >= target {
                return true;
            }
            if !sink.shared.running.load(Ordering::SeqCst) {
                return false;
            }
            let wait = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return false;
                    }
                    left
                }
                None => Duration::from_secs(1),
            };
            written = sink
                .shared
                .written_changed
                .wait_timeout(written, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// A `tracing_subscriber` layer that records `tracing` events in this log.
    pub fn layer(&self) -> EventLogLayer {
        EventLogLayer { log: self.clone() }
    }

    #[cfg(test)]
    fn set_max_file_bytes(&self, bytes: u64) {
        if let Some(sink) = &self.inner.sink {
            sink.shared.max_bytes.store(bytes, Ordering::Relaxed);
        }
    }
}

impl Core {
    fn push(
        &self,
        level: LogLevel,
        source: &str,
        message: &str,
        sink: Option<&FileSink>,
    ) -> Option<LogEntry> {
        if rank(level) < self.min_rank.load(Ordering::Relaxed) {
            return None;
        }
        let mut ring = lock(&self.ring);
        let entry = LogEntry {
            id: ring.next_id,
            ts: Utc::now(),
            level,
            source: clip(source, MAX_SOURCE_BYTES),
            message: clip(message, MAX_MESSAGE_BYTES),
        };
        ring.next_id = ring.next_id.saturating_add(1);
        if ring.entries.len() >= CAPACITY {
            ring.entries.pop_front();
        }
        ring.entries.push_back(entry.clone());
        // Both hand-offs happen under the lock so subscribers and the file see entries
        // in id order; neither of them blocks.
        let _ = self.live.send(entry.clone());
        if let Some(sink) = sink {
            sink.offer(&entry);
        }
        Some(entry)
    }
}

/// Ordering of levels: information < warning < error.
fn rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Information => 0,
        LogLevel::Warning => 1,
        LogLevel::Error => 2,
    }
}

fn level_of_rank(rank: u8) -> LogLevel {
    match rank {
        0 => LogLevel::Information,
        1 => LogLevel::Warning,
        _ => LogLevel::Error,
    }
}

fn level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warning => "warning",
        LogLevel::Information => "information",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn max_file_bytes(max_file_mib: u32) -> u64 {
    u64::from(max_file_mib.max(1)) * MIB
}

/// Cuts `text` to at most `max` bytes on a character boundary, noting the cut.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}... [truncated, {} bytes in total]",
        &text[..end],
        text.len()
    )
}

enum FileMsg {
    Entry(LogEntry),
    Close,
}

struct FileSink {
    queue: SyncSender<FileMsg>,
    shared: Arc<SinkShared>,
}

struct SinkShared {
    enabled: AtomicBool,
    max_bytes: AtomicU64,
    /// Entries accepted into the queue.
    accepted: AtomicU64,
    /// Entries left out of the file because the queue was full.
    dropped: AtomicU64,
    /// Entries the writer has handled (written, or failed and reported).
    written: Mutex<u64>,
    written_changed: Condvar,
    running: AtomicBool,
}

impl FileSink {
    fn start(dir: PathBuf, settings: &LoggingSettings, core: &Arc<Core>) -> io::Result<Self> {
        let (queue, receiver) = mpsc::sync_channel(FILE_QUEUE_CAPACITY);
        let shared = Arc::new(SinkShared {
            enabled: AtomicBool::new(settings.file_logging),
            max_bytes: AtomicU64::new(max_file_bytes(settings.max_file_mib)),
            accepted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            written: Mutex::new(0),
            written_changed: Condvar::new(),
            running: AtomicBool::new(true),
        });
        let thread_shared = Arc::clone(&shared);
        let core = Arc::downgrade(core);
        thread::Builder::new()
            .name("bb-event-log".to_owned())
            .spawn(move || write_log_file(&receiver, dir, &thread_shared, &core))?;
        Ok(Self { queue, shared })
    }

    fn offer(&self, entry: &LogEntry) {
        if !self.shared.enabled.load(Ordering::SeqCst) {
            return;
        }
        match self.queue.try_send(FileMsg::Entry(entry.clone())) {
            Ok(()) => {
                self.shared.accepted.fetch_add(1, Ordering::SeqCst);
            }
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            // The writer thread is gone; nothing can be written any more.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Body of the writer thread: runs until every `EventLog` clone is dropped.
fn write_log_file(
    receiver: &Receiver<FileMsg>,
    dir: PathBuf,
    shared: &SinkShared,
    core: &Weak<Core>,
) {
    let mut file = LogFile {
        dir,
        writer: None,
        size: 0,
        failing: false,
    };
    let mut written = 0u64;
    while let Ok(first) = receiver.recv() {
        let mut next = Some(first);
        while let Some(message) = next {
            match message {
                FileMsg::Entry(entry) => {
                    let max_bytes = shared.max_bytes.load(Ordering::Relaxed);
                    let dropped = shared.dropped.swap(0, Ordering::Relaxed);
                    if dropped > 0 {
                        let note = format!(
                            "{} {:<11} [{OWN_SOURCE}] {dropped} entries were left out of this file because writing could not keep up\n",
                            Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                            level_name(LogLevel::Warning),
                        );
                        let result = file.write_line(&note, max_bytes);
                        file.report(result, core);
                    }
                    let result = file.write_line(&format_line(&entry), max_bytes);
                    file.report(result, core);
                    written += 1;
                }
                FileMsg::Close => {
                    let result = file.flush();
                    file.report(result, core);
                    file.writer = None;
                }
            }
            next = receiver.try_recv().ok();
        }
        let result = file.flush();
        file.report(result, core);
        *lock(&shared.written) = written;
        shared.written_changed.notify_all();
    }
    let result = file.flush();
    file.report(result, core);
    {
        let _written = lock(&shared.written);
        shared.running.store(false, Ordering::SeqCst);
    }
    shared.written_changed.notify_all();
}

struct LogFile {
    dir: PathBuf,
    writer: Option<BufWriter<File>>,
    size: u64,
    /// An error has been reported and no write has succeeded since.
    failing: bool,
}

impl LogFile {
    fn path(&self) -> PathBuf {
        self.dir.join(LOG_FILE_NAME)
    }

    fn write_line(&mut self, line: &str, max_bytes: u64) -> io::Result<()> {
        let len = line.len() as u64;
        if self.writer.is_none() {
            self.open()?;
        }
        if self.size > 0 && self.size.saturating_add(len) > max_bytes {
            self.rotate()?;
        }
        let writer = match self.writer.as_mut() {
            Some(writer) => writer,
            None => return Err(io::Error::other("the log file is not open")),
        };
        writer.write_all(line.as_bytes())?;
        self.size += len;
        Ok(())
    }

    fn open(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let file = open_log_file(&self.path())?;
        self.size = file.metadata()?.len();
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        let base = self.path();
        match fs::remove_file(rotated_path(&base, ROTATED_FILES)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        for generation in (1..ROTATED_FILES).rev() {
            rename_if_exists(
                &rotated_path(&base, generation),
                &rotated_path(&base, generation + 1),
            )?;
        }
        rename_if_exists(&base, &rotated_path(&base, 1))?;
        self.open()
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.writer.as_mut() {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }

    fn report(&mut self, result: io::Result<()>, core: &Weak<Core>) {
        match result {
            // Only an operation on an open file proves that writing works again.
            Ok(()) => {
                if self.writer.is_some() {
                    self.failing = false;
                }
            }
            Err(err) => {
                // Reopen on the next write.
                self.writer = None;
                if !self.failing {
                    self.failing = true;
                    if let Some(core) = core.upgrade() {
                        core.push(
                            LogLevel::Error,
                            OWN_SOURCE,
                            &format!("cannot write the log file {}: {err}", self.path().display()),
                            None,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
fn open_log_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_log_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotated_path(base: &Path, generation: u32) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

fn rename_if_exists(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// One line of the log file: `<RFC 3339 time> <level> [<source>] <message>`. Control
/// characters and backslashes are escaped so that every entry stays on one line.
fn format_line(entry: &LogEntry) -> String {
    format!(
        "{} {:<11} [{}] {}\n",
        entry.ts.to_rfc3339_opts(SecondsFormat::Millis, true),
        level_name(entry.level),
        escape_line(&entry.source),
        escape_line(&entry.message)
    )
}

fn escape_line(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c == '\\' || c.is_control()) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:04x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Writes `entries` as CSV with a header row. Fields are quoted as needed by the `csv`
/// crate; a source or message that starts with `=`, `+`, `-`, `@`, tab or carriage
/// return gets a leading `'` so spreadsheet programs do not run it as a formula.
pub fn write_csv<W: Write>(writer: W, entries: &[LogEntry]) -> Result<()> {
    let failed = |err: csv::Error| io_error("cannot write CSV", err.into());
    let mut csv = csv::Writer::from_writer(writer);
    csv.write_record(["id", "ts", "level", "source", "message"])
        .map_err(failed)?;
    for entry in entries {
        let id = entry.id.to_string();
        let ts = entry.ts.to_rfc3339_opts(SecondsFormat::AutoSi, true);
        csv.write_record([
            id.as_str(),
            ts.as_str(),
            level_name(entry.level),
            &defuse_formula(&entry.source),
            &defuse_formula(&entry.message),
        ])
        .map_err(failed)?;
    }
    csv.flush()
        .map_err(|err| io_error("cannot write CSV", err))?;
    Ok(())
}

/// Writes `entries` as a pretty-printed JSON array of `LogEntry`.
pub fn write_json<W: Write>(writer: W, entries: &[LogEntry]) -> Result<()> {
    serde_json::to_writer_pretty(writer, entries)
        .map_err(|err| io_error("cannot write JSON", err.into()))
}

fn defuse_formula(text: &str) -> Cow<'_, str> {
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        Cow::Owned(format!("'{text}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Forwards `tracing` events to an [`EventLog`]; create it with [`EventLog::layer`].
#[derive(Clone, Debug)]
pub struct EventLogLayer {
    log: EventLog,
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        let Some(level) = map_level(*metadata.level()) else {
            return;
        };
        if !self.log.would_record(level) {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.log.record(level, metadata.target(), &visitor.finish());
    }
}

fn map_level(level: Level) -> Option<LogLevel> {
    if level == Level::ERROR {
        Some(LogLevel::Error)
    } else if level == Level::WARN {
        Some(LogLevel::Warning)
    } else if level == Level::INFO {
        Some(LogLevel::Information)
    } else {
        None
    }
}

/// Collects the `message` field and appends the other fields as ` name=value`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.message.is_empty() {
            self.fields.trim_start().to_owned()
        } else {
            self.message + &self.fields
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::error::ErrorKind;

    const WAIT: Duration = Duration::from_secs(10);

    fn settings(level: LogLevel, file_logging: bool) -> LoggingSettings {
        LoggingSettings {
            level,
            file_logging,
            max_file_mib: 10,
        }
    }

    fn memory_log() -> EventLog {
        EventLog::new(&settings(LogLevel::Information, false), None)
    }

    #[test]
    fn ring_buffer_keeps_the_newest_entries_with_monotonic_ids() {
        let log = memory_log();
        for i in 0..CAPACITY + 5 {
            log.information("test", &format!("entry {i}"));
        }
        let entries = log.entries();
        assert_eq!(entries.len(), CAPACITY);
        assert_eq!(entries[0].id, 6);
        assert_eq!(entries[CAPACITY - 1].id, (CAPACITY + 5) as u64);
        assert!(entries.windows(2).all(|pair| pair[0].id + 1 == pair[1].id));
        log.clear();
        assert!(log.entries().is_empty());
        let next = log.warning("test", "after clear").unwrap();
        assert_eq!(next.id, (CAPACITY + 6) as u64);
    }

    #[test]
    fn minimum_level_is_applied_and_live_changeable() {
        let log = EventLog::new(&settings(LogLevel::Warning, false), None);
        assert_eq!(log.level(), LogLevel::Warning);
        assert!(log.information("test", "dropped").is_none());
        assert!(log.warning("test", "kept").is_some());
        assert!(log.error("test", "kept").is_some());
        log.set_level(LogLevel::Error);
        assert!(log.warning("test", "dropped").is_none());
        log.apply_settings(&settings(LogLevel::Information, false));
        assert!(log.would_record(LogLevel::Information));
        assert!(log.information("test", "kept").is_some());
        let levels: Vec<LogLevel> = log.entries().iter().map(|entry| entry.level).collect();
        assert_eq!(
            levels,
            vec![LogLevel::Warning, LogLevel::Error, LogLevel::Information]
        );
    }

    #[test]
    fn query_filters_by_level_text_and_limit_newest_last() {
        let log = memory_log();
        log.information("transfer", "Upload started");
        log.warning("transfer", "slow network");
        log.error("sync", "upload FAILED: denied");
        log.information("ui", "window opened");
        let messages = |entries: Vec<LogEntry>| -> Vec<String> {
            entries.into_iter().map(|entry| entry.message).collect()
        };
        assert_eq!(log.query(None, None, None).len(), 4);
        assert_eq!(
            messages(log.query(Some(&[LogLevel::Error, LogLevel::Warning]), None, None)),
            vec!["slow network", "upload FAILED: denied"]
        );
        assert!(log.query(Some(&[]), None, None).is_empty());
        assert_eq!(
            messages(log.query(None, Some("UPLOAD"), None)),
            vec!["Upload started", "upload FAILED: denied"]
        );
        assert_eq!(
            messages(log.query(None, Some("transfer"), None)),
            vec!["Upload started", "slow network"]
        );
        assert_eq!(log.query(None, Some(""), None).len(), 4);
        assert_eq!(
            messages(log.query(None, None, Some(2))),
            vec!["upload FAILED: denied", "window opened"]
        );
        assert_eq!(
            messages(log.query(Some(&[LogLevel::Information]), Some("o"), Some(1))),
            vec!["window opened"]
        );
        assert!(log.query(None, None, Some(0)).is_empty());
    }

    #[test]
    fn long_messages_are_clipped_on_a_character_boundary() {
        let log = memory_log();
        let long = "\u{e9}".repeat(MAX_MESSAGE_BYTES);
        let entry = log.information("test", &long).unwrap();
        assert!(entry.message.len() < MAX_MESSAGE_BYTES + 64);
        assert!(
            entry
                .message
                .ends_with(&format!("[truncated, {} bytes in total]", long.len()))
        );
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn subscribers_receive_new_entries_in_order() {
        let log = memory_log();
        log.information("test", "before");
        let mut receiver = log.subscribe();
        log.information("test", "one");
        log.set_level(LogLevel::Error);
        log.information("test", "filtered");
        log.error("test", "two");
        let first = receiver.try_recv().unwrap();
        let second = receiver.try_recv().unwrap();
        assert_eq!(
            (first.message.as_str(), second.message.as_str()),
            ("one", "two")
        );
        assert!(first.id < second.id);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn csv_export_escapes_and_defuses_formulas() {
        let dir = tempfile::tempdir().unwrap();
        let log = memory_log();
        log.information("src,with,commas", "line one\nline \"two\"");
        log.error("sync", "=HYPERLINK(\"http://example.invalid\")");
        log.warning("-cli", "+1, @2");
        let path = dir.path().join("log.csv");
        log.export(&path, ExportFormat::Csv).unwrap();
        let mut reader = csv::Reader::from_path(&path).unwrap();
        assert_eq!(
            reader.headers().unwrap(),
            vec!["id", "ts", "level", "source", "message"]
        );
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        assert_eq!(records.len(), 3);
        assert_eq!(&records[0][2], "information");
        assert_eq!(&records[0][3], "src,with,commas");
        assert_eq!(&records[0][4], "line one\nline \"two\"");
        assert_eq!(&records[1][4], "'=HYPERLINK(\"http://example.invalid\")");
        assert_eq!(&records[2][3], "'-cli");
        assert_eq!(&records[2][4], "'+1, @2");
        assert_eq!(&records[0][0], log.entries()[0].id.to_string());
        let ts = chrono::DateTime::parse_from_rfc3339(&records[0][1]).unwrap();
        assert_eq!(ts, log.entries()[0].ts);
    }

    #[test]
    fn json_export_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let log = memory_log();
        log.information("a", "first");
        log.error("b", "second \u{1F600}");
        let path = dir.path().join("log.json");
        log.export(&path, ExportFormat::Json).unwrap();
        let parsed: Vec<LogEntry> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed, log.entries());
        // Exporting again replaces the file.
        log.clear();
        log.export(&path, ExportFormat::Json).unwrap();
        let parsed: Vec<LogEntry> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(parsed.is_empty());
        let err = log
            .export(&dir.path().join("missing/log.json"), ExportFormat::Json)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
    }

    #[test]
    fn file_logging_writes_escaped_lines_and_follows_the_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let log = EventLog::new(
            &settings(LogLevel::Information, true),
            Some(log_dir.clone()),
        );
        log.information("transfer", "first line");
        log.error("sync", "multi\nline \\ text\u{7}");
        assert!(log.flush(WAIT));
        let text = fs::read_to_string(log_dir.join(LOG_FILE_NAME)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with("information [transfer] first line"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].ends_with("error       [sync] multi\\nline \\\\ text\\u{0007}"),
            "{}",
            lines[1]
        );
        chrono::DateTime::parse_from_rfc3339(lines[0].split(' ').next().unwrap()).unwrap();

        log.apply_settings(&settings(LogLevel::Information, false));
        log.information("test", "memory only");
        assert!(log.flush(WAIT));
        log.apply_settings(&settings(LogLevel::Information, true));
        log.information("test", "back in the file");
        assert!(log.flush(WAIT));
        let text = fs::read_to_string(log_dir.join(LOG_FILE_NAME)).unwrap();
        assert!(!text.contains("memory only"));
        assert!(text.contains("back in the file"));
        assert_eq!(log.entries().len(), 4);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(log_dir.join(LOG_FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn rotation_keeps_three_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(
            &settings(LogLevel::Information, true),
            Some(dir.path().to_path_buf()),
        );
        log.set_max_file_bytes(1000);
        let mut total = 0;
        for i in 0..200 {
            let entry = log
                .information("rotation", &format!("message number {i:04}"))
                .unwrap();
            total += format_line(&entry).len();
            // Keep the queue short so that no entry is left out of the file.
            if i % 20 == 0 {
                assert!(log.flush(WAIT));
            }
        }
        assert!(log.flush(WAIT));
        assert!(
            total > 4000,
            "the test must write enough to rotate several times"
        );
        let base = dir.path().join(LOG_FILE_NAME);
        let mut files = vec![base.clone()];
        files.extend((1..=ROTATED_FILES).map(|generation| rotated_path(&base, generation)));
        for file in &files {
            let len = fs::metadata(file).unwrap().len();
            assert!(len > 0 && len <= 1000, "{}: {len}", file.display());
        }
        assert!(!rotated_path(&base, ROTATED_FILES + 1).exists());
        // Oldest to newest, the kept files hold one contiguous run ending with the last entry.
        let mut numbers = Vec::new();
        for file in files.iter().rev() {
            for line in fs::read_to_string(file).unwrap().lines() {
                let number: u32 = line.rsplit(' ').next().unwrap().parse().unwrap();
                numbers.push(number);
            }
        }
        assert_eq!(*numbers.last().unwrap(), 199);
        assert!(
            numbers.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "{numbers:?}"
        );
        assert!(
            numbers[0] > 0,
            "the oldest entries must have been rotated out"
        );
    }

    #[test]
    fn a_line_longer_than_the_limit_still_gets_written() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(
            &settings(LogLevel::Information, true),
            Some(dir.path().to_path_buf()),
        );
        log.set_max_file_bytes(100);
        log.information("test", "short");
        log.information("test", &"x".repeat(500));
        log.information("test", "after");
        assert!(log.flush(WAIT));
        let base = dir.path().join(LOG_FILE_NAME);
        assert!(fs::read_to_string(&base).unwrap().contains("after"));
        assert!(
            fs::read_to_string(rotated_path(&base, 1))
                .unwrap()
                .contains(&"x".repeat(500))
        );
        assert!(
            fs::read_to_string(rotated_path(&base, 2))
                .unwrap()
                .contains("short")
        );
    }

    #[test]
    fn file_errors_are_reported_once_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_folder = dir.path().join("file");
        fs::write(&not_a_folder, "x").unwrap();
        let log = EventLog::new(
            &settings(LogLevel::Information, true),
            Some(not_a_folder.clone()),
        );
        log.information("test", "one");
        assert!(log.flush(WAIT));
        log.information("test", "two");
        assert!(log.flush(WAIT));
        let errors = log.query(Some(&[LogLevel::Error]), None, None);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].source, OWN_SOURCE);
        assert!(errors[0].message.contains("cannot write the log file"));
        assert_eq!(log.entries().len(), 3);
        // Once the obstacle is gone the file is written again.
        fs::remove_file(&not_a_folder).unwrap();
        log.information("test", "three");
        assert!(log.flush(WAIT));
        let text = fs::read_to_string(not_a_folder.join(LOG_FILE_NAME)).unwrap();
        assert!(text.contains("three") && !text.contains("two"), "{text}");
    }

    #[test]
    fn dropping_the_log_stops_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(
            &settings(LogLevel::Information, true),
            Some(dir.path().to_path_buf()),
        );
        let shared = Arc::clone(&log.inner.sink.as_ref().unwrap().shared);
        log.information("test", "last words");
        drop(log);
        let deadline = Instant::now() + WAIT;
        while shared.running.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!shared.running.load(Ordering::SeqCst));
        let text = fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();
        assert!(text.contains("last words"));
    }

    #[test]
    fn flush_without_a_file_returns_at_once() {
        assert!(memory_log().flush(Duration::ZERO));
        assert!(memory_log().flush(Duration::MAX));
    }

    #[test]
    fn tracing_events_are_forwarded() {
        let log = memory_log();
        let subscriber = tracing_subscriber::registry().with(log.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "bucketbridge::transfer", bucket = "photos", "upload of {} started", "a.jpg");
            tracing::warn!(target: "bucketbridge::sync", "slow");
            tracing::error!(target: "bucketbridge::s3", code = 403, "access denied");
            tracing::debug!(target: "bucketbridge::s3", "ignored");
            tracing::trace!(target: "bucketbridge::s3", "ignored");
            tracing::info!(target: "bucketbridge::ui", answer = 42);
        });
        let entries = log.entries();
        let summary: Vec<(LogLevel, &str, &str)> = entries
            .iter()
            .map(|entry| (entry.level, entry.source.as_str(), entry.message.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    LogLevel::Information,
                    "bucketbridge::transfer",
                    "upload of a.jpg started bucket=\"photos\""
                ),
                (LogLevel::Warning, "bucketbridge::sync", "slow"),
                (
                    LogLevel::Error,
                    "bucketbridge::s3",
                    "access denied code=403"
                ),
                (LogLevel::Information, "bucketbridge::ui", "answer=42"),
            ]
        );
        log.set_level(LogLevel::Error);
        let subscriber = tracing_subscriber::registry().with(log.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("below the minimum");
        });
        assert_eq!(log.entries().len(), 4);
    }

    #[test]
    fn export_format_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&ExportFormat::Csv).unwrap(),
            "\"csv\""
        );
        assert_eq!(
            serde_json::from_str::<ExportFormat>("\"json\"").unwrap(),
            ExportFormat::Json
        );
    }
}
