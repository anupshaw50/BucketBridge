//! `bb account` and stored accounts, run in-process with an in-memory
//! secret store: secrets come from stdin, the environment or a prompt, never
//! from arguments, and a master password locks them.

use super::*;

const SECRET: &str = "example-secret-access-key";

/// `account add` arguments for an account on the test server.
fn add_args<'a>(s3: &'a TestS3, label: &'a str) -> Vec<&'a str> {
    vec![
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
    ]
}

/// `args` plus `--secret-stdin`, with the server's secret on stdin.
async fn add_from_stdin(bb: &InProcess, s3: &TestS3, label: &str, vars: &[(&str, &str)]) -> Output {
    let mut args = add_args(s3, label);
    args.push("--secret-stdin");
    let stdin = format!("{}\n", s3.secret_key());
    bb.run_with(&args, vars, stdin.as_bytes(), &Terminal::default())
        .await
}

#[tokio::test]
async fn add_list_default_and_remove() {
    let bb = InProcess::default();
    // The secret and a session token on stdin.
    let out = bb
        .run_with(
            &[
                "account",
                "add",
                "--label",
                "Work",
                "--access-key-id",
                "AKIDWORK",
                "--region",
                "eu-west-1",
                "--secret-stdin",
            ],
            &[],
            format!("{SECRET}\nexample-session-token\n").as_bytes(),
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert!(
        out.stdout.starts_with("added account Work ("),
        "{}",
        out.stdout
    );
    // The secret from the environment, for automation.
    let out = bb
        .run_with(
            &[
                "account",
                "add",
                "--label",
                "home",
                "--access-key-id",
                "AKIDHOME",
                "--endpoint",
                "https://s3.example.com",
                "--path-style",
                "--json",
            ],
            &[("BB_SECRET_ACCESS_KEY", "home-secret")],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    let home = out.records().remove(0);
    assert_eq!(home["label"], "home");
    assert_eq!(home["endpoint"], "https://s3.example.com");
    assert_eq!(home["path_style"], true);
    assert_eq!(home["is_default"], false);
    let home_id = home["id"].as_str().unwrap().to_owned();

    // Secrets are in the store, never in config.toml or the output.
    let stored: Vec<String> = bb
        .store
        .snapshot()
        .into_iter()
        .map(|(_, value)| value.expose().to_owned())
        .collect();
    for secret in [SECRET, "example-session-token", "home-secret"] {
        assert!(stored.iter().any(|s| s == secret), "{secret} not stored");
        assert!(!bb.config_text().contains(secret));
    }
    assert!(!out.stdout.contains("home-secret"));

    // The first account is the default (`*`).
    let out = bb.run(&["account", "list"]).await;
    out.expect_code(0);
    let lines = out.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("* Work "), "{}", lines[0]);
    assert!(lines[0].contains("eu-west-1") && lines[0].contains(" - "));
    assert!(lines[1].starts_with("  home "), "{}", lines[1]);
    assert!(lines[1].ends_with(&home_id));
    assert!(!out.stdout.contains(SECRET));

    let out = bb.run(&["account", "default", "HOME"]).await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!("default account is now home ({home_id})")
    );
    let out = bb.run(&["account", "list", "--json"]).await;
    let defaults: Vec<(String, bool)> = out
        .records()
        .iter()
        .map(|r| {
            (
                r["label"].as_str().unwrap().to_owned(),
                r["is_default"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        defaults,
        [("Work".to_owned(), false), ("home".to_owned(), true)]
    );
    assert!(out.records()[0].get("secret_ref").is_some());

    // Remove by label; its secrets go too.
    let out = bb.run(&["account", "remove", "work", "--json"]).await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["label"], "Work");
    let stored = bb.store.snapshot();
    assert!(stored.iter().all(|(_, value)| value.expose() != SECRET));
    let out = bb.run(&["account", "list"]).await;
    assert_eq!(out.lines().len(), 1);

    // The account is an argument here, not a global option.
    let out = bb
        .run(&["account", "remove", "home", "--account", "home"])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("do not apply"), "{}", out.stderr);
    bb.run(&["--mfa-code", "123456", "account", "list"])
        .await
        .expect_code(1);

    // Unknown accounts and taken labels.
    let out = bb.run(&["account", "remove", "nope", "--json"]).await;
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    let out = bb
        .run_with(
            &["account", "add", "--label", "HOME", "--access-key-id", "AK"],
            &[("BB_SECRET_ACCESS_KEY", "x")],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(2);
    assert!(out.stderr.contains("bb: error: "), "{}", out.stderr);
}

#[tokio::test]
async fn the_secret_comes_from_stdin_the_environment_or_a_prompt_only() {
    let bb = InProcess::default();
    let base = ["account", "add", "--label", "w", "--access-key-id", "AK"];
    // Secrets are not options or positional arguments.
    for extra in [
        &["--secret", SECRET][..],
        &["--secret-access-key", SECRET][..],
        &["--session-token", "token"][..],
        &[SECRET][..],
    ] {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        let out = bb.run(&args).await;
        out.expect_code(1);
        assert!(!out.stderr.is_empty());
    }
    // No stdin, no variable and no terminal: say how to provide it.
    let terminal = Terminal::default();
    let out = bb.run_with(&base, &[], b"", &terminal).await;
    out.expect_code(1);
    assert!(out.stderr.contains("--secret-stdin"), "{}", out.stderr);
    assert_eq!(terminal.prompts(), ["Secret access key: "]);
    // --secret-stdin with nothing on stdin.
    let mut args = base.to_vec();
    args.push("--secret-stdin");
    let out = bb.run_with(&args, &[], b"", &Terminal::default()).await;
    out.expect_code(1);
    assert!(
        out.stderr.contains("standard input is empty"),
        "{}",
        out.stderr
    );
    // The prompt, answered on the terminal without echo.
    let terminal = Terminal::answering(&[SECRET]);
    let out = bb.run_with(&base, &[], b"", &terminal).await;
    out.expect_code(0);
    assert_eq!(terminal.prompts(), ["Secret access key: "]);
    assert!(
        bb.store
            .snapshot()
            .iter()
            .any(|(_, v)| v.expose() == SECRET)
    );
    assert!(
        bb.session().list_accounts().await.unwrap().accounts[0]
            .session_token_ref
            .is_none()
    );
}

#[tokio::test]
async fn stored_accounts_are_used_by_default_by_label_or_by_id() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bb = InProcess::default();
    // No stored account: the environment account, which has no keys here.
    let out = bb.run(&["ls"]).await;
    out.expect_code(3);
    assert!(out.stderr.contains("BB_ACCESS_KEY_ID"), "{}", out.stderr);

    add_from_stdin(&bb, &s3, "test", &[]).await.expect_code(0);
    let out = bb.run(&["ls"]).await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    let id = bb.session().list_accounts().await.unwrap().accounts[0]
        .id
        .clone();
    for selector in ["TEST", id.as_str()] {
        let out = bb.run(&["--account", selector, "ls"]).await;
        out.expect_code(0);
        assert_eq!(out.lines(), [bucket.as_str()]);
    }
    // `environment` still names the account of the BB_* variables.
    let out = bb.run(&["ls", "--account", "environment"]).await;
    out.expect_code(3);
    let out = bb
        .run_with(
            &["ls", "--account", "environment"],
            &[
                ("BB_ACCESS_KEY_ID", s3.access_key()),
                ("BB_SECRET_ACCESS_KEY", s3.secret_key()),
                ("BB_REGION", s3.region()),
                ("BB_ENDPOINT", s3.endpoint()),
                ("BB_PATH_STYLE", "true"),
            ],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    let out = bb.run(&["ls", "--account", "other", "--json"]).await;
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    // An MFA code needs a stored account with an MFA device.
    let out = bb
        .run(&["--account", "environment", "--mfa-code", "123456", "ls"])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("--mfa-code"), "{}", out.stderr);
}

#[tokio::test]
async fn a_master_password_locks_stored_secrets() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bb = InProcess::default();
    bb.session()
        .set_master_password(None, Some("correct horse"))
        .await
        .unwrap();

    // Storing a secret needs the password: none given and no terminal.
    let out = add_from_stdin(&bb, &s3, "test", &[]).await;
    out.expect_code(3);
    let mut args = add_args(&s3, "test");
    args.extend(["--secret-stdin", "--json"]);
    let out = bb
        .run_with(&args, &[], b"secret\n", &Terminal::default())
        .await;
    out.expect_code(3);
    assert_eq!(out.error_record()["code"], "locked");
    // A wrong password is an authentication failure.
    let out = add_from_stdin(&bb, &s3, "test", &[("BB_MASTER_PASSWORD", "wrong")]).await;
    out.expect_code(3);
    assert!(
        bb.session()
            .list_accounts()
            .await
            .unwrap()
            .accounts
            .is_empty()
    );
    // The password from the terminal prompt.
    let terminal = Terminal::answering(&["correct horse"]);
    let mut args = add_args(&s3, "test");
    args.push("--secret-stdin");
    let stdin = format!("{}\n", s3.secret_key());
    let out = bb.run_with(&args, &[], stdin.as_bytes(), &terminal).await;
    out.expect_code(0);
    assert_eq!(terminal.prompts(), ["Master password: "]);
    // Under a master password the stored secret is encrypted.
    assert!(
        bb.store
            .snapshot()
            .iter()
            .all(|(_, value)| !value.expose().contains(s3.secret_key()))
    );

    // Listing accounts needs no secret.
    bb.run(&["account", "list"]).await.expect_code(0);
    // Using the account does: locked without the password.
    let out = bb.run(&["ls", "--json"]).await;
    out.expect_code(3);
    assert_eq!(out.error_record()["code"], "locked");
    let out = bb
        .run_with(
            &["ls"],
            &[("BB_MASTER_PASSWORD", "correct horse")],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    let terminal = Terminal::answering(&["correct horse"]);
    let out = bb.run_with(&["ls"], &[], b"", &terminal).await;
    out.expect_code(0);
    assert_eq!(terminal.prompts(), ["Master password: "]);
}
