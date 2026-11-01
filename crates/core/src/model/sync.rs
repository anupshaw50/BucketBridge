//! Folder synchronisation plans.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ObjectEntry;
use crate::s3uri::S3Uri;

/// Which way a sync copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// Local folder to S3.
    Upload,
    /// S3 prefix to local folder.
    Download,
    /// Both ways.
    Bidirectional,
}

/// What a sync row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    /// Copy to the target side.
    Copy,
    /// Delete from the target side.
    Delete,
    /// Both sides already match.
    Equal,
    /// Analysis or execution failed for this row.
    Failed,
}

/// One side of a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The local folder.
    Local,
    /// The S3 prefix.
    Remote,
}

/// Size and modification time of a local file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileMeta {
    /// Size in bytes.
    pub size: u64,
    /// Modification time, when the filesystem reports one.
    pub modified: Option<DateTime<Utc>>,
}

/// One file of a sync plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRow {
    /// `/`-separated path relative to the local root and the S3 prefix.
    pub rel_path: String,
    /// What to do.
    pub action: SyncAction,
    /// Side that a copy writes to or a delete removes from.
    pub target: Option<Side>,
    /// Local file state, if the file exists locally.
    pub local_meta: Option<FileMeta>,
    /// Remote object state, if the object exists.
    pub s3_meta: Option<ObjectEntry>,
    /// Failure message for `Failed` rows.
    pub error: Option<String>,
    /// Whether the action has been applied (`Equal` rows start as done).
    pub done: bool,
}

/// The result of analysing a local folder against an S3 prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    /// UUID v4.
    pub id: String,
    /// Account the plan runs with.
    pub account_id: String,
    /// Local folder.
    pub local_root: PathBuf,
    /// S3 prefix, serialized as `"s3://bucket/prefix/"`.
    pub s3_prefix: S3Uri,
    /// Direction.
    pub direction: SyncDirection,
    /// Whether extra files on the target side are deleted.
    pub delete: bool,
    /// When the plan was created.
    pub created_at: DateTime<Utc>,
    /// One row per file.
    pub rows: Vec<SyncRow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn enum_spellings() {
        assert_eq!(
            serde_json::to_value(SyncDirection::Upload).unwrap(),
            json!("upload")
        );
        assert_eq!(
            serde_json::to_value(SyncDirection::Download).unwrap(),
            json!("download")
        );
        assert_eq!(
            serde_json::to_value(SyncDirection::Bidirectional).unwrap(),
            json!("bidirectional")
        );
        assert_eq!(
            serde_json::to_value(SyncAction::Copy).unwrap(),
            json!("copy")
        );
        assert_eq!(
            serde_json::to_value(SyncAction::Delete).unwrap(),
            json!("delete")
        );
        assert_eq!(
            serde_json::to_value(SyncAction::Equal).unwrap(),
            json!("equal")
        );
        assert_eq!(
            serde_json::to_value(SyncAction::Failed).unwrap(),
            json!("failed")
        );
        assert_eq!(serde_json::to_value(Side::Local).unwrap(), json!("local"));
        assert_eq!(serde_json::to_value(Side::Remote).unwrap(), json!("remote"));
    }

    #[test]
    fn plan_shape_matches_contract() {
        let at = Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap();
        let plan = SyncPlan {
            id: "p1".into(),
            account_id: "a1".into(),
            local_root: PathBuf::from("/home/user/photos"),
            s3_prefix: S3Uri::parse("s3://bucket/photos/").unwrap(),
            direction: SyncDirection::Upload,
            delete: false,
            created_at: at,
            rows: vec![SyncRow {
                rel_path: "2024/a.jpg".into(),
                action: SyncAction::Copy,
                target: Some(Side::Remote),
                local_meta: Some(FileMeta {
                    size: 3,
                    modified: Some(at),
                }),
                s3_meta: None,
                error: None,
                done: false,
            }],
        };
        let value = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            value,
            json!({
                "id": "p1",
                "account_id": "a1",
                "local_root": "/home/user/photos",
                "s3_prefix": "s3://bucket/photos/",
                "direction": "upload",
                "delete": false,
                "created_at": "2026-10-29T08:15:00Z",
                "rows": [{
                    "rel_path": "2024/a.jpg",
                    "action": "copy",
                    "target": "remote",
                    "local_meta": { "size": 3, "modified": "2026-10-29T08:15:00Z" },
                    "s3_meta": null,
                    "error": null,
                    "done": false
                }]
            })
        );
        assert_eq!(serde_json::from_value::<SyncPlan>(value).unwrap(), plan);
    }

    #[test]
    fn invalid_s3_prefix_is_rejected_on_input() {
        let value = json!({
            "id": "p1",
            "account_id": "a1",
            "local_root": "/x",
            "s3_prefix": "bucket/photos/",
            "direction": "download",
            "delete": true,
            "created_at": "2026-10-29T08:15:00Z",
            "rows": []
        });
        let err = serde_json::from_value::<SyncPlan>(value).unwrap_err();
        assert!(err.to_string().contains("s3://"), "{err}");
    }
}
