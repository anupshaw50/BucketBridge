//! Object metadata (plan task T06): tags, storage class, server-side
//! encryption, HTTP headers, user metadata and canned ACLs.
//!
//! Everything is a method of [`S3`](crate::S3), implemented in the files of
//! this module:
//!
//! | Method | What it does |
//! |---|---|
//! | [`S3::object_details`](crate::S3::object_details) | `HeadObject`, `GetObjectTagging` and `GetObjectAcl` in one [`ObjectDetails`]; a failing tag or ACL request does not hide the rest |
//! | [`S3::get_object_tags`](crate::S3::get_object_tags), [`put_object_tags`](crate::S3::put_object_tags), [`delete_object_tags`](crate::S3::delete_object_tags) | one object's tags |
//! | [`S3::update_object_tags`](crate::S3::update_object_tags) | a [`TagEdit`] (replace, or add and remove) on many objects |
//! | [`S3::set_object_storage_class`](crate::S3::set_object_storage_class) | a [`StorageClass`] on many objects |
//! | [`S3::set_object_encryption`](crate::S3::set_object_encryption) | an [`EncryptionChange`] (SSE-S3, SSE-KMS, SSE-C) on many objects |
//! | [`S3::update_object_headers`](crate::S3::update_object_headers) | [`HeaderChanges`] (HTTP headers, user metadata) on many objects |
//! | [`S3::get_object_acl`](crate::S3::get_object_acl), [`put_object_acl`](crate::S3::put_object_acl), [`set_object_acl`](crate::S3::set_object_acl) | canned ACLs |
//!
//! # Batches
//!
//! The methods that take `keys` run up to [`BatchOptions::concurrency`]
//! objects at the same time and return one [`ObjectChange`] per distinct key,
//! in the order of `keys`. Invalid input (a tag that S3 would reject, an
//! unknown storage class) fails the whole call before anything is sent. A
//! problem with one object (it does not exist, it is archived, access is
//! denied) is that object's [`ObjectChange::error`] and never stops the
//! others. An object that already is as wanted is reported unchanged and
//! nothing is sent for it. [`BatchOptions::dry_run`] reads but writes
//! nothing and reports what would change.
//!
//! # Changes that copy the object
//!
//! Storage class, encryption and headers can only be changed by copying the
//! object onto itself; `inplace.rs` explains what the copy keeps. In short:
//! content headers, user metadata, tags, the other storage and encryption
//! settings and the ACL stay as they are, the object gets a new modification
//! time (and a new version on a versioned bucket), and an archived object
//! that is not restored is a `conflict` (its data cannot be read). Objects
//! larger than 5 GiB are copied in parts (see [`S3::copy_object_with`](crate::S3::copy_object_with)).
//!
//! # SSE-C
//!
//! A customer-provided key ([`CustomerKey`]) is passed with each call that
//! needs it and is never stored or logged. Reading or copying an object that
//! is encrypted with SSE-C needs its key; a wrong key makes S3 refuse the
//! request (HTTP 403) and the error says so, instead of anything being
//! silently skipped. To change the encryption of an SSE-C object, pass the key it
//! has now as `sse_c` and the new encryption as the target.

use std::collections::HashSet;
use std::future::Future;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{BbError, Error, Result};
use crate::s3uri::validate_key;

mod acl;
mod headers;
mod inplace;
mod read;
mod sse;
mod storage;
mod tags;

pub use acl::{
    ALL_USERS_URI, AUTHENTICATED_USERS_URI, AclGrant, AclPermission, BucketAcl, CannedAcl,
    GranteeKind, LOG_DELIVERY_URI, ObjectAcl, validate_grants,
};
pub use headers::{HeaderChange, HeaderChanges, MAX_HEADER_VALUE_BYTES, MAX_USER_META_BYTES};
pub use read::ObjectDetails;
pub use sse::{CUSTOMER_KEY_LEN, CustomerKey, EncryptionChange};
pub use storage::{RestoreStatus, StorageClass};
pub use tags::{
    MAX_TAG_KEY_CHARS, MAX_TAG_VALUE_CHARS, MAX_TAGS, TagEdit, validate_tag, validate_tags,
};

pub(crate) use headers::HeaderSet;
pub(crate) use inplace::CopyChanges;
pub(crate) use sse::SSE_C_ALGORITHM;
pub(crate) use storage::{archive_kind, class_name, parse_restore, unrestored_archive};

/// Objects edited at the same time by default.
pub const DEFAULT_BATCH_CONCURRENCY: usize = 8;

/// Most objects one batch call accepts.
pub const MAX_BATCH_KEYS: usize = 10_000;

/// Most objects edited at the same time.
pub const MAX_BATCH_CONCURRENCY: usize = 64;

/// How a batch call runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchOptions {
    /// Objects edited at the same time, 1 to [`MAX_BATCH_CONCURRENCY`].
    pub concurrency: usize,
    /// Read what would change, but write nothing.
    pub dry_run: bool,
}

impl Default for BatchOptions {
    fn default() -> Self {
        BatchOptions {
            concurrency: DEFAULT_BATCH_CONCURRENCY,
            dry_run: false,
        }
    }
}

impl BatchOptions {
    /// The default options with `dry_run` on.
    pub fn dry_run() -> Self {
        BatchOptions {
            dry_run: true,
            ..BatchOptions::default()
        }
    }
}

/// The outcome for one object of a batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectChange {
    /// The object key.
    pub key: String,
    /// The object was changed (with `dry_run`: would be). `false` when it
    /// already was as wanted, and when it failed.
    pub changed: bool,
    /// What the object had before, as short text: the storage class, the
    /// encryption (`none`, `s3`, `kms`, `c`) or the ACL (a canned name or
    /// `custom`). `None` for tags and headers, and when it is not known.
    pub previous: Option<String>,
    /// Why the object could not be changed.
    pub error: Option<BbError>,
    /// Things that went wrong after the object was changed, such as an ACL
    /// that could not be applied again.
    pub warnings: Vec<String>,
}

impl ObjectChange {
    pub(crate) fn changed(key: &str, previous: Option<String>) -> Self {
        ObjectChange {
            key: key.to_owned(),
            changed: true,
            previous,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub(crate) fn unchanged(key: &str, previous: Option<String>) -> Self {
        ObjectChange {
            changed: false,
            ..ObjectChange::changed(key, previous)
        }
    }

    pub(crate) fn failed(key: &str, error: &Error) -> Self {
        ObjectChange {
            error: Some(error.to_payload()),
            ..ObjectChange::unchanged(key, None)
        }
    }
}

/// A key that names an object: valid, and not a folder.
pub(crate) fn validate_object_key(key: &str) -> Result<()> {
    validate_key(key)?;
    if key.ends_with('/') {
        return Err(Error::Usage(format!(
            "'{}' is a folder; select the objects in it instead",
            key.escape_debug()
        )));
    }
    Ok(())
}

/// Runs `operation` for each distinct key, `options.concurrency` at a time,
/// and collects one result per key in the order of `keys`. The operation gets
/// an owned key so that its future borrows nothing from `keys`.
pub(crate) async fn run_batch<F, Fut>(
    keys: &[String],
    options: BatchOptions,
    operation: F,
) -> Result<Vec<ObjectChange>>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<ObjectChange>>,
{
    if keys.is_empty() {
        return Err(Error::Usage("select at least one object".into()));
    }
    if keys.len() > MAX_BATCH_KEYS {
        return Err(Error::Usage(format!(
            "{} objects are too many for one request; at most {MAX_BATCH_KEYS} are accepted",
            keys.len()
        )));
    }
    if !(1..=MAX_BATCH_CONCURRENCY).contains(&options.concurrency) {
        return Err(Error::Usage(format!(
            "the concurrency must be between 1 and {MAX_BATCH_CONCURRENCY}"
        )));
    }
    let mut seen = HashSet::new();
    let distinct: Vec<String> = keys
        .iter()
        .filter(|key| seen.insert(key.as_str()))
        .cloned()
        .collect();
    Ok(futures::stream::iter(distinct)
        .map(|key| {
            let work = operation(key.clone());
            async move {
                work.await
                    .unwrap_or_else(|error| ObjectChange::failed(&key, &error))
            }
        })
        .buffered(options.concurrency)
        .collect()
        .await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[tokio::test]
    async fn batches_report_each_distinct_key_in_order_and_keep_going_after_a_failure() {
        let names = keys(&["b", "a", "b", "fail", "c"]);
        let results = run_batch(&names, BatchOptions::default(), |key| async move {
            if key == "fail" {
                Err(Error::NotFound("gone".into()))
            } else if key == "c" {
                Ok(ObjectChange::unchanged(&key, Some("same".into())))
            } else {
                Ok(ObjectChange::changed(&key, None))
            }
        })
        .await
        .unwrap();
        let summary: Vec<(&str, bool, Option<ErrorKind>)> = results
            .iter()
            .map(|r| (r.key.as_str(), r.changed, r.error.as_ref().map(|e| e.code)))
            .collect();
        assert_eq!(
            summary,
            [
                ("b", true, None),
                ("a", true, None),
                ("fail", false, Some(ErrorKind::NotFound)),
                ("c", false, None),
            ]
        );
        assert_eq!(results[3].previous.as_deref(), Some("same"));
        assert_eq!(
            results[2].error.as_ref().map(|e| e.message.as_str()),
            Some("gone")
        );
    }

    #[tokio::test]
    async fn at_most_the_given_number_of_objects_run_at_once() {
        let names: Vec<String> = (0..20).map(|i| format!("k{i}")).collect();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let options = BatchOptions {
            concurrency: 3,
            dry_run: false,
        };
        let results = run_batch(&names, options, |key| {
            let (running, peak) = (running.clone(), peak.clone());
            async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(ObjectChange::changed(&key, None))
            }
        })
        .await
        .unwrap();
        assert_eq!(results.len(), 20);
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        let order: Vec<&str> = results.iter().map(|r| r.key.as_str()).collect();
        let expected: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(order, expected);
    }

    #[tokio::test]
    async fn bad_batches_are_usage_errors() {
        let operation = |key: String| async move { Ok(ObjectChange::changed(&key, None)) };
        let none: Vec<String> = Vec::new();
        let err = run_batch(&none, BatchOptions::default(), operation)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let many: Vec<String> = (0..=MAX_BATCH_KEYS).map(|i| i.to_string()).collect();
        assert!(
            run_batch(&many, BatchOptions::default(), operation)
                .await
                .is_err()
        );
        let one = keys(&["a"]);
        for concurrency in [0, MAX_BATCH_CONCURRENCY + 1] {
            let options = BatchOptions {
                concurrency,
                dry_run: false,
            };
            assert!(run_batch(&one, options, operation).await.is_err());
        }
        // The largest batch is fine.
        let largest: Vec<String> = (0..MAX_BATCH_KEYS).map(|i| i.to_string()).collect();
        let done = run_batch(&largest, BatchOptions::default(), operation)
            .await
            .unwrap();
        assert_eq!(done.len(), MAX_BATCH_KEYS);
    }

    #[test]
    fn folders_and_bad_keys_are_not_objects() {
        assert!(validate_object_key("a/b.txt").is_ok());
        for bad in ["", "dir/", "a/b/"] {
            assert_eq!(
                validate_object_key(bad).unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad:?}"
            );
        }
        assert!(validate_object_key(&"k".repeat(1025)).is_err());
    }

    #[test]
    fn results_serialize_with_stable_field_names() {
        let change = ObjectChange {
            key: "a".into(),
            changed: true,
            previous: Some("STANDARD".into()),
            error: None,
            warnings: vec!["w".into()],
        };
        assert_eq!(
            serde_json::to_value(&change).unwrap(),
            serde_json::json!({
                "key": "a",
                "changed": true,
                "previous": "STANDARD",
                "error": null,
                "warnings": ["w"]
            })
        );
        let failed = ObjectChange::failed("b", &Error::Conflict("archived".into()));
        assert!(!failed.changed);
        assert_eq!(
            serde_json::to_value(&failed).unwrap()["error"],
            serde_json::json!({ "code": "conflict", "message": "archived" })
        );
        assert_eq!(
            BatchOptions::dry_run().concurrency,
            DEFAULT_BATCH_CONCURRENCY
        );
        assert!(BatchOptions::dry_run().dry_run);
    }
}
