//! Secret storage: the OS keychain and the master-password vault.
//!
//! Two layers keep secrets (secret access keys, static session tokens and the
//! proxy password) off the disk:
//!
//! - [`SecretStore`] is a key/value store of secret strings addressed by a
//!   reference string. [`KeyringStore`] keeps them in the OS keychain (macOS
//!   Keychain, Windows Credential Manager, Linux Secret Service) under the
//!   service name [`SERVICE_NAME`]; [`MemoryStore`] keeps them in memory for
//!   tests.
//! - [`Vault`] is the policy on top of a store. Without a master password a
//!   secret is stored as-is and the OS keychain encrypts it at rest. With a
//!   master password every stored value is an AES-256-GCM envelope under a key
//!   derived from the password with Argon2id, and nothing can be read until
//!   [`Vault::unlock`] succeeds.
//!
//! `config.toml` (owned by the account manager) only ever holds references and
//! the [`SecurityConfig`](crate::config::SecurityConfig) (KDF parameters and a
//! verifier), never a secret or a key.
//!
//! # Reference naming
//!
//! Every stored secret is addressed by a [`SecretRef`] of the form
//! `<owner>/<slot>/<generation>`:
//!
//! - `owner` is the account id (a random UUID) or, for the proxy password,
//!   [`proxy_owner`] of the installation (`proxy-<installation id>`), so
//!   that no two configurations on one machine share a name;
//! - `slot` says which secret of the owner it is: [`SLOT_SECRET`] (secret
//!   access key), [`SLOT_SESSION_TOKEN`] or [`SLOT_PROXY_PASSWORD`];
//! - `generation` starts at 1 and grows by one each time the value is
//!   replaced or re-encrypted, so a new value is always written next to the
//!   old one before the old one is deleted (see [`Vault`] for the protocol).
//!
//! Example: `5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11/secret/3`.
//!
//! A reference in `config.toml` is only ever followed for the purpose it is
//! stored for: an account's references must be owned by that account and
//! sit in the right slot, the proxy password's must be owned by the proxy
//! of this installation (the account manager enforces this before it reads,
//! re-encrypts or deletes anything).
//!
//! # Blocking
//!
//! Every method of [`SecretStore`], [`Vault`] and [`PendingChange`] blocks the
//! calling thread: keychain calls can wait for an OS permission prompt and the
//! key derivation deliberately takes a noticeable fraction of a second. Async
//! callers must run them on a blocking thread, for example with
//! `tokio::task::spawn_blocking`, never directly on an async worker.

mod crypto;
pub mod store;
pub mod vault;

use std::fmt;
use std::str::FromStr;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};

pub use crypto::KdfCost;
pub use store::{KeyringStore, MemoryStore, SERVICE_NAME, SecretStore};
pub use vault::{Cleanup, PendingChange, SecretChange, SecurityUpdate, Vault};

/// Owner part of the proxy password reference in configurations written
/// before installations had an id. Every such configuration on a machine
/// shared this one name; it is still read, and replaced by [`proxy_owner`]
/// the next time the proxy password is stored.
pub const PROXY_OWNER: &str = "proxy";
/// The owner part of the proxy password reference of the installation
/// `installation_id`: `proxy-<installation id>`. Every configuration has its
/// own, so configurations sharing one keychain never overwrite or delete each
/// other's proxy password.
pub fn proxy_owner(installation_id: &str) -> String {
    format!("{PROXY_OWNER}-{installation_id}")
}

/// Owner part of the URL shortener token reference of the installation
/// `installation_id`: `shortener-<installation id>`.
pub fn shortener_owner(installation_id: &str) -> String {
    format!("shortener-{installation_id}")
}

/// Slot of an account's secret access key.
pub const SLOT_SECRET: &str = "secret";
/// Slot of an account's static session token.
pub const SLOT_SESSION_TOKEN: &str = "session-token";
/// Slot of the proxy password (owner [`proxy_owner`]).
pub const SLOT_PROXY_PASSWORD: &str = "password";

/// Slot of the URL shortener API token (owner [`shortener_owner`]).
pub const SLOT_SHORTENER_TOKEN: &str = "token";

const MAX_OWNER_LEN: usize = 64;
const MAX_SLOT_LEN: usize = 32;

/// A secret string in memory: zeroized on drop and redacted in `Debug`.
///
/// Read the value with [`SecretString::expose`] and keep the borrow short.
#[derive(Clone, Default, Zeroize, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps `value` without copying it.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The secret value.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

/// Reference of one secret in a [`SecretStore`]: `<owner>/<slot>/<generation>`.
///
/// References are not secret; they are what `config.toml` stores in
/// `secret_ref`, `session_token_ref` and `password_ref`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef {
    text: String,
    owner_len: usize,
    slot_len: usize,
    generation: u32,
}

impl SecretRef {
    /// Builds `<owner>/<slot>/<generation>`.
    ///
    /// `owner` is 1 to 64 ASCII letters, digits, `-` or `_`; `slot` is 1 to 32
    /// lowercase ASCII letters, digits or `-`; `generation` is at least 1.
    pub fn new(owner: &str, slot: &str, generation: u32) -> Result<Self> {
        check_owner(owner)?;
        check_slot(slot)?;
        if generation == 0 {
            return Err(invalid_reference(
                &format!("{owner}/{slot}/0"),
                "generation starts at 1",
            ));
        }
        Ok(Self {
            text: format!("{owner}/{slot}/{generation}"),
            owner_len: owner.len(),
            slot_len: slot.len(),
            generation,
        })
    }

    /// The first generation of `owner/slot`.
    pub fn first(owner: &str, slot: &str) -> Result<Self> {
        Self::new(owner, slot, 1)
    }

    /// Parses a reference read from the configuration.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('/');
        let (Some(owner), Some(slot), Some(generation), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid_reference(
                text,
                "expected <owner>/<slot>/<generation>",
            ));
        };
        let well_formed = !generation.is_empty()
            && generation.bytes().all(|b| b.is_ascii_digit())
            && !generation.starts_with('0');
        let generation = match generation.parse::<u32>() {
            Ok(value) if well_formed => value,
            _ => {
                return Err(invalid_reference(
                    text,
                    "generation must be a decimal number from 1 to 4294967295",
                ));
            }
        };
        Self::new(owner, slot, generation)
    }

    /// The reference string.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The owner: an account id, [`proxy_owner`] or the legacy
    /// [`PROXY_OWNER`].
    pub fn owner(&self) -> &str {
        &self.text[..self.owner_len]
    }

    /// The slot within the owner.
    pub fn slot(&self) -> &str {
        let start = self.owner_len + 1;
        &self.text[start..start + self.slot_len]
    }

    /// The generation, starting at 1.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The same owner and slot, one generation later.
    pub fn next(&self) -> Result<Self> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_reference(&self.text, "generation cannot grow any further"))?;
        Self::new(self.owner(), self.slot(), generation)
    }

    /// Whether `other` addresses the same owner and slot.
    pub fn same_slot(&self, other: &SecretRef) -> bool {
        self.owner() == other.owner() && self.slot() == other.slot()
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretRef({})", self.text)
    }
}

impl FromStr for SecretRef {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl AsRef<str> for SecretRef {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

fn check_owner(owner: &str) -> Result<()> {
    let valid_chars = owner
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if owner.is_empty() || owner.len() > MAX_OWNER_LEN || !valid_chars {
        return Err(invalid_part(
            "owner",
            owner,
            "use 1 to 64 ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

fn check_slot(slot: &str) -> Result<()> {
    let valid_chars = slot
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if slot.is_empty() || slot.len() > MAX_SLOT_LEN || !valid_chars {
        return Err(invalid_part(
            "slot",
            slot,
            "use 1 to 32 lowercase ASCII letters, digits or '-'",
        ));
    }
    Ok(())
}

// Error constructors of this module.

fn invalid_reference(text: &str, reason: &str) -> Error {
    Error::Usage(format!("invalid secret reference {text:?}: {reason}"))
}

fn invalid_part(part: &str, value: &str, rule: &str) -> Error {
    Error::Usage(format!("invalid secret reference {part} {value:?}: {rule}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    const OWNER: &str = "5f0c2a5e-0d6f-4a51-9d3e-2f6f1e0b9c11";

    #[test]
    fn reference_round_trip() -> Result<()> {
        let reference = SecretRef::new(OWNER, SLOT_SECRET, 3)?;
        assert_eq!(reference.as_str(), format!("{OWNER}/secret/3"));
        assert_eq!(reference.owner(), OWNER);
        assert_eq!(reference.slot(), SLOT_SECRET);
        assert_eq!(reference.generation(), 3);
        assert_eq!(SecretRef::parse(reference.as_str())?, reference);
        assert_eq!(reference.as_str().parse::<SecretRef>()?, reference);
        let next = reference.next()?;
        assert_eq!(next.generation(), 4);
        assert!(next.same_slot(&reference));
        assert_eq!(
            SecretRef::first(PROXY_OWNER, SLOT_PROXY_PASSWORD)?.as_str(),
            "proxy/password/1"
        );
        let id = "0b6f1c1e-3b0e-4e43-a5a1-6d8c4f3a2b10";
        assert_eq!(
            SecretRef::first(&proxy_owner(id), SLOT_PROXY_PASSWORD)?.as_str(),
            format!("proxy-{id}/password/1")
        );
        // The longest installation id the configuration accepts still fits.
        assert!(SecretRef::first(&proxy_owner(&"a".repeat(48)), SLOT_PROXY_PASSWORD).is_ok());
        Ok(())
    }

    #[test]
    fn invalid_references_are_rejected() {
        for text in [
            "",
            "owner",
            "owner/secret",
            "owner/secret/1/extra",
            "owner/secret/0",
            "owner/secret/01",
            "owner/secret/+1",
            "owner/secret/-1",
            "owner/secret/4294967296",
            "owner/Secret/1",
            "owner/secret slot/1",
            "own er/secret/1",
            "owner//1",
            "/secret/1",
            "owner/secret/",
            "../secret/1",
        ] {
            let error = SecretRef::parse(text).err();
            assert_eq!(
                error.map(|e| e.kind()),
                Some(ErrorKind::Usage),
                "{text:?} must be rejected"
            );
        }
        let long_owner = "a".repeat(65);
        assert!(SecretRef::new(&long_owner, SLOT_SECRET, 1).is_err());
        assert!(SecretRef::new(&"a".repeat(64), SLOT_SECRET, 1).is_ok());
    }

    #[test]
    fn generation_overflow_is_an_error() -> Result<()> {
        let last = SecretRef::new(OWNER, SLOT_SECRET, u32::MAX)?;
        assert_eq!(last.next().err().map(|e| e.kind()), Some(ErrorKind::Usage));
        Ok(())
    }

    #[test]
    fn secret_string_debug_is_redacted() {
        let secret = SecretString::from("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
        let debug = format!("{secret:?}");
        assert!(!debug.contains("wJalr"));
        assert_eq!(secret.expose(), "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    }

    #[test]
    fn handles_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SecretString>();
        assert_send_sync::<SecretRef>();
        assert_send_sync::<Vault>();
        assert_send_sync::<PendingChange>();
        assert_send_sync::<Cleanup>();
        assert_send_sync::<KeyringStore>();
        assert_send_sync::<MemoryStore>();
    }
}
