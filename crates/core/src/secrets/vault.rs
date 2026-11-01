//! The master-password vault: the policy layer over a [`SecretStore`].
//!
//! # States
//!
//! | `SecurityConfig` | key in memory | state | reading a secret |
//! |---|---|---|---|
//! | none | none | no master password | stored value as-is |
//! | present | none | locked | [`ErrorKind::Locked`](crate::ErrorKind::Locked) |
//! | present | present | unlocked | envelope opened with the key |
//!
//! [`Vault::unlock`] derives the key, checks it against the verifier and
//! only then installs it; a wrong password is [`ErrorKind::Auth`](crate::ErrorKind::Auth) and changes
//! nothing (the store is not even touched). [`Vault::lock`] drops the key,
//! which is wiped from memory once the last in-flight read using it ends.
//!
//! A protected vault never becomes a plain one on its own: only a verified
//! [`Vault::prepare_remove_master_password`] followed by
//! [`PendingChange::finalize`] does that. When the persisted configuration
//! loses its master-password section behind the vault's back
//! ([`Vault::sync_config`]), the vault locks and refuses to unlock until the
//! section is back or the password is changed or removed.
//!
//! # Two-phase changes
//!
//! Every change of stored secrets is crash-safe and lossless. The vault never
//! overwrites or deletes a value that the persisted configuration may still
//! reference:
//!
//! 1. `prepare_*` writes the new values under **new** references (the next
//!    generation, see [`SecretRef::next`]) and returns a [`PendingChange`].
//!    Old values stay untouched. If any write fails, the new values written
//!    so far are deleted again and the error is returned.
//! 2. The caller atomically persists what the change reports: the new
//!    [`SecurityConfig`] ([`PendingChange::security_update`]) and the new
//!    references ([`PendingChange::new_reference`]).
//! 3. On success the caller calls [`PendingChange::finalize`]: the vault
//!    switches to the new configuration and key, then deletes the obsolete
//!    values. If persisting failed, the caller calls [`PendingChange::abort`]
//!    instead, which deletes the new values.
//!
//! A crash before step 2 completes leaves the old configuration with all its
//! values intact (plus unreferenced new values); a crash after it leaves the
//! new configuration with all its values intact (plus unreferenced old
//! values). Unreferenced leftovers are listed by [`PendingChange::written`] and
//! [`PendingChange::obsolete`]; the caller can record them and remove them
//! later with [`Vault::purge`].
//!
//! Only one change can be pending at a time per vault; preparing another one
//! fails with [`ErrorKind::Conflict`](crate::ErrorKind::Conflict). Dropping a [`PendingChange`] without
//! finalizing or aborting it releases that slot but deletes nothing, because
//! the vault cannot know whether the caller persisted it. Several processes
//! (the desktop app and the CLI) sharing one configuration must serialize
//! their changes, for example with a lock file, since a vault only
//! coordinates the threads of its own process.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::crypto::{self, EnvelopeError, KdfCost, MasterKey};
use super::{SecretRef, SecretStore, SecretString};
use crate::config::SecurityConfig;
use crate::error::{Error, Result};
use crate::model::SecurityStatus;

/// One change of a stored secret, for [`Vault::prepare_secret_changes`].
#[derive(Debug)]
pub enum SecretChange {
    /// Store `value` under `reference`. When `replaces` is set, `reference`
    /// must be `replaces.next()` and the old value is deleted on finalize.
    /// Use [`SecretRef::first`] for a slot that holds no value yet.
    Put {
        /// Where the new value is written.
        reference: SecretRef,
        /// The current value of the same slot, if any.
        replaces: Option<SecretRef>,
        /// The new value; must not be empty.
        value: SecretString,
    },
    /// Delete the value under `reference` on finalize.
    Remove {
        /// The value to delete.
        reference: SecretRef,
    },
}

/// What a [`PendingChange`] does to the persisted [`SecurityConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityUpdate<'a> {
    /// Keep the persisted configuration.
    Unchanged,
    /// Persist this configuration (a master password was set or changed).
    Set(&'a SecurityConfig),
    /// Remove the configuration (the master password was removed).
    Removed,
}

/// Result of deleting values that are no longer referenced.
#[derive(Debug, Default)]
#[must_use]
pub struct Cleanup {
    /// References whose values were deleted (or were already gone).
    pub deleted: Vec<SecretRef>,
    /// References that could not be deleted; retry with [`Vault::purge`].
    pub leftover: Vec<SecretRef>,
    /// The first deletion error, if any.
    pub first_error: Option<Error>,
}

impl Cleanup {
    /// Whether every value was deleted.
    pub fn is_complete(&self) -> bool {
        self.leftover.is_empty()
    }
}

/// Master-password policy over a [`SecretStore`]. Cheap to clone; clones
/// share state. All methods block (see the [module docs](super)).
#[derive(Clone)]
pub struct Vault {
    shared: Arc<Shared>,
}

struct Shared {
    store: Arc<dyn SecretStore>,
    kdf_cost: KdfCost,
    state: Mutex<State>,
}

struct State {
    security: Option<SecurityConfig>,
    key: Option<Arc<MasterKey>>,
    /// Bumped whenever `security` changes.
    config_epoch: u64,
    /// Bumped by every `lock`.
    lock_epoch: u64,
    /// Id of the outstanding change, if any.
    pending: Option<u64>,
    next_change_id: u64,
    /// The persisted configuration lost the master-password section while the
    /// vault still has one: see [`Vault::sync_config`].
    diverged: bool,
}

#[derive(Clone, Copy)]
enum Rekey {
    Set,
    Change,
    Remove,
}

impl Vault {
    /// A vault over `store` with the persisted configuration, `None` when no
    /// master password is set. A protected vault starts locked.
    pub fn new(store: Arc<dyn SecretStore>, security: Option<SecurityConfig>) -> Result<Self> {
        Self::with_kdf_cost(store, security, KdfCost::RECOMMENDED)
    }

    /// Like [`Vault::new`], with the Argon2id cost used for passwords set or
    /// changed from now on. Existing configurations keep their own cost until
    /// the password is changed.
    pub fn with_kdf_cost(
        store: Arc<dyn SecretStore>,
        security: Option<SecurityConfig>,
        kdf_cost: KdfCost,
    ) -> Result<Self> {
        crypto::check_cost(kdf_cost)?;
        if let Some(config) = &security {
            crypto::check_security_config(config)?;
        }
        Ok(Self {
            shared: Arc::new(Shared {
                store,
                kdf_cost,
                state: Mutex::new(State {
                    security,
                    key: None,
                    config_epoch: 0,
                    lock_epoch: 0,
                    pending: None,
                    next_change_id: 1,
                    diverged: false,
                }),
            }),
        })
    }

    /// Whether a master password is set and whether secrets are readable.
    pub fn status(&self) -> SecurityStatus {
        let state = self.shared.state();
        let master_password_set = state.security.is_some();
        SecurityStatus {
            master_password_set,
            unlocked: !master_password_set || state.key.is_some(),
        }
    }

    /// The configuration the vault currently works with.
    pub fn security_config(&self) -> Option<SecurityConfig> {
        self.shared.state().security.clone()
    }

    /// Adopts the persisted configuration when another process changed it,
    /// for example when it set or changed the master password. Returns
    /// whether anything changed; a changed configuration starts locked.
    ///
    /// The one change that is not adopted is the loss of the master-password
    /// section: a vault that has a master password never falls back to plain
    /// mode because the file says so (anyone who can edit `config.toml` could
    /// otherwise make the next stored secret unencrypted). Instead it locks
    /// and refuses to unlock ([`ErrorKind::Conflict`](crate::ErrorKind::Conflict)) until the file has the
    /// same section again, or the password is changed or removed through
    /// this vault, or the process restarts and takes the file as it is.
    ///
    /// While a change of this vault is pending the call does nothing: that
    /// change installs its own configuration when it is finalized. Callers
    /// serialize changes across processes, so no other process changes the
    /// configuration meanwhile.
    pub fn sync_config(&self, security: Option<SecurityConfig>) -> Result<bool> {
        if let Some(config) = &security {
            crypto::check_security_config(config)?;
        }
        let mut state = self.shared.state();
        if state.pending.is_some() {
            return Ok(false);
        }
        if state.security == security {
            // The file agrees with the vault again.
            state.diverged = false;
            return Ok(false);
        }
        if state.security.is_some() && security.is_none() {
            if state.diverged {
                return Ok(false);
            }
            state.diverged = true;
            state.key = None;
            state.config_epoch += 1;
            return Ok(true);
        }
        state.security = security;
        state.key = None;
        state.diverged = false;
        state.config_epoch += 1;
        Ok(true)
    }

    /// Whether the persisted configuration lost the master-password section
    /// while this vault still has one (see [`Vault::sync_config`]): the vault
    /// is locked and cannot be unlocked.
    pub fn diverged(&self) -> bool {
        self.shared.state().diverged
    }

    /// Derives the key from `password` and unlocks the vault.
    ///
    /// A wrong password fails with [`ErrorKind::Auth`](crate::ErrorKind::Auth) and modifies nothing:
    /// a locked vault stays locked, an unlocked one stays unlocked. Fails with
    /// [`ErrorKind::Usage`](crate::ErrorKind::Usage) when no master password is set.
    pub fn unlock(&self, password: &str) -> Result<()> {
        crypto::check_password(password)?;
        let (config, config_epoch) = {
            let state = self.shared.state();
            if state.diverged {
                return Err(section_removed());
            }
            match &state.security {
                Some(config) => (config.clone(), state.config_epoch),
                None => return Err(no_master_password()),
            }
        };
        let key = unlock_key(password, &config)?;
        let mut state = self.shared.state();
        if state.config_epoch != config_epoch {
            return Err(changed_during_unlock());
        }
        state.key = Some(Arc::new(key));
        Ok(())
    }

    /// Checks `password` against the current master password and changes
    /// nothing: not the lock state, not the store. A wrong password fails with
    /// [`ErrorKind::Auth`](crate::ErrorKind::Auth); [`ErrorKind::Usage`](crate::ErrorKind::Usage) when no master password is set. Costs one
    /// key derivation.
    pub fn verify_password(&self, password: &str) -> Result<()> {
        crypto::check_password(password)?;
        let config = {
            let state = self.shared.state();
            match &state.security {
                Some(config) => config.clone(),
                None => return Err(no_master_password()),
            }
        };
        unlock_key(password, &config).map(|_key| ())
    }

    /// Forgets the key. Reads fail with [`ErrorKind::Locked`](crate::ErrorKind::Locked) until the next
    /// [`Vault::unlock`]. Does nothing visible when no master password is set.
    pub fn lock(&self) {
        let mut state = self.shared.state();
        state.key = None;
        state.lock_epoch += 1;
    }

    /// Reads the secret stored under `reference`.
    ///
    /// Fails with [`ErrorKind::Locked`](crate::ErrorKind::Locked) while locked, [`ErrorKind::NotFound`](crate::ErrorKind::NotFound)
    /// when nothing is stored there and [`ErrorKind::Io`](crate::ErrorKind::Io) when the stored value
    /// fails its integrity check.
    pub fn read(&self, reference: &SecretRef) -> Result<SecretString> {
        let key = {
            let state = self.shared.state();
            read_key(&state)?
        };
        let stored = self
            .shared
            .store
            .get(reference.as_str())?
            .ok_or_else(|| missing_secret(reference))?;
        decode(key.as_deref(), reference, stored)
    }

    /// Phase 1 of adding, replacing or removing secrets (account changes, key
    /// rotation, the proxy password). Writes every [`SecretChange::Put`] under
    /// its new reference; replaced and removed values are deleted on
    /// finalize.
    ///
    /// Puts need an unlocked vault when a master password is set; removals do
    /// not. A [`SecretChange::Put`] with `replaces: None` fails with
    /// [`ErrorKind::Conflict`](crate::ErrorKind::Conflict), writing nothing, when its reference already holds a
    /// value.
    pub fn prepare_secret_changes(&self, changes: Vec<SecretChange>) -> Result<PendingChange> {
        check_secret_changes(&changes)?;
        let has_put = changes
            .iter()
            .any(|change| matches!(change, SecretChange::Put { .. }));
        let (mut pending, key) =
            self.begin_change(|state| if has_put { read_key(state) } else { Ok(None) })?;
        // A value for a slot that holds none yet never overwrites one that is
        // already there: it can only belong to another configuration that
        // shares the keychain. Nothing is written before every slot is checked.
        for change in &changes {
            if let SecretChange::Put {
                reference,
                replaces: None,
                ..
            } = change
            {
                match self.shared.store.get(reference.as_str()) {
                    Ok(None) => {}
                    Ok(Some(_)) => return Err(pending.fail(value_exists(reference))),
                    Err(error) => return Err(pending.fail(error)),
                }
            }
        }
        for change in changes {
            match change {
                SecretChange::Put {
                    reference,
                    replaces,
                    value,
                } => {
                    let encoded = match encode(key.as_deref(), &reference, &value) {
                        Ok(encoded) => encoded,
                        Err(error) => return Err(pending.fail(error)),
                    };
                    pending.written.push(reference.clone());
                    if let Err(error) = self.shared.store.set(reference.as_str(), encoded.expose())
                    {
                        return Err(pending.fail(error));
                    }
                    if let Some(old) = replaces {
                        pending.obsolete.push(old.clone());
                        pending.renames.push((old, reference));
                    }
                }
                SecretChange::Remove { reference } => pending.obsolete.push(reference),
            }
        }
        Ok(pending)
    }

    /// Phase 1 of setting the first master password: encrypts every value in
    /// `references` under a new reference. Pass every reference the persisted
    /// configuration holds (account secrets, session tokens, proxy password).
    ///
    /// Fails with [`ErrorKind::Conflict`](crate::ErrorKind::Conflict) when a master password is already
    /// set.
    pub fn prepare_set_master_password(
        &self,
        new_password: &str,
        references: &[SecretRef],
    ) -> Result<PendingChange> {
        self.prepare_rekey(Rekey::Set, None, Some(new_password), references)
    }

    /// Phase 1 of changing the master password: re-encrypts every value under
    /// a new reference with a key from `new_password` and a fresh salt.
    ///
    /// A wrong `current` password fails with [`ErrorKind::Auth`](crate::ErrorKind::Auth) before
    /// anything is written. Changing to the same password re-derives the key
    /// with the current [`KdfCost`].
    pub fn prepare_change_master_password(
        &self,
        current: &str,
        new_password: &str,
        references: &[SecretRef],
    ) -> Result<PendingChange> {
        self.prepare_rekey(Rekey::Change, Some(current), Some(new_password), references)
    }

    /// Phase 1 of removing the master password: writes every value as-is
    /// under a new reference.
    ///
    /// A wrong `current` password fails with [`ErrorKind::Auth`](crate::ErrorKind::Auth) before
    /// anything is written.
    pub fn prepare_remove_master_password(
        &self,
        current: &str,
        references: &[SecretRef],
    ) -> Result<PendingChange> {
        self.prepare_rekey(Rekey::Remove, Some(current), None, references)
    }

    /// Deletes values that no configuration references any more, such as the
    /// leftovers of an interrupted change. Works while locked. Fails with
    /// [`ErrorKind::Conflict`](crate::ErrorKind::Conflict) while a change is pending.
    pub fn purge(&self, references: &[SecretRef]) -> Result<Cleanup> {
        let (pending, ()) = self.begin_change(|_| Ok(()))?;
        let cleanup = self.shared.delete_all(references);
        drop(pending);
        Ok(cleanup)
    }

    fn prepare_rekey(
        &self,
        rekey: Rekey,
        current: Option<&str>,
        new_password: Option<&str>,
        references: &[SecretRef],
    ) -> Result<PendingChange> {
        if let Some(password) = current {
            crypto::check_password(password)?;
        }
        if let Some(password) = new_password {
            crypto::check_password(password)?;
        }
        let renames = plan_renames(references)?;
        let (mut pending, config) = self.begin_change(|state| match (rekey, &state.security) {
            (Rekey::Set, None) => Ok(None),
            (Rekey::Set, Some(_)) => Err(master_password_already_set()),
            (Rekey::Change | Rekey::Remove, Some(config)) => Ok(Some(config.clone())),
            (Rekey::Change | Rekey::Remove, None) => Err(no_master_password()),
        })?;
        let old_key = match (&config, current) {
            (Some(config), Some(password)) => match unlock_key(password, config) {
                Ok(key) => Some(key),
                Err(error) => return Err(pending.fail(error)),
            },
            _ => None,
        };
        let new_master = match new_password {
            Some(password) => match new_master(password, self.shared.kdf_cost) {
                Ok(new_master) => Some(new_master),
                Err(error) => return Err(pending.fail(error)),
            },
            None => None,
        };
        let new_key = new_master.as_ref().map(|(_, key)| key);
        for (old, new) in &renames {
            let encoded = self
                .shared
                .store
                .get(old.as_str())
                .and_then(|stored| stored.ok_or_else(|| missing_secret(old)))
                .and_then(|stored| decode(old_key.as_ref(), old, stored))
                .and_then(|plain| encode(new_key, new, &plain));
            let encoded = match encoded {
                Ok(encoded) => encoded,
                Err(error) => return Err(pending.fail(error)),
            };
            pending.written.push(new.clone());
            if let Err(error) = self.shared.store.set(new.as_str(), encoded.expose()) {
                return Err(pending.fail(error));
            }
        }
        pending.obsolete = renames.iter().map(|(old, _)| old.clone()).collect();
        pending.renames = renames;
        pending.security = match new_master {
            Some((config, key)) => Transition::Set { config, key },
            None => Transition::Removed,
        };
        Ok(pending)
    }

    /// Claims the single pending-change slot after `check` accepted the state.
    fn begin_change<T>(
        &self,
        check: impl FnOnce(&State) -> Result<T>,
    ) -> Result<(PendingChange, T)> {
        let mut state = self.shared.state();
        if state.pending.is_some() {
            return Err(change_in_progress());
        }
        let value = check(&state)?;
        let id = state.next_change_id;
        state.next_change_id += 1;
        state.pending = Some(id);
        let pending = PendingChange {
            shared: Arc::clone(&self.shared),
            id,
            config_epoch: state.config_epoch,
            lock_epoch: state.lock_epoch,
            security: Transition::Unchanged,
            renames: Vec::new(),
            written: Vec::new(),
            obsolete: Vec::new(),
            finished: false,
        };
        Ok((pending, value))
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self.status();
        f.debug_struct("Vault")
            .field("master_password_set", &status.master_password_set)
            .field("unlocked", &status.unlocked)
            .field("kdf_cost", &self.shared.kdf_cost)
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // No critical section can panic halfway through an update, so the
        // state is consistent even after a poisoning panic elsewhere.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn delete_all(&self, references: &[SecretRef]) -> Cleanup {
        let mut cleanup = Cleanup::default();
        for reference in references {
            match self.store.delete(reference.as_str()) {
                Ok(()) => cleanup.deleted.push(reference.clone()),
                Err(error) => {
                    cleanup.leftover.push(reference.clone());
                    cleanup.first_error.get_or_insert(error);
                }
            }
        }
        cleanup
    }
}

enum Transition {
    Unchanged,
    Set {
        config: SecurityConfig,
        key: MasterKey,
    },
    Removed,
}

/// A prepared change whose new values are written but not yet in effect.
///
/// Persist what it reports, then call [`PendingChange::finalize`], or call
/// [`PendingChange::abort`] if persisting failed. See the
/// [module docs](self) for the protocol.
#[must_use = "a pending change must be finalized or aborted"]
pub struct PendingChange {
    shared: Arc<Shared>,
    id: u64,
    config_epoch: u64,
    lock_epoch: u64,
    security: Transition,
    renames: Vec<(SecretRef, SecretRef)>,
    written: Vec<SecretRef>,
    obsolete: Vec<SecretRef>,
    finished: bool,
}

impl PendingChange {
    /// What to persist as the security configuration.
    pub fn security_update(&self) -> SecurityUpdate<'_> {
        match &self.security {
            Transition::Unchanged => SecurityUpdate::Unchanged,
            Transition::Set { config, .. } => SecurityUpdate::Set(config),
            Transition::Removed => SecurityUpdate::Removed,
        }
    }

    /// The reference that replaces `old`, if this change replaces it.
    pub fn new_reference(&self, old: &SecretRef) -> Option<&SecretRef> {
        self.renames
            .iter()
            .find(|(from, _)| from == old)
            .map(|(_, to)| to)
    }

    /// Every `(old, new)` reference pair of this change.
    pub fn renames(&self) -> &[(SecretRef, SecretRef)] {
        &self.renames
    }

    /// New references holding values written by this change. They are
    /// unreferenced leftovers if the change is never persisted.
    pub fn written(&self) -> &[SecretRef] {
        &self.written
    }

    /// References deleted by [`PendingChange::finalize`]. They are
    /// unreferenced leftovers if the process dies after the change was
    /// persisted but before it was finalized.
    pub fn obsolete(&self) -> &[SecretRef] {
        &self.obsolete
    }

    /// Phase 3: the caller persisted this change. Switches the vault to the
    /// new configuration, then deletes the obsolete values.
    ///
    /// Deletion failures do not fail the call: the change is in effect and
    /// the values the configuration references are intact. They are reported
    /// in [`Cleanup::leftover`] for a later [`Vault::purge`]. A vault locked
    /// after `prepare` stays locked.
    pub fn finalize(mut self) -> Result<Cleanup> {
        {
            let mut state = self.shared.state();
            if state.pending != Some(self.id) || state.config_epoch != self.config_epoch {
                return Err(stale_change());
            }
            match std::mem::replace(&mut self.security, Transition::Unchanged) {
                Transition::Unchanged => {}
                Transition::Set { config, key } => {
                    let still_unlocked = state.lock_epoch == self.lock_epoch;
                    state.security = Some(config);
                    state.key = still_unlocked.then(|| Arc::new(key));
                    state.diverged = false;
                    state.config_epoch += 1;
                }
                Transition::Removed => {
                    state.security = None;
                    state.key = None;
                    state.diverged = false;
                    state.config_epoch += 1;
                }
            }
            state.pending = None;
        }
        self.finished = true;
        let obsolete = std::mem::take(&mut self.obsolete);
        Ok(self.shared.delete_all(&obsolete))
    }

    /// Persisting failed: deletes the values this change wrote. The previous
    /// configuration and all its values are untouched.
    pub fn abort(mut self) -> Cleanup {
        self.release();
        self.finished = true;
        let written = std::mem::take(&mut self.written);
        self.shared.delete_all(&written)
    }

    /// Rolls back a failed `prepare`: deletes what was written so far and
    /// returns `error`.
    fn fail(self, error: Error) -> Error {
        // Leftovers that cannot be deleted are unreferenced and harmless.
        let _ = self.abort();
        error
    }

    fn release(&self) {
        let mut state = self.shared.state();
        if state.pending == Some(self.id) {
            state.pending = None;
        }
    }
}

impl Drop for PendingChange {
    fn drop(&mut self) {
        if !self.finished {
            self.release();
        }
    }
}

impl fmt::Debug for PendingChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let security = match self.security_update() {
            SecurityUpdate::Unchanged => "unchanged",
            SecurityUpdate::Set(_) => "set",
            SecurityUpdate::Removed => "removed",
        };
        f.debug_struct("PendingChange")
            .field("security", &security)
            .field("renames", &self.renames)
            .field("written", &self.written)
            .field("obsolete", &self.obsolete)
            .finish_non_exhaustive()
    }
}

/// The key to read with: `None` without a master password.
fn read_key(state: &State) -> Result<Option<Arc<MasterKey>>> {
    match (&state.security, &state.key) {
        (None, _) => Ok(None),
        (Some(_), Some(key)) => Ok(Some(Arc::clone(key))),
        (Some(_), None) => Err(locked()),
    }
}

fn unlock_key(password: &str, config: &SecurityConfig) -> Result<MasterKey> {
    let key = crypto::derive_key(password, &config.kdf)?;
    if crypto::verify(&key, config)? {
        Ok(key)
    } else {
        Err(wrong_password())
    }
}

fn new_master(password: &str, cost: KdfCost) -> Result<(SecurityConfig, MasterKey)> {
    let kdf = crypto::new_kdf_params(cost)?;
    let key = crypto::derive_key(password, &kdf)?;
    let verifier_b64 = crypto::make_verifier(&key, &kdf)?;
    Ok((SecurityConfig { kdf, verifier_b64 }, key))
}

fn encode(
    key: Option<&MasterKey>,
    reference: &SecretRef,
    value: &SecretString,
) -> Result<SecretString> {
    match key {
        None => Ok(value.clone()),
        Some(key) => {
            crypto::seal_secret(key, reference.as_str(), value.expose()).map(SecretString::new)
        }
    }
}

fn decode(
    key: Option<&MasterKey>,
    reference: &SecretRef,
    stored: SecretString,
) -> Result<SecretString> {
    match key {
        None => Ok(stored),
        Some(key) => crypto::open_secret(key, reference.as_str(), stored.expose())
            .map_err(|error| unreadable_secret(reference, error)),
    }
}

fn check_secret_changes(changes: &[SecretChange]) -> Result<()> {
    if changes.is_empty() {
        return Err(no_changes());
    }
    let mut seen = BTreeSet::new();
    for change in changes {
        match change {
            SecretChange::Put {
                reference,
                replaces,
                value,
            } => {
                if value.is_empty() {
                    return Err(empty_value(reference));
                }
                if let Some(old) = replaces {
                    if old.next()? != *reference {
                        return Err(not_next_generation(old, reference));
                    }
                    if !seen.insert(old) {
                        return Err(duplicate_reference(old));
                    }
                }
                if !seen.insert(reference) {
                    return Err(duplicate_reference(reference));
                }
            }
            SecretChange::Remove { reference } => {
                if !seen.insert(reference) {
                    return Err(duplicate_reference(reference));
                }
            }
        }
    }
    Ok(())
}

fn plan_renames(references: &[SecretRef]) -> Result<Vec<(SecretRef, SecretRef)>> {
    let unique: BTreeSet<&SecretRef> = references.iter().collect();
    let mut renames = Vec::with_capacity(unique.len());
    for old in &unique {
        let new = old.next()?;
        if unique.contains(&new) {
            return Err(overlapping_references(old, &new));
        }
        renames.push(((*old).clone(), new));
    }
    Ok(renames)
}

// Error constructors of this module.

fn locked() -> Error {
    Error::Locked
}

fn wrong_password() -> Error {
    Error::Auth("wrong master password".to_owned())
}

fn no_master_password() -> Error {
    Error::Usage("no master password is set".to_owned())
}

fn master_password_already_set() -> Error {
    Error::Conflict("a master password is already set: change it instead".to_owned())
}

fn section_removed() -> Error {
    Error::Conflict(
        "the master password settings were removed from config.toml while BucketBridge was \
         running: restore the file (it holds the key derivation settings the secrets need) or \
         restart BucketBridge to use the file as it is"
            .to_owned(),
    )
}

fn value_exists(reference: &SecretRef) -> Error {
    Error::Conflict(format!(
        "a secret is already stored under {reference}: it was not written for this slot, so it \
         is not overwritten"
    ))
}

fn changed_during_unlock() -> Error {
    Error::Conflict("the master password changed while unlocking: try again".to_owned())
}

fn change_in_progress() -> Error {
    Error::Conflict("another change of stored secrets is in progress".to_owned())
}

fn stale_change() -> Error {
    Error::Conflict("this change is no longer pending in the vault".to_owned())
}

fn missing_secret(reference: &SecretRef) -> Error {
    Error::NotFound(format!(
        "no secret is stored under {reference}: enter it again"
    ))
}

fn unreadable_secret(reference: &SecretRef, error: EnvelopeError) -> Error {
    Error::io(
        format!("cannot read the secret stored under {reference}"),
        std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    )
}

fn no_changes() -> Error {
    Error::Usage("no secret changes were given".to_owned())
}

fn empty_value(reference: &SecretRef) -> Error {
    Error::Usage(format!(
        "the value for {reference} is empty: remove the secret instead"
    ))
}

fn not_next_generation(old: &SecretRef, new: &SecretRef) -> Error {
    Error::Usage(format!(
        "{new} cannot replace {old}: use the next generation of the same slot"
    ))
}

fn duplicate_reference(reference: &SecretRef) -> Error {
    Error::Usage(format!("{reference} appears more than once in one change"))
}

fn overlapping_references(old: &SecretRef, new: &SecretRef) -> Error {
    Error::Usage(format!(
        "{old} and {new} are both referenced: they cannot be re-encrypted together"
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::error::ErrorKind;
    use crate::secrets::crypto::tests::TEST_COST;
    use crate::secrets::store::testing::FlakyStore;
    use crate::secrets::{PROXY_OWNER, SLOT_PROXY_PASSWORD, SLOT_SECRET, SLOT_SESSION_TOKEN};

    const ACCOUNT_A: &str = "0b5e8d0c-5b8e-4d0e-9a51-6d2f0f6f1c01";
    const ACCOUNT_B: &str = "6c1d1f4a-2e7b-4f3c-8b9a-1e2d3c4b5a02";
    const SECRET_A: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const TOKEN_A: &str = "FwoGZXIvYXdzEXAMPLESESSIONTOKENzZXNzaW9uLXRva2Vu";
    const SECRET_B: &str = "je7MtGbClwBF/2Zp9Utk/h3yCo8nvbEXAMPLEKEY";
    const PROXY_PASSWORD: &str = "pr0xy p@ss:w/rd \u{e4}";
    const PASSWORD_1: &str = "first master password";
    const PASSWORD_2: &str = "second master password \u{1f511}";

    /// What the account manager would persist: the security configuration
    /// and, per secret, its current reference and expected plaintext.
    #[derive(Clone, Debug, PartialEq)]
    struct Persisted {
        security: Option<SecurityConfig>,
        secrets: Vec<(SecretRef, &'static str)>,
    }

    impl Persisted {
        fn references(&self) -> Vec<SecretRef> {
            self.secrets.iter().map(|(r, _)| r.clone()).collect()
        }

        /// The caller's half of phase 2.
        fn apply(&self, change: &PendingChange) -> Persisted {
            let security = match change.security_update() {
                SecurityUpdate::Unchanged => self.security.clone(),
                SecurityUpdate::Set(config) => Some(config.clone()),
                SecurityUpdate::Removed => None,
            };
            let secrets = self
                .secrets
                .iter()
                .map(|(reference, value)| {
                    let reference = change.new_reference(reference).unwrap_or(reference);
                    (reference.clone(), *value)
                })
                .collect();
            Persisted { security, secrets }
        }
    }

    fn reference(owner: &str, slot: &str, generation: u32) -> SecretRef {
        SecretRef::new(owner, slot, generation).unwrap()
    }

    fn vault(store: &Arc<FlakyStore>, security: Option<SecurityConfig>) -> Vault {
        Vault::with_kdf_cost(store.clone(), security, TEST_COST).unwrap()
    }

    /// Three accounts' worth of secrets stored without a master password.
    fn plain_fixture() -> (Arc<FlakyStore>, Vault, Persisted) {
        let store = Arc::new(FlakyStore::default());
        let vault = vault(&store, None);
        let secrets = vec![
            (reference(ACCOUNT_A, SLOT_SECRET, 1), SECRET_A),
            (reference(ACCOUNT_A, SLOT_SESSION_TOKEN, 1), TOKEN_A),
            (reference(ACCOUNT_B, SLOT_SECRET, 1), SECRET_B),
            (
                reference(PROXY_OWNER, SLOT_PROXY_PASSWORD, 1),
                PROXY_PASSWORD,
            ),
        ];
        let changes = secrets
            .iter()
            .map(|(reference, value)| SecretChange::Put {
                reference: reference.clone(),
                replaces: None,
                value: SecretString::from(*value),
            })
            .collect();
        let change = vault.prepare_secret_changes(changes).unwrap();
        assert!(change.finalize().unwrap().is_complete());
        let persisted = Persisted {
            security: None,
            secrets,
        };
        (store, vault, persisted)
    }

    /// The plain fixture after setting `PASSWORD_1`.
    fn protected_fixture() -> (Arc<FlakyStore>, Vault, Persisted) {
        let (store, vault, persisted) = plain_fixture();
        let change = vault
            .prepare_set_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        let persisted = persisted.apply(&change);
        assert!(change.finalize().unwrap().is_complete());
        (store, vault, persisted)
    }

    fn assert_readable(vault: &Vault, persisted: &Persisted) {
        for (reference, value) in &persisted.secrets {
            assert_eq!(
                vault.read(reference).unwrap().expose(),
                *value,
                "{reference}"
            );
        }
    }

    /// The store holds exactly the persisted references.
    fn assert_store_matches(store: &FlakyStore, persisted: &Persisted) {
        let mut expected: Vec<String> = persisted
            .secrets
            .iter()
            .map(|(r, _)| r.as_str().to_owned())
            .collect();
        expected.sort();
        assert_eq!(store.inner.references(), expected);
    }

    /// A process restart: a new vault from what was persisted.
    fn restart(store: &Arc<FlakyStore>, persisted: &Persisted) -> Vault {
        vault(store, persisted.security.clone())
    }

    fn kind<T: fmt::Debug>(result: Result<T>) -> ErrorKind {
        result.expect_err("expected an error").kind()
    }

    #[test]
    fn plain_mode_stores_values_as_is() {
        let (store, vault, persisted) = plain_fixture();
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: false,
                unlocked: true
            }
        );
        assert_readable(&vault, &persisted);
        let raw = store.raw();
        assert!(raw.iter().any(|(_, value)| value == SECRET_A));
        assert_eq!(kind(vault.unlock(PASSWORD_1)), ErrorKind::Usage);
        vault.lock();
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn lifecycle_set_change_remove() {
        let (store, vault, persisted) = plain_fixture();

        // Set.
        let change = vault
            .prepare_set_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        assert!(matches!(change.security_update(), SecurityUpdate::Set(_)));
        assert_eq!(change.written().len(), 4);
        // Still the old state until finalize.
        assert!(!vault.status().master_password_set);
        assert_readable(&vault, &persisted);
        let persisted = persisted.apply(&change);
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: true
            }
        );
        assert_eq!(vault.security_config(), persisted.security);
        assert_readable(&vault, &persisted);
        assert_store_matches(&store, &persisted);
        assert!(persisted.secrets.iter().all(|(r, _)| r.generation() == 2));

        // Restart: locked until unlocked with the right password.
        let vault = restart(&store, &persisted);
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
        assert_eq!(kind(vault.read(&persisted.secrets[0].0)), ErrorKind::Locked);
        assert_eq!(kind(vault.unlock(PASSWORD_2)), ErrorKind::Auth);
        vault.unlock(PASSWORD_1).unwrap();
        assert_readable(&vault, &persisted);

        // Change.
        let change = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let persisted_2 = persisted.apply(&change);
        assert_ne!(persisted_2.security, persisted.security);
        assert!(change.finalize().unwrap().is_complete());
        assert_readable(&vault, &persisted_2);
        assert_store_matches(&store, &persisted_2);

        let vault = restart(&store, &persisted_2);
        assert_eq!(kind(vault.unlock(PASSWORD_1)), ErrorKind::Auth);
        vault.unlock(PASSWORD_2).unwrap();
        assert_readable(&vault, &persisted_2);

        // Remove.
        let change = vault
            .prepare_remove_master_password(PASSWORD_2, &persisted_2.references())
            .unwrap();
        assert_eq!(change.security_update(), SecurityUpdate::Removed);
        let persisted_3 = persisted_2.apply(&change);
        assert!(change.finalize().unwrap().is_complete());
        assert!(!vault.status().master_password_set);
        assert_readable(&vault, &persisted_3);
        assert_store_matches(&store, &persisted_3);
        let raw = store.raw();
        assert!(raw.iter().any(|(_, value)| value == SECRET_B));

        let vault = restart(&store, &persisted_3);
        assert!(vault.status().unlocked);
        assert_readable(&vault, &persisted_3);
    }

    #[test]
    fn wrong_password_modifies_nothing() {
        let (store, vault, persisted) = protected_fixture();
        let vault_locked = restart(&store, &persisted);
        let before = store.raw();
        let operations = store.operations();

        assert_eq!(
            kind(vault_locked.unlock("not the password")),
            ErrorKind::Auth
        );
        assert!(!vault_locked.status().unlocked);
        assert_eq!(
            kind(vault_locked.prepare_change_master_password(
                "not the password",
                PASSWORD_2,
                &persisted.references()
            )),
            ErrorKind::Auth
        );
        assert_eq!(
            kind(
                vault_locked
                    .prepare_remove_master_password("not the password", &persisted.references())
            ),
            ErrorKind::Auth
        );
        // Same on the unlocked vault: it stays unlocked.
        assert_eq!(kind(vault.unlock("not the password")), ErrorKind::Auth);
        assert!(vault.status().unlocked);
        assert_readable(&vault, &persisted);

        assert_eq!(store.raw(), before);
        assert_eq!(store.operations(), operations);
        assert_eq!(vault_locked.security_config(), persisted.security);
        assert_eq!(vault.security_config(), persisted.security);

        // The failed attempts released the change slot.
        vault_locked.unlock(PASSWORD_1).unwrap();
        let change = vault_locked
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let _ = change.abort();
        assert_eq!(store.raw(), before);
    }

    #[test]
    fn lock_and_unlock() {
        let (_store, vault, persisted) = protected_fixture();
        assert_readable(&vault, &persisted);
        vault.lock();
        assert!(!vault.status().unlocked);
        for (reference, _) in &persisted.secrets {
            assert_eq!(kind(vault.read(reference)), ErrorKind::Locked);
        }
        assert_eq!(kind(vault.unlock("")), ErrorKind::Usage);
        vault.unlock(PASSWORD_1).unwrap();
        assert_readable(&vault, &persisted);
        // Clones share the state.
        let clone = vault.clone();
        clone.lock();
        assert!(!vault.status().unlocked);
    }

    #[test]
    fn no_plaintext_in_persisted_values() {
        let (store, vault, persisted) = protected_fixture();
        let config = vault.security_config().unwrap();
        let mut persisted_values: Vec<String> =
            store.raw().into_iter().map(|(_, value)| value).collect();
        persisted_values.extend([
            config.verifier_b64.clone(),
            config.kdf.salt_b64.clone(),
            config.kdf.algorithm.clone(),
            format!("{config:?}"),
        ]);
        for plaintext in [SECRET_A, TOKEN_A, SECRET_B, PROXY_PASSWORD, PASSWORD_1] {
            let forms = [
                plaintext.to_owned(),
                plaintext[..12].to_owned(),
                STANDARD.encode(plaintext),
                plaintext.bytes().map(|b| format!("{b:02x}")).collect(),
            ];
            for value in &persisted_values {
                for form in &forms {
                    assert!(!value.contains(form.as_str()), "{form} leaked");
                }
            }
        }
        // Every stored value is an envelope.
        for (_, value) in store.raw() {
            let blob = STANDARD.decode(value).unwrap();
            assert_eq!(&blob[..2], &[0xBB, 0x01]);
        }
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn envelopes_are_bound_to_their_reference() {
        let (store, vault, persisted) = protected_fixture();
        let (secret_ref, _) = &persisted.secrets[0];
        let (token_ref, _) = &persisted.secrets[1];
        let envelope = store.inner.get(secret_ref.as_str()).unwrap().unwrap();
        store
            .inner
            .set(token_ref.as_str(), envelope.expose())
            .unwrap();
        let error = vault.read(token_ref).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert!(
            error.to_string().contains("integrity check failed"),
            "{error}"
        );
        assert!(!error.to_string().contains(SECRET_A));
    }

    #[test]
    fn tampered_values_fail_clearly() {
        let (store, vault, persisted) = protected_fixture();
        let (target, _) = &persisted.secrets[2];
        let original = STANDARD
            .decode(store.inner.get(target.as_str()).unwrap().unwrap().expose())
            .unwrap();
        let mut flipped = original.clone();
        flipped[20] ^= 0x01;
        let mut version = original.clone();
        version[1] = 9;
        let cases = [
            (STANDARD.encode(&flipped), "integrity check failed"),
            (STANDARD.encode(&original[..20]), "truncated"),
            (STANDARD.encode(&version), "unsupported envelope version 9"),
            (SECRET_B.to_owned(), "not an encrypted envelope"),
            ("#".to_owned(), "not base64"),
        ];
        for (stored, expected) in cases {
            store.inner.set(target.as_str(), &stored).unwrap();
            let error = vault.read(target).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Io, "{expected}");
            assert!(error.to_string().contains(expected), "{error}");
        }
        store.inner.delete(target.as_str()).unwrap();
        assert_eq!(kind(vault.read(target)), ErrorKind::NotFound);
    }

    #[test]
    fn failed_write_during_set_keeps_the_old_state() {
        for n in 1..=4 {
            let (store, vault, persisted) = plain_fixture();
            let before = store.raw();
            store.fail_nth_write(n);
            let error = vault
                .prepare_set_master_password(PASSWORD_1, &persisted.references())
                .unwrap_err();
            assert!(error.to_string().starts_with("injected write failure"));
            assert_eq!(store.raw(), before, "write {n}");
            assert!(!vault.status().master_password_set);
            assert_readable(&vault, &persisted);
            assert_readable(&restart(&store, &persisted), &persisted);
            // The slot was released.
            let change = vault
                .prepare_set_master_password(PASSWORD_1, &persisted.references())
                .unwrap();
            let _ = change.abort();
        }
    }

    #[test]
    fn failed_write_during_change_and_remove_keeps_the_old_state() {
        for n in 1..=4 {
            for remove in [false, true] {
                let (store, vault, persisted) = protected_fixture();
                let before = store.raw();
                store.fail_nth_write(n);
                let result = if remove {
                    vault.prepare_remove_master_password(PASSWORD_1, &persisted.references())
                } else {
                    vault.prepare_change_master_password(
                        PASSWORD_1,
                        PASSWORD_2,
                        &persisted.references(),
                    )
                };
                assert_eq!(kind(result), ErrorKind::Io);
                assert_eq!(store.raw(), before, "write {n}, remove {remove}");
                assert_eq!(vault.security_config(), persisted.security);
                assert_readable(&vault, &persisted);
                let restarted = restart(&store, &persisted);
                restarted.unlock(PASSWORD_1).unwrap();
                assert_readable(&restarted, &persisted);
            }
        }
    }

    #[test]
    fn failed_rollback_leaves_only_unreferenced_values() {
        let (store, vault, persisted) = plain_fixture();
        store.fail_nth_write(3);
        store.fail_nth_delete(1);
        assert_eq!(
            kind(vault.prepare_set_master_password(PASSWORD_1, &persisted.references())),
            ErrorKind::Io
        );
        // The old configuration is complete; one new value could not be
        // removed and is unreferenced.
        assert_readable(&vault, &persisted);
        let leftovers: Vec<SecretRef> = store
            .inner
            .references()
            .iter()
            .map(|r| SecretRef::parse(r).unwrap())
            .filter(|r| !persisted.references().contains(r))
            .collect();
        assert_eq!(leftovers.len(), 1);
        assert!(vault.purge(&leftovers).unwrap().is_complete());
        assert_store_matches(&store, &persisted);
    }

    #[test]
    fn crash_before_persisting_keeps_the_old_state() {
        let (store, vault, persisted) = protected_fixture();
        let change = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let leftovers = change.written().to_vec();
        // The process dies: the change is neither persisted nor finalized.
        drop(change);
        drop(vault);
        let vault = restart(&store, &persisted);
        assert_eq!(kind(vault.unlock(PASSWORD_2)), ErrorKind::Auth);
        vault.unlock(PASSWORD_1).unwrap();
        assert_readable(&vault, &persisted);
        assert!(vault.purge(&leftovers).unwrap().is_complete());
        assert_store_matches(&store, &persisted);
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn crash_after_persisting_keeps_the_new_state() {
        let (store, vault, persisted) = protected_fixture();
        let change = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let persisted_new = persisted.apply(&change);
        let leftovers = change.obsolete().to_vec();
        // Persisted, then the process dies before finalize.
        drop(change);
        drop(vault);
        let vault = restart(&store, &persisted_new);
        assert_eq!(kind(vault.unlock(PASSWORD_1)), ErrorKind::Auth);
        vault.unlock(PASSWORD_2).unwrap();
        assert_readable(&vault, &persisted_new);
        assert!(vault.purge(&leftovers).unwrap().is_complete());
        assert_store_matches(&store, &persisted_new);
    }

    #[test]
    fn crash_after_persisting_a_removal_keeps_the_new_state() {
        let (store, vault, persisted) = protected_fixture();
        let change = vault
            .prepare_remove_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        let persisted_new = persisted.apply(&change);
        drop(change);
        let vault = restart(&store, &persisted_new);
        assert!(vault.status().unlocked);
        assert_readable(&vault, &persisted_new);
    }

    #[test]
    fn failed_delete_during_finalize_is_reported_not_fatal() {
        let (store, vault, persisted) = protected_fixture();
        let change = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let persisted_new = persisted.apply(&change);
        store.fail_nth_delete(2);
        let cleanup = change.finalize().unwrap();
        assert_eq!(cleanup.leftover.len(), 1);
        assert_eq!(cleanup.deleted.len(), 3);
        assert!(
            cleanup
                .first_error
                .as_ref()
                .is_some_and(|e| e.to_string().starts_with("injected delete failure"))
        );
        assert_readable(&vault, &persisted_new);
        let restarted = restart(&store, &persisted_new);
        restarted.unlock(PASSWORD_2).unwrap();
        assert_readable(&restarted, &persisted_new);
        assert!(vault.purge(&cleanup.leftover).unwrap().is_complete());
        assert_store_matches(&store, &persisted_new);
    }

    #[test]
    fn abort_deletes_only_the_new_values() {
        let (store, vault, persisted) = protected_fixture();
        let before = store.raw();
        let change = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        assert_eq!(store.inner.len(), 8);
        assert!(change.abort().is_complete());
        assert_eq!(store.raw(), before);
        assert_eq!(vault.security_config(), persisted.security);
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn rotating_one_account_key() {
        let (store, vault, persisted) = protected_fixture();
        let (old, _) = persisted.secrets[0].clone();
        let new = old.next().unwrap();
        let rotate = |value: &str| {
            vault.prepare_secret_changes(vec![SecretChange::Put {
                reference: new.clone(),
                replaces: Some(old.clone()),
                value: SecretString::from(value),
            }])
        };

        // Aborted: the old key stays.
        let change = rotate("rotated-secret-1").unwrap();
        assert_eq!(change.new_reference(&old), Some(&new));
        assert_eq!(vault.read(&new).unwrap().expose(), "rotated-secret-1");
        assert!(change.abort().is_complete());
        assert_eq!(kind(vault.read(&new)), ErrorKind::NotFound);
        assert_readable(&vault, &persisted);

        // Failed write: nothing changes.
        let before = store.raw();
        store.fail_nth_write(1);
        assert_eq!(kind(rotate("rotated-secret-2")), ErrorKind::Io);
        assert_eq!(store.raw(), before);

        // Finalized: the old key is gone, everything else intact.
        let change = rotate("rotated-secret-3").unwrap();
        let mut persisted_new = persisted.apply(&change);
        persisted_new.secrets[0].1 = "rotated-secret-3";
        assert_eq!(change.obsolete(), std::slice::from_ref(&old));
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(kind(vault.read(&old)), ErrorKind::NotFound);
        assert_readable(&vault, &persisted_new);
        assert_store_matches(&store, &persisted_new);

        // Envelopes are bound to the new reference.
        let restarted = restart(&store, &persisted_new);
        restarted.unlock(PASSWORD_1).unwrap();
        assert_readable(&restarted, &persisted_new);
    }

    #[test]
    fn adding_and_removing_secrets() {
        let (store, vault, mut persisted) = protected_fixture();
        // Removals work while locked; puts do not.
        vault.lock();
        let token = SecretRef::first(ACCOUNT_B, SLOT_SESSION_TOKEN).unwrap();
        let put = || {
            vault.prepare_secret_changes(vec![SecretChange::Put {
                reference: token.clone(),
                replaces: None,
                value: SecretString::from("new-session-token"),
            }])
        };
        assert_eq!(kind(put()), ErrorKind::Locked);
        let (removed, _) = persisted.secrets.remove(1);
        let change = vault
            .prepare_secret_changes(vec![SecretChange::Remove {
                reference: removed.clone(),
            }])
            .unwrap();
        assert!(change.finalize().unwrap().is_complete());
        assert_store_matches(&store, &persisted);

        vault.unlock(PASSWORD_1).unwrap();
        let change = put().unwrap();
        assert_eq!(change.written(), std::slice::from_ref(&token));
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(vault.read(&token).unwrap().expose(), "new-session-token");
        persisted.secrets.push((token, "new-session-token"));
        assert_store_matches(&store, &persisted);
    }

    #[test]
    fn invalid_changes_are_rejected_before_writing() {
        let (store, vault, persisted) = protected_fixture();
        let operations = store.operations();
        let (current, _) = persisted.secrets[0].clone();
        let put =
            |reference: SecretRef, replaces: Option<SecretRef>, value: &str| SecretChange::Put {
                reference,
                replaces,
                value: SecretString::from(value),
            };
        let skip = SecretRef::new(current.owner(), current.slot(), 4).unwrap();
        let other_slot = SecretRef::first(ACCOUNT_A, SLOT_SESSION_TOKEN).unwrap();
        let cases = vec![
            vec![],
            vec![put(skip, Some(current.clone()), "v")],
            vec![put(other_slot, Some(current.clone()), "v")],
            vec![put(current.next().unwrap(), Some(current.clone()), "")],
            vec![
                put(current.next().unwrap(), Some(current.clone()), "v"),
                SecretChange::Remove {
                    reference: current.clone(),
                },
            ],
            vec![
                SecretChange::Remove {
                    reference: current.clone(),
                },
                SecretChange::Remove {
                    reference: current.clone(),
                },
            ],
        ];
        for changes in cases {
            assert_eq!(
                kind(vault.prepare_secret_changes(changes)),
                ErrorKind::Usage
            );
        }
        let overlapping = [current.clone(), current.next().unwrap()];
        assert_eq!(
            kind(vault.prepare_change_master_password(PASSWORD_1, PASSWORD_2, &overlapping)),
            ErrorKind::Usage
        );
        assert_eq!(
            kind(vault.prepare_set_master_password(PASSWORD_2, &persisted.references())),
            ErrorKind::Conflict
        );
        assert_eq!(
            kind(vault.prepare_change_master_password(PASSWORD_1, "", &persisted.references())),
            ErrorKind::Usage
        );
        assert_eq!(store.operations(), operations);

        let (_store, plain, persisted) = plain_fixture();
        assert_eq!(
            kind(plain.prepare_change_master_password(
                PASSWORD_1,
                PASSWORD_2,
                &persisted.references()
            )),
            ErrorKind::Usage
        );
        assert_eq!(
            kind(plain.prepare_remove_master_password(PASSWORD_1, &persisted.references())),
            ErrorKind::Usage
        );
    }

    #[test]
    fn missing_secret_blocks_a_rekey() {
        let (store, vault, persisted) = protected_fixture();
        let (gone, _) = &persisted.secrets[3];
        store.inner.delete(gone.as_str()).unwrap();
        let before = store.raw();
        let error = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.to_string().contains(gone.as_str()));
        assert_eq!(store.raw(), before);
    }

    #[test]
    fn one_change_at_a_time() {
        let (_store, vault, persisted) = protected_fixture();
        let first = vault
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        assert_eq!(
            kind(vault.prepare_remove_master_password(PASSWORD_1, &persisted.references())),
            ErrorKind::Conflict
        );
        assert_eq!(kind(vault.purge(&[])), ErrorKind::Conflict);
        // Dropping releases the slot without deleting anything.
        drop(first);
        let second = vault
            .prepare_remove_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        let _ = second.abort();
    }

    #[test]
    fn locking_during_a_pending_set_keeps_the_vault_locked() {
        let (_store, vault, persisted) = plain_fixture();
        let change = vault
            .prepare_set_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        let persisted = persisted.apply(&change);
        vault.lock();
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
        vault.unlock(PASSWORD_1).unwrap();
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        let (store, vault, _) = protected_fixture();
        let mut config = vault.security_config().unwrap();
        config.verifier_b64 = "garbage".to_owned();
        assert_eq!(
            kind(Vault::with_kdf_cost(store.clone(), Some(config), TEST_COST)),
            ErrorKind::Usage
        );
        let mut config = vault.security_config().unwrap();
        config.kdf.m_cost_kib = u32::MAX;
        assert_eq!(
            kind(Vault::new(store.clone(), Some(config))),
            ErrorKind::Usage
        );
        let bad_cost = KdfCost {
            m_cost_kib: 1,
            t_cost: 1,
            p_cost: 1,
        };
        assert_eq!(
            kind(Vault::with_kdf_cost(store, None, bad_cost)),
            ErrorKind::Usage
        );
    }

    #[test]
    fn two_accounts_are_readable_concurrently() {
        let (_store, vault, persisted) = protected_fixture();
        let handles: Vec<_> = persisted
            .secrets
            .iter()
            .cloned()
            .map(|(reference, value)| {
                let vault = vault.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        assert_eq!(vault.read(&reference).unwrap().expose(), value);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn recommended_cost_end_to_end() {
        let store = Arc::new(FlakyStore::default());
        let vault = Vault::new(store.clone(), None).unwrap();
        let reference = SecretRef::first(ACCOUNT_A, SLOT_SECRET).unwrap();
        let change = vault
            .prepare_secret_changes(vec![SecretChange::Put {
                reference: reference.clone(),
                replaces: None,
                value: SecretString::from(SECRET_A),
            }])
            .unwrap();
        assert!(change.finalize().unwrap().is_complete());
        let change = vault
            .prepare_set_master_password(PASSWORD_1, std::slice::from_ref(&reference))
            .unwrap();
        let SecurityUpdate::Set(config) = change.security_update() else {
            panic!("expected a new configuration");
        };
        assert_eq!(
            (config.kdf.m_cost_kib, config.kdf.t_cost, config.kdf.p_cost),
            (262_144, 3, 4)
        );
        let config = config.clone();
        let new_reference = change.new_reference(&reference).unwrap().clone();
        assert!(change.finalize().unwrap().is_complete());
        // One derivation at full cost is enough (slow in debug builds);
        // restart and unlock are covered at test cost.
        assert_eq!(vault.security_config(), Some(config.clone()));
        assert_eq!(vault.read(&new_reference).unwrap().expose(), SECRET_A);
        assert!(Vault::new(store, Some(config)).is_ok());
    }

    #[test]
    fn debug_output_has_no_secrets() {
        let (_store, vault, persisted) = protected_fixture();
        let change = vault
            .prepare_secret_changes(vec![SecretChange::Put {
                reference: persisted.secrets[0].0.next().unwrap(),
                replaces: Some(persisted.secrets[0].0.clone()),
                value: SecretString::from("rotated-debug-secret"),
            }])
            .unwrap();
        let rendered = format!("{vault:?} {change:?}");
        assert!(!rendered.contains("rotated-debug-secret"));
        assert!(!rendered.contains(SECRET_A));
        let _ = change.abort();
    }

    #[test]
    fn sync_config_adopts_changes_from_another_process() {
        let (store, vault, persisted) = protected_fixture();
        // Another process (sharing the store) changes the master password.
        let other = restart(&store, &persisted);
        other.unlock(PASSWORD_1).unwrap();
        let change = other
            .prepare_change_master_password(PASSWORD_1, PASSWORD_2, &persisted.references())
            .unwrap();
        let persisted_2 = persisted.apply(&change);
        assert!(change.finalize().unwrap().is_complete());

        // Unchanged configuration: nothing happens, the vault stays unlocked.
        assert!(!vault.sync_config(persisted.security.clone()).unwrap());
        assert!(vault.status().unlocked);
        // The new configuration locks the vault until the new password is given.
        assert!(vault.sync_config(persisted_2.security.clone()).unwrap());
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
        assert_eq!(kind(vault.unlock(PASSWORD_1)), ErrorKind::Auth);
        vault.unlock(PASSWORD_2).unwrap();
        assert_readable(&vault, &persisted_2);

        // An invalid configuration is rejected and changes nothing.
        let mut broken = persisted_2.security.clone().unwrap();
        broken.verifier_b64 = "broken".to_owned();
        assert_eq!(kind(vault.sync_config(Some(broken))), ErrorKind::Usage);
        assert!(vault.status().master_password_set);
        assert!(!vault.diverged());

        // Another process sets a master password on a plain vault.
        let (_store, plain, _) = plain_fixture();
        assert!(plain.sync_config(persisted_2.security.clone()).unwrap());
        assert_eq!(
            plain.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
    }

    #[test]
    fn a_protected_vault_never_becomes_plain_when_the_section_disappears() {
        let (store, vault, persisted) = protected_fixture();
        assert!(vault.status().unlocked);
        let writes = store.operations();

        // The section is removed from config.toml while the vault is unlocked:
        // the vault locks (and says so) instead of switching to plain mode.
        assert!(vault.sync_config(None).unwrap());
        assert!(vault.diverged());
        assert_eq!(
            vault.status(),
            SecurityStatus {
                master_password_set: true,
                unlocked: false
            }
        );
        assert_eq!(vault.security_config(), persisted.security);
        for (reference, _) in &persisted.secrets {
            assert_eq!(kind(vault.read(reference)), ErrorKind::Locked);
        }
        // Seeing the same file again changes nothing more.
        assert!(!vault.sync_config(None).unwrap());
        // Nothing can be stored: puts need the key, and the key cannot come back.
        let put = |reference| {
            vault.prepare_secret_changes(vec![SecretChange::Put {
                reference,
                replaces: None,
                value: SecretString::from("stored-in-plaintext?"),
            }])
        };
        let fresh = reference(ACCOUNT_B, SLOT_SESSION_TOKEN, 1);
        assert_eq!(kind(put(fresh.clone())), ErrorKind::Locked);
        let error = vault.unlock(PASSWORD_1).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
        assert!(error.to_string().contains("config.toml"), "{error}");
        assert!(!vault.status().unlocked);
        assert_eq!(store.operations(), writes);
        assert!(
            store
                .raw()
                .iter()
                .all(|(_, value)| value != "stored-in-plaintext?")
        );

        // The section comes back: the vault unlocks again.
        assert!(!vault.sync_config(persisted.security.clone()).unwrap());
        assert!(!vault.diverged());
        vault.unlock(PASSWORD_1).unwrap();
        assert_readable(&vault, &persisted);
    }

    #[test]
    fn a_vault_without_its_section_can_still_be_repaired_with_its_password() {
        for remove in [false, true] {
            let (store, vault, persisted) = protected_fixture();
            vault.sync_config(None).unwrap();
            assert!(vault.diverged());
            // The current password is still verified against the vault's own
            // copy of the settings.
            assert_eq!(
                kind(vault.verify_password("not the password")),
                ErrorKind::Auth
            );
            vault.verify_password(PASSWORD_1).unwrap();
            assert!(vault.diverged(), "verifying changes nothing");
            let change = if remove {
                vault.prepare_remove_master_password(PASSWORD_1, &persisted.references())
            } else {
                vault.prepare_change_master_password(
                    PASSWORD_1,
                    PASSWORD_2,
                    &persisted.references(),
                )
            }
            .unwrap();
            let persisted_new = persisted.apply(&change);
            assert!(change.finalize().unwrap().is_complete());
            assert!(!vault.diverged());
            assert_eq!(vault.security_config(), persisted_new.security);
            assert_eq!(vault.status().master_password_set, !remove);
            assert!(vault.status().unlocked);
            assert_readable(&vault, &persisted_new);
            assert_store_matches(&store, &persisted_new);
        }
    }

    #[test]
    fn verifying_a_password_changes_nothing() {
        let (store, _vault, persisted) = protected_fixture();
        let vault = restart(&store, &persisted);
        let before = (store.raw(), store.operations());
        assert_eq!(
            kind(vault.verify_password("not the password")),
            ErrorKind::Auth
        );
        assert_eq!(kind(vault.verify_password("")), ErrorKind::Usage);
        vault.verify_password(PASSWORD_1).unwrap();
        // Still locked: verifying is not unlocking.
        assert!(!vault.status().unlocked);
        assert_eq!((store.raw(), store.operations()), before);
        let (_store, plain, _) = plain_fixture();
        assert_eq!(kind(plain.verify_password(PASSWORD_1)), ErrorKind::Usage);
    }

    #[test]
    fn a_new_slot_never_overwrites_an_existing_value() {
        let (store, vault, persisted) = plain_fixture();
        let (taken, value) = persisted.secrets[0].clone();
        let before = store.raw();
        let put = |reference: SecretRef, replaces: Option<SecretRef>| {
            vault.prepare_secret_changes(vec![SecretChange::Put {
                reference,
                replaces,
                value: SecretString::from("an intruder"),
            }])
        };
        let error = put(taken.clone(), None).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
        assert!(error.to_string().contains(taken.as_str()), "{error}");
        assert_eq!(store.raw(), before);
        assert_eq!(vault.read(&taken).unwrap().expose(), value);
        // Nothing is written before every slot of the change is checked.
        let free = reference(ACCOUNT_B, SLOT_SESSION_TOKEN, 1);
        let error = vault
            .prepare_secret_changes(vec![
                SecretChange::Put {
                    reference: free.clone(),
                    replaces: None,
                    value: SecretString::from("first"),
                },
                SecretChange::Put {
                    reference: taken.clone(),
                    replaces: None,
                    value: SecretString::from("second"),
                },
            ])
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(store.raw(), before);
        assert_eq!(store.operations().0, 4, "only the fixture wrote");
        // The slot is released: a free slot works, and so does replacing.
        let change = put(free, None).unwrap();
        assert!(change.finalize().unwrap().is_complete());
        let change = put(taken.next().unwrap(), Some(taken.clone())).unwrap();
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(kind(vault.read(&taken)), ErrorKind::NotFound);
    }

    #[test]
    fn sync_config_waits_for_a_pending_change() {
        let (_store, vault, persisted) = plain_fixture();
        let change = vault
            .prepare_set_master_password(PASSWORD_1, &persisted.references())
            .unwrap();
        let SecurityUpdate::Set(config) = change.security_update() else {
            panic!("expected a new configuration");
        };
        let config = config.clone();
        // A reader that already sees the persisted configuration must not
        // disturb the change in flight.
        assert!(!vault.sync_config(Some(config.clone())).unwrap());
        assert!(change.finalize().unwrap().is_complete());
        assert_eq!(vault.security_config(), Some(config));
        assert!(vault.status().unlocked);
    }
}
