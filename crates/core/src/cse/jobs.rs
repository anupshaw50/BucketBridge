//! Encrypt and decrypt options of transfer tasks.
//!
//! A task of the transfer queue is persisted, and a password must never be.
//! The mode therefore travels in the task's `group_id` (an opaque string of
//! the queue), which survives a restart:
//!
//! * `cse-enc-<compression>-<uuid>`: uploads are encrypted;
//! * `cse-dec-<uuid>`: downloads are decrypted.
//!
//! The runner reads the mode from the group ID and the password from
//! [`CsePasswords`] (memory only). A task of such a group that finds no
//! password, for example after a restart, fails with *password not found*:
//! it is never run in the clear.

use std::sync::{Arc, Mutex, PoisonError};

use super::format::Compression;
use crate::secrets::SecretString;

const ENCRYPT_PREFIX: &str = "cse-enc-";
const DECRYPT_PREFIX: &str = "cse-dec-";

/// The most groups whose password is kept; the oldest is forgotten first.
const MAX_GROUPS: usize = 64;

/// What the tasks of a group do besides transferring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CseMode {
    /// Encrypt uploads, compressing with the given algorithm first.
    Encrypt(Compression),
    /// Decrypt downloads of encrypted objects (others are saved as they are).
    Decrypt,
}

impl CseMode {
    /// A fresh group ID that carries this mode.
    pub fn new_group_id(self) -> String {
        let id = uuid::Uuid::new_v4();
        match self {
            CseMode::Encrypt(compression) => format!("{ENCRYPT_PREFIX}{compression}-{id}"),
            CseMode::Decrypt => format!("{DECRYPT_PREFIX}{id}"),
        }
    }

    /// The mode a group ID carries; `None` for an ordinary group.
    pub fn of_group(group_id: Option<&str>) -> Option<CseMode> {
        let group = group_id?;
        if group.starts_with(DECRYPT_PREFIX) {
            return Some(CseMode::Decrypt);
        }
        let rest = group.strip_prefix(ENCRYPT_PREFIX)?;
        let compression = rest.split('-').next()?.parse::<Compression>().ok()?;
        Some(CseMode::Encrypt(compression))
    }
}

/// The passwords of the encrypting and decrypting groups, in memory only.
///
/// Cheap to clone; clones share the passwords. At most 64 groups are kept
/// (the oldest is dropped, and zeroized, first), and everything is dropped
/// with the process.
#[derive(Clone, Default)]
pub struct CsePasswords {
    inner: Arc<Mutex<Vec<(String, SecretString)>>>,
}

impl std::fmt::Debug for CsePasswords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CsePasswords(<redacted>)")
    }
}

impl CsePasswords {
    /// No passwords.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers the password of `group_id`.
    pub fn insert(&self, group_id: &str, password: SecretString) {
        let mut groups = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        groups.retain(|(id, _)| id != group_id);
        groups.push((group_id.to_owned(), password));
        let excess = groups.len().saturating_sub(MAX_GROUPS);
        if excess > 0 {
            // Dropped entries zeroize themselves.
            groups.drain(..excess).for_each(drop);
        }
    }

    /// The password of `group_id`, if it is still known.
    pub fn get(&self, group_id: Option<&str>) -> Option<SecretString> {
        let group_id = group_id?;
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(id, _)| id == group_id)
            .map(|(_, password)| password.clone())
    }

    /// Forgets the password of `group_id`.
    pub fn forget(&self, group_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| id != group_id);
    }

    /// Forgets every password.
    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// How many groups have a password.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether no group has a password.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_carry_the_mode() {
        for mode in [
            CseMode::Encrypt(Compression::None),
            CseMode::Encrypt(Compression::Gzip),
            CseMode::Encrypt(Compression::Zstd),
            CseMode::Decrypt,
        ] {
            let id = mode.new_group_id();
            assert_eq!(CseMode::of_group(Some(&id)), Some(mode), "{id}");
        }
        assert_ne!(
            CseMode::Decrypt.new_group_id(),
            CseMode::Decrypt.new_group_id()
        );
        assert_eq!(CseMode::of_group(None), None);
        assert_eq!(CseMode::of_group(Some("3f2a-plain-uuid")), None);
        assert_eq!(CseMode::of_group(Some("cse-enc-lz4-x")), None);
    }

    #[test]
    fn passwords_are_kept_per_group_and_bounded() {
        let passwords = CsePasswords::new();
        assert!(passwords.is_empty());
        passwords.insert("a", "one".into());
        passwords.insert("a", "two".into());
        assert_eq!(passwords.len(), 1);
        assert_eq!(passwords.get(Some("a")).unwrap().expose(), "two");
        assert!(passwords.get(Some("b")).is_none());
        assert!(passwords.get(None).is_none());
        for n in 0..MAX_GROUPS + 5 {
            passwords.insert(&format!("g{n}"), "x".into());
        }
        assert_eq!(passwords.len(), MAX_GROUPS);
        assert!(passwords.get(Some("a")).is_none());
        assert!(passwords.get(Some("g0")).is_none());
        assert!(
            passwords
                .get(Some(&format!("g{}", MAX_GROUPS + 4)))
                .is_some()
        );
        passwords.forget(&format!("g{}", MAX_GROUPS + 4));
        assert_eq!(passwords.len(), MAX_GROUPS - 1);
        passwords.clear();
        assert!(passwords.is_empty());
        assert_eq!(format!("{passwords:?}"), "CsePasswords(<redacted>)");
    }
}
