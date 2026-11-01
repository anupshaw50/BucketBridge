//! Integration tests of the S3 transport (`bucketbridge_core::ops`) against a
//! real server: s3s-fs by default, MinIO with `--features minio` and
//! `BB_TEST_BACKEND=minio`. Tests that need features s3s-fs lacks are ignored
//! unless the `minio` feature is enabled (see docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use bucketbridge_core::aws_sdk_s3::primitives::ByteStream;
use bucketbridge_core::aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
use bucketbridge_core::model::{Account, BucketKind, ObjectEntry};
use bucketbridge_core::settings::MIB;
use bucketbridge_core::{ClientOptions, CopyOptions, Credentials, ErrorKind, S3, S3Uri};
use bucketbridge_tests::{Backend, TestS3};
use futures::StreamExt;

fn keys(entries: &[ObjectEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.key.as_str()).collect()
}

async fn put_many(s3: &TestS3, bucket: &str, keys: Vec<String>) {
    futures::stream::iter(keys)
        .for_each_concurrent(32, |key| async move {
            s3.put(bucket, &key, key.clone().into_bytes()).await
        })
        .await;
}

/// Deterministic bytes that are not a repeating short pattern.
fn payload(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn uri(bucket: &str, key: &str) -> S3Uri {
    S3Uri::new(bucket, key).unwrap()
}

// ---- buckets ----

#[tokio::test]
async fn list_buckets_reports_created_buckets_as_external() {
    let s3 = TestS3::start().await;
    let a = s3.new_bucket().await;
    let b = s3.new_bucket().await;
    let buckets = s3.ops().list_buckets().await.unwrap();
    let names: BTreeSet<&str> = buckets.iter().map(|b| b.name.as_str()).collect();
    assert!(
        names.contains(a.as_str()) && names.contains(b.as_str()),
        "{names:?}"
    );
    for bucket in &buckets {
        assert_eq!(bucket.kind, BucketKind::External);
        assert!(bucket.creation_date.is_some(), "{bucket:?}");
    }
}

#[tokio::test]
async fn create_and_delete_bucket() {
    let s3 = TestS3::start().await;
    let name = s3.unique_bucket();
    s3.ops().create_bucket(&name, None).await.unwrap();
    assert!(
        s3.ops()
            .list_buckets()
            .await
            .unwrap()
            .iter()
            .any(|b| b.name == name)
    );
    s3.ops().delete_bucket(&name).await.unwrap();
    assert!(
        !s3.ops()
            .list_buckets()
            .await
            .unwrap()
            .iter()
            .any(|b| b.name == name)
    );
}

#[tokio::test]
#[cfg_attr(
    feature = "minio",
    ignore = "MinIO rejects location constraints other than its own region"
)]
async fn create_bucket_in_an_explicit_region() {
    let s3 = TestS3::start().await;
    let name = s3.unique_bucket();
    s3.ops()
        .create_bucket(&name, Some("eu-west-1"))
        .await
        .unwrap();
    assert!(
        s3.ops()
            .list_buckets()
            .await
            .unwrap()
            .iter()
            .any(|b| b.name == name)
    );
}

#[tokio::test]
async fn invalid_bucket_names_are_rejected_before_sending() {
    let s3 = TestS3::start().await;
    for name in ["Bad_Name", "ab", "-start", "a..b", "192.168.0.1"] {
        let err = s3.ops().create_bucket(name, None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{name}: {err}");
    }
    let err = s3
        .ops()
        .create_bucket("good-name", Some("bad region"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(s3.ops().list_buckets().await.unwrap().is_empty());
}

#[tokio::test]
async fn creating_an_existing_bucket_is_a_conflict() {
    let s3 = TestS3::start().await;
    let name = s3.new_bucket().await;
    let err = s3.ops().create_bucket(&name, None).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
}

#[tokio::test]
async fn deleting_a_non_empty_bucket_is_a_conflict_and_keeps_the_data() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "keep/me.txt", "precious").await;

    let err = s3.ops().delete_bucket(&bucket).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    assert!(err.to_string().contains("not empty"), "{err}");
    assert_eq!(s3.get(&bucket, "keep/me.txt").await, b"precious");

    s3.ops()
        .delete_object(&bucket, "keep/me.txt", None)
        .await
        .unwrap();
    s3.ops().delete_bucket(&bucket).await.unwrap();
}

#[tokio::test]
async fn deleting_a_missing_bucket_is_not_found() {
    let s3 = TestS3::start().await;
    let err = s3.ops().delete_bucket("does-not-exist").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

// ---- listing ----

#[tokio::test]
async fn delimiter_listing_puts_prefixes_first() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    for key in [
        "b.txt",
        "a.txt",
        "dir1/x.txt",
        "dir1/sub/y.txt",
        "dir2/z.txt",
    ] {
        s3.put(&bucket, key, "1234").await;
    }
    let page = s3
        .ops()
        .list_objects(&bucket, "", false, None, 1000)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["dir1/", "dir2/", "a.txt", "b.txt"]);
    assert_eq!(page.next_token, None);
    for entry in &page.entries {
        assert_eq!(entry.is_prefix, entry.key.ends_with('/'));
        if entry.is_prefix {
            assert_eq!(entry.size, 0);
            assert_eq!(entry.last_modified, None);
        } else {
            assert_eq!(entry.size, 4);
            assert!(entry.last_modified.is_some());
            assert!(entry.checksums.is_empty());
        }
    }

    let page = s3
        .ops()
        .list_objects(&bucket, "dir1/", false, None, 1000)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["dir1/sub/", "dir1/x.txt"]);
}

#[tokio::test]
async fn flat_listing_is_recursive() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    for key in ["a.txt", "dir1/x.txt", "dir1/sub/y.txt", "dir2/z.txt"] {
        s3.put(&bucket, key, "x").await;
    }
    let page = s3
        .ops()
        .list_objects(&bucket, "", true, None, 1000)
        .await
        .unwrap();
    assert_eq!(
        keys(&page.entries),
        ["a.txt", "dir1/sub/y.txt", "dir1/x.txt", "dir2/z.txt"]
    );
    assert!(page.entries.iter().all(|e| !e.is_prefix));
    let page = s3
        .ops()
        .list_objects(&bucket, "dir1/", true, None, 1000)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["dir1/sub/y.txt", "dir1/x.txt"]);
}

#[tokio::test]
async fn listing_never_returns_the_entry_equal_to_the_prefix() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "report", "the prefix itself").await;
    s3.put(&bucket, "report-2024.txt", "a sibling").await;

    for flat in [false, true] {
        let page = s3
            .ops()
            .list_objects(&bucket, "report", flat, None, 1000)
            .await
            .unwrap();
        assert_eq!(keys(&page.entries), ["report-2024.txt"], "flat={flat}");
    }
    // With one key per page the first page holds only the prefix entry; it is
    // skipped instead of returning an empty page with a token.
    let page = s3
        .ops()
        .list_objects(&bucket, "report", true, None, 1)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["report-2024.txt"]);
}

#[tokio::test]
async fn pagination_across_page_boundaries() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let expected: Vec<String> = (0..25).map(|i| format!("p/obj-{i:02}")).collect();
    put_many(&s3, &bucket, expected.clone()).await;

    let mut seen = Vec::new();
    let mut sizes = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .ops()
            .list_objects(&bucket, "p/", true, token.as_deref(), 10)
            .await
            .unwrap();
        sizes.push(page.entries.len());
        seen.extend(page.entries.into_iter().map(|e| e.key));
        match page.next_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    assert_eq!(sizes, [10, 10, 5]);
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn pagination_with_prefixes_and_objects() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let mut all: Vec<String> = (0..5).map(|i| format!("q/d{i}/x")).collect();
    all.extend((0..7).map(|i| format!("q/f{i}")));
    put_many(&s3, &bucket, all).await;

    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .ops()
            .list_objects(&bucket, "q/", false, token.as_deref(), 4)
            .await
            .unwrap();
        assert!(page.entries.len() <= 4);
        seen.extend(page.entries);
        match page.next_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 12, "duplicates or gaps: {:?}", keys(&seen));
    let prefixes: BTreeSet<String> = seen
        .iter()
        .filter(|e| e.is_prefix)
        .map(|e| e.key.clone())
        .collect();
    let objects: BTreeSet<String> = seen
        .iter()
        .filter(|e| !e.is_prefix)
        .map(|e| e.key.clone())
        .collect();
    let expected_prefixes: BTreeSet<String> = (0..5).map(|i| format!("q/d{i}/")).collect();
    let expected_objects: BTreeSet<String> = (0..7).map(|i| format!("q/f{i}")).collect();
    assert_eq!(prefixes, expected_prefixes);
    assert_eq!(objects, expected_objects);
}

#[tokio::test]
async fn list_all_and_delete_prefix_beyond_one_page() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bulk: Vec<String> = (0..1001).map(|i| format!("bulk/item-{i:04}")).collect();
    put_many(&s3, &bucket, bulk.clone()).await;
    s3.put(
        &bucket,
        "bulky.txt",
        "sibling with the same leading characters",
    )
    .await;
    s3.put(&bucket, "other/x", "unrelated").await;

    let all = s3.ops().list_all(&bucket, "bulk/").await.unwrap();
    assert_eq!(all.len(), 1001);
    assert_eq!(all.iter().map(|e| e.key.clone()).collect::<Vec<_>>(), bulk);

    let outcome = s3.ops().delete_prefix(&bucket, "bulk/").await.unwrap();
    assert_eq!(outcome.deleted, 1001);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert!(
        s3.ops()
            .list_all(&bucket, "bulk/")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(s3.exists(&bucket, "bulky.txt").await);
    assert!(s3.exists(&bucket, "other/x").await);
}

#[tokio::test]
async fn delete_prefix_requires_a_folder_prefix() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "photos.txt", "x").await;
    for prefix in ["", "/", "photos"] {
        let err = s3.ops().delete_prefix(&bucket, prefix).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{prefix:?}");
    }
    assert!(s3.exists(&bucket, "photos.txt").await);
}

#[tokio::test]
async fn list_page_size_and_missing_bucket() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    for max_keys in [0, 1001] {
        let err = s3
            .ops()
            .list_objects(&bucket, "", false, None, max_keys)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
    let err = s3
        .ops()
        .list_objects("no-such-bucket", "", false, None, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    assert!(err.to_string().contains("s3://no-such-bucket/"), "{err}");
    let err = s3.ops().list_all("no-such-bucket", "").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

// ---- objects ----

#[tokio::test]
async fn head_object_returns_size_etag_metadata_and_checksums() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("docs/a.txt")
        .body(ByteStream::from_static(b"hello world"))
        .content_type("text/plain")
        .cache_control("max-age=60")
        .content_disposition("inline")
        .content_encoding("gzip")
        .metadata("owner", "team")
        .send()
        .await
        .unwrap();

    let head = s3
        .ops()
        .head_object(&bucket, "docs/a.txt", None)
        .await
        .unwrap();
    assert_eq!(head.key, "docs/a.txt");
    assert_eq!(head.size, 11);
    assert_eq!(
        head.etag.as_deref(),
        Some("5eb63bbbe01eeed093cb22bb8f5acdc3")
    );
    assert!(head.last_modified.is_some());
    assert_eq!(head.meta.content_type.as_deref(), Some("text/plain"));
    assert_eq!(head.meta.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(head.meta.content_disposition.as_deref(), Some("inline"));
    assert_eq!(head.meta.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(
        head.meta.user_meta.get("owner").map(String::as_str),
        Some("team")
    );
    // The SDK sends a CRC32 checksum by default; the server stores and returns it.
    assert_eq!(head.checksums.crc32.as_deref(), Some("DUoRhQ=="));
}

#[tokio::test]
async fn head_of_missing_object_or_bucket_is_not_found() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let err = s3
        .ops()
        .head_object(&bucket, "missing.txt", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    assert!(
        err.to_string()
            .contains(&format!("s3://{bucket}/missing.txt")),
        "{err}"
    );
    let err = s3
        .ops()
        .head_object("no-such-bucket", "k", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    let err = s3.ops().head_object(&bucket, "", None).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn create_folder_writes_a_marker() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.ops()
        .create_folder(&bucket, "photos/2024/")
        .await
        .unwrap();
    match s3.backend() {
        // s3s-fs stores folder markers as directories (see docs/testing.md).
        Backend::S3sFs => {
            let dir = s3.root().unwrap().join(&bucket).join("photos").join("2024");
            assert!(dir.is_dir(), "{}", dir.display());
        }
        Backend::MinIO => {
            let page = s3
                .ops()
                .list_objects(&bucket, "photos/", false, None, 1000)
                .await
                .unwrap();
            assert_eq!(keys(&page.entries), ["photos/2024/"]);
            assert!(page.entries[0].is_prefix);
        }
    }
    for bad in ["no-slash", "/", "", "//"] {
        let err = s3.ops().create_folder(&bucket, bad).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{bad:?}");
    }
}

#[tokio::test]
async fn delete_object_is_idempotent() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a/b.txt", "x").await;
    s3.ops()
        .delete_object(&bucket, "a/b.txt", None)
        .await
        .unwrap();
    assert!(!s3.exists(&bucket, "a/b.txt").await);
    s3.ops()
        .delete_object(&bucket, "a/b.txt", None)
        .await
        .unwrap();
    let err = s3.ops().delete_object(&bucket, "", None).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn delete_objects_batches_more_than_1000_keys() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let real = ["batch/a", "batch/b", "batch/c"];
    for key in real {
        s3.put(&bucket, key, "x").await;
    }
    s3.put(&bucket, "keep", "x").await;
    let mut keys: Vec<String> = real.iter().map(|k| (*k).to_owned()).collect();
    keys.extend((0..998).map(|i| format!("batch/missing-{i}")));
    let outcome = s3.ops().delete_objects(&bucket, &keys).await.unwrap();
    assert_eq!(outcome.deleted, 1001);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    for key in real {
        assert!(!s3.exists(&bucket, key).await, "{key}");
    }
    assert!(s3.exists(&bucket, "keep").await);

    let empty = s3.ops().delete_objects(&bucket, &[]).await.unwrap();
    assert_eq!(empty.deleted, 0);
    let err = s3
        .ops()
        .delete_objects(&bucket, &[String::new()])
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

// ---- copy ----

#[tokio::test]
async fn single_request_copy_keeps_metadata() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let target = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("src/a b+ü.txt")
        .body(ByteStream::from_static(b"copy me"))
        .content_type("text/markdown")
        .metadata("origin", "test")
        .send()
        .await
        .unwrap();

    let outcome = s3
        .ops()
        .copy_object(
            &uri(&bucket, "src/a b+ü.txt"),
            &uri(&target, "dst/copy.txt"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.size, 7);
    assert_eq!(outcome.parts, 0);
    assert!(outcome.etag.is_some());
    assert_eq!(s3.get(&target, "dst/copy.txt").await, b"copy me");
    let head = s3
        .ops()
        .head_object(&target, "dst/copy.txt", None)
        .await
        .unwrap();
    assert_eq!(head.meta.content_type.as_deref(), Some("text/markdown"));
    assert_eq!(
        head.meta.user_meta.get("origin").map(String::as_str),
        Some("test")
    );
}

#[tokio::test]
async fn multipart_copy_keeps_content_and_metadata() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let body = payload(12 * MIB as usize);
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("big.bin")
        .body(ByteStream::from(body.clone()))
        .content_type("application/x-test")
        .cache_control("no-store")
        .metadata("origin", "multipart")
        .send()
        .await
        .unwrap();

    let progress = Mutex::new(Vec::new());
    let options = CopyOptions {
        multipart_threshold: 6 * MIB,
        part_size: 5 * MIB,
        concurrency: 2,
    };
    let outcome = s3
        .ops()
        .copy_object_with(
            &uri(&bucket, "big.bin"),
            &uri(&bucket, "copy/big.bin"),
            &options,
            |done| {
                progress.lock().unwrap().push(done);
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.parts, 3);
    assert_eq!(outcome.size, body.len() as u64);
    assert!(
        outcome.etag.as_deref().is_some_and(|e| e.ends_with("-3")),
        "{outcome:?}"
    );

    let progress = progress.into_inner().unwrap();
    assert_eq!(progress.len(), 3);
    assert!(progress.windows(2).all(|w| w[0] < w[1]), "{progress:?}");
    assert_eq!(progress.last().copied(), Some(body.len() as u64));

    assert!(
        s3.get(&bucket, "copy/big.bin").await == body,
        "copied bytes differ"
    );
    let head = s3
        .ops()
        .head_object(&bucket, "copy/big.bin", None)
        .await
        .unwrap();
    assert_eq!(
        head.meta.content_type.as_deref(),
        Some("application/x-test")
    );
    assert_eq!(head.meta.cache_control.as_deref(), Some("no-store"));
    assert_eq!(
        head.meta.user_meta.get("origin").map(String::as_str),
        Some("multipart")
    );
    assert_eq!(s3.pending_uploads(&bucket, "copy/big.bin").await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_multipart_copy_aborts_the_upload() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "src.bin", payload(12 * MIB as usize)).await;

    // After the first part the copy waits until the test has deleted the
    // source, so the second part fails deterministically. The wait uses
    // block_in_place so that tasks queued on this worker (such as pooled
    // connection drivers) move to another thread instead of stalling.
    let (part_done_tx, mut part_done_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    let ops = s3.ops().clone();
    let (src, dst) = (uri(&bucket, "src.bin"), uri(&bucket, "dst.bin"));
    let copy = tokio::spawn(async move {
        let options = CopyOptions {
            multipart_threshold: 6 * MIB,
            part_size: 5 * MIB,
            concurrency: 1,
        };
        ops.copy_object_with(&src, &dst, &options, move |done| {
            if part_done_tx.send(done).is_ok() {
                tokio::task::block_in_place(|| {
                    let _ = resume_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                });
            }
        })
        .await
    });

    assert_eq!(part_done_rx.recv().await, Some(5 * MIB));
    s3.ops()
        .delete_object(&bucket, "src.bin", None)
        .await
        .unwrap();
    resume_tx.send(()).unwrap();

    let err = copy.await.unwrap().unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::NotFound | ErrorKind::Remote),
        "{err}"
    );
    assert!(err.to_string().contains("copying part 2"), "{err}");
    assert!(!s3.exists(&bucket, "dst.bin").await);
    assert_eq!(
        s3.pending_uploads(&bucket, "dst.bin").await,
        0,
        "the multipart upload was not aborted"
    );
}

#[tokio::test]
async fn copy_rejects_self_copy_and_reports_missing_source() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a", "x").await;
    let err = s3
        .ops()
        .copy_object(&uri(&bucket, "a"), &uri(&bucket, "a"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let err = s3
        .ops()
        .copy_object(&uri(&bucket, "missing"), &uri(&bucket, "b"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    let err = s3
        .ops()
        .copy_object(&uri(&bucket, "a"), &uri(&bucket, ""))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

// ---- presigned URLs ----

#[tokio::test]
async fn presigned_get_and_put_work_with_a_plain_http_client() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "shared/a b+ü.txt", "shared content").await;

    let url = s3
        .ops()
        .presign_get(&bucket, "shared/a b+ü.txt", Duration::from_secs(300))
        .await
        .unwrap();
    assert!(url.contains("X-Amz-Signature="), "{url}");
    assert!(url.contains("X-Amz-Expires=300"), "{url}");
    let response = reqwest::get(&url).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"shared content");

    // The signature is really checked.
    let tampered = url.replace("X-Amz-Expires=300", "X-Amz-Expires=301");
    assert_eq!(
        reqwest::get(&tampered).await.unwrap().status().as_u16(),
        403
    );

    let put_url = s3
        .ops()
        .presign_put(&bucket, "incoming/upload.txt", Duration::from_secs(60))
        .await
        .unwrap();
    let response = reqwest::Client::new()
        .put(&put_url)
        .header("content-type", "text/plain")
        .body("uploaded with a presigned URL")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(
        s3.get(&bucket, "incoming/upload.txt").await,
        b"uploaded with a presigned URL"
    );
}

#[tokio::test]
async fn presign_expiry_is_bounded() {
    let s3 = TestS3::start().await;
    let ops = s3.ops();
    for expires in [
        Duration::ZERO,
        Duration::from_millis(999),
        Duration::from_secs(7 * 24 * 3600 + 1),
    ] {
        let err = ops.presign_get("bucket", "k", expires).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{expires:?}");
        let err = ops.presign_put("bucket", "k", expires).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{expires:?}");
    }
    for expires in [Duration::from_secs(1), Duration::from_secs(7 * 24 * 3600)] {
        let url = ops.presign_get("bucket", "k", expires).await.unwrap();
        assert!(
            url.contains(&format!("X-Amz-Expires={}", expires.as_secs())),
            "{url}"
        );
    }
}

#[tokio::test]
async fn presigned_url_signs_the_request_payer_parameter() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "k.txt", "requester pays").await;
    let mut account = s3.account().clone();
    account.request_payer = true;
    let payer = S3::new(&account, &s3.credentials(), &ClientOptions::default()).unwrap();
    let url = payer
        .presign_get(&bucket, "k.txt", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(url.contains("x-amz-request-payer=requester"), "{url}");
    let response = reqwest::get(&url).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"requester pays");
}

// ---- accounts and transport ----

#[tokio::test]
async fn wrong_credentials_are_auth_errors() {
    let s3 = TestS3::start().await;
    let options = ClientOptions::default();
    let wrong_secret = s3.connect(
        &Credentials::new(s3.access_key(), "not-the-secret", None),
        &options,
    );
    let err = wrong_secret.list_buckets().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth, "{err}");
    assert_eq!(err.exit_code(), 3);

    let unknown_key = s3.connect(
        &Credentials::new("AKIDUNKNOWN", s3.secret_key(), None),
        &options,
    );
    let err = unknown_key.list_buckets().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth, "{err}");
}

#[tokio::test]
async fn custom_endpoint_uses_path_style_addressing() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    assert_eq!(s3.ops().bucket_kind(), BucketKind::External);
    assert!(s3.account().path_style);
    let url = s3
        .ops()
        .presign_get(&bucket, "dir/k", Duration::from_secs(60))
        .await
        .unwrap();
    let expected = format!("{}/{bucket}/dir/k?", s3.endpoint());
    assert!(
        url.starts_with(&expected),
        "{url} does not start with {expected}"
    );
}

#[tokio::test]
async fn two_accounts_on_two_servers_work_concurrently() {
    let (a, b) = tokio::join!(TestS3::start(), TestS3::start());
    let ops_a = S3::new(a.account(), &a.credentials(), &ClientOptions::default()).unwrap();
    let ops_b = S3::new(b.account(), &b.credentials(), &ClientOptions::default()).unwrap();
    let (bucket_a, bucket_b) = (a.unique_bucket(), b.unique_bucket());
    let (ra, rb) = tokio::join!(
        ops_a.create_bucket(&bucket_a, None),
        ops_b.create_bucket(&bucket_b, None)
    );
    ra.unwrap();
    rb.unwrap();

    let keys_a: Vec<String> = (0..20).map(|i| format!("a/{i}")).collect();
    let keys_b: Vec<String> = (0..20).map(|i| format!("b/{i}")).collect();
    tokio::join!(
        put_many(&a, &bucket_a, keys_a),
        put_many(&b, &bucket_b, keys_b)
    );

    let (list_a, list_b, buckets_a, buckets_b) = tokio::join!(
        ops_a.list_all(&bucket_a, ""),
        ops_b.list_all(&bucket_b, ""),
        ops_a.list_buckets(),
        ops_b.list_buckets()
    );
    assert_eq!(list_a.unwrap().len(), 20);
    assert_eq!(list_b.unwrap().len(), 20);
    let names_a: Vec<String> = buckets_a.unwrap().into_iter().map(|b| b.name).collect();
    let names_b: Vec<String> = buckets_b.unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(names_a, [bucket_a]);
    assert_eq!(names_b, [bucket_b]);

    if a.backend() == Backend::S3sFs {
        // Each server only accepts its own keys.
        let crossed = b.connect(&a.credentials(), &ClientOptions::default());
        assert_eq!(
            crossed.list_buckets().await.unwrap_err().kind(),
            ErrorKind::Auth
        );
    }
}

#[tokio::test]
async fn unreachable_endpoint_is_a_remote_error() {
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let account = Account {
        id: "offline".into(),
        label: "Offline".into(),
        access_key_id: "AKIDOFFLINE".into(),
        secret_ref: "offline/secret".into(),
        endpoint: Some(format!("http://127.0.0.1:{port}")),
        path_style: true,
        ..Account::default()
    };
    let options = ClientOptions {
        max_attempts: 1,
        connect_timeout: Duration::from_secs(2),
        ..ClientOptions::default()
    };
    let ops = S3::new(
        &account,
        &Credentials::new("AKIDOFFLINE", "secret", None),
        &options,
    )
    .unwrap();
    let err = ops.list_buckets().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
    assert_eq!(err.exit_code(), 2);
}

// ---- multipart uploads ----

#[tokio::test]
async fn abort_multipart_upload_frees_the_upload() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let created = s3
        .client()
        .create_multipart_upload()
        .bucket(&bucket)
        .key("partial.bin")
        .send()
        .await
        .unwrap();
    let upload_id = created.upload_id().unwrap().to_owned();
    assert_eq!(s3.pending_uploads(&bucket, "partial.bin").await, 1);
    s3.ops()
        .abort_multipart_upload(&bucket, "partial.bin", &upload_id)
        .await
        .unwrap();
    assert_eq!(s3.pending_uploads(&bucket, "partial.bin").await, 0);
    let err = s3
        .ops()
        .abort_multipart_upload(&bucket, "partial.bin", "")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

// ---- MinIO only ----

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement ListMultipartUploads"
)]
async fn list_and_abort_uncompleted_multipart_uploads() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    // MinIO lists uploads per object key, so the test lists by exact key.
    for key in ["up/a", "up/a", "up/b"] {
        s3.client()
            .create_multipart_upload()
            .bucket(&bucket)
            .key(key)
            .send()
            .await
            .unwrap();
    }
    let uploads = s3
        .ops()
        .list_multipart_uploads(&bucket, "up/a")
        .await
        .unwrap();
    assert_eq!(uploads.len(), 2, "{uploads:?}");
    assert!(uploads.iter().all(|u| u.key == "up/a"));
    assert!(
        uploads
            .iter()
            .all(|u| u.initiated.is_some() && !u.upload_id.is_empty())
    );
    assert_ne!(uploads[0].upload_id, uploads[1].upload_id);
    for upload in &uploads {
        s3.ops()
            .abort_multipart_upload(&bucket, &upload.key, &upload.upload_id)
            .await
            .unwrap();
    }
    assert_eq!(s3.pending_uploads(&bucket, "up/a").await, 0);
    assert_eq!(s3.pending_uploads(&bucket, "up/b").await, 1);
    let err = s3
        .ops()
        .abort_multipart_upload(&bucket, "up/a", "no-such-upload")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

async fn enable_versioning(s3: &TestS3, bucket: &str) {
    s3.client()
        .put_bucket_versioning()
        .bucket(bucket)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn list_versions_shows_versions_and_delete_markers() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    enable_versioning(&s3, &bucket).await;
    s3.put(&bucket, "v.txt", "one").await;
    s3.put(&bucket, "v.txt", "two").await;
    s3.ops()
        .delete_object(&bucket, "v.txt", None)
        .await
        .unwrap();
    s3.put(&bucket, "w.txt", "only").await;

    let page = s3
        .ops()
        .list_versions(&bucket, "", true, None, 1000)
        .await
        .unwrap();
    assert_eq!(page.next_token, None);
    let summary: Vec<(&str, bool, bool)> = page
        .entries
        .iter()
        .map(|v| (v.key.as_str(), v.is_delete_marker, v.is_latest))
        .collect();
    assert_eq!(
        summary,
        [
            ("v.txt", true, true),
            ("v.txt", false, false),
            ("v.txt", false, false),
            ("w.txt", false, true)
        ]
    );

    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .ops()
            .list_versions(&bucket, "", true, token.as_deref(), 1)
            .await
            .unwrap();
        collected.extend(page.entries);
        match page.next_token {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    assert_eq!(collected, page.entries);

    // Restore the previous version by deleting the delete marker.
    let marker = &page.entries[0];
    assert!(marker.is_delete_marker);
    s3.ops()
        .delete_object(&bucket, "v.txt", Some(&marker.version_id))
        .await
        .unwrap();
    assert_eq!(s3.get(&bucket, "v.txt").await, b"two");
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn bucket_with_only_versions_left_is_not_deleted() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    enable_versioning(&s3, &bucket).await;
    s3.put(&bucket, "gone.txt", "x").await;
    s3.ops()
        .delete_object(&bucket, "gone.txt", None)
        .await
        .unwrap();
    assert!(s3.ops().list_all(&bucket, "").await.unwrap().is_empty());
    let err = s3.ops().delete_bucket(&bucket).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    assert!(err.to_string().contains("versions"), "{err}");
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not list folder-marker objects"
)]
async fn folder_marker_is_never_listed_under_itself() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    s3.ops().create_folder(&bucket, "photos/").await.unwrap();
    s3.put(&bucket, "photos/a.jpg", "jpeg").await;
    let page = s3
        .ops()
        .list_objects(&bucket, "photos/", false, None, 1000)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["photos/a.jpg"]);
    let page = s3
        .ops()
        .list_objects(&bucket, "photos/", true, None, 1)
        .await
        .unwrap();
    assert_eq!(keys(&page.entries), ["photos/a.jpg"]);
    // Recursive listing includes the marker so that delete_prefix removes it.
    let all = s3.ops().list_all(&bucket, "photos/").await.unwrap();
    assert_eq!(keys(&all), ["photos/", "photos/a.jpg"]);
    let outcome = s3.ops().delete_prefix(&bucket, "photos/").await.unwrap();
    assert_eq!(outcome.deleted, 2);
    assert!(s3.ops().list_all(&bucket, "").await.unwrap().is_empty());
}
