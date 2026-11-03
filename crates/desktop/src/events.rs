//! Backend events (docs/desktop-api.md, "Events") and how they reach the UI.

use std::sync::atomic::{AtomicBool, Ordering};

use bucketbridge_core::EventLog;
use bucketbridge_core::model::SyncRow;
use bucketbridge_core::sync::{SyncEvents, SyncProgress, SyncReport};
use bucketbridge_core::transfer::QueueEvent;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::state::Core;

/// `SecurityStatus` after an unlock, a lock or a master-password change.
pub const SECURITY_CHANGED: &str = "security://changed";
/// Each new event-log entry at or above the configured level.
pub const LOG_ENTRY: &str = "log://entry";
/// `SyncProgress` while a plan executes, and once with `finished: true`.
pub const SYNC_PROGRESS: &str = "sync://progress";
/// `{ plan_id, index, row }` when a row finished or failed.
pub const SYNC_ROW: &str = "sync://row";
/// `SyncReport` once a plan's run has ended (after `sync://progress` with `finished: true`).
pub const SYNC_REPORT: &str = "sync://report";
/// The full `SyncJob[]` list after a sync job was saved or deleted.
pub const SYNC_JOBS_CHANGED: &str = "sync_jobs://changed";
/// `TransferTask` when a task is added, changes state, or reports progress
/// (at most every 250 ms per running task).
pub const TRANSFER_UPDATE: &str = "transfer://update";
/// `{ ids }` when `clear_finished` removed tasks.
pub const TRANSFER_REMOVED: &str = "transfer://removed";
/// `{ type: "asked", task_id, group_id, conflict }` when a task waits for an
/// overwrite decision, `{ type: "resolved", task_id }` when it no longer does.
pub const TRANSFER_CONFLICT: &str = "transfer://conflict";

/// `EditSession` whenever an edit-in-place session changes state (watching,
/// uploading, uploaded, conflict, error) and once with `stopped` when it ends.
pub const EDIT_CHANGED: &str = "edit://changed";

/// The full `Bookmark[]` list after a bookmark was added, renamed or removed.
pub const BOOKMARKS_CHANGED: &str = "bookmarks://changed";

/// Source of the event-log entries about the transfer queue.
const TRANSFER_LOG_SOURCE: &str = "bucketbridge::transfer";

/// Starts forwarding the session's security changes as `security://changed`,
/// new event-log entries as `log://entry`, and the transfer queue's events as
/// `transfer://update` and `transfer://removed` (a failed background save of
/// the queue file goes to the event log as an error). The log only records
/// entries at or above `settings.logging.level`, so that is all that is
/// forwarded. The forwarding tasks end when the session, the log or the queue
/// is dropped.
pub fn forward_events<R: Runtime>(app: &AppHandle<R>, core: &Core) {
    forward(app.clone(), core.session.subscribe(), SECURITY_CHANGED);
    forward(app.clone(), core.log.subscribe(), LOG_ENTRY);
    forward(app.clone(), core.edits.subscribe(), EDIT_CHANGED);
    forward_queue(app.clone(), core.queue.subscribe(), core.log.clone());
    forward_conflicts(app.clone(), core.runner.conflicts().subscribe());
}

/// Forwards the overwrite questions of waiting tasks as
/// `transfer://conflict`.
fn forward_conflicts<R: Runtime>(
    app: AppHandle<R>,
    mut events: UnboundedReceiver<bucketbridge_core::engine::ConflictEvent>,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            let _ = app.emit(TRANSFER_CONFLICT, event);
        }
    });
}

fn forward<R, T>(app: AppHandle<R>, mut receiver: broadcast::Receiver<T>, event: &'static str)
where
    R: Runtime,
    T: Serialize + Clone + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                // An emit only fails while the app shuts down. It is not
                // logged: the log://entry forwarder would feed itself.
                Ok(payload) => {
                    let _ = app.emit(event, payload);
                }
                // A subscriber that fell behind continues with newer items;
                // the UI re-reads the full state when it needs it.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Payload of `transfer://removed`.
#[derive(Clone, Serialize)]
struct RemovedEvent {
    ids: Vec<String>,
}

/// Forwards the queue's events in the order they happened. The queue already
/// limits progress updates to one per interval and running task, and never
/// drops an event, so nothing is batched or skipped here.
fn forward_queue<R: Runtime>(
    app: AppHandle<R>,
    mut events: UnboundedReceiver<QueueEvent>,
    log: EventLog,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            // An emit only fails while the app shuts down.
            match event {
                QueueEvent::Updated(task) => {
                    let _ = app.emit(TRANSFER_UPDATE, task);
                }
                QueueEvent::Removed(ids) => {
                    let _ = app.emit(TRANSFER_REMOVED, RemovedEvent { ids });
                }
                QueueEvent::PersistFailed(message) => {
                    log.error(TRANSFER_LOG_SOURCE, &message);
                }
            }
        }
    });
}

/// Payload of `sync://row`.
#[derive(Clone, Serialize)]
struct SyncRowEvent<'a> {
    plan_id: &'a str,
    index: usize,
    row: &'a SyncRow,
}

/// Forwards one sync run's [`SyncEvents`] as `sync://progress` and
/// `sync://row`. Emitting never blocks, as the executor requires.
pub(crate) struct SyncEmitter<R: Runtime> {
    app: AppHandle<R>,
    finished: AtomicBool,
}

impl<R: Runtime> SyncEmitter<R> {
    pub(crate) fn new(app: AppHandle<R>) -> Self {
        SyncEmitter {
            app,
            finished: AtomicBool::new(false),
        }
    }

    /// Sends the summary report of the finished run.
    pub(crate) fn report(&self, report: &SyncReport) {
        let _ = self.app.emit(SYNC_REPORT, report);
    }

    /// Whether the run already reported `finished: true`.
    pub(crate) fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

impl<R: Runtime> SyncEvents for SyncEmitter<R> {
    fn progress(&self, progress: &SyncProgress) {
        if progress.finished {
            self.finished.store(true, Ordering::SeqCst);
        }
        let _ = self.app.emit(SYNC_PROGRESS, progress);
    }

    fn row(&self, plan_id: &str, index: usize, row: &SyncRow) {
        let _ = self.app.emit(
            SYNC_ROW,
            SyncRowEvent {
                plan_id,
                index,
                row,
            },
        );
    }
}
