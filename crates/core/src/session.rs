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
use crate::creds::CredsHooks;
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

mod credentials;

pub use credentials::{ProfileOptions, SsoLoginPoll, SsoLoginStart};

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
    /// Hooks of the credential sources other than static keys: where the
    /// AWS files are, SSO and instance metadata test clients.
    pub creds_hooks: CredsHooks,
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
            creds_hooks: CredsHooks::default(),
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
            .field("creds_hooks", &self.creds_hooks)
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
    creds_hooks: CredsHooks,
    sso_logins: Mutex<credentials::SsoLogins>,
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
    /// HTTP clients of accounts with their own proxy choice, by the proxy
    /// settings they were built for, each with its generation.
    account_http: HashMap<String, (SharedHttpClient, u64)>,
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
    /// The accounts an assumed role signs through, with their MFA sessions.
    chain: Vec<(Account, Option<SystemTime>)>,
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
            creds_hooks: options.creds_hooks,
            sso_logins: Mutex::new(credentials::SsoLogins::default()),
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
            || update.clear_endpoint
            || update.source.is_some();
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

    /// Stores a new arrangement of account folders and the order of the
    /// accounts (see [`AccountManager::set_layout`]).
    pub async fn set_account_layout(
        &self,
        layout: crate::accountfile::AccountLayout,
    ) -> Result<AccountList> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.set_layout(&layout)).await
    }

    /// The export of the named accounts, or of all (see
    /// [`AccountManager::export_accounts`]).
    pub async fn export_accounts(
        &self,
        ids: Option<Vec<String>>,
    ) -> Result<crate::accountfile::AccountExport> {
        let manager = Arc::clone(&self.manager);
        blocking(move || manager.export_accounts(ids.as_deref())).await
    }

    /// Imports accounts from an export (see
    /// [`AccountManager::import_accounts`]).
    pub async fn import_accounts(
        &self,
        export: crate::accountfile::AccountExport,
        policy: crate::accountfile::DuplicatePolicy,
    ) -> Result<crate::accountfile::ImportReport> {
        let manager = Arc::clone(&self.manager);
        self.managed(move || manager.import_accounts(&export, policy))
            .await
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

    /// Like [`Session::shorten_url`], with `provider` choosing the service
    /// for this link. A shortener must still be chosen in the settings
    /// (`none` there means shortening is off, whatever `provider` says). The
    /// stored token belongs to the service of the settings: with another
    /// `provider` it is not used.
    pub async fn shorten_url_as(
        &self,
        provider: Option<crate::settings::ShortenerProvider>,
        long_url: &str,
    ) -> Result<String> {
        self.shorten_url_via(&self.shortener_endpoints, None, provider, long_url)
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
        self.shorten_url_via(endpoints, token, None, long_url).await
    }

    /// [`Session::shorten_url_with`] and [`Session::shorten_url_as`] together.
    pub async fn shorten_url_via(
        &self,
        endpoints: &crate::shorten::Endpoints,
        token: Option<SecretString>,
        provider: Option<crate::settings::ShortenerProvider>,
        long_url: &str,
    ) -> Result<String> {
        let manager = Arc::clone(&self.manager);
        let (shortener, stored) = self
            .managed(move || manager.shortener_credentials())
            .await?;
        if shortener.provider == crate::settings::ShortenerProvider::None {
            return Err(Error::Usage("no URL shortener is configured".into()));
        }
        let chosen = provider
            .filter(|p| *p != crate::settings::ShortenerProvider::None)
            .unwrap_or(shortener.provider);
        // The stored token is the one of the service in the settings.
        let token = if chosen == shortener.provider {
            token.filter(|t| !t.is_empty()).or(stored)
        } else {
            token.filter(|t| !t.is_empty())
        };
        let client = self.http_client().await?;
        crate::shorten::shorten_at(&client, endpoints, chosen, token.as_ref(), long_url).await
    }

    /// Asks `url` (normally [`crate::release::LATEST_RELEASE_URL`]) for the
    /// latest release of the project, through the proxy-aware client of the
    /// settings. Only the explicit check of `bb version --check` calls this;
    /// see [`crate::release`].
    pub async fn latest_release(&self, url: &str) -> Result<crate::release::LatestRelease> {
        let client = self.http_client().await?;
        crate::release::latest_release(&client, url).await
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
        let (account, _) = self.account_state(account_id).await?;
        let epoch = self.clients().epoch;
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        let (http_client, _) = self
            .account_http_client(&account, &config.settings.proxy, epoch)
            .await?;
        let (account, client, role) = self.mfa_login_client(account_id, http_client).await?;
        let session = if role {
            crate::creds::assume::request_role_session(&account, &client, code).await?
        } else {
            mfa::request_session(&account, &client, code, None).await?
        };
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
        let epoch = self.clients().epoch;
        let (http_client, _) = self
            .account_http_client(&account, &config.settings.proxy, epoch)
            .await?;
        let provider = self
            .provider_for(&config, &account, &http_client, &mut Vec::new())
            .await?;
        let mut options = ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client.clone());
        let mut builder =
            crate::cloudfront::cloudfront_config_with_provider(&account, provider, &options)?
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
        let mfa_session = match account.needs_mfa_session() {
            true => Some(self.mfa.active(&account)?),
            false => None,
        };
        let mfa_until = mfa_session.as_ref().map(|(_, until)| *until);
        let (http_client, http_generation) = self
            .account_http_client(&account, &config.settings.proxy, epoch)
            .await?;
        let key = S3Key {
            chain: self.source_chain(&config, &account),
            account,
            advanced: config.settings.advanced.clone(),
            data_integrity: config.settings.data_integrity.clone(),
            http_generation,
            mfa_until,
        };
        if let Some(s3) = self.cached_s3(account_id, &key) {
            return Ok(s3);
        }
        let mut options = ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client.clone());
        let (key, s3) = match mfa_session {
            Some((credentials, _)) => {
                let s3 = S3::new(&key.account, &credentials, &options)?;
                (key, s3)
            }
            None if key.account.has_static_keys() => {
                let manager = Arc::clone(&self.manager);
                let owned_id = account_id.to_owned();
                let (account, credentials) = self
                    .managed(move || manager.account_credentials(&owned_id))
                    .await?;
                let s3 = S3::new(&account, &credentials, &options)?;
                (S3Key { account, ..key }, s3)
            }
            None => {
                let provider = self
                    .provider_for(&config, &key.account, &http_client, &mut Vec::new())
                    .await?;
                let s3 = S3::with_provider(&key.account, provider, &options)?;
                (key, s3)
            }
        };
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

    /// Checks that the server accepts `credentials` as the keys of the
    /// stored account `account_id`, before they are saved: one signed
    /// request, sent the way the account's requests are (its endpoint,
    /// region and proxy). It lists the buckets, or one object of the first
    /// known bucket for an account that cannot list buckets (and succeeds
    /// without a request when it has none). Nothing is stored or cached.
    ///
    /// An unknown access key or a wrong secret fails with `auth`, as does an
    /// unreachable server with `remote`. An `AccessDenied` answer counts as
    /// accepted: the server recognised the keys, they are only not allowed
    /// to list.
    pub async fn verify_keys(
        &self,
        account_id: &str,
        credentials: &crate::client::Credentials,
    ) -> Result<()> {
        let epoch = self.clients().epoch;
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        let account = account_in(&config, account_id)?;
        if !account.has_static_keys() {
            return Err(Error::Usage(
                "an account that gets its credentials from another source has no keys to check"
                    .into(),
            ));
        }
        let (http_client, _) = self
            .account_http_client(&account, &config.settings.proxy, epoch)
            .await?;
        let mut options = ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client);
        let s3 = S3::new(&account, credentials, &options)?;
        let result = if account.options.cannot_list_buckets {
            match account.options.known_buckets.first() {
                Some(entry) => {
                    let (bucket, prefix) = crate::model::split_known_bucket(entry);
                    s3.list_objects(bucket, &prefix, true, None, 1)
                        .await
                        .map(|_| ())
                }
                None => Ok(()),
            }
        } else {
            s3.list_buckets().await.map(|_| ())
        };
        match result {
            Err(err)
                if err.service().and_then(|service| service.code()) == Some("AccessDenied") =>
            {
                Ok(())
            }
            other => other,
        }
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
        let epoch = self.clients().epoch;
        let (http_client, _) = self
            .account_http_client(&account, &config.settings.proxy, epoch)
            .await?;
        let provider = self
            .provider_for(&config, &account, &http_client, &mut Vec::new())
            .await?;
        let options = ClientOptions::from_settings(&config.settings);
        let mut builder = iam::iam_config_with_provider(&account, provider, &options)?;
        builder = builder.http_client(http_client);
        if let Some(hook) = &self.iam_config_hook {
            builder = hook(builder);
        }
        Ok(Iam::from_config(builder.build(), options.read_timeout))
    }

    /// The HTTP client of one account: the shared one of the app settings,
    /// or, when the account chose its own proxy (none, or a custom one), a
    /// client built for that choice and cached like the shared one. Such a
    /// client ignores the app's proxy and the proxy environment variables.
    async fn account_http_client(
        &self,
        account: &Account,
        global: &ProxySettings,
        epoch: u64,
    ) -> Result<(SharedHttpClient, u64)> {
        let Some(settings) = account.options.proxy.settings() else {
            return self.http_client_for(global, epoch).await;
        };
        let cache_key = format!("{settings:?}");
        if let Some((client, generation)) = self.clients().account_http.get(&cache_key) {
            return Ok((client.clone(), *generation));
        }
        let client = self
            .managed(move || {
                HttpClientBuilder::new()
                    .proxy(&settings, None)?
                    .environment_proxy(false)
                    .build()
            })
            .await?;
        let mut clients = self.clients();
        let generation = clients.next_generation;
        clients.next_generation += 1;
        if clients.epoch == epoch {
            clients
                .account_http
                .insert(cache_key, (client.clone(), generation));
        }
        Ok((client, generation))
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
        clients.account_http.clear();
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
                options: Default::default(),
                source: crate::model::CredentialSource::StaticKeys,
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
    async fn an_account_can_choose_its_own_proxy() {
        use crate::model::{AccountOptions, AccountProxy};
        use crate::settings::ProxyKind;
        let server = Recorder::start().await;
        let proxy = Recorder::start().await;
        let dead = Recorder::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let session = open(&dir, &store, options());

        // The app's proxy points at `dead`, which only records what it gets.
        let mut settings = session.settings().await.unwrap();
        settings.proxy.enabled = true;
        settings.proxy.host = "127.0.0.1".into();
        settings.proxy.port = dead.endpoint.rsplit(':').next().unwrap().parse().unwrap();
        session.save_settings(settings).await.unwrap();

        let with_proxy = |label: &str, proxy_choice: AccountProxy| {
            let mut input = server.input(label, None);
            input.options = AccountOptions {
                proxy: Box::new(proxy_choice),
                ..AccountOptions::default()
            };
            input
        };
        // `none` bypasses the app's proxy: the server gets the request.
        let direct = session
            .add_account(with_proxy("Direct", AccountProxy::None))
            .await
            .unwrap();
        let s3 = session.s3_for(&direct.id).await.unwrap();
        server.request_head(&s3).await;
        assert!(dead.heads.lock().unwrap().is_empty());

        // `global` uses the app's proxy (here the one that records).
        let global = session
            .add_account(with_proxy("Global", AccountProxy::Global))
            .await
            .unwrap();
        let s3 = session.s3_for(&global.id).await.unwrap();
        let head = dead.request_head(&s3).await;
        assert!(head.starts_with("get http://127.0.0.1:"), "{head}");

        // `custom` sends the account's requests to its own proxy.
        let port: u16 = proxy.endpoint.rsplit(':').next().unwrap().parse().unwrap();
        let custom = session
            .add_account(with_proxy(
                "Custom",
                AccountProxy::Custom {
                    kind: ProxyKind::Http,
                    host: "127.0.0.1".into(),
                    port,
                    bypass: Vec::new(),
                },
            ))
            .await
            .unwrap();
        let s3 = session.s3_for(&custom.id).await.unwrap();
        let head = proxy.request_head(&s3).await;
        assert!(head.starts_with("get http://127.0.0.1:"), "{head}");
        assert_eq!(server.heads.lock().unwrap().len(), 1);

        // An invalid custom proxy is refused when the account is saved.
        let bad = with_proxy(
            "Bad",
            AccountProxy::Custom {
                kind: ProxyKind::Http,
                host: String::new(),
                port: 8080,
                bypass: Vec::new(),
            },
        );
        assert_eq!(
            session.add_account(bad).await.unwrap_err().kind(),
            ErrorKind::Usage
        );
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
                options: Default::default(),
                source: crate::model::CredentialSource::StaticKeys,
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

    /// Credential sources other than static keys (see [`crate::creds`]).
    mod credential_sources {
        use aws_sdk_sso::operation::get_role_credentials::GetRoleCredentialsOutput;
        use aws_sdk_sso::operation::list_account_roles::ListAccountRolesOutput;
        use aws_sdk_sso::operation::list_accounts::ListAccountsOutput;
        use aws_sdk_sso::types::{AccountInfo, RoleCredentials, RoleInfo};
        use aws_sdk_ssooidc::operation::create_token::{CreateTokenError, CreateTokenOutput};
        use aws_sdk_ssooidc::operation::register_client::RegisterClientOutput;
        use aws_sdk_ssooidc::operation::start_device_authorization::StartDeviceAuthorizationOutput;
        use aws_sdk_sts::operation::assume_role::{AssumeRoleError, AssumeRoleOutput};

        use super::*;
        use crate::creds::sso::login::CLIENT_NAME;
        use crate::creds::{AwsFiles, CredsHooks};
        use crate::model::CredentialSource;

        const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/Writer";
        const ROLE_KEY: &str = "ASIAROLESESSIONKEY";
        const ROLE_SECRET: &str = "role-session-secret-value";

        fn coded(code: &str) -> aws_smithy_types::error::ErrorMetadata {
            aws_smithy_types::error::ErrorMetadata::builder()
                .code(code)
                .build()
        }

        fn assumed(key: &'static str) -> AssumeRoleOutput {
            AssumeRoleOutput::builder()
                .credentials(
                    StsCredentials::builder()
                        .access_key_id(key)
                        .secret_access_key(ROLE_SECRET)
                        .session_token("role-session-token")
                        .expiration(DateTime::from_secs(4_102_444_800))
                        .build()
                        .unwrap(),
                )
                .build()
        }

        fn assume_rule(key: &'static str) -> Rule {
            mock!(aws_sdk_sts::Client::assume_role)
                .match_requests(|input| {
                    input.role_arn() == Some(ROLE_ARN)
                        && input.role_session_name() == Some("bucketbridge")
                        && input.duration_seconds() == Some(3600)
                        && input.serial_number().is_none()
                })
                .then_output(move || assumed(key))
        }

        fn mfa_rules() -> (Rule, Rule) {
            let good = mock!(aws_sdk_sts::Client::assume_role)
                .match_requests(|input| {
                    input.serial_number() == Some(SERIAL) && input.token_code() == Some("123456")
                })
                .then_output(|| assumed(ROLE_KEY));
            let wrong = mock!(aws_sdk_sts::Client::assume_role)
                .match_requests(|input| input.token_code() == Some("000000"))
                .then_error(|| AssumeRoleError::generic(coded("AccessDenied")));
            (good, wrong)
        }

        fn sts_with(rules: &[&Rule]) -> SessionOptions {
            let rules: Vec<Rule> = rules.iter().map(|rule| (*rule).clone()).collect();
            let hook: StsConfigHook = Arc::new(move |builder| {
                let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
                for rule in &rules {
                    interceptor = interceptor.with_rule(rule);
                }
                builder
                    .interceptor(interceptor)
                    .http_client(create_mock_http_client())
            });
            SessionOptions {
                sts_config_hook: Some(hook),
                ..options()
            }
        }

        fn role_source(source_id: &str, mfa: Option<&str>) -> CredentialSource {
            CredentialSource::AssumeRole {
                source_account_id: source_id.to_owned(),
                role_arn: ROLE_ARN.to_owned(),
                session_name: "bucketbridge".to_owned(),
                duration_secs: 3600,
                external_id: None,
                mfa_serial: mfa.map(str::to_owned),
            }
        }

        /// A keyless account of `source` that sends its requests to the recorder.
        fn keyless(recorder: &Recorder, label: &str, source: CredentialSource) -> AccountInput {
            let mut input = recorder.input(label, None);
            input.access_key_id = String::new();
            input.secret_access_key = String::new();
            input.source = source;
            input
        }

        fn config_text(dir: &tempfile::TempDir) -> String {
            std::fs::read_to_string(Dirs::under(dir.path()).config_file()).unwrap()
        }

        #[tokio::test]
        async fn an_assumed_role_signs_with_the_role_session() {
            let recorder = Recorder::start().await;
            let rule = assume_rule(ROLE_KEY);
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sts_with(&[&rule]));
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let role = session
                .add_account(keyless(&recorder, "Role", role_source(&base.id, None)))
                .await
                .unwrap();
            // The role keeps no key of its own.
            assert_eq!(role.access_key_id, "");
            assert_eq!(role.secret_ref, "");
            let s3 = session.s3_for(&role.id).await.unwrap();
            let head = recorder.request_head(&s3).await;
            assert!(signed_by(&head, ROLE_KEY), "{head}");
            assert!(head.contains("x-amz-security-token"));
            assert!(!signed_by(&head, ACCESS_KEY));
            // The handle and the session are reused.
            let again = session.s3_for(&role.id).await.unwrap();
            recorder.request_head(&again).await;
            assert_eq!(rule.num_calls(), 1);
            let config = config_text(&dir);
            assert!(!config.contains(ROLE_SECRET) && !config.contains(SECRET));
            // Only the base account's two secrets are in the keychain.
            assert_eq!(store.len(), 1);
        }

        #[tokio::test]
        async fn a_rotated_source_key_rebuilds_the_role() {
            let recorder = Recorder::start().await;
            let rule = assume_rule(ROLE_KEY);
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sts_with(&[&rule]));
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let role = session
                .add_account(keyless(&recorder, "Role", role_source(&base.id, None)))
                .await
                .unwrap();
            recorder
                .request_head(&session.s3_for(&role.id).await.unwrap())
                .await;
            session
                .update_account(
                    &base.id,
                    updating(|update| update.secret_access_key = Some(ROTATED.to_owned())),
                )
                .await
                .unwrap();
            recorder
                .request_head(&session.s3_for(&role.id).await.unwrap())
                .await;
            assert_eq!(rule.num_calls(), 2);
        }

        #[tokio::test]
        async fn a_source_in_use_cannot_be_removed() {
            let recorder = Recorder::start().await;
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, options());
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let role = session
                .add_account(keyless(&recorder, "Role", role_source(&base.id, None)))
                .await
                .unwrap();
            let error = session.remove_account(&base.id).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Conflict);
            assert!(error.to_string().contains("Role"));
            session.remove_account(&role.id).await.unwrap();
            session.remove_account(&base.id).await.unwrap();
            assert!(store.is_empty());
        }

        #[tokio::test]
        async fn a_role_needs_an_existing_source_and_valid_settings() {
            let recorder = Recorder::start().await;
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, options());
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let missing = session
                .add_account(keyless(&recorder, "A", role_source("nope", None)))
                .await
                .unwrap_err();
            assert_eq!(missing.kind(), ErrorKind::NotFound);
            let bad_arn = CredentialSource::AssumeRole {
                source_account_id: base.id.clone(),
                role_arn: "not-an-arn".into(),
                session_name: "bucketbridge".into(),
                duration_secs: 3600,
                external_id: None,
                mfa_serial: None,
            };
            let error = session
                .add_account(keyless(&recorder, "B", bad_arn))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage);
            for duration_secs in [899, 43_201] {
                let source = CredentialSource::AssumeRole {
                    source_account_id: base.id.clone(),
                    role_arn: ROLE_ARN.into(),
                    session_name: "bucketbridge".into(),
                    duration_secs,
                    external_id: None,
                    mfa_serial: None,
                };
                let error = session
                    .add_account(keyless(&recorder, "C", source))
                    .await
                    .unwrap_err();
                assert_eq!(error.kind(), ErrorKind::Usage, "{duration_secs}");
            }
            for duration_secs in [900, 43_200] {
                let source = CredentialSource::AssumeRole {
                    source_account_id: base.id.clone(),
                    role_arn: ROLE_ARN.into(),
                    session_name: "bucketbridge".into(),
                    duration_secs,
                    external_id: Some("ext-id".into()),
                    mfa_serial: None,
                };
                session
                    .add_account(keyless(&recorder, &format!("D{duration_secs}"), source))
                    .await
                    .unwrap();
            }
            // Keys cannot be given to a keyless account.
            let mut with_key = keyless(&recorder, "E", role_source(&base.id, None));
            with_key.secret_access_key = SECRET.to_owned();
            assert_eq!(
                session.add_account(with_key).await.unwrap_err().kind(),
                ErrorKind::Usage
            );
        }

        #[tokio::test]
        async fn a_role_with_a_device_needs_a_code_and_the_session_ends() {
            let recorder = Recorder::start().await;
            let (good, wrong) = mfa_rules();
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sts_with(&[&good, &wrong]));
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let role = session
                .add_account(keyless(
                    &recorder,
                    "Role",
                    role_source(&base.id, Some(SERIAL)),
                ))
                .await
                .unwrap();
            assert_eq!(role.mfa_serial.as_deref(), Some(SERIAL));
            let error = session.s3_for(&role.id).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::MfaRequired);
            let error = session.mfa_login(&role.id, "000000").await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Auth, "{error}");
            assert_eq!(
                session.s3_for(&role.id).await.unwrap_err().kind(),
                ErrorKind::MfaRequired
            );
            session.mfa_login(&role.id, "123456").await.unwrap();
            let s3 = session.s3_for(&role.id).await.unwrap();
            let head = recorder.request_head(&s3).await;
            assert!(signed_by(&head, ROLE_KEY), "{head}");
            // Changing the role's settings drops the session.
            let updated = session
                .update_account(
                    &role.id,
                    updating(|update| {
                        update.source = Some(CredentialSource::AssumeRole {
                            source_account_id: base.id.clone(),
                            role_arn: ROLE_ARN.into(),
                            session_name: "other-name".into(),
                            duration_secs: 3600,
                            external_id: None,
                            mfa_serial: Some(SERIAL.into()),
                        });
                    }),
                )
                .await
                .unwrap();
            assert!(matches!(
                updated.source,
                CredentialSource::AssumeRole { ref session_name, .. } if session_name == "other-name"
            ));
            assert_eq!(
                session.s3_for(&role.id).await.unwrap_err().kind(),
                ErrorKind::MfaRequired
            );
            // A static account cannot change into another kind of source.
            let error = session
                .update_account(
                    &base.id,
                    updating(|update| update.source = Some(CredentialSource::InstanceRole)),
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage);
        }

        #[tokio::test]
        async fn a_locked_vault_comes_before_everything_else() {
            let recorder = Recorder::start().await;
            let (good, wrong) = mfa_rules();
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sts_with(&[&good, &wrong]));
            let base = session
                .add_account(recorder.input("Base", None))
                .await
                .unwrap();
            let role = session
                .add_account(keyless(
                    &recorder,
                    "Role",
                    role_source(&base.id, Some(SERIAL)),
                ))
                .await
                .unwrap();
            session
                .set_master_password(None, Some(PASSWORD))
                .await
                .unwrap();
            session.lock();
            assert_eq!(
                session.s3_for(&role.id).await.unwrap_err().kind(),
                ErrorKind::Locked
            );
            session.unlock(PASSWORD).await.unwrap();
            assert_eq!(
                session.s3_for(&role.id).await.unwrap_err().kind(),
                ErrorKind::MfaRequired
            );
        }

        fn aws_files_in(dir: &tempfile::TempDir) -> AwsFiles {
            let aws = dir.path().join("aws");
            std::fs::create_dir_all(&aws).unwrap();
            std::fs::write(
                aws.join("credentials"),
                "[dev]\naws_access_key_id = AKIAPROFILEKEY\naws_secret_access_key = profile-secret-value\n\n[other]\naws_access_key_id = AKIAOTHERKEY\naws_secret_access_key = other-secret-value\n",
            )
            .unwrap();
            std::fs::write(
                aws.join("config"),
                "[profile dev]\nregion = eu-west-2\n\n[profile broken]\nregion = us-east-1\n",
            )
            .unwrap();
            AwsFiles::in_dir(&aws)
        }

        fn with_files(files: AwsFiles) -> SessionOptions {
            SessionOptions {
                creds_hooks: CredsHooks {
                    aws_files: Some(files),
                    ..CredsHooks::default()
                },
                ..options()
            }
        }

        #[tokio::test]
        async fn an_aws_profile_account_signs_with_the_profile_and_stores_no_secret() {
            let recorder = Recorder::start().await;
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, with_files(aws_files_in(&dir)));
            let account = session
                .add_account(keyless(
                    &recorder,
                    "Dev",
                    CredentialSource::AwsProfile {
                        profile: "dev".into(),
                    },
                ))
                .await
                .unwrap();
            let s3 = session.s3_for(&account.id).await.unwrap();
            let head = recorder.request_head(&s3).await;
            assert!(signed_by(&head, "AKIAPROFILEKEY"), "{head}");
            assert!(store.is_empty());
            let config = config_text(&dir);
            assert!(!config.contains("profile-secret-value") && !config.contains("AKIAPROFILEKEY"));
        }

        #[tokio::test]
        async fn a_missing_profile_is_an_auth_error_when_used() {
            let recorder = Recorder::start().await;
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, with_files(aws_files_in(&dir)));
            let account = session
                .add_account(keyless(
                    &recorder,
                    "Gone",
                    CredentialSource::AwsProfile {
                        profile: "gone".into(),
                    },
                ))
                .await
                .unwrap();
            let s3 = session.s3_for(&account.id).await.unwrap();
            let error = s3.list_buckets().await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Auth, "{error}");
        }

        #[tokio::test]
        async fn importing_profiles_links_them_without_copying_secrets() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, with_files(aws_files_in(&dir)));
            // A label that is taken already.
            let recorder = Recorder::start().await;
            session
                .add_account(recorder.input("dev", None))
                .await
                .unwrap();
            let listed = session.list_aws_profiles().await.unwrap();
            let names: Vec<&str> = listed.iter().map(|e| e.profile.name.as_str()).collect();
            assert_eq!(names, ["broken", "dev", "other"]);
            assert!(!listed[0].profile.importable);
            let secrets_before = store.len();
            let outcome = session
                .import_aws_profiles(&[
                    "dev".to_owned(),
                    "other".to_owned(),
                    "broken".to_owned(),
                    "nope".to_owned(),
                    "dev".to_owned(),
                ])
                .await
                .unwrap();
            let labels: Vec<&str> = outcome.imported.iter().map(|a| a.label.as_str()).collect();
            assert_eq!(labels, ["dev (AWS)", "other"]);
            assert_eq!(outcome.imported[0].region, "eu-west-2");
            assert_eq!(outcome.skipped.len(), 2);
            assert_eq!(store.len(), secrets_before);
            let config = config_text(&dir);
            assert!(!config.contains("secret-value") && !config.contains("AKIAOTHERKEY"));
            // Importing again links nothing new.
            let again = session
                .import_aws_profiles(&["dev".to_owned()])
                .await
                .unwrap();
            assert!(again.imported.is_empty());
            assert!(again.skipped[0].reason.contains("already"));
            let listed = session.list_aws_profiles().await.unwrap();
            assert!(listed[1].account_id.is_some());
        }

        #[tokio::test]
        async fn a_profile_can_be_used_without_being_stored() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, with_files(aws_files_in(&dir)));
            session
                .s3_for_profile("dev", &ProfileOptions::default())
                .await
                .unwrap();
            let error = session
                .s3_for_profile("nope", &ProfileOptions::default())
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::NotFound);
            assert!(session.list_accounts().await.unwrap().accounts.is_empty());
        }

        #[tokio::test]
        async fn the_instance_role_signs_requests_and_the_probe_reports_it() {
            let recorder = Recorder::start().await;
            let imds = crate::creds::instance::testing::imds(Some("my-role"));
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(
                &dir,
                &store,
                SessionOptions {
                    creds_hooks: CredsHooks {
                        imds_endpoint: Some(imds.url.clone()),
                        ..CredsHooks::default()
                    },
                    ..options()
                },
            );
            let found = session.probe_instance_role().await.unwrap();
            assert_eq!(found.access_key_id, "ASIAINSTANCE");
            let account = session
                .add_account(keyless(&recorder, "Box", CredentialSource::InstanceRole))
                .await
                .unwrap();
            let s3 = session.s3_for(&account.id).await.unwrap();
            let head = recorder.request_head(&s3).await;
            assert!(signed_by(&head, "ASIAINSTANCE"), "{head}");
            assert!(store.is_empty());
        }

        #[tokio::test]
        async fn the_probe_fails_clearly_off_ec2() {
            let imds = crate::creds::instance::testing::imds(None);
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(MemoryStore::new());
            let session = open(
                &dir,
                &store,
                SessionOptions {
                    creds_hooks: CredsHooks {
                        imds_endpoint: Some(imds.url.clone()),
                        ..CredsHooks::default()
                    },
                    ..options()
                },
            );
            let error = session.probe_instance_role().await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Auth);
            assert!(error.to_string().contains("not an EC2 instance"), "{error}");
        }

        const START_URL: &str = "https://example.awsapps.com/start";

        fn sso_hooks(files: AwsFiles) -> SessionOptions {
            let register = mock!(aws_sdk_ssooidc::Client::register_client)
                .match_requests(|input| input.client_name() == Some(CLIENT_NAME))
                .then_output(|| {
                    RegisterClientOutput::builder()
                        .client_id("client-1")
                        .client_secret("client-secret-1")
                        .client_secret_expires_at(4_102_444_800)
                        .build()
                });
            let start =
                mock!(aws_sdk_ssooidc::Client::start_device_authorization).then_output(|| {
                    StartDeviceAuthorizationOutput::builder()
                        .device_code("device-code-1")
                        .user_code("WXYZ-1234")
                        .verification_uri("https://device.example.com/")
                        .verification_uri_complete(
                            "https://device.example.com/?user_code=WXYZ-1234",
                        )
                        .expires_in(600)
                        .interval(5)
                        .build()
                });
            let token = mock!(aws_sdk_ssooidc::Client::create_token)
                .sequence()
                .error(|| CreateTokenError::generic(coded("AuthorizationPendingException")))
                .output(|| {
                    CreateTokenOutput::builder()
                        .access_token("sso-access-token-1")
                        .refresh_token("sso-refresh-token-1")
                        .expires_in(28_800)
                        .build()
                })
                .build();
            let accounts = mock!(aws_sdk_sso::Client::list_accounts)
                .match_requests(|input| input.access_token() == Some("sso-access-token-1"))
                .then_output(|| {
                    ListAccountsOutput::builder()
                        .account_list(
                            AccountInfo::builder()
                                .account_id("123456789012")
                                .account_name("Production")
                                .build(),
                        )
                        .build()
                });
            let roles = mock!(aws_sdk_sso::Client::list_account_roles)
                .match_requests(|input| input.account_id() == Some("123456789012"))
                .then_output(|| {
                    ListAccountRolesOutput::builder()
                        .role_list(
                            RoleInfo::builder()
                                .role_name("Admin")
                                .account_id("123456789012")
                                .build(),
                        )
                        .build()
                });
            let credentials = mock!(aws_sdk_sso::Client::get_role_credentials)
                .match_requests(|input| {
                    input.role_name() == Some("Admin")
                        && input.account_id() == Some("123456789012")
                        && input.access_token() == Some("sso-access-token-1")
                })
                .then_output(|| {
                    GetRoleCredentialsOutput::builder()
                        .role_credentials(
                            RoleCredentials::builder()
                                .access_key_id("ASIASSOKEY")
                                .secret_access_key("sso-role-secret")
                                .session_token("sso-role-token")
                                .expiration(4_102_444_800_000)
                                .build(),
                        )
                        .build()
                });
            let oidc_rules = vec![register, start, token];
            let sso_rules = vec![accounts, roles, credentials];
            let oidc_hook: crate::creds::SsoOidcConfigHook = Arc::new(move |builder| {
                let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
                for rule in &oidc_rules {
                    interceptor = interceptor.with_rule(rule);
                }
                builder
                    .interceptor(interceptor)
                    .http_client(create_mock_http_client())
            });
            let sso_hook: crate::creds::SsoConfigHook = Arc::new(move |builder| {
                let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
                for rule in &sso_rules {
                    interceptor = interceptor.with_rule(rule);
                }
                builder
                    .interceptor(interceptor)
                    .http_client(create_mock_http_client())
            });
            SessionOptions {
                creds_hooks: CredsHooks {
                    aws_files: Some(files),
                    sso_oidc: Some(oidc_hook),
                    sso: Some(sso_hook),
                    ..CredsHooks::default()
                },
                ..options()
            }
        }

        #[tokio::test]
        async fn sso_sign_in_to_signed_requests() {
            let recorder = Recorder::start().await;
            let dir = tempfile::tempdir().unwrap();
            let files = AwsFiles::in_dir(&dir.path().join("aws"));
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sso_hooks(files.clone()));

            // Not signed in yet.
            let status = session
                .sso_status(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap();
            assert!(!status.signed_in);
            let error = session
                .sso_list_accounts(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Auth);

            let started = session
                .sso_start_login(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap();
            assert_eq!(started.user_code, "WXYZ-1234");
            assert_eq!(started.verification_uri, "https://device.example.com/");
            assert_eq!(started.interval_secs, 5);
            assert!(!format!("{started:?}").contains("device-code-1"));
            let first = session.sso_poll_login(&started.login_id).await.unwrap();
            assert_eq!(first, SsoLoginPoll::Pending { interval_secs: 5 });
            let done = session.sso_poll_login(&started.login_id).await.unwrap();
            assert!(matches!(done, SsoLoginPoll::Complete { .. }));
            // The sign-in has ended.
            assert_eq!(
                session
                    .sso_poll_login(&started.login_id)
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::NotFound
            );

            // The token is in the AWS CLI cache, and only there.
            let key = crate::creds::sso::CacheKey::new(START_URL, Some("corp"));
            let cache_file = key.path(files.sso_cache());
            let cached = std::fs::read_to_string(&cache_file).unwrap();
            assert!(cached.contains("sso-access-token-1"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&cache_file).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            assert!(store.is_empty());
            assert!(!config_text_if_any(&dir).contains("sso-access-token-1"));

            let accounts = session
                .sso_list_accounts(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap();
            assert_eq!(accounts[0].account_id, "123456789012");
            let roles = session
                .sso_list_roles(START_URL, "eu-west-1", Some("corp"), "123456789012")
                .await
                .unwrap();
            assert_eq!(roles, ["Admin"]);

            let account = session
                .add_account(keyless(
                    &recorder,
                    "Prod",
                    CredentialSource::Sso {
                        start_url: START_URL.into(),
                        sso_region: "eu-west-1".into(),
                        account_id: "123456789012".into(),
                        role_name: "Admin".into(),
                        sso_session: Some("corp".into()),
                    },
                ))
                .await
                .unwrap();
            let s3 = session.s3_for(&account.id).await.unwrap();
            let head = recorder.request_head(&s3).await;
            assert!(signed_by(&head, "ASIASSOKEY"), "{head}");
            let config = config_text(&dir);
            assert!(!config.contains("sso-access-token-1") && !config.contains("sso-role-secret"));

            let status = session
                .sso_status(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap();
            assert!(status.signed_in && status.can_refresh);
            session
                .sso_sign_out(START_URL, "eu-west-1", Some("corp"))
                .await
                .unwrap();
            assert!(!cache_file.exists());
        }

        fn config_text_if_any(dir: &tempfile::TempDir) -> String {
            std::fs::read_to_string(Dirs::under(dir.path()).config_file()).unwrap_or_default()
        }

        #[tokio::test]
        async fn a_cancelled_sign_in_cannot_be_polled() {
            let dir = tempfile::tempdir().unwrap();
            let files = AwsFiles::in_dir(&dir.path().join("aws"));
            let store = Arc::new(MemoryStore::new());
            let session = open(&dir, &store, sso_hooks(files));
            let started = session
                .sso_start_login(START_URL, "eu-west-1", None)
                .await
                .unwrap();
            session.sso_cancel_login(&started.login_id);
            assert_eq!(
                session
                    .sso_poll_login(&started.login_id)
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::NotFound
            );
            assert_eq!(
                session
                    .sso_start_login("http://insecure.example.com", "eu-west-1", None)
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::Usage
            );
        }
    }

    #[test]
    fn handles_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Session>();
        assert_send_sync::<AccountManager>();
    }
}
