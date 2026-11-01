//! The process-wide entry point: [`Session`].
//!
//! The desktop host and the `bb` CLI each hold one session. It owns:
//!
//! - the application directories and the [`AccountManager`] (accounts, their
//!   secrets, the master password);
//! - the in-memory MFA sessions ([`MfaSessions`]);
//! - one proxy-aware HTTP client ([`crate::proxy`]) shared by every SDK
//!   client, rebuilt when the proxy settings or the proxy password change;
//! - one [`S3`] handle per account, rebuilt when the account, the client
//!   settings, the HTTP client or its MFA session change.
//!
//! # Rules
//!
//! - [`Session::s3_for`] fails with `locked` while a master password is set
//!   and the secrets are locked, then with `mfa_required` when the account
//!   has an MFA serial but no valid MFA session ([`Session::mfa_login`]
//!   creates one). Accounts with an MFA session sign with its temporary
//!   credentials.
//! - [`Session::lock`] forgets the key, every MFA session and every cached
//!   client (they hold credentials). Rotating an account's keys, changing its
//!   MFA serial or endpoint, or removing it, drops its MFA session. An MFA
//!   login that is waiting for STS when any of this happens stores nothing
//!   ([`MfaSessions::generation`]), and an MFA session only ever signs for the
//!   account state it was issued for ([`MfaSessions::credentials`]), so a
//!   change made by another process drops it too.
//! - Security changes (unlock, lock, master password) are broadcast as
//!   [`SecurityStatus`] ([`Session::subscribe`]); the desktop forwards them
//!   as `security://changed`. So is a change found while reading
//!   `config.toml`: another process changing the master password, or the
//!   master-password section disappearing from the file (the vault then
//!   locks and does not fall back to plain mode, see
//!   [`crate::secrets::Vault::sync_config`]).
//!
//! Every method that touches the keychain, the key derivation or the
//! configuration file runs that work on a blocking thread, so all async
//! methods can be awaited from any Tokio task. In a process that holds a
//! session, make account and security changes through it: cached clients and
//! MFA sessions are then dropped immediately. Changes made elsewhere (another
//! process, or the account manager directly) are picked up on the next
//! [`Session::s3_for`], which compares what each cached client was built
//! from.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use aws_smithy_runtime_api::client::http::SharedHttpClient;
use tokio::sync::broadcast;

use crate::accounts::AccountManager;
use crate::client::ClientOptions;
use crate::cloudfront::{CloudFront, Distribution};
use crate::config::{AppConfig, ConfigStore, Dirs};
use crate::error::{Error, ErrorKind, Result};
use crate::iam::{self, Iam};
use crate::mfa::{self, MfaSessions};
use crate::model::{
    Account, AccountInput, AccountList, AccountUpdate, Bucket, BucketKind, SecurityStatus,
};
use crate::ops::S3;
use crate::proxy::HttpClientBuilder;
use crate::secrets::{KdfCost, KeyringStore, SecretStore, SecretString};
use crate::settings::{AdvancedSettings, DataIntegritySettings, ProxySettings, Settings};

/// Status updates buffered per subscriber; a slow subscriber skips the
/// oldest ones.
const STATUS_CHANNEL_CAPACITY: usize = 16;

/// Customizes the STS client configuration of [`Session::mfa_login`], for
/// example to inject a test client.
pub type StsConfigHook =
    Arc<dyn Fn(aws_sdk_sts::config::Builder) -> aws_sdk_sts::config::Builder + Send + Sync>;

/// Customizes the CloudFront client configuration of
/// [`Session::cloudfront_for`], for example to inject a test client.
pub type CloudFrontConfigHook = Arc<
    dyn Fn(aws_sdk_cloudfront::config::Builder) -> aws_sdk_cloudfront::config::Builder
        + Send
        + Sync,
>;
/// Customizes the IAM client configuration of [`Session::iam_for`], for
/// example to inject a test client.
pub type IamConfigHook =
    Arc<dyn Fn(aws_sdk_iam::config::Builder) -> aws_sdk_iam::config::Builder + Send + Sync>;

/// Options of [`Session::open_with`].
#[derive(Clone)]
pub struct SessionOptions {
    /// Argon2id cost for master passwords set from now on.
    pub kdf_cost: KdfCost,
    /// Whether the proxy environment variables apply while the proxy
    /// settings are disabled (default `true`, like the SDK's own client).
    pub environment_proxy: bool,
    /// MFA sessions to use; default: new ones measured with the system clock.
    pub mfa_sessions: Option<Arc<MfaSessions>>,
    /// Applied to the STS client configuration of [`Session::mfa_login`].
    pub sts_config_hook: Option<StsConfigHook>,
    /// Applied to the CloudFront client configuration of
    /// [`Session::cloudfront_for`].
    pub cloudfront_config_hook: Option<CloudFrontConfigHook>,
    /// Applied to the IAM client configuration of [`Session::iam_for`].
    pub iam_config_hook: Option<IamConfigHook>,
    /// Endpoints of the URL shorteners; default: the providers' real ones.
    /// Tests point them at a local server.
    pub shortener_endpoints: Option<crate::shorten::Endpoints>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            kdf_cost: KdfCost::RECOMMENDED,
            environment_proxy: true,
            mfa_sessions: None,
            sts_config_hook: None,
            cloudfront_config_hook: None,
            iam_config_hook: None,
            shortener_endpoints: None,
        }
    }
}

impl fmt::Debug for SessionOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionOptions")
            .field("kdf_cost", &self.kdf_cost)
            .field("environment_proxy", &self.environment_proxy)
            .field("mfa_sessions", &self.mfa_sessions.is_some())
            .field("sts_config_hook", &self.sts_config_hook.is_some())
            .field(
                "cloudfront_config_hook",
                &self.cloudfront_config_hook.is_some(),
            )
            .field("iam_config_hook", &self.iam_config_hook.is_some())
            .field("shortener_endpoints", &self.shortener_endpoints)
            .finish()
    }
}

/// The process-wide entry point; see the [module docs](self).
pub struct Session {
    dirs: Dirs,
    manager: Arc<AccountManager>,
    mfa: Arc<MfaSessions>,
    environment_proxy: bool,
    sts_config_hook: Option<StsConfigHook>,
    cloudfront_config_hook: Option<CloudFrontConfigHook>,
    iam_config_hook: Option<IamConfigHook>,
    shortener_endpoints: crate::shorten::Endpoints,
    clients: Mutex<Clients>,
    status: broadcast::Sender<SecurityStatus>,
    /// The status last announced; see [`Session::observe`].
    announced: Mutex<SecurityStatus>,
}

#[derive(Default)]
struct Clients {
    http: Option<CachedHttp>,
    next_generation: u64,
    s3: HashMap<String, CachedS3>,
    /// The distributions of each account, listed once per session (see
    /// [`Session::cloudfront_distributions`]).
    cloudfront: HashMap<String, Arc<Vec<Distribution>>>,
    /// Bumped whenever every client is forgotten (lock, settings and
    /// security changes): a client built before that is not cached.
    epoch: u64,
}

struct CachedHttp {
    proxy: ProxySettings,
    client: SharedHttpClient,
    generation: u64,
}

struct CachedS3 {
    key: S3Key,
    s3: S3,
}

/// Everything an S3 handle was built from.
#[derive(Clone, PartialEq)]
struct S3Key {
    account: Account,
    advanced: AdvancedSettings,
    data_integrity: DataIntegritySettings,
    http_generation: u64,
    mfa_until: Option<SystemTime>,
}

impl Session {
    /// Opens the session on the OS keychain. When the platform has no usable
    /// keychain (for example Linux without a Secret Service), the session
    /// still opens; everything that needs a stored secret then fails with an
    /// `io` error that says why. Blocking: call it at startup or from a
    /// blocking thread.
    pub fn open(dirs: Dirs) -> Result<Session> {
        let store: Arc<dyn SecretStore> = match KeyringStore::new() {
            Ok(store) => Arc::new(store),
            Err(error) => Arc::new(UnavailableStore {
                reason: error.to_string(),
            }),
        };
        Self::open_with(dirs, store, SessionOptions::default())
    }

    /// Opens the session on an explicit secret store (tests, diagnostics).
    /// Removes values left over by interrupted changes. Blocking.
    pub fn open_with(
        dirs: Dirs,
        store: Arc<dyn SecretStore>,
        options: SessionOptions,
    ) -> Result<Session> {
        let manager =
            AccountManager::with_kdf_cost(ConfigStore::new(&dirs), store, options.kdf_cost)?;
        // A failure keeps the leftovers listed for the next start.
        let _leftovers = manager.purge_leftovers();
        // A configuration from before installation ids moves its proxy
        // password to its own reference; retried at unlock when locked.
        let _migration = manager.migrate_legacy_references();
        let (status, _) = broadcast::channel(STATUS_CHANNEL_CAPACITY);
        let announced = Mutex::new(manager.vault().status());
        Ok(Session {
            dirs,
            manager: Arc::new(manager),
            mfa: options
                .mfa_sessions
                .unwrap_or_else(|| Arc::new(MfaSessions::new())),
            environment_proxy: options.environment_proxy,
            sts_config_hook: options.sts_config_hook,
            cloudfront_config_hook: options.cloudfront_config_hook,
            iam_config_hook: options.iam_config_hook,
            shortener_endpoints: options.shortener_endpoints.unwrap_or_default(),
            clients: Mutex::new(Clients::default()),
            status,
            announced,
        })
    }

    /// The application directories.
    pub fn dirs(&self) -> &Dirs {
        &self.dirs
    }

    /// The account manager, for reads; make changes through the session.
    pub fn accounts(&self) -> &AccountManager {
        &self.manager
    }

    /// The in-memory MFA sessions.
    pub fn mfa_sessions(&self) -> &MfaSessions {
        &self.mfa
    }

    /// Security status changes (unlock, lock, master password changes).
    pub fn subscribe(&self) -> broadcast::Receiver<SecurityStatus> {
        self.status.subscribe()
    }

    /// Whether a master password is set and whether secrets are readable.
    pub async fn security_status(&self) -> Result<SecurityStatus> {
        let manager = Arc::clone(&self.manager);
        let status = self.managed(move || manager.security_status()).await?;
        if !status.unlocked {
            self.forget_clients();
        }
        Ok(status)
    }

    /// Unlocks the secrets. A wrong password is an `auth` error and changes
    /// nothing.
    pub async fn unlock(&self, password: &str) -> Result<SecurityStatus> {
        let password = SecretString::from(password);
        let manager = Arc::clone(&self.manager);
        match blocking(move || manager.unlock(password.expose())).await {
            Ok(status) => {
                self.announce(status);
                Ok(status)
            }
            Err(error) => {
                // Reading the file may still have changed the status.
                self.observe();
                Err(error)
            }
        }
    }

    /// Locks the secrets and forgets every MFA session and cached client.
    pub fn lock(&self) -> SecurityStatus {
        let status = self.manager.lock();
        self.mfa.clear();
        self.forget_clients();
        self.announce(status);
        status
    }

    /// Sets, changes or removes the master password (see
    /// [`AccountManager::set_master_password`]).
    pub async fn set_master_password(
        &self,
        current: Option<&str>,
        new_password: Option<&str>,
    ) -> Result<SecurityStatus> {
        let current = current.map(SecretString::from);
        let new_password = new_password.map(SecretString::from);
        let manager = Arc::clone(&self.manager);
        let result = blocking(move || {
            manager.set_master_password(
                current.as_ref().map(SecretString::expose),
                new_password.as_ref().map(SecretString::expose),
            )
        })
        .await;
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                // Reading the file may still have changed the status.
                self.observe();
                return Err(error);
            }
        };
        self.forget_clients();
        self.announce(status);
        Ok(status)
    }

    /// Every account and the default account. Works while locked.
    pub async fn list_accounts(&self) -> Result<AccountList> {
        let manager = Arc::clone(&self.manager);
        blocking(move || manager.list()).await
    }

    /// Adds an account (see [`AccountManager::add`]).
    pub async fn add_account(&self, input: AccountInput) -> Result<Account> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.add(&input)).await
    }

    /// Updates an account (see [`AccountManager::update`]). Rotating its
    /// keys, changing its MFA serial or its endpoint drops its MFA session,
    /// and fails an MFA login of the account that is waiting for STS.
    pub async fn update_account(&self, id: &str, update: AccountUpdate) -> Result<Account> {
        let drops_mfa_session = update.access_key_id.is_some()
            || update.secret_access_key.is_some()
            || update.session_token.is_some()
            || update.clear_session_token
            || update.mfa_serial.is_some()
            || update.clear_mfa_serial
            || update.endpoint.is_some()
            || update.clear_endpoint;
        let manager = Arc::clone(&self.manager);
        let owned_id = id.to_owned();
        let account = self
            .managed(move || manager.update(&owned_id, &update))
            .await?;
        self.forget_s3(id);
        if drops_mfa_session {
            self.mfa.remove(id);
        }
        Ok(account)
    }

    /// Removes an account and its secrets, and drops its MFA session (an MFA
    /// login of the account that is waiting for STS fails).
    pub async fn remove_account(&self, id: &str) -> Result<()> {
        let manager = Arc::clone(&self.manager);
        let owned_id = id.to_owned();
        self.managed(move || manager.remove(&owned_id)).await?;
        self.forget_s3(id);
        self.mfa.remove(id);
        Ok(())
    }

    /// Makes `id` the default account.
    pub async fn set_default_account(&self, id: &str) -> Result<()> {
        let manager = Arc::clone(&self.manager);
        let owned_id = id.to_owned();
        self.managed(move || manager.set_default(&owned_id)).await
    }

    /// The current settings.
    pub async fn settings(&self) -> Result<Settings> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.load().map(|config| config.settings))
            .await
    }

    /// Validates and saves the settings, including the stricter proxy
    /// checks of [`crate::proxy::validate_proxy_settings`] (see
    /// [`AccountManager::save_settings`]). The proxy password reference is
    /// kept.
    pub async fn save_settings(&self, settings: Settings) -> Result<()> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.save_settings(&settings))
            .await?;
        self.forget_clients();
        Ok(())
    }

    /// Stores (`Some`) or deletes (`None`) the proxy password.
    pub async fn set_proxy_password(&self, password: Option<&str>) -> Result<()> {
        let password = password.map(SecretString::from);
        let manager = Arc::clone(&self.manager);
        self.managed(move || {
            manager.set_proxy_password(password.as_ref().map(SecretString::expose))
        })
        .await?;
        self.forget_clients();
        Ok(())
    }

    /// Whether the proxy environment variables apply while the proxy
    /// settings are disabled (see [`SessionOptions::environment_proxy`]).
    pub fn environment_proxy(&self) -> bool {
        self.environment_proxy
    }

    /// The proxy settings and, when they authenticate, the proxy password
    /// (see [`AccountManager::proxy_credentials`]).
    pub async fn proxy_credentials(&self) -> Result<(ProxySettings, Option<SecretString>)> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.proxy_credentials()).await
    }

    /// Stores (`Some`) or deletes (`None`) the URL shortener token.
    pub async fn set_shortener_token(&self, token: Option<&str>) -> Result<()> {
        let token = token.map(SecretString::from);
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.set_shortener_token(token.as_ref().map(SecretString::expose)))
            .await
    }

    /// Shortens `long_url` with the provider chosen in the settings. Fails
    /// with `usage` while no provider is chosen (nothing is ever sent then)
    /// and for a link that is not `http(s)`; see [`crate::shorten`].
    pub async fn shorten_url(&self, long_url: &str) -> Result<String> {
        self.shorten_url_with(&self.shortener_endpoints, None, long_url)
            .await
    }

    /// Like [`Session::shorten_url`] at explicit `endpoints`, with `token`
    /// replacing the stored token when it is not empty (`bb` reads it from
    /// the environment).
    pub async fn shorten_url_with(
        &self,
        endpoints: &crate::shorten::Endpoints,
        token: Option<SecretString>,
        long_url: &str,
    ) -> Result<String> {
        let manager = Arc::clone(&self.manager);
        let (shortener, stored) = self
            .managed(move || manager.shortener_credentials())
            .await?;
        if shortener.provider == crate::settings::ShortenerProvider::None {
            return Err(Error::Usage("no URL shortener is configured".into()));
        }
        let token = token.filter(|t| !t.is_empty()).or(stored);
        let client = self.http_client().await?;
        crate::shorten::shorten_at(
            &client,
            endpoints,
            shortener.provider,
            token.as_ref(),
            long_url,
        )
        .await
    }

    /// The proxy-aware HTTP client for the current settings.
    pub async fn http_client(&self) -> Result<SharedHttpClient> {
        let epoch = self.clients().epoch;
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        Ok(self.http_client_for(&config.settings.proxy, epoch).await?.0)
    }

    /// Logs the account in with the current code of its MFA device and keeps
    /// the session in memory. Returns when the session stops being used.
    ///
    /// The session is stored only if, while STS was answering, the secrets
    /// were not locked and the account was not removed or changed (rotated,
    /// another MFA serial or endpoint, by this process or another): the login
    /// then fails with `locked`, `not_found` or `conflict`, and the next
    /// request asks for a new code.
    pub async fn mfa_login(&self, account_id: &str, code: &str) -> Result<SystemTime> {
        // Read before the credentials: anything that forgets MFA sessions
        // after this point (a lock, an account change) makes the login fail.
        let generation = self.mfa.generation();
        let manager = Arc::clone(&self.manager);
        let owned_id = account_id.to_owned();
        let (account, credentials) = self
            .managed(move || manager.account_credentials(&owned_id))
            .await?;
        let http_client = self.http_client().await?;
        let mut builder = mfa::sts_config(&account, &credentials)?.http_client(http_client);
        drop(credentials);
        if let Some(hook) = &self.sts_config_hook {
            builder = hook(builder);
        }
        let client = aws_sdk_sts::Client::from_conf(builder.build());
        let session = mfa::request_session(&account, &client, code, None).await?;
        // The lock state and the account may have changed while STS answered.
        let (current, unlocked) = self.account_state(account_id).await?;
        if !unlocked {
            return Err(Error::Locked);
        }
        let usable_until = self.mfa.insert_since(&current, generation, session)?;
        self.forget_s3(account_id);
        Ok(usable_until)
    }

    /// The CloudFront handle of an account (see [`crate::cloudfront`]),
    /// signed with its credentials (the MFA session's when it has one) and
    /// sent through the proxy-aware HTTP client. Fails with `locked`,
    /// `mfa_required` or `not_found` like [`Session::s3_for`], and with
    /// `usage` for an account with a custom endpoint.
    pub async fn cloudfront_for(&self, account_id: &str) -> Result<CloudFront> {
        let manager = Arc::clone(&self.manager);
        let (config, status) = self
            .managed(move || {
                let config = manager.load()?;
                Ok((config, manager.vault().status()))
            })
            .await?;
        if !status.unlocked {
            self.forget_clients();
            return Err(Error::Locked);
        }
        let account = account_in(&config, account_id)?;
        // Refuse before any secret is read.
        crate::cloudfront::check_account(&account)?;
        let credentials = match account.mfa_serial {
            Some(_) => self.mfa.active(&account)?.0,
            None => {
                let manager = Arc::clone(&self.manager);
                let owned_id = account_id.to_owned();
                self.managed(move || manager.account_credentials(&owned_id))
                    .await?
                    .1
            }
        };
        let http_client = self.http_client().await?;
        let mut options = ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client.clone());
        let mut builder = crate::cloudfront::cloudfront_config(&account, &credentials, &options)?
            .http_client(http_client);
        if let Some(hook) = &self.cloudfront_config_hook {
            builder = hook(builder);
        }
        Ok(CloudFront::from_client(
            aws_sdk_cloudfront::Client::from_conf(builder.build()),
        ))
    }

    /// The distributions of an account, listed on the first call of the
    /// session and kept until the secrets are locked, the account changes or
    /// [`Session::forget_cloudfront`] is called. Used to mark buckets that
    /// have a distribution without a request on every listing.
    pub async fn cloudfront_distributions(
        &self,
        account_id: &str,
    ) -> Result<Arc<Vec<Distribution>>> {
        if let Some(cached) = self.clients().cloudfront.get(account_id) {
            return Ok(Arc::clone(cached));
        }
        let epoch = self.clients().epoch;
        let cloudfront = self.cloudfront_for(account_id).await?;
        let list = Arc::new(cloudfront.list_all_distributions().await?);
        let mut clients = self.clients();
        if clients.epoch == epoch {
            clients
                .cloudfront
                .insert(account_id.to_owned(), Arc::clone(&list));
        }
        Ok(list)
    }

    /// Gives the buckets of an AWS account that have a CloudFront
    /// distribution the [`BucketKind::CloudFront`] badge. Best effort and
    /// cheap: the account's distributions are listed once per session
    /// ([`Session::cloudfront_distributions`]); an account that may not list
    /// them is remembered as having none, and any other failure leaves the
    /// buckets as they are (the next call tries again). Buckets of accounts
    /// with a custom endpoint are never touched.
    pub async fn mark_cloudfront_buckets(&self, account_id: &str, buckets: &mut [Bucket]) {
        if !buckets.iter().any(|bucket| bucket.kind == BucketKind::Aws) {
            return;
        }
        let distributions = match self.cloudfront_distributions(account_id).await {
            Ok(list) => list,
            Err(error) => {
                if matches!(error.kind(), ErrorKind::Auth) {
                    self.clients()
                        .cloudfront
                        .insert(account_id.to_owned(), Arc::default());
                }
                return;
            }
        };
        let names: Vec<String> = buckets
            .iter()
            .filter(|bucket| bucket.kind == BucketKind::Aws)
            .map(|bucket| bucket.name.clone())
            .collect();
        let served = crate::cloudfront::buckets_with_distributions(&distributions, &names);
        for bucket in buckets.iter_mut() {
            if bucket.kind == BucketKind::Aws && served.contains(&bucket.name) {
                bucket.kind = BucketKind::CloudFront;
            }
        }
    }

    /// Drops the distributions kept for an account (after a change to them).
    pub fn forget_cloudfront(&self, account_id: &str) {
        self.clients().cloudfront.remove(account_id);
    }

    /// The account as stored now and whether the secrets are unlocked, read
    /// from the current `config.toml`.
    async fn account_state(&self, account_id: &str) -> Result<(Account, bool)> {
        let manager = Arc::clone(&self.manager);
        let owned_id = account_id.to_owned();
        self.managed(move || {
            let config = manager.load()?;
            let status = manager.vault().status();
            Ok((account_in(&config, &owned_id)?, status.unlocked))
        })
        .await
    }

    /// The S3 handle of an account, cached until something it was built
    /// from changes. Fails with `locked`, `mfa_required` or `not_found` (see
    /// the [module docs](self)).
    pub async fn s3_for(&self, account_id: &str) -> Result<S3> {
        let epoch = self.clients().epoch;
        let manager = Arc::clone(&self.manager);
        let (config, status) = self
            .managed(move || {
                let config = manager.load()?;
                Ok((config, manager.vault().status()))
            })
            .await?;
        if !status.unlocked {
            self.forget_clients();
            return Err(Error::Locked);
        }
        let account = account_in(&config, account_id)?;
        // A session issued for another state of the account (changed by
        // another process, for example) is dropped here.
        let mfa_session = match account.mfa_serial {
            Some(_) => Some(self.mfa.active(&account)?),
            None => None,
        };
        let mfa_until = mfa_session.as_ref().map(|(_, until)| *until);
        let (http_client, http_generation) =
            self.http_client_for(&config.settings.proxy, epoch).await?;
        let key = S3Key {
            account,
            advanced: config.settings.advanced.clone(),
            data_integrity: config.settings.data_integrity.clone(),
            http_generation,
            mfa_until,
        };
        if let Some(s3) = self.cached_s3(account_id, &key) {
            return Ok(s3);
        }
        let (key, credentials) = match mfa_session {
            Some((credentials, _)) => (key, credentials),
            None => {
                let manager = Arc::clone(&self.manager);
                let owned_id = account_id.to_owned();
                let (account, credentials) = self
                    .managed(move || manager.account_credentials(&owned_id))
                    .await?;
                (S3Key { account, ..key }, credentials)
            }
        };
        let mut options = ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client);
        let s3 = S3::new(&key.account, &credentials, &options)?;
        let mut clients = self.clients();
        if clients.epoch == epoch {
            clients.s3.insert(
                account_id.to_owned(),
                CachedS3 {
                    key,
                    s3: s3.clone(),
                },
            );
        }
        Ok(s3)
    }

    /// The IAM handle of an account, built for the call (IAM calls are
    /// rare, so nothing is cached). Fails with `locked`, `mfa_required` or
    /// `not_found` like [`Session::s3_for`], and with `usage`
    /// ([`iam::NOT_AVAILABLE`]) for an account with a custom endpoint.
    pub async fn iam_for(&self, account_id: &str) -> Result<Iam> {
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        let (account, unlocked) = self.account_state(account_id).await?;
        if !unlocked {
            return Err(Error::Locked);
        }
        if !iam::iam_supported(&account) {
            return Err(Error::Usage(iam::NOT_AVAILABLE.to_owned()));
        }
        let credentials = match account.mfa_serial {
            Some(_) => self.mfa.active(&account)?.0,
            None => {
                let manager = Arc::clone(&self.manager);
                let owned_id = account_id.to_owned();
                self.managed(move || manager.account_credentials(&owned_id))
                    .await?
                    .1
            }
        };
        let epoch = self.clients().epoch;
        let (http_client, _) = self.http_client_for(&config.settings.proxy, epoch).await?;
        let options = ClientOptions::from_settings(&config.settings);
        let mut builder = iam::iam_config(&account, &credentials, &options)?;
        builder = builder.http_client(http_client);
        if let Some(hook) = &self.iam_config_hook {
            builder = hook(builder);
        }
        Ok(Iam::from_config(builder.build(), options.read_timeout))
    }

    async fn http_client_for(
        &self,
        proxy: &ProxySettings,
        epoch: u64,
    ) -> Result<(SharedHttpClient, u64)> {
        if let Some(cached) = self
            .clients()
            .http
            .as_ref()
            .filter(|cached| cached.proxy == *proxy)
        {
            return Ok((cached.client.clone(), cached.generation));
        }
        let manager = Arc::clone(&self.manager);
        let environment_proxy = self.environment_proxy;
        let (proxy, client) = self
            .managed(move || {
                let (proxy, password) = manager.proxy_credentials()?;
                let client = HttpClientBuilder::new()
                    .proxy(&proxy, password.as_ref().map(SecretString::expose))?
                    .environment_proxy(environment_proxy)
                    .build()?;
                Ok((proxy, client))
            })
            .await?;
        let mut clients = self.clients();
        let generation = clients.next_generation;
        clients.next_generation += 1;
        if clients.epoch == epoch {
            clients.http = Some(CachedHttp {
                proxy,
                client: client.clone(),
                generation,
            });
        }
        Ok((client, generation))
    }

    fn cached_s3(&self, account_id: &str, key: &S3Key) -> Option<S3> {
        self.clients()
            .s3
            .get(account_id)
            .filter(|cached| cached.key == *key)
            .map(|cached| cached.s3.clone())
    }

    fn forget_clients(&self) {
        let mut clients = self.clients();
        clients.http = None;
        clients.s3.clear();
        clients.cloudfront.clear();
        clients.epoch += 1;
    }

    fn forget_s3(&self, account_id: &str) {
        let mut clients = self.clients();
        clients.s3.remove(account_id);
        clients.cloudfront.remove(account_id);
    }

    fn clients(&self) -> MutexGuard<'_, Clients> {
        // Each critical section is a single cache update: never inconsistent.
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs blocking account-manager work, then announces a security change
    /// that reading `config.toml` brought to light (see [`Session::observe`]).
    async fn managed<T, F>(&self, work: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let result = blocking(work).await;
        self.observe();
        result
    }

    /// Broadcasts `status` and remembers it as the announced one.
    fn announce(&self, status: SecurityStatus) {
        *self.announced() = status;
        // No subscriber is fine.
        let _receivers = self.status.send(status);
    }

    /// Announces the vault's status when it differs from the announced one:
    /// the master password was changed by another process, or its section
    /// disappeared from `config.toml` (the vault locks, see
    /// [`crate::secrets::Vault::sync_config`]). Locking, however it came about,
    /// drops every MFA session and cached client.
    fn observe(&self) {
        let status = self.manager.vault().status();
        let changed = {
            let mut announced = self.announced();
            let changed = *announced != status;
            *announced = status;
            changed
        };
        if changed {
            if !status.unlocked {
                self.mfa.clear();
                self.forget_clients();
            }
            // No subscriber is fine.
            let _receivers = self.status.send(status);
        }
    }

    fn announced(&self) -> MutexGuard<'_, SecurityStatus> {
        // A plain copy is stored: never inconsistent.
        self.announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The account `account_id` of `config`.
fn account_in(config: &AppConfig, account_id: &str) -> Result<Account> {
    config
        .accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("account {account_id} does not exist")))
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("dirs", &self.dirs)
            .field("accounts", &self.manager)
            .field("mfa", &self.mfa)
            .finish_non_exhaustive()
    }
}

/// Runs blocking work (keychain, key derivation, files) off the async
/// workers.
async fn blocking<T, F>(work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        if error.is_cancelled() {
            Error::Cancelled
        } else {
            Error::Internal(format!("a background task failed: {error}"))
        }
    })?
}

/// Stands in for the OS keychain when there is none: everything that needs
/// a stored secret fails with the reason; the rest of the app works.
struct UnavailableStore {
    reason: String,
}

impl UnavailableStore {
    fn error(&self) -> Error {
        Error::io(
            "the OS keychain is not available",
            std::io::Error::other(self.reason.clone()),
        )
    }
}

impl SecretStore for UnavailableStore {
    fn get(&self, _reference: &str) -> Result<Option<SecretString>> {
        Err(self.error())
    }

    fn set(&self, _reference: &str, _value: &str) -> Result<()> {
        Err(self.error())
    }

    fn delete(&self, _reference: &str) -> Result<()> {
        Err(self.error())
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_sts::operation::get_session_token::GetSessionTokenOutput;
    use aws_sdk_sts::types::Credentials as StsCredentials;
    use aws_smithy_mocks::{
        MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock,
    };
    use aws_smithy_types::DateTime;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Notify;

    use super::*;
    use crate::error::ErrorKind;
    use crate::secrets::MemoryStore;

    const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const ROTATED_ACCESS_KEY: &str = "AKIAROTATEDEXAMPLE01";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const ROTATED: &str = "rotated-secret-value";
    const PASSWORD: &str = "master password";
    const SERIAL: &str = "arn:aws:iam::123456789012:mfa/example-device";

    fn options() -> SessionOptions {
        SessionOptions {
            kdf_cost: KdfCost {
                m_cost_kib: 8,
                t_cost: 1,
                p_cost: 1,
            },
            environment_proxy: false,
            ..SessionOptions::default()
        }
    }

    fn open(dir: &tempfile::TempDir, store: &Arc<MemoryStore>, options: SessionOptions) -> Session {
        Session::open_with(Dirs::under(dir.path()), store.clone(), options).unwrap()
    }

    /// `AccountUpdate` zeroizes on drop, so struct update syntax is not
    /// available.
    fn updating(change: impl FnOnce(&mut AccountUpdate)) -> AccountUpdate {
        let mut update = AccountUpdate::default();
        change(&mut update);
        update
    }

    /// A local S3 endpoint that answers every request with an empty bucket
    /// listing and records the request heads, to see what requests are
    /// signed with.
    struct Recorder {
        endpoint: String,
        heads: Arc<Mutex<Vec<String>>>,
    }

    impl Recorder {
        async fn start() -> Recorder {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
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
                                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                                seen.lock().unwrap().push(head);
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
            Recorder { endpoint, heads }
        }

        /// Lists buckets with `s3` and returns the head of that request.
        async fn request_head(&self, s3: &S3) -> String {
            let before = self.heads.lock().unwrap().len();
            s3.list_buckets().await.unwrap();
            let heads = self.heads.lock().unwrap();
            assert_eq!(heads.len(), before + 1);
            heads[before].to_lowercase()
        }

        fn input(&self, label: &str, mfa_serial: Option<&str>) -> AccountInput {
            AccountInput {
                label: label.to_owned(),
                access_key_id: ACCESS_KEY.to_owned(),
                secret_access_key: SECRET.to_owned(),
                session_token: None,
                region: "us-east-1".to_owned(),
                endpoint: Some(self.endpoint.clone()),
                path_style: true,
                request_payer: false,
                mfa_serial: mfa_serial.map(str::to_owned),
            }
        }
    }

    fn signed_by(head: &str, access_key: &str) -> bool {
        head.contains(&format!("credential={}/", access_key.to_lowercase()))
    }

    fn sts_rule() -> Rule {
        mock!(aws_sdk_sts::Client::get_session_token)
            .match_requests(|input| {
                input.serial_number() == Some(SERIAL) && input.token_code() == Some("123456")
            })
            .then_output(|| {
                GetSessionTokenOutput::builder()
                    .credentials(
                        StsCredentials::builder()
                            .access_key_id("ASIATEMPORARYEXAMPLE")
                            .secret_access_key("temporary-secret")
                            .session_token("temporary-token")
                            .expiration(DateTime::from_secs(4_102_444_800))
                            .build()
                            .unwrap(),
                    )
                    .build()
            })
    }

    fn with_mock_sts(rule: &Rule) -> SessionOptions {
        let rule = rule.clone();
        let hook: StsConfigHook = Arc::new(move |builder| {
            builder
                .interceptor(
                    MockResponseInterceptor::new()
                        .rule_mode(RuleMode::MatchAny)
                        .with_rule(&rule),
                )
                .http_client(create_mock_http_client())
        });
        SessionOptions {
            sts_config_hook: Some(hook),
            ..options()
        }
    }

    #[tokio::test]
    async fn s3_for_is_locked_until_unlocked() {
        let recorder = Recorder::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let first = open(&dir, &store, options());
        let account = first
            .add_account(recorder.input("Work", None))
            .await
            .unwrap();
        first
            .set_master_password(None, Some(PASSWORD))
            .await
            .unwrap();
        assert!(first.s3_for(&account.id).await.is_ok());

        let session = open(&dir, &store, options());
        let mut changes = session.subscribe();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        assert_eq!(
            session.unlock("wrong password").await.unwrap_err().kind(),
            ErrorKind::Auth
        );
        let status = session.unlock(PASSWORD).await.unwrap();
        assert!(status.unlocked);
        assert_eq!(changes.recv().await.unwrap(), status);
        let s3 = session.s3_for(&account.id).await.unwrap();
        assert!(signed_by(&recorder.request_head(&s3).await, ACCESS_KEY));

        let locked = session.lock();
        assert!(!locked.unlocked);
        assert_eq!(changes.recv().await.unwrap(), locked);
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        assert_eq!(
            session.s3_for("missing").await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        session.unlock(PASSWORD).await.unwrap();
        assert_eq!(
            session.s3_for("missing").await.unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[tokio::test]
    async fn changed_keys_rebuild_the_client() {
        let recorder = Recorder::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let session = open(&dir, &store, options());
        let account = session
            .add_account(recorder.input("Work", None))
            .await
            .unwrap();
        let s3 = session.s3_for(&account.id).await.unwrap();
        assert!(signed_by(&recorder.request_head(&s3).await, ACCESS_KEY));

        let rotate = updating(|u| {
            u.access_key_id = Some(ROTATED_ACCESS_KEY.to_owned());
            u.secret_access_key = Some(ROTATED.to_owned());
        });
        session.update_account(&account.id, rotate).await.unwrap();
        let s3 = session.s3_for(&account.id).await.unwrap();
        assert!(signed_by(
            &recorder.request_head(&s3).await,
            ROTATED_ACCESS_KEY
        ));

        // A change made by another process is noticed as well.
        let other = open(&dir, &store, options());
        let back = updating(|u| {
            u.access_key_id = Some(ACCESS_KEY.to_owned());
            u.secret_access_key = Some(SECRET.to_owned());
        });
        other.update_account(&account.id, back).await.unwrap();
        let s3 = session.s3_for(&account.id).await.unwrap();
        assert!(signed_by(&recorder.request_head(&s3).await, ACCESS_KEY));
    }

    #[tokio::test]
    async fn mfa_accounts_need_a_session() {
        let recorder = Recorder::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let rule = sts_rule();
        let session = open(&dir, &store, with_mock_sts(&rule));
        let account = session
            .add_account(recorder.input("Work", Some(SERIAL)))
            .await
            .unwrap();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
        assert_eq!(
            session
                .mfa_login(&account.id, "12345")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
        session.mfa_login(&account.id, "123456").await.unwrap();
        assert_eq!(rule.num_calls(), 1);
        let s3 = session.s3_for(&account.id).await.unwrap();
        let head = recorder.request_head(&s3).await;
        assert!(signed_by(&head, "ASIATEMPORARYEXAMPLE"), "{head}");
        assert!(
            head.contains("x-amz-security-token: temporary-token"),
            "{head}"
        );

        // Rotating the keys drops the MFA session.
        let rotate = updating(|u| u.secret_access_key = Some(ROTATED.to_owned()));
        session.update_account(&account.id, rotate).await.unwrap();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::MfaRequired
        );

        // So does locking.
        session.mfa_login(&account.id, "123456").await.unwrap();
        assert!(session.s3_for(&account.id).await.is_ok());
        session.lock();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::MfaRequired
        );

        // Changing the region keeps it; removing the MFA serial drops it and
        // the account signs with its own keys again.
        session.mfa_login(&account.id, "123456").await.unwrap();
        let region = updating(|u| u.region = Some("eu-west-1".to_owned()));
        session.update_account(&account.id, region).await.unwrap();
        assert!(session.s3_for(&account.id).await.is_ok());
        let no_mfa = updating(|u| u.clear_mfa_serial = true);
        session.update_account(&account.id, no_mfa).await.unwrap();
        assert!(
            session
                .mfa_sessions()
                .usable_until_for(&account.id)
                .is_none()
        );
        let s3 = session.s3_for(&account.id).await.unwrap();
        let head = recorder.request_head(&s3).await;
        assert!(signed_by(&head, ACCESS_KEY), "{head}");
        assert!(!head.contains("x-amz-security-token"), "{head}");

        // Changing the endpoint drops it: the temporary credentials issued
        // for one server are never sent to another.
        let elsewhere = Recorder::start().await;
        let moving = session
            .add_account(recorder.input("Moving", Some(SERIAL)))
            .await
            .unwrap();
        session.mfa_login(&moving.id, "123456").await.unwrap();
        assert!(session.s3_for(&moving.id).await.is_ok());
        let moved = updating(|u| u.endpoint = Some(elsewhere.endpoint.clone()));
        session.update_account(&moving.id, moved).await.unwrap();
        assert!(
            session
                .mfa_sessions()
                .usable_until_for(&moving.id)
                .is_none()
        );
        assert_eq!(
            session.s3_for(&moving.id).await.unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
        assert!(elsewhere.heads.lock().unwrap().is_empty());
        session.remove_account(&moving.id).await.unwrap();

        // A change made by another process is noticed too: this process keeps
        // its MFA session only while the account is what it was issued for.
        let other = open(&dir, &store, with_mock_sts(&rule));
        let shared = session
            .add_account(recorder.input("Shared", Some(SERIAL)))
            .await
            .unwrap();
        let changes: Vec<(&str, AccountUpdate)> = vec![
            (
                "keys",
                updating(|u| u.secret_access_key = Some(ROTATED.to_owned())),
            ),
            (
                "access key",
                updating(|u| u.access_key_id = Some(ROTATED_ACCESS_KEY.to_owned())),
            ),
            (
                "session token",
                updating(|u| u.session_token = Some("static-token".to_owned())),
            ),
            (
                "MFA device",
                updating(|u| {
                    u.mfa_serial = Some("arn:aws:iam::123456789012:mfa/new-device".to_owned());
                }),
            ),
            (
                "endpoint",
                updating(|u| u.endpoint = Some(elsewhere.endpoint.clone())),
            ),
        ];
        for (what, change) in changes {
            // Back to what the session was issued for, then log in.
            let reset = updating(|u| {
                u.endpoint = Some(recorder.endpoint.clone());
                u.mfa_serial = Some(SERIAL.to_owned());
                u.clear_session_token = true;
            });
            session.update_account(&shared.id, reset).await.unwrap();
            session.mfa_login(&shared.id, "123456").await.unwrap();
            assert!(session.s3_for(&shared.id).await.is_ok(), "{what}");
            other.update_account(&shared.id, change).await.unwrap();
            assert_eq!(
                session.s3_for(&shared.id).await.unwrap_err().kind(),
                ErrorKind::MfaRequired,
                "{what}"
            );
            assert!(
                session
                    .mfa_sessions()
                    .usable_until_for(&shared.id)
                    .is_none(),
                "{what}: the stale session was dropped"
            );
        }
        // Nothing but the recorder's own requests ever carried the
        // temporary token, and the other endpoint saw nothing at all.
        assert!(elsewhere.heads.lock().unwrap().is_empty());

        session.remove_account(&shared.id).await.unwrap();

        // Removing the account drops everything.
        session.remove_account(&account.id).await.unwrap();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert!(store.is_empty());
    }

    /// An STS endpoint (the account's own: S3-compatible servers serve STS at
    /// their endpoint) that holds its answer until released, to change things
    /// while a login is waiting for it.
    struct HeldSts {
        endpoint: String,
        got_request: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl HeldSts {
        async fn start() -> HeldSts {
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
                                    .find_map(|line| line.strip_prefix("content-length:"))
                                    .and_then(|value| value.trim().parse::<usize>().ok())
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
            HeldSts {
                endpoint,
                got_request,
                release,
            }
        }

        fn input(&self) -> AccountInput {
            AccountInput {
                label: "Work".to_owned(),
                access_key_id: ACCESS_KEY.to_owned(),
                secret_access_key: SECRET.to_owned(),
                session_token: None,
                region: "us-east-1".to_owned(),
                endpoint: Some(self.endpoint.clone()),
                path_style: true,
                request_payer: false,
                mfa_serial: Some(SERIAL.to_owned()),
            }
        }

        fn input_labelled(&self, label: &str) -> AccountInput {
            let mut input = self.input();
            input.label = label.to_owned();
            input
        }

        /// Logs `account_id` in; while STS holds its answer, `meanwhile` runs;
        /// then STS answers. Returns the outcome of the login.
        async fn login_while(
            &self,
            session: &Arc<Session>,
            account_id: &str,
            meanwhile: impl std::future::Future<Output = ()>,
        ) -> Result<SystemTime> {
            let login = {
                let session = Arc::clone(session);
                let id = account_id.to_owned();
                tokio::spawn(async move { session.mfa_login(&id, "123456").await })
            };
            tokio::time::timeout(Duration::from_secs(20), self.got_request.notified())
                .await
                .unwrap();
            meanwhile.await;
            self.release.notify_one();
            login.await.unwrap()
        }
    }

    /// An MFA login waits for STS for a while. Whatever forgets MFA sessions
    /// meanwhile (a lock, a change of the account, its removal) must win: the
    /// session that STS issues afterwards is not stored.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mfa_logins_do_not_outlive_what_overtakes_them() {
        let sts = HeldSts::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let session = Arc::new(open(&dir, &store, options()));
        let account = session.add_account(sts.input()).await.unwrap();
        session
            .set_master_password(None, Some(PASSWORD))
            .await
            .unwrap();
        let id = account.id.clone();
        let stored = || session.mfa_sessions().usable_until_for(&id).is_some();

        // Lock, also when the secrets are unlocked again before STS answers.
        let outcome = sts
            .login_while(&session, &id, async {
                session.lock();
                assert!(session.mfa_sessions().usable_until_for(&id).is_none());
            })
            .await;
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::Locked);
        assert!(!stored());
        assert_eq!(
            session.s3_for(&id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        session.unlock(PASSWORD).await.unwrap();
        assert_eq!(
            session.s3_for(&id).await.unwrap_err().kind(),
            ErrorKind::MfaRequired,
            "no code was entered after the lock"
        );
        let outcome = sts
            .login_while(&session, &id, async {
                session.lock();
                session.unlock(PASSWORD).await.unwrap();
            })
            .await;
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::Conflict);
        assert!(!stored());

        // Keys rotated, another MFA device, another endpoint: the account is
        // no longer what the code was entered for.
        let elsewhere = Recorder::start().await;
        let changes: Vec<(&str, AccountUpdate)> = vec![
            (
                "keys",
                updating(|u| u.secret_access_key = Some(ROTATED.to_owned())),
            ),
            (
                "access key",
                updating(|u| u.access_key_id = Some(ROTATED_ACCESS_KEY.to_owned())),
            ),
            (
                "MFA device",
                updating(|u| {
                    u.mfa_serial = Some("arn:aws:iam::123456789012:mfa/new-device".to_owned());
                }),
            ),
            (
                "endpoint",
                updating(|u| u.endpoint = Some(elsewhere.endpoint.clone())),
            ),
        ];
        for (what, change) in changes {
            let outcome = sts
                .login_while(&session, &id, async {
                    session.update_account(&id, change).await.unwrap();
                })
                .await;
            assert_eq!(outcome.unwrap_err().kind(), ErrorKind::Conflict, "{what}");
            assert!(!stored(), "{what}");
            // Back to the original account for the next round.
            let reset = updating(|u| {
                u.secret_access_key = Some(SECRET.to_owned());
                u.access_key_id = Some(ACCESS_KEY.to_owned());
                u.mfa_serial = Some(SERIAL.to_owned());
                u.endpoint = Some(sts.endpoint.clone());
            });
            session.update_account(&id, reset).await.unwrap();
        }

        // The session of the account being forgotten while STS answers (what
        // `update_account` does after it saved a change).
        let outcome = sts
            .login_while(&session, &id, async {
                session.mfa_sessions().remove(&id);
            })
            .await;
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::Conflict);
        assert!(!stored());

        // Removal of the account.
        let outcome = sts
            .login_while(&session, &id, async {
                session.remove_account(&id).await.unwrap();
            })
            .await;
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::NotFound);
        assert!(!stored());
        assert_eq!(
            session.s3_for(&id).await.unwrap_err().kind(),
            ErrorKind::NotFound
        );

        // Changes that do not matter for the credentials do not stop a login,
        // and neither does an unrelated account.
        let account = session
            .add_account(sts.input_labelled("Again"))
            .await
            .unwrap();
        let other = session
            .add_account(sts.input_labelled("Unrelated"))
            .await
            .unwrap();
        let outcome = sts
            .login_while(&session, &account.id, async {
                let region = updating(|u| {
                    u.region = Some("eu-west-1".to_owned());
                    u.label = Some("Renamed".to_owned());
                });
                session.update_account(&account.id, region).await.unwrap();
                let rotate = updating(|u| u.secret_access_key = Some(ROTATED.to_owned()));
                session.update_account(&other.id, rotate).await.unwrap();
                session.mfa_sessions().remove(&other.id);
            })
            .await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(
            session
                .mfa_sessions()
                .usable_until_for(&account.id)
                .is_some()
        );
        assert!(session.s3_for(&account.id).await.is_ok());
    }

    /// The master password changed by another process, and the section of
    /// `config.toml` that holds it disappearing, are announced and lock the
    /// session, dropping what a lock drops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn security_changes_found_in_the_file_are_announced() {
        let recorder = Recorder::start().await;
        let rule = sts_rule();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let session = open(&dir, &store, with_mock_sts(&rule));
        let account = session
            .add_account(recorder.input("Work", Some(SERIAL)))
            .await
            .unwrap();
        session
            .set_master_password(None, Some(PASSWORD))
            .await
            .unwrap();
        session.mfa_login(&account.id, "123456").await.unwrap();
        assert!(session.s3_for(&account.id).await.is_ok());
        let mut changes = session.subscribe();

        // Another process changes the master password.
        let other = open(&dir, &store, options());
        other.unlock(PASSWORD).await.unwrap();
        other
            .set_master_password(Some(PASSWORD), Some("a new master password"))
            .await
            .unwrap();
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        let locked = SecurityStatus {
            master_password_set: true,
            unlocked: false,
        };
        assert_eq!(changes.try_recv().unwrap(), locked);
        assert!(changes.try_recv().is_err(), "announced once");
        assert!(
            session
                .mfa_sessions()
                .usable_until_for(&account.id)
                .is_none(),
            "a lock drops the MFA sessions"
        );

        // The section of config.toml disappears while the session is unlocked.
        session.unlock("a new master password").await.unwrap();
        assert_eq!(
            changes.try_recv().unwrap(),
            SecurityStatus {
                master_password_set: true,
                unlocked: true
            }
        );
        session.mfa_login(&account.id, "123456").await.unwrap();
        let section = session.accounts().load().unwrap().security.unwrap();
        ConfigStore::new(&Dirs::under(dir.path()))
            .update(|config| {
                config.security = None;
                Ok(())
            })
            .unwrap();
        let status = session.security_status().await.unwrap();
        assert_eq!(status, locked);
        assert_eq!(changes.try_recv().unwrap(), locked);
        assert!(changes.try_recv().is_err());
        assert!(
            session
                .mfa_sessions()
                .usable_until_for(&account.id)
                .is_none()
        );
        // No plain mode: nothing is stored, nothing is readable, and the
        // session cannot be unlocked until the section is back.
        let before = store.snapshot().len();
        let mut second = recorder.input("Second", None);
        second.secret_access_key = "second-secret-must-not-be-plain".to_owned();
        assert_eq!(
            session.add_account(second).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        assert_eq!(
            session.s3_for(&account.id).await.unwrap_err().kind(),
            ErrorKind::Locked
        );
        assert_eq!(
            session
                .unlock("a new master password")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(store.snapshot().len(), before);
        assert!(
            store
                .snapshot()
                .iter()
                .all(|(_, value)| value.expose() != "second-secret-must-not-be-plain")
        );
        ConfigStore::new(&Dirs::under(dir.path()))
            .update(|config| {
                config.security = Some(section.clone());
                Ok(())
            })
            .unwrap();
        session.unlock("a new master password").await.unwrap();
        assert!(session.s3_for(&account.id).await.is_err());
        session.mfa_login(&account.id, "123456").await.unwrap();
        assert!(session.s3_for(&account.id).await.is_ok());
    }

    #[tokio::test]
    async fn settings_keep_the_proxy_password_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let session = open(&dir, &store, options());
        session
            .set_proxy_password(Some("proxy password"))
            .await
            .unwrap();
        let mut settings = session.settings().await.unwrap();
        let reference = settings.proxy.password_ref.clone();
        let id = session.accounts().load().unwrap().installation_id.unwrap();
        assert_eq!(
            reference.as_deref(),
            Some(format!("proxy-{id}/password/1").as_str())
        );
        settings.proxy.enabled = true;
        settings.proxy.host = "proxy.example.test".into();
        settings.proxy.username = Some("user".into());
        settings.proxy.password_ref = None;
        session.save_settings(settings.clone()).await.unwrap();
        assert_eq!(
            session.settings().await.unwrap().proxy.password_ref,
            reference
        );
        assert!(session.http_client().await.is_ok());

        settings.proxy.bypass = vec!["host:8080".into()];
        assert_eq!(
            session.save_settings(settings).await.unwrap_err().kind(),
            ErrorKind::Usage
        );
    }

    #[tokio::test]
    async fn opening_purges_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        store.set("orphan/secret/2", "left over").unwrap();
        std::fs::write(
            dir.path().join(crate::accounts::SECRETS_JOURNAL_FILE),
            r#"{"version":1,"references":["orphan/secret/2"]}"#,
        )
        .unwrap();
        let _session = open(&dir, &store, options());
        assert!(store.is_empty());
    }

    #[test]
    fn unavailable_keychain_fails_with_the_reason() {
        let store = UnavailableStore {
            reason: "no Secret Service".to_owned(),
        };
        let error = store.get("a/secret/1").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert!(error.to_string().contains("no Secret Service"));
    }

    #[test]
    fn handles_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Session>();
        assert_send_sync::<AccountManager>();
    }
}
