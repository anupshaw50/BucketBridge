//! End-to-end tests of sync plan execution (`bucketbridge_core::sync::execute`) against
//! a real S3 server (s3s-fs by default, MinIO with `--features minio` and
//! `BB_TEST_BACKEND=minio`): plans are analysed from real listings, stored, executed with
//! the transfer engines, and checked on both sides.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bucketbridge_core::engine::{TransferEngine, TransferOptions};
use bucketbridge_core::model::{Side, SyncAction, SyncDirection, SyncPlan, SyncRow};
use bucketbridge_core::sync::{
    self, AnalyzeRequest, ExecuteOptions, RunSummary, SyncEvents, SyncExecutor, SyncProgress,
};
use bucketbridge_core::transfer::BandwidthLimiter;
use bucketbridge_core::{ErrorKind, S3Uri, SyncStore};
use bucketbridge_tests::TestS3;

const PREFIX: &str = "sync/";

/// Records every event of a run.
#[derive(Default)]
struct Recorder {
    progress: Mutex<Vec<SyncProgress>>,
    rows: Mutex<Vec<(usize, SyncRow)>>,
}

impl SyncEvents for Recorder {
    fn progress(&self, progress: &SyncProgress) {
        self.progress.lock().unwrap().push(progress.clone());
    }

    fn row(&self, _plan_id: &str, index: usize, row: &SyncRow) {
        self.rows.lock().unwrap().push((index, row.clone()));
    }
}

impl Recorder {
    fn finished_reports(&self) -> Vec<SyncProgress> {
        self.progress
            .lock()
            .unwrap()
            .iter()
            .filter(|progress| progress.finished)
            .cloned()
            .collect()
    }
}

/// Cancels the run once the first row result was reported.
struct CancelAfterFirstRow {
    executor: SyncExecutor,
    fired: AtomicBool,
    recorder: Recorder,
}

impl SyncEvents for CancelAfterFirstRow {
    fn progress(&self, progress: &SyncProgress) {
        self.recorder.progress(progress);
    }

    fn row(&self, plan_id: &str, index: usize, row: &SyncRow) {
        self.recorder.row(plan_id, index, row);
        if !self.fired.swap(true, Ordering::SeqCst) {
            assert!(self.executor.cancel(plan_id).unwrap());
        }
    }
}

/// One S3 server, one bucket, a local folder and a plan store.
struct Fixture {
    s3: TestS3,
    bucket: String,
    _dir: tempfile::TempDir,
    root: PathBuf,
    executor: SyncExecutor,
}

impl Fixture {
    async fn new() -> Fixture {
        let s3 = TestS3::start().await;
        let bucket = s3.new_bucket().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("local");
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
        Fixture {
            s3,
            bucket,
            _dir: dir,
            root,
            executor: SyncExecutor::new(store),
        }
    }

    fn prefix(&self) -> S3Uri {
        S3Uri::new(&self.bucket, PREFIX).unwrap()
    }

    fn engine(&self) -> TransferEngine {
        TransferEngine::new(self.s3.ops().clone(), TransferOptions::default()).unwrap()
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

    async fn remote_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .s3
            .ops()
            .list_all(&self.bucket, PREFIX)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.key)
            .collect();
        keys.sort();
        keys
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn set_age(&self, rel: &str, modified: SystemTime) {
        fs::File::options()
            .write(true)
            .open(self.root.join(rel))
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    fn local(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.root.join(rel)).ok()
    }

    /// Analyses, stores and returns the plan.
    async fn plan(&self, direction: SyncDirection, delete: bool) -> SyncPlan {
        let listing = self.s3.ops().list_all(&self.bucket, PREFIX).await.unwrap();
        let prefix = self.prefix();
        let mut request = AnalyzeRequest::new("test-account", &self.root, &prefix, direction);
        request.delete = delete;
        let plan = sync::analyze(&request, listing).unwrap();
        self.executor.store().insert(&plan).unwrap();
        plan
    }

    async fn execute_with(
        &self,
        plan_id: &str,
        options: ExecuteOptions,
        events: Arc<dyn SyncEvents>,
    ) -> RunSummary {
        self.executor
            .start(plan_id, self.engine(), options, events)
            .unwrap()
            .run()
            .await
            .unwrap()
    }

    async fn execute(&self, plan_id: &str) -> (RunSummary, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let summary = self
            .execute_with(plan_id, unlimited(4), recorder.clone())
            .await;
        (summary, recorder)
    }

    fn stored(&self, plan_id: &str) -> SyncPlan {
        self.executor.store().get(plan_id).unwrap()
    }

    /// A fresh analysis plans nothing but equal rows.
    async fn assert_in_sync(&self, direction: SyncDirection) {
        let listing = self.s3.ops().list_all(&self.bucket, PREFIX).await.unwrap();
        let prefix = self.prefix();
        let request = AnalyzeRequest::new("test-account", &self.root, &prefix, direction);
        let plan = sync::analyze(&request, listing).unwrap();
        let unequal: Vec<_> = plan
            .rows
            .iter()
            .filter(|row| row.action != SyncAction::Equal)
            .map(|row| (row.rel_path.as_str(), row.action, row.error.as_deref()))
            .collect();
        assert!(unequal.is_empty(), "{direction:?}: {unequal:?}");
    }
}

fn unlimited(concurrency: usize) -> ExecuteOptions {
    ExecuteOptions {
        concurrency,
        limiter: BandwidthLimiter::unlimited(),
    }
}

fn row<'p>(plan: &'p SyncPlan, rel: &str) -> &'p SyncRow {
    plan.rows
        .iter()
        .find(|row| row.rel_path == rel)
        .unwrap_or_else(|| panic!("no row {rel}"))
}

fn index(plan: &SyncPlan, rel: &str) -> usize {
    plan.rows
        .iter()
        .position(|row| row.rel_path == rel)
        .unwrap()
}

fn summary(plan: &SyncPlan) -> Vec<(&str, SyncAction, Option<Side>)> {
    plan.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.action, row.target))
        .collect()
}

fn hour() -> Duration {
    Duration::from_secs(3600)
}

#[tokio::test]
async fn upload_mirrors_the_folder_and_deletes_extra_objects() {
    let fx = Fixture::new().await;
    fx.write("a.txt", "local a");
    fx.write("nested/b.txt", "local b");
    fx.write("nested/deeper/c.txt", "local c");
    fx.put("a.txt", "old remote a").await;
    fx.put("stale.txt", "only in S3").await;
    fx.put("gone/deep/x.txt", "only in S3 too").await;

    let plan = fx.plan(SyncDirection::Upload, true).await;
    assert_eq!(
        summary(&plan),
        vec![
            ("a.txt", SyncAction::Copy, Some(Side::Remote)),
            ("gone/deep/x.txt", SyncAction::Delete, Some(Side::Remote)),
            ("nested/b.txt", SyncAction::Copy, Some(Side::Remote)),
            ("nested/deeper/c.txt", SyncAction::Copy, Some(Side::Remote)),
            ("stale.txt", SyncAction::Delete, Some(Side::Remote)),
        ]
    );
    let (result, events) = fx.execute(&plan.id).await;
    assert_eq!((result.attempted, result.errors), (5, 0));
    assert!(!result.cancelled);
    assert_eq!(
        (
            result.progress.total,
            result.progress.done,
            result.progress.failed
        ),
        (5, 5, 0)
    );

    assert_eq!(fx.remote("a.txt").await.unwrap(), b"local a");
    assert_eq!(fx.remote("nested/deeper/c.txt").await.unwrap(), b"local c");
    assert_eq!(
        fx.remote_keys().await,
        vec![
            "sync/a.txt",
            "sync/nested/b.txt",
            "sync/nested/deeper/c.txt"
        ]
    );
    let stored = fx.stored(&plan.id);
    assert!(
        stored
            .rows
            .iter()
            .all(|row| row.done && row.error.is_none())
    );

    // Events: one per row, then exactly one finished report matching the store.
    assert_eq!(events.rows.lock().unwrap().len(), 5);
    let finished = events.finished_reports();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0], result.progress);
    assert_eq!(finished[0].plan_id, plan.id);
    assert_eq!(fx.executor.store().counts(&plan.id).unwrap().done, 5);

    // Nothing left to do: a fresh analysis is all equal, and running the plan again
    // does nothing but report.
    fx.assert_in_sync(SyncDirection::Upload).await;
    let (again, events) = fx.execute(&plan.id).await;
    assert_eq!((again.attempted, again.errors), (0, 0));
    assert_eq!(events.finished_reports().len(), 1);
}

#[tokio::test]
async fn download_fills_the_folder_and_removes_emptied_folders() {
    let fx = Fixture::new().await;
    fx.put("a.txt", "remote a").await;
    fx.put("nested/b.txt", "remote b").await;
    fx.put("keep/kept.txt", "remote kept").await;
    fx.write("a.txt", "an older local a");
    fx.set_age("a.txt", SystemTime::now() - hour());
    fx.write("old/deep/x.txt", "only local");
    fx.write("old/deep/y.txt", "only local too");
    fx.write("keep/extra.txt", "only local, beside a synced file");

    let plan = fx.plan(SyncDirection::Download, true).await;
    assert_eq!(
        summary(&plan),
        vec![
            ("a.txt", SyncAction::Copy, Some(Side::Local)),
            ("keep/extra.txt", SyncAction::Delete, Some(Side::Local)),
            ("keep/kept.txt", SyncAction::Copy, Some(Side::Local)),
            ("nested/b.txt", SyncAction::Copy, Some(Side::Local)),
            ("old/deep/x.txt", SyncAction::Delete, Some(Side::Local)),
            ("old/deep/y.txt", SyncAction::Delete, Some(Side::Local)),
        ]
    );
    let (result, _) = fx.execute(&plan.id).await;
    assert_eq!((result.attempted, result.errors), (6, 0));

    assert_eq!(fx.local("a.txt").unwrap(), "remote a");
    assert_eq!(fx.local("nested/b.txt").unwrap(), "remote b");
    assert_eq!(fx.local("keep/kept.txt").unwrap(), "remote kept");
    assert!(fx.local("keep/extra.txt").is_none());
    // The folders the deletions emptied are gone; the root and used folders stay.
    assert!(!fx.root.join("old").exists());
    assert!(fx.root.join("keep").is_dir());
    assert!(fx.root.is_dir());
    // No temporary download files are left behind.
    let leftovers: Vec<PathBuf> = walk(&fx.root)
        .into_iter()
        .filter(|path| path.to_string_lossy().ends_with(".bbpart"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Downloaded files carry the object's time, so every direction agrees.
    for direction in [
        SyncDirection::Download,
        SyncDirection::Upload,
        SyncDirection::Bidirectional,
    ] {
        fx.assert_in_sync(direction).await;
    }
}

#[tokio::test]
async fn bidirectional_copies_both_ways_and_converges() {
    let fx = Fixture::new().await;
    fx.put("remote-only.txt", "from S3").await;
    fx.put("remote-newer.txt", "newer in S3").await;
    fx.put("local-newer.txt", "older in S3").await;
    fx.write("local-only.txt", "from the folder");
    fx.set_age("local-only.txt", SystemTime::now() - hour());
    fx.write("remote-newer.txt", "older locally");
    fx.set_age("remote-newer.txt", SystemTime::now() - hour());
    fx.write("local-newer.txt", "newer locally");
    fx.set_age("local-newer.txt", SystemTime::now() + hour());

    let plan = fx.plan(SyncDirection::Bidirectional, false).await;
    assert_eq!(
        summary(&plan),
        vec![
            ("local-newer.txt", SyncAction::Copy, Some(Side::Remote)),
            ("local-only.txt", SyncAction::Copy, Some(Side::Remote)),
            ("remote-newer.txt", SyncAction::Copy, Some(Side::Local)),
            ("remote-only.txt", SyncAction::Copy, Some(Side::Local)),
        ]
    );
    let (result, _) = fx.execute(&plan.id).await;
    assert_eq!((result.attempted, result.errors), (4, 0));
    assert_eq!(
        fx.remote("local-newer.txt").await.unwrap(),
        b"newer locally"
    );
    assert_eq!(
        fx.remote("local-only.txt").await.unwrap(),
        b"from the folder"
    );
    assert_eq!(fx.local("remote-newer.txt").unwrap(), "newer in S3");
    assert_eq!(fx.local("remote-only.txt").unwrap(), "from S3");

    // Uploaded files took the new object's time (otherwise S3 would look an hour
    // newer), so every direction agrees.
    for direction in [
        SyncDirection::Bidirectional,
        SyncDirection::Upload,
        SyncDirection::Download,
    ] {
        fx.assert_in_sync(direction).await;
    }
}

#[tokio::test]
async fn failing_rows_do_not_stop_the_others_and_a_new_run_retries_them() {
    let fx = Fixture::new().await;
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        fx.put(name, &format!("remote {name}")).await;
    }
    let plan = fx.plan(SyncDirection::Download, false).await;
    // The object disappears between the analysis and the execution.
    fx.s3
        .ops()
        .delete_object(&fx.bucket, "sync/b.txt", None)
        .await
        .unwrap();

    let (result, events) = fx.execute(&plan.id).await;
    assert_eq!((result.attempted, result.errors), (4, 1));
    assert_eq!(
        (
            result.progress.total,
            result.progress.done,
            result.progress.failed
        ),
        (4, 3, 1)
    );
    for name in ["a.txt", "c.txt", "d.txt"] {
        assert_eq!(fx.local(name).unwrap(), format!("remote {name}"));
    }
    let stored = fx.stored(&plan.id);
    let failed = row(&stored, "b.txt");
    assert_eq!((failed.action, failed.done), (SyncAction::Copy, false));
    assert!(failed.error.is_some());
    assert!(
        events
            .rows
            .lock()
            .unwrap()
            .iter()
            .any(|(at, row)| *at == index(&plan, "b.txt") && row.error.is_some())
    );

    // Once the object is back, running the plan again retries only that row.
    fx.put("b.txt", "remote b.txt").await;
    let (retry, _) = fx.execute(&plan.id).await;
    assert_eq!((retry.attempted, retry.errors), (1, 0));
    assert_eq!((retry.progress.done, retry.progress.failed), (4, 0));
    let stored = fx.stored(&plan.id);
    let fixed = row(&stored, "b.txt");
    assert!(fixed.done && fixed.error.is_none());
    assert_eq!(fx.local("b.txt").unwrap(), "remote b.txt");
}

#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_file_fails_only_its_row() {
    use std::os::unix::fs::PermissionsExt;

    let fx = Fixture::new().await;
    fx.write("open.txt", "readable");
    fx.write("locked.txt", "unreadable after the analysis");
    let plan = fx.plan(SyncDirection::Upload, false).await;
    let locked = fx.root.join("locked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&locked).is_ok() {
        // Running as root: permissions are not enforced.
        return;
    }
    let (result, _) = fx.execute(&plan.id).await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!((result.attempted, result.errors), (2, 1));
    assert_eq!(fx.remote("open.txt").await.unwrap(), b"readable");
    assert!(fx.remote("locked.txt").await.is_none());

    let (retry, _) = fx.execute(&plan.id).await;
    assert_eq!((retry.attempted, retry.errors), (1, 0));
    assert_eq!(
        fx.remote("locked.txt").await.unwrap(),
        b"unreadable after the analysis"
    );
    assert!(fx.stored(&plan.id).rows.iter().all(|row| row.done));
}

#[tokio::test]
async fn cancel_stops_promptly_and_the_next_run_finishes_the_plan() {
    let fx = Fixture::new().await;
    let body = "x".repeat(256 * 1024);
    for n in 0..4 {
        fx.write(&format!("file-{n}.bin"), &body);
    }
    let plan = fx.plan(SyncDirection::Upload, false).await;
    let events = Arc::new(CancelAfterFirstRow {
        executor: fx.executor.clone(),
        fired: AtomicBool::new(false),
        recorder: Recorder::default(),
    });
    // 512 KiB/s and one row at a time: each file takes about half a second.
    let slow = ExecuteOptions {
        concurrency: 1,
        limiter: BandwidthLimiter::new(NonZeroU64::new(512 * 1024)),
    };
    let started = Instant::now();
    let result = fx.execute_with(&plan.id, slow, events.clone()).await;
    assert!(result.cancelled);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(result.errors, 0);
    assert!(
        (1..4).contains(&result.progress.done),
        "{:?}",
        result.progress
    );
    assert_eq!(events.recorder.finished_reports().len(), 1);
    assert!(!fx.executor.is_running(&plan.id).unwrap());
    // Whatever was cancelled stays pending and leaves nothing half-written in S3 (an
    // upload whose last byte was already sent may still have been stored, complete).
    let stored = fx.stored(&plan.id);
    for row in &stored.rows {
        assert!(row.error.is_none(), "{row:?}");
        let remote = fx.remote(&row.rel_path).await;
        if row.done {
            assert_eq!(remote.unwrap().len(), body.len());
        } else {
            assert!(
                remote.is_none_or(|data| data.len() == body.len()),
                "{}",
                row.rel_path
            );
        }
    }

    let (rest, _) = fx.execute(&plan.id).await;
    assert!(!rest.cancelled);
    assert_eq!(rest.errors, 0);
    assert_eq!(rest.attempted, 4 - result.progress.done);
    assert_eq!(rest.progress.done, 4);
    fx.assert_in_sync(SyncDirection::Upload).await;
}

#[tokio::test]
async fn one_execution_per_plan_at_a_time() {
    let fx = Fixture::new().await;
    fx.write("a.txt", "a");
    let plan = fx.plan(SyncDirection::Upload, false).await;
    let recorder: Arc<dyn SyncEvents> = Arc::new(Recorder::default());
    let first = fx
        .executor
        .start(&plan.id, fx.engine(), unlimited(1), recorder.clone())
        .unwrap();
    assert!(fx.executor.is_running(&plan.id).unwrap());
    let err = fx
        .executor
        .start(&plan.id, fx.engine(), unlimited(1), recorder.clone())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    // Another spelling of the same id is the same plan.
    let err = fx
        .executor
        .start(
            &plan.id.to_uppercase(),
            fx.engine(),
            unlimited(1),
            recorder.clone(),
        )
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);

    let handle = tokio::spawn(first.run());
    let err = fx
        .executor
        .start(&plan.id, fx.engine(), unlimited(1), recorder.clone())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    handle.await.unwrap().unwrap();
    assert!(!fx.executor.is_running(&plan.id).unwrap());
    // Once the run is over the plan can run again; a claim that is dropped unused is
    // released too.
    let again = fx
        .executor
        .start(&plan.id, fx.engine(), unlimited(1), recorder.clone())
        .unwrap();
    drop(again);
    assert!(!fx.executor.is_running(&plan.id).unwrap());
    assert!(!fx.executor.cancel(&plan.id).unwrap());

    let err = fx
        .executor
        .start("not-a-plan-id", fx.engine(), unlimited(1), recorder.clone())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let missing = "00000000-0000-4000-8000-000000000000";
    let run = fx
        .executor
        .start(missing, fx.engine(), unlimited(1), recorder)
        .unwrap();
    assert_eq!(run.run().await.unwrap_err().kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn changes_made_after_the_analysis_are_never_destroyed() {
    let fx = Fixture::new().await;
    // A local file that a download with delete would remove.
    fx.write("victim.txt", "only local");
    // An object that an upload with delete would remove.
    fx.put("remote-victim.txt", "only in S3").await;
    let download = fx.plan(SyncDirection::Download, true).await;
    let upload = fx.plan(SyncDirection::Upload, true).await;

    // Both victims change after the analysis (and change size).
    fx.write("victim.txt", "edited after the analysis");
    fx.put("remote-victim.txt", "rewritten after the analysis")
        .await;

    let (result, _) = fx.execute(&download.id).await;
    let stored = fx.stored(&download.id);
    let local_victim = row(&stored, "victim.txt");
    assert!(!local_victim.done);
    assert!(
        local_victim.error.as_deref().unwrap().contains("changed"),
        "{local_victim:?}"
    );
    assert_eq!(fx.local("victim.txt").unwrap(), "edited after the analysis");
    assert!(result.errors >= 1);

    let (result, _) = fx.execute(&upload.id).await;
    let stored = fx.stored(&upload.id);
    let remote_victim = row(&stored, "remote-victim.txt");
    assert!(!remote_victim.done, "{remote_victim:?}");
    assert_eq!(
        fx.remote("remote-victim.txt").await.unwrap(),
        b"rewritten after the analysis"
    );
    assert!(result.errors >= 1);

    // Bidirectional: a target changed since the analysis is not overwritten.
    let fx = Fixture::new().await;
    fx.put("shared.txt", "newer in S3").await;
    fx.write("shared.txt", "older locally");
    fx.set_age("shared.txt", SystemTime::now() - hour());
    let plan = fx.plan(SyncDirection::Bidirectional, false).await;
    assert_eq!(row(&plan, "shared.txt").target, Some(Side::Local));
    fx.write("shared.txt", "edited locally after the analysis");
    let (result, _) = fx.execute(&plan.id).await;
    assert_eq!(result.errors, 1);
    assert_eq!(
        fx.local("shared.txt").unwrap(),
        "edited locally after the analysis"
    );
}

#[tokio::test]
async fn deletions_of_things_already_gone_count_as_done() {
    let fx = Fixture::new().await;
    fx.write("local-only.txt", "x");
    fx.put("remote-only.txt", "y").await;
    let download = fx.plan(SyncDirection::Download, true).await;
    let upload = fx.plan(SyncDirection::Upload, true).await;
    fs::remove_file(fx.root.join("local-only.txt")).unwrap();
    fx.s3
        .ops()
        .delete_object(&fx.bucket, "sync/remote-only.txt", None)
        .await
        .unwrap();

    // The deletions succeed; the copies of the vanished sources fail.
    fx.execute(&download.id).await;
    let stored = fx.stored(&download.id);
    let deleted = row(&stored, "local-only.txt");
    assert!(deleted.done && deleted.error.is_none(), "{deleted:?}");
    assert!(row(&stored, "remote-only.txt").error.is_some());
    fx.execute(&upload.id).await;
    let stored = fx.stored(&upload.id);
    let deleted = row(&stored, "remote-only.txt");
    assert!(deleted.done && deleted.error.is_none(), "{deleted:?}");
    assert!(row(&stored, "local-only.txt").error.is_some());
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}
