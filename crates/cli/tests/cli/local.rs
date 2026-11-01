//! `bb cp` and `bb mv` between local files and S3: uploads and downloads of
//! files and folders, filters, dry runs, failures and Ctrl-C.

use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, Instant};

use bucketbridge_tests::MockResponse;

use super::*;

/// A local folder `photos` with files in subfolders and an empty folder.
fn photo_tree(dir: &Path) -> PathBuf {
    let photos = dir.join("photos");
    for (path, body) in [
        ("a.jpg", "a"),
        ("b.png", "bb"),
        ("2024/c.jpg", "ccc"),
        ("2024/x/d.png", "dddd"),
    ] {
        let file = photos.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, body).unwrap();
    }
    fs::create_dir_all(photos.join("empty")).unwrap();
    photos
}

/// Every file below `dir`, as sorted `/`-separated relative paths.
fn files_below(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out.sort();
    out
}

#[tokio::test]
async fn single_files_go_up_and_down() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    fs::write(dir.join("note.txt"), "hello").unwrap();

    // Into a prefix with its own name, or to an exact key.
    let out = env.run_in(dir, &["cp", "note.txt", &format!("s3://{bucket}/")]);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!("uploaded note.txt to s3://{bucket}/note.txt")
    );
    env.run_in(
        dir,
        &["cp", "note.txt", &format!("s3://{bucket}/docs/renamed.txt")],
    )
    .expect_code(0);
    assert_eq!(env.keys(&bucket).await, ["docs/renamed.txt", "note.txt"]);
    assert_eq!(env.s3.get(&bucket, "docs/renamed.txt").await, b"hello");

    // Into an existing folder, into a new folder (trailing separator), or
    // to an exact path; missing parent folders are created.
    fs::create_dir(dir.join("in")).unwrap();
    let out = env.run_in(
        dir,
        &["cp", &format!("s3://{bucket}/docs/renamed.txt"), "in"],
    );
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!(
            "downloaded s3://{bucket}/docs/renamed.txt to {}",
            Path::new("in").join("renamed.txt").display()
        )
    );
    env.run_in(dir, &["cp", &format!("s3://{bucket}/note.txt"), "new/"])
        .expect_code(0);
    env.run_in(
        dir,
        &["cp", &format!("s3://{bucket}/note.txt"), "deep/er/copy.txt"],
    )
    .expect_code(0);
    assert_eq!(
        files_below(dir),
        [
            "deep/er/copy.txt",
            "in/renamed.txt",
            "new/note.txt",
            "note.txt"
        ]
    );
    assert_eq!(fs::read(dir.join("deep/er/copy.txt")).unwrap(), b"hello");

    // One NDJSON record per item.
    let out = env.run_in(
        dir,
        &["cp", "--json", "note.txt", &format!("s3://{bucket}/j.txt")],
    );
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["source"], "note.txt");
    assert_eq!(record["destination"], format!("s3://{bucket}/j.txt"));
    assert_eq!(record["size"], 5);
    assert!(record["etag"].is_string(), "{record}");
    assert_eq!(record["copied"], true);
    assert_eq!(record["dry_run"], false);
    assert_eq!(record["source_deleted"], false);
    assert!(record["error"].is_null());
    let out = env.run_in(
        dir,
        &["cp", "--json", &format!("s3://{bucket}/j.txt"), "j.txt"],
    );
    out.expect_code(0);
    assert_eq!(out.records()[0]["size"], 5);
    assert_eq!(out.records()[0]["destination"], "j.txt");
    // --quiet prints nothing for successful transfers.
    let out = env.run_in(
        dir,
        &["cp", "-q", "note.txt", &format!("s3://{bucket}/q.txt")],
    );
    out.expect_code(0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

#[tokio::test]
async fn folders_land_under_the_destination_with_their_name() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    photo_tree(dir);

    let out = env.run_in(
        dir,
        &["cp", "-r", "photos", &format!("s3://{bucket}/backup")],
    );
    out.expect_code(0);
    assert_eq!(out.lines().len(), 5, "{}", out.stdout);
    assert_eq!(
        env.keys(&bucket).await,
        [
            "backup/photos/2024/c.jpg",
            "backup/photos/2024/x/d.png",
            "backup/photos/a.jpg",
            "backup/photos/b.png"
        ]
    );
    // Empty folders become folder markers.
    assert!(env.marker_exists(&bucket, "backup/photos/empty/").await);

    // The selected prefix lands in the folder, which is created.
    let out = env.run_in(
        dir,
        &[
            "cp",
            "-r",
            &format!("s3://{bucket}/backup/photos/2024/"),
            "restore",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        files_below(&dir.join("restore")),
        ["2024/c.jpg", "2024/x/d.png"]
    );
    assert_eq!(fs::read(dir.join("restore/2024/x/d.png")).unwrap(), b"dddd");
    // A whole bucket lands with its keys.
    env.run_in(dir, &["cp", "-r", &format!("s3://{bucket}"), "all"])
        .expect_code(0);
    assert_eq!(
        files_below(&dir.join("all")),
        [
            "backup/photos/2024/c.jpg",
            "backup/photos/2024/x/d.png",
            "backup/photos/a.jpg",
            "backup/photos/b.png"
        ]
    );
    // Nothing below the prefix: nothing to do.
    let out = env.run_in(
        dir,
        &["cp", "-r", &format!("s3://{bucket}/nothing/"), "none"],
    );
    out.expect_code(0);
    assert!(out.stdout.is_empty());
    assert!(!dir.join("none").exists());
}

#[tokio::test]
async fn include_and_exclude_match_paths_below_the_selection() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    photo_tree(dir);

    // `**/` matches any depth, including none; a folder ends with `/`.
    env.run_in(
        dir,
        &[
            "cp",
            "-r",
            "photos",
            &format!("s3://{bucket}/up/"),
            "--exclude",
            "**/*.png",
            "--exclude",
            "empty/",
        ],
    )
    .expect_code(0);
    assert_eq!(
        env.keys(&bucket).await,
        ["up/photos/2024/c.jpg", "up/photos/a.jpg"]
    );
    assert!(!env.marker_exists(&bucket, "up/photos/empty/").await);

    // The last matching rule wins; `*` stays within one folder level.
    env.run_in(dir, &["cp", "-r", "photos", &format!("s3://{bucket}/all/")])
        .expect_code(0);
    let out = env.run_in(
        dir,
        &[
            "cp",
            "-r",
            &format!("s3://{bucket}/all/photos/"),
            "down",
            "--exclude",
            "*",
            "--include",
            "*.jpg",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        files_below(&dir.join("down")),
        ["photos/2024/c.jpg", "photos/2024/x/d.png", "photos/a.jpg"]
    );
}

#[tokio::test]
async fn dry_runs_write_nothing() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "remote/r.txt", "remote").await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    photo_tree(dir);

    let out = env.run_in(
        dir,
        &[
            "mv",
            "-r",
            "--dry-run",
            "--json",
            "photos",
            &format!("s3://{bucket}/"),
        ],
    );
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 5);
    assert!(
        records
            .iter()
            .all(|r| r["dry_run"] == true && r["copied"] == false && r["source_deleted"] == false)
    );
    assert_eq!(env.keys(&bucket).await, ["remote/r.txt"]);
    assert_eq!(files_below(dir).len(), 4);

    let out = env.run_in(
        dir,
        &[
            "cp",
            "-r",
            "--dry-run",
            &format!("s3://{bucket}/remote/"),
            "out",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!(
            "would download s3://{bucket}/remote/ to {}\nwould download s3://{bucket}/remote/r.txt to {}",
            Path::new("out").join("remote").display(),
            Path::new("out").join("remote").join("r.txt").display()
        )
    );
    assert!(!dir.join("out").exists());
    let out = env.run_in(
        dir,
        &[
            "mv",
            "--dry-run",
            &format!("s3://{bucket}/remote/r.txt"),
            "r.txt",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!("would move s3://{bucket}/remote/r.txt to r.txt")
    );
    assert!(!dir.join("r.txt").exists());
    assert!(env.s3.exists(&bucket, "remote/r.txt").await);
    // A dry run still needs the source.
    env.run_in(
        dir,
        &["cp", "--dry-run", &format!("s3://{bucket}/missing"), "m"],
    )
    .expect_code(2);
    env.run_in(
        dir,
        &["cp", "--dry-run", "missing.txt", &format!("s3://{bucket}/")],
    )
    .expect_code(2);
}

#[tokio::test]
async fn mv_deletes_each_source_after_its_transfer() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    let photos = photo_tree(dir);

    // Excluded files stay, and so do the folders holding them.
    let out = env.run_in(
        dir,
        &[
            "mv",
            "-r",
            "--json",
            "photos",
            &format!("s3://{bucket}/moved/"),
            "--exclude",
            "2024/x/**",
        ],
    );
    out.expect_code(0);
    assert!(
        out.records()
            .iter()
            .all(|r| r["copied"] == true && r["source_deleted"] == true)
    );
    assert_eq!(files_below(dir), ["photos/2024/x/d.png"]);
    assert!(!photos.join("empty").exists());
    assert_eq!(
        env.keys(&bucket).await,
        [
            "moved/photos/2024/c.jpg",
            "moved/photos/a.jpg",
            "moved/photos/b.png"
        ]
    );
    // Moving the rest removes the emptied folders up to the selected one.
    env.run_in(
        dir,
        &["mv", "-r", "photos", &format!("s3://{bucket}/moved/")],
    )
    .expect_code(0);
    assert!(!photos.exists());
    assert_eq!(env.keys(&bucket).await.len(), 4);

    // Back down: the objects are deleted after their download.
    let out = env.run_in(
        dir,
        &["mv", "-r", &format!("s3://{bucket}/moved/photos/"), "back"],
    );
    out.expect_code(0);
    assert_eq!(
        files_below(&dir.join("back")),
        [
            "photos/2024/c.jpg",
            "photos/2024/x/d.png",
            "photos/a.jpg",
            "photos/b.png"
        ]
    );
    assert!(env.keys(&bucket).await.is_empty());

    // A failed transfer keeps its source.
    fs::write(dir.join("keep.txt"), "k").unwrap();
    let out = run(env
        .bb()
        .current_dir(dir)
        .env("BB_SECRET_ACCESS_KEY", "wrong")
        .args(["mv", "keep.txt", &format!("s3://{bucket}/keep.txt")]));
    out.expect_code(3);
    assert!(dir.join("keep.txt").exists());
    assert!(!env.s3.exists(&bucket, "keep.txt").await);
    let out = env.run_in(
        dir,
        &[
            "mv",
            "--json",
            &format!("s3://{bucket}/missing.txt"),
            "missing.txt",
        ],
    );
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    assert!(!dir.join("missing.txt").exists());
    assert!(!dir.join("missing.txt.bbpart").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_files_are_reported_and_the_rest_is_transferred() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    let photos = photo_tree(dir);
    let locked = photos.join("2024/c.jpg");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&locked).is_ok() {
        // Running as root: permissions do not apply.
        return;
    }
    let out = env.run_in(
        dir,
        &["cp", "-r", "--json", "photos", &format!("s3://{bucket}/")],
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    out.expect_code(2);
    let records = out.records();
    assert_eq!(records.len(), 5);
    let failed: Vec<&serde_json::Value> = records.iter().filter(|r| r["copied"] == false).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["error"]["code"], "io");
    assert!(failed[0]["source"].as_str().unwrap().ends_with("c.jpg"));
    // The summary of an I/O failure carries the first one's cause.
    let message = out.error_record()["message"].as_str().unwrap().to_owned();
    assert!(
        message.starts_with("1 of 5 files could not be uploaded: ") && message.contains("c.jpg"),
        "{message}"
    );
    assert_eq!(env.keys(&bucket).await.len(), 3);

    // Without --json the failure is a line on stderr.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = env.run_in(
        dir,
        &["cp", "-r", "photos", &format!("s3://{bucket}/again/")],
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    out.expect_code(2);
    assert!(out.stderr.contains("bb: cannot upload "), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("bb: error: 1 of 5 files could not be uploaded: "),
        "{}",
        out.stderr
    );
}

#[tokio::test]
async fn usage_errors_and_missing_sources() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    photo_tree(dir);
    for (args, code, expected) in [
        (vec!["cp", "a.txt", "b.txt"], 1, "both locations are local"),
        (vec!["cp", "photos", "s3://b/"], 1, "--recursive"),
        (vec!["cp", "-r", "s3://b/k", "out"], 1, "--recursive"),
        (vec!["mv", "s3://b/p/", "out"], 1, "--recursive"),
        (vec!["cp", "missing.txt", "s3://b/"], 2, "missing.txt"),
    ] {
        let mut args = args;
        let fixed: Vec<String> = args
            .iter()
            .map(|a| a.replace("s3://b/", &format!("s3://{bucket}/")))
            .collect();
        args = fixed.iter().map(String::as_str).collect();
        let out = env.run_in(dir, &args);
        out.expect_code(code);
        assert!(out.stderr.contains(expected), "{args:?}: {}", out.stderr);
    }
    // A file where a folder is needed.
    fs::write(dir.join("file"), "x").unwrap();
    env.s3.put(&bucket, "p/k", "v").await;
    let out = env.run_in(dir, &["cp", "-r", &format!("s3://{bucket}/p/"), "file"]);
    out.expect_code(1);
    assert!(out.stderr.contains("is a file"), "{}", out.stderr);
    assert!(env.keys(&bucket).await.len() == 1);
}

#[tokio::test]
async fn clashing_destinations_stop_everything_before_it_starts() {
    // `p/x` and `p/x/y` cannot both exist as local files.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if request.query_param("list-type").is_some() => {
            MockResponse::xml(xml::objects(&["p/a", "p/x", "p/x/y"]))
        }
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    });
    let config = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path())
        .current_dir(work.path())
        .args(["cp", "-r", "--json", "s3://bucket/p/", "out"]));
    out.expect_code(2);
    let error = out.error_record();
    assert_eq!(error["code"], "conflict");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("nothing was downloaded"),
        "{error}"
    );
    assert!(out.stdout.is_empty());
    assert!(
        mock.requests()
            .iter()
            .all(|r| r.query_param("list-type").is_some())
    );
    assert!(!work.path().join("out").exists());
}

/// Transfers slow enough to interrupt: 5 MiB parts at 256 KiB/s.
#[cfg(unix)]
fn slow_transfers(env: &Env) {
    fs::write(
        env.config.path().join("config.toml"),
        "[settings.transfer]\nmultipart_threshold_mib = 5\npart_size_mib = 5\nbandwidth_limit_kibps = 256\nretry_attempts = 0\n",
    )
    .unwrap();
}

/// Sends Ctrl-C (SIGINT) to `child` once `ready` holds; returns its exit
/// code and stderr.
#[cfg(unix)]
async fn interrupt_when(
    mut child: std::process::Child,
    mut ready: impl AsyncFnMut() -> bool,
) -> (Option<i32>, String, Duration) {
    use std::io::Read;

    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready().await {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("bb exited early: {status}");
        }
        assert!(Instant::now() < deadline, "the transfer never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let started = Instant::now();
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "bb did not stop after Ctrl-C"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (exit.code(), stderr, started.elapsed())
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_aborts_an_upload_and_exits_with_130() {
    use std::process::Stdio;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    slow_transfers(&env);
    let work = tempfile::tempdir().unwrap();
    let big = work.path().join("big.bin");
    fs::write(&big, vec![7u8; 12 * 1024 * 1024]).unwrap();
    let child = env
        .process()
        .args([
            "mv",
            big.to_str().unwrap(),
            &format!("s3://{bucket}/big.bin"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (code, stderr, took) = interrupt_when(child, async || {
        env.s3.pending_uploads(&bucket, "big.bin").await > 0
    })
    .await;
    assert_eq!(code, Some(130), "{stderr}");
    assert!(stderr.contains("cancelled"), "{stderr}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
    // The multipart upload was aborted, and mv kept its source.
    assert_eq!(env.s3.pending_uploads(&bucket, "big.bin").await, 0);
    assert!(!env.s3.exists(&bucket, "big.bin").await);
    assert!(big.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_removes_a_partial_download_and_exits_with_130() {
    use std::process::Stdio;

    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3
        .put(&bucket, "big.bin", vec![3u8; 12 * 1024 * 1024])
        .await;
    slow_transfers(&env);
    let work = tempfile::tempdir().unwrap();
    let dest = work.path().join("big.bin");
    let partial = work.path().join("big.bin.bbpart");
    let child = env
        .process()
        .args([
            "mv",
            &format!("s3://{bucket}/big.bin"),
            dest.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (code, stderr, took) = interrupt_when(child, async || partial.exists()).await;
    assert_eq!(code, Some(130), "{stderr}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
    assert!(!partial.exists());
    assert!(!dest.exists());
    // mv kept its source.
    assert!(env.s3.exists(&bucket, "big.bin").await);
}
