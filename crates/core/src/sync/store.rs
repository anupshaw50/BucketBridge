//! Persistent store of sync plans (`data_dir/sync`), built so that executing a plan can
//! be resumed row by row.
//!
//! Each plan is two files named after its id:
//!
//! - `<id>.json`: a snapshot of the whole [`SyncPlan`], always replaced atomically
//!   (written to a temporary file in the same folder, flushed to disk with fsync, then
//!   renamed over the old snapshot).
//! - `<id>.journal`: an append-only log of row updates, one JSON line per update
//!   (`{"i":12,"done":true}` or `{"i":12,"done":false,"error":"..."}`).
//!
//! Marking a row appends one line with a single `write` call, so the cost does not
//! depend on the size of the plan, and a batch costs one write for all its lines.
//! Reading a plan replays the journal over the snapshot; [`SyncStore::compact`] folds the
//! journal into a new snapshot and should be called when an execution run ends.
//!
//! Journal lines are not fsynced. A crash of the application loses nothing (the data is
//! in the OS cache); a power failure may lose the last updates or leave a torn last line.
//! Replay ignores a torn last line and skips damaged lines (and lines naming a row that
//! cannot be marked); before the first append, a torn last line is cut off so that it
//! cannot swallow the new line. A lost update only means the row is executed again,
//! which the executor must treat as harmless (copies are idempotent, and deleting what
//! is already gone counts as success). Each line sets absolute values, so replaying a
//! journal that is already contained in the snapshot (a crash between the two steps of
//! compaction) changes nothing.
//!
//! Only `copy` and `delete` rows can be updated, and the planned action is never
//! changed. [`SyncStore::mark_done`] sets `done: true` and clears `error`;
//! [`SyncStore::record_error`] records an execution error: it sets `error` and keeps
//! `done: false`, so the next run retries the row. [`SyncStore::update_rows`] applies a
//! batch of both kinds with one write.
//!
//! Retention: [`SyncStore::insert`] prunes the store (see [`MAX_PLANS`], [`MAX_PLAN_AGE`]
//! and [`ACTIVE_GRACE`]), because the desktop stores one plan for every analysis. A journal
//! line is read with a length limit ([`MAX_JOURNAL_LINE`]), so a damaged journal (a long
//! zero-filled tail after a power failure) cannot use unbounded memory.
//!
//! The store is thread-safe and serialises access per plan. Share one `SyncStore` per
//! folder (for example in an `Arc`) and let only one process use a folder at a time;
//! a second instance cannot corrupt the journal, but its compaction could drop updates
//! the other instance appended meanwhile (those rows would run again).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Dirs;
use crate::error::{Error, Result};
use crate::model::{SyncAction, SyncDirection, SyncPlan, SyncRow};
use crate::s3uri::S3Uri;

use super::RowCounts;

/// Name of the store folder inside the data directory.
pub const SYNC_DIR_NAME: &str = "sync";

const TEMP_PREFIX: &str = ".plan-";
const TEMP_SUFFIX: &str = ".tmp";
/// Temporary files older than this are leftovers of a crash and are removed on open.
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);
const UNKNOWN_ERROR: &str = "unknown error";
/// The store keeps at most this many plans: the ones touched most recently.
pub const MAX_PLANS: usize = 50;
/// A plan that was not touched for this long is removed, finished or not: its rows
/// describe a folder and a prefix as they were, and mostly fail as "changed since the
/// analysis".
pub const MAX_PLAN_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A plan touched more recently than this is never removed (it may be running).
pub const ACTIVE_GRACE: Duration = Duration::from_secs(60 * 60);
/// The longest journal line that is read; longer ones are skipped. Lines written by the
/// store are shorter ([`MAX_ERROR_BYTES`] bounds the error text).
pub const MAX_JOURNAL_LINE: usize = 64 * 1024;
/// Error texts longer than this are cut when they are recorded.
const MAX_ERROR_BYTES: usize = 8 * 1024;

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

fn not_found(message: impl Into<String>) -> Error {
    Error::NotFound(message.into())
}

fn conflict(message: impl Into<String>) -> Error {
    Error::Conflict(message.into())
}

fn io_error(context: impl Into<String>, err: io::Error) -> Error {
    Error::io(context, err)
}

fn internal(message: impl Into<String>) -> Error {
    Error::Internal(message.into())
}

/// Result of one execution attempt of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowOutcome {
    /// The action was applied: `done: true`, `error` cleared.
    Done,
    /// The attempt failed: `error` is set (an empty text becomes "unknown error"), the
    /// row keeps its action and `done: false`.
    Error(String),
}

/// One row update for [`SyncStore::update_rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowUpdate {
    /// Index of the row in `SyncPlan::rows`.
    pub index: usize,
    /// Result of the attempt.
    pub outcome: RowOutcome,
}

/// Header and progress of a stored plan, as returned by [`SyncStore::list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanSummary {
    /// Plan id (UUID v4).
    pub id: String,
    /// Account the plan runs with.
    pub account_id: String,
    /// Local folder.
    pub local_root: PathBuf,
    /// Remote folder, `s3://bucket/prefix/`.
    pub s3_prefix: S3Uri,
    /// Direction.
    pub direction: SyncDirection,
    /// Whether extra files on the target side are deleted.
    pub delete: bool,
    /// When the plan was created.
    pub created_at: DateTime<Utc>,
    /// Progress with the `SyncProgress` definitions.
    pub counts: RowCounts,
}

/// See the [module documentation](self).
#[derive(Debug)]
pub struct SyncStore {
    dir: PathBuf,
    slots: Mutex<HashMap<Uuid, Arc<Mutex<Slot>>>>,
}

#[derive(Debug, Default)]
struct Slot {
    deleted: bool,
    index: Option<RowIndex>,
}

/// What row updates need to know about a plan, kept in memory after first use.
#[derive(Debug)]
struct RowIndex {
    /// Per row: whether it is a copy or delete row.
    markable: Vec<bool>,
    /// Append handle, opened (after cutting off a torn tail) on first use.
    journal: Option<File>,
}

impl SyncStore {
    /// Opens (creating if needed) the store folder `dir`.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|err| {
            io_error(
                format!("cannot create the plan folder {}", dir.display()),
                err,
            )
        })?;
        remove_stale_temp_files(&dir);
        Ok(Self {
            dir,
            slots: Mutex::new(HashMap::new()),
        })
    }

    /// Opens the store in `data_dir/sync`.
    pub fn open_in(dirs: &Dirs) -> Result<Self> {
        Self::open(dirs.data_dir.join(SYNC_DIR_NAME))
    }

    /// The store folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stores a new plan. Its id must be a lowercase, hyphenated UUID v4 (as produced by
    /// [`super::new_plan_id`]); an existing plan with that id is a conflict.
    pub fn insert(&self, plan: &SyncPlan) -> Result<()> {
        let id = parse_plan_id(&plan.id)?;
        if plan.id != id.to_string() {
            return Err(usage("plan ids must be lowercase, hyphenated UUIDs"));
        }
        let slot = self.slot(id);
        let mut slot = lock_slot(&slot);
        let snapshot = self.snapshot_path(id);
        match fs::symlink_metadata(&snapshot) {
            Ok(_) => return Err(conflict(format!("a plan with id {id} already exists"))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(io_error(
                    format!("cannot check {}", snapshot.display()),
                    err,
                ));
            }
        }
        slot.index = None;
        remove_if_exists(&self.journal_path(id))?;
        write_snapshot(&self.dir, &snapshot, plan)?;
        slot.deleted = false;
        slot.index = Some(RowIndex {
            markable: plan.rows.iter().map(RowState::markable).collect(),
            journal: None,
        });
        drop(slot);
        self.prune(id);
        Ok(())
    }

    /// Removes the plans beyond [`MAX_PLANS`] (least recently touched first) and the ones
    /// not touched for [`MAX_PLAN_AGE`], except `keep` and any plan touched within
    /// [`ACTIVE_GRACE`]. Decided from the files' times alone, so it never reads a plan.
    /// Best effort: a plan that cannot be removed stays.
    fn prune(&self, keep: Uuid) {
        let Ok(listing) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut plans: Vec<(SystemTime, Uuid)> = Vec::new();
        for entry in listing.flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let Ok(id) = Uuid::try_parse(stem) else {
                continue;
            };
            if stem == id.to_string() {
                plans.push((self.last_touched(id), id));
            }
        }
        plans.sort_by_key(|plan| std::cmp::Reverse(plan.0));
        let now = SystemTime::now();
        for (rank, (touched, id)) in plans.into_iter().enumerate() {
            let idle = now.duration_since(touched).unwrap_or_default();
            if id != keep && idle >= ACTIVE_GRACE && (rank >= MAX_PLANS || idle > MAX_PLAN_AGE) {
                let _ = self.delete(&id.to_string());
            }
        }
    }

    /// When a plan's snapshot or journal was last written (now when that is unknown, which
    /// protects the plan).
    fn last_touched(&self, id: Uuid) -> SystemTime {
        [self.snapshot_path(id), self.journal_path(id)]
            .iter()
            .filter_map(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
            .max()
            .unwrap_or_else(SystemTime::now)
    }

    /// Loads a plan with every recorded row update applied.
    pub fn get(&self, id: &str) -> Result<SyncPlan> {
        let id = parse_plan_id(id)?;
        let slot = self.slot(id);
        let mut slot = lock_slot(&slot);
        if slot.deleted {
            return Err(plan_not_found(id));
        }
        let (plan, _) = self.load::<SyncPlan>(id)?;
        slot.index = Some(RowIndex {
            markable: plan.rows.iter().map(RowState::markable).collect(),
            journal: None,
        });
        Ok(plan)
    }

    /// Summaries of all stored plans, newest first. Plan files that cannot be read are
    /// skipped here; [`SyncStore::get`] reports what is wrong with them.
    pub fn list(&self) -> Result<Vec<PlanSummary>> {
        let listing = fs::read_dir(&self.dir).map_err(|err| {
            io_error(
                format!("cannot list the plan folder {}", self.dir.display()),
                err,
            )
        })?;
        let mut summaries = Vec::new();
        for item in listing {
            let entry = item.map_err(|err| {
                io_error(
                    format!("cannot list the plan folder {}", self.dir.display()),
                    err,
                )
            })?;
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let Ok(id) = Uuid::try_parse(stem) else {
                continue;
            };
            if stem != id.to_string() {
                continue;
            }
            let slot = self.slot(id);
            let slot = lock_slot(&slot);
            if slot.deleted {
                continue;
            }
            if let Ok((head, _)) = self.load::<PlanHead>(id) {
                summaries.push(head.into_summary());
            }
        }
        summaries.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(summaries)
    }

    /// Removes a plan and its journal.
    pub fn delete(&self, id: &str) -> Result<()> {
        let id = parse_plan_id(id)?;
        let slot_handle = self.slot(id);
        let mut slot = lock_slot(&slot_handle);
        if slot.deleted {
            return Err(plan_not_found(id));
        }
        // Close the journal before removing it (required on Windows).
        slot.index = None;
        let existed = remove_if_exists(&self.snapshot_path(id))?;
        remove_if_exists(&self.journal_path(id))?;
        if !existed {
            return Err(plan_not_found(id));
        }
        slot.deleted = true;
        drop(slot);
        lock(&self.slots).remove(&id);
        Ok(())
    }

    /// Marks row `index` done.
    pub fn mark_done(&self, id: &str, index: usize) -> Result<()> {
        self.update_rows(
            id,
            &[RowUpdate {
                index,
                outcome: RowOutcome::Done,
            }],
        )
    }

    /// Records that executing row `index` failed with `error`; the row keeps its action
    /// and `done: false`, so the next run retries it.
    pub fn record_error(&self, id: &str, index: usize, error: &str) -> Result<()> {
        self.update_rows(
            id,
            &[RowUpdate {
                index,
                outcome: RowOutcome::Error(error.to_owned()),
            }],
        )
    }

    /// Progress counts of a plan (see [`RowCounts`]); reads only the fields it needs
    /// instead of building the whole plan in memory.
    pub fn counts(&self, id: &str) -> Result<RowCounts> {
        Ok(self.summary(id)?.counts)
    }

    /// Header and progress of one plan (for example its `account_id`, to build the
    /// transfer engine before executing it), without building the rows in memory.
    pub fn summary(&self, id: &str) -> Result<PlanSummary> {
        let id = parse_plan_id(id)?;
        let slot = self.slot(id);
        let slot = lock_slot(&slot);
        if slot.deleted {
            return Err(plan_not_found(id));
        }
        let (head, _) = self.load::<PlanHead>(id)?;
        Ok(head.into_summary())
    }

    /// Applies a batch of row updates with one journal write. All updates are validated
    /// first, so an invalid one (unknown row, or a row that is not copy or delete)
    /// rejects the whole batch with a usage error and records nothing.
    pub fn update_rows(&self, id: &str, updates: &[RowUpdate]) -> Result<()> {
        let id = parse_plan_id(id)?;
        let slot = self.slot(id);
        let mut slot = lock_slot(&slot);
        if slot.deleted {
            return Err(plan_not_found(id));
        }
        let index = self.ensure_index(id, &mut slot)?;
        let mut lines = Vec::with_capacity(updates.len() * 24);
        for update in updates {
            match index.markable.get(update.index) {
                None => {
                    return Err(usage(format!(
                        "row {} does not exist; the plan has {} rows",
                        update.index,
                        index.markable.len()
                    )));
                }
                Some(false) => {
                    return Err(usage(format!(
                        "row {} is not a copy or delete row and cannot be updated",
                        update.index
                    )));
                }
                Some(true) => {}
            }
            let line = match &update.outcome {
                RowOutcome::Done => JournalLine {
                    i: update.index,
                    done: true,
                    error: None,
                },
                RowOutcome::Error(error) => JournalLine {
                    i: update.index,
                    done: false,
                    error: Some(if error.trim().is_empty() {
                        UNKNOWN_ERROR.to_owned()
                    } else {
                        truncated(error, MAX_ERROR_BYTES)
                    }),
                },
            };
            serde_json::to_writer(&mut lines, &line)
                .map_err(|err| internal(format!("cannot encode a row update: {err}")))?;
            lines.push(b'\n');
        }
        if lines.is_empty() {
            return Ok(());
        }
        let journal = self.journal_path(id);
        if let Err(err) = index.append(&journal, &lines) {
            // The file may now end in a torn line: reload (and repair) on next use.
            slot.index = None;
            return Err(io_error(
                format!("cannot record row updates in {}", journal.display()),
                err,
            ));
        }
        Ok(())
    }

    /// Folds the journal into a new snapshot and removes the journal.
    pub fn compact(&self, id: &str) -> Result<()> {
        let id = parse_plan_id(id)?;
        let slot = self.slot(id);
        let mut slot = lock_slot(&slot);
        if slot.deleted {
            return Err(plan_not_found(id));
        }
        let (plan, applied) = self.load::<SyncPlan>(id)?;
        slot.index = None;
        if applied > 0 {
            write_snapshot(&self.dir, &self.snapshot_path(id), &plan)?;
        }
        remove_if_exists(&self.journal_path(id))?;
        slot.index = Some(RowIndex {
            markable: plan.rows.iter().map(RowState::markable).collect(),
            journal: None,
        });
        Ok(())
    }

    fn slot(&self, id: Uuid) -> Arc<Mutex<Slot>> {
        Arc::clone(lock(&self.slots).entry(id).or_default())
    }

    fn snapshot_path(&self, id: Uuid) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn journal_path(&self, id: Uuid) -> PathBuf {
        self.dir.join(format!("{id}.journal"))
    }

    fn ensure_index<'s>(&self, id: Uuid, slot: &'s mut Slot) -> Result<&'s mut RowIndex> {
        let index = match slot.index.take() {
            Some(index) => index,
            None => {
                let (head, _) = self.load::<PlanHead>(id)?;
                RowIndex {
                    markable: head.rows.iter().map(RowState::markable).collect(),
                    journal: None,
                }
            }
        };
        Ok(slot.index.insert(index))
    }

    /// Reads the snapshot and replays the journal; returns the plan and the number of
    /// journal lines applied.
    fn load<P: StoredPlan>(&self, id: Uuid) -> Result<(P, u64)> {
        let snapshot = self.snapshot_path(id);
        let bytes = fs::read(&snapshot).map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                plan_not_found(id)
            } else {
                io_error(format!("cannot read {}", snapshot.display()), err)
            }
        })?;
        let mut plan: P = serde_json::from_slice(&bytes).map_err(|err| {
            io_error(
                format!("the plan file {} is damaged", snapshot.display()),
                io::Error::new(io::ErrorKind::InvalidData, err),
            )
        })?;
        drop(bytes);
        let applied = replay_journal(&self.journal_path(id), plan.rows_mut())?;
        Ok((plan, applied))
    }
}

impl RowIndex {
    fn append(&mut self, path: &Path, lines: &[u8]) -> io::Result<()> {
        let file = match self.journal.take() {
            Some(file) => file,
            None => {
                cut_torn_tail(path)?;
                open_append(path)?
            }
        };
        self.journal.insert(file).write_all(lines)
    }
}

/// Cuts the journal at `path` back to its last complete line. Only a line torn by a
/// crash (or a zero-filled tail after a power failure) can end without a newline; left
/// in place, it would merge with the next appended line.
fn cut_torn_tail(path: &Path) -> io::Result<()> {
    const CHUNK: u64 = 4096;
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let len = file.metadata()?.len();
    let mut buffer = [0u8; CHUNK as usize];
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let chunk = &mut buffer[..(end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;
        if let Some(newline) = chunk.iter().rposition(|&byte| byte == b'\n') {
            let keep = start + newline as u64 + 1;
            if keep < len {
                file.set_len(keep)?;
            }
            return Ok(());
        }
        end = start;
    }
    // Not a single complete line.
    if len > 0 {
        file.set_len(0)?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_append(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// `text`, cut to at most `max` bytes at a character boundary.
fn truncated(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// Plan ids arrive from the UI: only a UUID v4 may become part of a file name.
pub(crate) fn parse_plan_id(id: &str) -> Result<Uuid> {
    match Uuid::try_parse(id) {
        Ok(uuid) if uuid.get_version_num() == 4 => Ok(uuid),
        _ => Err(usage("invalid plan id (expected a UUID v4)")),
    }
}

fn plan_not_found(id: Uuid) -> Error {
    not_found(format!("sync plan {id} does not exist"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Locks a plan slot; after a panic in another thread the cached index may be stale, so
/// it is dropped and rebuilt from disk.
fn lock_slot(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            guard.index = None;
            slot.clear_poison();
            guard
        }
    }
}

/// Returns whether the file existed.
fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io_error(format!("cannot remove {}", path.display()), err)),
    }
}

fn write_snapshot(dir: &Path, target: &Path, plan: &SyncPlan) -> Result<()> {
    let failed = |err: io::Error| {
        io_error(
            format!("cannot save the plan file {}", target.display()),
            err,
        )
    };
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .suffix(TEMP_SUFFIX)
        .tempfile_in(dir)
        .map_err(failed)?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        serde_json::to_writer(&mut writer, plan).map_err(|err| failed(err.into()))?;
        writer.flush().map_err(failed)?;
    }
    temp.as_file().sync_all().map_err(failed)?;
    temp.persist(target).map_err(|err| failed(err.error))?;
    sync_folder(dir);
    Ok(())
}

/// Makes the rename durable where the platform allows syncing a folder. Best effort: the
/// snapshot itself is already on disk, only the new name could be lost on power failure.
#[cfg(unix)]
fn sync_folder(dir: &Path) {
    if let Ok(folder) = File::open(dir) {
        let _ = folder.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_folder(_dir: &Path) {}

fn remove_stale_temp_files(dir: &Path) {
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(TEMP_PREFIX) || !name.ends_with(TEMP_SUFFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_TEMP_AGE);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalLine {
    i: usize,
    done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Replays the journal at `path` onto `rows`; returns the number of lines applied.
/// Damaged lines are skipped (losing an update only repeats work) and a torn last line
/// is ignored.
fn replay_journal<R: RowState>(path: &Path, rows: &mut [R]) -> Result<u64> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(io_error(format!("cannot read {}", path.display()), err)),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut applied = 0u64;
    loop {
        match read_bounded_line(&mut reader, &mut line)
            .map_err(|err| io_error(format!("cannot read {}", path.display()), err))?
        {
            // End of file, or a torn last line.
            LineRead::End => break,
            LineRead::TooLong => continue,
            LineRead::Line => {}
        }
        let Ok(update) = serde_json::from_slice::<JournalLine>(&line) else {
            continue;
        };
        if update.done == update.error.is_some() {
            continue;
        }
        let Some(row) = rows.get_mut(update.i).filter(|row| row.markable()) else {
            continue;
        };
        row.apply(update.done, update.error);
        applied += 1;
    }
    Ok(applied)
}

/// How [`read_bounded_line`] ended.
#[derive(Debug, PartialEq, Eq)]
enum LineRead {
    /// A complete line (without its newline) is in the buffer.
    Line,
    /// A complete line longer than [`MAX_JOURNAL_LINE`]; it was skipped.
    TooLong,
    /// The end of the file, or a last line without a newline (torn by a crash).
    End,
}

/// Reads one line into `line` using at most [`MAX_JOURNAL_LINE`] bytes of memory, however
/// long the line is (a power failure can leave gigabytes of zeros without a newline).
fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<LineRead> {
    line.clear();
    let mut overflow = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(LineRead::End);
        }
        let newline = chunk.iter().position(|&byte| byte == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if !overflow {
            if line.len() + take > MAX_JOURNAL_LINE {
                overflow = true;
                line.clear();
            } else {
                line.extend_from_slice(&chunk[..take]);
            }
        }
        reader.consume(newline.map_or(take, |at| at + 1));
        if newline.is_some() {
            return Ok(if overflow {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
    }
}

/// A row type the journal can be replayed onto.
trait RowState {
    fn markable(&self) -> bool;
    fn apply(&mut self, done: bool, error: Option<String>);
}

fn is_markable(action: SyncAction) -> bool {
    matches!(action, SyncAction::Copy | SyncAction::Delete)
}

impl RowState for SyncRow {
    fn markable(&self) -> bool {
        is_markable(self.action)
    }

    fn apply(&mut self, done: bool, error: Option<String>) {
        self.done = done;
        self.error = error;
    }
}

/// A plan type that can be loaded from a snapshot.
trait StoredPlan: DeserializeOwned {
    type Row: RowState;
    fn rows_mut(&mut self) -> &mut [Self::Row];
}

impl StoredPlan for SyncPlan {
    type Row = SyncRow;

    fn rows_mut(&mut self) -> &mut [SyncRow] {
        &mut self.rows
    }
}

/// The parts of a snapshot that [`SyncStore::list`] and row updates need.
#[derive(Deserialize)]
struct PlanHead {
    id: String,
    account_id: String,
    local_root: PathBuf,
    s3_prefix: S3Uri,
    direction: SyncDirection,
    delete: bool,
    created_at: DateTime<Utc>,
    rows: Vec<RowHead>,
}

#[derive(Deserialize)]
struct RowHead {
    action: SyncAction,
    done: bool,
    error: Option<IgnoredAny>,
}

impl RowState for RowHead {
    fn markable(&self) -> bool {
        is_markable(self.action)
    }

    fn apply(&mut self, done: bool, error: Option<String>) {
        self.done = done;
        self.error = error.map(|_| IgnoredAny);
    }
}

impl StoredPlan for PlanHead {
    type Row = RowHead;

    fn rows_mut(&mut self) -> &mut [RowHead] {
        &mut self.rows
    }
}

impl PlanHead {
    fn counts(&self) -> RowCounts {
        let mut counts = RowCounts::default();
        for row in &self.rows {
            counts.add(row.action, row.done, row.error.is_some());
        }
        counts
    }

    fn into_summary(self) -> PlanSummary {
        let counts = self.counts();
        PlanSummary {
            id: self.id,
            account_id: self.account_id,
            local_root: self.local_root,
            s3_prefix: self.s3_prefix,
            direction: self.direction,
            delete: self.delete,
            created_at: self.created_at,
            counts,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Instant, SystemTime};

    use chrono::TimeZone;

    use super::*;
    use crate::error::ErrorKind;
    use crate::model::{FileMeta, Side};

    fn row(rel_path: &str, action: SyncAction) -> SyncRow {
        SyncRow {
            rel_path: rel_path.to_owned(),
            action,
            target: match action {
                SyncAction::Copy | SyncAction::Delete => Some(Side::Remote),
                SyncAction::Equal | SyncAction::Failed => None,
            },
            local_meta: Some(FileMeta {
                size: 3,
                modified: None,
            }),
            s3_meta: None,
            error: (action == SyncAction::Failed).then(|| "cannot read".to_owned()),
            done: action == SyncAction::Equal,
        }
    }

    fn plan(rows: Vec<SyncRow>) -> SyncPlan {
        SyncPlan {
            id: Uuid::new_v4().to_string(),
            account_id: "account-1".to_owned(),
            local_root: PathBuf::from("/sync/root"),
            s3_prefix: S3Uri::parse("s3://bucket/prefix/").unwrap(),
            source_prefix: None,
            direction: SyncDirection::Upload,
            delete: true,
            created_at: Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap(),
            rows,
        }
    }

    fn sample() -> SyncPlan {
        plan(vec![
            row("a", SyncAction::Copy),
            row("b", SyncAction::Delete),
            row("c", SyncAction::Equal),
            row("d", SyncAction::Failed),
            row("e", SyncAction::Copy),
        ])
    }

    fn journal_lines(store: &SyncStore, id: &str) -> usize {
        fs::read_to_string(store.dir().join(format!("{id}.journal")))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn round_trip_and_row_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path().join("sync")).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        assert_eq!(store.get(&original.id).unwrap(), original);

        store.mark_done(&original.id, 0).unwrap();
        store
            .record_error(&original.id, 1, "access denied")
            .unwrap();
        store.record_error(&original.id, 4, "  ").unwrap();
        let updated = store.get(&original.id).unwrap();
        assert!(updated.rows[0].done && updated.rows[0].error.is_none());
        assert_eq!(updated.rows[1].action, SyncAction::Delete);
        assert!(!updated.rows[1].done);
        assert_eq!(updated.rows[1].error.as_deref(), Some("access denied"));
        assert_eq!(updated.rows[4].error.as_deref(), Some(UNKNOWN_ERROR));
        assert_eq!(updated.rows[2], original.rows[2]);
        assert_eq!(updated.rows[3], original.rows[3]);

        // Retrying a failed row clears its error.
        store.mark_done(&original.id, 1).unwrap();
        let retried = store.get(&original.id).unwrap();
        assert!(retried.rows[1].done && retried.rows[1].error.is_none());

        // A second store over the same folder sees the same state.
        let reopened = SyncStore::open(dir.path().join("sync")).unwrap();
        assert_eq!(reopened.get(&original.id).unwrap(), retried);
    }

    #[test]
    fn batched_updates_are_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        let batch = [
            RowUpdate {
                index: 0,
                outcome: RowOutcome::Done,
            },
            RowUpdate {
                index: 4,
                outcome: RowOutcome::Error("timeout".to_owned()),
            },
        ];
        store.update_rows(&original.id, &batch).unwrap();
        assert_eq!(journal_lines(&store, &original.id), 2);
        let bad_batch = [
            RowUpdate {
                index: 1,
                outcome: RowOutcome::Done,
            },
            RowUpdate {
                index: 2,
                outcome: RowOutcome::Done,
            },
        ];
        let err = store.update_rows(&original.id, &bad_batch).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let err = store.mark_done(&original.id, 99).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let err = store.record_error(&original.id, 3, "x").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert_eq!(journal_lines(&store, &original.id), 2);
        let loaded = store.get(&original.id).unwrap();
        assert!(loaded.rows[0].done);
        assert!(!loaded.rows[1].done);
        assert_eq!(loaded.rows[4].error.as_deref(), Some("timeout"));
        store.update_rows(&original.id, &[]).unwrap();
    }

    #[test]
    fn ids_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        for bad in [
            "",
            "../../etc/passwd",
            "not-a-uuid",
            "00000000-0000-1000-8000-000000000000",
        ] {
            assert_eq!(
                store.get(bad).unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad}"
            );
            assert_eq!(
                store.delete(bad).unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad}"
            );
        }
        let missing = Uuid::new_v4().to_string();
        assert_eq!(store.get(&missing).unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(
            store.mark_done(&missing, 0).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            store.delete(&missing).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let mut upper = sample();
        upper.id = upper.id.to_uppercase();
        assert_eq!(store.insert(&upper).unwrap_err().kind(), ErrorKind::Usage);
        let original = sample();
        store.insert(&original).unwrap();
        assert_eq!(
            store.insert(&original).unwrap_err().kind(),
            ErrorKind::Conflict
        );
        // Lookups accept other spellings of the same UUID.
        assert_eq!(store.get(&original.id.to_uppercase()).unwrap(), original);
    }

    #[test]
    fn list_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let older = sample();
        let mut newer = sample();
        newer.created_at += chrono::TimeDelta::hours(1);
        store.insert(&older).unwrap();
        store.insert(&newer).unwrap();
        store.record_error(&newer.id, 0, "boom").unwrap();
        store.mark_done(&newer.id, 1).unwrap();
        fs::write(dir.path().join("notes.json"), "{}").unwrap();
        let damaged = Uuid::new_v4().to_string();
        fs::write(dir.path().join(format!("{damaged}.json")), "damaged").unwrap();
        let err = store.get(&damaged).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        assert!(err.to_string().contains("damaged"), "{err}");

        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, newer.id);
        assert_eq!(summaries[1].id, older.id);
        // Rows: copy (error "boom"), delete (done), equal, failed, copy.
        let summary = &summaries[0];
        assert_eq!(
            summary.counts,
            RowCounts {
                total: 3,
                done: 1,
                failed: 2
            }
        );
        assert_eq!(summary.counts.pending(), 2);
        assert_eq!(store.counts(&newer.id).unwrap(), summary.counts);
        assert_eq!(
            super::super::row_counts(&store.get(&newer.id).unwrap().rows),
            summary.counts
        );
        assert_eq!(summary.s3_prefix.to_string(), "s3://bucket/prefix/");
        assert_eq!(summary.local_root, PathBuf::from("/sync/root"));
        let untouched = &summaries[1];
        assert_eq!(
            untouched.counts,
            RowCounts {
                total: 3,
                done: 0,
                failed: 1
            }
        );

        store.delete(&newer.id).unwrap();
        assert_eq!(
            store.get(&newer.id).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert!(!dir.path().join(format!("{}.journal", newer.id)).exists());
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            store.delete(&newer.id).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        // The id can be reused after deletion.
        store.insert(&newer).unwrap();
        assert!(!store.get(&newer.id).unwrap().rows[0].done);
    }

    #[test]
    fn compaction_folds_the_journal_into_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        store.mark_done(&original.id, 0).unwrap();
        store.record_error(&original.id, 1, "nope").unwrap();
        let before = store.get(&original.id).unwrap();
        store.compact(&original.id).unwrap();
        assert!(!dir.path().join(format!("{}.journal", original.id)).exists());
        assert_eq!(store.get(&original.id).unwrap(), before);
        let reopened = SyncStore::open(dir.path()).unwrap();
        assert_eq!(reopened.get(&original.id).unwrap(), before);
        // Marking after compaction starts a new journal.
        store.mark_done(&original.id, 1).unwrap();
        assert_eq!(journal_lines(&store, &original.id), 1);
        assert!(store.get(&original.id).unwrap().rows[1].done);
        // Compacting without a journal is a no-op.
        store.compact(&original.id).unwrap();
        store.compact(&original.id).unwrap();
    }

    #[test]
    fn stale_journal_replayed_after_compaction_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        store.record_error(&original.id, 0, "first").unwrap();
        store.mark_done(&original.id, 0).unwrap();
        store.record_error(&original.id, 4, "second").unwrap();
        let journal = dir.path().join(format!("{}.journal", original.id));
        let saved = fs::read(&journal).unwrap();
        let expected = store.get(&original.id).unwrap();
        store.compact(&original.id).unwrap();
        // Simulate a crash between writing the snapshot and removing the journal.
        fs::write(&journal, &saved).unwrap();
        let reopened = SyncStore::open(dir.path()).unwrap();
        assert_eq!(reopened.get(&original.id).unwrap(), expected);
    }

    #[test]
    fn torn_and_invalid_journal_tails_are_ignored_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        store.mark_done(&original.id, 0).unwrap();
        let journal = dir.path().join(format!("{}.journal", original.id));
        // A crash in the middle of a write leaves a partial line.
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(b"{\"i\":4,\"do").unwrap();
        drop(file);

        let reopened = SyncStore::open(dir.path()).unwrap();
        let loaded = reopened.get(&original.id).unwrap();
        assert!(loaded.rows[0].done);
        assert!(!loaded.rows[4].done);
        // The next append first removes the torn tail.
        reopened.mark_done(&original.id, 4).unwrap();
        let text = fs::read_to_string(&journal).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(reopened.get(&original.id).unwrap().rows[4].done);

        // Complete but invalid lines are skipped; the lines after them still count.
        for garbage in [
            "{\"i\":2,\"done\":true}\n",
            "{\"i\":77,\"done\":true}\n",
            "{\"i\":0,\"done\":true,\"error\":\"x\"}\n",
            "{\"i\":0,\"done\":false}\n",
            "{\"i\":0,\"done\":true,\"extra\":1}\n",
            "not json\n",
            "\0\0\0\0\n",
        ] {
            let fresh = sample();
            store.insert(&fresh).unwrap();
            let path = dir.path().join(format!("{}.journal", fresh.id));
            fs::write(
                &path,
                format!("{garbage}{{\"i\":4,\"done\":false,\"error\":\"e\"}}\n"),
            )
            .unwrap();
            let other = SyncStore::open(dir.path()).unwrap();
            let loaded = other.get(&fresh.id).unwrap();
            assert_eq!(loaded.rows[0], fresh.rows[0], "{garbage:?}");
            assert_eq!(loaded.rows[2], fresh.rows[2], "{garbage:?}");
            assert_eq!(loaded.rows[4].error.as_deref(), Some("e"), "{garbage:?}");
        }
    }

    #[test]
    fn a_zero_filled_tail_is_cut_before_appending() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        let journal = dir.path().join(format!("{}.journal", original.id));
        // After a power failure the file can be longer than what was written.
        let mut contents = b"{\"i\":0,\"done\":true}\n".to_vec();
        contents.extend(std::iter::repeat_n(0u8, 10_000));
        fs::write(&journal, &contents).unwrap();
        let reopened = SyncStore::open(dir.path()).unwrap();
        assert!(reopened.get(&original.id).unwrap().rows[0].done);
        reopened.mark_done(&original.id, 4).unwrap();
        let text = fs::read_to_string(&journal).unwrap();
        assert_eq!(text, "{\"i\":0,\"done\":true}\n{\"i\":4,\"done\":true}\n");

        // A journal without a single complete line is emptied.
        fs::write(&journal, b"{\"i\":1,").unwrap();
        let again = SyncStore::open(dir.path()).unwrap();
        again.mark_done(&original.id, 1).unwrap();
        assert_eq!(
            fs::read_to_string(&journal).unwrap(),
            "{\"i\":1,\"done\":true}\n"
        );
    }

    #[test]
    fn two_stores_on_one_folder_do_not_cut_each_others_lines() {
        let dir = tempfile::tempdir().unwrap();
        let first = SyncStore::open(dir.path()).unwrap();
        let second = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        first.insert(&original).unwrap();
        // Both stores know the plan before either has opened the journal.
        second.get(&original.id).unwrap();
        second.mark_done(&original.id, 0).unwrap();
        first.mark_done(&original.id, 1).unwrap();
        second.mark_done(&original.id, 4).unwrap();
        let loaded = first.get(&original.id).unwrap();
        assert!(loaded.rows[0].done && loaded.rows[1].done && loaded.rows[4].done);
    }

    #[test]
    fn concurrent_markers_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SyncStore::open(dir.path()).unwrap());
        let rows = (0..400)
            .map(|i| row(&format!("f{i:04}"), SyncAction::Copy))
            .collect();
        let original = plan(rows);
        store.insert(&original).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|worker| {
                let store = Arc::clone(&store);
                let id = original.id.clone();
                std::thread::spawn(move || {
                    for index in (worker..400).step_by(4) {
                        store.mark_done(&id, index).unwrap();
                    }
                })
            })
            .collect();
        for handle in threads {
            handle.join().unwrap();
        }
        let loaded = store.get(&original.id).unwrap();
        assert!(loaded.rows.iter().all(|row| row.done));
        assert_eq!(journal_lines(&store, &original.id), 400);
    }

    #[test]
    fn large_plans_are_marked_without_rewriting_the_snapshot() {
        const ROWS: usize = 100_000;
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let rows = (0..ROWS)
            .map(|i| row(&format!("folder/file-{i:06}.dat"), SyncAction::Copy))
            .collect();
        let original = plan(rows);
        store.insert(&original).unwrap();
        let snapshot = dir.path().join(format!("{}.json", original.id));
        let snapshot_meta = fs::metadata(&snapshot).unwrap();
        let started = Instant::now();
        for index in 0..ROWS / 2 {
            store.mark_done(&original.id, index).unwrap();
        }
        let batch: Vec<RowUpdate> = (ROWS / 2..ROWS)
            .map(|index| RowUpdate {
                index,
                outcome: RowOutcome::Done,
            })
            .collect();
        for chunk in batch.chunks(1000) {
            store.update_rows(&original.id, chunk).unwrap();
        }
        let elapsed = started.elapsed();
        let after = fs::metadata(&snapshot).unwrap();
        assert_eq!(after.len(), snapshot_meta.len());
        assert_eq!(
            after.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            snapshot_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
        );
        assert_eq!(journal_lines(&store, &original.id), ROWS);
        let loaded = store.get(&original.id).unwrap();
        assert!(loaded.rows.iter().all(|row| row.done));
        store.compact(&original.id).unwrap();
        let counts = store.list().unwrap()[0].counts;
        assert_eq!(
            (counts.total, counts.done, counts.pending()),
            (ROWS, ROWS, 0)
        );
        eprintln!("marked {ROWS} rows in {elapsed:?}");
        // Generous (a debug build takes a few seconds): this only catches a rewrite of the
        // snapshot per row, which would take minutes.
        assert!(elapsed < Duration::from_secs(60), "{elapsed:?}");
    }

    #[test]
    fn a_journal_line_is_read_with_bounded_memory() {
        // Eight megabytes of zeros without a newline, as a power failure can leave.
        let mut zeros = BufReader::with_capacity(8192, io::repeat(0).take(8 << 20));
        let mut line = Vec::new();
        assert_eq!(
            read_bounded_line(&mut zeros, &mut line).unwrap(),
            LineRead::End
        );
        assert!(
            line.capacity() <= MAX_JOURNAL_LINE + 8192,
            "{}",
            line.capacity()
        );

        // A complete line that is too long is skipped, and the next one is read.
        let mut text = vec![b'x'; MAX_JOURNAL_LINE + 1];
        text.extend_from_slice(b"\n{\"i\":1}\nlast");
        let mut reader = BufReader::with_capacity(4096, text.as_slice());
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::TooLong
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::Line
        );
        assert_eq!(line, b"{\"i\":1}");
        // A last line without a newline is torn.
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineRead::End
        );
    }

    #[test]
    fn replay_survives_a_huge_zero_tail_and_keeps_the_lines_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        let journal = dir.path().join(format!("{}.journal", original.id));
        let mut file = File::create(&journal).unwrap();
        file.write_all(b"{\"i\":0,\"done\":true}\n").unwrap();
        file.set_len(48 << 20).unwrap();
        drop(file);
        let reopened = SyncStore::open(dir.path()).unwrap();
        assert!(reopened.get(&original.id).unwrap().rows[0].done);
    }

    #[test]
    fn long_error_texts_are_cut_before_they_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let original = sample();
        store.insert(&original).unwrap();
        let long = "\u{e9}".repeat(MAX_ERROR_BYTES);
        store.record_error(&original.id, 0, &long).unwrap();
        let recorded = store.get(&original.id).unwrap().rows[0]
            .error
            .clone()
            .unwrap();
        assert!(recorded.len() <= MAX_ERROR_BYTES + 3, "{}", recorded.len());
        assert!(recorded.ends_with("..."));
        assert_eq!(truncated("short", 10), "short");
    }

    fn backdate(store: &SyncStore, id: &str, age: Duration) {
        let id = Uuid::try_parse(id).unwrap();
        File::options()
            .write(true)
            .open(store.snapshot_path(id))
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn old_and_surplus_plans_are_pruned_when_a_plan_is_inserted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncStore::open(dir.path()).unwrap();
        let day = Duration::from_secs(24 * 60 * 60);
        let stale = sample();
        let recent = sample();
        let running = sample();
        for plan in [&stale, &recent, &running] {
            store.insert(plan).unwrap();
        }
        backdate(&store, &stale.id, MAX_PLAN_AGE + day);
        backdate(&store, &recent.id, 2 * day);
        // Untouched for a long time, but it has journal activity: it may be running.
        backdate(&store, &running.id, MAX_PLAN_AGE + day);
        store.mark_done(&running.id, 0).unwrap();

        let newest = sample();
        store.insert(&newest).unwrap();
        let ids: Vec<String> = store.list().unwrap().into_iter().map(|p| p.id).collect();
        assert!(!ids.contains(&stale.id), "a stale plan is removed");
        assert!(!dir.path().join(format!("{}.json", stale.id)).exists());
        for kept in [&recent.id, &running.id, &newest.id] {
            assert!(ids.contains(kept), "{kept} must stay");
        }

        // More plans than the cap: the least recently touched go, the rest stay.
        let many: Vec<SyncPlan> = (0..MAX_PLANS + 10).map(|_| sample()).collect();
        for (n, plan) in many.iter().enumerate() {
            store.insert(plan).unwrap();
            let age = Duration::from_secs(2 * 60 * 60) + Duration::from_secs(60 * n as u64);
            backdate(&store, &plan.id, age);
        }
        let last = sample();
        store.insert(&last).unwrap();
        let after = store.list().unwrap();
        assert!(after.len() <= MAX_PLANS, "{}", after.len());
        let ids: Vec<&str> = after.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&last.id.as_str()));
        assert!(
            ids.contains(&many[0].id.as_str()),
            "newest of the old ones stays"
        );
        assert!(
            !ids.contains(&many[MAX_PLANS + 9].id.as_str()),
            "oldest goes"
        );
    }

    #[test]
    fn stale_temp_files_are_cleaned_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join(".plan-old.tmp");
        let fresh = dir.path().join(".plan-new.tmp");
        fs::write(&stale, "x").unwrap();
        fs::write(&fresh, "x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        SyncStore::open(dir.path()).unwrap();
        assert!(!stale.exists());
        assert!(fresh.exists());
    }
}
