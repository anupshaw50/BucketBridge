//! Copy and move between two accounts: the server-side route, the streaming
//! fallback, move safety, cancel, the overwrite policy and the queue.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bucketbridge_core::engine::{
    CopyRoute, Hooks, JobOptions, Outcome, Progress, S3Provider, S3Runner, SkipReason, StopSignal,
    TransferEngine, TransferOptions, enqueue_copy_with, enqueue_move_with, verify_copy,
};
use bucketbridge_core::model::{ObjectHead, TransferState, TransferTask};
use bucketbridge_core::settings::{DataIntegritySettings, IfExists, MIB, TransferSettings};
use bucketbridge_core::transfer::{QueueOptions, TransferQueue};
use bucketbridge_core::{ClientOptions, Error, ErrorKind, S3, S3Uri};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};
use md5::{Digest, Md5};

fn uri(bucket: &str, key: &str) -> S3Uri {
    S3Uri::new(bucket, key).unwrap()
}

fn payload(len: usize, seed: u32) -> Vec<u8> {
    let mut state: u32 = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn small_parts() -> TransferOptions {
    TransferOptions {
        multipart_threshold: 5 * MIB,
        part_size: 5 * MIB,
        max_parts_in_flight: 2,
        ..TransferOptions::default()
    }
}

fn engine(s3: &S3, options: TransferOptions) -> TransferEngine {
    TransferEngine::new(s3.clone(), options).unwrap()
}

async fn copy_to(
    from: &TransferEngine,
    to: &TransferEngine,
    src: &S3Uri,
    dst: &S3Uri,
) -> Outcome<(bucketbridge_core::model::CopyOutcome, CopyRoute)> {
    from.copy_to(to, src, dst, &Hooks::new(), None, &|_| {})
        .await
        .unwrap()
}

fn completed(
    outcome: Outcome<(bucketbridge_core::model::CopyOutcome, CopyRoute)>,
) -> (bucketbridge_core::model::CopyOutcome, CopyRoute) {
    match outcome {
        Outcome::Completed(done) => done,
        other => panic!("not completed: {other:?}"),
    }
}

// ---- a scripted object store with data, for the paths s3s-fs cannot show ----

type Stored = (Vec<u8>, BTreeMap<String, String>);

#[derive(Default)]
struct MiniStore {
    objects: Mutex<HashMap<(String, String), Stored>>,
    deny_copy: AtomicBool,
    fail_puts: AtomicBool,
    truncate_puts: AtomicBool,
}

impl MiniStore {
    fn put(&self, bucket: &str, key: &str, data: &[u8]) {
        self.objects.lock().unwrap().insert(
            (bucket.to_owned(), key.to_owned()),
            (data.to_vec(), BTreeMap::new()),
        );
    }

    fn data(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .unwrap()
            .get(&(bucket.to_owned(), key.to_owned()))
            .map(|(data, _)| data.clone())
    }

    fn handle(&self, request: &RecordedRequest) -> MockResponse {
        let path = request.path.trim_start_matches('/');
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        let (bucket, key) = (bucket.to_owned(), key.replace("%20", " "));
        let etag = |data: &[u8]| {
            let hex: String = Md5::digest(data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("\"{hex}\"")
        };
        let slot = (bucket.clone(), key.clone());
        match request.method.as_str() {
            "HEAD" => match self.objects.lock().unwrap().get(&slot) {
                None => MockResponse::error(404, "NoSuchKey", "missing"),
                Some((data, meta)) => {
                    let mut reply = MockResponse::empty(200)
                        .with_header("content-length", &data.len().to_string())
                        .with_header("etag", &etag(data))
                        .with_header("last-modified", "Wed, 21 Oct 2015 07:28:00 GMT");
                    for (name, value) in meta {
                        reply = reply.with_header(&format!("x-amz-meta-{name}"), value);
                    }
                    reply
                }
            },
            "GET" => match self.objects.lock().unwrap().get(&slot) {
                None => MockResponse::error(404, "NoSuchKey", "missing"),
                Some((data, _)) => {
                    let range = request
                        .header("range")
                        .and_then(|range| range.strip_prefix("bytes="))
                        .and_then(|range| range.split_once('-'))
                        .and_then(|(a, b)| {
                            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                        });
                    let (start, end) = range.unwrap_or((0, data.len().saturating_sub(1)));
                    let slice = &data[start..=end.min(data.len() - 1)];
                    let mut reply = MockResponse::empty(if range.is_some() { 206 } else { 200 })
                        .with_header("etag", &etag(data))
                        .with_header("last-modified", "Wed, 21 Oct 2015 07:28:00 GMT")
                        .with_header(
                            "content-range",
                            &format!("bytes {start}-{}/{}", start + slice.len() - 1, data.len()),
                        );
                    reply.body = String::from_utf8(slice.to_vec()).unwrap();
                    reply
                }
            },
            "PUT" if request.header("x-amz-copy-source").is_some() => {
                if self.deny_copy.load(Ordering::SeqCst) {
                    return MockResponse::error(403, "AccessDenied", "Access Denied");
                }
                let source = request.header("x-amz-copy-source").unwrap().to_owned();
                let source = source.trim_start_matches('/');
                let (sb, sk) = source.split_once('/').unwrap();
                let found = self
                    .objects
                    .lock()
                    .unwrap()
                    .get(&(sb.to_owned(), sk.to_owned()))
                    .cloned();
                match found {
                    None => MockResponse::error(404, "NoSuchKey", "missing"),
                    Some(entry) => {
                        let tag = etag(&entry.0);
                        self.objects.lock().unwrap().insert(slot, entry);
                        MockResponse::xml(format!(
                            "<CopyObjectResult><ETag>{tag}</ETag></CopyObjectResult>"
                        ))
                    }
                }
            }
            "PUT" => {
                if self.fail_puts.load(Ordering::SeqCst) {
                    return MockResponse::error(500, "InternalError", "injected");
                }
                let mut data = request.body.clone();
                if self.truncate_puts.load(Ordering::SeqCst) {
                    data.truncate(data.len() / 2);
                }
                let meta: BTreeMap<String, String> = request
                    .headers
                    .iter()
                    .filter_map(|(name, value)| {
                        name.strip_prefix("x-amz-meta-")
                            .map(|n| (n.to_owned(), value.clone()))
                    })
                    .collect();
                let tag = etag(&data);
                self.objects.lock().unwrap().insert(slot, (data, meta));
                MockResponse::empty(200).with_header("etag", &tag)
            }
            "DELETE" => {
                self.objects.lock().unwrap().remove(&slot);
                MockResponse::empty(204)
            }
            _ => MockResponse::error(400, "UnexpectedRequest", &request.path),
        }
    }
}

fn mini() -> (Arc<MiniStore>, MockS3) {
    let store = Arc::new(MiniStore::default());
    let handler = Arc::clone(&store);
    let mock = MockS3::start(move |request| handler.handle(request));
    (store, mock)
}

// ---- routes ----

#[tokio::test]
async fn same_endpoint_copies_on_the_server_when_the_destination_account_may_read() {
    let s3 = TestS3::start().await;
    let other = s3.connect(&s3.credentials(), &ClientOptions::default());
    let (src, dst) = (s3.new_bucket().await, s3.new_bucket().await);
    s3.put(&src, "a.txt", b"hello".to_vec()).await;
    let from = engine(s3.ops(), TransferOptions::default());
    let to = engine(&other, TransferOptions::default());
    let (done, route) =
        completed(copy_to(&from, &to, &uri(&src, "a.txt"), &uri(&dst, "b.txt")).await);
    assert_eq!(route, CopyRoute::ServerSide);
    assert_eq!(done.size, 5);
    assert_eq!(s3.get(&dst, "b.txt").await, b"hello");
}

#[tokio::test]
async fn different_endpoints_stream_through_this_machine_with_parts_and_metadata() {
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    let (src, dst) = (first.new_bucket().await, second.new_bucket().await);
    let big = payload(11 * MIB as usize + 123, 7);
    first.put(&src, "big.bin", big.clone()).await;
    first.put(&src, "small.txt", b"tiny".to_vec()).await;
    let from = engine(first.ops(), small_parts());
    let to = engine(second.ops(), small_parts());

    let (done, route) = completed(
        copy_to(
            &from,
            &to,
            &uri(&src, "big.bin"),
            &uri(&dst, "copy/big.bin"),
        )
        .await,
    );
    assert_eq!(route, CopyRoute::Streamed);
    assert_eq!(done.size, big.len() as u64);
    assert_eq!(done.parts, 3);
    assert_eq!(second.get(&dst, "copy/big.bin").await, big);
    assert_eq!(second.pending_uploads(&dst, "copy/big.bin").await, 0);

    let (_, route) =
        completed(copy_to(&from, &to, &uri(&src, "small.txt"), &uri(&dst, "small.txt")).await);
    assert_eq!(route, CopyRoute::Streamed);
    assert_eq!(second.get(&dst, "small.txt").await, b"tiny");

    // a folder marker is created in the other account
    let (_, _) = completed(copy_to(&from, &to, &uri(&src, "folder/"), &uri(&dst, "folder/")).await);

    // progress counts the bytes once
    let counter = Arc::new(bucketbridge_core::engine::ProgressCounter::new());
    let hooks = Hooks::new().with_progress(counter.clone());
    let outcome = from
        .copy_to(
            &to,
            &uri(&src, "big.bin"),
            &uri(&dst, "again.bin"),
            &hooks,
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(counter.done(), big.len() as u64);
    assert_eq!(counter.total(), Some(big.len() as u64));
}

#[tokio::test]
async fn access_denied_falls_back_to_streaming_and_requester_pays_is_sent_on_both_sides() {
    let (store, mock) = mini();
    store.deny_copy.store(true, Ordering::SeqCst);
    store.put("src", "a.txt", b"payload");
    let options = ClientOptions::default();
    let from = engine(&mock.ops_with(&options, true), TransferOptions::default());
    let to = engine(&mock.ops_with(&options, true), TransferOptions::default());

    let (done, route) =
        completed(copy_to(&from, &to, &uri("src", "a.txt"), &uri("dst", "a.txt")).await);
    assert_eq!(route, CopyRoute::Streamed);
    assert_eq!(done.size, 7);
    assert_eq!(store.data("dst", "a.txt").unwrap(), b"payload");

    let requests = mock.requests();
    // the server-side attempt came first and was refused
    let copy_attempt = requests
        .iter()
        .position(|r| r.header("x-amz-copy-source").is_some())
        .expect("a CopyObject attempt");
    let range_read = requests
        .iter()
        .position(|r| r.method == "GET" && r.header("range").is_some())
        .expect("a ranged GET of the source");
    assert!(copy_attempt < range_read);
    // requester pays on the source read and on the destination write
    let get = &requests[range_read];
    assert_eq!(get.header("x-amz-request-payer"), Some("requester"));
    let put = requests
        .iter()
        .find(|r| r.method == "PUT" && r.header("x-amz-copy-source").is_none())
        .expect("a PUT of the destination");
    assert_eq!(put.header("x-amz-request-payer"), Some("requester"));
    assert!(put.path.contains("/dst/"));
}

#[tokio::test]
async fn a_server_side_copy_is_used_when_it_works() {
    let (store, mock) = mini();
    store.put("src", "a.txt", b"payload");
    let from = engine(&mock.ops(), TransferOptions::default());
    let to = engine(
        &mock.ops_with(&ClientOptions::default(), false),
        TransferOptions::default(),
    );
    let (_, route) =
        completed(copy_to(&from, &to, &uri("src", "a.txt"), &uri("dst", "a.txt")).await);
    assert_eq!(route, CopyRoute::ServerSide);
    assert!(
        mock.requests()
            .iter()
            .all(|r| !(r.method == "GET" && r.header("range").is_some())),
        "no data was read through this machine"
    );
}

// ---- move safety ----

#[tokio::test]
async fn move_deletes_the_source_only_after_the_copy_was_verified() {
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    let (src, dst) = (first.new_bucket().await, second.new_bucket().await);
    first.put(&src, "a.txt", b"hello".to_vec()).await;
    let from = engine(first.ops(), TransferOptions::default());
    let to = engine(second.ops(), TransferOptions::default());
    let outcome = from
        .move_to(
            &to,
            &uri(&src, "a.txt"),
            &uri(&dst, "a.txt"),
            &Hooks::new(),
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(second.get(&dst, "a.txt").await, b"hello");
    assert!(!first.exists(&src, "a.txt").await);
}

#[tokio::test]
async fn a_failed_or_unverifiable_copy_keeps_the_source() {
    let (store, mock) = mini();
    store.deny_copy.store(true, Ordering::SeqCst);
    store.put("src", "a.txt", b"0123456789");
    let from = engine(&mock.ops(), TransferOptions::default());
    let to = engine(
        &mock.ops_with(&ClientOptions::default(), false),
        TransferOptions::default(),
    );

    // the destination refuses the write
    store.fail_puts.store(true, Ordering::SeqCst);
    let err = from
        .move_to(
            &to,
            &uri("src", "a.txt"),
            &uri("dst", "a.txt"),
            &Hooks::new(),
            None,
            &|_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote);
    assert!(store.data("src", "a.txt").is_some(), "the source must stay");
    assert!(store.data("dst", "a.txt").is_none());

    // the destination stores something other than what was sent
    store.fail_puts.store(false, Ordering::SeqCst);
    store.truncate_puts.store(true, Ordering::SeqCst);
    let err = from
        .move_to(
            &to,
            &uri("src", "a.txt"),
            &uri("dst", "a.txt"),
            &Hooks::new(),
            None,
            &|_| {},
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("could not be verified"), "{err}");
    assert!(store.data("src", "a.txt").is_some(), "the source must stay");

    // a good copy moves
    store.truncate_puts.store(false, Ordering::SeqCst);
    let outcome = from
        .move_to(
            &to,
            &uri("src", "a.txt"),
            &uri("dst", "a.txt"),
            &Hooks::new(),
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert!(store.data("src", "a.txt").is_none());
    assert_eq!(store.data("dst", "a.txt").unwrap(), b"0123456789");
}

#[test]
fn verification_compares_sizes_plain_etags_and_checksums_of_the_same_kind() {
    let head = |size: u64, etag: &str| ObjectHead {
        size,
        etag: Some(etag.to_owned()),
        ..ObjectHead::default()
    };
    verify_copy(&head(3, "aa"), &head(3, "aa")).unwrap();
    assert!(verify_copy(&head(3, "aa"), &head(4, "aa")).is_err());
    assert!(verify_copy(&head(3, "aa"), &head(3, "bb")).is_err());
    // a multipart ETag is not comparable with a single-part one
    verify_copy(&head(3, "aa-2"), &head(3, "bb")).unwrap();
    let mut a = head(3, "aa-2");
    let mut b = head(3, "bb-3");
    a.checksums.crc32 = Some("AAAA".into());
    b.checksums.crc32 = Some("AAAA".into());
    verify_copy(&a, &b).unwrap();
    b.checksums.crc32 = Some("BBBB".into());
    assert!(verify_copy(&a, &b).is_err());
    // composite values with different part counts are not comparable
    a.checksums.crc32 = Some("AAAA-2".into());
    b.checksums.crc32 = Some("BBBB-3".into());
    verify_copy(&a, &b).unwrap();
    b.checksums.crc32 = Some("BBBB-2".into());
    assert!(verify_copy(&a, &b).is_err());
}

// ---- cancel ----

/// Cancels the run once `limit` bytes were reported.
struct CancelAfter {
    stop: StopSignal,
    limit: u64,
    seen: std::sync::atomic::AtomicU64,
}

impl Progress for CancelAfter {
    fn set_total(&self, _total: u64) {}

    fn set_done(&self, _done: u64) {}

    fn add_done(&self, amount: u64) {
        if self.seen.fetch_add(amount, Ordering::SeqCst) + amount >= self.limit {
            self.stop.cancel();
        }
    }
}

#[tokio::test]
async fn cancelling_a_streamed_copy_leaves_nothing_behind() {
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    let (src, dst) = (first.new_bucket().await, second.new_bucket().await);
    first
        .put(&src, "big.bin", payload(16 * MIB as usize, 3))
        .await;
    let from = engine(first.ops(), small_parts());
    let to = engine(second.ops(), small_parts());
    let stop = StopSignal::new();
    let hooks = Hooks::new()
        .with_progress(Arc::new(CancelAfter {
            stop: stop.clone(),
            limit: 5 * MIB,
            seen: 0.into(),
        }))
        .with_stop(stop);
    let outcome = from
        .copy_to(
            &to,
            &uri(&src, "big.bin"),
            &uri(&dst, "big.bin"),
            &hooks,
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!second.exists(&dst, "big.bin").await);
    assert_eq!(second.pending_uploads(&dst, "big.bin").await, 0);
    assert!(first.exists(&src, "big.bin").await);
}

// ---- policy ----

#[tokio::test]
async fn the_overwrite_policy_applies_to_the_destination_account() {
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    let (src, dst) = (first.new_bucket().await, second.new_bucket().await);
    first.put(&src, "a.txt", b"new".to_vec()).await;
    second.put(&dst, "a.txt", b"old".to_vec()).await;
    let from = engine(first.ops(), TransferOptions::default());
    let to = engine(second.ops(), TransferOptions::default());

    let skipping = from.with_if_exists(IfExists::Skip);
    let outcome = skipping
        .move_to(
            &to,
            &uri(&src, "a.txt"),
            &uri(&dst, "a.txt"),
            &Hooks::new(),
            None,
            &|_| {},
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Skipped(SkipReason::Exists));
    assert_eq!(second.get(&dst, "a.txt").await, b"old");
    assert!(
        first.exists(&src, "a.txt").await,
        "a skipped move keeps its source"
    );

    let renaming = from.with_if_exists(IfExists::Rename);
    let (done, _) = completed(
        renaming
            .copy_to(
                &to,
                &uri(&src, "a.txt"),
                &uri(&dst, "a.txt"),
                &Hooks::new(),
                None,
                &|_| {},
            )
            .await
            .unwrap(),
    );
    assert_eq!(done.renamed_to.as_deref(), Some("a (1).txt"));
    assert_eq!(second.get(&dst, "a (1).txt").await, b"new");
    assert_eq!(second.get(&dst, "a.txt").await, b"old");
}

// ---- the queue ----

fn two_accounts(first: &TestS3, second: &TestS3) -> S3Provider {
    let accounts: HashMap<String, S3> = HashMap::from([
        ("account-1".to_owned(), first.ops().clone()),
        ("account-2".to_owned(), second.ops().clone()),
    ]);
    let accounts = Arc::new(accounts);
    Arc::new(move |id: String| {
        let accounts = Arc::clone(&accounts);
        Box::pin(async move {
            accounts
                .get(&id)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("account {id}")))
        })
    })
}

async fn wait_done(queue: &TransferQueue, tasks: &[TransferTask]) -> Vec<TransferTask> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let now: Vec<TransferTask> = tasks.iter().map(|t| queue.get(&t.id).unwrap()).collect();
        if now.iter().all(|t| {
            matches!(
                t.state,
                TransferState::Completed | TransferState::Skipped | TransferState::Failed
            )
        }) {
            return now;
        }
        assert!(Instant::now() < deadline, "tasks did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn the_queue_copies_and_moves_folders_between_accounts() {
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    let (src, dst) = (first.new_bucket().await, second.new_bucket().await);
    let big = payload(6 * MIB as usize, 9);
    first.put(&src, "d/big.bin", big.clone()).await;
    first.put(&src, "d/sub/a.txt", b"A".to_vec()).await;
    first.put(&src, "d/b.txt", b"B".to_vec()).await;
    let runner = S3Runner::new(
        two_accounts(&first, &second),
        DataIntegritySettings::default(),
    );
    let queue = TransferQueue::open(
        TransferSettings {
            multipart_threshold_mib: 5,
            part_size_mib: 5,
            ..TransferSettings::default()
        },
        runner.runners(),
        QueueOptions::default(),
    )
    .await
    .unwrap();
    let options = JobOptions {
        dst_account: Some("account-2".into()),
        ..JobOptions::default()
    };

    let tasks = enqueue_copy_with(
        &queue,
        first.ops(),
        "account-1",
        &src,
        &["d/".to_owned()],
        &dst,
        "copied/",
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert!(
        done.iter().all(|t| t.state == TransferState::Completed),
        "{done:?}"
    );
    assert_eq!(second.get(&dst, "copied/d/big.bin").await, big);
    assert_eq!(second.get(&dst, "copied/d/sub/a.txt").await, b"A");
    assert!(first.exists(&src, "d/b.txt").await);

    let tasks = enqueue_move_with(
        &queue,
        first.ops(),
        "account-1",
        &src,
        &["d/".to_owned()],
        &dst,
        "moved/",
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert!(
        done.iter().all(|t| t.state == TransferState::Completed),
        "{done:?}"
    );
    assert_eq!(second.get(&dst, "moved/d/big.bin").await, big);
    for key in ["d/big.bin", "d/sub/a.txt", "d/b.txt"] {
        assert!(!first.exists(&src, key).await, "{key} should be gone");
    }

    // with the policy `skip`, items that exist in the destination account are
    // reported as skipped and their sources stay
    first.put(&src, "e/x.txt", b"X".to_vec()).await;
    second.put(&dst, "e/x.txt", b"keep".to_vec()).await;
    let skip = JobOptions {
        dst_account: Some("account-2".into()),
        if_exists: Some(IfExists::Skip),
        ..JobOptions::default()
    };
    let tasks = enqueue_move_with(
        &queue,
        first.ops(),
        "account-1",
        &src,
        &["e/x.txt".to_owned()],
        &dst,
        "e/",
        &skip,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(done[0].state, TransferState::Skipped);
    assert_eq!(second.get(&dst, "e/x.txt").await, b"keep");
    assert!(first.exists(&src, "e/x.txt").await);
}
