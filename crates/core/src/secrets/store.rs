//! [`SecretStore`]: where secret strings live, addressed by reference string.
//!
//! [`KeyringStore`] uses the platform credential store through `keyring-core`
//! and one store crate per OS (the `keyring` crate itself is only a sample
//! front end and installs a process-wide default store, which a library
//! should not do):
//!
//! | OS | Backend | Crate |
//! |---|---|---|
//! | macOS | login Keychain, generic password items | `apple-native-keyring-store` (`keychain`) |
//! | Windows | Credential Manager, generic credentials | `windows-native-keyring-store` |
//! | Linux and BSD | Secret Service (GNOME Keyring, KWallet) over D-Bus | `zbus-secret-service-keyring-store` (`crypto-rust`) |
//!
//! Every item uses the service name [`SERVICE_NAME`] and the reference as the
//! account or user name. On Linux the pure Rust zbus store is used rather
//! than the libdbus one: it needs no system library and generates its D-Bus
//! session key with the OS random generator (the libdbus based store uses a
//! non-cryptographic generator for it), and the transport to the Secret
//! Service is encrypted (`crypto-rust`).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use keyring_core::{CredentialStore, Entry};
use zeroize::Zeroize;

use super::SecretString;
use crate::error::{Error, Result};

/// Service name of every OS keychain item written by BucketBridge.
pub const SERVICE_NAME: &str = "BucketBridge";

/// A store of secret strings addressed by a reference string.
///
/// Values are UTF-8 strings: KDE Wallet can only hold text, and every value
/// the vault writes (plain secrets and base64 envelopes) is text.
///
/// # Blocking
///
/// All methods block the calling thread, possibly for a long time when the OS
/// shows a permission prompt (macOS) or an unlock dialog (Linux). Async code
/// must call them from a blocking thread (`tokio::task::spawn_blocking`).
/// Implementations are `Send + Sync`, so one store can be shared by all
/// threads behind an `Arc`.
pub trait SecretStore: Send + Sync {
    /// Returns the value stored under `reference`, or `None` if there is none.
    fn get(&self, reference: &str) -> Result<Option<SecretString>>;

    /// Creates or replaces the value stored under `reference`.
    fn set(&self, reference: &str, value: &str) -> Result<()>;

    /// Deletes the value stored under `reference`. Deleting a reference that
    /// holds no value succeeds.
    fn delete(&self, reference: &str) -> Result<()>;
}

/// The OS keychain of the current platform.
pub struct KeyringStore {
    store: Arc<CredentialStore>,
    backend: &'static str,
}

impl KeyringStore {
    /// Opens the platform credential store.
    ///
    /// Fails with an [`ErrorKind::Io`](crate::ErrorKind::Io) error naming the backend when none is
    /// available, for example on Linux without a D-Bus session or without a
    /// running Secret Service provider. On macOS and Windows this does not
    /// touch the keychain yet.
    pub fn new() -> Result<Self> {
        let (store, backend) = platform_store()?;
        Ok(Self { store, backend })
    }

    /// Human readable name of the backend, for diagnostics.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    fn entry(&self, reference: &str) -> Result<Entry> {
        check_reference(reference)?;
        self.store
            .build(SERVICE_NAME, reference, None)
            .map_err(|error| keychain_error("open", reference, error))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, reference: &str) -> Result<Option<SecretString>> {
        match self.entry(reference)?.get_secret() {
            Ok(bytes) => utf8_secret(bytes)
                .map(Some)
                .ok_or_else(|| not_utf8(reference)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error("read", reference, error)),
        }
    }

    fn set(&self, reference: &str, value: &str) -> Result<()> {
        self.entry(reference)?
            .set_secret(value.as_bytes())
            .map_err(|error| keychain_error("write", reference, error))
    }

    fn delete(&self, reference: &str) -> Result<()> {
        match self.entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error("delete", reference, error)),
        }
    }
}

impl fmt::Debug for KeyringStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringStore")
            .field("backend", &self.backend)
            .field("service", &SERVICE_NAME)
            .finish()
    }
}

#[cfg(target_os = "macos")]
fn platform_store() -> Result<(Arc<CredentialStore>, &'static str)> {
    const BACKEND: &str = "macOS Keychain";
    let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()
        .map_err(|error| no_backend(BACKEND, "", error))?;
    Ok((store, BACKEND))
}

#[cfg(target_os = "windows")]
fn platform_store() -> Result<(Arc<CredentialStore>, &'static str)> {
    const BACKEND: &str = "Windows Credential Manager";
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()
        .map_err(|error| no_backend(BACKEND, "", error))?;
    Ok((store, BACKEND))
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn platform_store() -> Result<(Arc<CredentialStore>, &'static str)> {
    const BACKEND: &str = "Secret Service";
    let store: Arc<CredentialStore> =
        zbus_secret_service_keyring_store::Store::new().map_err(|error| {
            no_backend(
                BACKEND,
                " (a D-Bus session and a Secret Service provider such as GNOME Keyring \
                 or KWallet must be running)",
                error,
            )
        })?;
    Ok((store, BACKEND))
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )
)))]
fn platform_store() -> Result<(Arc<CredentialStore>, &'static str)> {
    Err(unsupported_platform())
}

fn check_reference(reference: &str) -> Result<()> {
    if reference.is_empty() {
        return Err(empty_reference());
    }
    Ok(())
}

/// Converts keychain bytes without copying; wipes them if they are not UTF-8.
fn utf8_secret(bytes: Vec<u8>) -> Option<SecretString> {
    match String::from_utf8(bytes) {
        Ok(text) => Some(SecretString::new(text)),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            None
        }
    }
}

/// An in-memory [`SecretStore`] for tests and diagnostics. Values are
/// zeroized when replaced, deleted or dropped.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, SecretString>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored values.
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// All references, sorted.
    pub fn references(&self) -> Vec<String> {
        self.entries().keys().cloned().collect()
    }

    /// Every reference with its stored value, sorted by reference.
    pub fn snapshot(&self) -> Vec<(String, SecretString)> {
        self.entries()
            .iter()
            .map(|(reference, value)| (reference.clone(), value.clone()))
            .collect()
    }

    fn entries(&self) -> MutexGuard<'_, BTreeMap<String, SecretString>> {
        // Every critical section is a single map operation, so the map is
        // consistent even if a thread panicked while holding the lock.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, reference: &str) -> Result<Option<SecretString>> {
        check_reference(reference)?;
        Ok(self.entries().get(reference).cloned())
    }

    fn set(&self, reference: &str, value: &str) -> Result<()> {
        check_reference(reference)?;
        self.entries()
            .insert(reference.to_owned(), SecretString::from(value));
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<()> {
        check_reference(reference)?;
        self.entries().remove(reference);
        Ok(())
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStore")
            .field("entries", &self.len())
            .finish()
    }
}

// Error constructors of this module. Keychain errors never carry the
// `keyring_core::Error` itself: some of its variants hold the raw secret
// bytes, which are wiped here instead. Platform errors are kept as the
// payload of the `io::Error`.

fn keychain_error(action: &str, reference: &str, error: keyring_core::Error) -> Error {
    use keyring_core::Error as KeyringError;
    use std::io::{Error as IoError, ErrorKind as IoErrorKind};
    let context = || format!("cannot {action} {reference:?} in the OS keychain");
    match error {
        KeyringError::NoStorageAccess(source) => Error::io(
            format!(
                "{} (access was denied or the keychain is locked)",
                context()
            ),
            IoError::new(IoErrorKind::PermissionDenied, source),
        ),
        KeyringError::PlatformFailure(source) => Error::io(context(), IoError::other(source)),
        KeyringError::NoEntry => {
            Error::NotFound(format!("{reference:?} is not in the OS keychain"))
        }
        KeyringError::BadEncoding(mut bytes) => {
            bytes.zeroize();
            not_utf8(reference)
        }
        KeyringError::BadDataFormat(mut bytes, source) => {
            bytes.zeroize();
            Error::io(
                format!("{} (the stored data is malformed)", context()),
                IoError::new(IoErrorKind::InvalidData, source),
            )
        }
        KeyringError::BadStoreFormat(reason) => Error::io(
            context(),
            IoError::new(
                IoErrorKind::InvalidData,
                format!("the OS keychain is malformed: {reason}"),
            ),
        ),
        KeyringError::TooLong(field, limit) => Error::Usage(format!(
            "{}: the {field} exceeds the OS keychain limit of {limit}",
            context()
        )),
        KeyringError::Invalid(field, reason) => {
            Error::Usage(format!("{}: invalid {field}: {reason}", context()))
        }
        KeyringError::Ambiguous(items) => Error::io(
            context(),
            IoError::other(format!("{} OS keychain items match it", items.len())),
        ),
        KeyringError::NotSupportedByStore(reason) => Error::Internal(format!(
            "{}: not supported by the OS keychain: {reason}",
            context()
        )),
        _ => Error::io(context(), IoError::other("unknown keychain error")),
    }
}

fn not_utf8(reference: &str) -> Error {
    Error::io(
        format!("cannot read {reference:?} in the OS keychain"),
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the stored value is not valid UTF-8",
        ),
    )
}

fn empty_reference() -> Error {
    Error::Usage("a secret reference must not be empty".to_owned())
}

#[cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "windows",
        all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        )
    )),
    allow(dead_code)
)]
fn no_backend(backend: &str, hint: &str, error: keyring_core::Error) -> Error {
    let context = format!("no OS keychain is available: cannot open the {backend}{hint}");
    let source = match error {
        keyring_core::Error::PlatformFailure(source)
        | keyring_core::Error::NoStorageAccess(source) => std::io::Error::other(source),
        _ => std::io::Error::other("the credential store could not be initialized"),
    };
    Error::io(context, source)
}

#[cfg_attr(
    any(
        target_os = "macos",
        target_os = "windows",
        all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        )
    ),
    allow(dead_code)
)]
fn unsupported_platform() -> Error {
    Error::io(
        "no OS keychain is available",
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this platform has no supported credential store",
        ),
    )
}

/// Test support shared by the vault and account-manager tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, PoisonError};

    use super::{MemoryStore, SecretStore};
    use crate::error::{Error, Result};
    use crate::secrets::SecretString;

    /// A store that counts operations and fails the n-th write or delete
    /// after it was armed.
    #[derive(Default)]
    pub(crate) struct FlakyStore {
        pub(crate) inner: MemoryStore,
        writes: AtomicUsize,
        deletes: AtomicUsize,
        fail_write_in: AtomicUsize,
        fail_delete_in: AtomicUsize,
        failing_deletes: AtomicUsize,
        before_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl FlakyStore {
        pub(crate) fn fail_nth_write(&self, n: usize) {
            self.fail_write_in.store(n, Ordering::SeqCst);
        }

        pub(crate) fn fail_nth_delete(&self, n: usize) {
            self.fail_delete_in.store(n, Ordering::SeqCst);
        }

        /// Fails the next `n` deletes.
        pub(crate) fn fail_next_deletes(&self, n: usize) {
            self.failing_deletes.store(n, Ordering::SeqCst);
        }

        /// Runs `hook` just before the next write is applied.
        pub(crate) fn before_next_write(&self, hook: impl FnOnce() + Send + 'static) {
            *self
                .before_write
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
        }

        pub(crate) fn operations(&self) -> (usize, usize) {
            (
                self.writes.load(Ordering::SeqCst),
                self.deletes.load(Ordering::SeqCst),
            )
        }

        pub(crate) fn raw(&self) -> Vec<(String, String)> {
            self.inner
                .snapshot()
                .into_iter()
                .map(|(reference, value)| (reference, value.expose().to_owned()))
                .collect()
        }

        fn trips(counter: &AtomicUsize) -> bool {
            counter
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok_and(|previous| previous == 1)
        }
    }

    impl SecretStore for FlakyStore {
        fn get(&self, reference: &str) -> Result<Option<SecretString>> {
            self.inner.get(reference)
        }

        fn set(&self, reference: &str, value: &str) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            let hook = self
                .before_write
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(hook) = hook {
                hook();
            }
            if Self::trips(&self.fail_write_in) {
                return Err(injected("write"));
            }
            self.inner.set(reference, value)
        }

        fn delete(&self, reference: &str) -> Result<()> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            let failing = self
                .failing_deletes
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if failing || Self::trips(&self.fail_delete_in) {
                return Err(injected("delete"));
            }
            self.inner.delete(reference)
        }
    }

    fn injected(operation: &str) -> Error {
        Error::io(
            format!("injected {operation} failure"),
            std::io::Error::other("test"),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn memory_store_semantics() -> Result<()> {
        let store = MemoryStore::new();
        assert!(store.is_empty());
        assert!(store.get("a/secret/1")?.is_none());
        store.set("a/secret/1", "first")?;
        assert_eq!(store.get("a/secret/1")?.unwrap().expose(), "first");
        store.set("a/secret/1", "second")?;
        assert_eq!(store.get("a/secret/1")?.unwrap().expose(), "second");
        assert_eq!(store.len(), 1);
        store.delete("a/secret/1")?;
        store.delete("a/secret/1")?;
        assert!(store.get("a/secret/1")?.is_none());
        assert_eq!(
            store.set("", "x").err().map(|e| e.kind()),
            Some(ErrorKind::Usage)
        );
        Ok(())
    }

    #[test]
    fn memory_store_debug_shows_no_values() -> Result<()> {
        let store = MemoryStore::new();
        store.set("a/secret/1", "hunter2-secret-value")?;
        let debug = format!("{store:?}");
        assert!(!debug.contains("hunter2"));
        Ok(())
    }

    #[test]
    fn keychain_errors_never_carry_secret_bytes() {
        let secret = b"wJalrXUtnFEMI/K7MDENG".to_vec();
        let errors = [
            keychain_error(
                "read",
                "a/secret/1",
                keyring_core::Error::BadEncoding(secret.clone()),
            ),
            keychain_error(
                "read",
                "a/secret/1",
                keyring_core::Error::BadDataFormat(
                    secret.clone(),
                    Box::new(std::io::Error::other("bad padding")),
                ),
            ),
        ];
        for error in errors {
            assert_eq!(error.kind(), ErrorKind::Io);
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("wJalr"), "{rendered}");
            assert!(!rendered.contains("119, 74"), "{rendered}");
            assert!(rendered.contains("a/secret/1"));
        }
    }

    #[test]
    fn keychain_error_kinds() {
        let denied = keychain_error(
            "write",
            "a/secret/1",
            keyring_core::Error::NoStorageAccess(Box::new(std::io::Error::other("locked"))),
        );
        assert_eq!(denied.kind(), ErrorKind::Io);
        assert!(std::error::Error::source(&denied).is_some());
        let too_long = keychain_error(
            "write",
            "a/secret/1",
            keyring_core::Error::TooLong("secret".to_owned(), 2560),
        );
        assert_eq!(too_long.kind(), ErrorKind::Usage);
        assert!(too_long.to_string().contains("2560"));
        let missing = keychain_error("read", "a/secret/1", keyring_core::Error::NoEntry);
        assert_eq!(missing.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn non_utf8_values_are_wiped_and_rejected() {
        assert!(utf8_secret(vec![0xff, 0xfe, 0x00]).is_none());
        assert_eq!(utf8_secret(b"ok".to_vec()).unwrap().expose(), "ok");
    }

    /// Round trip through the real OS keychain. Never runs by default: it is
    /// ignored and additionally requires `BB_TEST_KEYCHAIN=1`.
    #[test]
    #[ignore = "touches the real OS keychain; run with BB_TEST_KEYCHAIN=1 and --ignored"]
    fn real_keychain_round_trip() -> Result<()> {
        if std::env::var("BB_TEST_KEYCHAIN").as_deref() != Ok("1") {
            return Ok(());
        }
        let store = KeyringStore::new()?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("random");
        let owner: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let reference = format!("bb-test-{owner}/secret/1");
        store.delete(&reference)?;
        assert!(store.get(&reference)?.is_none());
        store.set(&reference, "first value")?;
        assert_eq!(store.get(&reference)?.unwrap().expose(), "first value");
        store.set(&reference, "second value \u{e4}\u{f6}")?;
        assert_eq!(
            store.get(&reference)?.unwrap().expose(),
            "second value \u{e4}\u{f6}"
        );
        store.delete(&reference)?;
        store.delete(&reference)?;
        assert!(store.get(&reference)?.is_none());
        Ok(())
    }
}
