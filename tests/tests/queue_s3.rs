//! The transfer queue running the S3 engines (`engine::S3Runner`) against a
//! real server: every task kind, folders, pause and resume, cancel, a
//! restart, a failed upload and its retry, the bandwidth and concurrency
//! caps, two accounts, and selections with entries that cannot be added.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bucketbridge_core::engine::{
    S3Provider, S3Runner, enqueue_copy, enqueue_delete, enqueue_download, enqueue_move,
    enqueue_rename, enqueue_upload, partial_path,
};
use bucketbridge_core::model::{Location, TransferKind, TransferState, TransferTask};
use bucketbridge_core::settings::{DataIntegritySettings, MIB, TransferSettings};
use bucketbridge_core::transfer::{QueueEvent, QueueOptions, TransferQueue};
use bucketbridge_core::{ClientOptions, Error, ErrorKind, S3};
use bucketbridge_tests::{Backend, MockResponse, MockS3, RecordedRequest, TestS3};
use tokio::task::AbortHandle;

const ACCOUNT: &str = "account-1";

/// Longest wait for any condition.
const WAIT: Duration = Duration::from_secs(90);

// ---- helpers ----

/// Deterministic bytes; different seeds give different bytes.
fn payload(len: usize, seed: u32) -> Vec<u8> {
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

fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, data).unwrap();
    path
}

fn provider(accounts: &[(&str, S3)]) -> S3Provider {
    let accounts: Arc<HashMap<String, S3>> = Arc::new(
        accounts
            .iter()
            .map(|(id, s3)| ((*id).to_owned(), s3.clone()))
            .collect(),
    );
    Arc::new(move |account_id: String| {
        let accounts = Arc::clone(&accounts);
        Box::pin(async move {
            accounts
                .get(&account_id)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("account {account_id} does not exist")))
        })
    })
}

/// 5 MiB parts from 5 MiB on, one part at a time, no automatic retries.
fn settings(limit_kibps: Option<u64>) -> TransferSettings {
    TransferSettings {
        max_concurrent_tasks: 4,
        max_parts_per_task: 1,
        multipart_threshold_mib: 5,
        part_size_mib: 5,
        bandwidth_limit_kibps: limit_kibps,
        retry_attempts: 0,
    }
}

async fn open(
    provider: S3Provider,
    settings: TransferSettings,
    options: QueueOptions,
) -> TransferQueue {
    let runner = S3Runner::new(provider, DataIntegritySettings::default());
    TransferQueue::open(settings, runner.runners(), options)
        .await
        .unwrap()
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Waits until the task is in `state`; fails fast when it ends otherwise.
async fn wait_state(queue: &TransferQueue, id: &str, state: TransferState) -> TransferTask {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = queue.get(id).unwrap();
        if task.state == state {
            return task;
        }
        let ended_otherwise = matches!(
            (task.state, state),
            (TransferState::Completed | TransferState::Cancelled, _)
                | (TransferState::Failed, TransferState::Completed)
        );
        assert!(
            !ended_otherwise,
            "task {id} ended {:?} ({:?}) instead of {state:?}",
            task.state, task.error
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for task {id} to be {state:?} (it is {:?})",
            task.state
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_all(queue: &TransferQueue, tasks: &[TransferTask], state: TransferState) {
    for task in tasks {
        wait_state(queue, &task.id, state).await;
    }
}

/// Upload id and number of finished parts in an upload task's resume state.
fn upload_progress(queue: &TransferQueue, id: &str) -> Option<(String, usize)> {
    let state = queue.resume_state(id).ok()??;
    Some((
        state["upload_id"].as_str()?.to_owned(),
        state["parts"].as_array()?.len(),
    ))
}

/// Durable offset in a download task's resume state.
fn download_offset(queue: &TransferQueue, id: &str) -> Option<u64> {
    queue.resume_state(id).ok()??["offset"].as_u64()
}

/// Polls an upload task until it completes and returns every upload id its
/// resume state showed meanwhile, and the first part count seen.
async fn watch_upload(queue: &TransferQueue, id: &str) -> (HashSet<String>, Option<usize>) {
    let mut ids = HashSet::new();
    let mut first_parts = None;
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some((upload_id, parts)) = upload_progress(queue, id) {
            ids.insert(upload_id);
            first_parts.get_or_insert(parts);
        }
        let task = queue.get(id).unwrap();
        match task.state {
            TransferState::Completed => return (ids, first_parts),
            TransferState::Failed | TransferState::Cancelled => {
                panic!("task {id} ended {:?}: {:?}", task.state, task.error)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "task {id} did not complete");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn group_of(tasks: &[TransferTask]) -> String {
    let group = tasks[0].group_id.clone().expect("a group id");
    assert!(
        tasks
            .iter()
            .all(|t| t.group_id.as_deref() == Some(group.as_str()))
    );
    group
}

fn strings(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| (*k).to_owned()).collect()
}

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

// ---- every kind ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_kind_runs_through_the_queue() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(None),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let small = payload(1000, 1);
    let big = payload(6 * MIB as usize + 3, 2);
    let small_path = write_file(local.path(), "a.txt", &small);
    let big_path = write_file(local.path(), "big.bin", &big);

    let uploads = enqueue_upload(&queue, ACCOUNT, &[small_path, big_path], &bucket, "up/")
        .await
        .unwrap();
    assert_eq!(uploads.len(), 2);
    assert!(uploads.iter().all(|t| t.kind == TransferKind::Upload));
    let upload_group = group_of(&uploads);
    wait_all(&queue, &uploads, TransferState::Completed).await;
    assert!(s3.get(&bucket, "up/a.txt").await == small);
    assert!(s3.get(&bucket, "up/big.bin").await == big);
    let done = queue.get(&uploads[1].id).unwrap();
    assert_eq!(done.bytes_total, Some(big.len() as u64));
    assert_eq!(done.bytes_done, big.len() as u64);
    assert_eq!(queue.resume_state(&uploads[1].id).unwrap(), None);

    let target = tempfile::tempdir().unwrap();
    let downloads = enqueue_download(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["up/a.txt", "up/big.bin"]),
        target.path(),
    )
    .await
    .unwrap();
    assert_ne!(group_of(&downloads), upload_group);
    wait_all(&queue, &downloads, TransferState::Completed).await;
    assert!(fs::read(target.path().join("a.txt")).unwrap() == small);
    assert!(fs::read(target.path().join("big.bin")).unwrap() == big);

    let copies = enqueue_copy(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["up/"]),
        &bucket,
        "copy/",
    )
    .await
    .unwrap();
    wait_all(&queue, &copies, TransferState::Completed).await;
    assert!(s3.get(&bucket, "copy/up/big.bin").await == big);

    let moves = enqueue_move(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["copy/up/a.txt"]),
        &bucket,
        "moved/",
    )
    .await
    .unwrap();
    wait_all(&queue, &moves, TransferState::Completed).await;
    assert!(!s3.exists(&bucket, "copy/up/a.txt").await);
    assert!(s3.get(&bucket, "moved/a.txt").await == small);

    let renames = enqueue_rename(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        "moved/a.txt",
        "renamed.txt",
    )
    .await
    .unwrap();
    assert_eq!(renames[0].kind, TransferKind::Move);
    wait_all(&queue, &renames, TransferState::Completed).await;
    assert!(s3.get(&bucket, "renamed.txt").await == small);
    let conflict = enqueue_rename(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        "up/a.txt",
        "renamed.txt",
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.kind(), ErrorKind::Conflict);

    let deletes = enqueue_delete(
        &queue,
        ACCOUNT,
        &bucket,
        &strings(&["renamed.txt", "copy/"]),
    )
    .await
    .unwrap();
    assert_eq!(deletes.len(), 2);
    assert!(deletes.iter().all(|t| t.dst.is_none()));
    wait_all(&queue, &deletes, TransferState::Completed).await;
    assert!(!s3.exists(&bucket, "renamed.txt").await);
    assert!(
        s3.ops()
            .list_all(&bucket, "copy/")
            .await
            .unwrap()
            .is_empty()
    );
    let prefix_delete = queue.get(&deletes[1].id).unwrap();
    assert_eq!(prefix_delete.bytes_done, 1, "objects are counted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folders_keep_their_relative_paths() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(None),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let root = local.path().join("photos");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("a.txt", b"top".to_vec()),
        ("2024/b c.jpg", payload(70_000, 3)),
        ("2024/trip ü/ファイル.txt", b"unicode".to_vec()),
        (
            "2024/trip ü/deeper/big.bin",
            payload(5 * MIB as usize + 9, 4),
        ),
    ];
    for (rel, data) in &files {
        write_file(&root, rel, data);
    }
    fs::create_dir_all(root.join("empty")).unwrap();

    let uploads = enqueue_upload(
        &queue,
        ACCOUNT,
        std::slice::from_ref(&root),
        &bucket,
        "backup/",
    )
    .await
    .unwrap();
    assert_eq!(uploads.len(), 5);
    group_of(&uploads);
    wait_all(&queue, &uploads, TransferState::Completed).await;
    assert!(marker_exists(&s3, &bucket, "backup/photos/empty/").await);

    let target = tempfile::tempdir().unwrap();
    let downloads = enqueue_download(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["backup/photos/"]),
        target.path(),
    )
    .await
    .unwrap();
    wait_all(&queue, &downloads, TransferState::Completed).await;
    for (rel, data) in &files {
        let path = target.path().join("photos").join(rel);
        assert!(fs::read(&path).unwrap() == *data, "{rel}: content differs");
    }
}

// ---- pause, cancel, restart ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_multipart_upload_resumes_with_its_finished_parts() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(Some(4096)),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let data = payload(11 * MIB as usize + 123, 5);
    let path = write_file(local.path(), "paused.bin", &data);
    let task = enqueue_upload(&queue, ACCOUNT, &[path], &bucket, "")
        .await
        .unwrap()[0]
        .clone();

    wait_for("the first part", || {
        upload_progress(&queue, &task.id).is_some_and(|(_, parts)| parts >= 1)
    })
    .await;
    queue.pause_task(&task.id).unwrap();
    wait_state(&queue, &task.id, TransferState::Stopped).await;
    let (upload_id, parts) = upload_progress(&queue, &task.id).unwrap();
    assert!((1..3).contains(&parts), "{parts} parts");
    assert!(!s3.exists(&bucket, "paused.bin").await);
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 1);

    queue.resume_task(&task.id).unwrap();
    let (ids, first_parts) = watch_upload(&queue, &task.id).await;
    assert_eq!(ids, HashSet::from([upload_id]), "the upload was restarted");
    assert!(first_parts.is_some_and(|n| n >= parts), "{first_parts:?}");
    assert!(
        s3.get(&bucket, "paused.bin").await == data,
        "content differs"
    );
    assert_eq!(s3.pending_uploads(&bucket, "paused.bin").await, 0);
    assert_eq!(queue.get(&task.id).unwrap().attempts, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_aborts_the_multipart_upload_and_deletes_the_partial_file() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(Some(4096)),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let path = write_file(local.path(), "cancel.bin", &payload(11 * MIB as usize, 6));

    // While it runs.
    let task = enqueue_upload(&queue, ACCOUNT, std::slice::from_ref(&path), &bucket, "")
        .await
        .unwrap()[0]
        .clone();
    wait_for("the first part", || {
        upload_progress(&queue, &task.id).is_some_and(|(_, parts)| parts >= 1)
    })
    .await;
    queue.cancel_task(&task.id).unwrap();
    let cancelled = wait_state(&queue, &task.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert_eq!(s3.pending_uploads(&bucket, "cancel.bin").await, 0);
    assert!(!s3.exists(&bucket, "cancel.bin").await);

    // After a pause.
    let task = enqueue_upload(&queue, ACCOUNT, std::slice::from_ref(&path), &bucket, "")
        .await
        .unwrap()[0]
        .clone();
    wait_for("the first part", || {
        upload_progress(&queue, &task.id).is_some_and(|(_, parts)| parts >= 1)
    })
    .await;
    queue.pause_task(&task.id).unwrap();
    wait_state(&queue, &task.id, TransferState::Stopped).await;
    assert_eq!(s3.pending_uploads(&bucket, "cancel.bin").await, 1);
    queue.cancel_task(&task.id).unwrap();
    let cancelled = wait_state(&queue, &task.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert_eq!(s3.pending_uploads(&bucket, "cancel.bin").await, 0);

    // A download.
    s3.put(&bucket, "remote.bin", payload(3 * MIB as usize, 7))
        .await;
    queue.update_settings(settings(Some(1024))).unwrap();
    let target = tempfile::tempdir().unwrap();
    let task = enqueue_download(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["remote.bin"]),
        target.path(),
    )
    .await
    .unwrap()[0]
        .clone();
    let dest = target.path().join("remote.bin");
    let partial = partial_path(&dest).unwrap();
    wait_for("downloaded bytes", || {
        fs::metadata(&partial).is_ok_and(|m| m.len() > 0)
    })
    .await;
    queue.cancel_task(&task.id).unwrap();
    let cancelled = wait_state(&queue, &task.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert!(!partial.exists(), "the partial file was left behind");
    assert!(!dest.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_mid_transfer_resumes_from_the_saved_state() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let state_dir = tempfile::tempdir().unwrap();
    let queue_file = state_dir.path().join("transfers.json");
    let accounts = provider(&[(ACCOUNT, s3.ops().clone())]);
    let queue = open(
        accounts.clone(),
        settings(Some(4096)),
        QueueOptions::persistent(&queue_file),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let upload_data = payload(11 * MIB as usize + 77, 8);
    let upload_path = write_file(local.path(), "up.bin", &upload_data);
    let download_data = payload(12 * MIB as usize + 5, 9);
    s3.put(&bucket, "down.bin", download_data.clone()).await;
    let target = tempfile::tempdir().unwrap();

    let upload = enqueue_upload(&queue, ACCOUNT, &[upload_path], &bucket, "")
        .await
        .unwrap()[0]
        .clone();
    let download = enqueue_download(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &strings(&["down.bin"]),
        target.path(),
    )
    .await
    .unwrap()[0]
        .clone();
    wait_for("progress on both transfers", || {
        upload_progress(&queue, &upload.id).is_some_and(|(_, parts)| parts >= 1)
            && download_offset(&queue, &download.id).is_some_and(|offset| offset >= MIB)
    })
    .await;
    queue.shutdown().await.unwrap();
    let (upload_id, _) = upload_progress(&queue, &upload.id).unwrap();
    drop(queue);

    let queue = open(
        accounts,
        settings(Some(8192)),
        QueueOptions::persistent(&queue_file),
    )
    .await;
    for task in [&upload, &download] {
        let reloaded = queue.get(&task.id).unwrap();
        assert_eq!(reloaded.state, TransferState::Stopped);
        assert!(reloaded.bytes_done > 0, "{reloaded:?}");
    }
    let saved_offset = download_offset(&queue, &download.id).unwrap();
    assert!(saved_offset >= MIB);
    let partial = partial_path(&target.path().join("down.bin")).unwrap();
    assert_eq!(fs::metadata(&partial).unwrap().len(), saved_offset);

    queue.start_all().unwrap();
    let (ids, _) = watch_upload(&queue, &upload.id).await;
    assert_eq!(ids, HashSet::from([upload_id]));
    wait_state(&queue, &download.id, TransferState::Completed).await;
    assert!(s3.get(&bucket, "up.bin").await == upload_data);
    assert!(fs::read(target.path().join("down.bin")).unwrap() == download_data);
    assert!(!partial.exists());
    assert_eq!(s3.pending_uploads(&bucket, "up.bin").await, 0);
    queue.shutdown().await.unwrap();
}

// ---- failure and retry ----

/// Relays TCP connections to a server; `cut` drops every connection and
/// refuses new ones until `restore`.
struct Proxy {
    endpoint: String,
    cut: Arc<AtomicBool>,
    connections: Arc<Mutex<Vec<AbortHandle>>>,
    accept: AbortHandle,
}

impl Proxy {
    async fn start(upstream: String) -> Proxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let cut = Arc::new(AtomicBool::new(false));
        let connections: Arc<Mutex<Vec<AbortHandle>>> = Arc::default();
        let accept = tokio::spawn({
            let cut = Arc::clone(&cut);
            let connections = Arc::clone(&connections);
            async move {
                loop {
                    let Ok((mut inbound, _)) = listener.accept().await else {
                        continue;
                    };
                    if cut.load(Ordering::SeqCst) {
                        continue;
                    }
                    let upstream = upstream.clone();
                    let relay = tokio::spawn(async move {
                        if let Ok(mut outbound) = tokio::net::TcpStream::connect(upstream).await {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                    });
                    connections.lock().unwrap().push(relay.abort_handle());
                }
            }
        })
        .abort_handle();
        Proxy {
            endpoint,
            cut,
            connections,
            accept,
        }
    }

    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        for connection in self.connections.lock().unwrap().drain(..) {
            connection.abort();
        }
    }

    fn restore(&self) {
        self.cut.store(false, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_upload_keeps_its_multipart_upload_for_the_retry() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let upstream = s3.endpoint().trim_start_matches("http://").to_owned();
    let proxy = Proxy::start(upstream).await;
    let mut account = s3.account().clone();
    account.endpoint = Some(proxy.endpoint.clone());
    let options = ClientOptions {
        max_attempts: 2,
        ..ClientOptions::default()
    };
    let via_proxy = S3::new(&account, &s3.credentials(), &options).unwrap();
    let queue = open(
        provider(&[(ACCOUNT, via_proxy)]),
        settings(Some(4096)),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let data = payload(11 * MIB as usize + 1, 10);
    let path = write_file(local.path(), "flaky.bin", &data);

    let task = enqueue_upload(&queue, ACCOUNT, std::slice::from_ref(&path), &bucket, "")
        .await
        .unwrap()[0]
        .clone();
    wait_for("the first part", || {
        upload_progress(&queue, &task.id).is_some_and(|(_, parts)| parts >= 1)
    })
    .await;
    proxy.cut();
    let failed = wait_state(&queue, &task.id, TransferState::Failed).await;
    assert!(failed.error.is_some());
    // The multipart upload and its finished parts are kept for the retry.
    let (upload_id, parts) = upload_progress(&queue, &task.id).unwrap();
    assert!(parts >= 1);
    assert_eq!(s3.pending_uploads(&bucket, "flaky.bin").await, 1);

    proxy.restore();
    queue.retry_task(&task.id).unwrap();
    let (ids, first_parts) = watch_upload(&queue, &task.id).await;
    assert_eq!(ids, HashSet::from([upload_id]), "the retry started over");
    assert!(first_parts.is_some_and(|n| n >= parts));
    assert!(s3.get(&bucket, "flaky.bin").await == data);
    assert_eq!(s3.pending_uploads(&bucket, "flaky.bin").await, 0);

    // Cancelling a failed upload aborts its multipart upload.
    let task = enqueue_upload(
        &queue,
        ACCOUNT,
        std::slice::from_ref(&path),
        &bucket,
        "again/",
    )
    .await
    .unwrap()[0]
        .clone();
    wait_for("the first part", || {
        upload_progress(&queue, &task.id).is_some_and(|(_, parts)| parts >= 1)
    })
    .await;
    proxy.cut();
    wait_state(&queue, &task.id, TransferState::Failed).await;
    proxy.restore();
    assert_eq!(s3.pending_uploads(&bucket, "again/flaky.bin").await, 1);
    queue.cancel_task(&task.id).unwrap();
    let cancelled = wait_state(&queue, &task.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert_eq!(s3.pending_uploads(&bucket, "again/flaky.bin").await, 0);
    assert!(!s3.exists(&bucket, "again/flaky.bin").await);
}

// ---- caps and accounts ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bandwidth_cap_holds_for_uploads_and_downloads_together() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let rate_kibps = 1024;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(Some(rate_kibps)),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let up = payload(3 * MIB as usize / 2, 11);
    let down = payload(3 * MIB as usize / 2, 12);
    let path = write_file(local.path(), "up.bin", &up);
    s3.put(&bucket, "down.bin", down.clone()).await;
    let target = tempfile::tempdir().unwrap();

    let started = Instant::now();
    let mut tasks = enqueue_upload(&queue, ACCOUNT, &[path], &bucket, "")
        .await
        .unwrap();
    tasks.extend(
        enqueue_download(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            &strings(&["down.bin"]),
            target.path(),
        )
        .await
        .unwrap(),
    );
    wait_all(&queue, &tasks, TransferState::Completed).await;
    let elapsed = started.elapsed();
    // 3 MiB at 1 MiB/s, less the limiter's initial burst of 1/8 s.
    let total = (up.len() + down.len()) as f64;
    let rate = (rate_kibps * 1024) as f64;
    let minimum = Duration::from_secs_f64((total - rate / 8.0) / rate);
    assert!(
        elapsed >= minimum.mul_f64(0.95),
        "{elapsed:?} is faster than the cap allows ({minimum:?})"
    );
    assert!(elapsed < minimum * 4, "{elapsed:?} is far too slow");
    assert!(s3.get(&bucket, "up.bin").await == up);
    assert!(fs::read(target.path().join("down.bin")).unwrap() == down);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_most_the_configured_number_of_tasks_run_at_once() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        TransferSettings {
            max_concurrent_tasks: 2,
            ..settings(Some(2048))
        },
        QueueOptions::default(),
    )
    .await;
    let mut events = queue.subscribe();
    let local = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = (0..5)
        .map(|i| {
            write_file(
                local.path(),
                &format!("f{i}.bin"),
                &payload(512 * 1024, 20 + i),
            )
        })
        .collect();
    let tasks = enqueue_upload(&queue, ACCOUNT, &paths, &bucket, "")
        .await
        .unwrap();
    let mut running = HashSet::new();
    let mut completed = HashSet::new();
    let mut peak = 0;
    while completed.len() < tasks.len() {
        let event = tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("no queue event")
            .expect("event stream closed");
        if let QueueEvent::Updated(task) = event {
            match task.state {
                TransferState::Running => {
                    running.insert(task.id);
                }
                TransferState::Completed => {
                    running.remove(&task.id);
                    completed.insert(task.id);
                }
                TransferState::Failed => panic!("{:?}", task.error),
                _ => {
                    running.remove(&task.id);
                }
            }
            peak = peak.max(running.len());
        }
    }
    assert_eq!(peak, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_accounts_share_one_queue() {
    let first = TestS3::start().await;
    let second = TestS3::start().await;
    let first_bucket = first.new_bucket().await;
    let second_bucket = second.new_bucket().await;
    let queue = open(
        provider(&[
            ("account-a", first.ops().clone()),
            ("account-b", second.ops().clone()),
        ]),
        settings(None),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let path = write_file(local.path(), "shared.txt", b"one file, two accounts");
    let a = enqueue_upload(
        &queue,
        "account-a",
        std::slice::from_ref(&path),
        &first_bucket,
        "",
    )
    .await
    .unwrap();
    let b = enqueue_upload(
        &queue,
        "account-b",
        std::slice::from_ref(&path),
        &second_bucket,
        "",
    )
    .await
    .unwrap();
    wait_all(&queue, &a, TransferState::Completed).await;
    wait_all(&queue, &b, TransferState::Completed).await;
    assert_eq!(
        first.get(&first_bucket, "shared.txt").await,
        b"one file, two accounts"
    );
    assert_eq!(
        second.get(&second_bucket, "shared.txt").await,
        b"one file, two accounts"
    );

    let unknown = enqueue_upload(&queue, "nobody", &[path], &first_bucket, "")
        .await
        .unwrap();
    let failed = wait_state(&queue, &unknown[0].id, TransferState::Failed).await;
    assert!(failed.error.unwrap().contains("nobody"));
}

// ---- selections ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entries_that_cannot_be_added_become_failed_tasks() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let queue = open(
        provider(&[(ACCOUNT, s3.ops().clone())]),
        settings(None),
        QueueOptions::default(),
    )
    .await;
    let local = tempfile::tempdir().unwrap();
    let good = write_file(local.path(), "good.txt", b"good");
    let missing = local.path().join("missing.txt");

    let tasks = enqueue_upload(&queue, ACCOUNT, &[good, missing.clone()], &bucket, "in/")
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    let group = group_of(&tasks);
    assert_eq!(tasks[0].state, TransferState::Queued);
    let rejected = &tasks[1];
    assert_eq!(rejected.state, TransferState::Failed);
    assert_eq!(rejected.group_id.as_deref(), Some(group.as_str()));
    assert_eq!(rejected.src, Location::Local { path: missing });
    let reason = rejected.error.clone().unwrap();
    assert!(reason.contains("missing.txt"), "{reason}");
    wait_state(&queue, &tasks[0].id, TransferState::Completed).await;
    assert_eq!(s3.get(&bucket, "in/good.txt").await, b"good");

    // A retry reports the same reason; a cancel touches nothing.
    queue.retry_task(&rejected.id).unwrap();
    let again = wait_state(&queue, &rejected.id, TransferState::Failed).await;
    assert_eq!(again.error.as_deref(), Some(reason.as_str()));
    assert_eq!(again.attempts, 1);
    queue.cancel_task(&rejected.id).unwrap();
    let cancelled = wait_state(&queue, &rejected.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);

    // Two selected entries for one destination: rejected, nothing added.
    let before = queue.list().len();
    let one = write_file(local.path(), "one/report.txt", b"1");
    let two = write_file(local.path(), "two/report.txt", b"2");
    let err = enqueue_upload(&queue, ACCOUNT, &[one, two], &bucket, "")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    for result in [
        enqueue_copy(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            &strings(&["a/report.txt", "b/report.txt"]),
            &bucket,
            "dst/",
        )
        .await,
        enqueue_move(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            &strings(&["a/report.txt", "b/report.txt"]),
            &bucket,
            "dst/",
        )
        .await,
        enqueue_download(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            &strings(&["a/report.txt", "b/report.txt"]),
            local.path(),
        )
        .await,
    ] {
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Conflict);
    }
    assert_eq!(
        queue.list().len(),
        before,
        "a rejected selection adds nothing"
    );

    // A delete of the bucket root is a failed entry next to the others.
    s3.put(&bucket, "x.txt", "x").await;
    let deletes = enqueue_delete(&queue, ACCOUNT, &bucket, &strings(&["x.txt", "/"]))
        .await
        .unwrap();
    assert_eq!(deletes.len(), 2);
    assert_eq!(deletes[1].state, TransferState::Failed);
    wait_state(&queue, &deletes[0].id, TransferState::Completed).await;
    assert!(!s3.exists(&bucket, "x.txt").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsafe_keys_become_failed_tasks_that_never_write() {
    fn list(request: &RecordedRequest) -> MockResponse {
        let prefix = request.query_param("prefix").unwrap_or_default();
        let contents: String = ["dir/ok.txt", "dir/../escape.txt"]
            .iter()
            .map(|key| {
                format!(
                    "<Contents><Key>{key}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"e\"</ETag><Size>2</Size></Contents>"
                )
            })
            .collect();
        MockResponse::xml(format!(
            "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>bucket</Name><Prefix>{prefix}</Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
        ))
    }
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").as_deref() == Some("2") {
            list(request)
        } else if request.method == "GET" && request.path == "/bucket/dir/ok.txt" {
            MockResponse {
                status: 200,
                headers: vec![
                    ("etag".into(), "\"e\"".into()),
                    (
                        "last-modified".into(),
                        "Thu, 29 Oct 2026 08:15:00 GMT".into(),
                    ),
                ],
                body: "ok".into(),
            }
        } else {
            MockResponse::error(
                400,
                "Unexpected",
                &format!("{} {}", request.method, request.path),
            )
        }
    });
    let queue = open(
        provider(&[(ACCOUNT, mock.ops())]),
        settings(None),
        QueueOptions::default(),
    )
    .await;
    let parent = tempfile::tempdir().unwrap();
    let dest = parent.path().join("dest");
    fs::create_dir(&dest).unwrap();
    let tasks = enqueue_download(
        &queue,
        &mock.ops(),
        ACCOUNT,
        "bucket",
        &strings(&["dir/"]),
        &dest,
    )
    .await
    .unwrap();
    let states: Vec<(String, TransferState)> = tasks
        .iter()
        .map(|t| match &t.src {
            Location::S3 { key, .. } => (key.clone(), t.state),
            Location::Local { .. } => panic!("{t:?}"),
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("dir/".to_owned(), TransferState::Queued),
            ("dir/ok.txt".to_owned(), TransferState::Queued),
            ("dir/../escape.txt".to_owned(), TransferState::Failed),
        ]
    );
    wait_state(&queue, &tasks[1].id, TransferState::Completed).await;
    assert_eq!(fs::read(dest.join("dir").join("ok.txt")).unwrap(), b"ok");

    let unsafe_task = &tasks[2];
    queue.retry_task(&unsafe_task.id).unwrap();
    let again = wait_state(&queue, &unsafe_task.id, TransferState::Failed).await;
    assert!(again.error.unwrap().contains("unsafe key"));
    queue.cancel_task(&unsafe_task.id).unwrap();
    wait_state(&queue, &unsafe_task.id, TransferState::Cancelled).await;
    // Nothing was ever written next to the destination folder.
    let beside: Vec<String> = fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(beside, ["dest"]);
    let requests = mock.requests();
    assert!(
        requests.iter().all(|r| !r.path.contains("escape")),
        "the unsafe key was requested"
    );
}
