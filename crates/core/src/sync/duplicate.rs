//! "Skip duplicate content" (v0.2.0): rows whose files have the same size and the same
//! content are turned into `equal` rows, even when the modification times differ.
//!
//! The planner decides by size and modification time. A copy row whose two sides have the
//! same size is a candidate. The content is compared with what S3 stores, cheapest
//! first, and the local file is read (and hashed) only for candidates:
//!
//! 1. the checksums of the listing entry, when it has any,
//! 2. the ETag, when the object was uploaded in one request without server-side
//!    encryption by KMS (a 32 digit hexadecimal MD5), compared with the MD5 of the file,
//! 3. otherwise one `HEAD` request for the object's stored checksums (SHA-256, SHA-1,
//!    CRC-32C, CRC-32 or CRC-64/NVME of the whole object), compared with the same
//!    checksum of the file.
//!
//! Anything that cannot be decided (a multipart ETag without stored checksums, an
//! unreadable file, a failed request) leaves the row as planned: a duplicate is only
//! declared when the content is known to match. For `s3_to_s3` plans there is no local
//! file; [`same_content_s3`] compares the ETags instead.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use md5::{Digest, Md5};

use super::row_local_path;
use crate::engine::Checksummer;
use crate::engine::checksum::{
    is_full_object as is_full_object_checksum, value_of as checksum_value_of,
};
use crate::error::Result;
use crate::model::{Checksums, ObjectEntry, SyncAction, SyncDirection, SyncPlan, SyncRow};
use crate::ops::S3;
use crate::path::join_key;
use crate::settings::ChecksumAlgorithm;

/// Checksum algorithms tried, strongest first.
const ALGORITHMS: [ChecksumAlgorithm; 5] = [
    ChecksumAlgorithm::Sha256,
    ChecksumAlgorithm::Sha1,
    ChecksumAlgorithm::Crc32c,
    ChecksumAlgorithm::Crc32,
    ChecksumAlgorithm::Crc64nvme,
];

/// What a pass found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DuplicateStats {
    /// Copy rows whose sizes matched, so that the content was compared.
    pub checked: usize,
    /// Rows turned into `equal` rows.
    pub duplicates: usize,
}

/// Turns the copy rows of a local plan whose files have identical content into `equal`
/// rows. At most `parallelism` files are compared at the same time. Blocking file reads
/// run on the blocking pool. Plans of direction `s3_to_s3` are left alone (see
/// [`same_content_s3`]).
pub async fn skip_duplicates(
    s3: &S3,
    plan: &mut SyncPlan,
    parallelism: usize,
) -> Result<DuplicateStats> {
    if plan.direction == SyncDirection::S3ToS3 {
        return Ok(DuplicateStats::default());
    }
    let bucket = plan.s3_prefix.bucket().to_owned();
    let key_prefix = plan.s3_prefix.key().to_owned();
    let candidates: Vec<(usize, SyncRow)> = plan
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| is_candidate(row))
        .map(|(index, row)| (index, row.clone()))
        .collect();
    let checked = candidates.len();
    let local_root = Arc::new(plan.local_root.clone());
    let results: Vec<(usize, bool)> = futures::stream::iter(candidates)
        .map(|(index, row)| {
            let s3 = s3.clone();
            let bucket = bucket.clone();
            let key = join_key(&key_prefix, &row.rel_path);
            let local_root = Arc::clone(&local_root);
            async move {
                (
                    index,
                    is_duplicate(&s3, &local_root, &bucket, &key, &row).await,
                )
            }
        })
        .buffer_unordered(parallelism.max(1))
        .collect()
        .await;
    let mut duplicates = 0;
    for (index, duplicate) in results {
        if duplicate && let Some(row) = plan.rows.get_mut(index) {
            row.action = SyncAction::Equal;
            row.target = None;
            row.done = true;
            row.error = None;
            duplicates += 1;
        }
    }
    Ok(DuplicateStats {
        checked,
        duplicates,
    })
}

/// The `s3_to_s3` counterpart of [`skip_duplicates`]: for copy rows whose two objects have
/// the same size and are not already known to match (see [`same_content_s3`] on the
/// listing entries), asks for both objects' ETags and stored checksums with `HEAD`
/// requests and compares them. Undecidable rows stay copy rows.
pub async fn skip_duplicates_s3(
    s3: &S3,
    plan: &mut SyncPlan,
    parallelism: usize,
) -> Result<DuplicateStats> {
    let Some(source_prefix) = plan.source_prefix.clone() else {
        return Ok(DuplicateStats::default());
    };
    let destination = plan.s3_prefix.clone();
    let candidates: Vec<(usize, String)> = plan
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| is_candidate(row))
        .map(|(index, row)| (index, row.rel_path.clone()))
        .collect();
    let checked = candidates.len();
    let results: Vec<(usize, bool)> = futures::stream::iter(candidates)
        .map(|(index, rel)| {
            let s3 = s3.clone();
            let source_bucket = source_prefix.bucket().to_owned();
            let source_key = join_key(source_prefix.key(), &rel);
            let destination_bucket = destination.bucket().to_owned();
            let destination_key = join_key(destination.key(), &rel);
            async move {
                let (source, target) = futures::join!(
                    s3.head_object(&source_bucket, &source_key, None),
                    s3.head_object(&destination_bucket, &destination_key, None)
                );
                let same = match (source, target) {
                    (Ok(source), Ok(target)) => {
                        same_content_s3(&entry_of(source), &entry_of(target))
                    }
                    _ => false,
                };
                (index, same)
            }
        })
        .buffer_unordered(parallelism.max(1))
        .collect()
        .await;
    let mut duplicates = 0;
    for (index, duplicate) in results {
        if duplicate && let Some(row) = plan.rows.get_mut(index) {
            row.action = SyncAction::Equal;
            row.target = None;
            row.done = true;
            row.error = None;
            duplicates += 1;
        }
    }
    Ok(DuplicateStats {
        checked,
        duplicates,
    })
}

fn entry_of(head: crate::model::ObjectHead) -> ObjectEntry {
    ObjectEntry {
        key: head.key,
        size: head.size,
        etag: head.etag,
        last_modified: head.last_modified,
        storage_class: head.storage_class,
        checksums: head.checksums,
        is_prefix: false,
    }
}

/// A copy row with a file on both sides that have the same size.
fn is_candidate(row: &SyncRow) -> bool {
    row.action == SyncAction::Copy
        && !row.done
        && row.error.is_none()
        && !row.rel_path.ends_with('/')
        && matches!(
            (&row.local_meta, &row.s3_meta),
            (Some(local), Some(remote)) if local.size == remote.size
        )
}

async fn is_duplicate(s3: &S3, local_root: &Path, bucket: &str, key: &str, row: &SyncRow) -> bool {
    let (Some(entry), Ok(path)) = (&row.s3_meta, row_local_path(local_root, row)) else {
        return false;
    };
    // The checksums of the listing, then the ETag.
    let mut decided = compare_known(entry, &path).await;
    if decided.is_none() {
        // One request for the stored checksums.
        decided = match s3.head_object(bucket, key, None).await {
            Ok(head) if head.size == entry.size => {
                let checksums = head.checksums;
                compare_checksums(&checksums, &path).await
            }
            _ => None,
        };
    }
    decided.unwrap_or(false)
}

/// `Some(true)` when the file has the content of the entry, `Some(false)` when it
/// certainly has not, `None` when the entry alone cannot tell.
async fn compare_known(entry: &ObjectEntry, path: &Path) -> Option<bool> {
    if let Some(same) = compare_checksums(&entry.checksums, path).await {
        return Some(same);
    }
    let etag = entry.etag.as_deref()?.trim_matches('"');
    if !is_md5_etag(etag) {
        return None;
    }
    let expected = etag.to_ascii_lowercase();
    let path = path.to_owned();
    let actual = tokio::task::spawn_blocking(move || md5_hex(&path))
        .await
        .ok()?
        .ok()?;
    // A mismatch may also come from an ETag that is not an MD5 (server-side encryption
    // by KMS): it is no proof of a difference, so the stored checksums are asked for.
    (actual == expected).then_some(true)
}

/// A single-request ETag: 32 hexadecimal digits (multipart ETags end in `-N`).
pub fn is_md5_etag(etag: &str) -> bool {
    etag.len() == 32 && etag.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Compares the whole-object checksums of S3 with the same checksum of the file: `Some`
/// with the verdict for the strongest checksum S3 has, `None` when it has none (or only
/// the composite checksum of a multipart upload).
pub async fn compare_checksums(checksums: &Checksums, path: &Path) -> Option<bool> {
    let (algorithm, expected) = ALGORITHMS.iter().find_map(|&algorithm| {
        checksum_value_of(checksums, algorithm)
            .filter(|value| is_full_object_checksum(value))
            .map(|value| (algorithm, value.to_owned()))
    })?;
    let path = path.to_owned();
    let actual = tokio::task::spawn_blocking(move || file_checksum(&path, algorithm))
        .await
        .ok()?
        .ok()??;
    Some(actual == expected)
}

/// Whether two S3 objects of an `s3_to_s3` plan have the same content: equal sizes and
/// the same single-request ETag, or the same whole-object checksum.
pub fn same_content_s3(source: &ObjectEntry, destination: &ObjectEntry) -> bool {
    if source.size != destination.size {
        return false;
    }
    let same_etag = match (source.etag.as_deref(), destination.etag.as_deref()) {
        (Some(a), Some(b)) => {
            let (a, b) = (a.trim_matches('"'), b.trim_matches('"'));
            is_md5_etag(a) && is_md5_etag(b) && a.eq_ignore_ascii_case(b)
        }
        _ => false,
    };
    same_etag
        || ALGORITHMS.iter().any(|&algorithm| {
            match (
                checksum_value_of(&source.checksums, algorithm),
                checksum_value_of(&destination.checksums, algorithm),
            ) {
                (Some(a), Some(b)) => is_full_object_checksum(a) && a == b,
                _ => false,
            }
        })
}

fn md5_hex(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn file_checksum(path: &Path, algorithm: ChecksumAlgorithm) -> std::io::Result<Option<String>> {
    let Some(mut checksummer) = Checksummer::new(algorithm) else {
        return Ok(None);
    };
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        checksummer.update(&buffer[..read]);
    }
    Ok(Some(checksummer.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::checksum_of;
    use crate::model::FileMeta;

    fn entry(size: u64, etag: Option<&str>, checksums: Checksums) -> ObjectEntry {
        ObjectEntry {
            key: "k".to_owned(),
            size,
            etag: etag.map(str::to_owned),
            last_modified: None,
            storage_class: None,
            checksums,
            is_prefix: false,
        }
    }

    fn row(local: u64, remote: u64) -> SyncRow {
        SyncRow {
            rel_path: "a.txt".to_owned(),
            action: SyncAction::Copy,
            target: Some(crate::model::Side::Remote),
            local_meta: Some(FileMeta {
                size: local,
                modified: None,
            }),
            s3_meta: Some(entry(remote, None, Checksums::default())),
            error: None,
            done: false,
        }
    }

    #[test]
    fn candidates_are_copy_rows_with_equal_sizes() {
        assert!(is_candidate(&row(5, 5)));
        assert!(!is_candidate(&row(5, 6)));
        let mut done = row(5, 5);
        done.done = true;
        assert!(!is_candidate(&done));
        let mut equal = row(5, 5);
        equal.action = SyncAction::Equal;
        assert!(!is_candidate(&equal));
        let mut marker = row(0, 0);
        marker.rel_path = "dir/".to_owned();
        assert!(!is_candidate(&marker));
        let mut missing = row(5, 5);
        missing.s3_meta = None;
        assert!(!is_candidate(&missing));
    }

    #[test]
    fn etags_are_md5_only_for_single_requests() {
        assert!(is_md5_etag("5d41402abc4b2a76b9719d911017c592"));
        assert!(is_md5_etag("5D41402ABC4B2A76B9719D911017C592"));
        assert!(!is_md5_etag("5d41402abc4b2a76b9719d911017c592-3"));
        assert!(!is_md5_etag("short"));
        assert!(!is_md5_etag("zzzz402abc4b2a76b9719d911017c592"));
    }

    #[tokio::test]
    async fn local_files_are_compared_with_etags_and_stored_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello").unwrap();
        // MD5 of "hello".
        let md5 = "5d41402abc4b2a76b9719d911017c592";
        let same = entry(5, Some(&format!("\"{md5}\"")), Checksums::default());
        assert_eq!(compare_known(&same, &path).await, Some(true));
        // A different ETag proves nothing (it may belong to an encrypted object).
        let other = entry(
            5,
            Some("00000000000000000000000000000000"),
            Checksums::default(),
        );
        assert_eq!(compare_known(&other, &path).await, None);
        // A multipart ETag is not an MD5.
        let multipart = entry(
            5,
            Some("5d41402abc4b2a76b9719d911017c592-2"),
            Checksums::default(),
        );
        assert_eq!(compare_known(&multipart, &path).await, None);

        let sha = checksum_of(ChecksumAlgorithm::Sha256, b"hello").unwrap();
        let stored = Checksums {
            sha256: Some(sha),
            ..Checksums::default()
        };
        assert_eq!(compare_checksums(&stored, &path).await, Some(true));
        let different = Checksums {
            sha256: Some(checksum_of(ChecksumAlgorithm::Sha256, b"world").unwrap()),
            ..Checksums::default()
        };
        assert_eq!(compare_checksums(&different, &path).await, Some(false));
        // Another algorithm is used when it is the only one.
        let crc = Checksums {
            crc32c: Some(checksum_of(ChecksumAlgorithm::Crc32c, b"hello").unwrap()),
            ..Checksums::default()
        };
        assert_eq!(compare_checksums(&crc, &path).await, Some(true));
        // A composite checksum of a multipart object cannot be compared.
        let composite = Checksums {
            sha256: Some("abcd-3".to_owned()),
            ..Checksums::default()
        };
        assert_eq!(compare_checksums(&composite, &path).await, None);
        assert_eq!(compare_checksums(&Checksums::default(), &path).await, None);
    }

    #[test]
    fn s3_objects_match_by_single_request_etag_or_checksum() {
        let md5 = "5d41402abc4b2a76b9719d911017c592";
        let a = entry(5, Some(md5), Checksums::default());
        let b = entry(5, Some(&format!("\"{md5}\"")), Checksums::default());
        assert!(same_content_s3(&a, &b));
        assert!(!same_content_s3(
            &a,
            &entry(6, Some(md5), Checksums::default())
        ));
        let multipart = entry(
            5,
            Some("5d41402abc4b2a76b9719d911017c592-2"),
            Checksums::default(),
        );
        assert!(!same_content_s3(&multipart, &multipart.clone()));
        let sha = Checksums {
            sha1: Some(checksum_of(ChecksumAlgorithm::Sha1, b"x").unwrap()),
            ..Checksums::default()
        };
        assert!(same_content_s3(
            &entry(1, None, sha.clone()),
            &entry(1, Some("whatever-2"), sha)
        ));
        assert!(!same_content_s3(
            &entry(1, None, Checksums::default()),
            &entry(1, None, Checksums::default())
        ));
    }
}
