//! Regression tests for bugs an independent review found in the transfer
//! engines and the transfer queue. Each test fails on the code as it was
//! reviewed and states the contract that holds now.
//!
//! * Partial download files: `<dest>.bbpart` against objects whose key ends
//!   in `.bbpart`; links planted under that name.
//! * A cancel whose cleanup fails; the multipart upload ID reaching the disk.
//! * Folder keys (`dir/`) that hold data; a single `PutObject` of a file that
//!   changes while it is read.
//! * Stopping a download while its partial file is read back; the cleanup of
//!   a partial file that a dropped run still holds.
//! * The number of bodies that stream at once under a bandwidth cap.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bucketbridge_core::engine::checksum::{ObjectChecksum, checksum_of};
use bucketbridge_core::engine::{
    DownloadSink, DownloadState, FileSink, Hooks, Outcome, Progress, S3Provider, S3Runner,
    SourceFingerprint, StopSignal, StreamFuture, StreamPermit, Throttle, ThrottleFuture,
    TransferEngine, TransferOptions, UploadSource, UploadState, enqueue_upload, is_partial_name,
    partial_path,
};
use bucketbridge_core::model::TransferState;
use bucketbridge_core::settings::{
    ChecksumAlgorithm, DataIntegritySettings, MIB, TransferSettings,
};
use bucketbridge_core::transfer::{QueueOptions, TransferQueue, is_retryable};
use bucketbridge_core::{ClientOptions, CopyOptions, ErrorKind, S3, S3Uri};
use bucketbridge_tests::{MockResponse, MockS3, TestS3};
use tokio::sync::{Semaphore, watch};

// ---- helpers ----

fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[0] ^ seed)
        .collect()
}

fn uri(bucket: &str, key: &str) -> S3Uri {
    S3Uri::new(bucket, key).unwrap()
}

fn no_checkpoint(_: &DownloadState) {}

fn engine_for(s3: &S3) -> TransferEngine {
    TransferEngine::new(s3.clone(), TransferOptions::default()).unwrap()
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Every file below `root`, as `/`-separated paths relative to it.
fn files_below(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, found);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

/// Holds the first throttle request until released; later ones pass. A
/// download throttles after its first piece arrived, so while it is held the
/// partial file exists and is locked.
struct HoldFirst {
    calls: AtomicU64,
    reached: watch::Sender<bool>,
    release: watch::Receiver<bool>,
}

impl HoldFirst {
    fn new() -> (Arc<Self>, watch::Receiver<bool>, watch::Sender<bool>) {
        let (reached_tx, reached_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let gate = Arc::new(HoldFirst {
            calls: AtomicU64::new(0),
            reached: reached_tx,
            release: release_rx,
        });
        (gate, reached_rx, release_tx)
    }
}

impl Throttle for HoldFirst {
    fn acquire(&self, _bytes: u64) -> ThrottleFuture<'_> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            let _ = self.reached.send(true);
        }
        let mut release = self.release.clone();
        Box::pin(async move {
            if first {
                let _ = release.wait_for(|open| *open).await;
            }
        })
    }
}

// ---- partial download files: names ending in `.bbpart` are reserved ----

async fn bucket_with_a_and_a_bbpart() -> (TestS3, String, Vec<u8>) {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let a = payload(300 * 1024, 0x11);
    s3.put(&bucket, "dir/a", a.clone()).await;
    // What a folder upload of an interrupted download used to leave behind.
    s3.put(&bucket, "dir/a.bbpart", payload(120 * 1024, 0x77))
        .await;
    (s3, bucket, a)
}

/// The expansion used to accept `dir/a` and `dir/a.bbpart` together: the
/// final file of the second is the partial file of the first, so one
/// download destroyed the other's data (or swapped content) and both tasks
/// reported success. Names ending in `.bbpart` in any letter case are
/// reserved now: such an object is a failed entry, everything else is
/// downloaded, and a partial file can never be another object's file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn objects_named_like_partial_files_are_failed_entries_not_collisions() {
    let (s3, bucket, a) = bucket_with_a_and_a_bbpart().await;
    s3.put(&bucket, "dir/B.BBPART", "upper case").await;
    s3.put(&bucket, "dir/sub/c.bbPart", "mixed case").await;
    let d = payload(2048, 0x33);
    s3.put(&bucket, "dir/sub/d", d.clone()).await;
    let engine = engine_for(s3.ops());
    let dest = tempfile::tempdir().unwrap();

    let expansion = engine
        .expand_download(&bucket, &["dir/".to_owned()], dest.path())
        .await
        .unwrap();
    let files: Vec<&str> = expansion
        .items
        .iter()
        .filter(|item| !item.is_folder)
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(files, ["dir/a", "dir/sub/d"]);
    let mut failed: Vec<(&str, ErrorKind)> = expansion
        .failures
        .iter()
        .map(|failure| (failure.entry.as_str(), failure.code))
        .collect();
    failed.sort_unstable_by_key(|(entry, _)| *entry);
    assert_eq!(
        failed,
        [
            ("dir/B.BBPART", ErrorKind::Usage),
            ("dir/a.bbpart", ErrorKind::Usage),
            ("dir/sub/c.bbPart", ErrorKind::Usage),
        ]
    );
    for failure in &expansion.failures {
        assert!(
            failure.message.contains("reserved") && failure.message.contains(".bbpart"),
            "{}",
            failure.message
        );
    }

    // Everything that was planned downloads, in any order, at once; nothing
    // is destroyed and no partial file is left.
    let engine = Arc::new(engine);
    let mut running = Vec::new();
    for item in expansion.items.into_iter().filter(|item| !item.is_folder) {
        let engine = Arc::clone(&engine);
        let src = uri(&bucket, &item.key);
        running.push(tokio::spawn(async move {
            engine
                .download_file(&src, &item.path, &Hooks::new(), None, &no_checkpoint)
                .await
        }));
    }
    for download in running {
        assert!(download.await.unwrap().unwrap().is_completed());
    }
    assert_eq!(fs::read(dest.path().join("dir/a")).unwrap(), a);
    assert_eq!(fs::read(dest.path().join("dir/sub/d")).unwrap(), d);
    assert_eq!(files_below(dest.path()), ["dir/a", "dir/sub/d"]);

    // Selected one by one, the reserved key fails alone: it takes no
    // destination name and does not reject the request.
    let other = tempfile::tempdir().unwrap();
    let expansion = engine
        .expand_download(
            &bucket,
            &["dir/a.bbpart".to_owned(), "dir/a".to_owned()],
            other.path(),
        )
        .await
        .unwrap();
    assert_eq!(expansion.items.len(), 1);
    assert_eq!(expansion.items[0].key, "dir/a");
    assert_eq!(expansion.failures.len(), 1);
    assert_eq!(expansion.failures[0].entry, "dir/a.bbpart");
    assert_eq!(expansion.failures[0].code, ErrorKind::Usage);
}

/// The engine does not trust its callers (the CLI downloads to a path it
/// was given, without an expansion): a destination with a reserved name is
/// refused, and whatever lies there is left alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_to_a_reserved_name_is_refused_and_touches_nothing() {
    let (s3, bucket, _a) = bucket_with_a_and_a_bbpart().await;
    let engine = engine_for(s3.ops());
    let dest = tempfile::tempdir().unwrap();
    let reserved = dest.path().join("a.bbpart");
    fs::write(&reserved, "somebody's file").unwrap();

    for name in ["a.bbpart", "A.BBPART"] {
        let err = engine
            .download_file(
                &uri(&bucket, "dir/a.bbpart"),
                &dest.path().join(name),
                &Hooks::new(),
                None,
                &no_checkpoint,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
        assert!(err.to_string().contains("reserved"), "{err}");
    }
    assert_eq!(fs::read(&reserved).unwrap(), b"somebody's file");
    assert_eq!(files_below(dest.path()), ["a.bbpart"]);
    assert!(is_partial_name("x.BBPART") && !is_partial_name("x.bbpart.txt"));
}

/// Two downloads of one destination used to share the partial file when the
/// second came after the first had dropped its lock but before the rename,
/// and the content of one ended up in the file of the other. While a
/// download runs, the partial file stays its own: a second download of the
/// destination is refused with a kind the queue retries, and the first
/// completes with its own bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_download_of_a_destination_waits_and_never_shares_the_partial_file() {
    let (s3, bucket, a) = bucket_with_a_and_a_bbpart().await;
    let engine = engine_for(s3.ops());
    let dest = tempfile::tempdir().unwrap();
    let file_a = dest.path().join("a");
    let partial = partial_path(&file_a).unwrap();

    let (gate, mut reached, release) = HoldFirst::new();
    let running = {
        let engine = engine.clone();
        let src = uri(&bucket, "dir/a");
        let dest = file_a.clone();
        let hooks = Hooks::new().with_throttle(gate);
        tokio::spawn(async move {
            engine
                .download_file(&src, &dest, &hooks, None, &no_checkpoint)
                .await
        })
    };
    reached.wait_for(|reached| *reached).await.unwrap();
    assert!(partial.exists(), "the partial file of dir/a exists");

    let err = engine
        .download_file(
            &uri(&bucket, "dir/a"),
            &file_a,
            &Hooks::new(),
            None,
            &no_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert!(is_retryable(err.kind()), "the queue must retry {err}");
    assert!(err.to_string().contains("another download"), "{err}");
    assert!(!file_a.exists(), "nothing was renamed into place");

    release.send(true).unwrap();
    assert!(running.await.unwrap().unwrap().is_completed());
    assert_eq!(fs::read(&file_a).unwrap(), a);
    assert!(!partial.exists());
    assert_eq!(files_below(dest.path()), ["a"]);
}

/// The cleanup of a cancelled download of `dir/a` deleted the completed
/// file of `dir/a.bbpart`. It removes the partial file and nothing else:
/// not a finished file of the same destination, and not a planted link's
/// target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cleanup_of_a_cancelled_download_removes_only_its_partial_file() {
    let (s3, bucket, a) = bucket_with_a_and_a_bbpart().await;
    let engine = engine_for(s3.ops());
    let dest = tempfile::tempdir().unwrap();
    let file_a = dest.path().join("a");
    assert!(
        engine
            .download_file(
                &uri(&bucket, "dir/a"),
                &file_a,
                &Hooks::new(),
                None,
                &no_checkpoint
            )
            .await
            .unwrap()
            .is_completed()
    );

    // A second task for the same destination is cancelled: its cleanup runs
    // with nothing to delete, and then with a leftover partial file.
    engine.discard_download(&file_a).await.unwrap();
    assert_eq!(fs::read(&file_a).unwrap(), a);
    let partial = partial_path(&file_a).unwrap();
    fs::write(&partial, b"leftover of an interrupted run").unwrap();
    engine.discard_download(&file_a).await.unwrap();
    assert!(!partial.exists());
    assert_eq!(fs::read(&file_a).unwrap(), a);
}

// ---- partial download files: links ----

/// A link planted at the partial name (scanners skip `*.bbpart`) made the
/// download open it through the link, truncate and overwrite the target
/// outside the destination folder, and the final rename turned the
/// destination itself into a link. The name is never followed now: a link
/// (dangling or not) or a file with other hard links is replaced by a file
/// of its own, and what it pointed to is untouched.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_planted_at_the_partial_name_is_replaced_never_written_through() {
    use std::os::unix::fs::symlink;

    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let content = payload(64 * 1024, 0x5a);
    for key in ["f1.txt", "f2.txt", "f3.txt", "f4.txt"] {
        s3.put(&bucket, key, content.clone()).await;
    }
    let engine = engine_for(s3.ops());
    let work = tempfile::tempdir().unwrap();
    let dest = work.path().join("sync");
    let outside = work.path().join("elsewhere");
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let victim = outside.join("victim.txt");
    fs::write(&victim, "PRECIOUS DATA OUTSIDE THE DESTINATION FOLDER").unwrap();

    // A link to a file, a link to nothing, a hard link, and a link to a folder.
    let never_created = outside.join("never-created.txt");
    symlink(&victim, dest.join("f1.txt.bbpart")).unwrap();
    symlink(&never_created, dest.join("f2.txt.bbpart")).unwrap();
    fs::hard_link(&victim, dest.join("f3.txt.bbpart")).unwrap();
    symlink(&outside, dest.join("f4.txt.bbpart")).unwrap();

    for key in ["f1.txt", "f2.txt", "f3.txt", "f4.txt"] {
        let outcome = engine
            .download_file(
                &uri(&bucket, key),
                &dest.join(key),
                &Hooks::new(),
                None,
                &no_checkpoint,
            )
            .await
            .unwrap();
        assert!(outcome.is_completed(), "{key}");
        let kind = fs::symlink_metadata(dest.join(key)).unwrap().file_type();
        assert!(kind.is_file(), "{key} must be an ordinary file, not a link");
        assert_eq!(fs::read(dest.join(key)).unwrap(), content, "{key}");
    }
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "PRECIOUS DATA OUTSIDE THE DESTINATION FOLDER"
    );
    assert!(!never_created.exists(), "created through a dangling link");
    assert_eq!(files_below(&outside), ["victim.txt"]);
    assert_eq!(
        files_below(&dest),
        ["f1.txt", "f2.txt", "f3.txt", "f4.txt"],
        "no partial file is left"
    );

    // A cleanup removes a planted link without touching its target.
    symlink(&victim, dest.join("f1.txt.bbpart")).unwrap();
    engine.discard_download(&dest.join("f1.txt")).await.unwrap();
    assert!(fs::symlink_metadata(dest.join("f1.txt.bbpart")).is_err());
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "PRECIOUS DATA OUTSIDE THE DESTINATION FOLDER"
    );
}

/// A link that is planted while the download runs (the partial file exists,
/// then the name is swapped) is never renamed into place either: the final
/// rename would have made the destination a link to the victim.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_swapped_in_during_the_download_is_never_renamed_into_place() {
    use std::os::unix::fs::symlink;

    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "f.txt", payload(64 * 1024, 0x21)).await;
    let engine = engine_for(s3.ops());
    let work = tempfile::tempdir().unwrap();
    let victim = work.path().join("victim.txt");
    fs::write(&victim, "PRECIOUS").unwrap();
    let dest = work.path().join("f.txt");
    let partial = partial_path(&dest).unwrap();

    let (gate, mut reached, release) = HoldFirst::new();
    let running = {
        let engine = engine.clone();
        let src = uri(&bucket, "f.txt");
        let dest = dest.clone();
        let hooks = Hooks::new().with_throttle(gate);
        tokio::spawn(async move {
            engine
                .download_file(&src, &dest, &hooks, None, &no_checkpoint)
                .await
        })
    };
    reached.wait_for(|reached| *reached).await.unwrap();
    fs::remove_file(&partial).unwrap();
    symlink(&victim, &partial).unwrap();
    release.send(true).unwrap();

    let err = running.await.unwrap().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert!(err.to_string().contains("replaced"), "{err}");
    assert!(fs::symlink_metadata(&dest).is_err(), "nothing was renamed");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "PRECIOUS");
}

// ---- a cancel whose cleanup fails ----

/// With the network down the cleanup of a cancelled multipart upload fails.
/// The task used to end `cancelled` anyway, with its resume state cleared:
/// the upload stayed open (and billed) on the server, named by nothing, and
/// cancelling again was a conflict. The task is `failed` now and keeps its
/// state; cancelling it again repeats the cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_whose_cleanup_fails_keeps_the_upload_findable_and_can_be_repeated() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;

    // A closed port: what the account looks like while the network is down.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let mut dead_account = s3.account().clone();
    dead_account.endpoint = Some(format!("http://{closed}"));
    let dead = S3::new(
        &dead_account,
        &s3.credentials(),
        &ClientOptions {
            max_attempts: 1,
            ..ClientOptions::default()
        },
    )
    .unwrap();
    let good = s3.ops().clone();
    let offline = Arc::new(AtomicBool::new(false));
    let provider: S3Provider = {
        let offline = Arc::clone(&offline);
        Arc::new(move |_account: String| {
            let s3 = if offline.load(Ordering::SeqCst) {
                dead.clone()
            } else {
                good.clone()
            };
            Box::pin(async move { Ok(s3) })
        })
    };
    let runner = S3Runner::new(provider, DataIntegritySettings::default());
    let queue = TransferQueue::open(
        TransferSettings {
            max_concurrent_tasks: 1,
            max_parts_per_task: 1,
            multipart_threshold_mib: 5,
            part_size_mib: 5,
            bandwidth_limit_kibps: Some(4096),
            retry_attempts: 0,
            ..TransferSettings::default()
        },
        runner.runners(),
        QueueOptions::default(),
    )
    .await
    .unwrap();

    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("big.bin");
    fs::write(&path, payload(11 * MIB as usize, 0x42)).unwrap();
    let task = enqueue_upload(&queue, "acct", &[path], &bucket, "")
        .await
        .unwrap()[0]
        .clone();
    wait_for("a finished part", || {
        queue
            .resume_state(&task.id)
            .ok()
            .flatten()
            .and_then(|s| s["parts"].as_array().map(Vec::len))
            .is_some_and(|n| n >= 1)
    })
    .await;
    queue.pause_task(&task.id).unwrap();
    wait_for("stopped", || {
        queue.get(&task.id).unwrap().state == TransferState::Stopped
    })
    .await;
    let upload_id = queue.resume_state(&task.id).unwrap().unwrap()["upload_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(s3.pending_uploads(&bucket, "big.bin").await, 1);

    // The network goes away while the user cancels.
    offline.store(true, Ordering::SeqCst);
    queue.cancel_task(&task.id).unwrap();
    wait_for("the cancellation to fail", || {
        queue.get(&task.id).unwrap().state == TransferState::Failed
    })
    .await;
    let kept = queue.get(&task.id).unwrap();
    let error = kept.error.clone().unwrap();
    assert!(error.contains("cleanup after cancelling failed"), "{error}");
    assert!(error.contains("cancelling it again"), "{error}");
    let resume = queue.resume_state(&task.id).unwrap().unwrap();
    assert_eq!(resume["upload_id"], upload_id.as_str());
    assert_eq!(s3.pending_uploads(&bucket, "big.bin").await, 1);

    // Back online, the same command finishes the job.
    offline.store(false, Ordering::SeqCst);
    queue.cancel_task(&task.id).unwrap();
    wait_for("the cancellation to finish", || {
        queue.get(&task.id).unwrap().state == TransferState::Cancelled
    })
    .await;
    assert_eq!(queue.get(&task.id).unwrap().error, None);
    assert_eq!(queue.resume_state(&task.id).unwrap(), None);
    assert_eq!(
        s3.pending_uploads(&bucket, "big.bin").await,
        0,
        "the multipart upload {upload_id} is still open"
    );
    queue.shutdown().await.unwrap();
}

/// A multipart upload created for a server-side copy whose abort fails stays
/// open on the server; the copy used to drop that failure. Its error names
/// the upload now, next to the failure of the copy itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_multipart_copy_that_cannot_abort_its_upload_says_so() {
    for abort_works in [false, true] {
        let mock = MockS3::start(move |request| {
            let copy_part = request.method == "PUT" && request.query_param("partNumber").is_some();
            match (request.method.as_str(), request.path.as_str()) {
                ("HEAD", "/bucket/src.bin") => MockResponse::empty(200)
                    .with_header("content-length", &(12 * MIB).to_string())
                    .with_header("etag", "\"0123456789abcdef0123456789abcdef\""),
                ("GET", "/bucket/src.bin") if request.query_param("tagging").is_some() => {
                    MockResponse::error(501, "NotImplemented", "no tagging")
                }
                ("POST", "/bucket/dst.bin") if request.query_param("uploads").is_some() => {
                    MockResponse::xml(
                        "<InitiateMultipartUploadResult><Bucket>bucket</Bucket><Key>dst.bin</Key><UploadId>UP-1</UploadId></InitiateMultipartUploadResult>",
                    )
                }
                ("PUT", "/bucket/dst.bin") if copy_part => {
                    if request.query_param("partNumber").as_deref() == Some("1") {
                        MockResponse::xml("<CopyPartResult><ETag>\"e1\"</ETag></CopyPartResult>")
                    } else {
                        MockResponse::error(500, "InternalError", "part two failed")
                    }
                }
                ("DELETE", "/bucket/dst.bin") if request.query_param("uploadId").is_some() => {
                    if abort_works {
                        MockResponse::empty(204)
                    } else {
                        MockResponse::error(500, "InternalError", "abort failed")
                    }
                }
                _ => MockResponse::error(400, "UnexpectedRequest", &request.path),
            }
        });
        let options = CopyOptions {
            multipart_threshold: 6 * MIB,
            part_size: 5 * MIB,
            concurrency: 1,
        };
        let err = mock
            .ops()
            .copy_object_with(
                &uri("bucket", "src.bin"),
                &uri("bucket", "dst.bin"),
                &options,
                |_| {},
            )
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("part two failed"), "{text}");
        assert!(text.contains("copying part 2"), "{text}");
        let abort_reported = text.contains("could not be aborted") && text.contains("UP-1");
        assert_eq!(abort_reported, !abort_works, "{text}");
        let aborts = mock
            .requests()
            .iter()
            .filter(|r| {
                r.method == "DELETE" && r.query_param("uploadId").as_deref() == Some("UP-1")
            })
            .count();
        assert_eq!(aborts, 1);
    }
}

// ---- the multipart upload ID reaches the disk at once ----

/// The ID of a new multipart upload reached the queue file only after the
/// persist delay (1 s by default); a crash in that window left an upload on
/// the server that nothing named. It is saved at once now.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_id_of_a_new_multipart_upload_is_saved_without_the_persist_delay() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let good = s3.ops().clone();
    let provider: S3Provider = Arc::new(move |_account: String| {
        let s3 = good.clone();
        Box::pin(async move { Ok(s3) })
    });
    let runner = S3Runner::new(provider, DataIntegritySettings::default());
    let work = tempfile::tempdir().unwrap();
    let queue_file = work.path().join("transfers.json");
    let queue = TransferQueue::open(
        TransferSettings {
            max_concurrent_tasks: 1,
            max_parts_per_task: 1,
            multipart_threshold_mib: 5,
            part_size_mib: 5,
            // Slow enough that the first part is still on its way when the
            // test looks at the file.
            bandwidth_limit_kibps: Some(256),
            retry_attempts: 0,
            ..TransferSettings::default()
        },
        runner.runners(),
        QueueOptions {
            // The delay of the old behaviour is an hour: nothing but an
            // urgent save can write the file inside this test.
            persist_delay: Duration::from_secs(3600),
            ..QueueOptions::persistent(&queue_file)
        },
    )
    .await
    .unwrap();
    let path = work.path().join("big.bin");
    fs::write(&path, payload(6 * MIB as usize, 0x42)).unwrap();
    let task = enqueue_upload(&queue, "acct", &[path], &bucket, "")
        .await
        .unwrap()[0]
        .clone();

    let began = Instant::now();
    let saved_id = loop {
        let saved = fs::read(&queue_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|file| {
                file["tasks"][0]["resume"]["upload_id"]
                    .as_str()
                    .map(str::to_owned)
            });
        if let Some(id) = saved {
            break id;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the upload ID was not saved within 20 s (persist delay: one hour)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(!saved_id.is_empty());
    assert_eq!(s3.pending_uploads(&bucket, "big.bin").await, 1);
    queue.pause_task(&task.id).unwrap();
    queue.shutdown().await.unwrap();
}

// ---- folder keys that hold data ----

/// A mock server for one key `data/` that holds `size` bytes (or none at
/// all with `None`): HEAD, copy and delete are answered like S3 does.
fn folder_key_server(size: Option<u64>) -> MockS3 {
    MockS3::start(move |request| match request.method.as_str() {
        "HEAD" => match size {
            Some(size) => MockResponse::empty(200)
                .with_header("content-length", &size.to_string())
                .with_header("etag", "\"0123456789abcdef0123456789abcdef\""),
            None => MockResponse::error(404, "NoSuchKey", "no such key"),
        },
        "PUT" => MockResponse::xml(
            "<CopyObjectResult><ETag>\"d41d8cd98f00b204e9800998ecf8427e\"</ETag></CopyObjectResult>",
        ),
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::error(500, "Unexpected", "unexpected request"),
    })
}

fn requests_of(mock: &MockS3) -> Vec<String> {
    mock.requests()
        .iter()
        .map(|r| match r.header("x-amz-copy-source") {
            Some(source) => format!("{} {} copy-source={source}", r.method, r.path),
            None => format!("{} {}", r.method, r.path),
        })
        .collect()
}

/// S3 allows data under a key that ends in `/`. Moving such an object wrote
/// an empty folder marker and then deleted the original data. Only an empty
/// (or missing) object is a marker now; anything else is copied for real
/// before its source is deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_a_folder_key_that_holds_data_copies_the_data() {
    let mock = folder_key_server(Some(MIB));
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let src = uri("bucket", "data/");
    let dst = uri("bucket", "moved/data/");
    let outcome = engine.move_object(&src, &dst, &Hooks::new()).await.unwrap();
    assert!(matches!(outcome, Outcome::Completed(done) if done.size == MIB));
    let requests = requests_of(&mock);
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("PUT /bucket/moved/data/ copy-source=")),
        "the data was never copied: {requests:?}"
    );
    assert!(
        !requests.iter().any(|r| r == "PUT /bucket/moved/data/"),
        "an empty marker replaced the data: {requests:?}"
    );
    let copied = requests.iter().position(|r| r.starts_with("PUT ")).unwrap();
    let deleted = requests
        .iter()
        .position(|r| r == "DELETE /bucket/data/")
        .expect("the source is deleted after the copy");
    assert!(copied < deleted, "{requests:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_and_missing_folder_markers_are_still_recreated_as_markers() {
    for size in [Some(0), None] {
        let mock = folder_key_server(size);
        let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
        let outcome = engine
            .move_object(
                &uri("bucket", "data/"),
                &uri("bucket", "moved/data/"),
                &Hooks::new(),
            )
            .await
            .unwrap();
        assert!(outcome.is_completed());
        let requests = requests_of(&mock);
        assert_eq!(
            requests,
            [
                "HEAD /bucket/data/",
                "PUT /bucket/moved/data/",
                "DELETE /bucket/data/"
            ],
            "{size:?}"
        );
    }

    // A failed HEAD is an error: the source is not deleted on a guess.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => MockResponse::error(500, "InternalError", "no head today"),
        _ => MockResponse::empty(204),
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let err = engine
        .move_object(
            &uri("bucket", "data/"),
            &uri("bucket", "moved/data/"),
            &Hooks::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
    assert!(mock.requests().iter().all(|r| r.method == "HEAD"));
}

/// Downloading such a key made a folder and dropped the bytes without a
/// word. It is an error now (and a failed entry in the expansion); a real
/// folder marker, and a prefix without any object, still make the folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_key_that_holds_data_is_not_downloaded_as_a_folder() {
    let work = tempfile::tempdir().unwrap();
    for (size, folder_made) in [(Some(5), false), (Some(0), true), (None, true)] {
        let mock = folder_key_server(size);
        let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
        let dest = work.path().join(format!("{size:?}"));
        let result = engine
            .download_file(
                &uri("bucket", "data/"),
                &dest,
                &Hooks::new(),
                None,
                &no_checkpoint,
            )
            .await;
        if folder_made {
            assert!(result.unwrap().is_completed(), "{size:?}");
        } else {
            let err = result.unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
            assert!(err.to_string().contains("holds 5 bytes"), "{err}");
        }
        assert_eq!(dest.is_dir(), folder_made, "{size:?}");
    }
}

fn listing(prefix: &str, entries: &[(&str, u64)]) -> MockResponse {
    let contents: String = entries
        .iter()
        .map(|(key, size)| {
            format!(
                "<Contents><Key>{key}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"e\"</ETag><Size>{size}</Size></Contents>"
            )
        })
        .collect();
    MockResponse::xml(format!(
        "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>bucket</Name><Prefix>{prefix}</Prefix><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>",
        entries.len()
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_expansion_reports_folder_keys_that_hold_data() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            listing(
                "dir/",
                &[
                    ("dir/", 0),
                    ("dir/empty/", 0),
                    ("dir/full/", 12),
                    ("dir/full/inside.txt", 3),
                    ("dir/ok.txt", 3),
                ],
            )
        } else {
            MockResponse::error(500, "Unexpected", &request.path)
        }
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let expansion = engine
        .expand_download("bucket", &["dir/".to_owned()], dest.path())
        .await
        .unwrap();
    let keys: Vec<&str> = expansion.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        ["dir/", "dir/empty/", "dir/full/inside.txt", "dir/ok.txt"]
    );
    assert_eq!(expansion.failures.len(), 1);
    let failure = &expansion.failures[0];
    assert_eq!(failure.entry, "dir/full/");
    assert_eq!(failure.code, ErrorKind::Usage);
    assert!(
        failure.message.contains("holds 12 bytes"),
        "{}",
        failure.message
    );

    // The same for the selected folder itself.
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            listing("dir/", &[("dir/", 7), ("dir/ok.txt", 3)])
        } else {
            MockResponse::error(500, "Unexpected", &request.path)
        }
    });
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let expansion = engine
        .expand_download("bucket", &["dir/".to_owned()], dest.path())
        .await
        .unwrap();
    let keys: Vec<&str> = expansion.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["dir/ok.txt"]);
    assert_eq!(expansion.failures.len(), 1);
    assert_eq!(expansion.failures[0].entry, "dir/");
}

// ---- a single PutObject of a file that changes while it is read ----

/// A source whose content changes after the first read, as a file rewritten
/// in place while it is uploaded; `is_unchanged` reports the change.
#[derive(Debug)]
struct ChangesWhileRead {
    old: Vec<u8>,
    new: Vec<u8>,
    reads: AtomicU64,
}

impl UploadSource for ChangesWhileRead {
    fn size(&self) -> u64 {
        self.old.len() as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let first = self.reads.fetch_add(1, Ordering::SeqCst) == 0;
        let data = if first { &self.old } else { &self.new };
        let start = usize::try_from(offset).unwrap();
        buf.copy_from_slice(&data[start..start + buf.len()]);
        Ok(())
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint {
            size: self.size(),
            modified: None,
        }
    }

    fn is_unchanged(&self) -> io::Result<bool> {
        Ok(self.reads.load(Ordering::SeqCst) <= 1)
    }

    fn describe(&self) -> String {
        "'changing.bin'".into()
    }
}

fn changing(len: usize) -> Arc<ChangesWhileRead> {
    Arc::new(ChangesWhileRead {
        old: vec![b'o'; len],
        new: vec![b'n'; len],
        reads: AtomicU64::new(0),
    })
}

/// A source that never changes.
#[derive(Debug)]
struct Steady(Vec<u8>);

impl UploadSource for Steady {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).unwrap();
        buf.copy_from_slice(&self.0[start..start + buf.len()]);
        Ok(())
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint {
            size: self.size(),
            modified: None,
        }
    }

    fn is_unchanged(&self) -> io::Result<bool> {
        Ok(true)
    }

    fn describe(&self) -> String {
        "'steady.bin'".into()
    }
}

/// With the checksum set to `none` nothing makes the server notice a body
/// that mixes old and new content, and a single `PutObject` never looked at
/// the source again (the multipart path does): the torn file was stored and
/// the run completed. The run fails now, so that the retry uploads the file
/// again and replaces the object.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_put_without_a_checksum_fails_when_the_file_changed_meanwhile() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let options = TransferOptions {
        multipart_threshold: 5 * MIB,
        part_size: 5 * MIB,
        checksum: ChecksumAlgorithm::None,
        ..TransferOptions::default()
    };
    let engine = TransferEngine::new(s3.ops().clone(), options).unwrap();
    let no_upload_checkpoint = |_: &UploadState| {};

    // Multipart: the change is detected before CompleteMultipartUpload.
    let multipart = engine
        .upload(
            changing(6 * MIB as usize),
            &uri(&bucket, "multi.bin"),
            None,
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await;
    assert!(multipart.is_err(), "the multipart path detects the change");

    // Single request: detected after the request.
    let err = engine
        .upload(
            changing(MIB as usize),
            &uri(&bucket, "single.bin"),
            None,
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io, "{err}");
    assert!(is_retryable(err.kind()));
    assert!(
        err.to_string()
            .contains("changed while it was being uploaded"),
        "{err}"
    );
    let stored = s3.get(&bucket, "single.bin").await;
    assert!(
        stored.contains(&b'o') && stored.contains(&b'n'),
        "the object was stored torn, which is why the run must fail"
    );

    // The retry uploads the file again and replaces the torn object.
    let content = payload(MIB as usize, 0x09);
    let outcome = engine
        .upload(
            Arc::new(Steady(content.clone())),
            &uri(&bucket, "single.bin"),
            None,
            &Hooks::new(),
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(s3.get(&bucket, "single.bin").await, content);
}

// ---- stopping a download while its partial file is read back ----

/// Asks the engine to stop as soon as a resumed download reports that its
/// partial file is complete, which is when the file is read back to be
/// verified.
struct StopWhenComplete {
    size: u64,
    stop: StopSignal,
    cancel: bool,
}

impl Progress for StopWhenComplete {
    fn set_total(&self, _total: u64) {}

    fn set_done(&self, done: u64) {
        if done == self.size {
            if self.cancel {
                self.stop.cancel();
            } else {
                self.stop.pause();
            }
        }
    }

    fn add_done(&self, _amount: u64) {}
}

/// Verifying a resumed download read the whole partial file in one blocking
/// call that ignored a stop request: with a large file the queue dropped the
/// run after its grace period while the call still held the file, and the
/// cancel's cleanup found the file locked and went away leaving it behind.
/// The read-back checks the stop signal between chunks now.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_ends_the_verification_of_a_resumed_download() {
    // Nothing is requested: every byte is already in the partial file.
    let mock = MockS3::start(|request| MockResponse::error(500, "Unexpected", &request.path));
    let engine = TransferEngine::new(mock.ops(), TransferOptions::default()).unwrap();
    let data = payload(3 * MIB as usize + 5, 0x6c);
    let state = DownloadState {
        bucket: "bucket".into(),
        key: "big.bin".into(),
        etag: "\"v1\"".into(),
        size: data.len() as u64,
        last_modified: None,
        mtime: None,
        checksum: Some(ObjectChecksum {
            algorithm: ChecksumAlgorithm::Crc32,
            value: checksum_of(ChecksumAlgorithm::Crc32, &data).unwrap(),
        }),
        offset: data.len() as u64,
    };
    let src = uri("bucket", "big.bin");

    // Without a stop the file verifies and is renamed into place.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("big.bin");
    fs::write(partial_path(&dest).unwrap(), &data).unwrap();
    let outcome = engine
        .download_file(
            &src,
            &dest,
            &Hooks::new(),
            Some(state.clone()),
            &no_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(fs::read(&dest).unwrap(), data);

    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("big.bin");
        let partial = partial_path(&dest).unwrap();
        fs::write(&partial, &data).unwrap();
        let stop = StopSignal::new();
        let hooks =
            Hooks::new()
                .with_stop(stop.clone())
                .with_progress(Arc::new(StopWhenComplete {
                    size: state.size,
                    stop,
                    cancel,
                }));
        let checkpoints = Mutex::new(Vec::new());
        let checkpoint = |s: &DownloadState| checkpoints.lock().unwrap().push(s.clone());
        let outcome = engine
            .download_file(&src, &dest, &hooks, Some(state.clone()), &checkpoint)
            .await
            .unwrap();
        assert!(
            !dest.exists(),
            "a download that was stopped renamed its file"
        );
        if cancel {
            assert_eq!(outcome, Outcome::Cancelled);
            assert!(
                !partial.exists(),
                "a cancelled download left its partial file"
            );
        } else {
            // A pause keeps the file, and a checkpoint that says every byte is
            // there, so that the next run verifies again.
            assert_eq!(outcome, Outcome::Paused);
            assert_eq!(fs::read(&partial).unwrap(), data);
            let last = checkpoints.lock().unwrap().last().cloned();
            assert_eq!(last.map(|s| s.offset), Some(state.size));
        }
    }
    assert!(mock.requests().is_empty());
}

/// A sink over a partial file whose flush is slow, like a disk that takes
/// long to sync; it says when the flush started.
struct SlowSync {
    inner: FileSink,
    started: Arc<AtomicBool>,
    pause: Duration,
}

impl DownloadSink for SlowSync {
    fn prepare(&mut self, offset: u64) -> io::Result<u64> {
        self.inner.prepare(offset)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.inner.write_all(data)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.started.store(true, Ordering::SeqCst);
        std::thread::sleep(self.pause);
        self.inner.sync()
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// The queue drops a run that outlives its grace period; a write or a flush
/// that was in progress then still holds the partial file, and the cleanup
/// that followed found it locked, returned success and left the file
/// behind for good. The cleanup waits for the holder (for a bounded time)
/// and deletes the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cleanup_waits_for_a_dropped_run_that_still_holds_the_partial_file() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "f.bin", payload(10 * 1024, 0x31)).await;
    let engine = engine_for(s3.ops());
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f.bin");
    let partial = partial_path(&dest).unwrap();

    let started = Arc::new(AtomicBool::new(false));
    let sink = SlowSync {
        inner: FileSink::open(&partial).unwrap(),
        started: Arc::clone(&started),
        pause: Duration::from_millis(600),
    };
    let run = {
        let engine = engine.clone();
        let src = uri(&bucket, "f.bin");
        tokio::spawn(async move {
            engine
                .download_to(&src, sink, &Hooks::new(), None, &no_checkpoint)
                .await
                .map(|_| ())
        })
    };
    let flag = Arc::clone(&started);
    wait_for("the flush to start", move || flag.load(Ordering::SeqCst)).await;
    // The queue gives up on the run: its future is dropped while the flush
    // still holds the file.
    run.abort();
    let _ = run.await;
    assert!(partial.exists());

    let asked = Instant::now();
    engine.discard_download(&dest).await.unwrap();
    assert!(
        !partial.exists(),
        "the cleanup returned while the partial file was still there"
    );
    assert!(
        asked.elapsed() >= Duration::from_millis(50),
        "the cleanup did not wait for the holder of the file"
    );
}

// ---- how many bodies stream at once ----

/// A throttle that admits `limit` streams at a time and records how many
/// were open at once.
struct FewStreams {
    places: Arc<Semaphore>,
    open: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    opened: AtomicUsize,
}

struct Held {
    _place: tokio::sync::OwnedSemaphorePermit,
    open: Arc<AtomicUsize>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FewStreams {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(FewStreams {
            places: Arc::new(Semaphore::new(limit)),
            open: Arc::default(),
            peak: Arc::default(),
            opened: AtomicUsize::new(0),
        })
    }
}

impl Throttle for FewStreams {
    fn acquire(&self, _bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(std::future::ready(()))
    }

    fn open_stream(&self) -> StreamFuture<'_> {
        Box::pin(async move {
            let place = Arc::clone(&self.places).acquire_owned().await.unwrap();
            self.opened.fetch_add(1, Ordering::SeqCst);
            let open = self.open.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(open, Ordering::SeqCst);
            StreamPermit::holding(Held {
                _place: place,
                open: Arc::clone(&self.open),
            })
        })
    }
}

/// At a very low bandwidth cap one round of the limiter over all streams
/// (up to 32 tasks of 32 parts) took longer than the 20 s after which an
/// idle request body fails. The engines ask the throttle for a place before
/// every request that streams a body, and hold it until the request ends,
/// so that a throttle can keep only the streams it can serve in time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engines_ask_the_throttle_for_a_place_before_every_streaming_request() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let engine = TransferEngine::new(
        s3.ops().clone(),
        TransferOptions {
            multipart_threshold: 5 * MIB,
            part_size: 5 * MIB,
            max_parts_in_flight: 4,
            ..TransferOptions::default()
        },
    )
    .unwrap();
    let throttle = FewStreams::new(2);
    let hooks = Hooks::new().with_throttle(throttle.clone());
    let no_upload_checkpoint = |_: &UploadState| {};

    // Four parts, four in flight, two places.
    let big = payload(16 * MIB as usize, 0x01);
    let outcome = engine
        .upload(
            Arc::new(Steady(big.clone())),
            &uri(&bucket, "big.bin"),
            None,
            &hooks,
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(s3.get(&bucket, "big.bin").await, big);
    assert_eq!(throttle.opened.load(Ordering::SeqCst), 4, "one per part");
    assert!(
        throttle.peak.load(Ordering::SeqCst) <= 2,
        "{} streams at once",
        throttle.peak.load(Ordering::SeqCst)
    );

    // A single request, then a download, each one stream.
    let small = payload(100 * 1024, 0x02);
    let outcome = engine
        .upload(
            Arc::new(Steady(small.clone())),
            &uri(&bucket, "small.bin"),
            None,
            &hooks,
            None,
            &no_upload_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(throttle.opened.load(Ordering::SeqCst), 5);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("small.bin");
    let outcome = engine
        .download_file(
            &uri(&bucket, "small.bin"),
            &dest,
            &hooks,
            None,
            &no_checkpoint,
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(fs::read(&dest).unwrap(), small);
    assert_eq!(throttle.opened.load(Ordering::SeqCst), 6);
    assert_eq!(throttle.open.load(Ordering::SeqCst), 0, "a place leaked");
}
