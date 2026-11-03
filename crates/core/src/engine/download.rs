//! Download engine: one object to a local file (or any [`DownloadSink`]).
//!
//! The object is streamed into `<dest>.bbpart` next to the destination (the
//! sync scanner skips that suffix), through the throttle. When every byte
//! has arrived and passed validation, the file is flushed to disk, its
//! modification time is set to the object's `Last-Modified` (the sync
//! planner relies on it) and it is renamed over the destination.
//!
//! Names ending in `.bbpart` (in any letter case) belong to the partial
//! files of downloads: nothing is ever downloaded under such a name, so a
//! partial file can never be the final file of another object, and the
//! engine may reuse, truncate or delete any file it finds under such a name.
//!
//! A resumed download asks for the missing range with `If-Match` on the
//! saved ETag and also compares the ETag, length and `Content-Range` of the
//! answer (some servers ignore `If-Match`); if the object changed, the
//! download starts over from the first byte. A ranged answer carries no
//! checksum, so when validation is on, a resumed file is checked against
//! the object's full-object checksum (saved from the first answer) before
//! it is renamed; the check reads the file in chunks and ends early on a
//! stop request.

use std::ffi::OsStr;
use std::fs::{File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aws_sdk_s3::operation::get_object::GetObjectOutput;
use aws_sdk_s3::primitives::ByteStreamError;
use aws_sdk_s3::types::ChecksumMode;
use aws_smithy_types::error::display::DisplayErrorContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::checksum::{ALGORITHMS, Checksummer, ObjectChecksum, checksum_field, is_full_object};
use super::control::{Hooks, Outcome, StopReason};
use super::policy::{ItemInfo, MTIME_META_KEY, Placed, object_time, parse_mtime};
use super::{
    TransferEngine, blocking, bucket_client, require_object_key, to_utc, unquote,
    without_response_validation,
};
use crate::error::{Error, ErrorKind, IoContext, Result};
use crate::model::TransferKind;
use crate::s3uri::S3Uri;
use crate::settings::{ChecksumAlgorithm, IfExists};

/// Suffix of the partial file a download writes before the rename.
pub const PARTIAL_SUFFIX: &str = ".bbpart";

/// Received bytes collected before one write to the sink.
const WRITE_BATCH: usize = 1024 * 1024;

/// Least time between two checkpoints (each one flushes the partial file to
/// disk first).
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// Bytes read at a time when a partial file is checksummed.
const VERIFY_CHUNK: usize = 1024 * 1024;

/// How long a clean-up waits for a download that still holds the partial
/// file: a run the queue dropped after its grace period can leave a write or
/// a flush behind that is still finishing.
const CLEANUP_LOCK_WAIT: Duration = Duration::from_secs(30);

/// First and longest pause between two looks at a locked partial file.
const LOCK_POLL: (Duration, Duration) = (Duration::from_millis(10), Duration::from_millis(250));

/// How many times opening a partial file starts over when its name changes
/// under it (a planted link is replaced, a vanished file created).
const OPEN_ATTEMPTS: usize = 4;

/// Resume state of a download, emitted through the checkpoint callback
/// (stored by the queue as JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadState {
    /// Source bucket.
    pub bucket: String,
    /// Source key.
    pub key: String,
    /// ETag of the object version being downloaded, as the server sent it.
    pub etag: String,
    /// Object size in bytes.
    pub size: u64,
    /// The object's `Last-Modified`.
    pub last_modified: Option<DateTime<Utc>>,
    /// The `mtime` user metadata of the object (see
    /// [`MTIME_META_KEY`](super::policy::MTIME_META_KEY)), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<DateTime<Utc>>,
    /// The object's full-object checksum, when validation is on and the
    /// server reported one.
    pub checksum: Option<ObjectChecksum>,
    /// Bytes stored durably in the partial file (the resume offset).
    pub offset: u64,
}

/// Result of a finished download.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DownloadOutcome {
    /// Object size in bytes.
    pub size: u64,
    /// ETag of the downloaded version, without quotes.
    pub etag: Option<String>,
    /// The object's `Last-Modified`, also set as the file's modification
    /// time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Bytes that were already present from an earlier run.
    pub resumed_from: u64,
    /// True when saved progress was dropped because the object had changed
    /// (or the partial data was gone) and the download started over.
    pub restarted: bool,
    /// The time a kept modification time gives the file: the `mtime` user
    /// metadata an upload stored, else `last_modified`.
    #[serde(default)]
    pub file_time: Option<DateTime<Utc>>,
    /// The path the file was written to when the destination existed and the
    /// policy renamed the download (`name (1).ext`); `None` otherwise.
    #[serde(default)]
    pub renamed_to: Option<std::path::PathBuf>,
}

/// Where downloaded bytes go.
///
/// The methods block; the engine calls them on Tokio's blocking thread pool,
/// one at a time.
pub trait DownloadSink: Send {
    /// Prepares to receive the object from byte `offset` on, dropping
    /// anything stored at or after it, and returns the offset writing
    /// continues at: `offset` itself, or 0 when the bytes before `offset`
    /// are not available (the download then starts over).
    fn prepare(&mut self, offset: u64) -> io::Result<u64>;

    /// Appends data.
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;

    /// Makes everything written so far durable; called before every
    /// checkpoint.
    fn sync(&mut self) -> io::Result<()>;

    /// The `algorithm` checksum of everything written, for sinks that can
    /// read their data back; `None` otherwise. Used to validate resumed
    /// downloads.
    fn checksum(&mut self, algorithm: ChecksumAlgorithm) -> io::Result<Option<String>> {
        let _ = algorithm;
        Ok(None)
    }

    /// A description for messages.
    fn describe(&self) -> String;
}

/// A partial download file.
#[derive(Debug)]
pub struct FileSink {
    path: PathBuf,
    file: File,
    /// Ends a read-back of the file early once it is cancelled.
    stop: Option<CancellationToken>,
}

impl FileSink {
    /// Opens the partial file at `path`, creating it and its parent folders
    /// when needed, without discarding its content (blocking).
    ///
    /// `path` must end in [`PARTIAL_SUFFIX`] (see [`partial_path`]); any
    /// other name is a [`Error::Usage`], so that a sink can never open, and
    /// later truncate, an ordinary file.
    ///
    /// A link is never followed. A new file is created exclusively, and a
    /// symbolic link under the name (or a file that has other hard links) is
    /// replaced by a new file of its own: the name is predictable and
    /// scanners skip it, so a link planted there must not make the download
    /// overwrite what it points to, outside the destination folder. An
    /// existing ordinary file is continued or truncated, as the download
    /// needs.
    ///
    /// The file is locked exclusively while the sink exists, so a second
    /// download to the same destination (from this process or another)
    /// fails instead of mixing its bytes in. That failure has the kind
    /// [`ErrorKind::Io`], which the transfer queue retries: the other
    /// download is usually about to finish. On file systems without locking
    /// the download goes ahead unlocked.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.file_name().is_some_and(is_partial_name) {
            return Err(Error::Usage(format!(
                "'{}' is not a partial file: its name must end in '{PARTIAL_SUFFIX}', so that a download never writes to an ordinary file",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .io_context(|| format!("creating the folder '{}'", parent.display()))?;
        }
        let file = open_partial(path)?;
        if let Err(TryLockError::WouldBlock) = file.try_lock() {
            return Err(Error::io(
                format!("opening '{}'", path.display()),
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "it is being written by another download of the same file",
                ),
            ));
        }
        Ok(FileSink {
            path: path.to_path_buf(),
            file,
            stop: None,
        })
    }

    /// Makes [`DownloadSink::checksum`] end early, with an
    /// [`io::ErrorKind::Interrupted`] error, once `token` is cancelled, so
    /// that reading a large partial file back never holds up a pause or a
    /// cancel.
    #[must_use]
    pub fn with_stop(mut self, token: CancellationToken) -> Self {
        self.stop = Some(token);
        self
    }

    /// The partial file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Completes a download without moving it, for callers that install
    /// the file themselves (folder sync checks the target again first):
    /// sets the modification time, flushes the file to disk, checks that
    /// the name still refers to the file that was written (a link planted
    /// under it meanwhile is refused), then releases the lock and returns
    /// the path (blocking).
    pub fn seal(self, modified: Option<DateTime<Utc>>) -> Result<PathBuf> {
        let FileSink { path, file, .. } = self;
        if let Some(modified) = modified {
            file.set_modified(SystemTime::from(modified))
                .io_context(|| format!("setting the modification time of '{}'", path.display()))?;
        }
        file.sync_all()
            .io_context(|| format!("flushing '{}' to disk", path.display()))?;
        ensure_still_named(&path, &file)
            .io_context(|| format!("checking '{}' before handing it over", path.display()))?;
        drop(file);
        Ok(path)
    }

    /// Completes a download: sets the modification time, flushes the file
    /// to disk and renames it over `dest` (blocking).
    ///
    /// The rename happens while the file is still open and locked: another
    /// download of the same destination cannot open the partial file in
    /// between and have its half-written data renamed in its place. Just
    /// before it, the name must still be the ordinary file that was
    /// written: a link planted under it meanwhile is never renamed into
    /// place.
    fn finish(self, dest: &Path, modified: Option<DateTime<Utc>>) -> Result<()> {
        let FileSink { path, file, .. } = self;
        if let Some(modified) = modified {
            file.set_modified(SystemTime::from(modified))
                .io_context(|| format!("setting the modification time of '{}'", path.display()))?;
        }
        file.sync_all()
            .io_context(|| format!("flushing '{}' to disk", path.display()))?;
        ensure_still_named(&path, &file)
            .io_context(|| format!("checking '{}' before renaming it", path.display()))?;
        std::fs::rename(&path, dest)
            .io_context(|| format!("renaming '{}' to '{}'", path.display(), dest.display()))?;
        drop(file);
        if let Some(parent) = dest.parent() {
            sync_dir(parent);
        }
        Ok(())
    }
}

impl DownloadSink for FileSink {
    fn prepare(&mut self, offset: u64) -> io::Result<u64> {
        let len = self.file.metadata()?.len();
        let offset = if offset <= len { offset } else { 0 };
        self.file.set_len(offset)?;
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(offset)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    fn checksum(&mut self, algorithm: ChecksumAlgorithm) -> io::Result<Option<String>> {
        let Some(mut checksummer) = Checksummer::new(algorithm) else {
            return Ok(None);
        };
        let end = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(0))?;
        let read = read_back(&self.file, end, self.stop.as_ref(), &mut checksummer);
        // Back to where writing continues, also after a stop.
        self.file.seek(SeekFrom::Start(end))?;
        if read? != end {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the partial file is shorter than the data written to it",
            ));
        }
        Ok(Some(checksummer.finish()))
    }

    fn describe(&self) -> String {
        format!("'{}'", self.path.display())
    }
}

/// Opens (and creates, when it does not exist) the partial file at `path`
/// without ever following a link; see [`FileSink::open`].
fn open_partial(path: &Path) -> Result<File> {
    let context = || format!("opening '{}'", path.display());
    for _ in 0..OPEN_ATTEMPTS {
        // Exclusive creation never follows a link: a name that is taken, by
        // a link (even a dangling one) too, fails.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(Error::io(context(), err)),
        }
        let named = match std::fs::symlink_metadata(path) {
            Ok(named) => named,
            // Gone again: create it.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(Error::io(context(), err)),
        };
        if named.file_type().is_symlink() || has_other_names(&named) {
            // Replaced by a file of ours, so that nothing it points to, or
            // shares its data with, is ever written.
            remove_name(path).io_context(context)?;
            continue;
        }
        if !named.is_file() {
            return Err(Error::io(context(), io::Error::other("it is not a file")));
        }
        // An earlier partial file, to continue or to start over in. Neither
        // creating nor truncating here: if the name was swapped for a link
        // meanwhile, this opens the link's target (nothing is written to it
        // yet) and the check below notices.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .io_context(context)?;
        let now = std::fs::symlink_metadata(path)
            .ok()
            .filter(Metadata::is_file);
        if let Some(named) = now
            && file.metadata().is_ok_and(|held| same_file(&named, &held))
        {
            return Ok(file);
        }
    }
    Err(Error::io(
        context(),
        io::Error::other("the name kept changing while it was being opened"),
    ))
}

/// Checks that `path` still names the ordinary file that `file` holds,
/// before the partial file is renamed into place.
fn ensure_still_named(path: &Path, file: &File) -> io::Result<()> {
    let named = std::fs::symlink_metadata(path)?;
    if named.is_file() && file.metadata().is_ok_and(|held| same_file(&named, &held)) {
        return Ok(());
    }
    Err(io::Error::other(
        "the name was replaced by another file while the download ran, so the data was not used",
    ))
}

/// Whether `named` (the metadata of a path, links not followed) describes
/// the file that `held` (the metadata of an open file) belongs to. Without a
/// file identity to compare (not Unix), only being an ordinary file counts.
fn same_file(named: &Metadata, held: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        named.dev() == held.dev() && named.ino() == held.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = held;
        named.is_file()
    }
}

/// Whether the file has other names (hard links): writing to it would
/// change the file under those names too.
fn has_other_names(named: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        named.is_file() && named.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = named;
        false
    }
}

/// Removes a name (a link, or one name of a file) without touching what it
/// points to.
fn remove_name(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        // Windows removes a link to a folder like a folder.
        #[cfg(windows)]
        Err(err) => std::fs::remove_dir(path).map_err(|_| err),
        #[cfg(not(windows))]
        Err(err) => Err(err),
    }
}

/// Feeds the first `end` bytes of `file` to `checksummer`, one chunk at a
/// time, and returns how many it read. A cancelled `stop` ends the reading
/// with [`io::ErrorKind::Interrupted`] before the next chunk.
fn read_back(
    file: &File,
    end: u64,
    stop: Option<&CancellationToken>,
    checksummer: &mut Checksummer,
) -> io::Result<u64> {
    let mut reader = file.take(end);
    let mut buf = vec![0u8; VERIFY_CHUNK];
    let mut total = 0u64;
    loop {
        if stop.is_some_and(CancellationToken::is_cancelled) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped while reading the partial file back",
            ));
        }
        let read = reader.read(&mut buf)?;
        if read == 0 {
            return Ok(total);
        }
        checksummer.update(&buf[..read]);
        total += read as u64;
    }
}

/// True when `name` ends in [`PARTIAL_SUFFIX`], in any letter case (Windows
/// and macOS compare file names without regard to case).
///
/// Such names are reserved for the partial files of downloads, which the
/// engine creates, reuses and deletes at will: no object is downloaded under
/// a name like that (an object whose key ends in `.bbpart` is refused, in
/// the expansion and by [`TransferEngine::download_file`]) and the expansion
/// of an upload leaves such files out, the same rule that the sync planner
/// follows. So a partial file can never be the final file of another
/// download.
pub fn is_partial_name(name: impl AsRef<OsStr>) -> bool {
    let name = name.as_ref().as_encoded_bytes();
    let suffix = PARTIAL_SUFFIX.as_bytes();
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The partial file of a download to `dest`: `dest` with
/// [`PARTIAL_SUFFIX`] appended to its file name.
pub fn partial_path(dest: &Path) -> Result<PathBuf> {
    let name = dest.file_name().ok_or_else(|| {
        Error::Usage(format!(
            "'{}' does not name a file to download to",
            dest.display()
        ))
    })?;
    let mut partial = name.to_os_string();
    partial.push(PARTIAL_SUFFIX);
    Ok(dest.with_file_name(partial))
}

/// Flushes a directory entry change to disk where the OS supports it. Best
/// effort: the downloaded data itself is already on disk.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Holds the sink between blocking calls.
struct Slot<S> {
    sink: Option<S>,
    description: String,
}

impl<S: DownloadSink + 'static> Slot<S> {
    fn new(sink: S) -> Self {
        let description = sink.describe();
        Slot {
            sink: Some(sink),
            description,
        }
    }

    fn take(&mut self) -> Result<S> {
        self.sink
            .take()
            .ok_or_else(|| Error::Internal("the download sink was lost".into()))
    }

    /// Runs `work` with the sink on the blocking pool.
    async fn run<R, F>(&mut self, work: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut S) -> io::Result<R> + Send + 'static,
    {
        let mut sink = self.take()?;
        let (sink, result) = blocking(move || {
            let result = work(&mut sink);
            (sink, result)
        })
        .await?;
        self.sink = Some(sink);
        result.io_context(|| format!("saving the download to {}", self.description))
    }

    /// Writes and empties `buffer`.
    async fn flush(&mut self, buffer: &mut Vec<u8>) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(buffer);
        let mut data = self
            .run(move |sink| sink.write_all(&data).map(|()| data))
            .await?;
        data.clear();
        *buffer = data;
        Ok(())
    }
}

/// How one request attempt ended.
enum Fetched {
    Done(Outcome<DownloadOutcome>),
    /// The saved progress cannot be used; start over from byte 0.
    Restart,
}

/// What the streaming loop needs to save progress.
struct Progressing<'a> {
    state: DownloadState,
    /// Bytes received (written or buffered).
    received: u64,
    buffer: Vec<u8>,
    /// Checkpoints are only useful when the version can be pinned by ETag.
    can_resume: bool,
    checkpoint: &'a (dyn Fn(&DownloadState) + Send + Sync),
}

impl Progressing<'_> {
    /// Writes what is buffered, flushes the sink and emits a checkpoint.
    async fn save<S: DownloadSink + 'static>(&mut self, sink: &mut Slot<S>) -> Result<()> {
        sink.flush(&mut self.buffer).await?;
        if self.can_resume {
            sink.run(|s| s.sync()).await?;
            self.state.offset = self.received;
            (self.checkpoint)(&self.state);
        }
        Ok(())
    }
}

impl TransferEngine {
    /// Downloads `src` to the local file `dest`.
    ///
    /// A key ending in `/` (a folder marker) creates the folder `dest`
    /// instead; such an object that holds data is refused with
    /// [`Error::Usage`], because the folder would drop the data. A `dest`
    /// whose name ends in [`PARTIAL_SUFFIX`] is refused with
    /// [`Error::Usage`] too (see [`is_partial_name`]). Parent folders are
    /// created. `resume` is the last checkpoint of an earlier run of the
    /// same task. `checkpoint` must return quickly without blocking.
    pub async fn download_file(
        &self,
        src: &S3Uri,
        dest: &Path,
        hooks: &Hooks,
        resume: Option<DownloadState>,
        checkpoint: &(dyn Fn(&DownloadState) + Send + Sync),
    ) -> Result<Outcome<DownloadOutcome>> {
        if src.key().ends_with('/') {
            let outcome = self.create_local_folder(src, dest, hooks).await?;
            return Ok(outcome.map(|()| DownloadOutcome::default()));
        }
        require_object_key(src)?;
        reject_partial_name(dest)?;
        let mut target = dest.to_path_buf();
        if self.options.if_exists != IfExists::Overwrite && hooks.stop.reason().is_none() {
            let head = self.s3.head_object(src.bucket(), src.key(), None).await?;
            let info = ItemInfo {
                size: Some(head.size),
                modified: object_time(&head),
            };
            match self
                .place_local(TransferKind::Download, src.to_string(), info, dest, hooks)
                .await?
            {
                Placed::Go(placed) => target = placed,
                Placed::Skip(reason) => return Ok(Outcome::Skipped(reason)),
                Placed::Stop(reason) => return Ok(Outcome::stopped(reason)),
            }
            reject_partial_name(&target)?;
        }
        let renamed = (target != dest).then(|| target.clone());
        let dest = target.as_path();
        let partial = partial_path(dest)?;
        match hooks.stop.reason() {
            Some(StopReason::Cancel) => {
                remove_partial(&partial).await?;
                return Ok(Outcome::Cancelled);
            }
            Some(StopReason::Pause) => return Ok(Outcome::Paused),
            None => {}
        }
        let sink = {
            let partial = partial.clone();
            let stop = hooks.stop.token().clone();
            blocking(move || FileSink::open(&partial).map(|sink| sink.with_stop(stop))).await??
        };
        let (outcome, sink) = match self.download_to(src, sink, hooks, resume, checkpoint).await {
            Ok(done) => done,
            Err(err) => {
                // An empty partial file holds nothing to resume from.
                remove_if_empty(&partial).await;
                return Err(err);
            }
        };
        match outcome {
            Outcome::Completed(done) => {
                let dest = dest.to_path_buf();
                let modified = self
                    .options
                    .preserve_mtime
                    .then_some(done.file_time)
                    .flatten();
                blocking(move || sink.finish(&dest, modified)).await??;
                let mut done = done;
                done.renamed_to = renamed;
                Ok(Outcome::Completed(done))
            }
            Outcome::Paused => Ok(Outcome::Paused),
            Outcome::Cancelled => {
                drop(sink);
                remove_partial(&partial).await?;
                Ok(Outcome::Cancelled)
            }
            Outcome::Skipped(reason) => Ok(Outcome::Skipped(reason)),
        }
    }

    /// Deletes the partial file of a download to `dest`: for a stopped task
    /// that is cancelled, or a failed task that is given up. A missing file
    /// counts as deleted.
    ///
    /// A partial file that a download still holds open (a run that was
    /// dropped while a write or a flush was in progress, or another download
    /// of the same destination) is waited for, up to 30 seconds; if it is
    /// still in use then, the call fails with an [`ErrorKind::Io`] error and
    /// leaves the file, so that the caller can try again.
    pub async fn discard_download(&self, dest: &Path) -> Result<()> {
        discard_partial(dest).await
    }

    /// Downloads `src` into `sink` and hands the sink back.
    ///
    /// The sink is told where to continue ([`DownloadSink::prepare`]); after
    /// a cancel it is returned as is, and cleaning it up is the caller's
    /// job. See [`TransferEngine::download_file`] for `resume` and
    /// `checkpoint`.
    pub async fn download_to<S: DownloadSink + 'static>(
        &self,
        src: &S3Uri,
        sink: S,
        hooks: &Hooks,
        resume: Option<DownloadState>,
        checkpoint: &(dyn Fn(&DownloadState) + Send + Sync),
    ) -> Result<(Outcome<DownloadOutcome>, S)> {
        require_object_key(src)?;
        let mut sink = Slot::new(sink);
        if let Some(reason) = hooks.stop.reason() {
            return Ok((Outcome::stopped(Some(reason)), sink.take()?));
        }
        let mut saved = resume.filter(|s| {
            s.bucket == src.bucket()
                && s.key == src.key()
                && !s.etag.is_empty()
                && s.offset > 0
                && s.offset <= s.size
        });
        let mut restarted = false;
        loop {
            let resumed = saved.is_some();
            match self
                .fetch(src, &mut sink, saved.take(), hooks, checkpoint)
                .await?
            {
                Fetched::Done(mut outcome) => {
                    if let Outcome::Completed(done) = &mut outcome {
                        done.restarted = restarted;
                    }
                    return Ok((outcome, sink.take()?));
                }
                Fetched::Restart if resumed => {
                    restarted = true;
                    hooks.progress.set_done(0);
                }
                Fetched::Restart => {
                    return Err(Error::Internal(format!(
                        "downloading {src}: a fresh download asked to start over"
                    )));
                }
            }
        }
    }

    /// Creates the local folder of a folder marker.
    ///
    /// S3 allows data under a key that ends in `/`. An object like that is
    /// no folder marker, and making the folder would drop its bytes without
    /// a word, so it is refused. A key without any object is a folder that
    /// exists only as a prefix: its folder is created.
    async fn create_local_folder(
        &self,
        src: &S3Uri,
        dest: &Path,
        hooks: &Hooks,
    ) -> Result<Outcome<()>> {
        if let Some(reason) = hooks.stop.reason() {
            return Ok(Outcome::stopped(Some(reason)));
        }
        hooks.progress.set_total(0);
        hooks.progress.set_done(0);
        let head = tokio::select! {
            biased;
            () = hooks.stop.stopped() => return Ok(Outcome::stopped(hooks.stop.reason())),
            head = self.s3.head_object(src.bucket(), src.key(), None) => head,
        };
        match head {
            Ok(head) if head.size > 0 => {
                return Err(Error::Usage(data_under_folder_key(src, head.size)));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let dest = dest.to_path_buf();
        blocking(move || {
            std::fs::create_dir_all(&dest)
                .io_context(|| format!("creating the folder '{}'", dest.display()))
        })
        .await??;
        Ok(Outcome::Completed(()))
    }

    /// One request: continue from `saved`, or start from byte 0.
    async fn fetch<S: DownloadSink + 'static>(
        &self,
        src: &S3Uri,
        sink: &mut Slot<S>,
        saved: Option<DownloadState>,
        hooks: &Hooks,
        checkpoint: &(dyn Fn(&DownloadState) + Send + Sync),
    ) -> Result<Fetched> {
        let wanted = saved.as_ref().map_or(0, |s| s.offset);
        let offset = sink.run(move |s| s.prepare(wanted)).await?;
        if offset != wanted {
            if offset != 0 {
                return Err(Error::Internal(format!(
                    "{} continued at byte {offset} instead of {wanted} or 0",
                    sink.description
                )));
            }
            return Ok(Fetched::Restart);
        }
        hooks.progress.set_done(offset);
        if let Some(state) = &saved {
            hooks.progress.set_total(state.size);
            if offset == state.size {
                // Every byte is already here.
                let progressing = Progressing {
                    state: state.clone(),
                    received: offset,
                    buffer: Vec::new(),
                    can_resume: true,
                    checkpoint,
                };
                return self.finish(src, sink, progressing, offset, hooks).await;
            }
        }

        // One of the few streams a low bandwidth cap can keep alive at once
        // (see `Throttle::open_stream`); held until the body is read.
        let _stream = tokio::select! {
            biased;
            () = hooks.stop.stopped() => {
                return Ok(Fetched::Done(Outcome::stopped(hooks.stop.reason())));
            }
            permit = hooks.open_stream() => permit,
        };
        let response = tokio::select! {
            biased;
            () = hooks.stop.stopped() => {
                return Ok(Fetched::Done(Outcome::stopped(hooks.stop.reason())));
            }
            response = self.get_object(src, saved.as_ref()) => response,
        };
        let out = match response {
            Ok(out) => out,
            Err(err) if saved.is_some() && object_changed(&err) => return Ok(Fetched::Restart),
            Err(err) => return Err(err),
        };
        let fresh = saved.is_none();
        let state = match saved {
            Some(mut state) if answers(&out, &state) => {
                if state.last_modified.is_none() {
                    state.last_modified = out.last_modified().and_then(to_utc);
                }
                state
            }
            Some(_) => return Ok(Fetched::Restart),
            None => self.fresh_state(src, &out)?,
        };
        hooks.progress.set_total(state.size);
        let can_resume = !state.etag.is_empty();
        if fresh && can_resume {
            // Replaces any older state at once: the partial file now holds
            // this version (a state at offset 0 means "start over").
            checkpoint(&state);
        }
        let mut progressing = Progressing {
            can_resume,
            state,
            received: offset,
            buffer: Vec::with_capacity(WRITE_BATCH),
            checkpoint,
        };
        let mut body = out.body;
        let mut last_checkpoint = Instant::now();
        loop {
            let next = tokio::select! {
                biased;
                () = hooks.stop.stopped() => None,
                chunk = body.try_next() => Some(chunk),
            };
            let chunk = match next {
                None => {
                    return match hooks.stop.reason() {
                        Some(StopReason::Cancel) => Ok(Fetched::Done(Outcome::Cancelled)),
                        Some(StopReason::Pause) | None => {
                            progressing.save(sink).await?;
                            Ok(Fetched::Done(Outcome::Paused))
                        }
                    };
                }
                Some(Ok(Some(chunk))) => chunk,
                Some(Ok(None)) => break,
                Some(Err(err)) => {
                    return Err(self.stream_failed(src, sink, progressing, &err).await);
                }
            };
            let len = chunk.len() as u64;
            if progressing.received + len > progressing.state.size {
                return Err(Error::Remote(format!(
                    "downloading {src}: the server sent more than the {} bytes it announced",
                    progressing.state.size
                )));
            }
            progressing.buffer.extend_from_slice(&chunk);
            progressing.received += len;
            hooks.progress.add_done(len);
            tokio::select! {
                biased;
                () = hooks.stop.stopped() => {}
                () = hooks.throttle(len) => {}
            }
            if progressing.buffer.len() >= WRITE_BATCH {
                sink.flush(&mut progressing.buffer).await?;
            }
            if progressing.can_resume && last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                progressing.save(sink).await?;
                last_checkpoint = Instant::now();
            }
        }
        sink.flush(&mut progressing.buffer).await?;
        if progressing.received != progressing.state.size {
            let received = progressing.received;
            let size = progressing.state.size;
            progressing.save(sink).await?;
            return Err(Error::Remote(format!(
                "downloading {src}: the connection ended after {received} of {size} bytes"
            )));
        }
        self.finish(src, sink, progressing, offset, hooks).await
    }

    /// All bytes are in the sink: validate a resumed download, flush, done.
    ///
    /// Reading a large partial file back takes a while; a stop request ends
    /// it early (see [`FileSink::with_stop`]). A pause then keeps the file
    /// and a checkpoint with every byte present, so that the next run
    /// verifies again; a cancel leaves the deleting to the caller.
    async fn finish<S: DownloadSink + 'static>(
        &self,
        src: &S3Uri,
        sink: &mut Slot<S>,
        mut progressing: Progressing<'_>,
        resumed_from: u64,
        hooks: &Hooks,
    ) -> Result<Fetched> {
        if resumed_from > 0
            && self.options.validate_downloads
            && let Some(expected) = progressing.state.checksum.clone()
        {
            let algorithm = expected.algorithm;
            let actual = match sink.run(move |s| s.checksum(algorithm)).await {
                Ok(actual) => actual,
                Err(err) if interrupted_by_stop(&err, hooks) => {
                    let outcome = match hooks.stop.reason() {
                        Some(StopReason::Cancel) => Outcome::Cancelled,
                        Some(StopReason::Pause) | None => {
                            progressing.save(sink).await?;
                            Outcome::Paused
                        }
                    };
                    return Ok(Fetched::Done(outcome));
                }
                Err(err) => return Err(err),
            };
            if actual.is_some_and(|actual| actual != expected.value) {
                sink.run(|s| s.prepare(0)).await?;
                if progressing.can_resume {
                    progressing.state.offset = 0;
                    (progressing.checkpoint)(&progressing.state);
                }
                return Err(Error::Remote(format!(
                    "downloading {src}: the downloaded data does not match the object's {} checksum; the partial data was discarded",
                    super::checksum::name(algorithm)
                )));
            }
        }
        sink.run(|s| s.sync()).await?;
        let state = progressing.state;
        if progressing.can_resume {
            // Lets a retry finish without downloading again when only the
            // final rename failed.
            (progressing.checkpoint)(&DownloadState {
                offset: state.size,
                ..state.clone()
            });
        }
        Ok(Fetched::Done(Outcome::Completed(DownloadOutcome {
            size: state.size,
            etag: (!state.etag.is_empty()).then(|| unquote(&state.etag).to_owned()),
            last_modified: state.last_modified,
            file_time: state.mtime.or(state.last_modified),
            renamed_to: None,
            resumed_from,
            restarted: false,
        })))
    }

    /// Saves what arrived before a stream error, unless the data failed
    /// validation, and builds the error.
    async fn stream_failed<S: DownloadSink + 'static>(
        &self,
        src: &S3Uri,
        sink: &mut Slot<S>,
        mut progressing: Progressing<'_>,
        err: &ByteStreamError,
    ) -> Error {
        if is_checksum_mismatch(err) {
            let discarded = sink.run(|s| s.prepare(0)).await;
            if progressing.can_resume {
                progressing.state.offset = 0;
                (progressing.checkpoint)(&progressing.state);
            }
            let failure = Error::Remote(format!(
                "downloading {src}: the data received does not match the object's checksum; the partial data was discarded ({})",
                DisplayErrorContext(err)
            ));
            return match discarded {
                Ok(_) => failure,
                Err(write_err) => write_err.with_context(failure.to_string()),
            };
        }
        let failure = Error::Remote(format!("downloading {src}: {}", DisplayErrorContext(err)));
        match progressing.save(sink).await {
            Ok(()) => failure,
            Err(save_err) => save_err.with_context(failure.to_string()),
        }
    }

    async fn get_object(
        &self,
        src: &S3Uri,
        saved: Option<&DownloadState>,
    ) -> Result<GetObjectOutput> {
        let client = bucket_client(&self.s3, src.bucket()).await?;
        let mut request = client
            .get_object()
            .bucket(src.bucket())
            .key(src.key())
            .set_version_id(self.download_version.clone())
            .set_request_payer(self.s3.request_payer());
        if let Some(state) = saved {
            request = request
                .range(format!("bytes={}-", state.offset))
                .if_match(&state.etag);
        }
        // The read timeout ends when the response headers arrive; the body is
        // watched by stalled-stream protection.
        let sent = if self.options.validate_downloads {
            request
                .checksum_mode(ChecksumMode::Enabled)
                .customize()
                .config_override(self.s3.metadata_timeout())
                .send()
                .await
        } else {
            request
                .customize()
                .config_override(without_response_validation(self.s3.metadata_timeout()))
                .send()
                .await
        };
        sent.map_err(|err| Error::from(err).with_context(src))
    }

    /// The state of a download that starts at byte 0.
    fn fresh_state(&self, src: &S3Uri, out: &GetObjectOutput) -> Result<DownloadState> {
        let size = out
            .content_length()
            .and_then(|len| u64::try_from(len).ok())
            .ok_or_else(|| {
                Error::Remote(format!(
                    "downloading {src}: the server did not report the object size"
                ))
            })?;
        let checksum = if self.options.validate_downloads {
            full_object_checksum(out)
        } else {
            None
        };
        Ok(DownloadState {
            bucket: src.bucket().to_owned(),
            key: src.key().to_owned(),
            etag: out.e_tag().unwrap_or_default().to_owned(),
            size,
            last_modified: out.last_modified().and_then(to_utc),
            mtime: out
                .metadata()
                .and_then(|meta| meta.get(MTIME_META_KEY))
                .and_then(|text| parse_mtime(text)),
            checksum,
            offset: 0,
        })
    }
}

/// Deletes the partial file of a download to `dest`, waiting for a download
/// that still holds it (see [`remove_partial_waiting`]).
pub(crate) async fn discard_partial(dest: &Path) -> Result<()> {
    remove_partial_waiting(&partial_path(dest)?, CLEANUP_LOCK_WAIT).await
}

/// What a deletion of a partial file found.
enum Removal {
    /// The file is gone (or never existed).
    Gone,
    /// A download holds the file's lock; nothing was deleted.
    Busy,
}

/// Deletes a partial file, unless a download is writing it right now (it
/// holds the file's lock, see [`FileSink::open`]): that file is left alone.
/// A missing file counts as deleted.
async fn remove_partial(partial: &Path) -> Result<()> {
    try_remove_partial(partial).await.map(|_| ())
}

/// Like [`remove_partial`] for a clean-up, which must be sure the file is
/// gone: a download that still holds the file (typically a run that was
/// dropped while a write or a flush was in progress, which ends in moments)
/// is waited for, for up to `wait`. If the file is still in use then, the
/// error has the kind [`ErrorKind::Io`] and the file stays, so that the
/// caller can try again.
async fn remove_partial_waiting(partial: &Path, wait: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut pause = LOCK_POLL.0;
    loop {
        if matches!(try_remove_partial(partial).await?, Removal::Gone) {
            return Ok(());
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(Error::io(
                format!("deleting the partial file '{}'", partial.display()),
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "a download is still using it, so it was left in place",
                ),
            ));
        }
        tokio::time::sleep(pause.min(left)).await;
        pause = (pause * 2).min(LOCK_POLL.1);
    }
}

/// Whether `err` proves that nothing exists at the path: either the name is
/// missing, or a parent component is not a folder (so no partial file can be
/// there either).
fn is_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

async fn try_remove_partial(partial: &Path) -> Result<Removal> {
    let partial = partial.to_path_buf();
    blocking(move || {
        let context = || format!("deleting the partial file '{}'", partial.display());
        match std::fs::symlink_metadata(&partial) {
            Ok(named) if named.file_type().is_symlink() => {
                // Not a download in progress (downloads replace a link under
                // the name): only the link goes, never what it points to.
                return remove_name(&partial)
                    .map(|()| Removal::Gone)
                    .map_err(|err| Error::io(context(), err));
            }
            Ok(_) => {}
            Err(err) if is_absent(&err) => return Ok(Removal::Gone),
            Err(err) => return Err(Error::io(context(), err)),
        }
        let file = match File::open(&partial) {
            Ok(file) => file,
            Err(err) if is_absent(&err) => return Ok(Removal::Gone),
            Err(err) => return Err(Error::io(context(), err)),
        };
        if let Err(TryLockError::WouldBlock) = file.try_lock() {
            return Ok(Removal::Busy);
        }
        // Deleted while the lock is held, so no download can open it in
        // between; the handle is closed right after.
        let removed = match std::fs::remove_file(&partial) {
            Ok(()) => Ok(Removal::Gone),
            Err(err) if is_absent(&err) => Ok(Removal::Gone),
            Err(err) => Err(Error::io(context(), err)),
        };
        drop(file);
        removed
    })
    .await?
}

/// Refuses a destination whose name is reserved for partial files.
fn reject_partial_name(dest: &Path) -> Result<()> {
    if dest.file_name().is_some_and(is_partial_name) {
        return Err(Error::Usage(format!(
            "'{}' ends in '{PARTIAL_SUFFIX}', which is reserved for the temporary files of downloads; download the object under another name",
            dest.display()
        )));
    }
    Ok(())
}

/// Why an object whose key ends in `/` is not downloaded: it holds data, so
/// it is no folder marker.
pub(super) fn data_under_folder_key(uri: impl std::fmt::Display, size: u64) -> String {
    format!(
        "{uri} holds {size} bytes, but its key ends in '/', which names a folder; a folder cannot hold data on disk, so the object is not downloaded (rename it to download it)"
    )
}

/// True when a read-back that failed was ended by a stop request.
fn interrupted_by_stop(err: &Error, hooks: &Hooks) -> bool {
    hooks.stop.is_stopped()
        && matches!(err, Error::Io { source, .. } if source.kind() == io::ErrorKind::Interrupted)
}

/// Deletes a partial file that holds no data (best effort).
async fn remove_if_empty(partial: &Path) {
    let partial = partial.to_path_buf();
    let _ = blocking(move || {
        if std::fs::metadata(&partial).is_ok_and(|m| m.is_file() && m.len() == 0) {
            let _ = std::fs::remove_file(&partial);
        }
    })
    .await;
}

/// True when a ranged request failed because the object is not the saved
/// version any more (`If-Match` failed, or the range is past its end).
fn object_changed(err: &Error) -> bool {
    err.service().is_some_and(|service| {
        matches!(service.status(), Some(412 | 416))
            || matches!(service.code(), Some("PreconditionFailed" | "InvalidRange"))
    })
}

/// True when the answer to a ranged request is the rest of the saved version.
fn answers(out: &GetObjectOutput, state: &DownloadState) -> bool {
    let same_version = out
        .e_tag()
        .is_some_and(|etag| unquote(etag) == unquote(&state.etag));
    let rest = state.size - state.offset;
    let same_length = out.content_length().and_then(|len| u64::try_from(len).ok()) == Some(rest);
    let same_range = out.content_range().and_then(parse_content_range)
        == Some((state.offset, state.size - 1, state.size));
    same_version && same_length && same_range
}

/// Parses `bytes first-last/total`.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    Some((
        first.trim().parse().ok()?,
        last.trim().parse().ok()?,
        total.trim().parse().ok()?,
    ))
}

/// The first full-object checksum of an answer, fastest algorithm first.
fn full_object_checksum(out: &GetObjectOutput) -> Option<ObjectChecksum> {
    ALGORITHMS.into_iter().find_map(|algorithm| {
        checksum_field!(out, algorithm)
            .filter(|value| is_full_object(value))
            .map(|value| ObjectChecksum {
                algorithm,
                value: value.to_owned(),
            })
    })
}

/// True when the SDK rejected the received data because its checksum did
/// not match the one the server sent.
fn is_checksum_mismatch(err: &ByteStreamError) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(error) = current {
        if error
            .downcast_ref::<aws_smithy_checksums::body::validate::Error>()
            .is_some()
        {
            return true;
        }
        current = error.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_paths_append_the_suffix() {
        assert_eq!(
            partial_path(Path::new("dir/a.jpg")).unwrap(),
            Path::new("dir/a.jpg.bbpart")
        );
        assert_eq!(
            partial_path(Path::new("plain")).unwrap(),
            Path::new("plain.bbpart")
        );
        assert!(partial_path(Path::new("/")).is_err());
        assert!(partial_path(Path::new("dir/..")).is_err());
    }

    #[test]
    fn content_ranges_parse() {
        assert_eq!(
            parse_content_range("bytes 100-199/200"),
            Some((100, 199, 200))
        );
        assert_eq!(parse_content_range("bytes 0-0/1"), Some((0, 0, 1)));
        assert_eq!(parse_content_range("bytes */200"), None);
        assert_eq!(parse_content_range("items 1-2/3"), None);
        assert_eq!(parse_content_range("bytes 1-2/*"), None);
    }

    #[test]
    fn file_sink_continues_or_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("f.bin.bbpart");
        let mut sink = FileSink::open(&path).unwrap();
        assert_eq!(sink.path(), path);
        assert!(sink.describe().contains("f.bin.bbpart"));
        assert_eq!(sink.prepare(0).unwrap(), 0);
        sink.write_all(b"hello world").unwrap();
        sink.sync().unwrap();
        let crc = sink.checksum(ChecksumAlgorithm::Crc32).unwrap();
        assert_eq!(crc.as_deref(), Some("DUoRhQ=="));
        assert_eq!(sink.checksum(ChecksumAlgorithm::None).unwrap(), None);
        // Checksumming does not move the write position.
        sink.write_all(b"!").unwrap();
        drop(sink);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world!");

        // Continue after 5 bytes: the rest is dropped.
        let mut sink = FileSink::open(&path).unwrap();
        assert_eq!(sink.prepare(5).unwrap(), 5);
        sink.write_all(b"-tail").unwrap();
        drop(sink);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello-tail");

        // Asking for more than the file holds starts over.
        let mut sink = FileSink::open(&path).unwrap();
        // A second download of the same file is refused while this one runs,
        // with a kind the queue retries.
        let err = FileSink::open(&path).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io, "{err}");
        assert!(crate::transfer::is_retryable(err.kind()));
        assert!(err.to_string().contains("another download"), "{err}");
        assert_eq!(sink.prepare(100).unwrap(), 0);
        sink.write_all(b"new").unwrap();
        let dest = dir.path().join("final.bin");
        let modified = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        sink.finish(&dest, Some(modified)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert!(!path.exists());
        let mtime: DateTime<Utc> = std::fs::metadata(&dest).unwrap().modified().unwrap().into();
        assert_eq!(mtime, modified);
    }

    #[tokio::test]
    async fn a_partial_file_under_a_file_instead_of_a_folder_counts_as_gone() {
        let dir = tempfile::tempdir().unwrap();
        // A file sits where the destination folder should be, so no partial
        // file can exist below it: the clean-up has nothing to do.
        std::fs::write(dir.path().join("out"), b"not a folder").unwrap();
        let dest = dir.path().join("out").join("f1.bin");
        let partial = partial_path(&dest).unwrap();
        remove_partial(&partial).await.unwrap();
        discard_partial(&dest).await.unwrap();
        assert!(
            dir.path().join("out").is_file(),
            "the blocking file was touched"
        );
    }

    #[tokio::test]
    async fn discarding_waits_for_a_download_that_still_holds_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("busy.bin");
        let partial = partial_path(&dest).unwrap();
        discard_partial(&dest).await.unwrap();
        let mut sink = FileSink::open(&partial).unwrap();
        sink.write_all(b"in progress").unwrap();

        // The engine's own cancel leaves a file that is being written alone.
        remove_partial(&partial).await.unwrap();
        assert!(partial.exists(), "a running download's file was deleted");

        // A clean-up that outlasts its wait fails (retryable) and keeps it.
        let err = remove_partial_waiting(&partial, Duration::from_millis(60))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io, "{err}");
        assert!(err.to_string().contains("still using it"), "{err}");
        assert!(partial.exists());

        // The holder is a dropped run finishing a write: the clean-up waits
        // for it and then deletes the file.
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(sink);
        });
        remove_partial_waiting(&partial, Duration::from_secs(20))
            .await
            .unwrap();
        holder.join().unwrap();
        assert!(!partial.exists());
        discard_partial(&dest).await.unwrap();
    }

    #[test]
    fn names_ending_in_the_partial_suffix_are_reserved() {
        for name in [
            "a.bbpart",
            "A.BBPART",
            "x.y.BbPart",
            ".bbpart",
            "a.bbpart.bbpart",
        ] {
            assert!(is_partial_name(name), "{name}");
        }
        for name in [
            "a",
            "a.bbpar",
            "bbpart",
            "a.bbpart.txt",
            "a.bbpart/",
            "",
            "a_bbpart",
        ] {
            assert!(!is_partial_name(name), "{name}");
        }
        assert!(is_partial_name(OsStr::new("dir.bbpart")));
        assert!(is_partial_name(String::from("é.bbpart")));

        // No download writes under such a name, and no sink opens one that is
        // not a partial file.
        let err = reject_partial_name(Path::new("dir/a.bbpart")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("reserved"), "{err}");
        reject_partial_name(Path::new("dir/a")).unwrap();
        reject_partial_name(Path::new("dir.bbpart/a")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ordinary = dir.path().join("keep.txt");
        std::fs::write(&ordinary, b"mine").unwrap();
        let err = FileSink::open(&ordinary).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
        assert_eq!(std::fs::read(&ordinary).unwrap(), b"mine");
    }

    #[test]
    fn reading_the_partial_file_back_ends_at_a_stop_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin.bbpart");
        let stop = CancellationToken::new();
        let mut sink = FileSink::open(&path).unwrap().with_stop(stop.clone());
        sink.prepare(0).unwrap();
        let chunk = vec![7u8; VERIFY_CHUNK];
        for _ in 0..3 {
            sink.write_all(&chunk).unwrap();
        }
        // Not stopped: the whole file is read back.
        let whole = sink.checksum(ChecksumAlgorithm::Crc32).unwrap();
        assert!(whole.is_some());

        stop.cancel();
        let err = sink.checksum(ChecksumAlgorithm::Crc32).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        // The write position is where writing continues.
        sink.write_all(b"!").unwrap();
        drop(sink);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            3 * VERIFY_CHUNK as u64 + 1
        );

        let hooks = Hooks::new();
        let interrupted = Error::io("x", io::Error::from(io::ErrorKind::Interrupted));
        assert!(!interrupted_by_stop(&interrupted, &hooks), "no stop yet");
        hooks.stop.pause();
        assert!(interrupted_by_stop(&interrupted, &hooks));
        assert!(!interrupted_by_stop(
            &Error::io("x", io::Error::from(io::ErrorKind::NotFound)),
            &hooks
        ));
    }

    #[test]
    fn state_shape_is_stable() {
        let state = DownloadState {
            bucket: "b".into(),
            key: "photos/a.jpg".into(),
            etag: "\"abc\"".into(),
            size: 10,
            last_modified: DateTime::from_timestamp(1_700_000_000, 0),
            checksum: Some(ObjectChecksum {
                algorithm: ChecksumAlgorithm::Crc64nvme,
                value: "v==".into(),
            }),
            mtime: None,
            offset: 4,
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value,
            json!({
                "bucket": "b",
                "key": "photos/a.jpg",
                "etag": "\"abc\"",
                "size": 10,
                "last_modified": "2023-11-14T22:13:20Z",
                "checksum": { "algorithm": "crc64nvme", "value": "v==" },
                "offset": 4
            })
        );
        assert_eq!(
            serde_json::from_value::<DownloadState>(value).unwrap(),
            state
        );
    }
}
