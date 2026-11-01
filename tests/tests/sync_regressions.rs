//! Regression tests for the data-safety findings of the sync review: targets the analysis
//! did not see, changes made while a transfer runs, leftovers of failed rows, links and
//! reserved names, the emptied-folder pass, adopted times, and rows whose result was lost.
//! Every test asserts the CORRECT behaviour; each failed on the code it was written against.
//!
//! The analysis is real (listings from s3s-fs); the executor runs with the real engines.
//! `MockS3` and a truncating endpoint stand in for servers that fail or answer on demand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use bucketbridge_core::engine::{TransferEngine, TransferOptions};
use bucketbridge_core::model::{
    Account, FileMeta, FilterSpec, ObjectEntry, Side, SyncAction, SyncDirection, SyncPlan, SyncRow,
};
use bucketbridge_core::sync::{
    self, AnalyzeRequest, ExecuteOptions, NoSyncEvents, RunSummary, SyncExecutor,
};
use bucketbridge_core::transfer::BandwidthLimiter;
use bucketbridge_core::{ClientOptions, Credentials, Filter, S3, S3Uri, SyncStore};
use bucketbridge_tests::{MockResponse, MockS3, TestS3};

const PREFIX: &str = "sync/";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const HTTP_DATE: &str = "Wed, 21 Oct 2015 07:28:00 GMT";

fn hour() -> Duration {
    Duration::from_secs(3600)
}

/// One S3 server, one bucket, a local folder and a plan store.
struct Fx {
    s3: TestS3,
    bucket: String,
    // Read by the Unix-only tests; elsewhere it only keeps the folder alive.
    #[cfg_attr(not(unix), allow(dead_code))]
    dir: tempfile::TempDir,
    root: PathBuf,
    executor: SyncExecutor,
}

impl Fx {
    async fn new() -> Fx {
        let s3 = TestS3::start().await;
        let bucket = s3.new_bucket().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("local");
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
        Fx {
            s3,
            bucket,
            dir,
            root,
            executor: SyncExecutor::new(store),
        }
    }

    fn prefix(&self) -> S3Uri {
        S3Uri::new(&self.bucket, PREFIX).unwrap()
    }

    async fn put(&self, rel: &str, body: &str) {
        self.s3
            .put(&self.bucket, &format!("{PREFIX}{rel}"), body)
            .await;
    }

    async fn remote(&self, rel: &str) -> Option<Vec<u8>> {
        let key = format!("{PREFIX}{rel}");
        if self.s3.exists(&self.bucket, &key).await {
            Some(self.s3.get(&self.bucket, &key).await)
        } else {
            None
        }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn local(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root.join(rel)).ok()
    }

    fn set_age(&self, rel: &str, modified: SystemTime) {
        set_mtime(&self.root.join(rel), modified);
    }

    async fn analyze(
        &self,
        direction: SyncDirection,
        delete: bool,
        filter: Option<&Filter>,
    ) -> SyncPlan {
        let listing = self.s3.ops().list_all(&self.bucket, PREFIX).await.unwrap();
        let prefix = self.prefix();
        let mut request = AnalyzeRequest::new("acct", &self.root, &prefix, direction);
        request.delete = delete;
        request.filter = filter;
        sync::analyze(&request, listing).unwrap()
    }

    async fn plan(
        &self,
        direction: SyncDirection,
        delete: bool,
        filter: Option<&Filter>,
    ) -> SyncPlan {
        let plan = self.analyze(direction, delete, filter).await;
        self.executor.store().insert(&plan).unwrap();
        plan
    }

    fn engine(&self, options: TransferOptions) -> TransferEngine {
        TransferEngine::new(self.s3.ops().clone(), options).unwrap()
    }

    async fn execute(&self, plan_id: &str) -> RunSummary {
        let options = ExecuteOptions {
            concurrency: 4,
            limiter: BandwidthLimiter::unlimited(),
        };
        self.executor
            .start(
                plan_id,
                self.engine(TransferOptions::default()),
                options,
                Arc::new(NoSyncEvents),
            )
            .unwrap()
            .run()
            .await
            .unwrap()
    }

    fn stored(&self, plan_id: &str) -> SyncPlan {
        self.executor.store().get(plan_id).unwrap()
    }
}

fn set_mtime(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn row<'p>(plan: &'p SyncPlan, rel: &str) -> &'p SyncRow {
    plan.rows
        .iter()
        .find(|row| row.rel_path == rel)
        .unwrap_or_else(|| panic!("no row {rel}"))
}

/// Every file or folder below `root` whose name ends in `.bbpart`.
fn leftovers(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(".bbpart") {
                found.push(path.clone());
            }
            if path.is_dir()
                && !fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            {
                pending.push(path);
            }
        }
    }
    found
}

async fn wait_for_partial(root: &Path) {
    let started = Instant::now();
    while leftovers(root).is_empty() && started.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!leftovers(root).is_empty(), "the download did not start");
}

fn case_insensitive(dir: &Path) -> bool {
    let probe = dir.join("CaseProbe.tmp");
    fs::write(&probe, "x").unwrap();
    let answer = dir.join("caseprobe.TMP").exists();
    fs::remove_file(&probe).unwrap();
    answer
}

// ---------------------------------------------------------------------------------------
// 1. Targets the analysis did not see
// ---------------------------------------------------------------------------------------

/// A path filter hides local names from the planner. With the filter prefix `docs/`, the
/// local `Docs/a.txt` is invisible and `docs/a.txt` looked like a new file; on a
/// case-insensitive file system the download replaced the local file.
#[tokio::test]
async fn names_hidden_by_a_filter_are_never_overwritten() {
    let fx = Fx::new().await;
    if !case_insensitive(&fx.root) {
        eprintln!("SKIP: case-sensitive file system");
        return;
    }
    fx.write("Docs/a.txt", "precious local only");
    fx.put("docs/a.txt", "remote").await;
    let filter = Filter::compile(&FilterSpec {
        prefix: Some("docs/".to_owned()),
        ..FilterSpec::default()
    })
    .unwrap();
    let plan = fx.plan(SyncDirection::Download, false, Some(&filter)).await;
    let planned = row(&plan, "docs/a.txt");
    assert_eq!(planned.action, SyncAction::Failed, "{planned:?}");
    assert!(
        planned
            .error
            .as_deref()
            .unwrap()
            .contains("never overwritten"),
        "{planned:?}"
    );
    let summary = fx.execute(&plan.id).await;
    assert_eq!(summary.attempted, 0);
    assert_eq!(fx.local("Docs/a.txt").unwrap(), "precious local only");

    // The same with a glob, which is case-sensitive: `A.TXT` is out of scope for `*.txt`.
    let fx = Fx::new().await;
    fx.write("A.TXT", "precious local only");
    fx.put("a.txt", "remote").await;
    let filter = Filter::compile(&FilterSpec {
        glob: Some("*.txt".to_owned()),
        ..FilterSpec::default()
    })
    .unwrap();
    let plan = fx.plan(SyncDirection::Download, false, Some(&filter)).await;
    assert_eq!(row(&plan, "a.txt").action, SyncAction::Failed);
    fx.execute(&plan.id).await;
    assert_eq!(fx.local("A.TXT").unwrap(), "precious local only");
}

/// Whatever the analysis let through: a local entry that appeared after it is never
/// replaced, wherever it is (a file, or a folder at the destination).
#[tokio::test]
async fn a_local_entry_that_appears_after_the_analysis_is_never_replaced() {
    let fx = Fx::new().await;
    fx.put("x.txt", "remote x").await;
    fx.put("report.bin", &"D".repeat(64 * 1024)).await;
    let plan = fx.plan(SyncDirection::Download, false, None).await;
    fx.write("x.txt", "created meanwhile");
    fx.write("report.bin/inside.txt", "user data");
    let summary = fx.execute(&plan.id).await;
    assert_eq!((summary.attempted, summary.errors), (2, 2));
    assert_eq!(fx.local("x.txt").unwrap(), "created meanwhile");
    assert_eq!(fx.local("report.bin/inside.txt").unwrap(), "user data");
    let stored = fx.stored(&plan.id);
    for rel in ["x.txt", "report.bin"] {
        let failed = row(&stored, rel);
        assert!(!failed.done);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("never overwritten"),
            "{failed:?}"
        );
    }
    // The failed rows left no partial file behind.
    assert!(leftovers(&fx.root).is_empty(), "{:?}", leftovers(&fx.root));
}

/// One bucket holding the composed and the decomposed spelling of a name: macOS file
/// systems have one file for both. The planner never plans such a pair as two copies.
#[tokio::test]
async fn composed_and_decomposed_keys_are_failed_rows_not_two_copies_onto_one_file() {
    let fx = Fx::new().await;
    let nfc = "caf\u{e9}.txt";
    let nfd = "cafe\u{301}.txt";
    // s3s-fs keeps keys in a directory of this very file system, which may merge the two
    // names, so the listing is built by hand.
    let listing = vec![
        ObjectEntry {
            key: format!("{PREFIX}{nfc}"),
            size: 10,
            ..ObjectEntry::default()
        },
        ObjectEntry {
            key: format!("{PREFIX}{nfd}"),
            size: 20,
            ..ObjectEntry::default()
        },
    ];
    let prefix = fx.prefix();
    for direction in [SyncDirection::Download, SyncDirection::Bidirectional] {
        for _ in 0..2 {
            let request = AnalyzeRequest::new("acct", &fx.root, &prefix, direction);
            let plan = sync::analyze(&request, listing.clone()).unwrap();
            assert_eq!(plan.rows.len(), 2);
            for planned in &plan.rows {
                assert_eq!(planned.action, SyncAction::Failed, "{planned:?}");
                assert!(
                    planned
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("Unicode normalization")
                );
            }
        }
    }
}

/// An object that appears after the analysis is not replaced by an upload that planned a
/// new object (one-way and bidirectional).
#[tokio::test]
async fn an_object_that_appears_after_the_analysis_is_never_overwritten() {
    for direction in [SyncDirection::Upload, SyncDirection::Bidirectional] {
        let fx = Fx::new().await;
        fx.write("new.txt", "local content");
        let plan = fx.plan(direction, false, None).await;
        assert_eq!(row(&plan, "new.txt").target, Some(Side::Remote));
        fx.put("new.txt", "created by someone else meanwhile").await;
        let summary = fx.execute(&plan.id).await;
        assert_eq!(summary.errors, 1, "{direction:?}");
        assert_eq!(
            fx.remote("new.txt").await.unwrap(),
            b"created by someone else meanwhile"
        );
        let stored = fx.stored(&plan.id);
        let failed = row(&stored, "new.txt");
        assert!(!failed.done);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("never overwritten")
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. Changes made while a transfer runs
// ---------------------------------------------------------------------------------------

/// Bidirectional download: the local file is edited while the newer object is downloaded.
/// The check before the transfer cannot see that; the one right before the file is moved
/// into place does.
#[tokio::test]
async fn a_local_edit_during_a_bidirectional_download_is_kept() {
    let body = "R".repeat(256 * 1024);
    let fx = Fx::new().await;
    fx.put("doc.txt", &body).await;
    fx.write("doc.txt", "old local");
    fx.set_age("doc.txt", SystemTime::now() - hour());
    let plan = fx.plan(SyncDirection::Bidirectional, false, None).await;
    assert_eq!(row(&plan, "doc.txt").target, Some(Side::Local));
    let slow = ExecuteOptions {
        concurrency: 1,
        limiter: BandwidthLimiter::new(NonZeroU64::new(96 * 1024)),
    };
    let run = fx
        .executor
        .start(
            &plan.id,
            fx.engine(TransferOptions::default()),
            slow,
            Arc::new(NoSyncEvents),
        )
        .unwrap();
    let handle = tokio::spawn(run.run());
    wait_for_partial(&fx.root).await;
    fx.write("doc.txt", "EDITED LOCALLY DURING THE DOWNLOAD");
    let summary = handle.await.unwrap().unwrap();
    assert_eq!(summary.errors, 1);
    assert_eq!(
        fx.local("doc.txt").unwrap(),
        "EDITED LOCALLY DURING THE DOWNLOAD"
    );
    let stored = fx.stored(&plan.id);
    assert!(!stored.rows[0].done);
    assert!(
        stored.rows[0].error.as_deref().unwrap().contains("changed"),
        "{:?}",
        stored.rows[0]
    );
    assert!(leftovers(&fx.root).is_empty(), "{:?}", leftovers(&fx.root));
}

/// Bidirectional upload: the object is replaced while the newer file is uploaded. The upload
/// asks the server to complete only if the object still has the ETag the check saw
/// (`If-Match`), so the edit survives, the row fails, and no multipart upload stays open.
/// (A server evaluates the condition when it commits the object; s3s-fs does so at
/// `CompleteMultipartUpload`, so this uses a multipart upload.)
#[tokio::test]
async fn a_remote_edit_during_a_bidirectional_upload_is_refused_by_the_server() {
    let fx = Fx::new().await;
    fx.put("doc.txt", "old remote").await;
    fx.write("doc.txt", &"L".repeat(4096));
    fx.set_age("doc.txt", SystemTime::now() + hour());
    let plan = fx.plan(SyncDirection::Bidirectional, false, None).await;
    assert_eq!(row(&plan, "doc.txt").target, Some(Side::Remote));
    let slow = ExecuteOptions {
        concurrency: 1,
        limiter: BandwidthLimiter::new(NonZeroU64::new(1024)),
    };
    let multipart = TransferOptions {
        multipart_threshold: 1,
        ..TransferOptions::default()
    };
    let run = fx
        .executor
        .start(&plan.id, fx.engine(multipart), slow, Arc::new(NoSyncEvents))
        .unwrap();
    let handle = tokio::spawn(run.run());
    tokio::time::sleep(Duration::from_millis(700)).await;
    fx.put("doc.txt", "EDITED REMOTELY DURING THE UPLOAD").await;
    let summary = handle.await.unwrap().unwrap();
    assert_eq!(summary.errors, 1);
    assert_eq!(
        fx.remote("doc.txt").await.unwrap(),
        b"EDITED REMOTELY DURING THE UPLOAD"
    );
    let stored = fx.stored(&plan.id);
    assert!(!stored.rows[0].done);
    assert!(
        stored.rows[0]
            .error
            .as_deref()
            .unwrap()
            .contains("changed since the analysis"),
        "{:?}",
        stored.rows[0]
    );
    assert_eq!(fx.s3.pending_uploads(&fx.bucket, "sync/doc.txt").await, 0);
}

fn head_response(etag: &str, size: usize) -> MockResponse {
    MockResponse {
        status: 200,
        headers: vec![
            ("etag".into(), format!("\"{etag}\"")),
            ("last-modified".into(), HTTP_DATE.into()),
            ("content-length".into(), size.to_string()),
        ],
        body: String::new(),
    }
}

fn mock_plan(root: &Path, direction: &str, remote: Option<ObjectEntry>) -> SyncPlan {
    make_plan(
        root,
        direction,
        false,
        vec![row_of(
            "doc.txt",
            SyncAction::Copy,
            Some(Side::Remote),
            Some(FileMeta {
                size: 3,
                modified: None,
            }),
            remote,
            false,
        )],
    )
}

fn planned_object() -> ObjectEntry {
    ObjectEntry {
        key: "sync/doc.txt".to_owned(),
        size: 3,
        etag: Some("abc".to_owned()),
        last_modified: Some("2015-10-21T07:28:00Z".parse().unwrap()),
        ..ObjectEntry::default()
    }
}

async fn run_mock_upload(mock: &MockS3, plan: &SyncPlan, root: &Path) -> (RunSummary, SyncPlan) {
    fs::write(root.join("doc.txt"), "abc").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SyncStore::open(dir.path()).unwrap());
    store.insert(plan).unwrap();
    let summary = run_plan(&store, mock.ops(), TransferOptions::default(), &plan.id).await;
    (summary, store.get(&plan.id).unwrap())
}

/// The contract of the conditional write: `If-None-Match: *` for an object the analysis
/// did not see, `If-Match: "<etag>"` for the object a bidirectional upload replaces, a
/// refusal (412) fails the row, and a server that does not know conditional writes (501)
/// gets the same upload without the header.
#[tokio::test]
async fn uploads_carry_conditional_write_headers_and_fall_back_without_them() {
    let root = tempfile::tempdir().unwrap();

    // A new object: create only.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => MockResponse::error(404, "NoSuchKey", "none"),
        "PUT" => MockResponse::empty(200).with_header("etag", "\"def\""),
        _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
    });
    let (summary, _) =
        run_mock_upload(&mock, &mock_plan(root.path(), "upload", None), root.path()).await;
    assert_eq!(summary.errors, 0);
    let put = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(put.header("if-none-match"), Some("*"));
    assert_eq!(put.header("if-match"), None);

    // A replaced object: only while it still has the ETag that was checked.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => head_response("abc", 3),
        "PUT" => MockResponse::empty(200).with_header("etag", "\"def\""),
        _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
    });
    let plan = mock_plan(root.path(), "bidirectional", Some(planned_object()));
    let (summary, stored) = run_mock_upload(&mock, &plan, root.path()).await;
    assert_eq!((summary.errors, stored.rows[0].done), (0, true));
    let put = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(put.header("if-match"), Some("\"abc\""));
    assert_eq!(put.header("if-none-match"), None);

    // A one-way upload over an object the analysis saw replaces it by design.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "PUT" => MockResponse::empty(200).with_header("etag", "\"def\""),
        _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
    });
    let plan = mock_plan(root.path(), "upload", Some(planned_object()));
    let (summary, _) = run_mock_upload(&mock, &plan, root.path()).await;
    assert_eq!(summary.errors, 0);
    let put = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(
        (put.header("if-match"), put.header("if-none-match")),
        (None, None)
    );

    // The server refuses: somebody wrote the object in the meantime.
    let heads = Arc::new(AtomicUsize::new(0));
    let counter = heads.clone();
    let mock = MockS3::start(move |request| match request.method.as_str() {
        "HEAD" if counter.fetch_add(1, Ordering::SeqCst) == 0 => {
            MockResponse::error(404, "NoSuchKey", "none")
        }
        "HEAD" => head_response("zzz", 99),
        "PUT" => MockResponse::error(
            412,
            "PreconditionFailed",
            "At least one of the pre-conditions you specified did not hold",
        ),
        _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
    });
    let (summary, stored) =
        run_mock_upload(&mock, &mock_plan(root.path(), "upload", None), root.path()).await;
    assert_eq!(summary.errors, 1);
    assert!(!stored.rows[0].done);
    assert!(
        stored.rows[0]
            .error
            .as_deref()
            .unwrap()
            .contains("changed since the analysis"),
        "{:?}",
        stored.rows[0]
    );

    // A server without conditional writes answers 501: the same upload goes ahead without
    // the header (the check before it stands in).
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => MockResponse::error(404, "NoSuchKey", "none"),
        "PUT" if request.header("if-none-match").is_some() => MockResponse::error(
            501,
            "NotImplemented",
            "A header you provided implies functionality that is not implemented",
        ),
        "PUT" => MockResponse::empty(200).with_header("etag", "\"def\""),
        _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
    });
    let (summary, stored) =
        run_mock_upload(&mock, &mock_plan(root.path(), "upload", None), root.path()).await;
    assert_eq!((summary.errors, stored.rows[0].done), (0, true));
    let puts: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0].header("if-none-match"), Some("*"));
    assert_eq!(puts[1].header("if-none-match"), None);
}

// ---------------------------------------------------------------------------------------
// 4. Failed rows leave nothing behind
// ---------------------------------------------------------------------------------------

fn entry(key: &str, size: u64) -> ObjectEntry {
    ObjectEntry {
        key: key.to_owned(),
        size,
        ..ObjectEntry::default()
    }
}

fn row_of(
    rel: &str,
    action: SyncAction,
    target: Option<Side>,
    local: Option<FileMeta>,
    remote: Option<ObjectEntry>,
    done: bool,
) -> SyncRow {
    SyncRow {
        rel_path: rel.to_owned(),
        action,
        target,
        local_meta: local,
        s3_meta: remote,
        error: None,
        done,
    }
}

fn make_plan(root: &Path, direction: &str, delete: bool, rows: Vec<SyncRow>) -> SyncPlan {
    serde_json::from_value(serde_json::json!({
        "id": sync::new_plan_id(),
        "account_id": "acct",
        "local_root": root,
        "s3_prefix": "s3://bucket/sync/",
        "direction": direction,
        "delete": delete,
        "created_at": "2026-10-29T08:15:00Z",
        "rows": rows,
    }))
    .unwrap()
}

async fn run_plan(
    store: &Arc<SyncStore>,
    s3: S3,
    options: TransferOptions,
    id: &str,
) -> RunSummary {
    let executor = SyncExecutor::new(store.clone());
    let engine = TransferEngine::new(s3, options).unwrap();
    let run_options = ExecuteOptions {
        concurrency: 2,
        limiter: BandwidthLimiter::unlimited(),
    };
    executor
        .start(id, engine, run_options, Arc::new(NoSyncEvents))
        .unwrap()
        .run()
        .await
        .unwrap()
}

/// An S3 endpoint whose GET answers announce `total` bytes, send `sent` and drop the
/// connection (a network failure in the middle of a download).
fn truncating_endpoint(total: usize, sent: usize) -> S3 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut seen = Vec::new();
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    seen.extend_from_slice(&buf[..n]);
                    if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {total}\r\netag: \"abc\"\r\nlast-modified: {HTTP_DATE}\r\ncontent-type: application/octet-stream\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&vec![b'x'; sent]);
                let _ = stream.flush();
            });
        }
    });
    let account = Account {
        id: "trunc".to_owned(),
        label: "Truncating".to_owned(),
        access_key_id: "AKIDTRUNC".to_owned(),
        secret_ref: "trunc/secret".to_owned(),
        region: "us-east-1".to_owned(),
        endpoint: Some(format!("http://{addr}")),
        path_style: true,
        ..Account::default()
    };
    S3::new(
        &account,
        &Credentials::new("AKIDTRUNC", "secret", None),
        &ClientOptions {
            max_attempts: 1,
            ..ClientOptions::default()
        },
    )
    .unwrap()
}

/// docs/sync.md: "A cancelled or failed download never leaves a partial file." A connection
/// that dies in the middle of the body used to leave `<file>.bbpart` with the data received.
#[tokio::test]
async fn a_download_cut_off_in_the_middle_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local");
    fs::create_dir_all(&root).unwrap();
    let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
    let plan = make_plan(
        &root,
        "download",
        false,
        vec![row_of(
            "big.bin",
            SyncAction::Copy,
            Some(Side::Local),
            None,
            Some(entry("sync/big.bin", 200_000)),
            false,
        )],
    );
    store.insert(&plan).unwrap();
    let summary = run_plan(
        &store,
        truncating_endpoint(200_000, 70_000),
        TransferOptions::default(),
        &plan.id,
    )
    .await;
    assert_eq!(summary.errors, 1);
    assert!(!root.join("big.bin").exists());
    assert!(leftovers(&root).is_empty(), "{:?}", leftovers(&root));
    let stored = store.get(&plan.id).unwrap();
    assert!(!stored.rows[0].done && stored.rows[0].error.is_some());
}

/// A failed multipart upload used to stay open on the server (the engine keeps it for a
/// retry that the sync never makes): every attempt is aborted now.
#[tokio::test]
async fn a_failed_multipart_upload_is_aborted() {
    let mock = MockS3::start(|request| {
        let query = |name: &str| request.query_param(name);
        match request.method.as_str() {
            "HEAD" => MockResponse::error(404, "NoSuchKey", "none"),
            "POST" if query("uploads").is_some() => MockResponse::xml(format!(
                "<InitiateMultipartUploadResult {NS}><Bucket>bucket</Bucket><Key>sync/big.bin</Key><UploadId>upload-1</UploadId></InitiateMultipartUploadResult>"
            )),
            "PUT" if query("partNumber").is_some() => {
                MockResponse::error(500, "InternalError", "boom")
            }
            "DELETE" if query("uploadId").is_some() => MockResponse::empty(204),
            _ => MockResponse::error(404, "NoSuchKey", "unexpected"),
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("big.bin"), vec![b'z'; 4096]).unwrap();
    let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
    let plan = make_plan(
        &root,
        "upload",
        false,
        vec![row_of(
            "big.bin",
            SyncAction::Copy,
            Some(Side::Remote),
            Some(FileMeta {
                size: 4096,
                modified: None,
            }),
            None,
            false,
        )],
    );
    store.insert(&plan).unwrap();
    // Multipart from the first byte on (the smallest legal threshold).
    let options = TransferOptions {
        multipart_threshold: 1,
        ..TransferOptions::default()
    };
    for attempt in 1..=2 {
        let summary = run_plan(&store, mock.ops(), options.clone(), &plan.id).await;
        assert_eq!(summary.errors, 1, "attempt {attempt}");
    }
    let requests = mock.requests();
    let creates = requests
        .iter()
        .filter(|r| r.method == "POST" && r.query_param("uploads").is_some())
        .count();
    let aborts = requests
        .iter()
        .filter(|r| r.method == "DELETE" && r.query_param("uploadId").is_some())
        .count();
    assert_eq!((creates, aborts), (2, 2));
}

/// Cancelling a running download leaves neither a partial or temporary file nor a failure.
#[tokio::test]
async fn a_cancelled_download_leaves_nothing_behind() {
    let fx = Fx::new().await;
    fx.s3
        .put(
            &fx.bucket,
            &format!("{PREFIX}slow.bin"),
            "S".repeat(2 * 1024 * 1024),
        )
        .await;
    let plan = fx.plan(SyncDirection::Download, false, None).await;
    let options = ExecuteOptions {
        concurrency: 1,
        limiter: BandwidthLimiter::new(NonZeroU64::new(256 * 1024)),
    };
    let run = fx
        .executor
        .start(
            &plan.id,
            fx.engine(TransferOptions::default()),
            options,
            Arc::new(NoSyncEvents),
        )
        .unwrap();
    let handle = tokio::spawn(run.run());
    wait_for_partial(&fx.root).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(fx.executor.cancel(&plan.id).unwrap());
    let summary = handle.await.unwrap().unwrap();
    assert!(summary.cancelled);
    assert_eq!(
        fs::read_dir(&fx.root).unwrap().count(),
        0,
        "the folder is empty"
    );
    let stored = fx.stored(&plan.id);
    assert!(!stored.rows[0].done && stored.rows[0].error.is_none());
}

// ---------------------------------------------------------------------------------------
// 5. Links named like partial files
// ---------------------------------------------------------------------------------------

/// A link to a folder named `*.bbpart` was skipped instead of shadowing the remote keys
/// below it, so an upload with `delete` removed them.
#[cfg(unix)]
#[tokio::test]
async fn a_link_to_a_folder_named_like_a_part_file_shadows_the_remote_keys_below_it() {
    use std::os::unix::fs::symlink;

    let fx = Fx::new().await;
    let outside = fx.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("y.txt"), "real content behind the link").unwrap();
    symlink(&outside, fx.root.join("plain")).unwrap();
    symlink(&outside, fx.root.join("x.bbpart")).unwrap();
    fx.put("plain/y.txt", "backup").await;
    fx.put("x.bbpart/y.txt", "backup").await;
    let plan = fx.plan(SyncDirection::Upload, true, None).await;
    for rel in ["plain/y.txt", "x.bbpart/y.txt"] {
        assert_eq!(row(&plan, rel).action, SyncAction::Failed, "{rel}");
    }
    fx.execute(&plan.id).await;
    assert!(fx.remote("plain/y.txt").await.is_some());
    assert!(fx.remote("x.bbpart/y.txt").await.is_some());
}

// ---------------------------------------------------------------------------------------
// 7. The emptied-folder pass
// ---------------------------------------------------------------------------------------

fn silent_mock() -> MockS3 {
    MockS3::start(|_| MockResponse::error(404, "NoSuchKey", "no requests expected"))
}

fn local_delete(rel: &str, done: bool) -> SyncRow {
    row_of(
        rel,
        SyncAction::Delete,
        Some(Side::Local),
        Some(FileMeta {
            size: 1,
            modified: None,
        }),
        None,
        done,
    )
}

/// The pass removes only folders that this run's deletions emptied, only inside the root and
/// never behind a link: a damaged row, rows done by an earlier run and a folder the user
/// re-created are all left alone, while a real deletion still removes its folders.
#[tokio::test]
async fn the_emptied_folder_pass_stays_inside_the_root_and_inside_this_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local");
    let outside = dir.path().join("outside-empty");
    fs::create_dir_all(root.join("recreated/empty")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(root.join("gone/deep")).unwrap();
    let doomed = root.join("gone/deep/x.txt");
    fs::write(&doomed, "x").unwrap();
    let planned = {
        let meta = fs::metadata(&doomed).unwrap();
        FileMeta {
            size: meta.len(),
            modified: Some(meta.modified().unwrap().into()),
        }
    };
    let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
    let mut real = local_delete("gone/deep/x.txt", false);
    real.local_meta = Some(planned);
    let plan = make_plan(
        &root,
        "download",
        true,
        vec![
            // A damaged plan: done, and pending.
            local_delete("../outside-empty/f.txt", true),
            local_delete("../outside-empty/g.txt", false),
            // Done by an earlier run; the user re-created the folder since.
            local_delete("recreated/empty/old.txt", true),
            real,
        ],
    );
    store.insert(&plan).unwrap();
    let mock = silent_mock();
    let summary = run_plan(&store, mock.ops(), TransferOptions::default(), &plan.id).await;
    assert_eq!(
        summary.attempted, 2,
        "the damaged pending row and the real one"
    );
    assert!(
        outside.is_dir(),
        "an empty folder outside the root was removed"
    );
    assert!(
        root.join("recreated/empty").is_dir(),
        "a re-created folder was removed"
    );
    assert!(
        !root.join("gone").exists(),
        "the folder this run emptied stays"
    );
    assert!(root.is_dir());
}

/// An ancestor that became a link since the analysis: the empty folder behind it is not
/// removed, whether the row is done (earlier run) or pending.
#[cfg(unix)]
#[tokio::test]
async fn the_emptied_folder_pass_does_not_follow_an_ancestor_link() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("local");
    let external = dir.path().join("external");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(external.join("b")).unwrap();
    symlink(&external, root.join("a")).unwrap();
    let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
    let plan = make_plan(
        &root,
        "download",
        true,
        vec![
            local_delete("a/b/f.txt", true),
            local_delete("a/b/g.txt", false),
        ],
    );
    store.insert(&plan).unwrap();
    let mock = silent_mock();
    run_plan(&store, mock.ops(), TransferOptions::default(), &plan.id).await;
    assert!(
        external.join("b").is_dir(),
        "the folder behind the link was removed"
    );
}

// ---------------------------------------------------------------------------------------
// 6 and 8. Times given to local files
// ---------------------------------------------------------------------------------------

/// Bidirectional upload of a symlinked file: the adopted time must not be written to the
/// link's target, which lives outside the sync folder.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_time_is_never_written_through_a_file_link() {
    use std::os::unix::fs::symlink;

    let fx = Fx::new().await;
    let outside = fx.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let target = outside.join("target.txt");
    fs::write(&target, "newer local content").unwrap();
    set_mtime(&target, SystemTime::now() + hour());
    symlink(&target, fx.root.join("link.txt")).unwrap();
    fx.put("link.txt", "old remote").await;
    let plan = fx.plan(SyncDirection::Bidirectional, false, None).await;
    assert_eq!(row(&plan, "link.txt").target, Some(Side::Remote));
    let before = fs::metadata(&target).unwrap().modified().unwrap();
    let summary = fx.execute(&plan.id).await;
    assert_eq!(summary.errors, 0);
    assert_eq!(fx.remote("link.txt").await.unwrap(), b"newer local content");
    let after = fs::metadata(&target).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "the time of a file outside the folder changed"
    );
}

/// Number of copy rows each of four analyses plans, running every plan, for a file dated
/// `skew` after the server's clock.
async fn copies_planned_per_run(direction: SyncDirection, skew: Duration) -> Vec<usize> {
    let fx = Fx::new().await;
    fx.write("photo.jpg", "unchanged content");
    fx.set_age("photo.jpg", SystemTime::now() + skew);
    let mut planned = Vec::new();
    for _ in 0..4 {
        let plan = fx.analyze(direction, false, None).await;
        let copies = plan
            .rows
            .iter()
            .filter(|row| row.action == SyncAction::Copy)
            .count();
        planned.push(copies);
        if copies > 0 {
            fx.executor.store().insert(&plan).unwrap();
            fx.execute(&plan.id).await;
        }
    }
    planned
}

/// A file dated after the server's clock was uploaded again on every upload-only run. The
/// run now gives such a file the object's time (and only such a file).
#[tokio::test]
async fn a_file_dated_after_the_servers_clock_is_uploaded_once() {
    assert_eq!(
        copies_planned_per_run(SyncDirection::Upload, hour()).await,
        vec![1, 0, 0, 0]
    );
    assert_eq!(
        copies_planned_per_run(SyncDirection::Bidirectional, hour()).await,
        vec![1, 0, 0, 0]
    );

    // An ordinary old file keeps its own time: upload-only runs do not touch local files.
    let fx = Fx::new().await;
    fx.write("old.txt", "old content");
    let old = SystemTime::now() - 24 * hour();
    fx.set_age("old.txt", old);
    let before = fs::metadata(fx.root.join("old.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let plan = fx.plan(SyncDirection::Upload, false, None).await;
    assert_eq!(fx.execute(&plan.id).await.errors, 0);
    let after = fs::metadata(fx.root.join("old.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);
}

// ---------------------------------------------------------------------------------------
// 9. Rows whose result was lost
// ---------------------------------------------------------------------------------------

/// A crash inside the batch of results, or a cancelled request that was stored anyway, leaves
/// a row pending whose work is already done. Running it again finds the target equal to the
/// source and marks the row done, in every direction.
#[tokio::test]
async fn rows_whose_result_was_lost_are_recognised_on_the_next_run() {
    for direction in [
        SyncDirection::Bidirectional,
        SyncDirection::Upload,
        SyncDirection::Download,
    ] {
        let fx = Fx::new().await;
        fx.write("up.txt", "local only");
        fx.set_age("up.txt", SystemTime::now() - hour());
        fx.put("down.txt", "remote only").await;
        let plan = fx.plan(direction, false, None).await;
        // The same rows with no result recorded: what the store holds after a lost tail.
        let mut replay = plan.clone();
        replay.id = sync::new_plan_id();
        fx.executor.store().insert(&replay).unwrap();

        let first = fx.execute(&plan.id).await;
        assert_eq!(first.errors, 0, "{direction:?}");
        let again = fx.execute(&replay.id).await;
        assert_eq!(
            again.errors,
            0,
            "{direction:?}: {:?}",
            fx.stored(&replay.id).rows
        );
        assert!(
            fx.stored(&replay.id)
                .rows
                .iter()
                .all(|row| row.done || row.action == SyncAction::Equal),
            "{direction:?}"
        );
        assert_eq!(
            fx.local("down.txt").is_some(),
            direction != SyncDirection::Upload
        );
    }
}

// ---------------------------------------------------------------------------------------
// 10. Globs
// ---------------------------------------------------------------------------------------

#[test]
fn a_negated_glob_class_does_not_match_the_separator() {
    let compile = |glob: &str| {
        Filter::compile(&FilterSpec {
            glob: Some(glob.to_owned()),
            ..FilterSpec::default()
        })
        .unwrap()
    };
    let negated = compile("a[!x]b");
    assert!(negated.matches_path("acb"));
    assert!(!negated.matches_path("a/b"));
    assert!(!compile("a[^x]b").matches_path("a/b"));
}

// ---------------------------------------------------------------------------------------
// 12. Execution paths that had no test
// ---------------------------------------------------------------------------------------

/// A file becomes a folder of the same name and the other way round, with `delete`: the
/// local deletions run first and the folders they empty are removed, so both copies work.
#[tokio::test]
async fn files_and_folders_swap_places_with_delete() {
    let fx = Fx::new().await;
    fx.put("x/y.txt", "remote y").await;
    fx.put("d", "remote d as a file").await;
    fx.write("x", "local x as a file");
    fx.write("d/keep.txt", "local keep");
    let plan = fx.plan(SyncDirection::Download, true, None).await;
    let summary = fx.execute(&plan.id).await;
    assert_eq!(summary.errors, 0, "{:?}", fx.stored(&plan.id).rows);
    assert_eq!(fx.local("x/y.txt").unwrap(), "remote y");
    assert_eq!(fx.local("d").unwrap(), "remote d as a file");
    assert!(leftovers(&fx.root).is_empty());
}

/// The bidirectional upload guard: the object changed between the analysis and the run.
#[tokio::test]
async fn a_bidirectional_upload_refuses_a_target_changed_before_the_run() {
    let fx = Fx::new().await;
    fx.put("doc.txt", "old remote").await;
    fx.write("doc.txt", "newer local content");
    fx.set_age("doc.txt", SystemTime::now() + hour());
    let plan = fx.plan(SyncDirection::Bidirectional, false, None).await;
    assert_eq!(row(&plan, "doc.txt").target, Some(Side::Remote));
    fx.put("doc.txt", "someone else edited this remotely").await;
    let summary = fx.execute(&plan.id).await;
    assert_eq!(summary.errors, 1);
    assert_eq!(
        fx.remote("doc.txt").await.unwrap(),
        b"someone else edited this remotely"
    );
}
