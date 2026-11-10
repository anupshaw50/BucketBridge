//! The NDJSON records printed with `--json`, one per item. Field names are
//! stable and documented in `docs/cli.md`. Listings of buckets and objects
//! print the IPC contract types (`Bucket`, `ObjectEntry`) unchanged.

use bucketbridge_core::BbError;
use bucketbridge_core::model::{Account, ObjectVersion, Side, SyncAction, SyncDirection};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// `bb ls --versions`: a version, a delete marker, or a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionRecord {
    /// Object key; folders end with `/`.
    pub key: String,
    /// Version ID; `null` for folders.
    pub version_id: Option<String>,
    /// The current version of the key.
    pub is_latest: bool,
    /// A delete marker rather than data.
    pub is_delete_marker: bool,
    /// Size in bytes (0 for delete markers and folders).
    pub size: u64,
    /// Entity tag without quotes.
    pub etag: Option<String>,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Storage class.
    pub storage_class: Option<String>,
    /// A common prefix (folder) of a non-recursive listing.
    pub is_prefix: bool,
}

impl VersionRecord {
    /// A folder row.
    pub fn prefix(key: &str) -> Self {
        VersionRecord {
            key: key.to_owned(),
            version_id: None,
            is_latest: false,
            is_delete_marker: false,
            size: 0,
            etag: None,
            last_modified: None,
            storage_class: None,
            is_prefix: true,
        }
    }
}

impl From<&ObjectVersion> for VersionRecord {
    fn from(version: &ObjectVersion) -> Self {
        VersionRecord {
            key: version.key.clone(),
            version_id: Some(version.version_id.clone()),
            is_latest: version.is_latest,
            is_delete_marker: version.is_delete_marker,
            size: version.size,
            etag: version.etag.clone(),
            last_modified: version.last_modified,
            storage_class: version.storage_class.clone(),
            is_prefix: false,
        }
    }
}

/// `bb mb` and `bb rb`: the bucket that was created or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BucketRecord {
    /// Bucket name.
    pub bucket: String,
    /// Region requested with `--region` (`null`: the default).
    pub region: Option<String>,
    /// Object Lock was turned on with `--object-lock` (left out otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_lock: Option<bool>,
    /// The Object Ownership requested with `--ownership` (left out otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
    /// The public access block requested with `--public-access-block`: `true`
    /// for all four switches on (left out otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_access_block: Option<bool>,
    /// What could not be applied although the bucket exists (left out when
    /// nothing).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl BucketRecord {
    /// A record with only the bucket and the region.
    pub fn new(bucket: &str, region: Option<String>) -> Self {
        BucketRecord {
            bucket: bucket.to_owned(),
            region,
            object_lock: None,
            ownership: None,
            public_access_block: None,
            warnings: Vec::new(),
        }
    }
}

/// `bb clone-bucket`: what became of one setting (or, with `--dry-run`, what
/// the source has for it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloneSettingRecord {
    /// The new bucket.
    pub bucket: String,
    /// The setting: `policy`, `cors`, `lifecycle`, `versioning`, `tags`,
    /// `encryption`, `website` or `public_access_block`.
    pub setting: String,
    /// `applied`, `skipped` (the source has none) or `failed`; with
    /// `--dry-run` `configured`, `not_configured`, `unsupported` or `failed`.
    pub state: String,
    /// Items the source setting holds (a dry run).
    pub count: Option<u32>,
    /// A short value of the source setting (a dry run).
    pub detail: Option<String>,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Why it failed.
    pub error: Option<BbError>,
}

/// `bb rm`: one object or version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeleteRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// The version deleted with `--versions` or `--version-id`; `null`
    /// otherwise.
    pub version_id: Option<String>,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the object or version was deleted.
    pub deleted: bool,
    /// Why it could not be deleted.
    pub error: Option<BbError>,
}

/// `bb restore-version`: the version that was copied over its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// The version that was restored.
    pub restored_version_id: String,
    /// The new current version, when the server reports one.
    pub version_id: Option<String>,
    /// Entity tag of the new version, without quotes.
    pub etag: Option<String>,
    /// Size in bytes.
    pub size: u64,
}

/// `bb cp` and `bb mv`: one file or object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopyRecord {
    /// Source: a local path or an `s3://` URI.
    pub source: String,
    /// Destination: a local path or an `s3://` URI.
    pub destination: String,
    /// Size in bytes, when known.
    pub size: Option<u64>,
    /// Entity tag of the new object, without quotes.
    pub etag: Option<String>,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the copy (upload, download or server-side copy) was made.
    pub copied: bool,
    /// Whether the source was deleted (`bb mv` only).
    pub source_deleted: bool,
    /// Why nothing was transferred: the destination exists and the overwrite
    /// policy keeps it. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Why the copy or the source deletion failed.
    pub error: Option<BbError>,
}

/// `bb presign`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresignRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// `get` or `put`.
    pub method: &'static str,
    /// Lifetime in seconds.
    pub expires_secs: u64,
    /// When the URL stops working.
    pub expires_at: DateTime<Utc>,
    /// The presigned URL.
    pub url: String,
    /// The short link, with `--shorten`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_url: Option<String>,
}

/// `bb mpu list`: one uncompleted multipart upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UploadRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// Upload ID.
    pub upload_id: String,
    /// When the upload was started.
    pub initiated: Option<DateTime<Utc>>,
    /// Storage class of the future object.
    pub storage_class: Option<String>,
    /// Who started the upload.
    pub initiator: Option<String>,
}

/// `bb account list` and `bb account add`: a stored account (the secrets
/// themselves are never printed, only the references to them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountRecord {
    /// The account as in `docs/desktop-api.md`.
    #[serde(flatten)]
    pub account: Account,
    /// Whether it is the default account.
    pub is_default: bool,
    /// The folder the account is in, if any.
    pub folder: Option<String>,
    /// A static-key account without a secret (imported from an account
    /// file): enter its secret access key before using it.
    pub needs_secret: bool,
}

/// `bb account move`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountMoveRecord {
    /// Account ID.
    pub id: String,
    /// Account label.
    pub label: String,
    /// The folder the account is in now, if any.
    pub folder: Option<String>,
}

/// `bb account folder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderRecord {
    /// Folder name.
    pub name: String,
    /// IDs of the accounts in the folder, in order.
    pub account_ids: Vec<String>,
    /// Their labels.
    pub accounts: Vec<String>,
}

/// `bb account export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportRecord {
    /// The file written.
    pub file: String,
    /// Accounts in it.
    pub accounts: usize,
    /// Folders in it.
    pub folders: usize,
}

/// `bb account import`: one account of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportRecord {
    /// `imported`, `duplicate` or `skipped`.
    pub kind: &'static str,
    /// Label in the file (the new label for an imported account).
    pub label: String,
    /// ID of the new account (`imported`).
    pub id: Option<String>,
    /// The label in the file when it was renamed (`imported`).
    pub renamed_from: Option<String>,
    /// The account has static keys and no secret yet (`imported`).
    pub needs_secret: Option<bool>,
    /// Label of the existing account it duplicates (`duplicate`).
    pub existing: Option<String>,
    /// `label` or `same_account` (`duplicate`), or why it was left out (`skipped`).
    pub reason: Option<String>,
}

/// `bb account import`: the last record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSummaryRecord {
    /// Accounts added.
    pub imported: usize,
    /// Duplicates left out.
    pub duplicates: usize,
    /// Invalid accounts left out.
    pub skipped: usize,
    /// Folders created.
    pub folders_created: Vec<String>,
}

/// `bb account remove` and `bb account default`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountChangeRecord {
    /// Account ID.
    pub id: String,
    /// Account label.
    pub label: String,
}

/// `bb mpu abort`: one upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AbortRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// Upload ID.
    pub upload_id: String,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the upload was aborted.
    pub aborted: bool,
    /// Why it could not be aborted.
    pub error: Option<BbError>,
}

/// `bb mpu resume`: the check of the file and, unless `--dry-run`, the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeRecord {
    /// Bucket name.
    pub bucket: String,
    /// Object key.
    pub key: String,
    /// Upload ID.
    pub upload_id: String,
    /// Size of the file.
    pub file_size: u64,
    /// Size of every part but the last.
    pub part_size: u64,
    /// Parts of the whole file.
    pub part_count: u32,
    /// Parts that were already on the server.
    pub parts_uploaded: u32,
    /// Bytes that still had to be uploaded.
    pub bytes_remaining: u64,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the upload was completed.
    pub completed: bool,
    /// The ETag of the finished object, without quotes.
    pub etag: Option<String>,
}

/// `bb sync`: one record per row that needs attention, then one summary.
/// The `type` field tells them apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncRecord {
    /// A file the plan copies or deletes, or cannot sync.
    Row(SyncRowRecord),
    /// How the whole run ended.
    Summary(SyncSummaryRecord),
    /// The report of `--summary`: counts and bytes per action, the duration and the
    /// failures. The last record.
    Report(bucketbridge_core::sync::SyncReport),
}

/// `bb sync`: one row of the plan, printed when it finished (or, with
/// `--dry-run`, when it is planned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRowRecord {
    /// The stored plan that runs the row; `null` for a dry run and when
    /// nothing was left to execute.
    pub plan_id: Option<String>,
    /// Path below the local folder and the S3 prefix, with `/` separators.
    pub path: String,
    /// `copy`, `delete` or `failed` (a file the analysis could not plan).
    pub action: SyncAction,
    /// The side a copy writes to or a delete removes from; `null` for
    /// `failed`.
    pub target: Option<Side>,
    /// The local file; `null` for `failed`.
    pub local: Option<String>,
    /// The `s3://` URI of the object (the destination of an S3 to S3 copy); `null` for
    /// `failed`.
    pub remote: Option<String>,
    /// The source object of an S3 to S3 copy; left out for every other row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Size in bytes of the file that is copied or deleted, when known.
    pub size: Option<u64>,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the copy or deletion was made.
    pub done: bool,
    /// Why the row failed.
    pub error: Option<String>,
}

/// `bb sync`: the last record. Every row of the plan is counted in exactly
/// one of `equal`, `copied`, `deleted`, `failed` and `pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncSummaryRecord {
    /// The plan left in the store, which `--resume` continues; `null` when
    /// none is left (a dry run, nothing to do, or every row done).
    pub plan_id: Option<String>,
    /// `upload`, `download` or `bidirectional`.
    pub direction: SyncDirection,
    /// Whether `--delete` was given.
    pub delete: bool,
    /// The local folder (absolute), or the source prefix of an S3 to S3 sync.
    pub local: String,
    /// The S3 folder, `s3://bucket/prefix/`.
    pub remote: String,
    /// Whether this was a `--dry-run`.
    pub dry_run: bool,
    /// Whether the run was interrupted before it finished.
    pub cancelled: bool,
    /// Rows where both sides already agree.
    pub equal: usize,
    /// Rows copied.
    pub copied: usize,
    /// Rows deleted.
    pub deleted: usize,
    /// Rows that could not be synced, or failed while running.
    pub failed: usize,
    /// Rows still to do: not attempted (interrupted), or with `--dry-run`
    /// the rows a real run would execute.
    pub pending: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::ErrorKind;
    use serde_json::json;

    #[test]
    fn field_names_are_stable() {
        let record = DeleteRecord {
            bucket: "b".into(),
            key: "k".into(),
            version_id: None,
            dry_run: false,
            deleted: false,
            error: Some(BbError {
                code: ErrorKind::Auth,
                message: "denied".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({
                "bucket": "b",
                "key": "k",
                "version_id": null,
                "dry_run": false,
                "deleted": false,
                "error": { "code": "auth", "message": "denied" }
            })
        );
        let copy = CopyRecord {
            source: "s3://a/x".into(),
            destination: "s3://b/y".into(),
            size: Some(3),
            etag: None,
            dry_run: true,
            copied: false,
            source_deleted: false,
            skipped: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&copy).unwrap(),
            json!({
                "source": "s3://a/x",
                "destination": "s3://b/y",
                "size": 3,
                "etag": null,
                "dry_run": true,
                "copied": false,
                "source_deleted": false,
                "error": null
            })
        );
        assert_eq!(
            serde_json::to_value(VersionRecord::prefix("dir/")).unwrap(),
            json!({
                "key": "dir/",
                "version_id": null,
                "is_latest": false,
                "is_delete_marker": false,
                "size": 0,
                "etag": null,
                "last_modified": null,
                "storage_class": null,
                "is_prefix": true
            })
        );
    }
}

/// `bb account import-aws`: a profile that was not imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSkippedRecord {
    /// Profile name.
    pub profile: String,
    /// Why nothing was created.
    pub reason: String,
    /// Always `true`; tells these records from the imported accounts.
    pub skipped: bool,
}

/// `bb account sso-login`: what the user must do (printed first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SsoPromptRecord {
    /// The code to enter at the verification page.
    pub user_code: String,
    /// The page to open.
    pub verification_uri: String,
    /// The page with the code filled in, when offered.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires.
    pub expires_in_secs: u64,
}

/// `bb account sso-login`: the sign-in is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SsoSignedInRecord {
    /// Start URL of the portal.
    pub start_url: String,
    /// Region of IAM Identity Center.
    pub sso_region: String,
    /// When the access token expires (RFC 3339).
    pub expires_at: String,
}
