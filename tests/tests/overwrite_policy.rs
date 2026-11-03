//! The overwrite policy (`IfExists`) in every engine, with folder batches
//! through the queue, and the modification-time metadata, against s3s-fs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bucketbridge_core::engine::{
    Choice, Conflict, ConflictResolver, Hooks, JobOptions, Outcome, S3Provider, S3Runner,
    SkipReason, TransferEngine, TransferOptions, enqueue_copy_with, enqueue_download_with,
    enqueue_move_with, enqueue_upload_with, parse_mtime,
};
use bucketbridge_core::model::{TransferState, TransferTask};
use bucketbridge_core::settings::{DataIntegritySettings, IfExists, TransferSettings};
use bucketbridge_core::transfer::{QueueOptions, TransferQueue};
use bucketbridge_core::{Error, ErrorKind, S3, S3Uri};
use bucketbridge_tests::TestS3;
use futures::future::BoxFuture;

const ACCOUNT: &str = "account-1";

fn uri(bucket: &str, key: &str) -> S3Uri {
    S3Uri::new(bucket, key).unwrap()
}

fn engine(s3: &TestS3, policy: IfExists) -> TransferEngine {
    TransferEngine::new(s3.ops().clone(), TransferOptions::default())
        .unwrap()
        .with_if_exists(policy)
}

fn set_mtime(path: &Path, secs_from_now: i64) {
    let now = SystemTime::now();
    let time = if secs_from_now >= 0 {
        now + Duration::from_secs(secs_from_now.unsigned_abs())
    } else {
        now - Duration::from_secs(secs_from_now.unsigned_abs())
    };
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn write(dir: &Path, name: &str, data: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, data).unwrap();
    path
}

async fn upload(
    engine: &TransferEngine,
    path: &Path,
    dst: &S3Uri,
) -> Outcome<bucketbridge_core::engine::UploadOutcome> {
    engine
        .upload_file(path, dst, &Hooks::new(), None, &|_| {})
        .await
        .unwrap()
}

async fn download(
    engine: &TransferEngine,
    src: &S3Uri,
    dest: &Path,
) -> Outcome<bucketbridge_core::engine::DownloadOutcome> {
    engine
        .download_file(src, dest, &Hooks::new(), None, &|_| {})
        .await
        .unwrap()
}

/// Answers every question with one choice and counts the questions.
struct Fixed {
    choice: Choice,
    asked: AtomicUsize,
    seen: Mutex<Vec<Conflict>>,
}

impl Fixed {
    fn new(choice: Choice) -> Arc<Self> {
        Arc::new(Fixed {
            choice,
            asked: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }
}

impl ConflictResolver for Fixed {
    fn resolve<'a>(&'a self, conflict: &'a Conflict) -> BoxFuture<'a, Result<Choice, Error>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(conflict.clone());
        Box::pin(std::future::ready(Ok(self.choice)))
    }
}

// ---- uploads ----

#[tokio::test]
async fn upload_follows_every_policy() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let dst = uri(&bucket, "docs/a.txt");

    // Nothing there yet: every policy just uploads, without renaming.
    for (n, policy) in [
        IfExists::Ask,
        IfExists::Overwrite,
        IfExists::Skip,
        IfExists::OverwriteIfNewer,
        IfExists::Rename,
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("fresh/{n}.txt");
        let path = write(dir.path(), "fresh.txt", "first");
        let outcome = upload(&engine(&s3, policy), &path, &uri(&bucket, &key)).await;
        let Outcome::Completed(done) = outcome else {
            panic!("{policy:?}: {outcome:?}");
        };
        assert_eq!(done.renamed_to, None);
        assert_eq!(s3.get(&bucket, &key).await, b"first");
    }

    s3.put(&bucket, "docs/a.txt", "old").await;
    let path = write(dir.path(), "a.txt", "new");

    // overwrite
    let outcome = upload(&engine(&s3, IfExists::Overwrite), &path, &dst).await;
    assert!(outcome.is_completed());
    assert_eq!(s3.get(&bucket, "docs/a.txt").await, b"new");

    // skip
    s3.put(&bucket, "docs/a.txt", "old").await;
    let outcome = upload(&engine(&s3, IfExists::Skip), &path, &dst).await;
    assert_eq!(outcome, Outcome::Skipped(SkipReason::Exists));
    assert_eq!(s3.get(&bucket, "docs/a.txt").await, b"old");

    // rename: the original stays, the new file gets a free name
    let outcome = upload(&engine(&s3, IfExists::Rename), &path, &dst).await;
    let Outcome::Completed(done) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(done.renamed_to.as_deref(), Some("docs/a (1).txt"));
    assert_eq!(s3.get(&bucket, "docs/a.txt").await, b"old");
    assert_eq!(s3.get(&bucket, "docs/a (1).txt").await, b"new");
    let outcome = upload(&engine(&s3, IfExists::Rename), &path, &dst).await;
    let Outcome::Completed(done) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(done.renamed_to.as_deref(), Some("docs/a (2).txt"));

    // newer: an older local file is skipped, a newer one replaces
    let old_stamp = write(dir.path(), "older.txt", "older");
    set_mtime(&old_stamp, -100_000);
    let outcome = upload(&engine(&s3, IfExists::OverwriteIfNewer), &old_stamp, &dst).await;
    assert_eq!(outcome, Outcome::Skipped(SkipReason::NotNewer));
    assert_eq!(s3.get(&bucket, "docs/a.txt").await, b"old");
    let new_stamp = write(dir.path(), "newer.txt", "newer");
    set_mtime(&new_stamp, 100_000);
    let outcome = upload(&engine(&s3, IfExists::OverwriteIfNewer), &new_stamp, &dst).await;
    assert!(outcome.is_completed());
    assert_eq!(s3.get(&bucket, "docs/a.txt").await, b"newer");
}

#[tokio::test]
async fn upload_asks_the_resolver_and_refuses_without_one() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.txt", "new");
    let dst = uri(&bucket, "a.txt");
    s3.put(&bucket, "a.txt", "old").await;

    let err = engine(&s3, IfExists::Ask)
        .upload_file(&path, &dst, &Hooks::new(), None, &|_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert_eq!(s3.get(&bucket, "a.txt").await, b"old");

    for (choice, expect_body, expect_skip) in [
        (Choice::Skip, "old", true),
        (Choice::Overwrite, "new", false),
    ] {
        s3.put(&bucket, "a.txt", "old").await;
        let resolver = Fixed::new(choice);
        let engine = engine(&s3, IfExists::Ask).with_conflict_resolver(resolver.clone());
        let outcome = upload(&engine, &path, &dst).await;
        assert_eq!(matches!(outcome, Outcome::Skipped(_)), expect_skip);
        assert_eq!(s3.get(&bucket, "a.txt").await, expect_body.as_bytes());
        assert_eq!(resolver.asked.load(Ordering::SeqCst), 1);
        let seen = resolver.seen.lock().unwrap();
        assert_eq!(seen[0].destination, format!("s3://{bucket}/a.txt"));
        assert_eq!(seen[0].destination_info.size, Some(3));
        assert_eq!(seen[0].source_info.size, Some(3));
    }

    s3.put(&bucket, "a.txt", "old").await;
    let resolver = Fixed::new(Choice::Rename);
    let engine = engine(&s3, IfExists::Ask).with_conflict_resolver(resolver);
    let Outcome::Completed(done) = upload(&engine, &path, &dst).await else {
        panic!("not completed");
    };
    assert_eq!(done.renamed_to.as_deref(), Some("a (1).txt"));
    s3.put(&bucket, "a.txt", "old").await;
    let resolver = Fixed::new(Choice::OverwriteIfNewer);
    set_mtime(&path, -100_000);
    let engine = self::engine(&s3, IfExists::Ask).with_conflict_resolver(resolver);
    assert_eq!(
        upload(&engine, &path, &dst).await,
        Outcome::Skipped(SkipReason::NotNewer)
    );
}

// ---- downloads ----

#[tokio::test]
async fn download_follows_every_policy() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let src = uri(&bucket, "a.txt");
    s3.put(&bucket, "a.txt", "remote").await;

    // No local file yet: every policy just downloads.
    for (n, policy) in [IfExists::Ask, IfExists::Skip, IfExists::Rename]
        .into_iter()
        .enumerate()
    {
        let dest = dir.path().join(format!("fresh{n}.txt"));
        let outcome = download(&engine(&s3, policy), &src, &dest).await;
        let Outcome::Completed(done) = outcome else {
            panic!("{policy:?}: {outcome:?}");
        };
        assert_eq!(done.renamed_to, None);
        assert_eq!(fs::read(&dest).unwrap(), b"remote");
    }

    let dest = write(dir.path(), "a.txt", "local");
    let outcome = download(&engine(&s3, IfExists::Skip), &src, &dest).await;
    assert_eq!(outcome, Outcome::Skipped(SkipReason::Exists));
    assert_eq!(fs::read(&dest).unwrap(), b"local");

    let outcome = download(&engine(&s3, IfExists::Rename), &src, &dest).await;
    let Outcome::Completed(done) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(done.renamed_to, Some(dir.path().join("a (1).txt")));
    assert_eq!(fs::read(&dest).unwrap(), b"local");
    assert_eq!(fs::read(dir.path().join("a (1).txt")).unwrap(), b"remote");
    let Outcome::Completed(done) = download(&engine(&s3, IfExists::Rename), &src, &dest).await
    else {
        panic!("not completed");
    };
    assert_eq!(done.renamed_to, Some(dir.path().join("a (2).txt")));

    // newer: the object is about now; a local file stamped in the future is
    // newer, one in the past is older.
    set_mtime(&dest, 100_000);
    let outcome = download(&engine(&s3, IfExists::OverwriteIfNewer), &src, &dest).await;
    assert_eq!(outcome, Outcome::Skipped(SkipReason::NotNewer));
    assert_eq!(fs::read(&dest).unwrap(), b"local");
    set_mtime(&dest, -100_000);
    let outcome = download(&engine(&s3, IfExists::OverwriteIfNewer), &src, &dest).await;
    assert!(outcome.is_completed());
    assert_eq!(fs::read(&dest).unwrap(), b"remote");

    // overwrite and ask
    fs::write(&dest, "local").unwrap();
    assert!(
        download(&engine(&s3, IfExists::Overwrite), &src, &dest)
            .await
            .is_completed()
    );
    assert_eq!(fs::read(&dest).unwrap(), b"remote");
    fs::write(&dest, "local").unwrap();
    let resolver = Fixed::new(Choice::Skip);
    let asking = engine(&s3, IfExists::Ask).with_conflict_resolver(resolver.clone());
    assert_eq!(
        download(&asking, &src, &dest).await,
        Outcome::Skipped(SkipReason::Exists)
    );
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 1);
    let err = engine(&s3, IfExists::Ask)
        .download_file(&src, &dest, &Hooks::new(), None, &|_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert_eq!(fs::read(&dest).unwrap(), b"local");
}

// ---- copy and move ----

#[tokio::test]
async fn copy_and_move_follow_every_policy() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let src = uri(&bucket, "src/a.txt");
    let dst = uri(&bucket, "dst/a.txt");

    // skip: copy and move both leave the destination and the source alone
    s3.put(&bucket, "src/a.txt", "source").await;
    s3.put(&bucket, "dst/a.txt", "dest").await;
    let skipping = engine(&s3, IfExists::Skip);
    let outcome = skipping.copy(&src, &dst, &Hooks::new()).await.unwrap();
    assert_eq!(outcome, Outcome::Skipped(SkipReason::Exists));
    let outcome = skipping
        .move_object(&src, &dst, &Hooks::new())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Skipped(SkipReason::Exists));
    assert_eq!(s3.get(&bucket, "dst/a.txt").await, b"dest");
    assert!(
        s3.exists(&bucket, "src/a.txt").await,
        "a skipped move keeps its source"
    );

    // rename: a move creates the new name and removes the source
    let renaming = engine(&s3, IfExists::Rename);
    let Outcome::Completed(done) = renaming.copy(&src, &dst, &Hooks::new()).await.unwrap() else {
        panic!("not completed");
    };
    assert_eq!(done.renamed_to.as_deref(), Some("dst/a (1).txt"));
    assert_eq!(s3.get(&bucket, "dst/a (1).txt").await, b"source");
    let Outcome::Completed(done) = renaming
        .move_object(&src, &dst, &Hooks::new())
        .await
        .unwrap()
    else {
        panic!("not completed");
    };
    assert_eq!(done.renamed_to.as_deref(), Some("dst/a (2).txt"));
    assert!(!s3.exists(&bucket, "src/a.txt").await);
    assert_eq!(s3.get(&bucket, "dst/a.txt").await, b"dest");

    // overwrite
    s3.put(&bucket, "src/a.txt", "source").await;
    assert!(
        engine(&s3, IfExists::Overwrite)
            .copy(&src, &dst, &Hooks::new())
            .await
            .unwrap()
            .is_completed()
    );
    assert_eq!(s3.get(&bucket, "dst/a.txt").await, b"source");

    // newer compares the two objects: upload them with known local times
    let older = write(dir.path(), "o.txt", "older");
    set_mtime(&older, -100_000);
    let newer = write(dir.path(), "n.txt", "newer");
    set_mtime(&newer, 100_000);
    let plain = engine(&s3, IfExists::Overwrite);
    assert!(
        upload(&plain, &older, &uri(&bucket, "t/older"))
            .await
            .is_completed()
    );
    assert!(
        upload(&plain, &newer, &uri(&bucket, "t/newer"))
            .await
            .is_completed()
    );
    let newer_policy = engine(&s3, IfExists::OverwriteIfNewer);
    // older source onto newer destination: skipped
    let outcome = newer_policy
        .copy(
            &uri(&bucket, "t/older"),
            &uri(&bucket, "t/newer"),
            &Hooks::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Skipped(SkipReason::NotNewer));
    // newer source onto older destination: copied
    let outcome = newer_policy
        .move_object(
            &uri(&bucket, "t/newer"),
            &uri(&bucket, "t/older"),
            &Hooks::new(),
        )
        .await
        .unwrap();
    assert!(outcome.is_completed());
    assert_eq!(s3.get(&bucket, "t/older").await, b"newer");
    assert!(!s3.exists(&bucket, "t/newer").await);

    // ask
    s3.put(&bucket, "src/a.txt", "source").await;
    s3.put(&bucket, "dst/a.txt", "dest").await;
    let resolver = Fixed::new(Choice::Skip);
    let asking = engine(&s3, IfExists::Ask).with_conflict_resolver(resolver.clone());
    assert_eq!(
        asking.copy(&src, &dst, &Hooks::new()).await.unwrap(),
        Outcome::Skipped(SkipReason::Exists)
    );
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 1);
}

// ---- folder batches through the queue ----

fn provider(s3: &TestS3) -> S3Provider {
    let ops: S3 = s3.ops().clone();
    Arc::new(move |account_id: String| {
        let ops = ops.clone();
        Box::pin(async move {
            if account_id == ACCOUNT {
                Ok(ops)
            } else {
                Err(Error::NotFound(format!("account {account_id}")))
            }
        })
    })
}

async fn open_queue(s3: &TestS3) -> (TransferQueue, S3Runner) {
    let runner = S3Runner::new(provider(s3), DataIntegritySettings::default());
    let queue = TransferQueue::open(
        TransferSettings::default(),
        runner.runners(),
        QueueOptions::default(),
    )
    .await
    .unwrap();
    (queue, runner)
}

async fn wait_done(queue: &TransferQueue, tasks: &[TransferTask]) -> HashMap<String, TransferTask> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now: Vec<TransferTask> = tasks.iter().map(|t| queue.get(&t.id).unwrap()).collect();
        let finished = now.iter().all(|t| {
            matches!(
                t.state,
                TransferState::Completed | TransferState::Skipped | TransferState::Failed
            )
        });
        if finished {
            return now.into_iter().map(|t| (t.id.clone(), t)).collect();
        }
        assert!(Instant::now() < deadline, "tasks did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn states(done: &HashMap<String, TransferTask>) -> (usize, usize, usize) {
    let count = |state| done.values().filter(|t| t.state == state).count();
    (
        count(TransferState::Completed),
        count(TransferState::Skipped),
        count(TransferState::Failed),
    )
}

#[tokio::test]
async fn folder_upload_skips_existing_files_and_reports_them() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let (queue, _) = open_queue(&s3).await;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "tree/a.txt", "A");
    write(dir.path(), "tree/b.txt", "B");
    write(dir.path(), "tree/sub/c.txt", "C");
    s3.put(&bucket, "up/tree/b.txt", "old-b").await;

    let options = JobOptions {
        if_exists: Some(IfExists::Skip),
        ..JobOptions::default()
    };
    let tasks = enqueue_upload_with(
        &queue,
        ACCOUNT,
        &[dir.path().join("tree")],
        &bucket,
        "up/",
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(states(&done), (2, 1, 0));
    let skipped: Vec<&TransferTask> = done
        .values()
        .filter(|t| t.state == TransferState::Skipped)
        .collect();
    assert!(
        skipped[0]
            .error
            .as_deref()
            .unwrap()
            .contains("already exists")
    );
    assert_eq!(s3.get(&bucket, "up/tree/b.txt").await, b"old-b");
    assert_eq!(s3.get(&bucket, "up/tree/a.txt").await, b"A");

    // The same folder with `rename`: the existing files get numbered names.
    let options = JobOptions {
        if_exists: Some(IfExists::Rename),
        ..JobOptions::default()
    };
    let tasks = enqueue_upload_with(
        &queue,
        ACCOUNT,
        &[dir.path().join("tree")],
        &bucket,
        "up/",
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(states(&done), (3, 0, 0));
    assert_eq!(s3.get(&bucket, "up/tree/b (1).txt").await, b"B");
    assert_eq!(s3.get(&bucket, "up/tree/sub/c (1).txt").await, b"C");
    assert_eq!(s3.get(&bucket, "up/tree/b.txt").await, b"old-b");
}

#[tokio::test]
async fn folder_download_copy_and_move_apply_the_policy_per_item() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let (queue, _) = open_queue(&s3).await;
    let dir = tempfile::tempdir().unwrap();
    for name in ["f/a.txt", "f/b.txt", "f/c.txt"] {
        s3.put(&bucket, name, name).await;
    }

    // download with skip: one local file exists
    let local = dir.path().join("out");
    write(&local, "f/b.txt", "mine");
    let options = JobOptions {
        if_exists: Some(IfExists::Skip),
        ..JobOptions::default()
    };
    let tasks = enqueue_download_with(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["f/".to_owned()],
        &local,
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    // The folder `f/` is a task of its own, next to its three files.
    assert_eq!(states(&done), (3, 1, 0));
    assert_eq!(fs::read(local.join("f/b.txt")).unwrap(), b"mine");
    assert_eq!(fs::read(local.join("f/a.txt")).unwrap(), b"f/a.txt");

    // copy with skip onto a prefix that holds one of the names
    s3.put(&bucket, "g/f/c.txt", "keep").await;
    let tasks = enqueue_copy_with(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["f/".to_owned()],
        &bucket,
        "g/",
        &options,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(states(&done), (2, 1, 0));
    assert_eq!(s3.get(&bucket, "g/f/c.txt").await, b"keep");

    // move with rename: nothing is skipped, the source folder is emptied
    let rename = JobOptions {
        if_exists: Some(IfExists::Rename),
        ..JobOptions::default()
    };
    let tasks = enqueue_move_with(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["f/".to_owned()],
        &bucket,
        "g/",
        &rename,
    )
    .await
    .unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(states(&done), (3, 0, 0));
    assert_eq!(s3.get(&bucket, "g/f/c (1).txt").await, b"f/c.txt");
    assert!(!s3.exists(&bucket, "f/a.txt").await);
}

#[tokio::test]
async fn ask_pauses_the_task_until_the_user_answers_and_a_batch_answer_covers_the_rest() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let (queue, runner) = open_queue(&s3).await;
    let broker = runner.conflicts();
    let dir = tempfile::tempdir().unwrap();
    for name in ["a", "b", "c"] {
        write(dir.path(), &format!("t/{name}.txt"), "new");
        s3.put(&bucket, &format!("t/{name}.txt"), "old").await;
    }
    let options = JobOptions {
        if_exists: Some(IfExists::Ask),
        ..JobOptions::default()
    };
    let tasks = enqueue_upload_with(
        &queue,
        ACCOUNT,
        &[dir.path().join("t")],
        &bucket,
        "",
        &options,
    )
    .await
    .unwrap();
    // The tasks wait: nothing is written while no answer was given.
    let deadline = Instant::now() + Duration::from_secs(30);
    while broker.pending().len() < 3 {
        assert!(Instant::now() < deadline, "the questions never arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        tasks
            .iter()
            .all(|t| queue.get(&t.id).unwrap().state == TransferState::Running)
    );
    assert_eq!(s3.get(&bucket, "t/a.txt").await, b"old");
    let first = broker.pending()[0].clone();
    assert!(first.conflict.destination.starts_with("s3://"));
    broker.answer(&first.task_id, Choice::Skip, true).unwrap();
    let done = wait_done(&queue, &tasks).await;
    assert_eq!(states(&done), (0, 3, 0));
    for name in ["a", "b", "c"] {
        assert_eq!(s3.get(&bucket, &format!("t/{name}.txt")).await, b"old");
    }
    assert!(broker.pending().is_empty());
}

// ---- modification times ----

#[tokio::test]
async fn upload_stores_the_local_time_and_a_round_trip_keeps_it() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.txt", "content");
    let stamp = SystemTime::now() - Duration::from_secs(40 * 24 * 3600);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let local: chrono::DateTime<chrono::Utc> = stamp.into();
    let keep = engine(&s3, IfExists::Overwrite);
    assert!(
        upload(&keep, &path, &uri(&bucket, "a.txt"))
            .await
            .is_completed()
    );

    let head = s3.ops().head_object(&bucket, "a.txt", None).await.unwrap();
    let stored = head.meta.user_meta.get("mtime").expect("mtime metadata");
    assert_eq!(parse_mtime(stored), Some(local));

    // Downloading gives the file the stored time, not the upload time.
    let dest = dir.path().join("back.txt");
    assert!(
        download(&keep, &uri(&bucket, "a.txt"), &dest)
            .await
            .is_completed()
    );
    let back: chrono::DateTime<chrono::Utc> =
        fs::metadata(&dest).unwrap().modified().unwrap().into();
    assert_eq!(back, local);
    assert_ne!(
        head.last_modified.map(|t| t.timestamp()),
        Some(local.timestamp())
    );

    // With the option off: no metadata on upload, and the download is
    // stamped with the time of the download.
    let off = keep.with_preserve_mtime(false);
    assert!(
        upload(&off, &path, &uri(&bucket, "b.txt"))
            .await
            .is_completed()
    );
    let head = s3.ops().head_object(&bucket, "b.txt", None).await.unwrap();
    assert!(!head.meta.user_meta.contains_key("mtime"));
    let dest = dir.path().join("b.txt");
    assert!(
        download(&off, &uri(&bucket, "b.txt"), &dest)
            .await
            .is_completed()
    );
    let stamped: chrono::DateTime<chrono::Utc> =
        fs::metadata(&dest).unwrap().modified().unwrap().into();
    assert!(stamped > local + chrono::Duration::days(30));

    // An object without the metadata falls back to Last-Modified.
    s3.put(&bucket, "plain.txt", "x").await;
    let head = s3
        .ops()
        .head_object(&bucket, "plain.txt", None)
        .await
        .unwrap();
    let dest = dir.path().join("plain.txt");
    assert!(
        download(&keep, &uri(&bucket, "plain.txt"), &dest)
            .await
            .is_completed()
    );
    let got: chrono::DateTime<chrono::Utc> =
        fs::metadata(&dest).unwrap().modified().unwrap().into();
    assert_eq!(got.timestamp(), head.last_modified.unwrap().timestamp());
}
