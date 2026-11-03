//! Regression tests of the security review of accounts, sessions, MFA and
//! proxies. Each test reproduces an attack that used to work and asserts the
//! correct behaviour. Secrets live in a `MemoryStore`; the OS keychain is
//! never touched.
//!
//! 1. `config.toml` is not integrity protected: references in it must not
//!    make BucketBridge read a stored secret for another purpose, and removing
//!    the master-password section must not switch the vault to plain mode.
//! 2. Two configuration folders on one machine share one keychain.
//! 3. An MFA login that is waiting for STS must not outlive a lock, and an MFA
//!    session must not outlive the account state it was issued for.
//! 4. A password typed into the proxy host field must not leak.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bucketbridge_core::config::ConfigStore;
use bucketbridge_core::mfa::MfaSession;
use bucketbridge_core::model::{AccountInput, AccountUpdate, SecurityStatus};
use bucketbridge_core::proxy::validate_proxy_settings;
use bucketbridge_core::secrets::{KdfCost, MemoryStore, SecretStore};
use bucketbridge_core::session::SessionOptions;
use bucketbridge_core::settings::{ProxyKind, ProxySettings};
use bucketbridge_core::{AccountManager, Credentials, Dirs, ErrorKind, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const MASTER_PASSWORD: &str = "correct horse battery staple";
const SERIAL: &str = "arn:aws:iam::123456789012:mfa/example-device";
const COST: KdfCost = KdfCost {
    m_cost_kib: 8,
    t_cost: 1,
    p_cost: 1,
};

fn options() -> SessionOptions {
    SessionOptions {
        kdf_cost: COST,
        environment_proxy: false,
        ..SessionOptions::default()
    }
}

fn open(dir: &tempfile::TempDir, store: &Arc<MemoryStore>) -> Session {
    Session::open_with(Dirs::under(dir.path()), store.clone(), options()).unwrap()
}

fn manager(dir: &tempfile::TempDir, store: &Arc<MemoryStore>) -> AccountManager {
    let store: Arc<dyn SecretStore> = store.clone();
    AccountManager::with_kdf_cost(ConfigStore::new(&Dirs::under(dir.path())), store, COST).unwrap()
}

fn input(endpoint: Option<String>, mfa_serial: Option<&str>) -> AccountInput {
    AccountInput {
        source: bucketbridge_core::model::CredentialSource::StaticKeys,
        label: "Work".to_owned(),
        access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
        secret_access_key: SECRET.to_owned(),
        session_token: None,
        region: "eu-west-1".to_owned(),
        endpoint,
        path_style: true,
        request_payer: false,
        mfa_serial: mfa_serial.map(str::to_owned),
    }
}

fn updating(change: impl FnOnce(&mut AccountUpdate)) -> AccountUpdate {
    let mut update = AccountUpdate::default();
    change(&mut update);
    update
}

fn edit_config(dir: &tempfile::TempDir, change: impl FnOnce(&mut bucketbridge_core::AppConfig)) {
    ConfigStore::new(&Dirs::under(dir.path()))
        .update(|config| {
            change(config);
            Ok(())
        })
        .unwrap();
}

/// A TCP listener that records every request head (lowercased) and answers
/// 200 with an empty bucket listing: good enough for a proxy and for S3.
struct Recorder {
    port: u16,
    endpoint: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&heads);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            seen.lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(&buffer[..end]).to_lowercase());
                            let body = "<ListAllMyBucketsResult><Buckets></Buckets></ListAllMyBucketsResult>";
                            let response = format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/xml\r\n\
                                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            return;
                        }
                    }
                });
            }
        });
        Self {
            port,
            endpoint: format!("http://127.0.0.1:{port}"),
            heads,
        }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

// ---- 1. references in config.toml ------------------------------------------

/// The proxy password reference was pointed at the account's secret access
/// key, and the proxy at a host the attacker controls. The secret used to be
/// sent in `Proxy-Authorization` after the user unlocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_password_reference_cannot_point_at_an_account_secret() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let account = {
        let session = open(&dir, &store);
        let account = session.add_account(input(None, None)).await.unwrap();
        session
            .set_master_password(None, Some(MASTER_PASSWORD))
            .await
            .unwrap();
        account
    };
    let attacker = Recorder::start().await;
    edit_config(&dir, |config| {
        let secret_ref = config.accounts[0].secret_ref.clone();
        let proxy = &mut config.settings.proxy;
        proxy.enabled = true;
        proxy.kind = ProxyKind::Http;
        proxy.host = "127.0.0.1".to_owned();
        proxy.port = attacker.port;
        proxy.username = Some("x".to_owned());
        proxy.password_ref = Some(secret_ref);
        config.settings.advanced.max_attempts = 1;
    });

    let session = open(&dir, &store);
    session.unlock(MASTER_PASSWORD).await.unwrap();
    let error = session.accounts().proxy_credentials().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage, "{error}");
    assert!(!error.to_string().contains(SECRET));
    let error = session.s3_for(&account.id).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage, "{error}");
    assert!(
        attacker.heads().is_empty(),
        "the attacker's proxy was contacted: {:#?}",
        attacker.heads()
    );
}

/// The session token reference was pointed at the account's own secret access
/// key and its endpoint at an attacker's host: the secret used to travel in
/// `x-amz-security-token`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_token_reference_cannot_point_at_the_secret_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let account = {
        let session = open(&dir, &store);
        let account = session.add_account(input(None, None)).await.unwrap();
        session
            .set_master_password(None, Some(MASTER_PASSWORD))
            .await
            .unwrap();
        account
    };
    let attacker = Recorder::start().await;
    edit_config(&dir, |config| {
        let account = &mut config.accounts[0];
        account.session_token_ref = Some(account.secret_ref.clone());
        account.endpoint = Some(attacker.endpoint.clone());
    });

    let session = open(&dir, &store);
    session.unlock(MASTER_PASSWORD).await.unwrap();
    let error = session.s3_for(&account.id).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage, "{error}");
    assert!(attacker.heads().is_empty(), "{:#?}", attacker.heads());
}

/// Another account's secret, or its token, is no better a target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_cannot_be_pointed_at_another_accounts_secret() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let session = open(&dir, &store);
    let first = session.add_account(input(None, None)).await.unwrap();
    let mut other = input(None, None);
    other.label = "Other".to_owned();
    other.secret_access_key = "another-accounts-secret".to_owned();
    let second = session.add_account(other).await.unwrap();
    edit_config(&dir, |config| {
        config.accounts[0].secret_ref = second.secret_ref.clone();
    });
    let error = session.s3_for(&first.id).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage, "{error}");
    assert!(session.s3_for(&second.id).await.is_ok());
}

/// Deleting the `[security]` section while the app runs used to switch the
/// vault silently to plain mode: the next saved secret was stored unencrypted,
/// and no status change was announced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_security_section_locks_instead_of_switching_to_plain_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let session = open(&dir, &store);
    session.add_account(input(None, None)).await.unwrap();
    session
        .set_master_password(None, Some(MASTER_PASSWORD))
        .await
        .unwrap();
    let mut changes = session.subscribe();
    let section = session.accounts().load().unwrap().security.unwrap();

    edit_config(&dir, |config| config.security = None);

    let mut second = input(None, None);
    second.label = "Second".to_owned();
    second.secret_access_key = "second-secret-now-in-plaintext".to_owned();
    let error = session.add_account(second).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Locked, "{error}");
    assert!(
        store
            .snapshot()
            .iter()
            .all(|(_, value)| value.expose() != "second-secret-now-in-plaintext"),
        "the new secret was stored in plain text"
    );
    // The change was announced, and the session cannot be unlocked as long as
    // the file lacks the section.
    assert_eq!(
        changes.try_recv().unwrap(),
        SecurityStatus {
            master_password_set: true,
            unlocked: false
        }
    );
    assert_eq!(
        session.unlock(MASTER_PASSWORD).await.unwrap_err().kind(),
        ErrorKind::Conflict
    );
    // Restoring the section (a backup) recovers.
    edit_config(&dir, |config| config.security = Some(section));
    assert!(session.unlock(MASTER_PASSWORD).await.unwrap().unlocked);
}

// ---- 2. two configuration folders, one keychain ----------------------------

fn enable_proxy(manager: &AccountManager) {
    let mut settings = manager.load().unwrap().settings;
    settings.proxy.enabled = true;
    settings.proxy.host = "proxy.example.test".to_owned();
    settings.proxy.username = Some("user".to_owned());
    manager.save_settings(&settings).unwrap();
}

fn proxy_password(manager: &AccountManager) -> bucketbridge_core::Result<String> {
    let (_, password) = manager.proxy_credentials()?;
    Ok(password.unwrap().expose().to_owned())
}

/// Both configurations used to store their proxy password under
/// `proxy/password/1`, and each deleted the other's value when it changed it.
#[test]
fn proxy_passwords_of_two_config_dirs_do_not_overwrite_each_other() {
    let store = Arc::new(MemoryStore::new());
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (manager(&dir_a, &store), manager(&dir_b, &store));
    enable_proxy(&a);
    enable_proxy(&b);

    a.set_proxy_password(Some("password-of-A")).unwrap();
    b.set_proxy_password(Some("password-of-B")).unwrap();
    assert_eq!(proxy_password(&a).unwrap(), "password-of-A");
    assert_eq!(proxy_password(&b).unwrap(), "password-of-B");
    b.set_proxy_password(Some("password-of-B-2")).unwrap();
    assert_eq!(proxy_password(&a).unwrap(), "password-of-A");
    assert_eq!(proxy_password(&b).unwrap(), "password-of-B-2");
    let (id_a, id_b) = (
        a.load().unwrap().installation_id.unwrap(),
        b.load().unwrap().installation_id.unwrap(),
    );
    assert_ne!(id_a, id_b);
    for (id, manager) in [(&id_a, &a), (&id_b, &b)] {
        let reference = manager.load().unwrap().settings.proxy.password_ref.unwrap();
        assert!(
            reference.starts_with(&format!("proxy-{id}/")),
            "{reference}"
        );
    }
}

/// A master password in one folder used to make the other folder's plaintext
/// proxy password overwrite the envelope (and the reverse).
#[test]
fn master_password_in_one_dir_does_not_break_the_other() {
    let store = Arc::new(MemoryStore::new());
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (manager(&dir_a, &store), manager(&dir_b, &store));
    enable_proxy(&a);
    enable_proxy(&b);
    a.set_master_password(None, Some("master of A")).unwrap();
    a.set_proxy_password(Some("password-of-A")).unwrap();
    b.set_proxy_password(Some("password-of-B")).unwrap();
    assert_eq!(proxy_password(&a).unwrap(), "password-of-A");
    assert_eq!(proxy_password(&b).unwrap(), "password-of-B");
}

/// Account secrets of independent configurations never collide either, and
/// rotating or removing in one leaves the other alone.
#[test]
fn account_secrets_of_two_config_dirs_are_independent() {
    let store = Arc::new(MemoryStore::new());
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (manager(&dir_a, &store), manager(&dir_b, &store));
    let in_a = a.add(&input(None, None)).unwrap();
    let in_b = b.add(&input(None, None)).unwrap();
    assert_ne!(in_a.secret_ref, in_b.secret_ref);
    a.update(
        &in_a.id,
        &updating(|u| u.secret_access_key = Some("rotated".into())),
    )
    .unwrap();
    a.remove(&in_a.id).unwrap();
    let (_, credentials) = b.account_credentials(&in_b.id).unwrap();
    assert_eq!(credentials.secret_access_key, SECRET);
}

/// The installation id lives in `config.toml`, so a byte-for-byte copy of the
/// file is the same installation: it must not be used next to the original
/// with a shared keychain (see docs/security.md).
#[test]
fn a_copied_configuration_carries_the_installation_id() {
    let store = Arc::new(MemoryStore::new());
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = manager(&dir_a, &store);
    a.add(&input(None, None)).unwrap();
    std::fs::copy(
        dir_a.path().join("config.toml"),
        dir_b.path().join("config.toml"),
    )
    .unwrap();
    let b = manager(&dir_b, &store);
    assert_eq!(
        a.load().unwrap().installation_id,
        b.load().unwrap().installation_id
    );
}

// ---- 3. MFA ----------------------------------------------------------------

fn temporary_session() -> MfaSession {
    MfaSession::new(
        Credentials::new(
            "ASIATEMPORARYEXAMPLE",
            "temporary-secret",
            Some("temporary-token-from-sts".to_owned()),
        ),
        SystemTime::now() + Duration::from_secs(3600),
    )
}

/// Changing only the endpoint used to keep the MFA session: the temporary
/// credentials issued for endpoint A were sent to endpoint B.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoint_change_drops_the_mfa_session() {
    let (a, b) = (Recorder::start().await, Recorder::start().await);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let session = open(&dir, &store);
    let account = session
        .add_account(input(Some(a.endpoint.clone()), Some(SERIAL)))
        .await
        .unwrap();
    session
        .mfa_sessions()
        .insert(&account, temporary_session())
        .unwrap();
    let s3 = session.s3_for(&account.id).await.unwrap();
    s3.list_buckets().await.unwrap();
    assert!(
        a.heads()[0].contains("x-amz-security-token: temporary-token-from-sts"),
        "{:?}",
        a.heads()
    );

    let moved = updating(|u| u.endpoint = Some(b.endpoint.clone()));
    session.update_account(&account.id, moved).await.unwrap();
    assert_eq!(
        session.s3_for(&account.id).await.unwrap_err().kind(),
        ErrorKind::MfaRequired
    );
    assert!(b.heads().is_empty(), "{:?}", b.heads());
}

/// Another process rotated the keys and replaced the MFA device: this process
/// used to keep signing with the MFA session of the old key and device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changes_by_another_process_drop_the_mfa_session() {
    let recorder = Recorder::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let desktop = open(&dir, &store);
    let account = desktop
        .add_account(input(Some(recorder.endpoint.clone()), Some(SERIAL)))
        .await
        .unwrap();
    desktop
        .mfa_sessions()
        .insert(&account, temporary_session())
        .unwrap();
    assert!(desktop.s3_for(&account.id).await.is_ok());

    let cli = open(&dir, &store);
    let change = updating(|u| {
        u.access_key_id = Some("AKIAROTATEDEXAMPLE01".to_owned());
        u.secret_access_key = Some("rotated-secret".to_owned());
        u.mfa_serial = Some("arn:aws:iam::123456789012:mfa/new-device".to_owned());
    });
    cli.update_account(&account.id, change).await.unwrap();

    assert_eq!(
        desktop.s3_for(&account.id).await.unwrap_err().kind(),
        ErrorKind::MfaRequired
    );
    assert!(recorder.heads().is_empty(), "{:?}", recorder.heads());
}

/// An STS endpoint that holds its answer until released.
struct HeldSts {
    endpoint: String,
    got_request: Arc<Notify>,
    release: Arc<Notify>,
}

impl HeldSts {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let got_request = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (got, rel) = (Arc::clone(&got_request), Arc::clone(&release));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (got, rel) = (Arc::clone(&got), Arc::clone(&rel));
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head_end, length) = loop {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
                            let length = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while buffer.len() < head_end + length {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                    }
                    got.notify_one();
                    rel.notified().await;
                    let body = r#"<GetSessionTokenResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <GetSessionTokenResult>
    <Credentials>
      <SessionToken>temporary-token</SessionToken>
      <SecretAccessKey>temporary-secret</SecretAccessKey>
      <Expiration>2099-01-01T00:00:00Z</Expiration>
      <AccessKeyId>ASIATEMPORARYEXAMPLE</AccessKeyId>
    </Credentials>
  </GetSessionTokenResult>
  <ResponseMetadata><RequestId>58c5dbae-abef-11e0-8cfe-09039844ac7d</RequestId></ResponseMetadata>
</GetSessionTokenResponse>"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/xml\r\ncontent-length: {}\r\n\
                         connection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            endpoint,
            got_request,
            release,
        }
    }
}

/// `lock` "forgets every MFA session", but a login waiting for STS used to
/// store its session after the lock: once unlocked again, the account worked
/// without a new MFA code. The same race existed for removing the account, for
/// a key rotation and for a change of the MFA serial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mfa_login_in_flight_does_not_survive_lock_or_account_changes() {
    let sts = HeldSts::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let session = Arc::new(open(&dir, &store));
    let account = session
        .add_account(input(Some(sts.endpoint.clone()), Some(SERIAL)))
        .await
        .unwrap();
    session
        .set_master_password(None, Some(MASTER_PASSWORD))
        .await
        .unwrap();
    let id = account.id.clone();

    let login = {
        let session = Arc::clone(&session);
        let id = id.clone();
        tokio::spawn(async move { session.mfa_login(&id, "123456").await })
    };
    tokio::time::timeout(Duration::from_secs(20), sts.got_request.notified())
        .await
        .unwrap();
    assert!(!session.lock().unlocked);
    sts.release.notify_one();
    assert_eq!(login.await.unwrap().unwrap_err().kind(), ErrorKind::Locked);

    // Nothing was stored: after unlocking, a new code is required.
    assert!(session.mfa_sessions().usable_until_for(&id).is_none());
    session.unlock(MASTER_PASSWORD).await.unwrap();
    assert_eq!(
        session.s3_for(&id).await.unwrap_err().kind(),
        ErrorKind::MfaRequired
    );

    // A key rotation while STS answers wins as well.
    let login = {
        let session = Arc::clone(&session);
        let id = id.clone();
        tokio::spawn(async move { session.mfa_login(&id, "123456").await })
    };
    tokio::time::timeout(Duration::from_secs(20), sts.got_request.notified())
        .await
        .unwrap();
    let rotate = updating(|u| u.secret_access_key = Some("rotated-secret".to_owned()));
    session.update_account(&id, rotate).await.unwrap();
    sts.release.notify_one();
    assert!(login.await.unwrap().is_err());
    assert!(session.mfa_sessions().usable_until_for(&id).is_none());

    // So does the removal of the account.
    let login = {
        let session = Arc::clone(&session);
        let id = id.clone();
        tokio::spawn(async move { session.mfa_login(&id, "123456").await })
    };
    tokio::time::timeout(Duration::from_secs(20), sts.got_request.notified())
        .await
        .unwrap();
    session.remove_account(&id).await.unwrap();
    sts.release.notify_one();
    assert_eq!(
        login.await.unwrap().unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert!(session.mfa_sessions().usable_until_for(&id).is_none());
}

// ---- 4. credentials typed into the proxy host field -------------------------

const HOST_WITH_CREDENTIALS: &str = "alice:s3cr3t-proxy-pw@proxy.example.com";

#[test]
fn proxy_host_error_does_not_echo_a_typed_password() {
    let settings = ProxySettings {
        enabled: true,
        host: HOST_WITH_CREDENTIALS.to_owned(),
        ..ProxySettings::default()
    };
    let error = validate_proxy_settings(&settings).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(!error.to_string().contains("s3cr3t-proxy-pw"), "{error}");
}

#[test]
fn account_manager_does_not_save_credentials_in_the_proxy_host() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let manager = manager(&dir, &store);
    let mut settings = manager.load().unwrap().settings;
    settings.proxy.enabled = true;
    settings.proxy.host = HOST_WITH_CREDENTIALS.to_owned();
    let error = manager.save_settings(&settings).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(!error.to_string().contains("s3cr3t-proxy-pw"), "{error}");
    // The same for a proxy that is switched off, and nothing was written.
    settings.proxy.enabled = false;
    assert!(manager.save_settings(&settings).is_err());
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default();
    assert!(!text.contains("s3cr3t-proxy-pw"));
}
