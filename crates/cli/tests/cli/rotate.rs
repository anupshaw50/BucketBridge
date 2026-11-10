//! `bb account update-credentials`: rotating the keys of a stored account.
//! In-process with an in-memory secret store, so the old and the new secret
//! can be looked for in the store.

use bucketbridge_cli::i18n::{tr, trf};

use super::*;

const OLD_KEY: &str = "AKIAOLDKEY";
const OLD_SECRET: &str = "old-secret-access-key";

/// An account `rot` on the test server with keys the server does not know.
async fn old_account(bb: &InProcess, s3: &TestS3, extra_stdin: &str) -> Output {
    let stdin = format!("{OLD_SECRET}\n{extra_stdin}");
    bb.run_with(
        &[
            "account",
            "add",
            "--label",
            "rot",
            "--access-key-id",
            OLD_KEY,
            "--region",
            s3.region(),
            "--endpoint",
            s3.endpoint(),
            "--path-style",
            "--secret-stdin",
        ],
        &[],
        stdin.as_bytes(),
        &Terminal::default(),
    )
    .await
}

fn stored(bb: &InProcess, secret: &str) -> bool {
    bb.store
        .snapshot()
        .iter()
        .any(|(_, value)| value.expose() == secret)
}

async fn account(bb: &InProcess) -> bucketbridge_core::model::Account {
    bb.session().list_accounts().await.unwrap().accounts[0].clone()
}

#[tokio::test]
async fn new_keys_from_stdin_replace_the_old_ones_and_are_verified() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let bb = InProcess::default();
    old_account(&bb, &s3, "").await.expect_code(0);
    // The old keys are unknown to the server.
    bb.run(&["--account", "rot", "ls"]).await.expect_code(3);

    let stdin = format!("{}\n", s3.secret_key());
    let out = bb
        .run_with(
            &[
                "--json",
                "account",
                "update-credentials",
                "rot",
                "--access-key-id",
                s3.access_key(),
                "--secret-stdin",
                "--verify",
            ],
            &[],
            stdin.as_bytes(),
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["label"], "rot");
    assert_eq!(records[0]["access_key_id"], s3.access_key());
    assert_eq!(records[0]["verified"], true);
    // No secret is ever printed.
    assert!(!out.stdout.contains(s3.secret_key()) && !out.stderr.contains(s3.secret_key()));

    // The old secret is gone from the store, the new one is in.
    assert!(!stored(&bb, OLD_SECRET));
    assert!(stored(&bb, s3.secret_key()));
    assert_eq!(account(&bb).await.access_key_id, s3.access_key());
    // Nothing secret reaches config.toml.
    assert!(!bb.config_text().contains(s3.secret_key()));
    // The account works now.
    let out = bb.run(&["--account", "rot", "ls"]).await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);

    // The human line says it was verified.
    let out = bb
        .run_with(
            &[
                "account",
                "update-credentials",
                "rot",
                "--access-key-id",
                s3.access_key(),
                "--secret-stdin",
                "--verify",
            ],
            &[],
            stdin.as_bytes(),
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "replaced the credentials of account {label} ({id}) with access key {key} (verified)",
            &[
                ("label", "rot"),
                ("id", account(&bb).await.id.as_str()),
                ("key", s3.access_key()),
            ],
        )
    );
}

#[tokio::test]
async fn a_failed_verification_saves_nothing() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    old_account(&bb, &s3, "").await.expect_code(0);
    let before = bb.store.snapshot();
    let config_before = bb.config_text();

    // The right access key ID with a wrong secret: the signature fails.
    let out = bb
        .run_with(
            &[
                "--json",
                "account",
                "update-credentials",
                "rot",
                "--access-key-id",
                s3.access_key(),
                "--secret-stdin",
                "--verify",
            ],
            &[],
            b"not-the-secret\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(3);
    assert_eq!(out.error_record()["code"], "auth");
    assert!(
        out.stderr.contains(&tr(
            "the new keys were not saved: the server did not accept them"
        )),
        "{}",
        out.stderr
    );
    assert!(out.stdout.is_empty());
    // The store and the configuration are exactly as before.
    let after = bb.store.snapshot();
    assert_eq!(before.len(), after.len());
    assert!(
        before
            .iter()
            .zip(&after)
            .all(|(a, b)| a.0 == b.0 && a.1.expose() == b.1.expose())
    );
    assert_eq!(bb.config_text(), config_before);
    assert!(stored(&bb, OLD_SECRET));
    assert!(!stored(&bb, "not-the-secret"));
    assert_eq!(account(&bb).await.access_key_id, OLD_KEY);

    // Without --verify the same keys are saved without a request.
    let out = bb
        .run_with(
            &[
                "--json",
                "account",
                "update-credentials",
                "rot",
                "--access-key-id",
                s3.access_key(),
                "--secret-stdin",
            ],
            &[],
            b"not-the-secret\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["verified"], false);
    assert!(stored(&bb, "not-the-secret") && !stored(&bb, OLD_SECRET));
}

#[tokio::test]
async fn an_unreachable_server_fails_verification_with_exit_2() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    // A port nobody listens on.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", free.local_addr().unwrap());
    drop(free);
    bb.run_with(
        &[
            "account",
            "add",
            "--label",
            "down",
            "--access-key-id",
            OLD_KEY,
            "--region",
            s3.region(),
            "--endpoint",
            &endpoint,
            "--path-style",
            "--secret-stdin",
        ],
        &[],
        b"old\n",
        &Terminal::default(),
    )
    .await
    .expect_code(0);
    let out = bb
        .run_with(
            &[
                "account",
                "update-credentials",
                "down",
                "--access-key-id",
                "AKNEW",
                "--secret-stdin",
                "--verify",
            ],
            &[],
            b"new\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(2);
    assert!(stored(&bb, "old") && !stored(&bb, "new"));
}

#[tokio::test]
async fn the_environment_and_the_terminal_supply_the_keys() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    old_account(&bb, &s3, "").await.expect_code(0);

    // BB_ACCESS_KEY_ID and BB_SECRET_ACCESS_KEY.
    let terminal = Terminal::default();
    let out = bb
        .run_with(
            &["account", "update-credentials", "rot"],
            &[
                ("BB_ACCESS_KEY_ID", "AKFROMENV"),
                ("BB_SECRET_ACCESS_KEY", "secret-from-env"),
            ],
            b"",
            &terminal,
        )
        .await;
    out.expect_code(0);
    assert!(terminal.prompts().is_empty());
    assert_eq!(account(&bb).await.access_key_id, "AKFROMENV");
    assert!(stored(&bb, "secret-from-env") && !stored(&bb, OLD_SECRET));

    // Both asked on the terminal, in that order.
    let terminal = Terminal::answering(&["AKPROMPT", "secret-from-prompt"]);
    bb.run_with(
        &["account", "update-credentials", "rot"],
        &[],
        b"",
        &terminal,
    )
    .await
    .expect_code(0);
    assert_eq!(
        terminal.prompts(),
        [tr("Access key ID: "), tr("Secret access key: ")]
    );
    assert_eq!(account(&bb).await.access_key_id, "AKPROMPT");
    assert!(stored(&bb, "secret-from-prompt") && !stored(&bb, "secret-from-env"));

    // Nothing to read it from: a usage error that says how.
    let out = bb
        .run(&[
            "account",
            "update-credentials",
            "rot",
            "--access-key-id",
            "X",
        ])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("--secret-stdin"), "{}", out.stderr);
    let out = bb.run(&["account", "update-credentials", "rot"]).await;
    out.expect_code(1);
    assert!(out.stderr.contains("--access-key-id"), "{}", out.stderr);
    // The secret is not an option or a positional argument.
    for extra in [&["--secret", "x"][..], &["--secret-access-key", "x"][..]] {
        let mut args = vec!["account", "update-credentials", "rot"];
        args.extend_from_slice(extra);
        bb.run(&args).await.expect_code(1);
    }
    assert!(stored(&bb, "secret-from-prompt"));
}

#[tokio::test]
async fn the_old_session_token_goes_with_the_old_keys() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    old_account(&bb, &s3, "old-token\n").await.expect_code(0);
    assert!(account(&bb).await.session_token_ref.is_some());
    assert!(stored(&bb, "old-token"));

    // New long-term keys: the token of the old keys is removed.
    bb.run_with(
        &[
            "account",
            "update-credentials",
            "rot",
            "--access-key-id",
            "AKNEW",
            "--secret-stdin",
        ],
        &[],
        b"new-secret\n",
        &Terminal::default(),
    )
    .await
    .expect_code(0);
    assert!(account(&bb).await.session_token_ref.is_none());
    assert!(!stored(&bb, "old-token"));

    // A new token on the second line is stored.
    bb.run_with(
        &[
            "account",
            "update-credentials",
            "rot",
            "--access-key-id",
            "AKNEW2",
            "--secret-stdin",
        ],
        &[],
        b"newer-secret\nnew-token\n",
        &Terminal::default(),
    )
    .await
    .expect_code(0);
    assert!(stored(&bb, "new-token") && stored(&bb, "newer-secret"));
    assert!(!stored(&bb, "new-secret"));
}

#[tokio::test]
async fn under_a_master_password_the_vault_is_updated_and_stays_encrypted() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    old_account(&bb, &s3, "").await.expect_code(0);
    bb.session()
        .set_master_password(None, Some("correct horse"))
        .await
        .unwrap();
    let args = [
        "account",
        "update-credentials",
        "rot",
        "--access-key-id",
        "AKVAULT",
        "--secret-stdin",
    ];

    // Locked and nobody to ask: nothing changes.
    let out = bb
        .run_with(&args, &[], b"vault-secret\n", &Terminal::default())
        .await;
    out.expect_code(3);
    assert_eq!(account(&bb).await.access_key_id, OLD_KEY);
    // A wrong password.
    let out = bb
        .run_with(
            &args,
            &[("BB_MASTER_PASSWORD", "wrong")],
            b"vault-secret\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(3);
    assert_eq!(account(&bb).await.access_key_id, OLD_KEY);

    // The right password, with the keys the server knows: rotated, verified,
    // and no entry holds a plain text secret.
    let good = [
        "account",
        "update-credentials",
        "rot",
        "--access-key-id",
        s3.access_key(),
        "--secret-stdin",
        "--verify",
    ];
    let stdin = format!("{}\n", s3.secret_key());
    let out = bb
        .run_with(
            &good,
            &[("BB_MASTER_PASSWORD", "correct horse")],
            stdin.as_bytes(),
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(account(&bb).await.access_key_id, s3.access_key());
    assert!(bb.store.snapshot().iter().all(|(_, v)| {
        !v.expose().contains(s3.secret_key()) && !v.expose().contains(OLD_SECRET)
    }));
    // The vault holds the new secret: the account works with the password.
    let out = bb
        .run_with(
            &["--account", "rot", "ls"],
            &[("BB_MASTER_PASSWORD", "correct horse")],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    // And is locked without it.
    bb.run(&["--account", "rot", "ls"]).await.expect_code(3);
}

#[tokio::test]
async fn only_accounts_with_keys_can_be_rotated() {
    let bb = InProcess::default();
    bb.run(&[
        "account",
        "add",
        "--label",
        "role",
        "--instance-role",
        "--no-probe",
    ])
    .await
    .expect_code(0);
    let out = bb
        .run_with(
            &[
                "account",
                "update-credentials",
                "role",
                "--access-key-id",
                "AK",
                "--secret-stdin",
            ],
            &[],
            b"s\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(1);
    let out = bb
        .run_with(
            &[
                "account",
                "update-credentials",
                "nobody",
                "--access-key-id",
                "AK",
                "--secret-stdin",
            ],
            &[],
            b"s\n",
            &Terminal::default(),
        )
        .await;
    out.expect_code(2);
}
