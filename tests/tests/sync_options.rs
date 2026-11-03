//! End-to-end tests of the v0.2.0 sync options against an in-process S3 server:
//! S3 to S3 sync, and the asynchronous analysis with job options.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use bucketbridge_core::engine::{TransferEngine, TransferOptions};
use bucketbridge_core::model::{Side, SyncAction, SyncDirection, SyncPlan};
use bucketbridge_core::sync::{
    self, AnalysisRequest, ExecuteOptions, NoSyncEvents, SyncExecutor, SyncOptions,
};
use bucketbridge_core::transfer::BandwidthLimiter;
use bucketbridge_core::{S3Uri, SyncStore};
use bucketbridge_tests::TestS3;

struct Fixture {
    s3: TestS3,
    dir: tempfile::TempDir,
    root: PathBuf,
    executor: SyncExecutor,
}

impl Fixture {
    async fn new() -> Fixture {
        let s3 = TestS3::start().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("local");
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(SyncStore::open(dir.path().join("plans")).unwrap());
        Fixture {
            s3,
            dir,
            root,
            executor: SyncExecutor::new(store),
        }
    }

    fn engine(&self) -> TransferEngine {
        TransferEngine::new(self.s3.ops().clone(), TransferOptions::default()).unwrap()
    }

    async fn analyze(&self, request: &AnalysisRequest) -> SyncPlan {
        let plan = sync::analyze_async(self.s3.ops(), request).await.unwrap();
        self.executor.store().insert(&plan).unwrap();
        plan
    }

    async fn run(&self, plan: &SyncPlan) -> sync::RunSummary {
        self.executor
            .start(
                &plan.id,
                self.engine(),
                ExecuteOptions {
                    concurrency: 4,
                    limiter: BandwidthLimiter::unlimited(),
                },
                Arc::new(NoSyncEvents),
            )
            .unwrap()
            .run()
            .await
            .unwrap()
    }
}

fn copy_request(source: &S3Uri, destination: &S3Uri, delete: bool) -> AnalysisRequest {
    AnalysisRequest {
        account_id: "test-account".to_owned(),
        direction: SyncDirection::S3ToS3,
        local_root: PathBuf::new(),
        s3_prefix: destination.clone(),
        source_prefix: Some(source.clone()),
        delete,
        options: SyncOptions::default(),
        cache_dir: None,
    }
}

fn summary(plan: &SyncPlan) -> Vec<(&str, SyncAction)> {
    plan.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.action))
        .collect()
}

#[tokio::test]
async fn s3_to_s3_copies_between_buckets_and_deletes_extras() {
    let fx = Fixture::new().await;
    let src_bucket = fx.s3.new_bucket().await;
    let dst_bucket = fx.s3.new_bucket().await;
    fx.s3.put(&src_bucket, "in/a.txt", "alpha").await;
    fx.s3.put(&src_bucket, "in/deep/b.txt", "beta").await;
    fx.s3.put(&src_bucket, "other/skip.txt", "outside").await;
    fx.s3.put(&dst_bucket, "out/extra.txt", "extra").await;
    fx.s3.put(&dst_bucket, "out/a.txt", "old").await;

    let source = S3Uri::new(&src_bucket, "in/").unwrap();
    let destination = S3Uri::new(&dst_bucket, "out/").unwrap();

    // Without delete the extra object stays.
    let plan = fx
        .analyze(&copy_request(&source, &destination, false))
        .await;
    assert_eq!(plan.direction, SyncDirection::S3ToS3);
    assert_eq!(
        summary(&plan),
        vec![
            ("a.txt", SyncAction::Copy),
            ("deep/b.txt", SyncAction::Copy)
        ]
    );
    assert!(plan.rows.iter().all(|row| row.target == Some(Side::Remote)));
    // The stored plan keeps the source.
    assert_eq!(
        fx.executor.store().get(&plan.id).unwrap().source_prefix,
        Some(source.clone())
    );
    let run = fx.run(&plan).await;
    assert_eq!(run.errors, 0, "{run:?}");
    assert_eq!(fx.s3.get(&dst_bucket, "out/a.txt").await, b"alpha");
    assert_eq!(fx.s3.get(&dst_bucket, "out/deep/b.txt").await, b"beta");
    assert!(fx.s3.exists(&dst_bucket, "out/extra.txt").await);
    // The source is untouched.
    assert!(fx.s3.exists(&src_bucket, "in/a.txt").await);

    // Now in sync, except for the extra object, which delete removes.
    let again = fx.analyze(&copy_request(&source, &destination, true)).await;
    assert_eq!(
        summary(&again),
        vec![
            ("a.txt", SyncAction::Equal),
            ("deep/b.txt", SyncAction::Equal),
            ("extra.txt", SyncAction::Delete),
        ]
    );
    let run = fx.run(&again).await;
    assert_eq!(run.errors, 0);
    assert!(!fx.s3.exists(&dst_bucket, "out/extra.txt").await);
}

#[tokio::test]
async fn s3_to_s3_works_inside_one_bucket_and_refuses_overlap() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fx.s3.put(&bucket, "a/one.txt", "1").await;
    let source = S3Uri::new(&bucket, "a/").unwrap();
    let destination = S3Uri::new(&bucket, "b/").unwrap();
    let plan = fx
        .analyze(&copy_request(&source, &destination, false))
        .await;
    fx.run(&plan).await;
    assert_eq!(fx.s3.get(&bucket, "b/one.txt").await, b"1");

    let nested = S3Uri::new(&bucket, "a/inner/").unwrap();
    let err = sync::analyze_async(fx.s3.ops(), &copy_request(&source, &nested, false))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Usage);
}

#[tokio::test]
async fn s3_to_s3_never_replaces_what_changed_after_the_analysis() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fx.s3.put(&bucket, "a/one.txt", "source").await;
    fx.s3.put(&bucket, "a/two.txt", "source two").await;
    let source = S3Uri::new(&bucket, "a/").unwrap();
    let destination = S3Uri::new(&bucket, "b/").unwrap();
    let plan = fx
        .analyze(&copy_request(&source, &destination, false))
        .await;
    // A destination object appears, and a source object changes size.
    fx.s3.put(&bucket, "b/one.txt", "someone else's").await;
    fx.s3.put(&bucket, "a/two.txt", "source two, longer").await;
    let run = fx.run(&plan).await;
    assert_eq!(run.errors, 2, "{run:?}");
    assert_eq!(fx.s3.get(&bucket, "b/one.txt").await, b"someone else's");
    assert!(!fx.s3.exists(&bucket, "b/two.txt").await);
    let stored = fx.executor.store().get(&plan.id).unwrap();
    assert!(stored.rows.iter().all(|row| {
        row.error
            .as_deref()
            .is_some_and(|error| error.contains("analyze again"))
    }));
}

#[tokio::test]
async fn the_async_analysis_applies_job_options_to_a_local_sync() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fs::create_dir_all(fx.root.join("build")).unwrap();
    fs::write(fx.root.join("keep.txt"), "k").unwrap();
    fs::write(fx.root.join("skip.tmp"), "t").unwrap();
    fs::write(fx.root.join("build/out.bin"), "b").unwrap();
    fx.s3.put(&bucket, "p/skip.tmp", "remote tmp").await;
    let rules = fx.dir.path().join("rules.txt");
    fs::write(&rules, "# build output\nbuild/\n*.tmp\n!keep.tmp\n").unwrap();

    let request = AnalysisRequest {
        account_id: "test-account".to_owned(),
        direction: SyncDirection::Upload,
        local_root: fx.root.clone(),
        s3_prefix: S3Uri::new(&bucket, "p/").unwrap(),
        source_prefix: None,
        delete: true,
        options: SyncOptions {
            exclude_file: Some(rules.display().to_string()),
            ..SyncOptions::default()
        },
        cache_dir: None,
    };
    let plan = fx.analyze(&request).await;
    // Excluded paths are out of scope on both sides: skip.tmp is neither uploaded nor
    // deleted remotely.
    assert_eq!(summary(&plan), vec![("keep.txt", SyncAction::Copy)]);
    fx.run(&plan).await;
    assert_eq!(fx.s3.get(&bucket, "p/keep.txt").await, b"k");
    assert!(fx.s3.exists(&bucket, "p/skip.tmp").await);
}

fn set_modified_in_the_future(path: &std::path::Path) {
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(later)
        .unwrap();
}

fn upload_request(fx: &Fixture, bucket: &str, options: SyncOptions) -> AnalysisRequest {
    AnalysisRequest {
        account_id: "test-account".to_owned(),
        direction: SyncDirection::Upload,
        local_root: fx.root.clone(),
        s3_prefix: S3Uri::new(bucket, "p/").unwrap(),
        source_prefix: None,
        delete: false,
        options,
        cache_dir: None,
    }
}

#[tokio::test]
async fn skip_duplicate_content_treats_equal_files_as_equal_despite_the_time() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fs::write(fx.root.join("same.txt"), "identical").unwrap();
    fs::write(fx.root.join("changed.txt"), "AAAAAAAAA").unwrap();
    fs::write(fx.root.join("bigger.txt"), "longer than remote").unwrap();
    fx.s3.put(&bucket, "p/same.txt", "identical").await;
    fx.s3.put(&bucket, "p/changed.txt", "BBBBBBBBB").await;
    fx.s3.put(&bucket, "p/bigger.txt", "short").await;
    for name in ["same.txt", "changed.txt", "bigger.txt"] {
        set_modified_in_the_future(&fx.root.join(name));
    }

    // By default the newer local files are copied.
    let plain = fx
        .analyze(&upload_request(&fx, &bucket, SyncOptions::default()))
        .await;
    assert_eq!(
        summary(&plain),
        vec![
            ("bigger.txt", SyncAction::Copy),
            ("changed.txt", SyncAction::Copy),
            ("same.txt", SyncAction::Copy),
        ]
    );

    // With the option only the files whose content differs are.
    let options = SyncOptions {
        skip_duplicate_content: true,
        ..SyncOptions::default()
    };
    let skipping = fx.analyze(&upload_request(&fx, &bucket, options)).await;
    assert_eq!(
        summary(&skipping),
        vec![
            ("bigger.txt", SyncAction::Copy),
            ("changed.txt", SyncAction::Copy),
            ("same.txt", SyncAction::Equal),
        ]
    );
    let same = skipping
        .rows
        .iter()
        .find(|row| row.rel_path == "same.txt")
        .unwrap();
    assert!(same.done && same.target.is_none());
    // The equal row is never executed; the copies are.
    let run = fx.run(&skipping).await;
    assert_eq!(run.errors, 0);
    assert_eq!(fx.s3.get(&bucket, "p/changed.txt").await, b"AAAAAAAAA");
}

#[tokio::test]
async fn skip_duplicate_content_applies_to_downloads_and_s3_copies() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fs::write(fx.root.join("a.txt"), "content").unwrap();
    fx.s3.put(&bucket, "p/a.txt", "content").await;
    // The local file is old: a download would normally replace it with the S3 object.
    fs::File::options()
        .write(true)
        .open(fx.root.join("a.txt"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
    let mut request = upload_request(
        &fx,
        &bucket,
        SyncOptions {
            skip_duplicate_content: true,
            ..SyncOptions::default()
        },
    );
    request.direction = SyncDirection::Download;
    assert_eq!(
        summary(&fx.analyze(&request).await),
        vec![("a.txt", SyncAction::Equal)]
    );
    request.options.skip_duplicate_content = false;
    assert_eq!(
        summary(&fx.analyze(&request).await),
        vec![("a.txt", SyncAction::Copy)]
    );

    // Two buckets: the source is newer than the destination by more than the tolerance.
    let other = fx.s3.new_bucket().await;
    fx.s3.put(&other, "dst/same.txt", "twin").await;
    fx.s3.put(&other, "dst/diff.txt", "AAAA").await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    fx.s3.put(&bucket, "src/same.txt", "twin").await;
    fx.s3.put(&bucket, "src/diff.txt", "BBBB").await;
    let source = S3Uri::new(&bucket, "src/").unwrap();
    let destination = S3Uri::new(&other, "dst/").unwrap();
    let mut copy = copy_request(&source, &destination, false);
    let plain = fx.analyze(&copy).await;
    assert_eq!(
        summary(&plain),
        vec![
            ("diff.txt", SyncAction::Copy),
            ("same.txt", SyncAction::Copy)
        ]
    );
    copy.options.skip_duplicate_content = true;
    let skipping = fx.analyze(&copy).await;
    assert_eq!(
        summary(&skipping),
        vec![
            ("diff.txt", SyncAction::Copy),
            ("same.txt", SyncAction::Equal)
        ]
    );
}

#[tokio::test]
async fn empty_folders_are_ignored_by_default_and_become_markers_when_asked() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fs::create_dir_all(fx.root.join("empty")).unwrap();
    fs::create_dir_all(fx.root.join("nested/leaf")).unwrap();
    fs::create_dir_all(fx.root.join("full")).unwrap();
    fs::write(fx.root.join("full/file.txt"), "f").unwrap();

    // The default: only files.
    let plain = fx
        .analyze(&upload_request(&fx, &bucket, SyncOptions::default()))
        .await;
    assert_eq!(summary(&plain), vec![("full/file.txt", SyncAction::Copy)]);

    let options = SyncOptions {
        skip_empty_folders: false,
        ..SyncOptions::default()
    };
    let plan = fx.analyze(&upload_request(&fx, &bucket, options)).await;
    assert_eq!(
        summary(&plan),
        vec![
            ("empty/", SyncAction::Copy),
            ("full/file.txt", SyncAction::Copy),
            ("nested/leaf/", SyncAction::Copy),
        ]
    );
    assert!(
        plan.rows
            .iter()
            .filter(|row| row.rel_path.ends_with('/'))
            .all(|row| row.target == Some(Side::Remote) && row.local_meta.is_some())
    );
    let run = fx.run(&plan).await;
    assert_eq!(run.errors, 0, "{run:?}");
    // s3s-fs keeps markers as directories and never lists them (docs/testing.md).
    let root = fx.s3.root().unwrap().join(&bucket).join("p");
    assert!(root.join("empty").is_dir());
    assert!(root.join("nested/leaf").is_dir());
    assert_eq!(fx.s3.get(&bucket, "p/full/file.txt").await, b"f");
}

#[tokio::test]
async fn a_folder_row_creates_the_local_folder_and_never_works_through_a_link() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    let marker = |rel: &str| bucketbridge_core::model::SyncRow {
        rel_path: rel.to_owned(),
        action: SyncAction::Copy,
        target: Some(Side::Local),
        local_meta: None,
        s3_meta: Some(bucketbridge_core::model::ObjectEntry {
            key: format!("p/{rel}"),
            size: 0,
            etag: None,
            last_modified: None,
            storage_class: None,
            checksums: bucketbridge_core::model::Checksums::default(),
            is_prefix: false,
        }),
        error: None,
        done: false,
    };
    let outside = fx.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let mut rows = vec![marker("a/b/"), marker("blocked/inner/"), marker("file/")];
    fs::write(fx.root.join("file"), "a file").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, fx.root.join("blocked")).unwrap();
    #[cfg(not(unix))]
    rows.remove(1);
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    let plan = SyncPlan {
        id: uuid_v4(),
        account_id: "test-account".to_owned(),
        local_root: fx.root.clone(),
        s3_prefix: S3Uri::new(&bucket, "p/").unwrap(),
        source_prefix: None,
        direction: SyncDirection::Download,
        delete: false,
        created_at: chrono::Utc::now(),
        rows,
    };
    fx.executor.store().insert(&plan).unwrap();
    let run = fx.run(&plan).await;
    assert_eq!(run.attempted, plan.rows.len());
    assert!(fx.root.join("a/b").is_dir());
    // A file in the way and a link on the way are left alone and fail their rows.
    let stored = fx.executor.store().get(&plan.id).unwrap();
    let state = |rel: &str| stored.rows.iter().find(|row| row.rel_path == rel).unwrap();
    assert!(state("a/b/").done);
    assert!(!state("file/").done && state("file/").error.is_some());
    #[cfg(unix)]
    {
        assert!(!state("blocked/inner/").done);
        assert!(!outside.join("inner").exists());
    }
    // Running again changes nothing for the finished row.
    let again = fx.run(&plan).await;
    assert_eq!(again.attempted, plan.rows.len() - 1);
}

fn uuid_v4() -> String {
    bucketbridge_core::sync::new_plan_id()
}

#[tokio::test]
async fn the_analysis_uses_the_cache_only_when_allowed_and_never_when_deleting() {
    let fx = Fixture::new().await;
    let bucket = fx.s3.new_bucket().await;
    fs::write(fx.root.join("a.txt"), "a").unwrap();
    fx.s3.put(&bucket, "p/b.txt", "remote").await;
    let cache_dir = fx.dir.path().join("sync-cache");
    let mut request = upload_request(&fx, &bucket, SyncOptions::default());
    request.cache_dir = Some(cache_dir.clone());

    // Deleting always lists afresh: nothing is read from or written to the cache.
    request.delete = true;
    fx.analyze(&request).await;
    assert!(!cache_dir.exists());

    // The option off: the same.
    request.delete = false;
    request.options.use_cache = false;
    fx.analyze(&request).await;
    assert!(!cache_dir.exists());

    // Allowed: the listing is kept, and a later analysis sees what changed meanwhile.
    request.options.use_cache = true;
    fx.analyze(&request).await;
    let cache = bucketbridge_core::sync::ListingCache::at(cache_dir.clone());
    let snapshot = cache.load("test-account", &bucket, "p/").unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    fx.s3.put(&bucket, "p/a.txt", "now there").await;
    let plan = fx.analyze(&request).await;
    assert_eq!(
        summary(&plan),
        vec![("a.txt", SyncAction::Copy)],
        "a changed prefix is listed again"
    );
    assert_eq!(
        cache
            .load("test-account", &bucket, "p/")
            .unwrap()
            .entries
            .len(),
        2
    );
}
