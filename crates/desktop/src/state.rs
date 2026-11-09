//! Application state shared by every command: [`AppState`] and [`Core`].

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bucketbridge_core::edit::{EditManager, EditOptions, KEEP_LEFTOVER_COPIES};
use bucketbridge_core::engine::{S3Provider, S3Runner};
use bucketbridge_core::transfer::{BandwidthLimiter, QueueOptions, TransferQueue};
use bucketbridge_core::{
    Dirs, Error, EventLog, Result, Session, Settings, SyncExecutor, SyncStore, crash,
};

use crate::error::CommandError;
use crate::{logging, open};

/// Version of the app, from the workspace `Cargo.toml`.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// File in the data folder that the running app keeps locked.
const INSTANCE_LOCK_FILE: &str = "instance.lock";

/// File in the data folder that holds the persisted transfer queue.
const QUEUE_FILE: &str = "transfers.json";

/// Folder in the data folder that holds the copies of edit sessions.
const EDIT_DIR_NAME: &str = "edit";

/// How long a transfer gets to stop after a pause or cancel request, and
/// when the app exits; what is still running after that is dropped (the queue
/// keeps its last checkpoint). The queue's default, spelled out because exit
/// waits for it.
const QUEUE_STOP_GRACE: Duration = Duration::from_secs(5);

/// Opens a local file with the system's default application.
pub type Opener = Arc<dyn Fn(&Path) -> std::result::Result<(), String> + Send + Sync>;

/// Opens a local file with the given application (an `.app` bundle on macOS,
/// an executable elsewhere).
pub type OpenWith = Arc<dyn Fn(&Path, &str) -> std::result::Result<(), String> + Send + Sync>;

/// Opens an `http(s)` URL in the system browser.
pub type UrlOpener = Arc<dyn Fn(&str) -> std::result::Result<(), String> + Send + Sync>;

/// What [`Core::new`] is built from.
pub struct CoreParts {
    /// Application folders.
    pub dirs: Dirs,
    /// Accounts, secrets, settings and the cached S3 handles.
    pub session: Session,
    /// The event log behind `get_log` and `log://entry`.
    pub log: EventLog,
    /// How `open_object` hands a downloaded file to the OS.
    pub opener: Opener,
    /// How `open_object_with` hands a downloaded file to a chosen application.
    pub open_with: OpenWith,
    /// How `open_bucket_in_browser` opens the bucket URL.
    pub url_opener: UrlOpener,
}

/// Everything the commands share. Created once at startup.
pub struct Core {
    pub(crate) dirs: Dirs,
    /// Shared with the queue's runners, which reach the accounts through it.
    pub(crate) session: Arc<Session>,
    pub(crate) log: EventLog,
    pub(crate) sync: SyncExecutor,
    /// The transfer queue, persisted in `data_dir/transfers.json`.
    pub(crate) queue: Arc<TransferQueue>,
    /// The queue's S3 runners; `save_settings` hands them the data-integrity
    /// settings.
    pub(crate) runner: S3Runner,
    /// The one bandwidth cap (`settings.transfer.bandwidth_limit_kibps`):
    /// the queue's own limiter, shared by every queued transfer, every sync
    /// run and `open_object`. Updated by `save_settings`.
    pub(crate) limiter: BandwidthLimiter,
    pub(crate) opener: Opener,
    /// Active drive mounts (`mount_bucket`); dropped mounts are unmounted.
    pub(crate) mounts: crate::commands::mount::MountRegistry,
    pub(crate) open_with: OpenWith,
    pub(crate) url_opener: UrlOpener,
    /// Edit-in-place sessions (`start_edit`).
    pub(crate) edits: EditManager,
    /// Quit, link and window-state bookkeeping of the host.
    pub(crate) lifecycle: crate::quit::Lifecycle,
    runs: Mutex<Runs>,
    runs_changed: Condvar,
    _instance: Option<InstanceLock>,
}

/// Sync runs started by `sync_execute` that have not ended yet.
#[derive(Default)]
struct Runs {
    next_token: u64,
    plans: HashMap<u64, String>,
}

impl Core {
    /// Builds the state from its parts: reads the settings, opens the sync
    /// plan store and the transfer queue (with its saved tasks) in the data
    /// folder. Blocking: call it outside an async runtime.
    pub fn new(parts: CoreParts) -> Result<Arc<Core>> {
        Self::build(parts, None)
    }

    fn build(parts: CoreParts, instance: Option<InstanceLock>) -> Result<Arc<Core>> {
        let settings = parts.session.accounts().load()?.settings;
        let store = SyncStore::open_in(&parts.dirs)?;
        let session = Arc::new(parts.session);
        let runner = S3Runner::new(s3_provider(&session), settings.data_integrity.clone());
        let options = QueueOptions {
            stop_grace: QUEUE_STOP_GRACE,
            ..QueueOptions::persistent(parts.dirs.data_dir.join(QUEUE_FILE))
        };
        let queue = tauri::async_runtime::block_on(TransferQueue::open(
            settings.transfer.clone(),
            runner.runners(),
            options,
        ))?;
        let edits = EditManager::new(
            parts.dirs.data_dir.join(EDIT_DIR_NAME),
            s3_provider(&session),
            EditOptions::default(),
        )
        .with_throttle(Arc::new(open::LimiterThrottle(queue.limiter())));
        Ok(Arc::new(Core {
            limiter: queue.limiter(),
            edits,
            queue: Arc::new(queue),
            runner,
            sync: SyncExecutor::new(Arc::new(store)),
            dirs: parts.dirs,
            session,
            log: parts.log,
            opener: parts.opener,
            mounts: Default::default(),
            open_with: parts.open_with,
            url_opener: parts.url_opener,
            lifecycle: crate::quit::Lifecycle::new(&settings),
            runs: Mutex::new(Runs::default()),
            runs_changed: Condvar::new(),
            _instance: instance,
        }))
    }

    /// Startup of the real app: application folders (`BB_CONFIG_DIR` or the
    /// platform folders), the data-folder lock, the crash reporter, the
    /// session on the OS keychain, the event log with file logging per the
    /// settings and its `tracing` layer. Blocking.
    pub(crate) fn start() -> Result<Arc<Core>> {
        let dirs = Dirs::discover()?;
        dirs.create_all()?;
        let instance = InstanceLock::acquire(&dirs.data_dir)?;
        crash::install(dirs.crash_dir.clone(), VERSION);
        let session = Session::open(dirs.clone())?;
        let settings = session.accounts().load()?.settings;
        let log = EventLog::new(&settings.logging, Some(dirs.log_dir.clone()));
        logging::install(&log);
        let opener: Opener = Arc::new(|path: &Path| {
            tauri_plugin_opener::open_path(path, None::<&str>).map_err(|err| err.to_string())
        });
        let open_with: OpenWith = Arc::new(|path: &Path, app: &str| {
            tauri_plugin_opener::open_path(path, Some(app)).map_err(|err| err.to_string())
        });
        let url_opener: UrlOpener = Arc::new(|url: &str| {
            tauri_plugin_opener::open_url(url, None::<&str>).map_err(|err| err.to_string())
        });
        let core = Self::build(
            CoreParts {
                dirs,
                session,
                log,
                opener,
                open_with,
                url_opener,
            },
            instance,
        )?;
        tracing::info!(target: "bucketbridge::app", "BucketBridge {VERSION} started");
        spawn_log_cleanup(&core);
        Ok(core)
    }

    /// The application folders.
    pub fn dirs(&self) -> &Dirs {
        &self.dirs
    }

    /// The session.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The event log.
    pub fn log(&self) -> &EventLog {
        &self.log
    }

    /// The transfer queue behind the `enqueue_*` and queue-control commands.
    pub fn queue(&self) -> &TransferQueue {
        &self.queue
    }

    /// The queue's S3 runners (they hold the data-integrity settings of the
    /// transfers that start from now on).
    pub fn runner(&self) -> &S3Runner {
        &self.runner
    }

    /// The one bandwidth cap of the app: the queue's limiter, shared by
    /// queued transfers, sync runs and `open_object`.
    pub fn limiter(&self) -> &BandwidthLimiter {
        &self.limiter
    }

    /// Applies saved settings to the running app: log level and file
    /// logging, the queue (concurrency, retries, part sizes), the data
    /// integrity of the runners and the bandwidth cap of running and future
    /// transfers.
    pub(crate) fn apply_settings(&self, settings: &Settings) {
        self.log.apply_settings(&settings.logging);
        self.lifecycle.apply_settings(settings);
        self.runner
            .set_data_integrity(settings.data_integrity.clone());
        if let Err(error) = self.queue.update_settings(settings.transfer.clone()) {
            // The queue only refuses once it has been shut down.
            tracing::warn!(
                target: "bucketbridge::desktop",
                "the transfer queue did not take the new settings: {error}"
            );
        }
        // The queue sets the shared limiter itself; this keeps sync runs and
        // `open_object` in line when it refuses.
        let rate = bandwidth_rate(settings);
        if self.limiter.rate() != rate {
            self.limiter.set_rate(rate);
        }
    }

    /// The transfers and sync runs that are active now; `since` as in
    /// [`crate::activity::summarize`].
    pub(crate) fn activity(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::activity::Activity {
        let runs = lock(&self.runs).plans.len();
        crate::activity::summarize(&self.queue.list(), runs, since)
    }

    /// Records a sync run that is about to be spawned; returns its token.
    pub(crate) fn begin_run(&self, plan_id: &str) -> u64 {
        let mut runs = lock(&self.runs);
        let token = runs.next_token;
        runs.next_token = runs.next_token.wrapping_add(1);
        runs.plans.insert(token, plan_id.to_owned());
        token
    }

    /// Records the end of the run with `token`.
    pub(crate) fn end_run(&self, token: u64) {
        lock(&self.runs).plans.remove(&token);
        self.runs_changed.notify_all();
    }

    /// Called when the app exits. Blocking.
    ///
    /// 1. The transfer queue stops: it refuses further commands, turns
    ///    queued tasks into `stopped` ones and asks running transfers to
    ///    pause. They get the queue's own grace period of five seconds, then
    ///    the queue file is saved, so the next start finds every unfinished
    ///    task `stopped` with its resume state.
    /// 2. At the same time, running sync runs are cancelled (their engines
    ///    abort multipart uploads and delete partial files; the rows stay
    ///    pending) and awaited for up to `grace`.
    /// 3. It waits for the queue to finish saving.
    /// 4. The log file is flushed.
    ///
    /// Calling it again is harmless.
    pub fn shutdown(&self, grace: Duration) {
        self.mounts.clear();
        self.shutdown_edits(grace);
        let queue = Arc::clone(&self.queue);
        let stopping = tauri::async_runtime::spawn(async move { queue.shutdown().await });
        let plans: Vec<String> = lock(&self.runs).plans.values().cloned().collect();
        for plan in &plans {
            // An invalid id cannot be running; nothing to report.
            let _ = self.sync.cancel(plan);
        }
        let deadline = Instant::now().checked_add(grace);
        let mut runs = lock(&self.runs);
        while !runs.plans.is_empty() {
            let left = match deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => grace,
            };
            if left.is_zero() {
                break;
            }
            runs = self
                .runs_changed
                .wait_timeout(runs, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let unfinished = runs.plans.len();
        drop(runs);
        if unfinished > 0 {
            tracing::warn!(
                target: "bucketbridge::app",
                "{unfinished} sync run(s) did not stop in time; their rows stay pending"
            );
        }
        match tauri::async_runtime::block_on(stopping) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(
                target: "bucketbridge::app",
                "the transfer queue was not saved at exit: {error}"
            ),
            Err(error) => tracing::error!(
                target: "bucketbridge::app",
                "stopping the transfer queue failed: {error}"
            ),
        }
        tracing::info!(target: "bucketbridge::app", "BucketBridge stopped");
        self.log.flush(Duration::from_secs(2));
    }

    /// Uploads the pending saves of the edit sessions and ends them. A
    /// session that cannot upload (a conflict or an error) keeps its copy on
    /// disk and is logged. Blocking.
    fn shutdown_edits(&self, grace: Duration) {
        let edits = self.edits.clone();
        let left = tauri::async_runtime::block_on(async move { edits.shutdown(grace).await });
        for session in left {
            tracing::warn!(
                target: "bucketbridge::app",
                "s3://{}/{} was not uploaded at exit; the edited copy stays in {}",
                session.bucket,
                session.key,
                session.local_path
            );
        }
    }

    /// Removes the copies of edit sessions that never ended and are a week
    /// old (best effort, blocking).
    pub(crate) fn purge_edit_copies(&self) {
        self.edits.purge_stale(KEEP_LEFTOVER_COPIES);
    }

    /// Removes `open_object` copies older than a day (best effort, blocking).
    pub(crate) fn purge_opened_files(&self) {
        open::purge(&open::open_root(&self.dirs), open::KEEP_OPENED_FILES);
    }
}

/// The state managed by the app: the [`Core`], or the error that kept it from
/// starting. In that case every command rejects with that error, so the UI
/// can show it (for example an unreadable `config.toml`).
pub struct AppState(std::result::Result<Arc<Core>, CommandError>);

impl AppState {
    /// A started app.
    pub fn ready(core: Arc<Core>) -> Self {
        AppState(Ok(core))
    }

    /// An app whose startup failed with `error`.
    pub fn failed(error: CommandError) -> Self {
        AppState(Err(error))
    }

    /// The core, or the startup error.
    pub fn core(&self) -> std::result::Result<Arc<Core>, CommandError> {
        self.0.clone()
    }
}

/// How the queue's runners reach an account: through the session, at the
/// start of every run and cleanup, so a run always uses the account's current
/// credentials and fails with `locked` or `mfa_required` while they are not
/// available (the task becomes `failed`; `retry_task` runs it again).
fn s3_provider(session: &Arc<Session>) -> S3Provider {
    let session = Arc::clone(session);
    Arc::new(move |account_id: String| {
        let session = Arc::clone(&session);
        Box::pin(async move { session.s3_for(&account_id).await })
    })
}

/// `settings.transfer.bandwidth_limit_kibps` in bytes per second.
fn bandwidth_rate(settings: &Settings) -> Option<NonZeroU64> {
    settings
        .transfer
        .bandwidth_limit_kibps
        .and_then(|kibps| NonZeroU64::new(kibps.saturating_mul(1024)))
}

/// Exclusive lock on `data_dir/instance.lock`, held for the life of the
/// process (the OS releases it when the process ends, also after a crash).
///
/// The single-instance plugin already turns a second launch into a focus of
/// the first window; this lock is the guarantee behind it for the data that
/// assumes one process (the sync plan store, the transfer queue), for the
/// cases the plugin cannot see, such as a second user session.
struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    fn acquire(data_dir: &Path) -> Result<Option<InstanceLock>> {
        let path = data_dir.join(INSTANCE_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| Error::io(format!("opening {}", path.display()), err))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(InstanceLock { _file: file })),
            Err(TryLockError::WouldBlock) => Err(Error::Conflict(format!(
                "BucketBridge is already running with the data folder {}; close the other window first",
                data_dir.display()
            ))),
            // A file system without locks (some network shares) cannot
            // enforce this; the single-instance plugin still applies.
            Err(TryLockError::Error(err)) if err.kind() == io::ErrorKind::Unsupported => {
                eprintln!(
                    "BucketBridge: {} cannot be locked on this file system: {err}",
                    path.display()
                );
                Ok(None)
            }
            Err(TryLockError::Error(err)) => {
                Err(Error::io(format!("locking {}", path.display()), err))
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How often old log files are removed (also once at start-up).
const LOG_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Removes old log files now and then every day, with the settings as they
/// are at that time. The thread holds the app core only weakly and ends with it.
fn spawn_log_cleanup(core: &Arc<Core>) {
    let core = Arc::downgrade(core);
    let spawned = std::thread::Builder::new()
        .name("bb-log-cleanup".to_owned())
        .spawn(move || {
            loop {
                let Some(core) = core.upgrade() else { return };
                if let Ok(config) = core.session.accounts().load() {
                    core.log.cleanup_old_files(&config.settings.logging);
                }
                drop(core);
                std::thread::sleep(LOG_CLEANUP_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "bucketbridge::app", "cannot start the log cleanup: {error}");
    }
}
