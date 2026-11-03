//! Integration tests of `bucketbridge_core::Session` (accounts, the master
//! password and per-account S3 handles) against real S3 servers: two
//! accounts used at the same time, and a rotated secret that is actually
//! used. Secrets live in an in-memory store; the OS keychain is never
//! touched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bucketbridge_core::model::{AccountInput, AccountUpdate};
use bucketbridge_core::secrets::{KdfCost, MemoryStore};
use bucketbridge_core::session::SessionOptions;
use bucketbridge_core::{Dirs, ErrorKind, Session};
use bucketbridge_tests::TestS3;

const MASTER_PASSWORD: &str = "integration master password";

fn session(dir: &tempfile::TempDir) -> Session {
    let options = SessionOptions {
        // The cheapest Argon2id cost: these tests are about S3, not the KDF.
        kdf_cost: KdfCost {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        },
        // Proxy variables of the machine running the tests must not apply.
        environment_proxy: false,
        ..SessionOptions::default()
    };
    Session::open_with(
        Dirs::under(dir.path()),
        Arc::new(MemoryStore::new()),
        options,
    )
    .unwrap()
}

/// `AccountUpdate` zeroizes on drop, so struct update syntax is not
/// available.
fn updating(change: impl FnOnce(&mut AccountUpdate)) -> AccountUpdate {
    let mut update = AccountUpdate::default();
    change(&mut update);
    update
}

fn input(server: &TestS3, label: &str, secret: &str) -> AccountInput {
    AccountInput {
        source: bucketbridge_core::model::CredentialSource::StaticKeys,
        label: label.to_owned(),
        access_key_id: server.access_key().to_owned(),
        secret_access_key: secret.to_owned(),
        session_token: None,
        region: server.region().to_owned(),
        endpoint: Some(server.endpoint().to_owned()),
        path_style: true,
        request_payer: false,
        mfa_serial: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_accounts_are_used_at_the_same_time() {
    let (server_a, server_b) = tokio::join!(TestS3::start(), TestS3::start());
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir);
    let a = session
        .add_account(input(&server_a, "First", server_a.secret_key()))
        .await
        .unwrap();
    let b = session
        .add_account(input(&server_b, "Second", server_b.secret_key()))
        .await
        .unwrap();
    // Read the secrets through the master-password vault, too.
    session
        .set_master_password(None, Some(MASTER_PASSWORD))
        .await
        .unwrap();

    let (s3_a, s3_b) = tokio::try_join!(session.s3_for(&a.id), session.s3_for(&b.id)).unwrap();
    let (bucket_a, bucket_b) = (server_a.unique_bucket(), server_b.unique_bucket());
    let (created_a, created_b) = tokio::join!(
        s3_a.create_bucket(&bucket_a, None),
        s3_b.create_bucket(&bucket_b, None)
    );
    created_a.unwrap();
    created_b.unwrap();

    // Each account works on its own server only.
    let (listed_a, listed_b) = tokio::try_join!(s3_a.list_buckets(), s3_b.list_buckets()).unwrap();
    let names = |buckets: &[bucketbridge_core::model::Bucket]| {
        buckets.iter().map(|b| b.name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(names(&listed_a), vec![bucket_a.clone()]);
    assert_eq!(names(&listed_b), vec![bucket_b.clone()]);

    // Both keep working while used concurrently many times.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        for (id, bucket) in [
            (a.id.clone(), bucket_a.clone()),
            (b.id.clone(), bucket_b.clone()),
        ] {
            let s3 = session.s3_for(&id).await.unwrap();
            tasks.push(tokio::spawn(async move {
                let listed = s3.list_buckets().await.unwrap();
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].name, bucket);
            }));
        }
    }
    for task in tasks {
        task.await.unwrap();
    }

    // Locking stops both until the master password is given again.
    session.lock();
    for id in [&a.id, &b.id] {
        assert_eq!(
            session.s3_for(id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
    }
    session.unlock(MASTER_PASSWORD).await.unwrap();
    assert_eq!(
        session
            .s3_for(&b.id)
            .await
            .unwrap()
            .list_buckets()
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_rotated_secret_is_actually_used() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir);
    let account = session
        .add_account(input(&server, "Rotating", "not-the-right-secret"))
        .await
        .unwrap();
    let error = session
        .s3_for(&account.id)
        .await
        .unwrap()
        .list_buckets()
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth, "{error}");

    let rotate = updating(|u| u.secret_access_key = Some(server.secret_key().to_owned()));
    let rotated = session.update_account(&account.id, rotate).await.unwrap();
    assert_ne!(rotated.secret_ref, account.secret_ref);
    let buckets = session
        .s3_for(&account.id)
        .await
        .unwrap()
        .list_buckets()
        .await
        .unwrap();
    assert!(buckets.is_empty());
}
