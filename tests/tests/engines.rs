//! Integration tests of the transfer engines (`bucketbridge_core::engine`)
//! against a real server: s3s-fs by default, MinIO with `--features minio`
//! and `BB_TEST_BACKEND=minio`. Request-level contracts (headers, bodies)
//! and server behaviour that s3s-fs lacks (`If-Match` on GET, per-key
//! delete failures) are checked against the scripted `MockS3`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bucketbridge_core::engine::checksum::{is_full_object, value_of};
use bucketbridge_core::engine::{
    DownloadSink, DownloadState, Hooks, Outcome, ProgressCounter, SourceFingerprint, StopReason,
    StopSignal, Throttle, ThrottleFuture, TransferEngine, TransferOptions, UploadSource,
    UploadState, checksum_of, expand_upload, partial_path,
};
use bucketbridge_core::settings::{ChecksumAlgorithm, MIB};
use bucketbridge_core::{ClientOptions, ErrorKind, S3Uri};
use bucketbridge_tests::{Backend, MockResponse, MockS3, RecordedRequest, TestS3};
use futures::StreamExt;

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

// ---- helpers ----

/// Deterministic bytes that are not a repeating short pattern; different
/// seeds give different bytes.
fn payload(len: usize, seed: u32) -> Vec<u8> {
    // A bijective mix of the seed; xorshift only needs a non-zero state.
    let mut state: u32 = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
    if state == 0 {
        state = 0x1234_5678;
    }
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

/// The smallest legal parts (5 MiB), multipart from 5 MiB on.
fn small_parts() -> TransferOptions {
    TransferOptions {
        multipart_threshold: 5 * MIB,
        part_size: 5 * MIB,
        max_parts_in_flight: 2,
        ..TransferOptions::default()
    }
}

fn engine(s3: &TestS3, options: TransferOptions) -> TransferEngine {
    TransferEngine::new(s3.ops().clone(), options).unwrap()
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    path
}

/// A throttle that counts what it is asked for.
#[derive(Default)]
struct CountingThrottle {
    bytes: AtomicU64,
}

impl Throttle for CountingThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        self.bytes.fetch_add(bytes, Ordering::SeqCst);
        Box::pin(std::future::ready(()))
    }
}

impl CountingThrottle {
    fn total(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }
}

/// A throttle that stops the run once `limit` bytes have passed it.
struct StopAfter {
    bytes: AtomicU64,
    limit: u64,
    stop: StopSignal,
    reason: StopReason,
}

impl StopAfter {
    fn new(limit: u64, stop: &StopSignal, reason: StopReason) -> Arc<Self> {
        Arc::new(StopAfter {
            bytes: AtomicU64::new(0),
            limit,
            stop: stop.clone(),
            reason,
        })
    }
}

impl Throttle for StopAfter {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        if self.bytes.fetch_add(bytes, Ordering::SeqCst) + bytes >= self.limit {
            match self.reason {
                StopReason::Pause => self.stop.pause(),
                StopReason::Cancel => self.stop.cancel(),
            }
        }
        Box::pin(std::future::ready(()))
    }
}

/// Checkpoints received, in order.
struct Saved<S>(Mutex<Vec<S>>);

impl<S: Clone> Saved<S> {
    fn new() -> Self {
        Saved(Mutex::new(Vec::new()))
    }

    fn push(&self, state: &S) {
        self.0.lock().unwrap().push(state.clone());
    }

    fn last(&self) -> S {
        self.0
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a checkpoint")
    }

    fn all(&self) -> Vec<S> {
        self.0.lock().unwrap().clone()
    }
}

fn no_upload_checkpoint(_: &UploadState) {}

fn no_download_checkpoint(_: &DownloadState) {}

/// Whether a folder marker exists (s3s-fs stores markers as directories and
/// never lists them, see docs/testing.md).
async fn marker_exists(s3: &TestS3, bucket: &str, key: &str) -> bool {
    match s3.backend() {
        Backend::S3sFs => s3
            .root()
            .unwrap()
            .join(bucket)
            .join(key.trim_end_matches('/'))
            .is_dir(),
        Backend::MinIO => s3
            .ops()
            .list_all(bucket, key)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.key == key),
    }
}

/// Uploads a file with the engine, without hooks.
async fn upload(engine: &TransferEngine, path: &Path, dst: &S3Uri) {
    let outcome = engine
        .upload_file(path, dst, &Hooks::new(), None, &no_upload_checkpoint)
        .await
        .unwrap();
    assert!(outcome.is_completed(), "{outcome:?}");
}

// ---- uploads ----

#[tokio::test]
async fn uploads_around_the_multipart_threshold_round_trip() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, small_parts());
    let dir = tempfile::tempdir().unwrap();
    let threshold = 5 * MIB as usize;
    let cases = [
        (0, 0),
        (1, 0),
        (threshold - 1, 0),
        (threshold, 1),
        (threshold + 1, 2),
        (11 * MIB as usize + 123, 3),
    ];
    for (index, (len, parts)) in cases.into_iter().enumerate() {
        let data = payload(len, index as u32 + 7);
        let path = write_file(dir.path(), &format!("file-{len}.txt"), &data);
        let key = format!("sizes/file {len}.txt");
        let throttle = Arc::new(CountingThrottle::default());
        let progress = Arc::new(ProgressCounter::new());
        let hooks = Hooks::new()
            .with_throttle(throttle.clone())
            .with_progress(progress.clone());
        let saved = Saved::new();
        let outcome = engine
            .upload_file(&path, &uri(&bucket, &key), &hooks, None, &|s| saved.push(s))
            .await
            .unwrap()
            .into_completed()
            .unwrap();
        assert_eq!(outcome.size, len as u64, "{len}");
        assert_eq!(outcome.parts, parts, "{len}");
        assert_eq!(outcome.reused_parts, 0);
        assert!(outcome.etag.is_some());
        // Every byte passed the throttle exactly once and was counted once.
        assert_eq!(throttle.total(), len as u64, "{len}");
        assert_eq!(progress.done(), len as u64, "{len}");
        assert_eq!(progress.total(), Some(len as u64));
        // Multipart uploads checkpoint after creating the upload and after
        // every part; single requests have nothing to resume.
        let checkpoints = saved.all();
        if parts == 0 {
            assert!(checkpoints.is_empty(), "{checkpoints:?}");
        } else {
            let last = checkpoints.last().unwrap();
            assert_eq!(last.parts.len(), parts as usize);
            assert!(checkpoints.iter().all(|c| c.upload_id == last.upload_id));
            assert_eq!(last.part_size, 5 * MIB);
        }
        assert!(
            s3.get(&bucket, &key).await == data,
            "{len}: content differs"
        );
        let head = s3.ops().head_object(&bucket, &key, None).await.unwrap();
        assert_eq!(head.size, len as u64);
        assert_eq!(head.meta.content_type.as_deref(), Some("text/plain"));
    }
    assert_eq!(s3.pending_uploads(&bucket, "sizes/").await, 0);
}

#[tokio::test]
async fn every_checksum_algorithm_round_trips() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let small = payload(1000, 1);
    let large = payload(5 * MIB as usize + 4097, 2);
    let small_path = write_file(dir.path(), "small.bin", &small);
    let large_path = write_file(dir.path(), "large.bin", &large);
    for algorithm in [
        ChecksumAlgorithm::Crc32,
        ChecksumAlgorithm::Crc32c,
        ChecksumAlgorithm::Crc64nvme,
        ChecksumAlgorithm::Sha1,
        ChecksumAlgorithm::Sha256,
        ChecksumAlgorithm::None,
    ] {
        let engine = engine(
            &s3,
            TransferOptions {
                checksum: algorithm,
                ..small_parts()
            },
        );
        for (path, data, parts) in [(&small_path, &small, 0), (&large_path, &large, 2)] {
            let key = format!(
                "{algorithm:?}/{}",
                path.file_name().unwrap().to_string_lossy()
            );
            let outcome = engine
                .upload_file(
                    path,
                    &uri(&bucket, &key),
                    &Hooks::new(),
                    None,
                    &no_upload_checkpoint,
                )
                .await
                .unwrap()
                .into_completed()
                .unwrap();
            assert_eq!(outcome.parts, parts, "{key}");
            assert!(
                s3.get(&bucket, &key).await == *data,
                "{key}: content differs"
            );
            let head = s3.ops().head_object(&bucket, &key, None).await.unwrap();
            let expected = checksum_of(algorithm, data);
            match expected {
                None => {
                    assert_eq!(outcome.checksum, None, "{key}");
                    if s3.backend() == Backend::S3sFs {
                        assert!(head.checksums.is_empty(), "{key}: {:?}", head.checksums);
                    }
                }
                Some(expected) => {
                    let stored = value_of(&head.checksums, algorithm)
                        .unwrap_or_else(|| panic!("{key}: no stored checksum"));
                    if parts == 0 || is_full_object(stored) {
                        assert_eq!(stored, expected, "{key}");
                    } else {
                        // A composite checksum of the part checksums.
                        assert!(stored.ends_with("-2"), "{key}: {stored}");
                    }
                    // A single request always reports it; a completion
                    // response carries it where the server sends it.
                    if parts == 0 || outcome.checksum.is_some() {
                        assert_eq!(outcome.checksum.as_deref(), Some(stored), "{key}");
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn paused_multipart_upload_resumes_without_sending_finished_parts_again() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(
        &s3,
        TransferOptions {
            max_parts_in_flight: 1,
            ..small_parts()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let data = payload(11 * MIB as usize + 123, 3);
    let path = write_file(dir.path(), "resume.bin", &data);
    let dst = uri(&bucket, "resume.bin");

    // Pause as soon as the first part is stored.
    let stop = StopSignal::new();
    let saved = Saved::new();
    let checkpoint = |state: &UploadState| {
        saved.push(state);
        if state.parts.len() == 1 {
            stop.pause();
        }
    };
    let hooks = Hooks::new().with_stop(stop.clone());
    let outcome = engine
        .upload_file(&path, &dst, &hooks, None, &checkpoint)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let paused = saved.last();
    assert_eq!(paused.parts.len(), 1);
    assert_eq!(paused.parts[0].number, 1);
    assert!(paused.parts[0].checksum.is_some());
    assert!(!s3.exists(&bucket, "resume.bin").await);
    assert_eq!(s3.pending_uploads(&bucket, "resume.bin").await, 1);

    // The state survives a JSON round trip, as the queue stores it.
    let stored = serde_json::to_value(&paused).unwrap();
    let restored: UploadState = serde_json::from_value(stored).unwrap();

    let throttle = Arc::new(CountingThrottle::default());
    let progress = Arc::new(ProgressCounter::new());
    let hooks = Hooks::new()
        .with_throttle(throttle.clone())
        .with_progress(progress.clone());
    let resumed = Saved::new();
    let outcome = engine
        .upload_file(&path, &dst, &hooks, Some(restored), &|s| resumed.push(s))
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.parts, 3);
    assert_eq!(outcome.reused_parts, 1);
    // Only parts 2 and 3 were sent.
    assert_eq!(throttle.total(), data.len() as u64 - 5 * MIB);
    assert_eq!(progress.done(), data.len() as u64);
    assert!(
        resumed
            .all()
            .iter()
            .all(|state| state.upload_id == paused.upload_id)
    );
    assert!(
        s3.get(&bucket, "resume.bin").await == data,
        "content differs"
    );
    assert_eq!(s3.pending_uploads(&bucket, "resume.bin").await, 0);
}

#[tokio::test]
async fn cancelled_multipart_upload_leaves_no_object_and_no_upload() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, small_parts());
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), "cancel.bin", &payload(16 * MIB as usize, 4));
    let stop = StopSignal::new();
    let checkpoint = |state: &UploadState| {
        if !state.parts.is_empty() {
            stop.cancel();
        }
    };
    let hooks = Hooks::new().with_stop(stop.clone());
    let outcome = engine
        .upload_file(
            &path,
            &uri(&bucket, "cancel.bin"),
            &hooks,
            None,
            &checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!s3.exists(&bucket, "cancel.bin").await);
    assert_eq!(
        s3.pending_uploads(&bucket, "cancel.bin").await,
        0,
        "the multipart upload was not aborted"
    );
}

#[tokio::test]
async fn cancelling_a_paused_upload_aborts_it() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, small_parts());
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), "paused.bin", &payload(11 * MIB as usize, 5));
    let dst = uri(&bucket, "paused.bin");
    let stop = StopSignal::new();
    let saved = Saved::new();
    let checkpoint = |state: &UploadState| {
        saved.push(state);
        stop.pause();
    };
    let outcome = engine
        .upload_file(
            &path,
            &dst,
            &Hooks::new().with_stop(stop.clone()),
            None,
            &checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let state = saved.last();
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 1);

    // Cancelling a stopped task: either the cleanup call, or a run that
    // starts with a cancel request.
    let cancelled = StopSignal::new();
    cancelled.cancel();
    let outcome = engine
        .upload_file(
            &path,
            &dst,
            &Hooks::new().with_stop(cancelled),
            Some(state),
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 0);
    assert!(!s3.exists(&bucket, "paused.bin").await);

    // The explicit cleanup call does the same.
    let stop = StopSignal::new();
    let saved = Saved::new();
    let checkpoint = |state: &UploadState| {
        saved.push(state);
        stop.pause();
    };
    let outcome = engine
        .upload_file(
            &path,
            &dst,
            &Hooks::new().with_stop(stop.clone()),
            None,
            &checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 1);
    engine.discard_upload(&saved.last()).await.unwrap();
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 0);
}

#[tokio::test]
async fn a_changed_file_starts_a_new_upload() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(
        &s3,
        TransferOptions {
            max_parts_in_flight: 1,
            ..small_parts()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), "changing.bin", &payload(11 * MIB as usize, 6));
    let dst = uri(&bucket, "changing.bin");
    let stop = StopSignal::new();
    let saved = Saved::new();
    let checkpoint = |state: &UploadState| {
        saved.push(state);
        if !state.parts.is_empty() {
            stop.pause();
        }
    };
    let outcome = engine
        .upload_file(
            &path,
            &dst,
            &Hooks::new().with_stop(stop.clone()),
            None,
            &checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let paused = saved.last();

    // A different size is a different fingerprint whatever the clock says.
    let changed = payload(12 * MIB as usize + 5, 7);
    fs::write(&path, &changed).unwrap();
    let resumed = Saved::new();
    let outcome = engine
        .upload_file(&path, &dst, &Hooks::new(), Some(paused.clone()), &|s| {
            resumed.push(s)
        })
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.reused_parts, 0);
    assert_eq!(outcome.parts, 3);
    assert_ne!(resumed.last().upload_id, paused.upload_id);
    assert!(s3.get(&bucket, "changing.bin").await == changed);
    assert_eq!(s3.pending_uploads(&bucket, "changing.bin").await, 0);
}

#[tokio::test]
async fn upload_errors_folder_markers_and_early_stops() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();

    let err = engine
        .upload_file(
            &dir.path().join("missing.txt"),
            &uri(&bucket, "missing.txt"),
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert!(err.to_string().contains("missing.txt"), "{err}");

    let file = write_file(dir.path(), "a.txt", b"a");
    let err = engine
        .upload_file(
            &file,
            &uri(&bucket, ""),
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");

    // A key ending in '/' is a folder marker; the local path is a folder.
    let outcome = engine
        .upload_file(
            dir.path(),
            &uri(&bucket, "photos/empty/"),
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert!(marker_exists(&s3, &bucket, "photos/empty/").await);

    // A stop requested before the start sends nothing.
    let stop = StopSignal::new();
    stop.pause();
    let outcome = engine
        .upload_file(
            &file,
            &uri(&bucket, "never.txt"),
            &Hooks::new().with_stop(stop),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    assert!(!s3.exists(&bucket, "never.txt").await);

    let bad = TransferOptions {
        part_size: MIB,
        ..TransferOptions::default()
    };
    let err = TransferEngine::new(s3.ops().clone(), bad).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

// ---- downloads ----

#[tokio::test]
async fn downloads_round_trip_with_modification_time_and_no_partial_file() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    for (index, len) in [0usize, 1, 3 * MIB as usize + 5].into_iter().enumerate() {
        let data = payload(len, 20 + index as u32);
        let key = format!("dl/object {len}.bin");
        s3.put(&bucket, &key, data.clone()).await;
        let dest = dir
            .path()
            .join("nested")
            .join("deeper")
            .join(format!("{len}.bin"));
        let throttle = Arc::new(CountingThrottle::default());
        let progress = Arc::new(ProgressCounter::new());
        let hooks = Hooks::new()
            .with_throttle(throttle.clone())
            .with_progress(progress.clone());
        let outcome = engine
            .download_file(
                &uri(&bucket, &key),
                &dest,
                &hooks,
                None,
                &no_download_checkpoint,
            )
            .await
            .unwrap()
            .into_completed()
            .unwrap();
        assert_eq!(outcome.size, len as u64);
        assert_eq!(outcome.resumed_from, 0);
        assert!(!outcome.restarted);
        assert!(fs::read(&dest).unwrap() == data, "{len}: content differs");
        assert!(!partial_path(&dest).unwrap().exists());
        assert_eq!(throttle.total(), len as u64);
        assert_eq!(progress.done(), len as u64);
        assert_eq!(progress.total(), Some(len as u64));
        let head = s3.ops().head_object(&bucket, &key, None).await.unwrap();
        let modified = head.last_modified.unwrap();
        assert_eq!(outcome.last_modified, Some(modified));
        assert_eq!(
            fs::metadata(&dest).unwrap().modified().unwrap(),
            SystemTime::from(modified)
        );
    }

    // A download replaces an existing file.
    let dest = dir.path().join("replace.bin");
    fs::write(&dest, b"old content that is longer").unwrap();
    s3.put(&bucket, "replace.bin", "new").await;
    let outcome = engine
        .download_file(
            &uri(&bucket, "replace.bin"),
            &dest,
            &Hooks::new(),
            None,
            &no_download_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(fs::read(&dest).unwrap(), b"new");
}

#[tokio::test]
async fn paused_download_resumes_to_identical_bytes() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    let data = payload(3 * MIB as usize + 17, 30);
    s3.put(&bucket, "big.bin", data.clone()).await;
    let src = uri(&bucket, "big.bin");
    let dest = dir.path().join("big.bin");

    let stop = StopSignal::new();
    let hooks = Hooks::new()
        .with_throttle(StopAfter::new(MIB, &stop, StopReason::Pause))
        .with_stop(stop.clone());
    let saved = Saved::new();
    let outcome = engine
        .download_file(&src, &dest, &hooks, None, &|s| saved.push(s))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let state = saved.last();
    assert!(
        state.offset >= MIB && state.offset < data.len() as u64,
        "{state:?}"
    );
    assert_eq!(state.size, data.len() as u64);
    assert!(state.checksum.is_some(), "{state:?}");
    let partial = partial_path(&dest).unwrap();
    assert_eq!(fs::metadata(&partial).unwrap().len(), state.offset);
    assert!(!dest.exists());

    let state: DownloadState =
        serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    let progress = Arc::new(ProgressCounter::new());
    let throttle = Arc::new(CountingThrottle::default());
    let hooks = Hooks::new()
        .with_progress(progress.clone())
        .with_throttle(throttle.clone());
    let outcome = engine
        .download_file(
            &src,
            &dest,
            &hooks,
            Some(state.clone()),
            &no_download_checkpoint,
        )
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.resumed_from, state.offset);
    assert!(!outcome.restarted);
    assert_eq!(throttle.total(), data.len() as u64 - state.offset);
    assert_eq!(progress.done(), data.len() as u64);
    assert!(fs::read(&dest).unwrap() == data, "content differs");
    assert!(!partial.exists());
}

#[tokio::test]
async fn download_starts_over_when_the_object_changed() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    let src = uri(&bucket, "changing.bin");
    // The new version has the same size (a different ETag), or is shorter
    // than the saved offset (the range no longer exists).
    for (index, new_len) in [3 * MIB as usize, 1000].into_iter().enumerate() {
        s3.put(&bucket, "changing.bin", payload(3 * MIB as usize, 40))
            .await;
        let dest = dir.path().join(format!("changing-{index}.bin"));
        let stop = StopSignal::new();
        let hooks = Hooks::new()
            .with_throttle(StopAfter::new(MIB, &stop, StopReason::Pause))
            .with_stop(stop.clone());
        let saved = Saved::new();
        let outcome = engine
            .download_file(&src, &dest, &hooks, None, &|s| saved.push(s))
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Paused);
        let state = saved.last();
        assert!(state.offset >= MIB, "{state:?}");

        let new_version = payload(new_len, 41 + index as u32);
        s3.put(&bucket, "changing.bin", new_version.clone()).await;
        let outcome = engine
            .download_file(
                &src,
                &dest,
                &Hooks::new(),
                Some(state),
                &no_download_checkpoint,
            )
            .await
            .unwrap()
            .into_completed()
            .unwrap();
        assert!(outcome.restarted, "{new_len}");
        assert_eq!(outcome.resumed_from, 0);
        assert_eq!(outcome.size, new_len as u64);
        assert!(
            fs::read(&dest).unwrap() == new_version,
            "{new_len}: content differs"
        );
        assert!(!partial_path(&dest).unwrap().exists());
    }
}

#[tokio::test]
async fn cancelled_download_deletes_the_partial_file() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    s3.put(&bucket, "c.bin", payload(3 * MIB as usize, 50))
        .await;
    let src = uri(&bucket, "c.bin");
    let dest = dir.path().join("c.bin");
    let partial = partial_path(&dest).unwrap();

    let stop = StopSignal::new();
    let hooks = Hooks::new()
        .with_throttle(StopAfter::new(MIB, &stop, StopReason::Cancel))
        .with_stop(stop.clone());
    let outcome = engine
        .download_file(&src, &dest, &hooks, None, &no_download_checkpoint)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!partial.exists());
    assert!(!dest.exists());

    // A paused download, cancelled later: by the cleanup call, or by a run
    // that starts with a cancel request.
    for use_cleanup in [true, false] {
        let stop = StopSignal::new();
        let hooks = Hooks::new()
            .with_throttle(StopAfter::new(MIB, &stop, StopReason::Pause))
            .with_stop(stop.clone());
        let saved = Saved::new();
        let outcome = engine
            .download_file(&src, &dest, &hooks, None, &|s| saved.push(s))
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Paused);
        assert!(partial.exists());
        if use_cleanup {
            engine.discard_download(&dest).await.unwrap();
        } else {
            let cancelled = StopSignal::new();
            cancelled.cancel();
            let outcome = engine
                .download_file(
                    &src,
                    &dest,
                    &Hooks::new().with_stop(cancelled),
                    Some(saved.last()),
                    &no_download_checkpoint,
                )
                .await
                .unwrap();
            assert_eq!(outcome, Outcome::Cancelled);
        }
        assert!(!partial.exists());
        assert!(!dest.exists());
    }
    // Discarding twice is fine.
    engine.discard_download(&dest).await.unwrap();
}

#[tokio::test]
async fn corrupted_partial_data_is_detected_after_a_resume() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    let data = payload(3 * MIB as usize, 60);
    s3.put(&bucket, "v.bin", data.clone()).await;
    let src = uri(&bucket, "v.bin");
    let dest = dir.path().join("v.bin");

    let stop = StopSignal::new();
    let hooks = Hooks::new()
        .with_throttle(StopAfter::new(MIB, &stop, StopReason::Pause))
        .with_stop(stop.clone());
    let saved = Saved::new();
    let outcome = engine
        .download_file(&src, &dest, &hooks, None, &|s| saved.push(s))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let state = saved.last();
    assert!(state.checksum.is_some(), "{state:?}");

    // Damage one byte of what was already downloaded.
    let partial = partial_path(&dest).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&partial).unwrap();
    file.seek(SeekFrom::Start(10)).unwrap();
    file.write_all(&[data[10] ^ 0xff]).unwrap();
    drop(file);

    let after = Saved::new();
    let err = engine
        .download_file(&src, &dest, &Hooks::new(), Some(state), &|s| after.push(s))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("checksum"), "{err}");
    assert!(!dest.exists());
    let reset = after.last();
    assert_eq!(reset.offset, 0);

    // The retry starts over and succeeds.
    let outcome = engine
        .download_file(
            &src,
            &dest,
            &Hooks::new(),
            Some(reset),
            &no_download_checkpoint,
        )
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.resumed_from, 0);
    assert!(fs::read(&dest).unwrap() == data);
}

#[tokio::test]
async fn download_errors_leave_nothing_behind() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("missing.bin");
    let err = engine
        .download_file(
            &uri(&bucket, "missing.bin"),
            &dest,
            &Hooks::new(),
            None,
            &no_download_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    assert!(!dest.exists());
    assert!(!partial_path(&dest).unwrap().exists());

    let err = engine
        .download_file(
            &uri(&bucket, "x"),
            Path::new("/"),
            &Hooks::new(),
            None,
            &no_download_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");

    // A folder marker key creates the folder.
    let folder = dir.path().join("made").join("here");
    let outcome = engine
        .download_file(
            &uri(&bucket, "some/folder/"),
            &folder,
            &Hooks::new(),
            None,
            &no_download_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert!(folder.is_dir());
}

// ---- expansion and folders ----

#[tokio::test]
async fn folder_upload_and_download_round_trip() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, small_parts());
    let local = tempfile::tempdir().unwrap();
    let root = local.path().join("photos");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a.txt", b"top level".to_vec()),
        ("2024/b c.jpg", payload(70_000, 70)),
        ("2024/trip ü/ファイル one.txt", b"unicode".to_vec()),
        (
            "2024/trip ü/deeper/big.bin",
            payload(5 * MIB as usize + 3, 71),
        ),
    ];
    for (rel, data) in &files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
    }
    fs::create_dir_all(root.join("empty")).unwrap();

    let expansion = expand_upload(vec![root.clone()], "backup/".into())
        .await
        .unwrap();
    assert!(expansion.failures.is_empty(), "{:?}", expansion.failures);
    let keys: Vec<&str> = expansion.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "backup/photos/a.txt",
            "backup/photos/2024/b c.jpg",
            "backup/photos/2024/trip ü/ファイル one.txt",
            "backup/photos/2024/trip ü/deeper/big.bin",
            "backup/photos/empty/",
        ]
    );
    for item in &expansion.items {
        upload(&engine, &item.path, &uri(&bucket, &item.key)).await;
    }
    assert!(marker_exists(&s3, &bucket, "backup/photos/empty/").await);

    let target = tempfile::tempdir().unwrap();
    let download = engine
        .expand_download(&bucket, &["backup/photos/".to_owned()], target.path())
        .await
        .unwrap();
    assert!(download.failures.is_empty(), "{:?}", download.failures);
    assert_eq!(download.items[0].key, "backup/photos/");
    assert!(download.items[0].is_folder);
    assert_eq!(download.items[0].path, target.path().join("photos"));
    for item in &download.items {
        assert!(item.path.starts_with(target.path()));
        let outcome = engine
            .download_file(
                &uri(&bucket, &item.key),
                &item.path,
                &Hooks::new(),
                None,
                &no_download_checkpoint,
            )
            .await
            .unwrap();
        assert!(outcome.is_completed(), "{item:?}");
    }
    for (rel, data) in &files {
        let path = target.path().join("photos").join(rel);
        assert!(fs::read(&path).unwrap() == *data, "{rel}: content differs");
    }

    // Selecting single objects strips their parent part.
    let single = engine
        .expand_download(
            &bucket,
            &["backup/photos/2024/b c.jpg".to_owned()],
            target.path(),
        )
        .await
        .unwrap();
    assert_eq!(single.items.len(), 1);
    assert_eq!(single.items[0].path, target.path().join("b c.jpg"));
    assert_eq!(single.items[0].size, None);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs never lists folder markers"
)]
async fn empty_folders_round_trip() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let local = tempfile::tempdir().unwrap();
    let root = local.path().join("tree");
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("nested").join("also empty")).unwrap();
    fs::write(root.join("file.txt"), "x").unwrap();
    let expansion = expand_upload(vec![root], String::new()).await.unwrap();
    for item in &expansion.items {
        upload(&engine, &item.path, &uri(&bucket, &item.key)).await;
    }
    let target = tempfile::tempdir().unwrap();
    let download = engine
        .expand_download(&bucket, &["tree/".to_owned()], target.path())
        .await
        .unwrap();
    assert!(download.failures.is_empty(), "{:?}", download.failures);
    for item in &download.items {
        let outcome = engine
            .download_file(
                &uri(&bucket, &item.key),
                &item.path,
                &Hooks::new(),
                None,
                &no_download_checkpoint,
            )
            .await
            .unwrap();
        assert!(outcome.is_completed());
    }
    assert!(target.path().join("tree").join("empty").is_dir());
    assert!(
        target
            .path()
            .join("tree")
            .join("nested")
            .join("also empty")
            .is_dir()
    );
    assert_eq!(
        fs::read(target.path().join("tree").join("file.txt")).unwrap(),
        b"x"
    );
}

fn list_response(prefix: &str, keys: &[&str]) -> MockResponse {
    let contents: String = keys
        .iter()
        .map(|key| {
            // A folder marker is an empty object; data under a key that ends
            // in '/' is refused by the download expansion.
            let size = if key.ends_with('/') { 0 } else { 3 };
            format!(
                "<Contents><Key>{key}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"e\"</ETag><Size>{size}</Size></Contents>"
            )
        })
        .collect();
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>bucket</Name><Prefix>{prefix}</Prefix><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>",
        keys.len()
    ))
}

fn unexpected(request: &RecordedRequest) -> MockResponse {
    MockResponse::error(
        400,
        "UnexpectedRequest",
        &format!(
            "unexpected {} {}?{}",
            request.method, request.path, request.query
        ),
    )
}

#[tokio::test]
async fn download_expansion_rejects_keys_that_could_escape_the_folder() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            list_response(
                "dir/",
                &[
                    "dir/",
                    "dir/ok.txt",
                    "dir/../../escape.txt",
                    "dir/sub/../../../x",
                    "dir/a\\b",
                    "dir//double",
                    "dir/./dot",
                    "dir/C:/evil",
                    "dir/sub/fine.txt",
                ],
            )
        } else {
            unexpected(request)
        }
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let out = engine
        .expand_download(
            "bucket",
            &[
                "dir/".to_owned(),
                "x/..".to_owned(),
                "../top.txt".to_owned(),
            ],
            dest.path(),
        )
        .await
        .unwrap();
    let items: Vec<(&str, PathBuf)> = out
        .items
        .iter()
        .map(|i| {
            (
                i.key.as_str(),
                i.path.strip_prefix(dest.path()).unwrap().to_path_buf(),
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            ("dir/", PathBuf::from("dir")),
            ("dir/ok.txt", Path::new("dir").join("ok.txt")),
            (
                "dir/sub/fine.txt",
                Path::new("dir").join("sub").join("fine.txt")
            ),
            // The parent part of a selected key is stripped, '..' included.
            ("../top.txt", PathBuf::from("top.txt")),
        ]
    );
    // Selected keys are checked first, then the objects of each listing.
    let rejected: Vec<&str> = out.failures.iter().map(|f| f.entry.as_str()).collect();
    assert_eq!(
        rejected,
        [
            "x/..",
            "dir/../../escape.txt",
            "dir/sub/../../../x",
            "dir/a\\b",
            "dir//double",
            "dir/./dot",
            "dir/C:/evil",
        ]
    );
    assert!(out.failures.iter().all(|f| f.code == ErrorKind::Usage));
    // Nothing was written anywhere.
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn download_expansion_rejects_selections_that_land_on_one_file() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            let prefix = request.query_param("prefix").unwrap_or_default();
            match prefix.as_str() {
                "one/" => list_response(
                    "one/",
                    &[
                        "one/x.txt",
                        "one/x.txt/inner",
                        "one/Case.txt",
                        "one/case.txt",
                    ],
                ),
                _ => list_response(&prefix, &["unexpected"]),
            }
        } else {
            unexpected(request)
        }
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let expand = |keys: &[&str]| {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        let engine = engine.clone();
        let dest = dest.path().to_path_buf();
        async move { engine.expand_download("bucket", &keys, &dest).await }
    };

    // Two selected objects with one name, or a file and a folder with one
    // name: the whole request is rejected before anything is listed.
    for selection in [
        &["a/report.txt", "b/report.txt"][..],
        &["x/a", "y/a/"][..],
        &["other/one/", "one/"][..],
    ] {
        let err = expand(selection).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict, "{selection:?}: {err}");
        let text = err.to_string();
        assert!(
            text.contains(selection[0]) && text.contains(selection[1]),
            "{text}"
        );
    }
    assert!(
        mock.requests().is_empty(),
        "a rejected request must not list anything"
    );

    // Names that differ only in case land on one file where the file
    // system ignores case.
    let mixed = expand(&["a/Report.txt", "b/report.txt"]).await;
    if cfg!(any(windows, target_os = "macos")) {
        assert_eq!(mixed.unwrap_err().kind(), ErrorKind::Conflict);
    } else {
        assert_eq!(mixed.unwrap().items.len(), 2);
    }

    // The same key twice is one entry.
    let out = expand(&["a/report.txt", "a/report.txt"]).await.unwrap();
    assert_eq!(out.items.len(), 1);

    // Inside one selected folder, objects that cannot coexist as local
    // files are per-entry failures and the rest is downloaded.
    let out = expand(&["one/"]).await.unwrap();
    let items: Vec<&str> = out.items.iter().map(|i| i.key.as_str()).collect();
    let failed: Vec<(&str, ErrorKind)> = out
        .failures
        .iter()
        .map(|f| (f.entry.as_str(), f.code))
        .collect();
    if cfg!(any(windows, target_os = "macos")) {
        assert_eq!(items, ["one/", "one/x.txt", "one/Case.txt"]);
        assert_eq!(
            failed,
            [
                ("one/x.txt/inner", ErrorKind::Conflict),
                ("one/case.txt", ErrorKind::Conflict)
            ]
        );
    } else {
        assert_eq!(items, ["one/", "one/x.txt", "one/Case.txt", "one/case.txt"]);
        assert_eq!(failed, [("one/x.txt/inner", ErrorKind::Conflict)]);
    }
}

#[tokio::test]
async fn copy_and_move_expansion_follow_the_folder_semantics() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let other = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    for key in ["photos/2024/a.jpg", "photos/2024/sub/b.jpg", "photos/x.txt"] {
        s3.put(&bucket, key, key).await;
    }
    let keys = |list: &[&str]| list.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>();
    let pairs =
        |out: &bucketbridge_core::engine::Expansion<bucketbridge_core::engine::CopyItem>| {
            out.items
                .iter()
                .map(|i| (i.src_key.clone(), i.dst_key.clone()))
                .collect::<Vec<_>>()
        };

    let out = engine
        .expand_copy(
            &bucket,
            &keys(&["photos/2024/", "photos/x.txt"]),
            &other,
            "backup/",
        )
        .await
        .unwrap();
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    assert_eq!(
        pairs(&out),
        [
            (
                "photos/2024/a.jpg".to_owned(),
                "backup/2024/a.jpg".to_owned()
            ),
            (
                "photos/2024/sub/b.jpg".to_owned(),
                "backup/2024/sub/b.jpg".to_owned()
            ),
            ("photos/x.txt".to_owned(), "backup/x.txt".to_owned()),
        ]
    );
    assert_eq!(out.items[0].size, Some(17));
    assert_eq!(out.items[2].size, None);
    for item in &out.items {
        let outcome = engine
            .copy(
                &uri(&bucket, &item.src_key),
                &uri(&other, &item.dst_key),
                &Hooks::new(),
            )
            .await
            .unwrap();
        assert!(outcome.is_completed());
    }
    assert_eq!(
        s3.get(&other, "backup/2024/sub/b.jpg").await,
        b"photos/2024/sub/b.jpg"
    );

    // Onto itself, and into itself for a move.
    let out = engine
        .expand_copy(&bucket, &keys(&["photos/x.txt"]), &bucket, "photos/")
        .await
        .unwrap();
    assert!(out.items.is_empty());
    assert_eq!(out.failures[0].code, ErrorKind::Usage);
    let out = engine
        .expand_move(&bucket, &keys(&["photos/"]), &bucket, "photos/backup/")
        .await
        .unwrap();
    assert!(out.items.is_empty());
    assert!(
        out.failures[0].message.contains("into itself"),
        "{:?}",
        out.failures
    );
    let out = engine
        .expand_move(&bucket, &keys(&["photos/2024/"]), &bucket, "photos/")
        .await
        .unwrap();
    assert!(out.items.is_empty());
    assert!(
        out.failures[0].message.contains("onto itself"),
        "{:?}",
        out.failures
    );
    // Copying a folder into a sub-folder of itself is allowed; the listing
    // is taken first, so it ends.
    let out = engine
        .expand_copy(
            &bucket,
            &keys(&["photos/2024/"]),
            &bucket,
            "photos/2024/copy/",
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 2);
    assert!(
        out.items
            .iter()
            .all(|i| i.dst_key.starts_with("photos/2024/copy/2024/"))
    );
    // A move to another bucket under the same prefix is not "into itself".
    let out = engine
        .expand_move(&bucket, &keys(&["photos/"]), &other, "photos/")
        .await
        .unwrap();
    assert_eq!(out.items.len(), 3);
    // A failed listing fails that selection only.
    let out = engine
        .expand_copy("no-such-bucket", &keys(&["p/", "q.txt"]), &bucket, "")
        .await
        .unwrap();
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.failures.len(), 1);
    assert_eq!(out.failures[0].code, ErrorKind::NotFound);

    // Two selected sources with one name would land on one destination:
    // the request is rejected and produces nothing (a file and a folder
    // count too).
    for selection in [
        &["photos/x.txt", "photos/2024/x.txt"][..],
        &["photos/2024/", "elsewhere/2024"][..],
    ] {
        for is_move in [false, true] {
            let result = if is_move {
                engine
                    .expand_move(&bucket, &keys(selection), &other, "dst/")
                    .await
            } else {
                engine
                    .expand_copy(&bucket, &keys(selection), &other, "dst/")
                    .await
            };
            let err = result.unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Conflict, "{selection:?}: {err}");
            assert!(err.to_string().contains(selection[1]), "{err}");
        }
    }
    // Keys that differ only in case are different objects.
    let out = engine
        .expand_copy(
            &bucket,
            &keys(&["photos/x.txt", "other/X.txt"]),
            &other,
            "dst/",
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 2);
    // The same key twice is one entry.
    let out = engine
        .expand_move(
            &bucket,
            &keys(&["photos/x.txt", "photos/x.txt"]),
            &other,
            "",
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 1);
    // An object can only be moved once: a key inside another selected
    // folder is refused (copying it twice is fine).
    let nested = keys(&["photos/", "photos/2024/a.jpg"]);
    let err = engine
        .expand_move(&bucket, &nested, &other, "dst/")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
    let out = engine
        .expand_copy(&bucket, &nested, &other, "dst/")
        .await
        .unwrap();
    assert_eq!(out.items.len(), 4);
}

// ---- copy, move, rename, delete ----

#[tokio::test]
async fn copy_and_move_objects() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let data = payload(100_000, 80);
    s3.put(&bucket, "src/a.bin", data.clone()).await;

    let progress = Arc::new(ProgressCounter::new());
    let outcome = engine
        .copy(
            &uri(&bucket, "src/a.bin"),
            &uri(&bucket, "copy/a.bin"),
            &Hooks::new().with_progress(progress.clone()),
        )
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.size, data.len() as u64);
    assert_eq!(progress.total(), Some(data.len() as u64));
    assert_eq!(progress.done(), data.len() as u64);
    assert!(s3.get(&bucket, "copy/a.bin").await == data);

    let outcome = engine
        .move_object(
            &uri(&bucket, "copy/a.bin"),
            &uri(&bucket, "moved/a.bin"),
            &Hooks::new(),
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert!(!s3.exists(&bucket, "copy/a.bin").await);
    assert!(s3.get(&bucket, "moved/a.bin").await == data);

    // When the copy fails, the source stays.
    let err = engine
        .move_object(
            &uri(&bucket, "src/a.bin"),
            &uri("no-such-bucket", "a.bin"),
            &Hooks::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    assert!(s3.exists(&bucket, "src/a.bin").await);

    let err = engine
        .copy(
            &uri(&bucket, "src/a.bin"),
            &uri(&bucket, "src/a.bin"),
            &Hooks::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let err = engine
        .copy(&uri(&bucket, "missing"), &uri(&bucket, "x"), &Hooks::new())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // Folder markers are copied as markers.
    s3.ops().create_folder(&bucket, "folder/").await.unwrap();
    let outcome = engine
        .move_object(
            &uri(&bucket, "folder/"),
            &uri(&bucket, "renamed/"),
            &Hooks::new(),
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert!(marker_exists(&s3, &bucket, "renamed/").await);
    assert!(!marker_exists(&s3, &bucket, "folder/").await);

    // A stop before the start copies nothing.
    let stop = StopSignal::new();
    stop.cancel();
    let outcome = engine
        .move_object(
            &uri(&bucket, "src/a.bin"),
            &uri(&bucket, "never.bin"),
            &Hooks::new().with_stop(stop),
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!s3.exists(&bucket, "never.bin").await);
    assert!(s3.exists(&bucket, "src/a.bin").await);
}

#[tokio::test]
async fn move_reports_that_the_copy_exists_when_the_source_cannot_be_deleted() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" if request.path == "/bucket/src.txt" => MockResponse::empty(200)
            .with_header("etag", "\"abc\"")
            .with_header("last-modified", "Thu, 29 Oct 2026 08:15:00 GMT"),
        "PUT"
            if request.path == "/bucket/dst.txt"
                && request.header("x-amz-copy-source").is_some() =>
        {
            MockResponse::xml(format!(
                "<CopyObjectResult {NS}><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"abc\"</ETag></CopyObjectResult>"
            ))
        }
        "DELETE" if request.path == "/bucket/src.txt" => {
            MockResponse::error(403, "AccessDenied", "Access Denied")
        }
        _ => unexpected(request),
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let err = engine
        .move_object(
            &uri("bucket", "src.txt"),
            &uri("bucket", "dst.txt"),
            &Hooks::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth, "{err}");
    let text = err.to_string();
    assert!(
        text.contains("the copy s3://bucket/dst.txt exists") && text.contains("still there"),
        "{text}"
    );
    let methods: Vec<String> = mock.requests().iter().map(|r| r.method.clone()).collect();
    // The delete comes strictly after the copy.
    assert_eq!(methods.last().map(String::as_str), Some("DELETE"));
    let put = methods.iter().position(|m| m == "PUT").unwrap();
    assert_eq!(put, methods.len() - 2);
}

#[tokio::test]
async fn rename_checks_conflicts_and_moves() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    for key in [
        "docs/a.txt",
        "docs/sub/b.txt",
        "other/c.txt",
        "solo.txt",
        "taken.txt",
    ] {
        s3.put(&bucket, key, key).await;
    }
    let move_all = |items: Vec<bucketbridge_core::engine::CopyItem>| {
        let engine = engine.clone();
        let bucket = bucket.clone();
        async move {
            for item in items {
                let outcome = engine
                    .move_object(
                        &uri(&bucket, &item.src_key),
                        &uri(&bucket, &item.dst_key),
                        &Hooks::new(),
                    )
                    .await
                    .unwrap();
                assert!(outcome.is_completed());
            }
        }
    };

    let err = engine
        .plan_rename(&bucket, "solo.txt", "taken.txt")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    let err = engine
        .plan_rename(&bucket, "docs/", "other/")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    for (from, to) in [
        ("docs/", "docs/inner/"),
        ("docs/", "docs2"),
        ("solo.txt", "solo/"),
        ("solo.txt", "solo.txt"),
        ("docs/", "/"),
    ] {
        let err = engine.plan_rename(&bucket, from, to).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{from} -> {to}: {err}");
    }
    let err = engine
        .plan_rename(&bucket, "missing.txt", "x.txt")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    let err = engine
        .plan_rename(&bucket, "nothing/", "x/")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");

    let items = engine
        .plan_rename(&bucket, "solo.txt", "renamed.txt")
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].size, Some(8));
    move_all(items).await;
    assert!(!s3.exists(&bucket, "solo.txt").await);
    assert_eq!(s3.get(&bucket, "renamed.txt").await, b"solo.txt");

    let items = engine
        .plan_rename(&bucket, "docs/", "archive/")
        .await
        .unwrap();
    let targets: Vec<&str> = items.iter().map(|i| i.dst_key.as_str()).collect();
    assert_eq!(targets, ["archive/a.txt", "archive/sub/b.txt"]);
    move_all(items).await;
    assert!(
        s3.ops()
            .list_all(&bucket, "docs/")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s3.get(&bucket, "archive/sub/b.txt").await,
        b"docs/sub/b.txt"
    );
}

#[tokio::test]
async fn delete_objects_and_prefixes_with_progress() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = engine(&s3, TransferOptions::default());
    let bulk: Vec<String> = (0..1001).map(|i| format!("bulk/item-{i:04}")).collect();
    futures::stream::iter(bulk)
        .for_each_concurrent(32, |key| {
            let s3 = &s3;
            let bucket = &bucket;
            async move { s3.put(bucket, &key, "x").await }
        })
        .await;
    s3.put(&bucket, "bulky.txt", "sibling").await;
    s3.put(&bucket, "single.txt", "x").await;

    let progress = Arc::new(ProgressCounter::new());
    let outcome = engine
        .delete(
            &uri(&bucket, "bulk/"),
            &Hooks::new().with_progress(progress.clone()),
        )
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.deleted, 1001);
    assert_eq!(progress.total(), Some(1001));
    assert_eq!(progress.done(), 1001);
    assert!(
        s3.ops()
            .list_all(&bucket, "bulk/")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(s3.exists(&bucket, "bulky.txt").await);

    let progress = Arc::new(ProgressCounter::new());
    let outcome = engine
        .delete(
            &uri(&bucket, "single.txt"),
            &Hooks::new().with_progress(progress.clone()),
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(progress.done(), 1);
    assert!(!s3.exists(&bucket, "single.txt").await);

    for refused in ["", "/", "//"] {
        let err = engine
            .delete(&uri(&bucket, refused), &Hooks::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{refused:?}");
    }
    assert!(s3.exists(&bucket, "bulky.txt").await);

    let stop = StopSignal::new();
    stop.pause();
    let outcome = engine
        .delete(&uri(&bucket, "bulky.txt"), &Hooks::new().with_stop(stop))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    assert!(s3.exists(&bucket, "bulky.txt").await);
}

#[tokio::test]
async fn delete_reports_keys_the_server_refused() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            list_response("logs/", &["logs/a", "logs/locked", "logs/b"])
        } else if request.method == "POST" && request.query_param("delete").is_some() {
            MockResponse::xml(format!(
                "<DeleteResult {NS}><Error><Key>logs/locked</Key><Code>AccessDenied</Code><Message>Access Denied</Message></Error></DeleteResult>"
            ))
        } else {
            unexpected(request)
        }
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let progress = Arc::new(ProgressCounter::new());
    let err = engine
        .delete(
            &uri("bucket", "logs/"),
            &Hooks::new().with_progress(progress.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth, "{err}");
    assert!(err.to_string().contains("1 of 3 objects"), "{err}");
    assert!(
        err.to_string().contains("logs/locked (AccessDenied"),
        "{err}"
    );
    assert_eq!(progress.done(), 2);
}

// ---- request-level contracts ----

#[tokio::test]
async fn uploads_send_explicit_checksums_plain_bodies_and_the_payer_header() {
    let mock = MockS3::start(|request| {
        let query = |name: &str| request.query_param(name);
        match request.method.as_str() {
            "POST" if query("uploads").is_some() => MockResponse::xml(format!(
                "<InitiateMultipartUploadResult {NS}><Bucket>bucket</Bucket><Key>big.txt</Key><UploadId>upload-1</UploadId></InitiateMultipartUploadResult>"
            )),
            "PUT" if query("partNumber").is_some() => MockResponse::empty(200).with_header(
                "etag",
                &format!("\"part-{}\"", query("partNumber").unwrap()),
            ),
            "POST" if query("uploadId").is_some() => MockResponse::xml(format!(
                "<CompleteMultipartUploadResult {NS}><Bucket>bucket</Bucket><Key>big.txt</Key><ETag>\"whole-2\"</ETag></CompleteMultipartUploadResult>"
            )),
            "PUT" => MockResponse::empty(200).with_header("etag", "\"single\""),
            _ => unexpected(request),
        }
    });
    let options = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let small = payload(3000, 90);
    let big = payload(5 * MIB as usize + 1024, 91);
    let small_path = write_file(dir.path(), "small.txt", &small);
    let big_path = write_file(dir.path(), "big.txt", &big);

    for algorithm in [ChecksumAlgorithm::Crc32c, ChecksumAlgorithm::None] {
        let engine = TransferEngine::new(
            mock.ops_with(&options, true),
            TransferOptions {
                checksum: algorithm,
                ..small_parts()
            },
        )
        .unwrap();
        let before = mock.requests().len();
        upload(&engine, &small_path, &uri("bucket", "small.txt")).await;
        upload(&engine, &big_path, &uri("bucket", "big.txt")).await;
        let requests = mock.requests().split_off(before);
        assert_eq!(requests.len(), 5, "{requests:?}");
        for request in &requests {
            assert_eq!(request.header("x-amz-request-payer"), Some("requester"));
            assert!(request.header("x-amz-trailer").is_none());
            assert!(
                request
                    .header("content-encoding")
                    .is_none_or(|e| !e.contains("aws-chunked")),
                "{:?}",
                request.headers
            );
        }
        let checksum_headers = |request: &RecordedRequest| {
            request
                .headers
                .iter()
                .filter(|(name, _)| {
                    name.starts_with("x-amz-checksum-") && name != "x-amz-checksum-type"
                })
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Vec<_>>()
        };
        let header = "x-amz-checksum-crc32c".to_owned();
        let expected = |data: &[u8]| match algorithm {
            ChecksumAlgorithm::None => Vec::new(),
            _ => vec![(header.clone(), checksum_of(algorithm, data).unwrap())],
        };

        // The single upload: the raw file as the body, with its checksum.
        let single = &requests[0];
        assert_eq!(single.method, "PUT");
        assert_eq!(single.header("content-type"), Some("text/plain"));
        assert!(single.body == small, "the body was not sent as is");
        assert_eq!(checksum_headers(single), expected(&small));

        // The multipart upload: created with the algorithm, each part
        // with its checksum, and both checksums in the completion.
        let create = &requests[1];
        assert_eq!(create.method, "POST");
        assert_eq!(create.header("content-type"), Some("text/plain"));
        assert_eq!(
            create.header("x-amz-checksum-algorithm"),
            (algorithm == ChecksumAlgorithm::Crc32c).then_some("CRC32C")
        );
        let mut parts: Vec<&RecordedRequest> = requests[2..4].iter().collect();
        parts.sort_by_key(|r| r.query_param("partNumber"));
        let ranges = [0..5 * MIB as usize, 5 * MIB as usize..big.len()];
        for (part, range) in parts.iter().zip(ranges) {
            assert!(part.body == big[range.clone()], "part body differs");
            assert_eq!(checksum_headers(part), expected(&big[range]));
        }
        let complete = requests[4].body_text();
        assert!(
            complete.contains("<PartNumber>1</PartNumber>")
                && complete.contains("<PartNumber>2</PartNumber>"),
            "{complete}"
        );
        match algorithm {
            ChecksumAlgorithm::None => assert!(!complete.contains("Checksum"), "{complete}"),
            _ => {
                for range in [0..5 * MIB as usize, 5 * MIB as usize..big.len()] {
                    let value = checksum_of(algorithm, &big[range]).unwrap();
                    assert!(
                        complete.contains(&format!("<ChecksumCRC32C>{value}</ChecksumCRC32C>")),
                        "{complete}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn resumed_download_asks_for_the_rest_with_if_match_and_restarts_on_412() {
    let mock = MockS3::start(|request| {
        if request.method != "GET" {
            return unexpected(request);
        }
        let ranged = request.header("range").map(str::to_owned);
        match (request.path.as_str(), ranged.as_deref()) {
            ("/bucket/same.txt", Some("bytes=5-")) => MockResponse {
                status: 206,
                headers: vec![
                    ("etag".into(), "\"v1\"".into()),
                    ("content-range".into(), "bytes 5-10/11".into()),
                    (
                        "last-modified".into(),
                        "Thu, 29 Oct 2026 08:15:00 GMT".into(),
                    ),
                ],
                body: " world".into(),
            },
            ("/bucket/changed.txt", Some(_)) => MockResponse::error(
                412,
                "PreconditionFailed",
                "At least one of the pre-conditions you specified did not hold",
            ),
            ("/bucket/changed.txt", None) => MockResponse {
                status: 200,
                headers: vec![
                    ("etag".into(), "\"v2\"".into()),
                    (
                        "last-modified".into(),
                        "Thu, 29 Oct 2026 08:15:00 GMT".into(),
                    ),
                ],
                body: "brand new content".into(),
            },
            _ => unexpected(request),
        }
    });
    let options = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    let engine =
        TransferEngine::new(mock.ops_with(&options, true), TransferOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();

    for (key, expected, restarted) in [
        ("same.txt", &b"hello world"[..], false),
        ("changed.txt", &b"brand new content"[..], true),
    ] {
        let dest = dir.path().join(key);
        fs::write(partial_path(&dest).unwrap(), b"hello").unwrap();
        let state = DownloadState {
            bucket: "bucket".into(),
            key: key.into(),
            etag: "\"v1\"".into(),
            size: 11,
            last_modified: None,
            checksum: None,
            offset: 5,
        };
        let before = mock.requests().len();
        let outcome = engine
            .download_file(
                &uri("bucket", key),
                &dest,
                &Hooks::new(),
                Some(state),
                &no_download_checkpoint,
            )
            .await
            .unwrap()
            .into_completed()
            .unwrap();
        assert_eq!(outcome.restarted, restarted, "{key}");
        assert_eq!(fs::read(&dest).unwrap(), expected, "{key}");
        let requests = mock.requests().split_off(before);
        let first = &requests[0];
        assert_eq!(first.header("range"), Some("bytes=5-"));
        assert_eq!(first.header("if-match"), Some("\"v1\""));
        assert_eq!(first.header("x-amz-request-payer"), Some("requester"));
        assert_eq!(first.header("x-amz-checksum-mode"), Some("ENABLED"));
        if restarted {
            assert_eq!(requests.len(), 2);
            assert!(requests[1].header("range").is_none());
            assert!(requests[1].header("if-match").is_none());
        } else {
            assert_eq!(requests.len(), 1);
        }
    }
}

// ---- 5 GiB round trip ----

/// splitmix64, used to generate content that is cheap to recompute at any
/// offset.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Fills `buf` with the generated bytes at `offset`.
fn generate(mut offset: u64, buf: &mut [u8]) {
    let mut at = 0;
    while at < buf.len() {
        let block = mix(offset / 8).to_le_bytes();
        let start = (offset % 8) as usize;
        let len = (8 - start).min(buf.len() - at);
        buf[at..at + len].copy_from_slice(&block[start..start + len]);
        at += len;
        offset += len as u64;
    }
}

/// Generated upload content: nothing is stored locally.
#[derive(Debug)]
struct Generated {
    size: u64,
}

impl UploadSource for Generated {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset + buf.len() as u64 > self.size {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past the end"));
        }
        generate(offset, buf);
        Ok(())
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint {
            size: self.size,
            modified: None,
        }
    }

    fn is_unchanged(&self) -> io::Result<bool> {
        Ok(true)
    }

    fn describe(&self) -> String {
        "generated data".to_owned()
    }
}

/// A download sink that compares every byte with the generated content.
#[derive(Default)]
struct Verifying {
    position: u64,
    first_mismatch: Option<u64>,
    expected: Vec<u8>,
}

impl DownloadSink for Verifying {
    fn prepare(&mut self, offset: u64) -> io::Result<u64> {
        self.position = offset;
        Ok(offset)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.expected.resize(data.len(), 0);
        generate(self.position, &mut self.expected);
        if self.first_mismatch.is_none() && self.expected != data {
            let at = self.expected.iter().zip(data).position(|(a, b)| a != b);
            self.first_mismatch = Some(self.position + at.unwrap_or(0) as u64);
        }
        self.position += data.len() as u64;
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn describe(&self) -> String {
        "the verifying sink".to_owned()
    }
}

#[test]
fn generated_content_is_position_independent() {
    let mut whole = vec![0u8; 100];
    generate(3, &mut whole);
    let mut part = vec![0u8; 50];
    generate(40, &mut part);
    assert_eq!(part, whole[37..87]);
    let source = Generated { size: 64 };
    let mut buf = [0u8; 8];
    assert!(source.read_exact_at(60, &mut buf).is_err());
    source.read_exact_at(56, &mut buf).unwrap();
    let mut sink = Verifying::default();
    sink.prepare(56).unwrap();
    sink.write_all(&buf).unwrap();
    assert_eq!(sink.first_mismatch, None);
    sink.write_all(&[0, 0, 0]).unwrap();
    assert!(sink.first_mismatch.is_some());
}

/// A 5 GiB object round-trips via multipart.
/// The data is generated and checked as a stream; nothing is stored
/// locally. The upload pauses after 1010 of its 1024 parts, so resuming has
/// to read the part list across two pages; the download pauses half-way and
/// resumes with a ranged request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO and BB_TEST_LARGE=1: moves 5 GiB in each direction"
)]
async fn five_gib_object_round_trips_via_multipart() {
    if std::env::var("BB_TEST_LARGE").as_deref() != Ok("1") {
        eprintln!("skipped: set BB_TEST_LARGE=1 to run the 5 GiB round trip");
        return;
    }
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    let size = 5 * 1024 * MIB;
    let engine = engine(
        &s3,
        TransferOptions {
            multipart_threshold: 8 * MIB,
            part_size: 5 * MIB,
            max_parts_in_flight: 8,
            ..TransferOptions::default()
        },
    );
    let dst = uri(&bucket, "large/5gib.bin");
    let source: Arc<dyn UploadSource> = Arc::new(Generated { size });

    let stop = StopSignal::new();
    let saved = Saved::new();
    let checkpoint = |state: &UploadState| {
        saved.push(state);
        if state.parts.len() >= 1010 {
            stop.pause();
        }
    };
    let outcome = engine
        .upload(
            Arc::clone(&source),
            &dst,
            None,
            &Hooks::new().with_stop(stop.clone()),
            None,
            &checkpoint,
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let paused = saved.last();
    drop(saved);
    assert!(paused.parts.len() >= 1010);

    let progress = Arc::new(ProgressCounter::new());
    let outcome = engine
        .upload(
            source,
            &dst,
            None,
            &Hooks::new().with_progress(progress.clone()),
            Some(paused.clone()),
            &no_upload_checkpoint,
        )
        .await
        .unwrap()
        .into_completed()
        .unwrap();
    assert_eq!(outcome.parts, 1024);
    assert!(outcome.reused_parts as usize >= paused.parts.len());
    assert_eq!(progress.done(), size);
    assert_eq!(s3.pending_uploads(&bucket, dst.key()).await, 0);
    let head = s3
        .ops()
        .head_object(&bucket, dst.key(), None)
        .await
        .unwrap();
    assert_eq!(head.size, size);
    assert!(
        head.etag.as_deref().is_some_and(|e| e.ends_with("-1024")),
        "{head:?}"
    );

    let stop = StopSignal::new();
    let saved = Saved::new();
    let hooks = Hooks::new()
        .with_throttle(StopAfter::new(size / 2, &stop, StopReason::Pause))
        .with_stop(stop.clone());
    let (outcome, sink) = engine
        .download_to(&dst, Verifying::default(), &hooks, None, &|s| saved.push(s))
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Paused);
    let state = saved.last();
    assert!(state.offset >= size / 2 && state.offset < size, "{state:?}");
    let (outcome, sink) = engine
        .download_to(
            &dst,
            sink,
            &Hooks::new(),
            Some(state.clone()),
            &no_download_checkpoint,
        )
        .await
        .unwrap();
    let done = outcome.into_completed().unwrap();
    assert_eq!(done.resumed_from, state.offset);
    assert_eq!(done.size, size);
    assert_eq!(sink.position, size);
    assert_eq!(sink.first_mismatch, None);
}
