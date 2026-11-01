//! Transfer queue tasks.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a transfer task does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    /// Local file or folder to S3.
    Upload,
    /// S3 object or prefix to a local folder.
    Download,
    /// Server-side copy.
    Copy,
    /// Server-side copy followed by deleting the source.
    Move,
    /// Delete an object or a prefix.
    Delete,
}

/// Lifecycle state of a transfer task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Waiting for a free slot.
    #[default]
    Queued,
    /// In progress.
    Running,
    /// Paused by the user or by an app restart.
    Stopped,
    /// Finished successfully.
    Completed,
    /// Finished with an error; can be retried.
    Failed,
    /// Cancelled and cleaned up.
    Cancelled,
}

/// Source or destination of a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Location {
    /// A local file or folder.
    Local {
        /// Local path.
        path: PathBuf,
    },
    /// An S3 object or prefix (prefixes end with `/`).
    S3 {
        /// Bucket name.
        bucket: String,
        /// Key or prefix.
        key: String,
    },
}

/// A task in the transfer queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferTask {
    /// UUID v4.
    pub id: String,
    /// Account the task runs with.
    pub account_id: String,
    /// Shared by tasks created from one folder operation.
    pub group_id: Option<String>,
    /// What the task does.
    pub kind: TransferKind,
    /// Source.
    pub src: Location,
    /// Destination; `None` for deletes.
    pub dst: Option<Location>,
    /// Total bytes (objects for delete tasks), when known.
    pub bytes_total: Option<u64>,
    /// Bytes (objects for delete tasks) done so far.
    pub bytes_done: u64,
    /// Smoothed speed in bytes per second; 0 when idle.
    pub speed: u64,
    /// Estimated seconds remaining.
    pub eta: Option<u64>,
    /// Current state.
    pub state: TransferState,
    /// Last error message.
    pub error: Option<String>,
    /// Attempts made so far.
    pub attempts: u32,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn enum_spellings() {
        let kinds = [
            (TransferKind::Upload, "upload"),
            (TransferKind::Download, "download"),
            (TransferKind::Copy, "copy"),
            (TransferKind::Move, "move"),
            (TransferKind::Delete, "delete"),
        ];
        for (kind, text) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(text));
        }
        let states = [
            (TransferState::Queued, "queued"),
            (TransferState::Running, "running"),
            (TransferState::Stopped, "stopped"),
            (TransferState::Completed, "completed"),
            (TransferState::Failed, "failed"),
            (TransferState::Cancelled, "cancelled"),
        ];
        for (state, text) in states {
            assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
            assert_eq!(
                serde_json::from_value::<TransferState>(json!(text)).unwrap(),
                state
            );
        }
    }

    #[test]
    fn task_shape_matches_contract() {
        let at = Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap();
        let task = TransferTask {
            id: "t1".into(),
            account_id: "a1".into(),
            group_id: None,
            kind: TransferKind::Download,
            src: Location::S3 {
                bucket: "b".into(),
                key: "photos/a.jpg".into(),
            },
            dst: Some(Location::Local {
                path: PathBuf::from("/tmp/x/a.jpg"),
            }),
            bytes_total: Some(10),
            bytes_done: 4,
            speed: 2,
            eta: Some(3),
            state: TransferState::Running,
            error: None,
            attempts: 1,
            created_at: at,
            updated_at: at,
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(
            value,
            json!({
                "id": "t1",
                "account_id": "a1",
                "group_id": null,
                "kind": "download",
                "src": { "type": "s3", "bucket": "b", "key": "photos/a.jpg" },
                "dst": { "type": "local", "path": "/tmp/x/a.jpg" },
                "bytes_total": 10,
                "bytes_done": 4,
                "speed": 2,
                "eta": 3,
                "state": "running",
                "error": null,
                "attempts": 1,
                "created_at": "2026-10-29T08:15:00Z",
                "updated_at": "2026-10-29T08:15:00Z"
            })
        );
        assert_eq!(serde_json::from_value::<TransferTask>(value).unwrap(), task);
    }

    #[test]
    fn delete_task_has_null_destination() {
        let value = json!({
            "id": "t2",
            "account_id": "a1",
            "group_id": "g",
            "kind": "delete",
            "src": { "type": "s3", "bucket": "b", "key": "logs/" },
            "dst": null,
            "bytes_total": null,
            "bytes_done": 0,
            "speed": 0,
            "eta": null,
            "state": "queued",
            "error": null,
            "attempts": 0,
            "created_at": "2026-10-29T08:15:00Z",
            "updated_at": "2026-10-29T08:15:00Z"
        });
        let task: TransferTask = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(task.dst, None);
        assert_eq!(serde_json::to_value(&task).unwrap(), value);
    }
}
