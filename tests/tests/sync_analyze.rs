//! End-to-end test of the sync analysis (`bucketbridge_core::sync`) against a real S3
//! server (s3s-fs by default, MinIO with `--features minio` and `BB_TEST_BACKEND=minio`).
//!
//! The remote side is listed with the real transport (`S3::list_all`), so the planner
//! sees genuine `ObjectEntry` values: server sizes and timestamps, and ETags without
//! quotes where the server reports them (s3s-fs listings carry none).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use bucketbridge_core::model::{ObjectEntry, Side, SyncAction, SyncDirection, SyncPlan};
use bucketbridge_core::sync::{self, AnalyzeRequest, RowCounts, RowOutcome, RowUpdate};
use bucketbridge_core::{ErrorKind, S3Uri, SyncStore};
use bucketbridge_tests::TestS3;

use Side::{Local, Remote};
use SyncAction::{Copy, Delete, Equal, Failed};

const PREFIX: &str = "sync/";

/// Writes `contents` to `root/rel` and sets its modification time.
fn write_file(root: &Path, rel: &str, contents: &str, modified: SystemTime) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

fn summary(plan: &SyncPlan) -> Vec<(&str, SyncAction, Option<Side>)> {
    plan.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.action, row.target))
        .collect()
}

fn analyze(
    root: &Path,
    prefix: &S3Uri,
    direction: SyncDirection,
    delete: bool,
    listing: &[ObjectEntry],
) -> SyncPlan {
    let mut request = AnalyzeRequest::new("test-account", root, prefix, direction);
    request.delete = delete;
    sync::analyze(&request, listing.iter().cloned()).unwrap()
}

#[tokio::test]
async fn analysis_of_a_real_listing_in_every_direction() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "sync/same.txt", "same").await;
    s3.put(&bucket, "sync/changed.txt", "remote version").await;
    s3.put(&bucket, "sync/remote-only.txt", "r").await;
    s3.put(&bucket, "sync/nested/deep.txt", "deep").await;
    s3.put(&bucket, "sync/stale.bbpart", "partial download")
        .await;
    s3.put(&bucket, "outside/other.txt", "not part of the sync")
        .await;

    let listing = s3.ops().list_all(&bucket, PREFIX).await.unwrap();
    let entry = |key: &str| {
        listing
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("{key} is not listed: {listing:?}"))
    };
    assert_eq!(listing.len(), 5, "{listing:?}");
    assert_eq!(entry("sync/changed.txt").size, 14);
    for entry in &listing {
        assert!(!entry.is_prefix);
        assert!(
            entry.etag.as_deref().is_none_or(|etag| !etag.contains('"')),
            "{entry:?}"
        );
    }
    let server_time = |key: &str| SystemTime::from(entry(key).last_modified.unwrap());

    // Local folder: one file equal to S3, one changed and newer, one only local, and
    // one with the same size that is older than the object.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local");
    let minute = Duration::from_secs(60);
    write_file(&root, "same.txt", "same", server_time("sync/same.txt"));
    write_file(
        &root,
        "changed.txt",
        "local!",
        server_time("sync/changed.txt") + minute,
    );
    write_file(&root, "local-only.txt", "l", SystemTime::now());
    write_file(
        &root,
        "nested/deep.txt",
        "deep",
        server_time("sync/nested/deep.txt") - minute,
    );
    // The downloader's temporary files are never synced.
    write_file(&root, "ignored.bbpart", "tmp", SystemTime::now());

    let prefix = S3Uri::new(&bucket, PREFIX).unwrap();

    // Upload with delete: the folder is the source of truth.
    let plan = analyze(&root, &prefix, SyncDirection::Upload, true, &listing);
    assert_eq!(
        summary(&plan),
        vec![
            ("changed.txt", Copy, Some(Remote)),
            ("local-only.txt", Copy, Some(Remote)),
            ("nested/deep.txt", Equal, None),
            ("remote-only.txt", Delete, Some(Remote)),
            ("same.txt", Equal, None),
            ("stale.bbpart", Failed, None),
        ]
    );
    let changed = plan
        .rows
        .iter()
        .find(|row| row.rel_path == "changed.txt")
        .unwrap();
    assert_eq!(changed.s3_meta.as_ref(), Some(entry("sync/changed.txt")));
    assert_eq!(changed.local_meta.as_ref().map(|meta| meta.size), Some(6));
    assert_eq!(plan.s3_prefix, prefix);
    assert_eq!(plan.local_root, root);
    assert_eq!(plan.account_id, "test-account");
    assert!(
        plan.rows
            .iter()
            .all(|row| row.done == (row.action == Equal))
    );

    // Upload without delete: objects that exist only in S3 produce no row.
    let plan = analyze(&root, &prefix, SyncDirection::Upload, false, &listing);
    assert_eq!(
        summary(&plan),
        vec![
            ("changed.txt", Copy, Some(Remote)),
            ("local-only.txt", Copy, Some(Remote)),
            ("nested/deep.txt", Equal, None),
            ("same.txt", Equal, None),
        ]
    );

    // Download without delete: local-only files produce no row; the newer object
    // replaces the older local copy of the same size.
    let plan = analyze(&root, &prefix, SyncDirection::Download, false, &listing);
    assert_eq!(
        summary(&plan),
        vec![
            ("changed.txt", Copy, Some(Local)),
            ("nested/deep.txt", Copy, Some(Local)),
            ("remote-only.txt", Copy, Some(Local)),
            ("same.txt", Equal, None),
            ("stale.bbpart", Failed, None),
        ]
    );

    // Download with delete also removes the local-only file.
    let plan = analyze(&root, &prefix, SyncDirection::Download, true, &listing);
    assert_eq!(
        plan.rows
            .iter()
            .find(|row| row.rel_path == "local-only.txt")
            .map(|row| (row.action, row.target)),
        Some((Delete, Some(Local)))
    );

    // Bidirectional: the newer side wins, one-sided files are copied across.
    let plan = analyze(
        &root,
        &prefix,
        SyncDirection::Bidirectional,
        false,
        &listing,
    );
    assert_eq!(
        summary(&plan),
        vec![
            ("changed.txt", Copy, Some(Remote)),
            ("local-only.txt", Copy, Some(Remote)),
            ("nested/deep.txt", Copy, Some(Local)),
            ("remote-only.txt", Copy, Some(Local)),
            ("same.txt", Equal, None),
            ("stale.bbpart", Failed, None),
        ]
    );

    // Bidirectional with delete is refused.
    let mut request =
        AnalyzeRequest::new("test-account", &root, &prefix, SyncDirection::Bidirectional);
    request.delete = true;
    let err = sync::analyze(&request, listing.iter().cloned()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);

    // The plan survives the store and records execution results row by row.
    let store = SyncStore::open(dir.path().join("plans")).unwrap();
    store.insert(&plan).unwrap();
    assert_eq!(store.get(&plan.id).unwrap(), plan);
    let index = |rel: &str| {
        plan.rows
            .iter()
            .position(|row| row.rel_path == rel)
            .unwrap()
    };
    store
        .update_rows(
            &plan.id,
            &[
                RowUpdate {
                    index: index("changed.txt"),
                    outcome: RowOutcome::Done,
                },
                RowUpdate {
                    index: index("remote-only.txt"),
                    outcome: RowOutcome::Error("connection reset".to_owned()),
                },
            ],
        )
        .unwrap();
    let counts = store.counts(&plan.id).unwrap();
    assert_eq!(
        counts,
        RowCounts {
            total: 4,
            done: 1,
            failed: 2,
        }
    );
    let stored = store.get(&plan.id).unwrap();
    let retry = &stored.rows[index("remote-only.txt")];
    assert_eq!((retry.action, retry.done), (Copy, false));
    assert_eq!(retry.error.as_deref(), Some("connection reset"));
    assert_eq!(sync::row_counts(&stored.rows), counts);
}
