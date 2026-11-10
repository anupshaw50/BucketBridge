//! `bb cp` and `bb mv`: the overwrite policy (`--if-exists`), modification
//! times (`--no-preserve-mtime`) and copies between accounts
//! (`--from-account`, `--to-account`).

use bucketbridge_cli::i18n::tr;

use super::*;
use std::time::{Duration, SystemTime};

fn set_mtime(path: &Path, secs_from_now: i64) {
    let now = SystemTime::now();
    let time = if secs_from_now >= 0 {
        now + Duration::from_secs(secs_from_now.unsigned_abs())
    } else {
        now - Duration::from_secs(secs_from_now.unsigned_abs())
    };
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn file(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

#[tokio::test]
async fn upload_policies_decide_what_happens_to_an_existing_object() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let local = file(dir.path(), "a.txt", "new");
    let target = format!("s3://{bucket}/a.txt");

    env.s3.put(&bucket, "a.txt", "old").await;
    // skip: reported on stderr, exit code 0, the object is untouched
    let out = env.run(&["cp", "--if-exists", "skip", &local, &target]);
    out.expect_code(0);
    assert!(out.stderr.contains(&tr("skipped")), "{}", out.stderr);
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"old");
    // ... and with --json as a record
    let out = env.run(&["cp", "--json", "--if-exists", "skip", &local, &target]);
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["copied"], false);
    assert!(record["skipped"].as_str().unwrap().contains("exists"));

    // rename
    env.run(&["cp", "--if-exists", "rename", &local, &target])
        .expect_code(0);
    env.run(&["cp", "--if-exists", "rename", &local, &target])
        .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "a (1).txt").await, b"new");
    assert_eq!(env.s3.get(&bucket, "a (2).txt").await, b"new");
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"old");

    // newer: an old local file does not replace, a new one does
    set_mtime(Path::new(&local), -100_000);
    env.run(&["cp", "--if-exists", "newer", &local, &target])
        .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"old");
    set_mtime(Path::new(&local), 100_000);
    env.run(&["cp", "--if-exists", "newer", &local, &target])
        .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"new");

    // overwrite, and an unknown policy
    env.s3.put(&bucket, "a.txt", "old").await;
    env.run(&["cp", "--if-exists", "overwrite", &local, &target])
        .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"new");
    env.run(&["cp", "--if-exists", "sometimes", &local, &target])
        .expect_code(1);
}

#[tokio::test]
async fn ask_needs_a_terminal() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let local = file(dir.path(), "a.txt", "new");
    env.s3.put(&bucket, "a.txt", "old").await;
    let out = env.run(&[
        "cp",
        "--if-exists",
        "ask",
        &local,
        &format!("s3://{bucket}/a.txt"),
    ]);
    out.expect_code(1);
    assert!(out.stderr.contains(&tr("terminal")), "{}", out.stderr);
    assert_eq!(env.s3.get(&bucket, "a.txt").await, b"old");
}

#[tokio::test]
async fn folder_download_and_move_report_skipped_items_and_keep_their_sources() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    for key in ["t/a.txt", "t/b.txt"] {
        env.s3.put(&bucket, key, key.as_bytes().to_vec()).await;
    }
    let dir = tempfile::tempdir().unwrap();
    file(dir.path(), "out/t/b.txt", "mine");
    let out = dir.path().join("out");
    let result = env.run(&[
        "cp",
        "-r",
        "--if-exists",
        "skip",
        &format!("s3://{bucket}/t/"),
        &out.display().to_string(),
    ]);
    result.expect_code(0);
    assert_eq!(std::fs::read(out.join("t/b.txt")).unwrap(), b"mine");
    assert_eq!(std::fs::read(out.join("t/a.txt")).unwrap(), b"t/a.txt");
    assert!(result.stderr.contains(&tr("skipped")), "{}", result.stderr);

    // mv of local files: a skipped file stays on this machine
    let up = file(dir.path(), "up/x.txt", "X");
    file(dir.path(), "up/y.txt", "Y");
    env.s3.put(&bucket, "m/up/x.txt", "already").await;
    env.run(&[
        "mv",
        "-r",
        "--if-exists",
        "skip",
        &dir.path().join("up").display().to_string(),
        &format!("s3://{bucket}/m/"),
    ])
    .expect_code(0);
    assert!(Path::new(&up).exists(), "the skipped source was deleted");
    assert!(!dir.path().join("up/y.txt").exists());
    assert_eq!(env.s3.get(&bucket, "m/up/x.txt").await, b"already");
    assert_eq!(env.s3.get(&bucket, "m/up/y.txt").await, b"Y");
}

#[tokio::test]
async fn modification_times_are_kept_unless_told_not_to() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let local = file(dir.path(), "a.txt", "content");
    set_mtime(Path::new(&local), -40 * 24 * 3600);
    let before = std::fs::metadata(&local).unwrap().modified().unwrap();

    env.run(&["cp", &local, &format!("s3://{bucket}/kept.txt")])
        .expect_code(0);
    env.run(&[
        "cp",
        "--no-preserve-mtime",
        &local,
        &format!("s3://{bucket}/dropped.txt"),
    ])
    .expect_code(0);
    let kept = env
        .s3
        .ops()
        .head_object(&bucket, "kept.txt", None)
        .await
        .unwrap();
    assert!(kept.meta.user_meta.contains_key("mtime"));
    let dropped = env
        .s3
        .ops()
        .head_object(&bucket, "dropped.txt", None)
        .await
        .unwrap();
    assert!(!dropped.meta.user_meta.contains_key("mtime"));

    // The round trip keeps the original time ...
    let back = dir.path().join("back.txt");
    env.run(&[
        "cp",
        &format!("s3://{bucket}/kept.txt"),
        &back.display().to_string(),
    ])
    .expect_code(0);
    let got = std::fs::metadata(&back).unwrap().modified().unwrap();
    let diff = got
        .duration_since(before)
        .unwrap_or_else(|err| err.duration());
    assert!(diff < Duration::from_secs(1), "{diff:?}");
    // ... and --no-preserve-mtime leaves the download time.
    let fresh = dir.path().join("fresh.txt");
    env.run(&[
        "cp",
        "--no-preserve-mtime",
        &format!("s3://{bucket}/kept.txt"),
        &fresh.display().to_string(),
    ])
    .expect_code(0);
    let got = std::fs::metadata(&fresh).unwrap().modified().unwrap();
    assert!(got > before + Duration::from_secs(30 * 24 * 3600));
}

/// Two stored accounts on two servers.
async fn two_accounts() -> (InProcess, TestS3, TestS3) {
    let bb = InProcess::default();
    let (first, second) = (TestS3::start().await, TestS3::start().await);
    for (label, s3) in [("First", &first), ("Second", &second)] {
        let out = bb
            .run_with(
                &[
                    "account",
                    "add",
                    "--label",
                    label,
                    "--access-key-id",
                    s3.access_key(),
                    "--region",
                    s3.region(),
                    "--endpoint",
                    s3.endpoint(),
                    "--path-style",
                    "--secret-stdin",
                ],
                &[],
                format!("{}\n", s3.secret_key()).as_bytes(),
                &Terminal::default(),
            )
            .await;
        out.expect_code(0);
    }
    (bb, first, second)
}

#[tokio::test]
async fn cp_and_mv_move_objects_between_accounts() {
    let (bb, first, second) = two_accounts().await;
    let src = first.new_bucket().await;
    let dst = second.new_bucket().await;
    for key in ["d/a.txt", "d/sub/b.txt", "one.txt"] {
        first.put(&src, key, key.as_bytes().to_vec()).await;
    }

    // one object
    bb.run(&[
        "cp",
        "--from-account",
        "First",
        "--to-account",
        "Second",
        &format!("s3://{src}/one.txt"),
        &format!("s3://{dst}/copy.txt"),
    ])
    .await
    .expect_code(0);
    assert_eq!(second.get(&dst, "copy.txt").await, b"one.txt");
    assert!(first.exists(&src, "one.txt").await);

    // a folder, copied
    bb.run(&[
        "cp",
        "-r",
        "--from-account",
        "First",
        "--to-account",
        "Second",
        &format!("s3://{src}/d/"),
        &format!("s3://{dst}/"),
    ])
    .await
    .expect_code(0);
    assert_eq!(second.get(&dst, "d/sub/b.txt").await, b"d/sub/b.txt");
    assert!(first.exists(&src, "d/a.txt").await);

    // moved: the sources go after the copies were verified
    bb.run(&[
        "mv",
        "-r",
        "--from-account",
        "First",
        "--to-account",
        "Second",
        &format!("s3://{src}/d/"),
        &format!("s3://{dst}/moved/"),
    ])
    .await
    .expect_code(0);
    assert_eq!(second.get(&dst, "moved/d/a.txt").await, b"d/a.txt");
    assert!(!first.exists(&src, "d/a.txt").await);
    assert!(!first.exists(&src, "d/sub/b.txt").await);

    // the policy applies to the destination account's objects
    let out = bb
        .run(&[
            "cp",
            "--if-exists",
            "skip",
            "--from-account",
            "First",
            "--to-account",
            "Second",
            &format!("s3://{src}/one.txt"),
            &format!("s3://{dst}/copy.txt"),
        ])
        .await;
    out.expect_code(0);
    assert!(out.stderr.contains(&tr("skipped")), "{}", out.stderr);

    // flags that do not fit the direction are usage errors
    bb.run(&[
        "cp",
        "--from-account",
        "First",
        "/tmp/x",
        &format!("s3://{dst}/x"),
    ])
    .await
    .expect_code(1);
    bb.run(&[
        "cp",
        "--from-account",
        "Nobody",
        &format!("s3://{src}/one.txt"),
        &format!("s3://{dst}/x"),
    ])
    .await
    .expect_code(2);
}

#[tokio::test]
async fn upload_rules_of_the_settings_apply_to_cp_and_sync_unless_disabled() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    std::fs::write(
        env.config.path().join("config.toml"),
        "[settings.upload_rules]\nenabled = true\n\n[[settings.upload_rules.rules]]\npattern = \"*.dat\"\ncontent_type = \"application/x-custom\"\n\n[settings.upload_rules.rules.headers]\nx-amz-meta-team = \"data\"\n",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let local = file(dir.path(), "A.DAT", "content");

    env.run(&["cp", &local, &format!("s3://{bucket}/with.dat")])
        .expect_code(0);
    let with = env
        .s3
        .ops()
        .head_object(&bucket, "with.dat", None)
        .await
        .unwrap();
    assert_eq!(
        with.meta.content_type.as_deref(),
        Some("application/x-custom")
    );
    assert_eq!(
        with.meta.user_meta.get("team").map(String::as_str),
        Some("data")
    );

    env.run(&[
        "cp",
        "--no-upload-rules",
        &local,
        &format!("s3://{bucket}/without.dat"),
    ])
    .expect_code(0);
    let without = env
        .s3
        .ops()
        .head_object(&bucket, "without.dat", None)
        .await
        .unwrap();
    assert_ne!(
        without.meta.content_type.as_deref(),
        Some("application/x-custom")
    );
    assert!(!without.meta.user_meta.contains_key("team"));

    let folder = dir.path().join("tree");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("s.dat"), "x").unwrap();
    env.run(&[
        "sync",
        folder.to_str().unwrap(),
        &format!("s3://{bucket}/synced"),
    ])
    .expect_code(0);
    let synced = env
        .s3
        .ops()
        .head_object(&bucket, "synced/s.dat", None)
        .await
        .unwrap();
    assert_eq!(
        synced.meta.content_type.as_deref(),
        Some("application/x-custom")
    );
}
