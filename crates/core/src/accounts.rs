//! Accounts, their secrets and the master password: the [`AccountManager`].
//!
//! The account manager ties `config.toml` ([`ConfigStore`]) to the secret
//! store ([`SecretStore`]) through the master-password [`Vault`]:
//!
//! - accounts and every non-secret field live in `config.toml`;
//! - secret access keys, static session tokens and the proxy password live in
//!   the secret store, under the references (`*_ref`) the configuration
//!   holds (naming scheme in [`crate::secrets`]);
//! - `config.toml` is written only through [`ConfigStore::update`].
//!
//! # Crash safety and concurrency
//!
//! Every operation that changes secrets holds two locks for its whole
//! duration: an in-process lock that also keeps readers from seeing a
//! half-applied change, and the advisory file lock `secrets.lock` in the
//! config directory, so that the desktop app and the CLI never interleave
//! their changes. A change then runs:
//!
//! 1. it records every reference it will write or make obsolete in
//!    `secrets-journal.json`;
//! 2. the vault writes the new values under new references
//!    ([`Vault::prepare_secret_changes`] and friends);
//! 3. the configuration is persisted through [`ConfigStore::update`];
//! 4. the vault change is finalized, which deletes the old values;
//! 5. journal references that the persisted configuration does not
//!    reference are deleted from the store.
//!
//! The persisted configuration is the only source of truth: after a crash at
//! any point it references values that exist, and every value it does not
//! reference is listed in the journal and removed by
//! [`AccountManager::purge_leftovers`] (which [`crate::Session`] runs at
//! startup). If [`ConfigStore::update`] reports an error after possibly
//! writing, the file is read back to decide whether the change took effect.
//!
//! The journal is written, flushed and renamed into place, and the directory
//! is flushed too, before the first value is written. A journal that cannot be
//! read is never discarded silently: it is moved aside
//! (`secrets-journal.json.damaged-<n>`) and reported, because the leftovers it
//! listed (for example plaintext copies after an interrupted removal of the
//! master password) can no longer be found automatically.
//!
//! # References are checked against their use
//!
//! `config.toml` is not integrity protected, so a reference it holds is only
//! followed for the purpose it is stored for: an account's secret and session
//! token must be owned by that account and sit in their own slot, the proxy
//! password must be the proxy password of this installation
//! ([`crate::secrets::proxy_owner`], or the legacy shared owner of
//! configurations from before installation ids). Anything else is rejected
//! with a `usage` error before it is read, and is never deleted. A master
//! password change refuses to run while one exists, so no secret is left out
//! of it unnoticed. Otherwise editing the file could make the proxy (or an
//! endpoint) receive another account's secret.
//!
//! # Installations
//!
//! Every configuration has a random installation id (see
//! [`crate::config`]). The proxy password lives under an owner derived from
//! it, so configurations that share one OS keychain never overwrite or delete
//! each other's items; account secrets are owned by the account's random id.
//! A value for a slot that holds none yet is never written over an existing
//! item. A configuration from before ids existed keeps working: its legacy
//! proxy password reference is still read, and
//! [`AccountManager::migrate_legacy_references`] (run when a session opens
//! and when the secrets are unlocked) moves it to the installation's own
//! reference with the same two-phase protocol.
//!
//! # Blocking
//!
//! Every method blocks (keychain, key derivation, file locks). Async code
//! calls them through `tokio::task::spawn_blocking`, as [`crate::Session`]
//! does.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind as IoErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde::{Deserialize, Serialize};

use crate::client::{Credentials, validate_endpoint, validate_region};
use crate::config::{AppConfig, CONFIG_VERSION, ConfigStore, new_uuid_v4, sync_dir};
use crate::error::{Error, IoContext, Result};
use crate::mfa::validate_mfa_serial;
use crate::model::{
    Account, AccountInput, AccountList, AccountUpdate, CredentialSource, MAX_ROLE_DURATION_SECS,
    MIN_ROLE_DURATION_SECS, SecurityStatus,
};
use crate::proxy::validate_proxy_settings;
use crate::secrets::{
    KdfCost, PROXY_OWNER, PendingChange, SLOT_PROXY_PASSWORD, SLOT_SECRET, SLOT_SESSION_TOKEN,
    SLOT_SHORTENER_TOKEN, SecretChange, SecretRef, SecretStore, SecretString, SecurityUpdate,
    Vault, proxy_owner, shortener_owner,
};
use crate::settings::{ProxySettings, Settings, ShortenerProvider, ShortenerSettings};

/// File in the config directory that serializes secret changes across
/// processes.
pub const SECRETS_LOCK_FILE: &str = "secrets.lock";

/// File in the config directory that lists references which may be left
/// over by an interrupted change. It holds references only, never secrets.
pub const SECRETS_JOURNAL_FILE: &str = "secrets-journal.json";

const JOURNAL_VERSION: u32 = 1;
const MAX_LABEL_CHARS: usize = 200;
const MAX_SECRET_BYTES: usize = 4096;
const MAX_PROXY_PASSWORD_BYTES: usize = 1024;
/// How many generations are tried when looking for a free reference.
const MAX_UNUSED_PROBES: u32 = 16;
/// How many damaged journals are kept next to the journal.
const MAX_DAMAGED_JOURNALS: u32 = 100;

/// Accounts, their secrets and the master password. See the
/// [module docs](self).
pub struct AccountManager {
    config: ConfigStore,
    store: Arc<dyn SecretStore>,
    vault: Vault,
    lock_path: PathBuf,
    journal_path: PathBuf,
    /// Changes hold it exclusively, consistent reads shared.
    changes: RwLock<()>,
}

impl AccountManager {
    /// Opens the configuration and the secret store; a protected vault
    /// starts locked.
    pub fn open(config: ConfigStore, store: Arc<dyn SecretStore>) -> Result<Self> {
        Self::with_kdf_cost(config, store, KdfCost::RECOMMENDED)
    }

    /// Like [`AccountManager::open`], with the Argon2id cost used for master
    /// passwords set from now on.
    pub fn with_kdf_cost(
        config: ConfigStore,
        store: Arc<dyn SecretStore>,
        kdf_cost: KdfCost,
    ) -> Result<Self> {
        let current = config.load()?;
        let vault = Vault::with_kdf_cost(Arc::clone(&store), current.security, kdf_cost)?;
        let dir = parent_dir(config.path()).to_path_buf();
        Ok(Self {
            lock_path: dir.join(SECRETS_LOCK_FILE),
            journal_path: dir.join(SECRETS_JOURNAL_FILE),
            config,
            store,
            vault,
            changes: RwLock::new(()),
        })
    }

    /// The configuration store.
    pub fn config_store(&self) -> &ConfigStore {
        &self.config
    }

    /// The master-password vault.
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// Loads `config.toml` and adopts a master-password change another
    /// process made meanwhile (the vault then locks).
    pub fn load(&self) -> Result<AppConfig> {
        let config = self.config.load()?;
        self.vault.sync_config(config.security.clone())?;
        Ok(config)
    }

    /// Every account and the default account. Works while locked.
    pub fn list(&self) -> Result<AccountList> {
        let config = self.config.load()?;
        Ok(AccountList {
            accounts: config.accounts,
            default_account_id: config.default_account_id,
        })
    }

    /// One account.
    pub fn get(&self, id: &str) -> Result<Account> {
        find_account(&self.config.load()?, id).cloned()
    }

    /// Whether a master password is set and whether secrets are readable.
    pub fn security_status(&self) -> Result<SecurityStatus> {
        self.load()?;
        Ok(self.vault.status())
    }

    /// Unlocks the secrets. A wrong password is an auth error and changes
    /// nothing. Fails with `conflict` while the master-password section of
    /// `config.toml` is missing (see [`Vault::sync_config`]). A configuration
    /// from before installation ids moves its proxy password to its own
    /// reference now that the key is there
    /// ([`AccountManager::migrate_legacy_references`]); unlocking succeeds
    /// whether or not that works.
    pub fn unlock(&self, password: &str) -> Result<SecurityStatus> {
        self.load()?;
        self.vault.unlock(password)?;
        let _migration = self.migrate_legacy_references();
        Ok(self.vault.status())
    }

    /// Locks the secrets again (the key is dropped).
    pub fn lock(&self) -> SecurityStatus {
        self.vault.lock();
        self.vault.status()
    }

    /// The account and its long-term credentials, read together so that a
    /// concurrent rotation is either fully seen or not at all. Fails with
    /// `locked` while the secrets are locked.
    pub fn account_credentials(&self, id: &str) -> Result<(Account, Credentials)> {
        let _reading = self.read_guard();
        let config = self.load()?;
        let account = find_account(&config, id)?.clone();
        let credentials = self.read_credentials(&account)?;
        Ok((account, credentials))
    }

    /// The proxy settings and, when they authenticate (enabled, with a user
    /// name and a stored password), the proxy password. Fails with `usage`
    /// when `password_ref` is not the proxy password of this installation (an
    /// edited `config.toml`): no other secret is ever read for the proxy.
    pub fn proxy_credentials(&self) -> Result<(ProxySettings, Option<SecretString>)> {
        let _reading = self.read_guard();
        let config = self.load()?;
        let installation_id = config.installation_id;
        let proxy = config.settings.proxy;
        let password = match (proxy.enabled, &proxy.username, &proxy.password_ref) {
            (true, Some(_), Some(reference)) => {
                let reference = proxy_ref(installation_id.as_deref(), reference)?;
                Some(
                    self.vault
                        .read(&reference)
                        .map_err(|e| e.with_context("proxy password"))?,
                )
            }
            _ => None,
        };
        Ok((proxy, password))
    }

    /// Adds an account and stores its secrets. The first account becomes the
    /// default. Fails with `conflict` when the label is taken
    /// (case-insensitively) and with `locked` while a master password is set
    /// and locked.
    pub fn add(&self, input: &AccountInput) -> Result<Account> {
        let label = check_label(&input.label)?;
        let session_token = non_empty(input.session_token.as_deref());
        if input.source.is_static() {
            check_access_key(&input.access_key_id)?;
            check_secret("secret access key", &input.secret_access_key)?;
            if let Some(token) = session_token {
                check_secret("session token", token)?;
            }
        } else if !input.access_key_id.trim().is_empty()
            || !input.secret_access_key.is_empty()
            || session_token.is_some()
        {
            return Err(Error::Usage(
                "an account that gets its credentials from another source has no access key, \
                 secret key or session token of its own"
                    .into(),
            ));
        }
        validate_region(&input.region)?;
        let endpoint = non_empty(input.endpoint.as_deref());
        if let Some(endpoint) = endpoint {
            validate_endpoint(endpoint)?;
        }
        let mut mfa_serial = non_empty(input.mfa_serial.as_deref());
        if let Some(serial) = mfa_serial {
            validate_mfa_serial(serial)?;
        }

        let _change = self.change_guard()?;
        let config = self.load()?;
        check_label_free(&config, label, None)?;
        if input.source.is_static() {
            self.require_unlocked()?;
        } else {
            check_source(&config, &input.source, None)?;
            // The device of an assumed role is the role's; no other source
            // can use one.
            match (input.source.role_mfa_serial(), mfa_serial) {
                (Some(_), _) => mfa_serial = input.source.role_mfa_serial(),
                (None, Some(_)) => {
                    return Err(Error::Usage(
                        "an MFA device applies to static keys and to assumed roles only".into(),
                    ));
                }
                (None, None) => {}
            }
        }

        let id = new_uuid_v4()?;
        let (secret_ref, token_ref) = if input.source.is_static() {
            (
                Some(SecretRef::first(&id, SLOT_SECRET)?),
                session_token
                    .map(|_| SecretRef::first(&id, SLOT_SESSION_TOKEN))
                    .transpose()?,
            )
        } else {
            (None, None)
        };
        let account = Account {
            id,
            label: label.to_owned(),
            access_key_id: input.access_key_id.trim().to_owned(),
            secret_ref: secret_ref
                .as_ref()
                .map(SecretRef::to_string)
                .unwrap_or_default(),
            session_token_ref: token_ref.as_ref().map(SecretRef::to_string),
            region: input.region.clone(),
            endpoint: endpoint.map(str::to_owned),
            path_style: input.path_style,
            request_payer: input.request_payer,
            mfa_serial: mfa_serial.map(str::to_owned),
            source: input.source.clone(),
        };

        let mut changes = Vec::new();
        if let Some(reference) = secret_ref {
            changes.push(SecretChange::Put {
                reference,
                replaces: None,
                value: SecretString::from(input.secret_access_key.as_str()),
            });
        }
        if let (Some(reference), Some(token)) = (token_ref, session_token) {
            changes.push(SecretChange::Put {
                reference,
                replaces: None,
                value: SecretString::from(token),
            });
        }
        let planned = planned_references(&changes);
        let insert = |config: &mut AppConfig| -> Result<()> {
            check_label_free(config, &account.label, None)?;
            if !account.source.is_static() {
                check_source(config, &account.source, None)?;
            }
            if config.accounts.iter().any(|a| a.id == account.id) {
                return Err(Error::Internal("generated a duplicate account id".into()));
            }
            config.accounts.push(account.clone());
            if config.default_account_id.is_none() {
                config.default_account_id = Some(account.id.clone());
            }
            Ok(())
        };
        if changes.is_empty() {
            self.persist(insert)?;
        } else {
            self.apply_change(
                |vault| vault.prepare_secret_changes(changes),
                &planned,
                |config, _| insert(config),
            )?;
        }
        Ok(account)
    }

    /// Updates an account; `None` fields keep their value. A new secret
    /// access key or session token is written under the next generation of
    /// its reference and the old value is deleted once the configuration is
    /// saved (key rotation).
    pub fn update(&self, id: &str, update: &AccountUpdate) -> Result<Account> {
        check_update(update)?;
        let _change = self.change_guard()?;
        let config = self.load()?;
        let current = find_account(&config, id)?.clone();
        let mut updated = current.clone();

        if let Some(label) = &update.label {
            let label = check_label(label)?;
            check_label_free(&config, label, Some(id))?;
            updated.label = label.to_owned();
        }
        if let Some(access_key_id) = &update.access_key_id {
            updated.access_key_id = access_key_id.trim().to_owned();
        }
        if let Some(region) = &update.region {
            updated.region = region.clone();
        }
        if update.clear_endpoint {
            updated.endpoint = None;
        } else if let Some(endpoint) = non_empty(update.endpoint.as_deref()) {
            updated.endpoint = Some(endpoint.to_owned());
        }
        if let Some(path_style) = update.path_style {
            updated.path_style = path_style;
        }
        if let Some(request_payer) = update.request_payer {
            updated.request_payer = request_payer;
        }
        if update.clear_mfa_serial {
            updated.mfa_serial = None;
        } else if let Some(serial) = non_empty(update.mfa_serial.as_deref()) {
            updated.mfa_serial = Some(serial.to_owned());
        }
        if let Some(source) = &update.source {
            if current.source.is_static()
                || std::mem::discriminant(&current.source) != std::mem::discriminant(source)
            {
                return Err(Error::Usage(
                    "the kind of credential source of an account cannot change".into(),
                ));
            }
            check_source(&config, source, Some(id))?;
            updated.source = source.clone();
            updated.mfa_serial = source.role_mfa_serial().map(str::to_owned);
        }
        if !updated.source.is_static()
            && (update.secret_access_key.is_some()
                || update.session_token.is_some()
                || update.access_key_id.is_some())
        {
            return Err(Error::Usage(
                "an account that gets its credentials from another source has no keys to change"
                    .into(),
            ));
        }

        // A reference that is not this account's own (an edited config.toml)
        // is replaced by a new one, never followed or deleted: what it used to
        // point at stays in the keychain.
        let mut changes = Vec::new();
        if let Some(secret) = &update.secret_access_key {
            let old = account_ref(&current, &current.secret_ref, SLOT_SECRET).ok();
            let (new, replaces) = match old {
                Some(old) => (old.next()?, Some(old)),
                None => (self.unused_reference(&current.id, SLOT_SECRET)?, None),
            };
            updated.secret_ref = new.to_string();
            changes.push(SecretChange::Put {
                reference: new,
                replaces,
                value: SecretString::from(secret.as_str()),
            });
        }
        let old_token = current
            .session_token_ref
            .as_deref()
            .and_then(|reference| account_ref(&current, reference, SLOT_SESSION_TOKEN).ok());
        if update.clear_session_token {
            if let Some(old) = old_token {
                changes.push(SecretChange::Remove { reference: old });
            }
            updated.session_token_ref = None;
        } else if let Some(token) = non_empty(update.session_token.as_deref()) {
            let (new, replaces) = match old_token {
                Some(old) => (old.next()?, Some(old)),
                None => (
                    self.unused_reference(&current.id, SLOT_SESSION_TOKEN)?,
                    None,
                ),
            };
            updated.session_token_ref = Some(new.to_string());
            changes.push(SecretChange::Put {
                reference: new,
                replaces,
                value: SecretString::from(token),
            });
        }

        let replace = |config: &mut AppConfig| -> Result<()> {
            if let Some(label) = &update.label {
                check_label_free(config, label.trim(), Some(id))?;
            }
            let slot = config
                .accounts
                .iter_mut()
                .find(|account| account.id == id)
                .ok_or_else(|| account_not_found(id))?;
            if slot.secret_ref != current.secret_ref
                || slot.session_token_ref != current.session_token_ref
            {
                return Err(changed_concurrently());
            }
            *slot = updated.clone();
            Ok(())
        };
        if changes.is_empty() {
            self.persist(replace)?;
        } else {
            if changes
                .iter()
                .any(|change| matches!(change, SecretChange::Put { .. }))
            {
                self.require_unlocked()?;
            }
            let planned = planned_references(&changes);
            self.apply_change(
                |vault| vault.prepare_secret_changes(changes),
                &planned,
                |config, _| replace(config),
            )?;
        }
        Ok(updated)
    }

    /// Removes an account: the configuration first, then its secrets. Works
    /// while locked; secrets that are already gone are fine. A secret that
    /// cannot be deleted now stays in the journal for the next
    /// [`AccountManager::purge_leftovers`].
    pub fn remove(&self, id: &str) -> Result<()> {
        let _change = self.change_guard()?;
        let config = self.config.load()?;
        let account = find_account(&config, id)?.clone();
        if let Some(user) = config.accounts.iter().find(|other| {
            matches!(&other.source, CredentialSource::AssumeRole { source_account_id, .. }
                if source_account_id == id)
        }) {
            return Err(Error::Conflict(format!(
                "account {:?} assumes a role through this account: remove it first",
                user.label
            )));
        }
        // Only the account's own references are deleted: an edited reference
        // may point at another account's secret.
        let references: Vec<String> = [
            account_ref(&account, &account.secret_ref, SLOT_SECRET)
                .ok()
                .filter(|_| account.has_static_keys()),
            account
                .session_token_ref
                .as_deref()
                .and_then(|reference| account_ref(&account, reference, SLOT_SESSION_TOKEN).ok()),
        ]
        .into_iter()
        .flatten()
        .map(|reference| reference.to_string())
        .collect();
        self.journal_add(&references)?;
        let persisted = self.persist(|config| {
            let before = config.accounts.len();
            config.accounts.retain(|account| account.id != id);
            if config.accounts.len() == before {
                return Err(account_not_found(id));
            }
            if config.default_account_id.as_deref() == Some(id) {
                config.default_account_id = config.accounts.first().map(|a| a.id.clone());
            }
            // The external buckets pinned to the account go with it.
            config.pinned_buckets.remove(id);
            // So do its bookmarks.
            config
                .bookmarks
                .retain(|bookmark| bookmark.account_id != id);
            // And its saved sync jobs.
            config.sync_jobs.retain(|job| job.account_id != id);
            Ok(())
        });
        // Deletes the secrets now that nothing references them (or, if the
        // configuration could not be saved, just tidies the journal).
        self.purge_quietly();
        persisted.map(|_| ())
    }

    /// Makes `id` the default account.
    pub fn set_default(&self, id: &str) -> Result<()> {
        self.persist(|config| {
            find_account(config, id)?;
            config.default_account_id = Some(id.to_owned());
            Ok(())
        })
        .map(|_| ())
    }

    /// Saves the settings after [`Settings::validate`] and the stricter
    /// [`validate_proxy_settings`] (host, user name and bypass entries of an
    /// enabled proxy). The stored proxy password reference is kept as it is:
    /// only [`AccountManager::set_proxy_password`] changes it, so a caller
    /// cannot point the proxy at another secret.
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        validate_proxy_settings(&settings.proxy)?;
        self.persist(|config| {
            let password_ref = config.settings.proxy.password_ref.take();
            let token_ref = config.settings.shortener.token_ref.take();
            config.settings = settings.clone();
            config.settings.proxy.password_ref = password_ref;
            config.settings.shortener.token_ref = token_ref;
            Ok(())
        })
        .map(|_| ())
    }

    /// Sets (`current: None`), changes, or removes (`new_password: None`)
    /// the master password, re-encrypting every referenced secret: account
    /// secrets, session tokens and the proxy password. `current` is
    /// required when a master password is set (and ignored otherwise). A
    /// wrong `current` is an auth error and changes nothing: it is verified
    /// before the journal, the keychain or the configuration is touched.
    /// A configuration with a reference that does not belong to what it is
    /// stored for (an edited `config.toml`) is refused with `usage`, changing
    /// nothing: enter that secret again, or remove the account, first.
    pub fn set_master_password(
        &self,
        current: Option<&str>,
        new_password: Option<&str>,
    ) -> Result<SecurityStatus> {
        let _change = self.change_guard()?;
        let config = self.load()?;
        // The vault's view: it differs from the file only while the file lost
        // its master-password section (see `Vault::sync_config`).
        let master_password_set = self.vault.status().master_password_set;
        match (master_password_set, current, new_password) {
            (true, None, _) => {
                return Err(Error::Usage(
                    "the current master password is required".into(),
                ));
            }
            (false, _, None) => return Err(Error::Usage("no master password is set".into())),
            _ => {}
        }
        if let (true, Some(current)) = (master_password_set, current) {
            self.vault.verify_password(current)?;
        }
        let references = checked_references(&config)?;
        let mut planned: Vec<String> = references.iter().map(SecretRef::to_string).collect();
        for reference in &references {
            planned.push(reference.next()?.to_string());
        }
        let prepare = |vault: &Vault| match (master_password_set, current, new_password) {
            (false, _, Some(new_password)) => {
                vault.prepare_set_master_password(new_password, &references)
            }
            (true, Some(current), Some(new_password)) => {
                vault.prepare_change_master_password(current, new_password, &references)
            }
            (true, Some(current), None) => {
                vault.prepare_remove_master_password(current, &references)
            }
            _ => Err(Error::Internal("unreachable master password case".into())),
        };
        self.apply_change(prepare, &planned, |config, pending| {
            config.security = match pending.security_update() {
                SecurityUpdate::Set(security) => Some(security.clone()),
                SecurityUpdate::Removed => None,
                SecurityUpdate::Unchanged => config.security.clone(),
            };
            remap_references(config, pending)
        })?;
        Ok(self.vault.status())
    }

    /// Stores (`Some`) or deletes (`None`) the proxy password and points
    /// `settings.proxy.password_ref` at it. The password is stored under the
    /// installation's own owner ([`proxy_owner`]); a legacy reference
    /// (`proxy/password/<n>`, shared by every configuration of that time) is
    /// replaced and its value deleted. A reference in the configuration that
    /// is not a proxy password reference of this installation is neither
    /// followed nor deleted, only replaced or cleared.
    pub fn set_proxy_password(&self, password: Option<&str>) -> Result<()> {
        let password = non_empty(password);
        if let Some(password) = password {
            if password.len() > MAX_PROXY_PASSWORD_BYTES {
                return Err(Error::Usage(format!(
                    "the proxy password must be at most {MAX_PROXY_PASSWORD_BYTES} bytes"
                )));
            }
            check_secret("proxy password", password)?;
        }
        let _change = self.change_guard()?;
        let config = self.load()?;
        let current_raw = config.settings.proxy.password_ref.clone();
        let current = current_raw
            .as_deref()
            .and_then(|reference| proxy_ref(config.installation_id.as_deref(), reference).ok());
        let (changes, new_ref) = match (password, current) {
            (None, None) => {
                if current_raw.is_some() {
                    // Not a reference to follow: only drop it.
                    self.persist(|config| {
                        if config.settings.proxy.password_ref != current_raw {
                            return Err(changed_concurrently());
                        }
                        config.settings.proxy.password_ref = None;
                        Ok(())
                    })?;
                }
                return Ok(());
            }
            (None, Some(old)) => (vec![SecretChange::Remove { reference: old }], None),
            (Some(password), old) => {
                self.require_unlocked()?;
                let owner = proxy_owner(&self.ensure_installation_id()?);
                let mut changes = Vec::new();
                let (new, replaces) = match old {
                    Some(old) if old.owner() == owner => (old.next()?, Some(old)),
                    legacy => {
                        changes.extend(legacy.map(|reference| SecretChange::Remove { reference }));
                        (self.unused_reference(&owner, SLOT_PROXY_PASSWORD)?, None)
                    }
                };
                changes.insert(
                    0,
                    SecretChange::Put {
                        reference: new.clone(),
                        replaces,
                        value: SecretString::from(password),
                    },
                );
                (changes, Some(new.to_string()))
            }
        };
        let planned = planned_references(&changes);
        self.apply_change(
            |vault| vault.prepare_secret_changes(changes),
            &planned,
            |config, _| {
                if config.settings.proxy.password_ref != current_raw {
                    return Err(changed_concurrently());
                }
                config.settings.proxy.password_ref = new_ref.clone();
                Ok(())
            },
        )?;
        Ok(())
    }

    /// The URL shortener settings and, when a provider is chosen and a token
    /// is stored, the token. Fails with `usage` when `token_ref` is not the
    /// shortener token of this installation (an edited `config.toml`): no
    /// other secret is ever read for the shortener.
    pub fn shortener_credentials(&self) -> Result<(ShortenerSettings, Option<SecretString>)> {
        let _reading = self.read_guard();
        let config = self.load()?;
        let installation_id = config.installation_id;
        let shortener = config.settings.shortener;
        let token = match (shortener.provider, &shortener.token_ref) {
            (ShortenerProvider::None, _) | (_, None) => None,
            (_, Some(reference)) => {
                let reference = shortener_ref(installation_id.as_deref(), reference)?;
                Some(
                    self.vault
                        .read(&reference)
                        .map_err(|e| e.with_context("URL shortener token"))?,
                )
            }
        };
        Ok((shortener, token))
    }

    /// Stores (`Some`) or deletes (`None`) the URL shortener token and points
    /// `settings.shortener.token_ref` at it, with the same crash-safe
    /// protocol as the proxy password.
    pub fn set_shortener_token(&self, token: Option<&str>) -> Result<()> {
        let token = non_empty(token);
        if let Some(token) = token {
            check_secret("URL shortener token", token)?;
        }
        let _change = self.change_guard()?;
        let config = self.load()?;
        let current_raw = config.settings.shortener.token_ref.clone();
        let current = current_raw
            .as_deref()
            .and_then(|reference| shortener_ref(config.installation_id.as_deref(), reference).ok());
        let (changes, new_ref) = match (token, current) {
            (None, None) => {
                if current_raw.is_some() {
                    self.persist(|config| {
                        if config.settings.shortener.token_ref != current_raw {
                            return Err(changed_concurrently());
                        }
                        config.settings.shortener.token_ref = None;
                        Ok(())
                    })?;
                }
                return Ok(());
            }
            (None, Some(old)) => (vec![SecretChange::Remove { reference: old }], None),
            (Some(token), old) => {
                self.require_unlocked()?;
                let owner = shortener_owner(&self.ensure_installation_id()?);
                let (new, replaces) = match old {
                    Some(old) if old.owner() == owner => (old.next()?, Some(old)),
                    _ => (self.unused_reference(&owner, SLOT_SHORTENER_TOKEN)?, None),
                };
                let changes = vec![SecretChange::Put {
                    reference: new.clone(),
                    replaces,
                    value: SecretString::from(token),
                }];
                (changes, Some(new.to_string()))
            }
        };
        let planned = planned_references(&changes);
        self.apply_change(
            |vault| vault.prepare_secret_changes(changes),
            &planned,
            |config, _| {
                if config.settings.shortener.token_ref != current_raw {
                    return Err(changed_concurrently());
                }
                config.settings.shortener.token_ref = new_ref.clone();
                Ok(())
            },
        )?;
        Ok(())
    }

    /// Moves the proxy password of a configuration from before installation
    /// ids (reference `proxy/password/<n>`, one name shared by every
    /// configuration of that time) to this installation's own reference, with
    /// the same two-phase protocol as every secret change: the new value is
    /// written, the configuration is saved, then the legacy value is
    /// deleted. Does nothing when there is nothing to move, when the secrets
    /// are locked (it is retried by [`AccountManager::unlock`]) or when the
    /// legacy value is gone.
    pub fn migrate_legacy_references(&self) -> Result<()> {
        let _change = self.change_guard()?;
        let config = self.load()?;
        let Some(raw) = config.settings.proxy.password_ref.clone() else {
            return Ok(());
        };
        let Ok(legacy) = proxy_ref(config.installation_id.as_deref(), &raw) else {
            return Ok(());
        };
        if legacy.owner() != PROXY_OWNER || !self.vault.status().unlocked {
            return Ok(());
        }
        let value = match self.vault.read(&legacy) {
            Ok(value) => value,
            Err(error) if error.kind() == crate::error::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.with_context("proxy password")),
        };
        let owner = proxy_owner(&self.ensure_installation_id()?);
        let new = self.unused_reference(&owner, SLOT_PROXY_PASSWORD)?;
        let changes = vec![
            SecretChange::Put {
                reference: new.clone(),
                replaces: None,
                value,
            },
            SecretChange::Remove { reference: legacy },
        ];
        let planned = planned_references(&changes);
        self.apply_change(
            |vault| vault.prepare_secret_changes(changes),
            &planned,
            |config, _| {
                if config.settings.proxy.password_ref.as_deref() != Some(raw.as_str()) {
                    return Err(changed_concurrently());
                }
                config.settings.proxy.password_ref = Some(new.to_string());
                Ok(())
            },
        )?;
        Ok(())
    }

    /// Deletes values left over by interrupted changes: every journal entry
    /// the persisted configuration does not reference. Returns an error
    /// (keeping the failed entries for next time) when a deletion fails.
    pub fn purge_leftovers(&self) -> Result<()> {
        let _change = self.change_guard()?;
        self.purge()
    }

    fn read_credentials(&self, account: &Account) -> Result<Credentials> {
        if !account.has_static_keys() {
            return Err(Error::Usage(format!(
                "account {:?} has no keys of its own: its credentials come from {}",
                account.label,
                source_name(&account.source)
            )));
        }
        let context = || format!("account {:?}", account.label);
        let secret = self
            .vault
            .read(&account_ref(account, &account.secret_ref, SLOT_SECRET)?)
            .map_err(|e| e.with_context(context()))?;
        let token = match &account.session_token_ref {
            Some(reference) => Some(
                self.vault
                    .read(&account_ref(account, reference, SLOT_SESSION_TOKEN)?)
                    .map_err(|e| e.with_context(context()))?,
            ),
            None => None,
        };
        Ok(Credentials::new(
            account.access_key_id.as_str(),
            secret.expose(),
            token.as_ref().map(|token| token.expose().to_owned()),
        ))
    }

    fn require_unlocked(&self) -> Result<()> {
        if self.vault.status().unlocked {
            Ok(())
        } else {
            Err(Error::Locked)
        }
    }

    /// The installation id; created and saved now when the configuration has
    /// none yet. The caller holds the change guard.
    fn ensure_installation_id(&self) -> Result<String> {
        if let Some(id) = self.config.load()?.installation_id {
            return Ok(id);
        }
        self.config.update(|config| {
            config
                .installation_id
                .clone()
                .ok_or_else(|| Error::Internal("the installation id was not created".into()))
        })
    }

    /// The first generation of `owner/slot` that holds no value. A new slot
    /// starts at generation 1, and a later one is taken only when an earlier
    /// one is occupied (by a leftover, or by another configuration that shares
    /// the keychain), so nothing is ever written over an existing item, and
    /// nothing that is in the way is deleted.
    fn unused_reference(&self, owner: &str, slot: &str) -> Result<SecretRef> {
        let mut reference = SecretRef::first(owner, slot)?;
        for _ in 0..MAX_UNUSED_PROBES {
            if self.store.get(reference.as_str())?.is_none() {
                return Ok(reference);
            }
            reference = reference.next()?;
        }
        Err(Error::Conflict(format!(
            "no free keychain slot for {owner}/{slot}: remove the stale BucketBridge items"
        )))
    }

    /// Steps 1 to 5 of the module docs.
    fn apply_change(
        &self,
        prepare: impl FnOnce(&Vault) -> Result<PendingChange>,
        planned: &[String],
        update: impl FnOnce(&mut AppConfig, &PendingChange) -> Result<()>,
    ) -> Result<AppConfig> {
        self.journal_add(planned)?;
        let pending = match prepare(&self.vault) {
            Ok(pending) => pending,
            Err(error) => {
                self.purge_quietly();
                return Err(error);
            }
        };
        let result = match self.persist(|config| update(config, &pending)) {
            Ok(config) => {
                // The change is persisted. Should finalizing fail, the next
                // load adopts the persisted configuration and the purge
                // deletes what finalizing would have deleted.
                let _cleanup = pending.finalize();
                Ok(config)
            }
            Err(error) => {
                // Not persisted: drop the change without deleting anything;
                // the purge removes the values it wrote.
                drop(pending);
                Err(error)
            }
        };
        self.purge_quietly();
        result
    }

    /// Writes a change through [`ConfigStore::update`] and returns the
    /// configuration as written. When `update` fails after possibly
    /// writing, the file is read back: the change counts as persisted when
    /// the file holds exactly the configuration that was being written.
    fn persist(&self, change: impl FnOnce(&mut AppConfig) -> Result<()>) -> Result<AppConfig> {
        let mut written = None;
        let result = self.config.update(|config| {
            change(config)?;
            written = Some(config.clone());
            Ok(())
        });
        match (result, written) {
            (Ok(()), Some(mut config)) => {
                config.version = CONFIG_VERSION;
                Ok(config)
            }
            (Ok(()), None) => Err(Error::Internal(
                "the configuration change did not run".into(),
            )),
            (Err(error), Some(mut expected)) => {
                expected.version = CONFIG_VERSION;
                match self.config.load() {
                    Ok(actual) if actual == expected => Ok(actual),
                    _ => Err(error),
                }
            }
            (Err(error), None) => Err(error),
        }
    }

    fn purge_quietly(&self) {
        // Failures keep the entries in the journal for the next purge.
        let _ = self.purge();
    }

    /// Step 5 of the module docs; the caller holds the change guard.
    fn purge(&self) -> Result<()> {
        let journal = self.read_journal()?;
        if journal.is_empty() {
            return Ok(());
        }
        let referenced = referenced_strings(&self.config.load()?);
        let mut remaining = BTreeSet::new();
        let mut first_error = None;
        for reference in journal {
            if referenced.contains(&reference) {
                continue;
            }
            if let Err(error) = self.store.delete(&reference) {
                first_error.get_or_insert(error);
                remaining.insert(reference);
            }
        }
        self.write_journal(&remaining)?;
        first_error.map_or(Ok(()), Err)
    }

    fn change_guard(&self) -> Result<ChangeGuard<'_>> {
        let writing = self.changes.write().unwrap_or_else(PoisonError::into_inner);
        let file = lock_file(&self.lock_path)?;
        Ok(ChangeGuard {
            _file: file,
            _writing: writing,
        })
    }

    fn read_guard(&self) -> RwLockReadGuard<'_, ()> {
        self.changes.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn journal_add(&self, references: &[String]) -> Result<()> {
        if references.is_empty() {
            return Ok(());
        }
        let mut journal = self.read_journal()?;
        journal.extend(references.iter().cloned());
        self.write_journal(&journal)
    }

    fn read_journal(&self) -> Result<BTreeSet<String>> {
        let bytes = match fs::read(&self.journal_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(error) => {
                return Err(Error::io(
                    format!("reading {}", self.journal_path.display()),
                    error,
                ));
            }
        };
        match parse_journal(&bytes) {
            Some(references) => Ok(references),
            None => Err(self.set_journal_aside()),
        }
    }

    /// A journal that cannot be read is moved next to itself instead of being
    /// discarded, and the damage is reported: the leftovers it listed can no
    /// longer be removed automatically. Returns the error to report.
    fn set_journal_aside(&self) -> Error {
        let dir = parent_dir(&self.journal_path);
        let aside = (1..=MAX_DAMAGED_JOURNALS)
            .map(|n| dir.join(format!("{SECRETS_JOURNAL_FILE}.damaged-{n}")))
            .find(|path| !path.exists());
        let Some(aside) = aside else {
            return Error::Config {
                path: self.journal_path.clone(),
                message: format!(
                    "the journal of interrupted changes is damaged, and {MAX_DAMAGED_JOURNALS} \
                     damaged copies are already set aside: remove them"
                ),
            };
        };
        if let Err(error) = fs::rename(&self.journal_path, &aside) {
            return Error::io(
                format!(
                    "moving the damaged journal {} to {}",
                    self.journal_path.display(),
                    aside.display()
                ),
                error,
            );
        }
        // Best effort: the journal is gone either way.
        let _ = sync_dir(dir);
        tracing::warn!(
            target: "bucketbridge::secrets",
            "the journal of interrupted changes was damaged and moved to {}",
            aside.display()
        );
        Error::Config {
            path: self.journal_path.clone(),
            message: format!(
                "the journal of interrupted changes is damaged; it was moved to {}. Values it \
                 listed may still be in the OS keychain (items named BucketBridge that no \
                 account uses): try the operation again",
                aside.display()
            ),
        }
    }

    fn write_journal(&self, references: &BTreeSet<String>) -> Result<()> {
        if references.is_empty() {
            return match fs::remove_file(&self.journal_path) {
                Ok(()) => sync_dir(parent_dir(&self.journal_path)),
                Err(error) if error.kind() == IoErrorKind::NotFound => Ok(()),
                Err(error) => Err(Error::io(
                    format!("removing {}", self.journal_path.display()),
                    error,
                )),
            };
        }
        let journal = Journal {
            version: JOURNAL_VERSION,
            references: references.clone(),
        };
        let text = serde_json::to_vec_pretty(&journal)
            .map_err(|error| Error::Internal(format!("cannot encode the journal: {error}")))?;
        write_atomically(&self.journal_path, &text)
    }
}

impl std::fmt::Debug for AccountManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountManager")
            .field("config", &self.config.path())
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}

/// Held for the whole of a secret change.
struct ChangeGuard<'a> {
    _file: File,
    _writing: RwLockWriteGuard<'a, ()>,
}

/// `secrets-journal.json`.
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    references: BTreeSet<String>,
}

/// The references of a journal; `None` when it is not one of ours (not
/// JSON, an unknown version, or an entry that is not a reference).
fn parse_journal(bytes: &[u8]) -> Option<BTreeSet<String>> {
    let journal: Journal = serde_json::from_slice(bytes).ok()?;
    let usable = (1..=JOURNAL_VERSION).contains(&journal.version)
        && journal
            .references
            .iter()
            .all(|reference| SecretRef::parse(reference).is_ok());
    usable.then_some(journal.references)
}

fn find_account<'a>(config: &'a AppConfig, id: &str) -> Result<&'a Account> {
    config
        .accounts
        .iter()
        .find(|account| account.id == id)
        .ok_or_else(|| account_not_found(id))
}

/// Validates a credential source other than static keys. `own_id` is the
/// account being changed (it cannot be its own source).
fn check_source(config: &AppConfig, source: &CredentialSource, own_id: Option<&str>) -> Result<()> {
    match source {
        CredentialSource::StaticKeys | CredentialSource::InstanceRole => Ok(()),
        CredentialSource::AwsProfile { profile } => {
            let name = profile.trim();
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(Error::Usage(
                    "the AWS profile name must be 1 to 128 characters without control characters"
                        .into(),
                ));
            }
            Ok(())
        }
        CredentialSource::AssumeRole {
            source_account_id,
            role_arn,
            session_name,
            duration_secs,
            external_id,
            mfa_serial,
        } => {
            let Some(source_account) = config.accounts.iter().find(|a| a.id == *source_account_id)
            else {
                return Err(Error::NotFound(format!(
                    "the source account {source_account_id:?} of the role does not exist"
                )));
            };
            if own_id == Some(source_account_id.as_str()) {
                return Err(Error::Usage(
                    "an account cannot assume a role through itself".into(),
                ));
            }
            // Following the chain of sources must come back to nothing.
            let mut seen = vec![source_account.id.as_str()];
            let mut next = source_account;
            while let CredentialSource::AssumeRole {
                source_account_id: parent,
                ..
            } = &next.source
            {
                let Some(parent_account) = config.accounts.iter().find(|a| a.id == *parent) else {
                    break;
                };
                if seen.contains(&parent_account.id.as_str()) || own_id == Some(parent.as_str()) {
                    return Err(Error::Usage(
                        "the source accounts of the role form a loop".into(),
                    ));
                }
                seen.push(parent_account.id.as_str());
                next = parent_account;
            }
            check_role_arn(role_arn)?;
            check_role_text("session name", session_name, 2, 64, "_+=,.@-")?;
            if !(MIN_ROLE_DURATION_SECS..=MAX_ROLE_DURATION_SECS).contains(duration_secs) {
                return Err(Error::Usage(format!(
                    "the role session length must be {MIN_ROLE_DURATION_SECS} to \
                     {MAX_ROLE_DURATION_SECS} seconds"
                )));
            }
            if let Some(external_id) = external_id {
                check_role_text("external id", external_id, 2, 1224, "_+=,.@:/-")?;
            }
            if let Some(serial) = mfa_serial {
                validate_mfa_serial(serial)?;
            }
            Ok(())
        }
        CredentialSource::Sso {
            start_url,
            sso_region,
            account_id,
            role_name,
            sso_session,
        } => {
            if !start_url.starts_with("https://") || start_url.len() < 12 {
                return Err(Error::Usage(
                    "the IAM Identity Center start URL must start with https://".into(),
                ));
            }
            validate_region(sso_region)?;
            if account_id.len() != 12 || !account_id.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Usage("the AWS account id must be 12 digits".into()));
            }
            if role_name.trim().is_empty() || role_name.len() > 64 {
                return Err(Error::Usage(
                    "the role name must be 1 to 64 characters".into(),
                ));
            }
            if let Some(name) = sso_session
                && (name.trim().is_empty() || name.len() > 128)
            {
                return Err(Error::Usage(
                    "the sso-session name must be 1 to 128 characters".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Checks a role ARN: `arn:<partition>:iam::<12 digits>:role/<name>`.
fn check_role_arn(arn: &str) -> Result<()> {
    let parts: Vec<&str> = arn.splitn(6, ':').collect();
    let valid = parts.len() == 6
        && parts[0] == "arn"
        && parts[1].starts_with("aws")
        && parts[2] == "iam"
        && parts[3].is_empty()
        && parts[4].len() == 12
        && parts[4].bytes().all(|b| b.is_ascii_digit())
        && parts[5].starts_with("role/")
        && parts[5].len() > "role/".len()
        && arn.len() <= 2048
        && arn.bytes().all(|b| b.is_ascii_graphic());
    if valid {
        Ok(())
    } else {
        Err(Error::Usage(
            "the role ARN must look like arn:aws:iam::123456789012:role/Name".into(),
        ))
    }
}

fn check_role_text(what: &str, value: &str, min: usize, max: usize, extra: &str) -> Result<()> {
    let valid_chars = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if (min..=max).contains(&value.len()) && valid_chars {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "the {what} must be {min} to {max} letters, digits or any of {extra}"
        )))
    }
}

/// Short phrase for error messages.
fn source_name(source: &CredentialSource) -> &'static str {
    match source {
        CredentialSource::StaticKeys => "static keys",
        CredentialSource::AwsProfile { .. } => "an AWS profile",
        CredentialSource::AssumeRole { .. } => "an assumed role",
        CredentialSource::Sso { .. } => "IAM Identity Center",
        CredentialSource::InstanceRole => "the instance or container role",
    }
}

fn check_label(label: &str) -> Result<&str> {
    let label = label.trim();
    if label.is_empty() {
        return Err(Error::Usage("the account label is empty".into()));
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(Error::Usage(format!(
            "the account label must be at most {MAX_LABEL_CHARS} characters"
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the account label must not contain control characters".into(),
        ));
    }
    Ok(label)
}

/// Labels are unique case-insensitively; `except` is the account being
/// renamed.
fn check_label_free(config: &AppConfig, label: &str, except: Option<&str>) -> Result<()> {
    let wanted = label.to_lowercase();
    let taken = config.accounts.iter().any(|account| {
        Some(account.id.as_str()) != except && account.label.trim().to_lowercase() == wanted
    });
    if taken {
        Err(Error::Conflict(format!(
            "an account labelled {label:?} already exists"
        )))
    } else {
        Ok(())
    }
}

fn check_access_key(access_key_id: &str) -> Result<()> {
    let key = access_key_id.trim();
    if key.is_empty() {
        return Err(Error::Usage("the access key ID is empty".into()));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Usage(
            "the access key ID must not contain spaces or control characters".into(),
        ));
    }
    Ok(())
}

/// Secrets are stored exactly as given; control characters (a pasted line
/// break) are rejected rather than silently kept or removed.
fn check_secret(what: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Usage(format!("the {what} is empty")));
    }
    if value.len() > MAX_SECRET_BYTES {
        return Err(Error::Usage(format!(
            "the {what} must be at most {MAX_SECRET_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::Usage(format!(
            "the {what} must not contain control characters such as line breaks"
        )));
    }
    Ok(())
}

fn check_update(update: &AccountUpdate) -> Result<()> {
    if update.clear_session_token && non_empty(update.session_token.as_deref()).is_some() {
        return Err(Error::Usage(
            "a session token cannot be set and cleared at once".into(),
        ));
    }
    if update.clear_endpoint && non_empty(update.endpoint.as_deref()).is_some() {
        return Err(Error::Usage(
            "an endpoint cannot be set and cleared at once".into(),
        ));
    }
    if update.clear_mfa_serial && non_empty(update.mfa_serial.as_deref()).is_some() {
        return Err(Error::Usage(
            "an MFA serial cannot be set and cleared at once".into(),
        ));
    }
    if let Some(label) = &update.label {
        check_label(label)?;
    }
    if let Some(key) = &update.access_key_id {
        check_access_key(key)?;
    }
    if let Some(secret) = &update.secret_access_key {
        check_secret("secret access key", secret)?;
    }
    if let Some(token) = non_empty(update.session_token.as_deref()) {
        check_secret("session token", token)?;
    }
    if let Some(region) = &update.region {
        validate_region(region)?;
    }
    if let Some(endpoint) = non_empty(update.endpoint.as_deref()) {
        validate_endpoint(endpoint)?;
    }
    if let Some(serial) = non_empty(update.mfa_serial.as_deref()) {
        validate_mfa_serial(serial)?;
    }
    Ok(())
}

/// Optional text fields: an empty string means "not given".
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn parse_ref(reference: &str) -> Result<SecretRef> {
    SecretRef::parse(reference)
}

/// The reference an account stores `slot` under, checked: it must be owned by
/// the account and sit in that slot. Anything else is an edited configuration
/// and is rejected before it is used.
fn account_ref(account: &Account, reference: &str, slot: &str) -> Result<SecretRef> {
    let parsed = parse_ref(reference)
        .map_err(|error| error.with_context(format!("account {:?}", account.label)))?;
    if parsed.owner() == account.id && parsed.slot() == slot {
        Ok(parsed)
    } else {
        Err(Error::Usage(format!(
            "account {:?}: the reference {reference:?} is not this account's {} (was config.toml \
             edited?): enter the secret again",
            account.label,
            slot_name(slot)
        )))
    }
}

fn slot_name(slot: &str) -> &'static str {
    match slot {
        SLOT_SECRET => "secret access key",
        SLOT_SESSION_TOKEN => "session token",
        _ => "secret",
    }
}

/// The proxy password reference, checked: the proxy password of this
/// installation, or the legacy owner of configurations from before
/// installation ids.
fn proxy_ref(installation_id: Option<&str>, reference: &str) -> Result<SecretRef> {
    let parsed = parse_ref(reference).map_err(|error| error.with_context("proxy password"))?;
    let own = installation_id.map(proxy_owner);
    let belongs = parsed.slot() == SLOT_PROXY_PASSWORD
        && (parsed.owner() == PROXY_OWNER || own.as_deref() == Some(parsed.owner()));
    if belongs {
        Ok(parsed)
    } else {
        Err(Error::Usage(format!(
            "proxy password: the reference {reference:?} is not the proxy password of this \
             installation (was config.toml edited?): enter the proxy password again"
        )))
    }
}

/// The shortener token reference, checked: the token of this installation.
fn shortener_ref(installation_id: Option<&str>, reference: &str) -> Result<SecretRef> {
    let parsed = parse_ref(reference).map_err(|error| error.with_context("URL shortener token"))?;
    let own = installation_id.map(shortener_owner);
    if parsed.slot() == SLOT_SHORTENER_TOKEN && own.as_deref() == Some(parsed.owner()) {
        Ok(parsed)
    } else {
        Err(Error::Usage(format!(
            "URL shortener token: the reference {reference:?} is not the shortener token of this \
             installation (was config.toml edited?): enter the token again"
        )))
    }
}

/// Every secret reference the configuration holds.
fn referenced_strings(config: &AppConfig) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    for account in config.accounts.iter().filter(|a| a.has_static_keys()) {
        references.insert(account.secret_ref.clone());
        references.extend(account.session_token_ref.clone());
    }
    references.extend(config.settings.proxy.password_ref.clone());
    references.extend(config.settings.shortener.token_ref.clone());
    references
}

/// Every reference of the configuration, checked against what it is stored
/// for (see [`account_ref`] and [`proxy_ref`]): what a master password change
/// re-encrypts. An edited reference is an error, so that no secret is left
/// out of the change unnoticed.
fn checked_references(config: &AppConfig) -> Result<Vec<SecretRef>> {
    let mut references = BTreeSet::new();
    for account in config.accounts.iter().filter(|a| a.has_static_keys()) {
        references.insert(account_ref(account, &account.secret_ref, SLOT_SECRET)?);
        if let Some(reference) = &account.session_token_ref {
            references.insert(account_ref(account, reference, SLOT_SESSION_TOKEN)?);
        }
    }
    if let Some(reference) = &config.settings.proxy.password_ref {
        references.insert(proxy_ref(config.installation_id.as_deref(), reference)?);
    }
    if let Some(reference) = &config.settings.shortener.token_ref {
        references.insert(shortener_ref(config.installation_id.as_deref(), reference)?);
    }
    Ok(references.into_iter().collect())
}

/// Points every reference of `config` at its replacement in `pending`. Fails
/// when a reference is not one `pending` was prepared for, for example because
/// the configuration changed meanwhile.
fn remap_references(config: &mut AppConfig, pending: &PendingChange) -> Result<()> {
    let remap = |reference: &mut String, checked: SecretRef| -> Result<()> {
        let new = pending
            .new_reference(&checked)
            .ok_or_else(changed_concurrently)?;
        *reference = new.to_string();
        Ok(())
    };
    for account in config
        .accounts
        .iter_mut()
        .filter(|account| account.has_static_keys())
    {
        let secret = account_ref(account, &account.secret_ref, SLOT_SECRET)?;
        remap(&mut account.secret_ref, secret)?;
        if let Some(reference) = account.session_token_ref.clone() {
            let token = account_ref(account, &reference, SLOT_SESSION_TOKEN)?;
            if let Some(slot) = &mut account.session_token_ref {
                remap(slot, token)?;
            }
        }
    }
    if let Some(reference) = config.settings.proxy.password_ref.clone() {
        let checked = proxy_ref(config.installation_id.as_deref(), &reference)?;
        if let Some(slot) = &mut config.settings.proxy.password_ref {
            remap(slot, checked)?;
        }
    }
    if let Some(reference) = config.settings.shortener.token_ref.clone() {
        let checked = shortener_ref(config.installation_id.as_deref(), &reference)?;
        if let Some(slot) = &mut config.settings.shortener.token_ref {
            remap(slot, checked)?;
        }
    }
    Ok(())
}

/// References a change may write or make obsolete, for the journal.
fn planned_references(changes: &[SecretChange]) -> Vec<String> {
    let mut planned = Vec::new();
    for change in changes {
        match change {
            SecretChange::Put {
                reference,
                replaces,
                ..
            } => {
                planned.push(reference.to_string());
                planned.extend(replaces.as_ref().map(SecretRef::to_string));
            }
            SecretChange::Remove { reference } => planned.push(reference.to_string()),
        }
    }
    planned
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Creates `dir` and its parents with mode `0700` on Unix.
fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .io_context(|| format!("creating directory {}", dir.display()))
}

/// Opens and exclusively locks `path` (blocking until other holders are
/// done).
fn lock_file(path: &Path) -> Result<File> {
    create_private_dir(parent_dir(path))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .io_context(|| format!("opening lock file {}", path.display()))?;
    file.lock()
        .io_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

/// Replaces `path` with `contents` atomically and durably (temporary file,
/// flush, rename, flush of the directory: a journal entry must survive a crash
/// before the first value it lists is written).
fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = parent_dir(path);
    create_private_dir(dir)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".journal.")
        .suffix(".tmp")
        .tempfile_in(dir)
        .io_context(|| format!("creating a temporary file in {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .io_context(|| format!("restricting permissions of {}", temp.path().display()))?;
    }
    temp.write_all(contents)
        .and_then(|()| temp.as_file().sync_all())
        .io_context(|| format!("writing {}", temp.path().display()))?;
    temp.persist(path)
        .map_err(|error| Error::io(format!("replacing {}", path.display()), error.error))?;
    sync_dir(dir)
}

// Error constructors of this module.

fn account_not_found(id: &str) -> Error {
    Error::NotFound(format!("account {id} does not exist"))
}

fn changed_concurrently() -> Error {
    Error::Conflict("the configuration changed during the operation: try again".into())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::config::Dirs;
    use crate::error::ErrorKind;
    use crate::secrets::store::testing::FlakyStore;

    const SECRET_A: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const SECRET_B: &str = "je7MtGbClwBF/2Zp9Utk/h3yCo8nvbEXAMPLEKEY";
    const ROTATED: &str = "rotated-secret-value-for-account-a";
    const TOKEN: &str = "FwoGZXIvYXdzEXAMPLESESSIONTOKENzZXNzaW9uLXRva2Vu";
    const PROXY_PASSWORD: &str = "proxy-password-value";
    const PASSWORD_1: &str = "first master password";
    const PASSWORD_2: &str = "second master password";
    const TEST_COST: KdfCost = KdfCost {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    };

    /// Configuration text, keychain values, keychain operations (writes,
    /// deletes) and journal text.
    type Snapshot = (
        String,
        Vec<(String, String)>,
        (usize, usize),
        Option<String>,
    );

    /// A way to edit an account behind the account manager's back.
    type Edit = Box<dyn Fn(&mut Account)>;

    struct Fixture {
        dir: tempfile::TempDir,
        store: Arc<FlakyStore>,
        manager: AccountManager,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(FlakyStore::default());
            let manager = Self::open(&dir, &store);
            Fixture {
                dir,
                store,
                manager,
            }
        }

        /// Another process on the same directory and keychain.
        fn open(dir: &tempfile::TempDir, store: &Arc<FlakyStore>) -> AccountManager {
            let config = ConfigStore::new(&Dirs::under(dir.path()));
            AccountManager::with_kdf_cost(config, store.clone(), TEST_COST).unwrap()
        }

        fn reopen(&self) -> AccountManager {
            Self::open(&self.dir, &self.store)
        }

        fn config_text(&self) -> String {
            fs::read_to_string(self.dir.path().join("config.toml")).unwrap_or_default()
        }

        fn journal_exists(&self) -> bool {
            self.dir.path().join(SECRETS_JOURNAL_FILE).exists()
        }

        fn journal_text(&self) -> Option<String> {
            fs::read_to_string(self.dir.path().join(SECRETS_JOURNAL_FILE)).ok()
        }

        /// Another configuration folder on the same machine: its own
        /// directory, the same OS keychain.
        fn another_installation(&self) -> (tempfile::TempDir, AccountManager) {
            let dir = tempfile::tempdir().unwrap();
            let manager = Self::open(&dir, &self.store);
            (dir, manager)
        }

        /// Edits `config.toml` behind the account manager's back, as any
        /// program running as the user could.
        fn tamper(&self, change: impl FnOnce(&mut AppConfig)) {
            self.manager
                .config_store()
                .update(|config| {
                    change(config);
                    Ok(())
                })
                .unwrap();
        }

        fn installation_id(&self) -> String {
            self.manager.load().unwrap().installation_id.unwrap()
        }

        /// What a master password change or a wrong attempt may not touch.
        fn snapshot(&self) -> Snapshot {
            (
                self.config_text(),
                self.store.raw(),
                self.store.operations(),
                self.journal_text(),
            )
        }

        fn add(&self, label: &str, secret: &str) -> Account {
            self.manager.add(&input(label, secret)).unwrap()
        }

        fn secret_of(&self, id: &str) -> String {
            let (_, credentials) = self.manager.account_credentials(id).unwrap();
            credentials.secret_access_key.clone()
        }
    }

    impl AccountManager {
        /// The proxy password of this installation, read regardless of the
        /// proxy being enabled (test helper).
        fn proxy_password_value(&self) -> String {
            let config = self.load().unwrap();
            let reference = config.settings.proxy.password_ref.unwrap();
            self.vault
                .read(&SecretRef::parse(&reference).unwrap())
                .unwrap()
                .expose()
                .to_owned()
        }
    }

    fn input(label: &str, secret: &str) -> AccountInput {
        AccountInput {
            source: crate::model::CredentialSource::StaticKeys,
            label: label.to_owned(),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: secret.to_owned(),
            session_token: None,
            region: "eu-west-1".to_owned(),
            endpoint: None,
            path_style: false,
            request_payer: false,
            mfa_serial: None,
        }
    }

    /// `AccountUpdate` zeroizes on drop, so struct update syntax is not
    /// available.
    fn updating(change: impl FnOnce(&mut AccountUpdate)) -> AccountUpdate {
        let mut update = AccountUpdate::default();
        change(&mut update);
        update
    }

    fn rotate(secret: &str) -> AccountUpdate {
        updating(|u| u.secret_access_key = Some(secret.to_owned()))
    }

    fn kind<T: std::fmt::Debug>(result: Result<T>) -> ErrorKind {
        result.expect_err("expected an error").kind()
    }

    /// Every byte BucketBridge wrote under the directory.
    fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    files.push((path, bytes));
                }
            }
        }
        files
    }

    fn assert_no_secret_on_disk(dir: &Path, secrets: &[&str]) {
        for (path, bytes) in files_under(dir) {
            let text = String::from_utf8_lossy(&bytes);
            for secret in secrets {
                for form in [secret.to_string(), STANDARD.encode(secret)] {
                    assert!(
                        !text.contains(&form),
                        "{} contains a secret",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn add_list_get_and_default() {
        let f = Fixture::new();
        assert!(f.manager.list().unwrap().accounts.is_empty());
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        let list = f.manager.list().unwrap();
        assert_eq!(list.accounts, vec![a.clone(), b.clone()]);
        assert_eq!(list.default_account_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(f.manager.get(&b.id).unwrap(), b);
        assert_eq!(a.secret_ref, format!("{}/secret/1", a.id));
        assert_eq!(a.id.len(), 36);
        assert_eq!(&a.id[14..15], "4");
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert_eq!(f.secret_of(&b.id), SECRET_B);
        f.manager.set_default(&b.id).unwrap();
        assert_eq!(
            f.manager.list().unwrap().default_account_id.as_deref(),
            Some(b.id.as_str())
        );
        assert_eq!(kind(f.manager.set_default("missing")), ErrorKind::NotFound);
        assert_eq!(kind(f.manager.get("missing")), ErrorKind::NotFound);
        assert!(!f.journal_exists());
    }

    #[test]
    fn labels_are_unique_case_insensitively() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Other", SECRET_B);
        assert_eq!(
            kind(f.manager.add(&input("  wORK ", SECRET_B))),
            ErrorKind::Conflict
        );
        let rename = updating(|u| {
            u.label = Some("work".into());
        });
        assert_eq!(kind(f.manager.update(&b.id, &rename)), ErrorKind::Conflict);
        // Renaming an account to a new case of its own label is fine.
        let recase = updating(|u| {
            u.label = Some("WORK".into());
        });
        assert_eq!(f.manager.update(&a.id, &recase).unwrap().label, "WORK");
        assert_eq!(f.store.inner.len(), 2);
    }

    #[test]
    fn invalid_input_is_rejected_before_anything_is_stored() {
        let f = Fixture::new();
        let mut cases = Vec::new();
        let mut i = input("", SECRET_A);
        cases.push(i.clone());
        i = input("x", "");
        cases.push(i.clone());
        i = input("x", "line\nbreak");
        cases.push(i.clone());
        i = input("x", SECRET_A);
        i.access_key_id = " ".into();
        cases.push(i.clone());
        i = input("x", SECRET_A);
        i.region = "eu west".into();
        cases.push(i.clone());
        i = input("x", SECRET_A);
        i.endpoint = Some("ftp://example.test".into());
        cases.push(i.clone());
        i = input("x", SECRET_A);
        i.mfa_serial = Some("bad serial".into());
        cases.push(i.clone());
        for case in &cases {
            assert_eq!(kind(f.manager.add(case)), ErrorKind::Usage, "{case:?}");
        }
        assert!(f.store.inner.is_empty());
        assert_eq!(f.store.operations(), (0, 0));
        assert!(f.manager.list().unwrap().accounts.is_empty());
    }

    #[test]
    fn update_rotates_and_replaces_secrets() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);

        let rotated = f.manager.update(&a.id, &rotate(ROTATED)).unwrap();
        assert_eq!(rotated.secret_ref, format!("{}/secret/2", a.id));
        assert_eq!(f.secret_of(&a.id), ROTATED);
        assert_eq!(f.store.inner.references(), vec![rotated.secret_ref.clone()]);

        let with_token = updating(|u| {
            u.session_token = Some(TOKEN.into());
            u.region = Some("us-west-2".into());
            u.endpoint = Some("https://s3.example.test".into());
        });
        let updated = f.manager.update(&a.id, &with_token).unwrap();
        assert_eq!(
            updated.session_token_ref.as_deref(),
            Some(format!("{}/session-token/1", a.id).as_str())
        );
        assert_eq!(updated.region, "us-west-2");
        let (_, credentials) = f.manager.account_credentials(&a.id).unwrap();
        assert_eq!(credentials.session_token.as_deref(), Some(TOKEN));

        let cleared = updating(|u| {
            u.clear_session_token = true;
            u.clear_endpoint = true;
        });
        let updated = f.manager.update(&a.id, &cleared).unwrap();
        assert_eq!(updated.session_token_ref, None);
        assert_eq!(updated.endpoint, None);
        assert_eq!(f.store.inner.len(), 1);
        assert_eq!(f.manager.get(&a.id).unwrap(), updated);

        let both = updating(|u| {
            u.session_token = Some(TOKEN.into());
            u.clear_session_token = true;
        });
        assert_eq!(kind(f.manager.update(&a.id, &both)), ErrorKind::Usage);
        assert_eq!(
            kind(f.manager.update("missing", &rotate(ROTATED))),
            ErrorKind::NotFound
        );
        assert!(!f.journal_exists());
    }

    #[test]
    fn remove_deletes_the_secrets_and_tolerates_missing_ones() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        let with_token = updating(|u| {
            u.session_token = Some(TOKEN.into());
        });
        let a = f.manager.update(&a.id, &with_token).unwrap();
        assert_eq!(f.store.inner.len(), 3);

        f.manager.remove(&a.id).unwrap();
        assert_eq!(f.store.inner.references(), vec![b.secret_ref.clone()]);
        let list = f.manager.list().unwrap();
        assert_eq!(list.accounts, vec![b.clone()]);
        // The default moved to the remaining account.
        assert_eq!(list.default_account_id.as_deref(), Some(b.id.as_str()));

        // A secret that is already gone does not block the removal.
        f.store.inner.delete(&b.secret_ref).unwrap();
        f.manager.remove(&b.id).unwrap();
        assert!(f.manager.list().unwrap().accounts.is_empty());
        assert_eq!(kind(f.manager.remove(&b.id)), ErrorKind::NotFound);
        assert!(f.store.inner.is_empty());
        assert!(!f.journal_exists());
    }

    #[test]
    fn remove_takes_the_pinned_buckets_of_the_account_with_it() {
        use crate::bucketcfg::{pin_bucket, pinned_buckets};

        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        let store = f.manager.config_store();
        pin_bucket(store, &a.id, "shared-data").unwrap();
        pin_bucket(store, &b.id, "partner-exports").unwrap();
        // Other account changes keep the pins.
        f.manager.set_default(&b.id).unwrap();
        f.manager
            .update(
                &a.id,
                &updating(|u| {
                    u.region = Some("eu-west-1".into());
                }),
            )
            .unwrap();
        let config = f.manager.load().unwrap();
        assert_eq!(pinned_buckets(&config, &a.id), ["shared-data"]);
        assert_eq!(pinned_buckets(&config, &b.id), ["partner-exports"]);

        f.manager.remove(&a.id).unwrap();
        let config = f.manager.load().unwrap();
        assert_eq!(pinned_buckets(&config, &a.id), Vec::<String>::new());
        assert_eq!(pinned_buckets(&config, &b.id), ["partner-exports"]);
        assert!(
            !f.config_text().contains("shared-data"),
            "{}",
            f.config_text()
        );

        // A new account never inherits a removed account's pins.
        f.manager.remove(&b.id).unwrap();
        assert!(!f.config_text().contains("pinned_buckets"));
    }

    #[test]
    fn remove_drops_the_bookmarks_of_the_account() {
        use crate::bookmarks;

        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        let store = f.manager.config_store();
        bookmarks::add(store, &a.id, "reports", "2024/", None).unwrap();
        bookmarks::add(store, &b.id, "photos", "", None).unwrap();
        f.manager.remove(&a.id).unwrap();
        let left = bookmarks::list(store).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].bucket, "photos");
        f.manager.remove(&b.id).unwrap();
        assert!(!f.config_text().contains("bookmarks"));
    }

    #[test]
    fn remove_works_while_locked_and_failed_deletes_are_purged_later() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        f.manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        f.manager.lock();
        f.store.fail_nth_delete(1);
        f.manager.remove(&a.id).unwrap();
        assert!(f.manager.list().unwrap().accounts.is_empty());
        // The secret could not be deleted: it is journaled, then purged.
        assert_eq!(f.store.inner.len(), 1);
        assert!(f.journal_exists());
        f.manager.purge_leftovers().unwrap();
        assert!(f.store.inner.is_empty());
        assert!(!f.journal_exists());
    }

    #[test]
    fn master_password_lifecycle_covers_every_secret() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        f.manager
            .update(
                &a.id,
                &updating(|u| {
                    u.session_token = Some(TOKEN.into());
                }),
            )
            .unwrap();
        f.manager.set_proxy_password(Some(PROXY_PASSWORD)).unwrap();

        // Set.
        let status = f
            .manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        assert_eq!(
            status,
            SecurityStatus {
                master_password_set: true,
                unlocked: true
            }
        );
        assert_eq!(f.store.inner.len(), 4);
        for (_, value) in f.store.raw() {
            let blob = STANDARD.decode(value).unwrap();
            assert_eq!(&blob[..2], &[0xBB, 0x01], "every value is an envelope");
        }
        assert_eq!(f.secret_of(&a.id), SECRET_A);

        // Another process sees it locked.
        let other = f.reopen();
        assert!(!other.security_status().unwrap().unlocked);
        assert_eq!(kind(other.account_credentials(&b.id)), ErrorKind::Locked);
        assert_eq!(
            kind(f.manager.set_master_password(None, Some(PASSWORD_2))),
            ErrorKind::Usage
        );

        // Wrong password: nothing modified, secrets still readable. Nothing
        // means the configuration, the keychain (not a single write or
        // delete, not even of values that are not referenced) and the journal.
        let before = f.snapshot();
        assert_eq!(kind(other.unlock(PASSWORD_2)), ErrorKind::Auth);
        assert_eq!(
            kind(other.set_master_password(Some(PASSWORD_2), Some("x"))),
            ErrorKind::Auth
        );
        assert_eq!(
            kind(other.set_master_password(Some(PASSWORD_2), None)),
            ErrorKind::Auth
        );
        // Also while a value sits where the change would have written its
        // next generation (another configuration's, or a leftover): it is
        // not deleted by a failed attempt, and no journal is written.
        let referenced = SecretRef::parse(&f.manager.get(&b.id).unwrap().secret_ref).unwrap();
        let next = referenced.next().unwrap();
        f.store
            .inner
            .set(next.as_str(), "next generation of B")
            .unwrap();
        let before_with_next = f.snapshot();
        assert_eq!(
            kind(other.set_master_password(Some(PASSWORD_2), Some("x"))),
            ErrorKind::Auth
        );
        assert_eq!(
            kind(other.set_master_password(Some(PASSWORD_2), None)),
            ErrorKind::Auth
        );
        assert_eq!(f.snapshot(), before_with_next);
        f.store.inner.delete(next.as_str()).unwrap();
        assert_eq!(f.snapshot(), before);
        assert!(!f.journal_exists());
        assert!(!other.security_status().unwrap().unlocked);
        other.unlock(PASSWORD_1).unwrap();
        assert_eq!(f.secret_of(&b.id), SECRET_B);
        let (proxy, password) = other.proxy_credentials().unwrap();
        assert!(proxy.password_ref.is_some());
        assert!(password.is_none(), "the proxy is disabled");

        // Change, then remove.
        other
            .set_master_password(Some(PASSWORD_1), Some(PASSWORD_2))
            .unwrap();
        assert!(!f.manager.security_status().unwrap().unlocked);
        f.manager.unlock(PASSWORD_2).unwrap();
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        f.manager
            .set_master_password(Some(PASSWORD_2), None)
            .unwrap();
        // A running process never takes the removal of the master password
        // from the file on trust (anyone who can edit it could do the same):
        // it locks, and stays locked until the file is trusted again.
        assert_eq!(
            other.security_status().unwrap(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
        assert_eq!(kind(other.unlock(PASSWORD_2)), ErrorKind::Conflict);
        assert_eq!(kind(other.account_credentials(&a.id)), ErrorKind::Locked);
        // A restart takes the file as it is: no master password.
        let restarted = f.reopen();
        assert_eq!(
            restarted.security_status().unwrap(),
            SecurityStatus {
                master_password_set: false,
                unlocked: true
            }
        );
        let (_, credentials) = restarted.account_credentials(&a.id).unwrap();
        assert_eq!(credentials.secret_access_key, SECRET_A);
        assert_eq!(credentials.session_token.as_deref(), Some(TOKEN));
        assert_eq!(f.store.inner.len(), 4);
        assert!(!f.journal_exists());
    }

    #[test]
    fn no_secret_bytes_on_disk() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        f.manager
            .update(
                &a.id,
                &updating(|u| {
                    u.session_token = Some(TOKEN.into());
                }),
            )
            .unwrap();
        f.manager.update(&a.id, &rotate(ROTATED)).unwrap();
        f.manager.set_proxy_password(Some(PROXY_PASSWORD)).unwrap();
        f.manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        f.manager
            .set_master_password(Some(PASSWORD_1), Some(PASSWORD_2))
            .unwrap();
        f.manager
            .set_master_password(Some(PASSWORD_2), None)
            .unwrap();
        assert_no_secret_on_disk(
            f.dir.path(),
            &[
                SECRET_A,
                ROTATED,
                TOKEN,
                PROXY_PASSWORD,
                PASSWORD_1,
                PASSWORD_2,
            ],
        );
        assert!(f.config_text().contains(&a.id));
    }

    #[test]
    fn failed_rotation_changes_nothing() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        f.manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        let config_before = f.config_text();
        let store_before = f.store.raw();

        // The new secret cannot be written.
        f.store.fail_nth_write(1);
        assert_eq!(
            kind(f.manager.update(&a.id, &rotate(ROTATED))),
            ErrorKind::Io
        );
        assert_eq!(f.config_text(), config_before);
        assert_eq!(f.store.raw(), store_before);
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert!(!f.journal_exists());

        // The configuration becomes unreadable while the new secret is
        // written, so it cannot be saved: the rotation fails, and the new value
        // is removed as soon as the configuration can be read again.
        let config_path = f.dir.path().join("config.toml");
        let corrupt = config_path.clone();
        f.store
            .before_next_write(move || fs::write(&corrupt, "not = [valid toml").unwrap());
        assert_eq!(
            kind(f.manager.update(&a.id, &rotate(ROTATED))),
            ErrorKind::Io
        );
        assert_eq!(f.store.inner.len(), 2);
        assert!(f.journal_exists());
        fs::write(&config_path, &config_before).unwrap();
        f.manager.purge_leftovers().unwrap();
        assert_eq!(f.store.raw(), store_before);
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert!(!f.journal_exists());

        // Rotation while locked is refused before anything is written.
        f.manager.lock();
        assert_eq!(
            kind(f.manager.update(&a.id, &rotate(ROTATED))),
            ErrorKind::Locked
        );
        assert_eq!(f.store.raw(), store_before);
    }

    #[test]
    fn rotation_survives_a_failed_cleanup() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        // Both the finalize and the purge fail to delete the old value.
        f.store.fail_next_deletes(2);
        let rotated = f.manager.update(&a.id, &rotate(ROTATED)).unwrap();
        assert_eq!(f.secret_of(&a.id), ROTATED);
        // The old value could not be deleted yet.
        assert_eq!(f.store.inner.len(), 2);
        assert!(f.journal_exists());
        // A restart purges it.
        f.reopen().purge_leftovers().unwrap();
        assert_eq!(f.store.inner.references(), vec![rotated.secret_ref]);
        assert!(!f.journal_exists());
    }

    #[test]
    fn interrupted_change_is_cleaned_up_by_the_journal() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        // A process died after writing a new generation that never got
        // referenced, and after persisting a rotation whose old value it
        // never deleted.
        let orphan = SecretRef::parse(&a.secret_ref).unwrap().next().unwrap();
        f.store.inner.set(orphan.as_str(), "orphan").unwrap();
        let journal = Journal {
            version: JOURNAL_VERSION,
            references: [orphan.to_string(), a.secret_ref.clone()].into(),
        };
        fs::write(
            f.dir.path().join(SECRETS_JOURNAL_FILE),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        f.reopen().purge_leftovers().unwrap();
        // The referenced value stays, the orphan is gone.
        assert_eq!(f.store.inner.references(), vec![a.secret_ref.clone()]);
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert!(!f.journal_exists());
    }

    #[test]
    fn proxy_password_set_replace_clear() {
        let f = Fixture::new();
        f.manager.set_proxy_password(Some("first")).unwrap();
        f.manager.set_proxy_password(Some("second")).unwrap();
        let id = f.installation_id();
        let own = format!("proxy-{id}/password/2");
        let mut settings = f.manager.load().unwrap().settings;
        assert_eq!(settings.proxy.password_ref.as_deref(), Some(own.as_str()));
        assert_eq!(f.store.inner.references(), vec![own.clone()]);
        settings.proxy.enabled = true;
        settings.proxy.host = "proxy.example.test".into();
        settings.proxy.username = Some("user".into());
        // A caller cannot redirect the password reference.
        settings.proxy.password_ref = Some("someone/secret/1".into());
        f.manager.save_settings(&settings).unwrap();
        let (proxy, password) = f.manager.proxy_credentials().unwrap();
        assert_eq!(proxy.password_ref.as_deref(), Some(own.as_str()));
        assert_eq!(password.unwrap().expose(), "second");

        // Editing config.toml directly is not the API: a reference that is not
        // the proxy password of this installation is rejected when it is used,
        // whatever it points at.
        let account = f.add("Work", SECRET_A);
        let (_other_dir, other) = f.another_installation();
        other.set_proxy_password(Some("theirs")).unwrap();
        let theirs = other.load().unwrap().settings.proxy.password_ref.unwrap();
        for edited in [
            account.secret_ref.clone(),
            format!("{}/session-token/1", account.id),
            theirs.clone(),
            format!("proxy-{id}/secret/2"),
            format!("proxy-{id}-x/password/2"),
            format!("{id}/password/2"),
            "proxy/secret/1".to_owned(),
            "not a reference".to_owned(),
        ] {
            f.tamper(|config| config.settings.proxy.password_ref = Some(edited.clone()));
            let error = f.manager.proxy_credentials().unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{edited}: {error}");
            assert!(!error.to_string().contains(SECRET_A), "{error}");
        }
        // ... and it is replaced, never deleted: the secret it pointed at and
        // the other installation's password are untouched. (What the edited
        // reference stopped pointing at stays in the keychain.)
        f.tamper(|config| config.settings.proxy.password_ref = Some(account.secret_ref.clone()));
        f.manager.set_proxy_password(Some("third")).unwrap();
        let replaced = f
            .manager
            .load()
            .unwrap()
            .settings
            .proxy
            .password_ref
            .unwrap();
        assert_eq!(replaced, format!("proxy-{id}/password/1"));
        assert_eq!(f.secret_of(&account.id), SECRET_A);
        assert_eq!(other.proxy_password_value(), "theirs");
        let (_, password) = f.manager.proxy_credentials().unwrap();
        assert_eq!(password.unwrap().expose(), "third");
        // Clearing an edited reference only drops it from the configuration.
        f.tamper(|config| config.settings.proxy.password_ref = Some(theirs.clone()));
        f.manager.set_proxy_password(None).unwrap();
        assert_eq!(f.manager.load().unwrap().settings.proxy.password_ref, None);
        assert_eq!(other.proxy_password_value(), "theirs");
    }

    #[test]
    fn shortener_token_set_replace_clear_and_survives_settings_saves() {
        let f = Fixture::new();
        f.manager.set_shortener_token(Some("first-token")).unwrap();
        f.manager.set_shortener_token(Some("second-token")).unwrap();
        let id = f.installation_id();
        let own = format!("shortener-{id}/token/2");
        let mut settings = f.manager.load().unwrap().settings;
        assert_eq!(settings.shortener.token_ref.as_deref(), Some(own.as_str()));
        assert_eq!(f.store.inner.references(), vec![own.clone()]);

        // The token is not part of the settings the UI saves, and a caller
        // cannot redirect the reference.
        settings.shortener.provider = ShortenerProvider::Bitly;
        settings.shortener.token_ref = Some("someone/secret/1".into());
        f.manager.save_settings(&settings).unwrap();
        let (shortener, token) = f.manager.shortener_credentials().unwrap();
        assert_eq!(shortener.provider, ShortenerProvider::Bitly);
        assert_eq!(shortener.token_ref.as_deref(), Some(own.as_str()));
        assert_eq!(token.unwrap().expose(), "second-token");

        // No provider: the token is not even read.
        settings.shortener.provider = ShortenerProvider::None;
        f.manager.save_settings(&settings).unwrap();
        assert!(f.manager.shortener_credentials().unwrap().1.is_none());

        // An edited reference is refused when used, replaced when set again.
        let account = f.add("Work", SECRET_A);
        settings.shortener.provider = ShortenerProvider::Bitly;
        f.manager.save_settings(&settings).unwrap();
        f.tamper(|config| config.settings.shortener.token_ref = Some(account.secret_ref.clone()));
        let error = f.manager.shortener_credentials().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(!error.to_string().contains(SECRET_A));
        f.manager.set_shortener_token(Some("third-token")).unwrap();
        assert_eq!(f.secret_of(&account.id), SECRET_A);
        assert_eq!(
            f.manager
                .shortener_credentials()
                .unwrap()
                .1
                .unwrap()
                .expose(),
            "third-token"
        );

        f.manager.set_shortener_token(None).unwrap();
        assert_eq!(f.manager.load().unwrap().settings.shortener.token_ref, None);
        assert!(f.manager.shortener_credentials().unwrap().1.is_none());
        assert!(f.manager.set_shortener_token(Some("line\nbreak")).is_err());
    }

    #[test]
    fn shortener_token_follows_a_master_password_change() {
        let f = Fixture::new();
        f.manager.set_shortener_token(Some("token-value")).unwrap();
        let mut settings = f.manager.load().unwrap().settings;
        settings.shortener.provider = ShortenerProvider::Tinyurl;
        f.manager.save_settings(&settings).unwrap();
        f.manager.set_master_password(None, Some("master")).unwrap();
        for (_, stored) in f.store.raw() {
            assert!(!stored.contains("token-value"), "{stored}");
        }
        f.manager.lock();
        assert_eq!(
            f.manager.shortener_credentials().unwrap_err().kind(),
            ErrorKind::Locked
        );
        f.manager.unlock("master").unwrap();
        assert_eq!(
            f.manager
                .shortener_credentials()
                .unwrap()
                .1
                .unwrap()
                .expose(),
            "token-value"
        );
        f.manager.set_master_password(Some("master"), None).unwrap();
        assert_eq!(
            f.manager
                .shortener_credentials()
                .unwrap()
                .1
                .unwrap()
                .expose(),
            "token-value"
        );
    }

    fn enable_proxy(manager: &AccountManager) {
        let mut settings = manager.load().unwrap().settings;
        settings.proxy.enabled = true;
        settings.proxy.host = "proxy.example.test".into();
        settings.proxy.username = Some("user".into());
        manager.save_settings(&settings).unwrap();
    }

    fn proxy_password(manager: &AccountManager) -> Result<String> {
        let (_, password) = manager.proxy_credentials()?;
        Ok(password.unwrap().expose().to_owned())
    }

    #[test]
    fn references_are_checked_against_their_use() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        let with_token = |id: &str, token: &str| {
            f.manager
                .update(
                    id,
                    &updating(|u| {
                        u.session_token = Some(token.into());
                    }),
                )
                .unwrap()
        };
        let a = with_token(&a.id, TOKEN);
        let b = with_token(&b.id, "token of account B");
        f.manager.set_proxy_password(Some(PROXY_PASSWORD)).unwrap();
        let proxy_reference = f
            .manager
            .load()
            .unwrap()
            .settings
            .proxy
            .password_ref
            .unwrap();
        let (b_secret, b_token) = (b.secret_ref.clone(), b.session_token_ref.clone().unwrap());
        let edits: Vec<(&str, Edit)> = vec![
            (
                "another account's secret",
                Box::new(move |account: &mut Account| account.secret_ref = b_secret.clone()),
            ),
            (
                "the secret as the session token",
                Box::new(|account: &mut Account| {
                    account.session_token_ref = Some(account.secret_ref.clone());
                }),
            ),
            (
                "another account's session token",
                Box::new(move |account: &mut Account| {
                    account.session_token_ref = Some(b_token.clone());
                }),
            ),
            (
                "the session token as the secret",
                Box::new(|account: &mut Account| {
                    account.secret_ref = account.session_token_ref.clone().unwrap();
                }),
            ),
            (
                "the proxy password as the secret",
                Box::new(move |account: &mut Account| {
                    account.secret_ref = proxy_reference.clone();
                }),
            ),
            (
                "something that is not a reference",
                Box::new(|account: &mut Account| account.secret_ref = "not a reference".into()),
            ),
        ];
        for (what, edit) in &edits {
            let saved = f.config_text();
            f.tamper(|config| edit(&mut config.accounts[0]));
            let error = f.manager.account_credentials(&a.id).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{what}: {error}");
            for secret in [
                SECRET_A,
                SECRET_B,
                TOKEN,
                "token of account B",
                PROXY_PASSWORD,
            ] {
                assert!(!error.to_string().contains(secret), "{what}: {error}");
            }
            // The other account is not affected.
            assert_eq!(f.secret_of(&b.id), SECRET_B, "{what}");
            fs::write(f.dir.path().join("config.toml"), saved).unwrap();
            assert_eq!(f.secret_of(&a.id), SECRET_A, "{what}: restored");
        }
    }

    #[test]
    fn an_edited_reference_is_never_deleted_or_rewritten() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        f.tamper(|config| config.accounts[0].secret_ref = b.secret_ref.clone());

        // A master password cannot be set over it: nothing would be wrong,
        // but a secret would be silently left out of the protection.
        let before = f.snapshot();
        let error = f
            .manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{error}");
        assert!(error.to_string().contains("Work"), "{error}");
        assert_eq!(f.snapshot(), before);
        assert!(!f.journal_exists());

        // Removing the account deletes its own references only: B's secret
        // stays, and so does what A's own value used to be.
        f.manager.remove(&a.id).unwrap();
        assert_eq!(f.secret_of(&b.id), SECRET_B);
        assert!(f.store.inner.references().contains(&b.secret_ref));
        assert!(!f.journal_exists());
    }

    #[test]
    fn entering_a_secret_again_repairs_an_edited_reference() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let b = f.add("Personal", SECRET_B);
        f.tamper(|config| config.accounts[0].secret_ref = b.secret_ref.clone());
        let repaired = f.manager.update(&a.id, &rotate(ROTATED)).unwrap();
        // The new value does not overwrite the account's own old one (which
        // an edited reference stopped pointing at, and stays in the keychain);
        // B is untouched.
        assert_eq!(repaired.secret_ref, format!("{}/secret/2", a.id));
        assert_eq!(f.secret_of(&a.id), ROTATED);
        assert_eq!(f.secret_of(&b.id), SECRET_B);
        assert!(f.store.inner.references().contains(&a.secret_ref));
        // The master password can be set now.
        f.manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        assert_eq!(f.secret_of(&a.id), ROTATED);
        assert_eq!(f.secret_of(&b.id), SECRET_B);
    }

    #[test]
    fn installations_sharing_one_keychain_do_not_touch_each_other() {
        let f = Fixture::new();
        let (_dir_b, b) = f.another_installation();
        enable_proxy(&f.manager);
        enable_proxy(&b);

        f.manager.set_proxy_password(Some("password-of-A")).unwrap();
        b.set_proxy_password(Some("password-of-B")).unwrap();
        assert_eq!(proxy_password(&f.manager).unwrap(), "password-of-A");
        assert_eq!(proxy_password(&b).unwrap(), "password-of-B");
        let id_a = f.installation_id();
        let id_b = b.load().unwrap().installation_id.unwrap();
        assert_ne!(id_a, id_b);
        assert_eq!(f.store.inner.references(), {
            let mut references = vec![
                format!("proxy-{id_a}/password/1"),
                format!("proxy-{id_b}/password/1"),
            ];
            references.sort();
            references
        });

        // B changes its password and sets a master password: A is unaffected,
        // and A's later changes do not disturb B.
        b.set_proxy_password(Some("password-of-B-2")).unwrap();
        b.set_master_password(None, Some("master of B")).unwrap();
        assert_eq!(proxy_password(&f.manager).unwrap(), "password-of-A");
        f.manager
            .set_proxy_password(Some("password-of-A-2"))
            .unwrap();
        assert_eq!(proxy_password(&f.manager).unwrap(), "password-of-A-2");
        assert_eq!(proxy_password(&b).unwrap(), "password-of-B-2");
        f.manager.set_proxy_password(None).unwrap();
        assert_eq!(proxy_password(&b).unwrap(), "password-of-B-2");
        assert!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .is_none()
        );

        // The same after rotating account secrets in each.
        let work = f.add("Work", SECRET_A);
        let mut theirs = input("Work", SECRET_B);
        theirs.access_key_id = "AKIAOTHERINSTALLATION".to_owned();
        let their_work = b.add(&theirs).unwrap();
        f.manager.update(&work.id, &rotate(ROTATED)).unwrap();
        assert_eq!(f.secret_of(&work.id), ROTATED);
        assert_eq!(
            b.account_credentials(&their_work.id)
                .unwrap()
                .1
                .secret_access_key,
            SECRET_B
        );
        f.manager.remove(&work.id).unwrap();
        assert_eq!(
            b.account_credentials(&their_work.id)
                .unwrap()
                .1
                .secret_access_key,
            SECRET_B
        );
    }

    #[test]
    fn a_new_slot_never_overwrites_a_value_that_is_already_there() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        // Orphans of another configuration that shares the keychain (or of an
        // earlier failed cleanup) sit where first generations would go.
        let token_slot = format!("{}/session-token/1", a.id);
        f.store.inner.set(&token_slot, "someone else's").unwrap();
        let updated = f
            .manager
            .update(
                &a.id,
                &updating(|u| {
                    u.session_token = Some(TOKEN.into());
                }),
            )
            .unwrap();
        assert_eq!(
            updated.session_token_ref.as_deref(),
            Some(format!("{}/session-token/2", a.id).as_str())
        );
        let (_, credentials) = f.manager.account_credentials(&a.id).unwrap();
        assert_eq!(credentials.session_token.as_deref(), Some(TOKEN));
        // What is in the way is left alone.
        assert_eq!(
            f.store.inner.get(&token_slot).unwrap().unwrap().expose(),
            "someone else's"
        );

        // A proxy password never takes a slot that is occupied either.
        f.manager.set_proxy_password(Some("first")).unwrap();
        let id = f.installation_id();
        f.manager.set_proxy_password(None).unwrap();
        let taken = format!("proxy-{id}/password/1");
        f.store.inner.set(&taken, "someone else's").unwrap();
        f.manager.set_proxy_password(Some("second")).unwrap();
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some(format!("proxy-{id}/password/2").as_str())
        );
        assert_eq!(
            f.store.inner.get(&taken).unwrap().unwrap().expose(),
            "someone else's"
        );
    }

    #[test]
    fn a_configuration_without_an_installation_id_gets_one_at_its_first_write() {
        let f = Fixture::new();
        assert_eq!(f.manager.load().unwrap().installation_id, None);
        assert!(!f.dir.path().join("config.toml").exists());
        f.add("Work", SECRET_A);
        let id = f.installation_id();
        assert!(
            f.config_text()
                .contains(&format!("installation_id = \"{id}\""))
        );
        // Later changes keep it, from this process and from another.
        f.manager
            .set_default(&f.manager.list().unwrap().accounts[0].id)
            .unwrap();
        f.manager.set_proxy_password(Some("x")).unwrap();
        f.reopen().set_proxy_password(Some("y")).unwrap();
        assert_eq!(f.installation_id(), id);
    }

    /// A configuration as the previous version left it: no installation id,
    /// and the proxy password (when `stored`) under the owner every
    /// configuration shared. With a master password, the manager is locked.
    fn legacy_fixture(master_password: bool, stored: bool) -> (Fixture, Account) {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        enable_proxy(&f.manager);
        if master_password {
            f.manager
                .set_master_password(None, Some(PASSWORD_1))
                .unwrap();
        }
        let legacy = SecretRef::parse("proxy/password/1").unwrap();
        if stored {
            // Sealed under the master password, if there is one.
            let change = f
                .manager
                .vault()
                .prepare_secret_changes(vec![SecretChange::Put {
                    reference: legacy,
                    replaces: None,
                    value: SecretString::from(PROXY_PASSWORD),
                }])
                .unwrap();
            assert!(change.finalize().unwrap().is_complete());
        }
        f.tamper(|config| config.settings.proxy.password_ref = Some("proxy/password/1".into()));
        let id = f.installation_id();
        let text = f
            .config_text()
            .replace(&format!("installation_id = \"{id}\"\n"), "");
        fs::write(f.dir.path().join("config.toml"), text).unwrap();
        assert_eq!(f.manager.load().unwrap().installation_id, None);
        if master_password {
            f.manager.lock();
        }
        (f, a)
    }

    #[test]
    fn a_legacy_proxy_password_keeps_working_and_moves_to_the_installation() {
        let (f, a) = legacy_fixture(false, true);
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
        let before = f.store.inner.len();

        f.manager.migrate_legacy_references().unwrap();
        let id = f.installation_id();
        let moved = format!("proxy-{id}/password/1");
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some(moved.as_str())
        );
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
        assert_eq!(f.store.inner.len(), before);
        let references = f.store.inner.references();
        assert!(!references.contains(&"proxy/password/1".to_owned()));
        assert!(references.contains(&moved));
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert!(!f.journal_exists());
        // Nothing left to move.
        let after = f.snapshot();
        f.manager.migrate_legacy_references().unwrap();
        assert_eq!(f.snapshot(), after);
    }

    #[test]
    fn migrating_waits_for_the_key_and_happens_at_unlock() {
        let (f, _) = legacy_fixture(true, true);
        // Locked: nothing is read or written.
        let before = f.snapshot();
        f.manager.migrate_legacy_references().unwrap();
        assert_eq!(f.snapshot(), before);
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some("proxy/password/1")
        );

        f.manager.unlock(PASSWORD_1).unwrap();
        let id = f.installation_id();
        let moved = format!("proxy-{id}/password/1");
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some(moved.as_str())
        );
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
        assert!(
            !f.store
                .inner
                .references()
                .contains(&"proxy/password/1".to_owned())
        );
        // Every stored value is still an envelope, bound to its new reference.
        for (reference, value) in f.store.raw() {
            assert_eq!(
                &STANDARD.decode(&value).unwrap()[..2],
                &[0xBB, 0x01],
                "{reference}"
            );
        }
        assert!(!f.journal_exists());
    }

    #[test]
    fn a_failed_migration_loses_nothing_and_leaves_no_leftovers_behind() {
        let (f, _) = legacy_fixture(false, true);
        let before = f.snapshot();

        // The new value cannot be written: the legacy value and the file are
        // untouched, and the journal is cleaned up.
        f.store.fail_nth_write(1);
        assert_eq!(kind(f.manager.migrate_legacy_references()), ErrorKind::Io);
        let id = f.installation_id();
        assert_eq!(f.store.raw(), before.1);
        assert!(!f.journal_exists());
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);

        // The legacy value cannot be deleted afterwards: the migration still
        // took effect, and the leftover is journaled and purged later.
        f.store.fail_next_deletes(2);
        f.manager.migrate_legacy_references().unwrap();
        let moved = format!("proxy-{id}/password/1");
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some(moved.as_str())
        );
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
        assert!(
            f.store
                .inner
                .references()
                .contains(&"proxy/password/1".to_owned())
        );
        assert!(f.journal_exists());
        f.reopen().purge_leftovers().unwrap();
        assert!(
            !f.store
                .inner
                .references()
                .contains(&"proxy/password/1".to_owned())
        );
        assert!(!f.journal_exists());
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
    }

    #[test]
    fn a_missing_legacy_value_is_left_alone() {
        let (f, _) = legacy_fixture(false, false);
        let before = f.snapshot();
        f.manager.migrate_legacy_references().unwrap();
        assert_eq!(f.snapshot(), before);
        assert_eq!(
            f.manager
                .load()
                .unwrap()
                .settings
                .proxy
                .password_ref
                .as_deref(),
            Some("proxy/password/1")
        );
        assert_eq!(kind(f.manager.proxy_credentials()), ErrorKind::NotFound);
        // Entering it again stores it under the installation's own owner.
        f.manager.set_proxy_password(Some(PROXY_PASSWORD)).unwrap();
        let id = f.installation_id();
        assert_eq!(
            f.manager.load().unwrap().settings.proxy.password_ref,
            Some(format!("proxy-{id}/password/1"))
        );
        assert_eq!(proxy_password(&f.manager).unwrap(), PROXY_PASSWORD);
    }

    #[test]
    fn a_legacy_proxy_password_is_replaced_and_cleared_like_a_current_one() {
        for clear in [false, true] {
            let (f, _) = legacy_fixture(false, true);
            let password = (!clear).then_some("new password");
            f.manager.set_proxy_password(password).unwrap();
            assert!(
                !f.store
                    .inner
                    .references()
                    .contains(&"proxy/password/1".to_owned())
            );
            assert_eq!(
                f.manager
                    .load()
                    .unwrap()
                    .settings
                    .proxy
                    .password_ref
                    .is_some(),
                !clear
            );
            assert!(!f.journal_exists());
        }
    }

    #[test]
    fn a_damaged_journal_is_set_aside_and_reported() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        let journal = f.dir.path().join(SECRETS_JOURNAL_FILE);
        let aside = f
            .dir
            .path()
            .join(format!("{SECRETS_JOURNAL_FILE}.damaged-1"));
        let bad_versions = r#"{"version":9,"references":["x/secret/2"]}"#;
        let bad_entry = r#"{"version":1,"references":["not a reference"]}"#;
        for damaged in ["{not json", "", bad_versions, bad_entry] {
            fs::write(&journal, damaged).unwrap();
            let before = f.store.raw();
            // The change that finds it fails before it writes anything.
            let error = f.manager.add(&input("Other", SECRET_B)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Io, "{damaged:?}: {error}");
            let message = error.to_string();
            assert!(message.contains("damaged"), "{message}");
            assert!(message.contains(".damaged-1"), "{message}");
            assert_eq!(f.store.raw(), before);
            assert_eq!(f.manager.list().unwrap().accounts.len(), 1);
            // It is kept, not discarded.
            assert_eq!(fs::read_to_string(&aside).unwrap(), damaged);
            assert!(!journal.exists());
            // The next attempt goes through.
            let added = f.manager.add(&input("Other", SECRET_B)).unwrap();
            f.manager.remove(&added.id).unwrap();
            fs::remove_file(&aside).unwrap();
        }
        // A purge reports it as well; several damaged copies are all kept.
        fs::write(&journal, "{not json").unwrap();
        assert_eq!(kind(f.manager.purge_leftovers()), ErrorKind::Io);
        fs::write(&journal, "{still not json").unwrap();
        assert_eq!(kind(f.manager.purge_leftovers()), ErrorKind::Io);
        assert_eq!(fs::read_to_string(&aside).unwrap(), "{not json");
        assert_eq!(
            fs::read_to_string(
                f.dir
                    .path()
                    .join(format!("{SECRETS_JOURNAL_FILE}.damaged-2"))
            )
            .unwrap(),
            "{still not json"
        );
        f.manager.purge_leftovers().unwrap();
        assert_eq!(f.secret_of(&a.id), SECRET_A);
    }

    #[test]
    fn the_journal_is_written_whole_and_leaves_no_temporary_files() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        // A failed deletion keeps the journal around.
        f.store.fail_next_deletes(2);
        f.manager.update(&a.id, &rotate(ROTATED)).unwrap();
        let text = f.journal_text().unwrap();
        let journal: Journal = serde_json::from_str(&text).unwrap();
        assert_eq!(journal.version, JOURNAL_VERSION);
        assert_eq!(journal.references, BTreeSet::from([a.secret_ref.clone()]));
        let mut names: Vec<String> = fs::read_dir(f.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "config.toml".to_owned(),
                "config.toml.lock".to_owned(),
                SECRETS_JOURNAL_FILE.to_owned(),
                SECRETS_LOCK_FILE.to_owned(),
            ]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(f.dir.path().join(SECRETS_JOURNAL_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        f.manager.purge_leftovers().unwrap();
        assert!(!f.journal_exists());
    }

    #[test]
    fn settings_are_validated_including_the_proxy() {
        let f = Fixture::new();
        let mut settings = f.manager.load().unwrap().settings;
        settings.proxy.enabled = true;
        settings.proxy.host = "alice:s3cr3t-proxy-pw@proxy.example.com".into();
        let error = f.manager.save_settings(&settings).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(!error.to_string().contains("s3cr3t-proxy-pw"), "{error}");
        assert!(
            !f.dir.path().join("config.toml").exists(),
            "nothing was saved"
        );

        // The stricter checks of the proxy page apply here, too.
        settings.proxy.host = "proxy.example.com".into();
        settings.proxy.bypass = vec!["host:8080".into()];
        assert_eq!(kind(f.manager.save_settings(&settings)), ErrorKind::Usage);
        settings.proxy.bypass = vec![];
        settings.proxy.username = Some("user:name".into());
        assert_eq!(kind(f.manager.save_settings(&settings)), ErrorKind::Usage);
        settings.proxy.username = Some("user".into());
        f.manager.save_settings(&settings).unwrap();
        // A proxy that is off is only held to the general rules.
        settings.proxy.enabled = false;
        settings.proxy.bypass = vec!["host:8080".into()];
        f.manager.save_settings(&settings).unwrap();
        // The host cannot hold credentials even then.
        settings.proxy.host = "alice:pw@proxy".into();
        assert_eq!(kind(f.manager.save_settings(&settings)), ErrorKind::Usage);
    }

    #[test]
    fn a_vanished_master_password_section_locks_instead_of_switching_to_plain_mode() {
        let f = Fixture::new();
        let a = f.add("Work", SECRET_A);
        f.manager
            .set_master_password(None, Some(PASSWORD_1))
            .unwrap();
        let section = f.manager.load().unwrap().security.unwrap();
        assert!(f.manager.security_status().unwrap().unlocked);

        f.tamper(|config| config.security = None);
        let locked = SecurityStatus {
            master_password_set: true,
            unlocked: false,
        };
        assert_eq!(f.manager.security_status().unwrap(), locked);
        let before = f.snapshot();
        // Nothing can be stored, in plain text or otherwise, and nothing
        // can be read; unlocking is refused with an explanation.
        assert_eq!(
            kind(f.manager.add(&input("Other", SECRET_B))),
            ErrorKind::Locked
        );
        assert_eq!(
            kind(f.manager.account_credentials(&a.id)),
            ErrorKind::Locked
        );
        assert_eq!(
            kind(f.manager.set_proxy_password(Some("x"))),
            ErrorKind::Locked
        );
        let error = f.manager.unlock(PASSWORD_1).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
        assert_eq!(
            kind(f.manager.set_master_password(None, Some(PASSWORD_2))),
            ErrorKind::Usage
        );
        assert_eq!(f.store.raw(), before.1);
        assert_eq!(f.store.operations(), before.2);
        for (_, value) in f.store.raw() {
            assert!(!value.contains(SECRET_B), "nothing in plain text");
        }

        // Putting the section back (a backup) makes the file trustworthy again.
        f.tamper(|config| config.security = Some(section.clone()));
        assert_eq!(f.manager.security_status().unwrap(), locked);
        f.manager.unlock(PASSWORD_1).unwrap();
        assert_eq!(f.secret_of(&a.id), SECRET_A);

        // The master password itself repairs a file that lost the section for
        // good: the key derivation settings are only in the vault's memory.
        f.tamper(|config| config.security = None);
        assert_eq!(f.manager.security_status().unwrap(), locked);
        assert_eq!(
            kind(f.manager.set_master_password(Some(PASSWORD_2), Some("x"))),
            ErrorKind::Auth
        );
        f.manager
            .set_master_password(Some(PASSWORD_1), Some(PASSWORD_2))
            .unwrap();
        assert!(f.manager.load().unwrap().security.is_some());
        assert_eq!(f.secret_of(&a.id), SECRET_A);
        assert!(f.reopen().security_status().unwrap().master_password_set);
        let restarted = f.reopen();
        restarted.unlock(PASSWORD_2).unwrap();
        assert_eq!(
            restarted
                .account_credentials(&a.id)
                .unwrap()
                .1
                .secret_access_key,
            SECRET_A
        );
    }

    #[test]
    fn debug_output_has_no_secrets() {
        let f = Fixture::new();
        f.add("Work", SECRET_A);
        let rendered = format!("{:?} {:?}", f.manager, input("x", SECRET_B));
        assert!(!rendered.contains(SECRET_A) && !rendered.contains(SECRET_B));
    }
}
