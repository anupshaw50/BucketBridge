//! Client-side encryption in `bb cp`, `bb mv`, `bb encrypt` and `bb decrypt`.

use std::fs;

use bucketbridge_cli::i18n::tr;

use super::*;

const PASSWORD: &str = "correct horse battery staple";

fn run_with(env: &Env, password: &str, args: &[&str]) -> Output {
    run(env.bb().env("BB_CSE_PASSWORD", password).args(args))
}

fn is_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BBCSE")
}

/// A bucket and a local folder holding `plain.txt`.
async fn setup(env: &Env, body: &[u8]) -> (String, tempfile::TempDir) {
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("plain.txt"), body).unwrap();
    (bucket, dir)
}

#[tokio::test]
async fn encrypted_upload_and_decrypting_download_round_trip() {
    let env = Env::start().await;
    let body = b"the quick brown fox ".repeat(1000);
    let (bucket, dir) = setup(&env, &body).await;
    let src = dir.path().join("plain.txt");
    let uri = format!("s3://{bucket}/secret.txt");

    run_with(
        &env,
        PASSWORD,
        &["cp", "--encrypt", src.to_str().unwrap(), &uri],
    )
    .expect_code(0);
    let stored = env.s3.get(&bucket, "secret.txt").await;
    assert!(is_envelope(&stored));
    assert!(!stored.windows(9).any(|w| w == b"the quick"));

    // Without --decrypt the ciphertext is downloaded as it is.
    let raw = dir.path().join("raw.bin");
    run_with(&env, PASSWORD, &["cp", &uri, raw.to_str().unwrap()]).expect_code(0);
    assert_eq!(fs::read(&raw).unwrap(), stored);

    let back = dir.path().join("back.txt");
    run_with(
        &env,
        PASSWORD,
        &["cp", "--decrypt", &uri, back.to_str().unwrap()],
    )
    .expect_code(0);
    assert_eq!(fs::read(&back).unwrap(), body);
}

#[tokio::test]
async fn compressed_upload_round_trips_and_is_smaller() {
    let env = Env::start().await;
    let body = b"abcabcabc".repeat(20_000);
    let (bucket, dir) = setup(&env, &body).await;
    let src = dir.path().join("plain.txt");
    for algorithm in ["gzip", "zstd"] {
        let uri = format!("s3://{bucket}/{algorithm}.bin");
        run_with(
            &env,
            PASSWORD,
            &[
                "cp",
                "--encrypt",
                "--compress",
                algorithm,
                src.to_str().unwrap(),
                &uri,
            ],
        )
        .expect_code(0);
        let stored = env.s3.get(&bucket, &format!("{algorithm}.bin")).await;
        assert!(is_envelope(&stored));
        assert!(
            stored.len() < body.len() / 4,
            "{algorithm}: {}",
            stored.len()
        );
        let back = dir.path().join(format!("{algorithm}.out"));
        run_with(
            &env,
            PASSWORD,
            &["cp", "--decrypt", &uri, back.to_str().unwrap()],
        )
        .expect_code(0);
        assert_eq!(fs::read(&back).unwrap(), body);
    }
}

#[tokio::test]
async fn wrong_password_is_an_auth_error_and_leaves_no_file() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, b"hello").await;
    let src = dir.path().join("plain.txt");
    let uri = format!("s3://{bucket}/secret.txt");
    run_with(
        &env,
        PASSWORD,
        &["cp", "--encrypt", src.to_str().unwrap(), &uri],
    )
    .expect_code(0);

    let out_path = dir.path().join("out.txt");
    let out = run_with(
        &env,
        "not the password",
        &["cp", "--decrypt", &uri, out_path.to_str().unwrap()],
    );
    out.expect_code(3);
    assert!(out.stderr.contains("password not valid"), "{}", out.stderr);
    assert!(!out_path.exists());

    let json = run(env.bb().env("BB_CSE_PASSWORD", "nope").args([
        "--json",
        "cp",
        "--decrypt",
        &uri,
        out_path.to_str().unwrap(),
    ]));
    json.expect_code(3);
    assert_eq!(json.error_record()["code"], "auth");
}

#[tokio::test]
async fn tampered_data_is_reported_and_not_kept() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, &b"payload ".repeat(5000)).await;
    let src = dir.path().join("plain.txt");
    let uri = format!("s3://{bucket}/secret.txt");
    run_with(
        &env,
        PASSWORD,
        &["cp", "--encrypt", src.to_str().unwrap(), &uri],
    )
    .expect_code(0);
    let mut stored = env.s3.get(&bucket, "secret.txt").await;
    let last = stored.len() - 1;
    stored[last] ^= 0xff;
    env.s3.put(&bucket, "secret.txt", stored).await;

    let out_path = dir.path().join("out.txt");
    let out = run_with(
        &env,
        PASSWORD,
        &["cp", "--decrypt", &uri, out_path.to_str().unwrap()],
    );
    out.expect_code(2);
    assert!(out.stderr.contains("integrity check"), "{}", out.stderr);
    assert!(!out_path.exists());
}

#[tokio::test]
async fn an_encrypted_file_is_not_encrypted_twice() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, b"hello").await;
    let src = dir.path().join("plain.txt");
    let first = format!("s3://{bucket}/one.bin");
    run_with(
        &env,
        PASSWORD,
        &["cp", "--encrypt", src.to_str().unwrap(), &first],
    )
    .expect_code(0);
    let ciphertext = dir.path().join("cipher.bin");
    fs::write(&ciphertext, env.s3.get(&bucket, "one.bin").await).unwrap();

    let second = format!("s3://{bucket}/two.bin");
    let out = run_with(
        &env,
        PASSWORD,
        &["cp", "--encrypt", ciphertext.to_str().unwrap(), &second],
    );
    out.expect_code(2);
    assert!(
        out.stderr.contains(&tr("already client-side encrypted")),
        "{}",
        out.stderr
    );
    assert_eq!(env.keys(&bucket).await, ["one.bin"]);

    // bb encrypt refuses an encrypted object as well.
    let out = run_with(&env, PASSWORD, &["encrypt", &first]);
    out.expect_code(2);
    assert!(out.stderr.contains("already encrypted"), "{}", out.stderr);
}

#[tokio::test]
async fn password_file_is_used_and_flags_are_checked() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, b"hello").await;
    let src = dir.path().join("plain.txt");
    let uri = format!("s3://{bucket}/secret.txt");
    let file = dir.path().join("pw");
    fs::write(&file, format!("{PASSWORD}\n")).unwrap();
    env.run(&[
        "cp",
        "--encrypt",
        "--password-file",
        file.to_str().unwrap(),
        src.to_str().unwrap(),
        &uri,
    ])
    .expect_code(0);
    // The environment variable gives the same password.
    let back = dir.path().join("back.txt");
    run_with(
        &env,
        PASSWORD,
        &["cp", "--decrypt", &uri, back.to_str().unwrap()],
    )
    .expect_code(0);
    assert_eq!(fs::read(&back).unwrap(), b"hello");

    // No password anywhere and no terminal: a usage error before any transfer.
    let out = env.run(&[
        "cp",
        "--encrypt",
        src.to_str().unwrap(),
        &format!("s3://{bucket}/x"),
    ]);
    out.expect_code(1);
    assert!(out.stderr.contains("BB_CSE_PASSWORD"), "{}", out.stderr);
    assert!(!env.keys(&bucket).await.contains(&"x".to_owned()));

    for args in [
        vec!["cp", "--encrypt", "--decrypt", "a", "s3://b/k"],
        vec!["cp", "--compress", "zstd", "a", "s3://b/k"],
        vec!["cp", "--password-file", "pw", "a", "s3://b/k"],
        vec!["cp", "--decrypt", "a", "s3://b/k"],
        vec!["cp", "--encrypt", "s3://b/k", "a"],
        vec!["cp", "--encrypt", "s3://b/k", "s3://c/k"],
        vec!["cp", "--encrypt", "--compress", "lz4", "a", "s3://b/k"],
    ] {
        run_with(&env, PASSWORD, &args).expect_code(1);
    }
}

#[tokio::test]
async fn decrypting_download_saves_plain_objects_as_they_are() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3
        .put(&bucket, "plain.txt", b"just text".to_vec())
        .await;
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("plain.txt");
    run_with(
        &env,
        PASSWORD,
        &[
            "cp",
            "--decrypt",
            &format!("s3://{bucket}/plain.txt"),
            out_path.to_str().unwrap(),
        ],
    )
    .expect_code(0);
    assert_eq!(fs::read(&out_path).unwrap(), b"just text");
}

#[tokio::test]
async fn recursive_encrypted_upload_and_decrypting_download() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("docs");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::write(tree.join("a.txt"), "alpha").unwrap();
    fs::write(tree.join("sub/b.txt"), "beta").unwrap();
    let dst = format!("s3://{bucket}/backup/");
    run_with(
        &env,
        PASSWORD,
        &["cp", "-r", "--encrypt", tree.to_str().unwrap(), &dst],
    )
    .expect_code(0);
    for key in ["backup/docs/a.txt", "backup/docs/sub/b.txt"] {
        assert!(is_envelope(&env.s3.get(&bucket, key).await), "{key}");
    }
    let out = dir.path().join("restored");
    fs::create_dir_all(&out).unwrap();
    run_with(
        &env,
        PASSWORD,
        &[
            "cp",
            "-r",
            "--decrypt",
            &format!("{dst}docs/"),
            out.to_str().unwrap(),
        ],
    )
    .expect_code(0);
    assert_eq!(fs::read(out.join("docs/a.txt")).unwrap(), b"alpha");
    assert_eq!(fs::read(out.join("docs/sub/b.txt")).unwrap(), b"beta");
}

#[tokio::test]
async fn mv_encrypts_and_removes_the_local_file() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, b"moving").await;
    let src = dir.path().join("plain.txt");
    run_with(
        &env,
        PASSWORD,
        &[
            "mv",
            "--encrypt",
            src.to_str().unwrap(),
            &format!("s3://{bucket}/m.bin"),
        ],
    )
    .expect_code(0);
    assert!(!src.exists());
    assert!(is_envelope(&env.s3.get(&bucket, "m.bin").await));
}

#[tokio::test]
async fn encrypt_and_decrypt_rewrite_objects_in_place() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "docs/a.txt", b"alpha".to_vec()).await;
    env.s3.put(&bucket, "docs/b.txt", b"beta".to_vec()).await;
    let a = format!("s3://{bucket}/docs/a.txt");

    let out = run_with(&env, PASSWORD, &["--json", "encrypt", &a]);
    out.expect_code(0);
    assert_eq!(out.records()[0]["action"], "encrypt");
    assert_eq!(out.records()[0]["size_before"], 5);
    assert!(is_envelope(&env.s3.get(&bucket, "docs/a.txt").await));

    // A prefix needs --recursive; objects that are encrypted already are skipped.
    run_with(
        &env,
        PASSWORD,
        &["encrypt", &format!("s3://{bucket}/docs/")],
    )
    .expect_code(1);
    let out = run_with(
        &env,
        PASSWORD,
        &[
            "encrypt",
            "-r",
            "--compress",
            "gzip",
            &format!("s3://{bucket}/docs/"),
        ],
    );
    out.expect_code(0);
    assert!(out.stdout.contains(&tr("skipped s3://")), "{}", out.stdout);
    assert!(is_envelope(&env.s3.get(&bucket, "docs/b.txt").await));

    let out = run_with(&env, "wrong", &["decrypt", &a]);
    out.expect_code(3);
    assert!(out.stderr.contains("password not valid"));
    assert!(is_envelope(&env.s3.get(&bucket, "docs/a.txt").await));

    run_with(
        &env,
        PASSWORD,
        &["decrypt", "-r", &format!("s3://{bucket}/docs/")],
    )
    .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "docs/a.txt").await, b"alpha");
    assert_eq!(env.s3.get(&bucket, "docs/b.txt").await, b"beta");

    // Decrypting a plain object is a usage error.
    run_with(&env, PASSWORD, &["decrypt", &a]).expect_code(1);
}

#[tokio::test]
async fn the_password_can_be_typed_on_the_terminal() {
    let env = Env::start().await;
    let (bucket, dir) = setup(&env, b"typed").await;
    let src = dir.path().join("plain.txt");
    let proc = InProcess::default();
    let terminal = Terminal::answering(&[PASSWORD, "different"]);
    let args = [
        "cp",
        "--encrypt",
        src.to_str().unwrap(),
        &format!("s3://{bucket}/t.bin"),
    ];
    let vars = [
        ("BB_ACCESS_KEY_ID", env.s3.access_key()),
        ("BB_SECRET_ACCESS_KEY", env.s3.secret_key()),
        ("BB_REGION", env.s3.region()),
        ("BB_ENDPOINT", env.s3.endpoint()),
        ("BB_PATH_STYLE", "true"),
    ];
    let out = proc.run_with(&args, &vars, b"", &terminal).await;
    out.expect_code(1);
    assert!(out.stderr.contains(&tr("differ")), "{}", out.stderr);
    assert_eq!(terminal.prompts().len(), 2);

    let terminal = Terminal::answering(&[PASSWORD, PASSWORD]);
    proc.run_with(&args, &vars, b"", &terminal)
        .await
        .expect_code(0);
    assert!(is_envelope(&env.s3.get(&bucket, "t.bin").await));
}
