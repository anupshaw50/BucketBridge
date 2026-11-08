//! Behaviour shared by every command: help, usage errors, `--account`,
//! `--config-dir`, exit codes, JSON error records, closed pipes and Ctrl-C.

use std::process::Stdio;

use bucketbridge_cli::i18n::{tr, trf};
use super::*;

#[test]
fn help_and_version_exit_with_0() {
    let config = tempfile::tempdir().unwrap();
    let out = run(bb(config.path()).arg("--help"));
    out.expect_code(0);
    assert!(out.stdout.contains(tr("Usage:").as_str()), "{}", out.stdout);
    for command in [
        "ls", "mb", "rb", "rm", "cp", "mv", "sync", "presign", "mpu", "account",
    ] {
        assert!(out.stdout.contains(command), "{command} missing from help");
    }
    assert!(out.stdout.contains("BB_ACCESS_KEY_ID"));
    for args in [
        ["ls", "--help"],
        ["mpu", "--help"],
        ["cp", "--help"],
        ["sync", "--help"],
    ] {
        let out = run(bb(config.path()).args(args));
        out.expect_code(0);
        assert!(out.stdout.contains(tr("Usage:").as_str()), "{args:?}");
    }
    let out = run(bb(config.path()).args(["sync", "--help"]));
    for flag in ["--direction", "--delete", "--dry-run", "--resume"] {
        assert!(out.stdout.contains(flag), "{flag} missing from sync help");
    }
    let out = run(bb(config.path()).args(["mpu", "abort", "--help"]));
    out.expect_code(0);
    assert!(out.stdout.contains("--upload-id"));
    let out = run(bb(config.path()).arg("--version"));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!("bb {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_with_1_before_any_request() {
    let config = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["ls", "not-a-uri"],
        vec!["ls", "s3://bad bucket/"],
        vec!["ls", "--no-such-flag"],
        vec!["ls", "--recursive"],
        vec!["rm"],
        vec!["cp", "s3://b/k"],
        vec!["presign", "s3://b/k", "--expires", "1.5h"],
        vec!["mpu", "abort", "--bucket", "b"],
        vec!["mb", "s3://b/key"],
    ] {
        let out = run(bb(config.path()).args(&args));
        out.expect_code(1);
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!out.stderr.is_empty(), "{args:?}");
    }
    // Checked before credentials are needed.
    for (args, expected) in [
        (
            &["cp", "./a.txt", "./b.txt"][..],
            "both locations are local",
        ),
        (&["cp", "s3://b/p/", "./out"][..], "--recursive"),
        (&["mv", "-r", "s3://b/k", "./out"][..], "--recursive"),
    ] {
        let out = run(bb(config.path()).args(args));
        out.expect_code(1);
        assert!(
            out.stderr.contains(&tr(expected)),
            "{args:?}: {}",
            out.stderr
        );
    }

    let with_credentials = |var: &str, value: &str| {
        run(bb(config.path())
            .env("BB_ACCESS_KEY_ID", "AK")
            .env("BB_SECRET_ACCESS_KEY", "SK")
            .env(var, value)
            .arg("ls"))
    };
    let out = with_credentials("BB_PATH_STYLE", "maybe");
    out.expect_code(1);
    assert!(out.stderr.contains("BB_PATH_STYLE"), "{}", out.stderr);
    with_credentials("BB_ENDPOINT", "ftp://example.com").expect_code(1);
    with_credentials("BB_REGION", "eu west").expect_code(1);
}

#[tokio::test]
async fn account_flag_names_the_environment_account() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let out = env.run(&["ls", "--account", "environment"]);
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    env.run(&["--account", "ENVIRONMENT", "ls"]).expect_code(0);

    let out = env.run(&["ls", "--account", "work"]);
    out.expect_code(2);
    assert!(
        out.stderr.contains(&trf(
            "account '{selector}' not found",
            &[("selector", "work")],
        )),
        "{}",
        out.stderr
    );
    let out = env.run(&["ls", "--account", "work", "--json"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
}

#[tokio::test]
async fn authentication_failures_exit_with_3() {
    let env = Env::start().await;
    let out = run(env.bb().env("BB_SECRET_ACCESS_KEY", "wrong").arg("ls"));
    out.expect_code(3);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.starts_with("bb: error: "), "{}", out.stderr);

    let out = run(env
        .bb()
        .env("BB_SECRET_ACCESS_KEY", "wrong")
        .args(["ls", "--json"]));
    out.expect_code(3);
    let record = out.error_record();
    assert_eq!(record["code"], "auth");
    assert!(record["message"].as_str().is_some_and(|m| !m.is_empty()));

    let out = run(bb(env.config.path())
        .env("BB_ENDPOINT", env.s3.endpoint())
        .arg("ls"));
    out.expect_code(3);
    assert!(out.stderr.contains("BB_ACCESS_KEY_ID"), "{}", out.stderr);
}

#[tokio::test]
async fn remote_failures_exit_with_2() {
    let env = Env::start().await;
    let out = env.run(&["ls", "s3://no-such-bucket/"]);
    out.expect_code(2);
    assert!(out.stderr.contains("no-such-bucket"), "{}", out.stderr);

    // An endpoint nobody listens on, with one attempt so the test is fast.
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    std::fs::write(
        env.config.path().join("config.toml"),
        "[settings.advanced]\nmax_attempts = 1\nconnect_timeout_secs = 2\n",
    )
    .unwrap();
    let out = run(env
        .bb()
        .env("BB_ENDPOINT", format!("http://127.0.0.1:{port}"))
        .args(["ls", "--json"]));
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "remote");
}

#[test]
fn corrupt_config_is_reported_and_left_alone() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("config.toml"), "not = = toml").unwrap();
    let out = run(bb(config.path())
        .env("BB_ACCESS_KEY_ID", "AK")
        .env("BB_SECRET_ACCESS_KEY", "SK")
        .arg("ls"));
    out.expect_code(2);
    assert!(out.stderr.contains("config.toml"), "{}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(config.path().join("config.toml")).unwrap(),
        "not = = toml"
    );
}

#[tokio::test]
async fn closed_stdout_is_not_an_error() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "a.txt", "x").await;
    let mut child = bb_process(env.config.path())
        .env("BB_ACCESS_KEY_ID", env.s3.access_key())
        .env("BB_SECRET_ACCESS_KEY", env.s3.secret_key())
        .env("BB_ENDPOINT", env.s3.endpoint())
        .env("BB_PATH_STYLE", "1")
        .args(["ls", "--json", &format!("s3://{bucket}/")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the read end before bb writes anything.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_cancels_and_exits_with_130() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use bucketbridge_tests::MockResponse;

    // The server holds the request until the test releases it.
    let entered = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let seen = entered.clone();
    let mock = MockS3::start(move |_| {
        seen.store(true, Ordering::SeqCst);
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20));
        MockResponse::xml(xml::buckets(&[]))
    });
    let config = tempfile::tempdir().unwrap();
    let mut cmd = bb_process(config.path());
    mock_env(&mut cmd, &mock);
    let mut child = cmd
        .arg("ls")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    while !entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "bb never reached the server");
        tokio::time::sleep(Duration::from_millis(10)).await;
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
            started.elapsed() < Duration::from_secs(10),
            "bb did not stop after Ctrl-C"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    release_tx.send(()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(exit.code(), Some(130));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cancelled"), "{stderr}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "bb took {:?} to stop",
        started.elapsed()
    );
}
