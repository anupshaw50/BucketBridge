//! `bb edit`. The "editor" is a small shell script that saves a new content
//! into the copy it is given, so these tests run on Unix only.

#![cfg(unix)]

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

const WAIT: Duration = Duration::from_secs(40);

/// An editor script: reports that it started, waits for the `go` file and
/// then writes `$PAYLOAD` into the file it was given, in the background.
fn editor(dir: &Path) -> PathBuf {
    let path = dir.join("editor.sh");
    fs::write(
        &path,
        "#!/bin/sh\n\
         touch \"$STARTED\"\n\
         ( while [ ! -f \"$GO\" ]; do sleep 0.05; done; printf '%s' \"$PAYLOAD\" > \"$1\" ) >/dev/null 2>&1 &\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Session {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    started: PathBuf,
    go: PathBuf,
}

/// Starts `bb edit uri --app <editor>` writing `payload` once `go` exists.
fn start(env: &Env, scratch: &Path, uri: &str, payload: &str) -> Session {
    let (stdout, stderr) = (scratch.join("stdout"), scratch.join("stderr"));
    let (started, go) = (scratch.join("started"), scratch.join("go"));
    let child = env
        .process()
        .args(["edit", "--app"])
        .arg(editor(scratch))
        .arg(uri)
        .env("STARTED", &started)
        .env("GO", &go)
        .env("PAYLOAD", payload)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    Session {
        child,
        stdout,
        stderr,
        started,
        go,
    }
}

impl Session {
    async fn wait_started(&self) {
        wait_until("the editor to start", || self.started.exists()).await;
    }

    fn save(&self) {
        fs::write(&self.go, b"").unwrap();
    }

    /// Ctrl-C, then the exit code and the two streams.
    async fn interrupt(mut self) -> (i32, String, String) {
        let pid = self.child.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-INT", &pid])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + WAIT;
        let code = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status.code().expect("bb exited normally");
            }
            assert!(Instant::now() < deadline, "bb did not stop after Ctrl-C");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        (
            code,
            fs::read_to_string(&self.stdout).unwrap(),
            fs::read_to_string(&self.stderr).unwrap(),
        )
    }
}

async fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_content(env: &Env, bucket: &str, key: &str, expected: &[u8]) {
    let deadline = Instant::now() + WAIT;
    while env.s3.get(bucket, key).await != expected {
        assert!(Instant::now() < deadline, "{key} never became {expected:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_save_is_uploaded_and_ctrl_c_ends_the_session() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "notes/todo.txt", "original").await;
    let scratch = tempfile::tempdir().unwrap();

    let session = start(
        &env,
        scratch.path(),
        &format!("s3://{bucket}/notes/todo.txt"),
        "edited in place",
    );
    session.wait_started().await;
    session.save();
    wait_for_content(&env, &bucket, "notes/todo.txt", b"edited in place").await;
    let (code, stdout, stderr) = session.interrupt().await;

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("editing s3://"), "{stdout}");
    assert!(
        stdout.contains(&format!("uploaded s3://{bucket}/notes/todo.txt")),
        "{stdout}"
    );
    assert!(stdout.contains("stopped editing"), "{stdout}");
    // The copy is removed after a clean stop.
    let copies = env.config.path().join("data").join("edit");
    let left = fs::read_dir(&copies).map(|dir| dir.count()).unwrap_or(0);
    assert_eq!(left, 0, "{copies:?}");
}

#[tokio::test]
async fn a_changed_object_is_never_overwritten_without_a_terminal() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "c.txt", "original").await;
    let scratch = tempfile::tempdir().unwrap();

    let session = start(
        &env,
        scratch.path(),
        &format!("s3://{bucket}/c.txt"),
        "my edit",
    );
    session.wait_started().await;
    env.s3
        .put(&bucket, "c.txt", "changed by someone else")
        .await;
    session.save();
    // The conflict is reported while the session keeps running.
    let stderr_path = session.stderr.clone();
    wait_until("the conflict notice", || {
        fs::read_to_string(&stderr_path).is_ok_and(|text| text.contains("changed on the server"))
    })
    .await;
    let (code, stdout, stderr) = session.interrupt().await;

    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("your edited copy is kept at"), "{stderr}");
    assert!(!stdout.contains("uploaded"), "{stdout}");
    assert_eq!(
        env.s3.get(&bucket, "c.txt").await,
        b"changed by someone else"
    );
    // The unsaved work is still on disk.
    let copies = env.config.path().join("data").join("edit");
    let folder = fs::read_dir(&copies)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let kept = folder.join("c.txt");
    assert!(stderr.contains(&folder.display().to_string()), "{stderr}");
    assert_eq!(fs::read(&kept).unwrap(), b"my edit");
}

#[tokio::test]
async fn the_json_records_name_the_events() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "j.txt", "original").await;
    let scratch = tempfile::tempdir().unwrap();
    let (stdout_path, stderr_path) = (scratch.path().join("out"), scratch.path().join("err"));
    let (started, go) = (scratch.path().join("started"), scratch.path().join("go"));
    let child = env
        .process()
        .args(["--json", "edit", "--app"])
        .arg(editor(scratch.path()))
        .arg(format!("s3://{bucket}/j.txt"))
        .env("STARTED", &started)
        .env("GO", &go)
        .env("PAYLOAD", "json edit")
        .stdin(Stdio::null())
        .stdout(File::create(&stdout_path).unwrap())
        .stderr(File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let session = Session {
        child,
        stdout: stdout_path,
        stderr: stderr_path,
        started,
        go,
    };
    session.wait_started().await;
    session.save();
    wait_for_content(&env, &bucket, "j.txt", b"json edit").await;
    let (code, stdout, stderr) = session.interrupt().await;
    assert_eq!(code, 0, "{stderr}");
    let events: Vec<String> = stdout
        .lines()
        .map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["bucket"], bucket.as_str());
            assert_eq!(record["key"], "j.txt");
            record["event"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(events, ["started", "uploaded", "stopped"]);
}

#[tokio::test]
async fn folders_missing_objects_and_broken_applications_are_rejected() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "a.txt", "x").await;

    let out = env.run(&["edit", &format!("s3://{bucket}/dir/")]);
    out.expect_code(1);
    let out = env.run(&["edit", &format!("s3://{bucket}")]);
    out.expect_code(1);
    let out = env.run(&["edit", &format!("s3://{bucket}/missing.txt")]);
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("bb: error:"), "{}", out.stderr);

    let out = env.run(&[
        "edit",
        "--app",
        "/nonexistent/editor",
        &format!("s3://{bucket}/a.txt"),
    ]);
    out.expect_code(2);
    assert!(out.stderr.contains("/nonexistent/editor"), "{}", out.stderr);
    // The failed start left no copy behind.
    let copies = env.config.path().join("data").join("edit");
    let left = fs::read_dir(&copies).map(|dir| dir.count()).unwrap_or(0);
    assert_eq!(left, 0);
}
