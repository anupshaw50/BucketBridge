//! Objects, versions, object metadata and multipart uploads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Checksum values as S3 returns them (base64; composite multipart
/// checksums keep their `-N` suffix). Listings never carry values, only
/// `HeadObject` and `GetObject` do.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Checksums {
    /// CRC-32.
    pub crc32: Option<String>,
    /// CRC-32C (Castagnoli).
    pub crc32c: Option<String>,
    /// CRC-64/NVME.
    pub crc64nvme: Option<String>,
    /// SHA-1.
    pub sha1: Option<String>,
    /// SHA-256.
    pub sha256: Option<String>,
}

impl Checksums {
    /// True when no checksum value is present.
    pub fn is_empty(&self) -> bool {
        self.crc32.is_none()
            && self.crc32c.is_none()
            && self.crc64nvme.is_none()
            && self.sha1.is_none()
            && self.sha256.is_none()
    }
}

/// One row of an object listing: an object, or a common prefix ("folder").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectEntry {
    /// Full key; prefixes end with `/`.
    pub key: String,
    /// Size in bytes; 0 for prefixes.
    pub size: u64,
    /// Entity tag without the surrounding quotes.
    pub etag: Option<String>,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Storage class as reported by the server.
    pub storage_class: Option<String>,
    /// Checksum values (empty in listings).
    pub checksums: Checksums,
    /// True only for common prefixes synthesized by a delimiter listing.
    /// Zero-byte folder-marker objects (keys ending in `/`) are objects.
    pub is_prefix: bool,
}

impl ObjectEntry {
    /// A common-prefix entry.
    pub fn prefix(key: impl Into<String>) -> Self {
        ObjectEntry {
            key: key.into(),
            is_prefix: true,
            ..ObjectEntry::default()
        }
    }
}

/// One page of an object listing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectPage {
    /// Prefixes first, then objects, each in key order.
    pub entries: Vec<ObjectEntry>,
    /// Pass back to fetch the next page; `None` on the last page.
    pub next_token: Option<String>,
}

/// One version or delete marker of an object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectVersion {
    /// Object key.
    pub key: String,
    /// Version ID (`"null"` for objects written before versioning was enabled).
    pub version_id: String,
    /// True for the current version.
    pub is_latest: bool,
    /// True for a delete marker.
    pub is_delete_marker: bool,
    /// Size in bytes; 0 for delete markers.
    pub size: u64,
    /// Entity tag without quotes; `None` for delete markers.
    pub etag: Option<String>,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Storage class; `None` for delete markers.
    pub storage_class: Option<String>,
}

/// One page of a version listing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VersionPage {
    /// Versions and delete markers, by key, newest first within a key.
    pub entries: Vec<ObjectVersion>,
    /// Common prefixes (keys ending in `/`) of a delimiter listing.
    pub prefixes: Vec<String>,
    /// Opaque token for the next page; `None` on the last page.
    pub next_token: Option<String>,
}

/// Server-side encryption of an object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sse {
    /// Not encrypted (or not reported).
    #[default]
    None,
    /// SSE-S3 (keys managed by the server).
    S3,
    /// SSE-KMS.
    Kms {
        /// KMS key ID or ARN; `None` means the AWS managed key.
        key_id: Option<String>,
        /// Whether an S3 Bucket Key is used.
        bucket_key: bool,
    },
    /// SSE-C; the customer key itself is never stored.
    #[serde(rename = "c")]
    Customer {
        /// Base64 MD5 of the customer key, as reported by the server.
        key_md5: String,
    },
}

/// Object Lock retention mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockMode {
    /// Users with special permission may shorten or remove the retention.
    Governance,
    /// Nobody can shorten or remove the retention.
    Compliance,
}

/// Object Lock state of an object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectLock {
    /// Retention mode, if a retention period is set.
    pub mode: Option<LockMode>,
    /// End of the retention period.
    pub retain_until: Option<DateTime<Utc>>,
    /// Whether a legal hold is on.
    pub legal_hold: bool,
}

/// Editable metadata of an object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectMeta {
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// `Cache-Control`.
    pub cache_control: Option<String>,
    /// `Content-Disposition`.
    pub content_disposition: Option<String>,
    /// `Content-Encoding`.
    pub content_encoding: Option<String>,
    /// User metadata (`x-amz-meta-*`), keys without the prefix.
    pub user_meta: BTreeMap<String, String>,
    /// Object tags.
    pub tags: BTreeMap<String, String>,
    /// Canned ACL, when known.
    pub acl: Option<String>,
    /// Server-side encryption.
    pub sse: Sse,
    /// Object Lock state, when the object has any.
    pub object_lock: Option<ObjectLock>,
    /// `x-amz-website-redirect-location`: where a website endpoint redirects
    /// requests for the object.
    #[serde(default)]
    pub website_redirect: Option<String>,
}

/// Result of `HeadObject`.
///
/// `meta.tags` and `meta.acl` are not part of a `HeadObject` response and are
/// always empty here; they need `GetObjectTagging` and `GetObjectAcl`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ObjectHead {
    /// Object key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Entity tag without quotes.
    pub etag: Option<String>,
    /// Last modification time.
    pub last_modified: Option<DateTime<Utc>>,
    /// Storage class; S3 omits it for `STANDARD`.
    pub storage_class: Option<String>,
    /// Version ID, on versioned buckets.
    pub version_id: Option<String>,
    /// Checksum values (requested with checksum mode enabled).
    pub checksums: Checksums,
    /// Content headers, user metadata, encryption and lock state.
    pub meta: ObjectMeta,
}

/// A key that a batch delete could not remove.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeleteFailure {
    /// The key.
    pub key: String,
    /// The version, for versioned deletes.
    pub version_id: Option<String>,
    /// Service error code, such as `AccessDenied`.
    pub code: Option<String>,
    /// Service error message.
    pub message: String,
}

/// Outcome of a batch delete.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeleteOutcome {
    /// Number of keys the server reported as deleted (keys that did not
    /// exist count as deleted, as in S3).
    pub deleted: u64,
    /// Keys that could not be deleted.
    pub failures: Vec<DeleteFailure>,
}

/// An in-progress (uncompleted) multipart upload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MultipartUpload {
    /// Object key being uploaded.
    pub key: String,
    /// Upload ID.
    pub upload_id: String,
    /// When the upload was started.
    pub initiated: Option<DateTime<Utc>>,
    /// Storage class of the future object.
    pub storage_class: Option<String>,
    /// Display name or ID of the principal that started the upload.
    pub initiator: Option<String>,
}

/// Outcome of a server-side copy.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CopyOutcome {
    /// Bytes copied (the source size).
    pub size: u64,
    /// Entity tag of the new object, without quotes.
    pub etag: Option<String>,
    /// Number of parts for a multipart copy; 0 for a single-request copy.
    pub parts: u32,
    /// The key the copy was stored under when the destination existed and
    /// the overwrite policy renamed it (`name (1).ext`); `None` otherwise.
    #[serde(default)]
    pub renamed_to: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap()
    }

    #[test]
    fn object_entry_and_page_shape() {
        let page = ObjectPage {
            entries: vec![
                ObjectEntry::prefix("photos/2024/"),
                ObjectEntry {
                    key: "photos/a.jpg".into(),
                    size: 1234,
                    etag: Some("9b2cf535f27731c974343645a3985328".into()),
                    last_modified: Some(ts()),
                    storage_class: Some("STANDARD".into()),
                    checksums: Checksums {
                        crc32: Some("i9aeUg==".into()),
                        ..Checksums::default()
                    },
                    is_prefix: false,
                },
            ],
            next_token: Some("token".into()),
        };
        let value = serde_json::to_value(&page).unwrap();
        assert_eq!(
            value,
            json!({
                "entries": [
                    {
                        "key": "photos/2024/",
                        "size": 0,
                        "etag": null,
                        "last_modified": null,
                        "storage_class": null,
                        "checksums": { "crc32": null, "crc32c": null, "crc64nvme": null, "sha1": null, "sha256": null },
                        "is_prefix": true
                    },
                    {
                        "key": "photos/a.jpg",
                        "size": 1234,
                        "etag": "9b2cf535f27731c974343645a3985328",
                        "last_modified": "2026-10-29T08:15:00Z",
                        "storage_class": "STANDARD",
                        "checksums": { "crc32": "i9aeUg==", "crc32c": null, "crc64nvme": null, "sha1": null, "sha256": null },
                        "is_prefix": false
                    }
                ],
                "next_token": "token"
            })
        );
        assert_eq!(serde_json::from_value::<ObjectPage>(value).unwrap(), page);
    }

    #[test]
    fn object_version_shape() {
        let version = ObjectVersion {
            key: "a.txt".into(),
            version_id: "3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY".into(),
            is_latest: true,
            is_delete_marker: false,
            size: 5,
            etag: Some("e".into()),
            last_modified: Some(ts()),
            storage_class: Some("STANDARD".into()),
        };
        let value = serde_json::to_value(&version).unwrap();
        assert_eq!(
            value,
            json!({
                "key": "a.txt",
                "version_id": "3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY",
                "is_latest": true,
                "is_delete_marker": false,
                "size": 5,
                "etag": "e",
                "last_modified": "2026-10-29T08:15:00Z",
                "storage_class": "STANDARD"
            })
        );
        assert_eq!(
            serde_json::from_value::<ObjectVersion>(value).unwrap(),
            version
        );
    }

    #[test]
    fn sse_tagged_union_spellings() {
        assert_eq!(
            serde_json::to_value(Sse::None).unwrap(),
            json!({ "type": "none" })
        );
        assert_eq!(
            serde_json::to_value(Sse::S3).unwrap(),
            json!({ "type": "s3" })
        );
        assert_eq!(
            serde_json::to_value(Sse::Kms {
                key_id: None,
                bucket_key: true
            })
            .unwrap(),
            json!({ "type": "kms", "key_id": null, "bucket_key": true })
        );
        assert_eq!(
            serde_json::to_value(Sse::Customer {
                key_md5: "md5==".into()
            })
            .unwrap(),
            json!({ "type": "c", "key_md5": "md5==" })
        );
        let back: Sse =
            serde_json::from_value(json!({ "type": "kms", "key_id": "k", "bucket_key": false }))
                .unwrap();
        assert_eq!(
            back,
            Sse::Kms {
                key_id: Some("k".into()),
                bucket_key: false
            }
        );
        assert!(
            serde_json::from_value::<Sse>(json!({ "type": "customer", "key_md5": "x" })).is_err()
        );
    }

    #[test]
    fn object_meta_shape() {
        let meta = ObjectMeta {
            content_type: Some("image/jpeg".into()),
            cache_control: None,
            content_disposition: None,
            content_encoding: Some("gzip".into()),
            user_meta: BTreeMap::from([("owner".to_owned(), "team".to_owned())]),
            tags: BTreeMap::from([("env".to_owned(), "prod".to_owned())]),
            acl: Some("private".into()),
            sse: Sse::S3,
            object_lock: Some(ObjectLock {
                mode: Some(LockMode::Governance),
                retain_until: Some(ts()),
                legal_hold: true,
            }),
            website_redirect: Some("/new".into()),
        };
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            value,
            json!({
                "content_type": "image/jpeg",
                "cache_control": null,
                "content_disposition": null,
                "content_encoding": "gzip",
                "user_meta": { "owner": "team" },
                "tags": { "env": "prod" },
                "acl": "private",
                "sse": { "type": "s3" },
                "object_lock": { "mode": "governance", "retain_until": "2026-10-29T08:15:00Z", "legal_hold": true },
                "website_redirect": "/new"
            })
        );
        assert_eq!(serde_json::from_value::<ObjectMeta>(value).unwrap(), meta);
        assert_eq!(
            serde_json::to_value(LockMode::Compliance).unwrap(),
            json!("compliance")
        );
    }

    #[test]
    fn checksums_is_empty() {
        assert!(Checksums::default().is_empty());
        let sums = Checksums {
            sha256: Some("x".into()),
            ..Checksums::default()
        };
        assert!(!sums.is_empty());
    }

    #[test]
    fn fractional_timestamps_round_trip() {
        let with_millis = Utc.timestamp_millis_opt(1_793_000_000_123).unwrap();
        let entry = ObjectEntry {
            key: "k".into(),
            last_modified: Some(with_millis),
            ..ObjectEntry::default()
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["last_modified"], json!("2026-10-26T07:33:20.123Z"));
        assert_eq!(serde_json::from_value::<ObjectEntry>(value).unwrap(), entry);
    }
}
