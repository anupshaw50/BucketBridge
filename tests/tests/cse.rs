//! Client-side encryption (task T07) against a real server: the encrypting
//! upload and decrypting download as queue tasks, in-place rewrites of
//! objects, and the password states.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bucketbridge_core::cse::{
    self, Action, Compression, CsePasswords, EncryptOptions, Problem, TransformRequest,
};
use bucketbridge_core::engine::{
    Hooks, S3Provider, S3Runner, TransferEngine, TransferOptions, enqueue_download,
    enqueue_download_decrypted, enqueue_upload, enqueue_upload_encrypted,
};
use bucketbridge_core::model::{TransferState, TransferTask};
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::settings::{DataIntegritySettings, TransferSettings};
use bucketbridge_core::transfer::{QueueOptions, TransferQueue};
use bucketbridge_core::{Error, ErrorKind, S3};
use bucketbridge_tests::TestS3;

const ACCOUNT: &str = "account-1";
const PASSWORD: &str = "a password for the tests";
const WAIT: Duration = Duration::from_secs(120);
const MIB: usize = 1024 * 1024;

fn payload(len: usize, seed: u32) -> Vec<u8> {
    let mut state: u32 = seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn secret(text: &str) -> SecretString {
    SecretString::from(text)
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, data).unwrap();
    path
}

fn provider(s3: &S3) -> S3Provider {
    let s3 = s3.clone();
    Arc::new(move |_| {
        let s3 = s3.clone();
        Box::pin(async move { Ok(s3) })
    })
}

fn settings() -> TransferSettings {
    TransferSettings {
        max_concurrent_tasks: 4,
        max_parts_per_task: 1,
        multipart_threshold_mib: 5,
        part_size_mib: 5,
        bandwidth_limit_kibps: None,
        retry_attempts: 0,
        ..TransferSettings::default()
    }
}

async fn queue_with(s3: &S3, passwords: &CsePasswords) -> TransferQueue {
    let runner = S3Runner::new(provider(s3), DataIntegritySettings::default())
        .with_cse_passwords(passwords.clone());
    TransferQueue::open(settings(), runner.runners(), QueueOptions::default())
        .await
        .unwrap()
}

async fn settle(queue: &TransferQueue, tasks: &[TransferTask]) -> Vec<TransferTask> {
    let deadline = Instant::now() + WAIT;
    loop {
        let now: Vec<TransferTask> = tasks.iter().map(|t| queue.get(&t.id).unwrap()).collect();
        if now
            .iter()
            .all(|t| matches!(t.state, TransferState::Completed | TransferState::Failed))
        {
            return now;
        }
        assert!(Instant::now() < deadline, "tasks did not settle: {now:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn all_completed(queue: &TransferQueue, tasks: &[TransferTask]) {
    for task in settle(queue, tasks).await {
        assert_eq!(task.state, TransferState::Completed, "{:?}", task.error);
    }
}

fn engine(s3: &TestS3) -> TransferEngine {
    TransferEngine::new(s3.ops().clone(), TransferOptions::default()).unwrap()
}

#[tokio::test]
async fn encrypting_upload_stores_ciphertext_and_decrypting_download_restores_every_size() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let chunk = MIB;
    let sizes = [0, 1, chunk - 1, chunk, chunk + 1, 2 * chunk + 17, 3 * chunk];
    for compression in [Compression::None, Compression::Gzip, Compression::Zstd] {
        let source = tempfile::tempdir().unwrap();
        let mut originals = HashMap::new();
        for (n, size) in sizes.iter().enumerate() {
            let name = format!("f{n}.bin");
            let data = payload(*size, u32::try_from(n).unwrap());
            write_file(source.path(), &name, &data);
            originals.insert(name, data);
        }
        let prefix = format!("{compression}/");
        let tasks = enqueue_upload_encrypted(
            &queue,
            &passwords,
            secret(PASSWORD),
            compression,
            ACCOUNT,
            &[source.path().join("f0.bin"), source.path().join("f1.bin")],
            &bucket,
            &prefix,
        )
        .await
        .unwrap();
        all_completed(&queue, &tasks).await;
        // The folder form: every file of the directory.
        let more = enqueue_upload_encrypted(
            &queue,
            &passwords,
            secret(PASSWORD),
            compression,
            ACCOUNT,
            &originals
                .keys()
                .map(|name| source.path().join(name))
                .collect::<Vec<_>>(),
            &bucket,
            &prefix,
        )
        .await
        .unwrap();
        all_completed(&queue, &more).await;

        for (name, data) in &originals {
            let key = format!("{prefix}{name}");
            let stored = s3.get(&bucket, &key).await;
            assert_eq!(&stored[..5], b"BBCSE", "{key}");
            assert!(
                !stored
                    .windows(32)
                    .any(|w| !data.is_empty() && data.len() >= 32 && w == &data[..32]),
                "{key}: plaintext leaked"
            );
            let probe = cse::probe_object(s3.ops(), &bucket, &key).await.unwrap();
            assert!(probe.encrypted && probe.marked, "{key}");
            assert_eq!(probe.compression, Some(compression));
        }
        let head = s3
            .ops()
            .head_object(&bucket, &format!("{prefix}f3.bin"), None)
            .await
            .unwrap();
        assert_eq!(
            head.meta.user_meta.get("bb-cse").map(String::as_str),
            Some(format!("bbcse1;aes-256-gcm;{compression}").as_str())
        );

        let dest = tempfile::tempdir().unwrap();
        let keys: Vec<String> = originals.keys().map(|n| format!("{prefix}{n}")).collect();
        let down = enqueue_download_decrypted(
            &queue,
            &passwords,
            Some(secret(PASSWORD)),
            s3.ops(),
            ACCOUNT,
            &bucket,
            &keys,
            dest.path(),
        )
        .await
        .unwrap();
        all_completed(&queue, &down).await;
        for (name, data) in &originals {
            assert_eq!(
                &fs::read(dest.path().join(name)).unwrap(),
                data,
                "{compression} {name}"
            );
        }
        assert!(fs::read_dir(dest.path()).unwrap().all(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("bb-decrypting") && !name.ends_with(".partial")
        }));
    }
}

#[tokio::test]
async fn a_large_encrypted_file_uploads_in_parts() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let data = payload(6 * MIB + 5, 9);
    let path = write_file(source.path(), "big.bin", &data);
    let tasks = enqueue_upload_encrypted(
        &queue,
        &passwords,
        secret(PASSWORD),
        Compression::Zstd,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap();
    all_completed(&queue, &tasks).await;
    let dest = tempfile::tempdir().unwrap();
    let down = enqueue_download_decrypted(
        &queue,
        &passwords,
        Some(secret(PASSWORD)),
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["big.bin".to_owned()],
        dest.path(),
    )
    .await
    .unwrap();
    all_completed(&queue, &down).await;
    assert_eq!(fs::read(dest.path().join("big.bin")).unwrap(), data);
}

#[tokio::test]
async fn password_states_on_download() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "secret.txt", b"top secret contents");
    let up = enqueue_upload_encrypted(
        &queue,
        &passwords,
        secret(PASSWORD),
        Compression::None,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap();
    all_completed(&queue, &up).await;
    let keys = vec!["secret.txt".to_owned()];

    for (password, expected) in [
        (None, Problem::PasswordNotFound),
        (Some(secret("")), Problem::PasswordNotFound),
        (Some(secret("wrong")), Problem::PasswordNotValid),
    ] {
        let dest = tempfile::tempdir().unwrap();
        let tasks = enqueue_download_decrypted(
            &queue,
            &passwords,
            password,
            s3.ops(),
            ACCOUNT,
            &bucket,
            &keys,
            dest.path(),
        )
        .await
        .unwrap();
        let done = settle(&queue, &tasks).await;
        assert_eq!(done[0].state, TransferState::Failed);
        let message = done[0].error.clone().unwrap();
        let expected_text = match expected {
            Problem::PasswordNotFound => "password not found",
            _ => "password not valid",
        };
        assert!(message.contains(expected_text), "{message}");
        // Nothing was downloaded at all.
        assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
    }

    let missing = cse::check_object_password(s3.ops(), &bucket, "secret.txt", None)
        .await
        .unwrap_err();
    assert_eq!(cse::problem(&missing), Some(Problem::PasswordNotFound));
    assert_eq!(missing.kind(), ErrorKind::Auth);
    let wrong = cse::check_object_password(s3.ops(), &bucket, "secret.txt", Some(&secret("nope")))
        .await
        .unwrap_err();
    assert_eq!(cse::problem(&wrong), Some(Problem::PasswordNotValid));
    cse::check_object_password(s3.ops(), &bucket, "secret.txt", Some(&secret(PASSWORD)))
        .await
        .unwrap();
}

#[tokio::test]
async fn plain_objects_are_saved_as_they_are_by_a_decrypting_download() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "plain.txt", b"just text".to_vec()).await;
    s3.put(&bucket, "empty.txt", Vec::new()).await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let dest = tempfile::tempdir().unwrap();
    let tasks = enqueue_download_decrypted(
        &queue,
        &passwords,
        None,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["plain.txt".to_owned(), "empty.txt".to_owned()],
        dest.path(),
    )
    .await
    .unwrap();
    all_completed(&queue, &tasks).await;
    assert_eq!(
        fs::read(dest.path().join("plain.txt")).unwrap(),
        b"just text"
    );
    assert_eq!(fs::read(dest.path().join("empty.txt")).unwrap(), b"");
    let probe = cse::probe_object(s3.ops(), &bucket, "plain.txt")
        .await
        .unwrap();
    assert!(!probe.encrypted && !probe.marked);
    let probe = cse::probe_object(s3.ops(), &bucket, "empty.txt")
        .await
        .unwrap();
    assert!(!probe.encrypted);
}

#[tokio::test]
async fn a_modified_object_fails_the_download_and_leaves_no_file() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "a.bin", &payload(3 * MIB, 4));
    let up = enqueue_upload_encrypted(
        &queue,
        &passwords,
        secret(PASSWORD),
        Compression::None,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap();
    all_completed(&queue, &up).await;
    let stored = s3.get(&bucket, "a.bin").await;

    for (name, bad) in [
        ("flipped", {
            let mut bad = stored.clone();
            let at = bad.len() / 2;
            bad[at] ^= 1;
            bad
        }),
        ("truncated", stored[..stored.len() - MIB - 100].to_vec()),
        ("cut at the end", stored[..stored.len() - 1].to_vec()),
    ] {
        s3.put(&bucket, "bad.bin", bad).await;
        let dest = tempfile::tempdir().unwrap();
        let tasks = enqueue_download_decrypted(
            &queue,
            &passwords,
            Some(secret(PASSWORD)),
            s3.ops(),
            ACCOUNT,
            &bucket,
            &["bad.bin".to_owned()],
            dest.path(),
        )
        .await
        .unwrap();
        let done = settle(&queue, &tasks).await;
        assert_eq!(done[0].state, TransferState::Failed, "{name}");
        assert!(
            done[0].error.clone().unwrap().contains("integrity"),
            "{name}: {:?}",
            done[0].error
        );
        assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0, "{name}");
    }
}

#[tokio::test]
async fn an_encrypting_task_without_its_password_never_uploads_plaintext() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    // The runner knows no password: the situation after a restart, when the
    // queue file still names the encrypting group.
    let runner_passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &runner_passwords).await;
    let other = CsePasswords::new();
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "x.txt", b"plain");
    let tasks = enqueue_upload_encrypted(
        &queue,
        &other,
        secret(PASSWORD),
        Compression::None,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap();
    let done = settle(&queue, &tasks).await;
    assert_eq!(done[0].state, TransferState::Failed);
    let message = done[0].error.clone().unwrap();
    assert!(message.contains("password not found"), "{message}");
    assert!(!s3.exists(&bucket, "x.txt").await);
}

#[tokio::test]
async fn queue_state_never_holds_the_password() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "x.txt", b"plain");
    let up = enqueue_upload_encrypted(
        &queue,
        &passwords,
        secret("unmistakable-password-text"),
        Compression::Gzip,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap();
    all_completed(&queue, &up).await;
    let task = queue.get(&up[0].id).unwrap();
    let json = serde_json::to_string(&task).unwrap();
    assert!(!json.contains("unmistakable"));
    assert!(task.group_id.unwrap().starts_with("cse-enc-gzip-"));
    let resume = format!("{:?}", queue.resume_state(&up[0].id));
    assert!(!resume.contains("unmistakable"));
}

#[tokio::test]
async fn transform_object_encrypts_and_decrypts_in_place_keeping_metadata() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let data = payload(2 * MIB + 3, 7);
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("docs/report.csv")
        .content_type("text/csv")
        .cache_control("max-age=60")
        .content_disposition("attachment")
        .metadata("owner", "ops")
        .body(bucketbridge_core::aws_sdk_s3::primitives::ByteStream::from(
            data.clone(),
        ))
        .send()
        .await
        .unwrap();
    let engine = engine(&s3);
    let hooks = Hooks::new();
    let encrypt = TransformRequest {
        bucket: bucket.clone(),
        key: "docs/report.csv".to_owned(),
        action: Action::Encrypt(EncryptOptions {
            compression: Compression::Gzip,
            ..EncryptOptions::default()
        }),
        password: secret(PASSWORD),
    };
    let outcome = cse::transform_object(&engine, &encrypt, &hooks)
        .await
        .unwrap();
    assert_eq!(outcome.size_before, data.len() as u64);
    let head = s3
        .ops()
        .head_object(&bucket, "docs/report.csv", None)
        .await
        .unwrap();
    assert_eq!(head.size, outcome.size_after);
    assert_eq!(head.meta.content_type.as_deref(), Some("text/csv"));
    assert_eq!(head.meta.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(head.meta.content_disposition.as_deref(), Some("attachment"));
    assert_eq!(
        head.meta.user_meta.get("owner").map(String::as_str),
        Some("ops")
    );
    assert_eq!(
        head.meta.user_meta.get("bb-cse").map(String::as_str),
        Some("bbcse1;aes-256-gcm;gzip")
    );
    assert!(
        s3.get(&bucket, "docs/report.csv")
            .await
            .starts_with(b"BBCSE")
    );

    // Encrypting twice is refused, and nothing changes.
    let again = cse::transform_object(&engine, &encrypt, &hooks)
        .await
        .unwrap_err();
    assert_eq!(again.kind(), ErrorKind::Conflict);

    let decrypt = |password: &str| TransformRequest {
        bucket: bucket.clone(),
        key: "docs/report.csv".to_owned(),
        action: Action::Decrypt,
        password: secret(password),
    };
    let wrong = cse::transform_object(&engine, &decrypt("wrong"), &hooks)
        .await
        .unwrap_err();
    assert_eq!(cse::problem(&wrong), Some(Problem::PasswordNotValid));
    assert!(
        s3.get(&bucket, "docs/report.csv")
            .await
            .starts_with(b"BBCSE")
    );

    cse::transform_object(&engine, &decrypt(PASSWORD), &hooks)
        .await
        .unwrap();
    assert_eq!(s3.get(&bucket, "docs/report.csv").await, data);
    let head = s3
        .ops()
        .head_object(&bucket, "docs/report.csv", None)
        .await
        .unwrap();
    assert!(!head.meta.user_meta.contains_key("bb-cse"));
    assert_eq!(
        head.meta.user_meta.get("owner").map(String::as_str),
        Some("ops")
    );
    assert_eq!(head.meta.content_type.as_deref(), Some("text/csv"));

    let not_encrypted = cse::transform_object(&engine, &decrypt(PASSWORD), &hooks)
        .await
        .unwrap_err();
    assert_eq!(not_encrypted.kind(), ErrorKind::Usage);

    let folder = TransformRequest {
        key: "docs/".to_owned(),
        ..decrypt(PASSWORD)
    };
    assert!(matches!(
        cse::transform_object(&engine, &folder, &hooks).await,
        Err(Error::Usage(_))
    ));
    let missing = TransformRequest {
        key: "nothing.txt".to_owned(),
        ..decrypt(PASSWORD)
    };
    assert_eq!(
        cse::transform_object(&engine, &missing, &hooks)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
}

#[tokio::test]
async fn transform_object_restores_the_content_encoding_it_had() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("a.txt")
        .content_encoding("identity")
        .body(bucketbridge_core::aws_sdk_s3::primitives::ByteStream::from_static(b"hello"))
        .send()
        .await
        .unwrap();
    let engine = engine(&s3);
    let request = |action| TransformRequest {
        bucket: bucket.clone(),
        key: "a.txt".to_owned(),
        action,
        password: secret(PASSWORD),
    };
    cse::transform_object(
        &engine,
        &request(Action::Encrypt(EncryptOptions::default())),
        &Hooks::new(),
    )
    .await
    .unwrap();
    let head = s3.ops().head_object(&bucket, "a.txt", None).await.unwrap();
    assert_eq!(head.meta.content_encoding, None);
    assert_eq!(
        head.meta
            .user_meta
            .get("bb-cse-content-encoding")
            .map(String::as_str),
        Some("identity")
    );
    cse::transform_object(&engine, &request(Action::Decrypt), &Hooks::new())
        .await
        .unwrap();
    let head = s3.ops().head_object(&bucket, "a.txt", None).await.unwrap();
    assert_eq!(head.meta.content_encoding.as_deref(), Some("identity"));
    assert!(!head.meta.user_meta.contains_key("bb-cse-content-encoding"));
}

#[tokio::test]
async fn ordinary_transfers_are_unchanged() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "p.txt", b"plain text");
    let up = enqueue_upload(&queue, ACCOUNT, &[path], &bucket, "")
        .await
        .unwrap();
    all_completed(&queue, &up).await;
    assert_eq!(s3.get(&bucket, "p.txt").await, b"plain text");
    let head = s3.ops().head_object(&bucket, "p.txt", None).await.unwrap();
    assert!(!head.meta.user_meta.contains_key("bb-cse"));
    let dest = tempfile::tempdir().unwrap();
    let down = enqueue_download(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        &["p.txt".to_owned()],
        dest.path(),
    )
    .await
    .unwrap();
    all_completed(&queue, &down).await;
    assert_eq!(fs::read(dest.path().join("p.txt")).unwrap(), b"plain text");
}

#[tokio::test]
async fn empty_password_is_refused_before_anything_is_queued() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let passwords = CsePasswords::new();
    let queue = queue_with(s3.ops(), &passwords).await;
    let source = tempfile::tempdir().unwrap();
    let path = write_file(source.path(), "p.txt", b"x");
    let err = enqueue_upload_encrypted(
        &queue,
        &passwords,
        secret(""),
        Compression::None,
        ACCOUNT,
        &[path],
        &bucket,
        "",
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(passwords.is_empty());
}
