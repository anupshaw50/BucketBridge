//! Configuration file (`config.toml`) and application directories.
//!
//! The configuration never holds secrets: accounts and proxy settings only
//! carry `*_ref` keys into the secret store.
//!
//! # Concurrency and durability
//!
//! [`ConfigStore::update`] is the only way to write: a read-modify-write
//! cycle under an exclusive advisory lock on `config.toml.lock`, so the
//! desktop app and the CLI never lose each other's changes (there is
//! deliberately no "save this snapshot" call, which would overwrite changes
//! made since the snapshot was loaded). The new contents go to a temporary
//! file in the same directory, which is flushed to disk and then renamed
//! over `config.toml`, so readers always see either the old or the new
//! file. On Unix the file is created with mode `0600` and newly created
//! directories with mode `0700`.
//!
//! A file that cannot be parsed, has invalid settings, or was written by a
//! newer format version, is reported as [`Error::Config`] and is never
//! overwritten: [`ConfigStore::update`] fails before changing anything.
//!
//! # Installation id
//!
//! The first time the file is written it gets a random `installation_id`
//! (a UUID v4), which every later write keeps. It names this installation
//! in the OS keychain: the proxy password lives under an owner derived from
//! it (see [`crate::secrets::proxy_owner`]), so two configuration folders on
//! one machine (`BB_CONFIG_DIR`, `--config-dir`) never share or overwrite
//! each other's items. A byte-for-byte copy of `config.toml` is the same
//! installation: it carries the id along.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind as IoErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::model::{Account, SavedFilter};
use crate::settings::Settings;

/// Current configuration format version.
pub const CONFIG_VERSION: u32 = 1;

/// Environment variable that relocates every BucketBridge directory.
pub const CONFIG_DIR_ENV: &str = "BB_CONFIG_DIR";

/// File name of the configuration inside the config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Longest installation id accepted when reading the file: it becomes part
/// of a secret reference owner, which is limited to 64 characters.
const MAX_INSTALLATION_ID_LEN: usize = 48;
/// Shortest installation id accepted when reading the file.
const MIN_INSTALLATION_ID_LEN: usize = 8;

/// Everything stored in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Format version; see [`CONFIG_VERSION`].
    pub version: u32,
    /// Random id of this installation, created the first time the file is
    /// written and kept afterwards; `None` in a file written before ids
    /// existed. See the [module docs](self).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    /// Stored accounts.
    pub accounts: Vec<Account>,
    /// Account used when none is named.
    pub default_account_id: Option<String>,
    /// Application settings.
    pub settings: Settings,
    /// Master-password parameters; `None` when no master password is set.
    pub security: Option<SecurityConfig>,
    /// Saved object filters.
    pub saved_filters: Vec<SavedFilter>,
    /// External buckets pinned to an account: account id to bucket names
    /// (see [`crate::bucketcfg`]). Left out of the file while empty.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pinned_buckets: BTreeMap<String, Vec<String>>,
    /// Saved CloudFront invalidation path sets (see [`crate::cloudfront`]).
    /// Left out of the file while empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalidation_path_sets: Vec<crate::cloudfront::PathSet>,
    /// Saved locations (see [`crate::bookmarks`]). Left out of the file while
    /// empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            version: CONFIG_VERSION,
            installation_id: None,
            accounts: Vec::new(),
            default_account_id: None,
            settings: Settings::default(),
            security: None,
            saved_filters: Vec::new(),
            pinned_buckets: BTreeMap::new(),
            invalidation_path_sets: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

/// Master-password verification data (no secrets; the derived key is never stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Key-derivation parameters.
    pub kdf: KdfParams,
    /// Base64 verifier that proves a derived key is correct.
    pub verifier_b64: String,
}

/// Parameters of the password key-derivation function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Algorithm name, for example `"argon2id"`.
    pub algorithm: String,
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
    /// Base64 salt.
    pub salt_b64: String,
}

/// The directories BucketBridge reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Holds `config.toml`.
    pub config_dir: PathBuf,
    /// Machine-local data such as the persisted transfer queue.
    pub data_dir: PathBuf,
    /// Event-log files.
    pub log_dir: PathBuf,
    /// Crash reports.
    pub crash_dir: PathBuf,
}

impl Dirs {
    /// Uses `$BB_CONFIG_DIR` when it is set and non-empty, the platform
    /// directories otherwise.
    pub fn discover() -> Result<Self> {
        Self::from_override(std::env::var_os(CONFIG_DIR_ENV))
    }

    /// Like [`Dirs::discover`], with the value of `BB_CONFIG_DIR` passed in.
    /// A relative override is resolved against the current directory.
    pub fn from_override(value: Option<OsString>) -> Result<Self> {
        match value {
            Some(value) if !value.is_empty() => {
                let root = std::path::absolute(PathBuf::from(&value)).io_context(|| {
                    format!(
                        "resolving {CONFIG_DIR_ENV}={}",
                        PathBuf::from(&value).display()
                    )
                })?;
                Ok(Dirs::under(root))
            }
            _ => Self::platform(),
        }
    }

    /// Everything under one folder: `config.toml` directly in it, then
    /// `data/`, `logs/` and `crash/`. Used for `BB_CONFIG_DIR`, portable
    /// installs and tests.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Dirs {
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
            crash_dir: root.join("crash"),
            config_dir: root,
        }
    }

    /// The per-user platform directories: the config directory for
    /// `config.toml`, the local data directory for data, and the state
    /// directory (Linux) or local data directory (macOS, Windows) for
    /// `logs/` and `crash/`.
    pub fn platform() -> Result<Self> {
        let project = directories::ProjectDirs::from("", "", "BucketBridge").ok_or_else(|| {
            Error::io(
                format!(
                    "locating the configuration directory (set {CONFIG_DIR_ENV} to choose one)"
                ),
                std::io::Error::new(IoErrorKind::NotFound, "no home directory found"),
            )
        })?;
        let state = project
            .state_dir()
            .unwrap_or_else(|| project.data_local_dir())
            .to_path_buf();
        Ok(Dirs {
            config_dir: project.config_dir().to_path_buf(),
            data_dir: project.data_local_dir().to_path_buf(),
            log_dir: state.join("logs"),
            crash_dir: state.join("crash"),
        })
    }

    /// Path of `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    /// Creates every directory that does not exist yet (mode `0700` on Unix;
    /// existing directories are left as they are).
    pub fn create_all(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.log_dir,
            &self.crash_dir,
        ] {
            create_private_dir(dir)?;
        }
        Ok(())
    }
}

/// Reads and writes `config.toml` safely across threads and processes.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// The store for `dirs.config_file()`.
    pub fn new(dirs: &Dirs) -> Self {
        ConfigStore {
            path: dirs.config_file(),
        }
    }

    /// A store for an explicit file path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        ConfigStore { path: path.into() }
    }

    /// Path of the configuration file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the configuration. A missing file yields the defaults (and
    /// creates nothing). This is a blocking call.
    ///
    /// The result is a snapshot for reading. To change the configuration,
    /// apply the change inside [`ConfigStore::update`], which re-reads the
    /// file under the lock; never write back a snapshot.
    pub fn load(&self) -> Result<AppConfig> {
        self.read()
    }

    /// Read-modify-write under the exclusive lock, creating the directory if
    /// needed; the only way to write the configuration. `change` receives
    /// the current file contents (the defaults when there is no file), so
    /// concurrent writers in other threads or processes never lose each
    /// other's changes.
    ///
    /// A configuration without an installation id gets a new one before
    /// `change` runs (see the [module docs](self)).
    ///
    /// When `change` fails, or leaves invalid settings, nothing is written. A
    /// file that is corrupt, has invalid settings or a newer format version
    /// makes this fail with [`Error::Config`] before `change` runs, and the
    /// file is left untouched. This is a blocking call.
    pub fn update<T>(&self, change: impl FnOnce(&mut AppConfig) -> Result<T>) -> Result<T> {
        let _lock = self.lock()?;
        let mut config = self.read()?;
        // The id exists before `change` runs, so a caller that compares what
        // it wrote with what is on disk afterwards sees the same id.
        ensure_installation_id(&mut config)?;
        let out = change(&mut config)?;
        // A change that replaced the whole configuration must not drop it.
        ensure_installation_id(&mut config)?;
        config.settings.validate()?;
        self.write(&config)?;
        Ok(out)
    }

    fn dir(&self) -> &Path {
        match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        }
    }

    fn lock(&self) -> Result<File> {
        create_private_dir(self.dir())?;
        let mut lock_name = self.path.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .io_context(|| format!("opening lock file {}", lock_path.display()))?;
        file.lock()
            .io_context(|| format!("locking {}", lock_path.display()))?;
        Ok(file)
    }

    fn read(&self) -> Result<AppConfig> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == IoErrorKind::NotFound => return Ok(AppConfig::default()),
            Err(err) if err.kind() == IoErrorKind::InvalidData => {
                return Err(self.invalid("the file is not valid UTF-8"));
            }
            Err(err) => return Err(Error::io(format!("reading {}", self.path.display()), err)),
        };
        let config: AppConfig = toml::from_str(&text)
            .map_err(|err| self.invalid(&format!("the file is not valid TOML: {err}")))?;
        if config.version == 0 || config.version > CONFIG_VERSION {
            return Err(self.invalid(&format!(
                "format version {} is not supported by this build (supported: 1 to {CONFIG_VERSION}); \
                 it was probably written by a newer BucketBridge, so it is left untouched",
                config.version
            )));
        }
        config
            .settings
            .validate()
            .map_err(|err| self.invalid(&err.to_string()))?;
        if let Some(id) = &config.installation_id
            && !is_valid_installation_id(id)
        {
            return Err(self.invalid(&format!(
                "installation_id must be {MIN_INSTALLATION_ID_LEN} to {MAX_INSTALLATION_ID_LEN} \
                 ASCII letters, digits, '-' or '_'"
            )));
        }
        Ok(config)
    }

    fn write(&self, config: &AppConfig) -> Result<()> {
        let mut config = config.clone();
        config.version = CONFIG_VERSION;
        let text = toml::to_string_pretty(&config)
            .map_err(|err| Error::Usage(format!("the configuration cannot be saved: {err}")))?;

        let dir = self.dir();
        create_private_dir(dir)?;
        let mut temp = tempfile::Builder::new()
            .prefix(".config.")
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
        temp.write_all(text.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .io_context(|| format!("writing {}", temp.path().display()))?;
        temp.persist(&self.path)
            .map_err(|err| Error::io(format!("replacing {}", self.path.display()), err.error))?;
        sync_dir(dir)?;
        Ok(())
    }

    fn invalid(&self, message: &str) -> Error {
        Error::Config {
            path: self.path.clone(),
            message: message.to_owned(),
        }
    }
}

/// Creates `dir` and its missing parents with mode `0700` on Unix.
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

fn is_valid_installation_id(id: &str) -> bool {
    (MIN_INSTALLATION_ID_LEN..=MAX_INSTALLATION_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn ensure_installation_id(config: &mut AppConfig) -> Result<()> {
    if config.installation_id.is_none() {
        config.installation_id = Some(new_uuid_v4()?);
    }
    Ok(())
}

/// A random UUID v4 (RFC 9562) from the OS random number generator: the ids
/// of accounts and of the installation.
pub(crate) fn new_uuid_v4() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        Error::Internal(format!("the OS random number generator failed: {error}"))
    })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// Makes a completed rename durable (Unix only; Windows has no directory handles for this).
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)
            .and_then(|handle| handle.sync_all())
            .io_context(|| format!("flushing directory {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::FilterSpec;
    use crate::settings::ChecksumAlgorithm;

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(&Dirs::under(dir.path().join("cfg")));
        (dir, store)
    }

    /// Replaces the whole configuration through `update` (test setup).
    fn replace(store: &ConfigStore, config: &AppConfig) -> Result<()> {
        store.update(|current| {
            *current = config.clone();
            Ok(())
        })
    }

    fn account(n: usize) -> Account {
        Account {
            id: format!("id-{n}"),
            label: format!("Account {n}"),
            access_key_id: format!("AKIA{n}"),
            secret_ref: format!("account/{n}/secret"),
            ..Account::default()
        }
    }

    #[test]
    fn missing_file_loads_defaults_without_creating_anything() {
        let (_dir, store) = store();
        assert_eq!(store.load().unwrap(), AppConfig::default());
        assert!(!store.path().exists());
        assert!(!store.path().parent().unwrap().exists());
    }

    #[test]
    fn round_trip_preserves_everything() {
        let (_dir, store) = store();
        let mut config = AppConfig {
            installation_id: Some("0b6f1c1e-3b0e-4e43-a5a1-6d8c4f3a2b10".into()),
            ..AppConfig::default()
        };
        let mut with_endpoint = account(1);
        with_endpoint.endpoint = Some("http://127.0.0.1:9000".into());
        with_endpoint.path_style = true;
        with_endpoint.session_token_ref = Some("account/1/token".into());
        config.accounts = vec![with_endpoint, account(2)];
        config.default_account_id = Some("id-2".into());
        config.settings.data_integrity.checksum_algorithm = ChecksumAlgorithm::Sha256;
        config.settings.transfer.bandwidth_limit_kibps = Some(2048);
        config.settings.general.default_download_dir = Some(PathBuf::from("/downloads"));
        config.settings.proxy.bypass = vec!["*.example.com".into(), "10.0.0.0/8".into()];
        config.security = Some(SecurityConfig {
            kdf: KdfParams {
                algorithm: "argon2id".into(),
                m_cost_kib: 65536,
                t_cost: 3,
                p_cost: 1,
                salt_b64: "c2FsdA==".into(),
            },
            verifier_b64: "dmVyaWZpZXI=".into(),
        });
        config.saved_filters = vec![SavedFilter {
            name: "logs".into(),
            filter: FilterSpec {
                prefix: Some("logs/".into()),
                ..FilterSpec::default()
            },
        }];
        replace(&store, &config).unwrap();
        assert_eq!(store.load().unwrap(), config);
    }

    #[cfg(unix)]
    #[test]
    fn file_and_directory_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, store) = store();
        replace(&store, &AppConfig::default()).unwrap();
        let file_mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
        let mut lock = store.path().as_os_str().to_owned();
        lock.push(".lock");
        assert_eq!(
            fs::metadata(PathBuf::from(lock))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let (_dir, store) = store();
        for _ in 0..3 {
            replace(&store, &AppConfig::default()).unwrap();
        }
        let names: Vec<String> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let mut names = names;
        names.sort();
        assert_eq!(
            names,
            vec!["config.toml".to_owned(), "config.toml.lock".to_owned()]
        );
    }

    #[test]
    fn partial_and_older_files_load_with_defaults() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            "[settings.advanced]\nmax_attempts = 2\n\n[[accounts]]\nid = \"a\"\nlabel = \"L\"\naccess_key_id = \"K\"\nsecret_ref = \"s\"\n",
        )
        .unwrap();
        let config = store.load().unwrap();
        assert_eq!(config.version, CONFIG_VERSION);
        assert_eq!(config.settings.advanced.max_attempts, 2);
        assert_eq!(config.settings.advanced.list_page_size, 1000);
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].region, "us-east-1");
    }

    #[test]
    fn corrupt_file_is_reported_and_never_overwritten() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let garbage = "this is = = not toml [[[";
        fs::write(store.path(), garbage).unwrap();

        let err = store.load().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        let text = err.to_string();
        assert!(text.contains("not valid TOML"), "{text}");
        assert!(text.contains("config.toml"), "{text}");

        let mut called = false;
        let err = store
            .update(|_| {
                called = true;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        assert!(!called);
        assert_eq!(fs::read_to_string(store.path()).unwrap(), garbage);
    }

    #[test]
    fn non_utf8_file_is_reported() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), [0xff, 0xfe, 0x00]).unwrap();
        let err = store.load().unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn newer_or_zero_version_is_refused() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "version = 99\n").unwrap();
        let err = store.load().unwrap_err();
        assert!(err.to_string().contains("format version 99"), "{err}");
        fs::write(store.path(), "version = 0\n").unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn invalid_settings_in_file_are_reported() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "[settings.advanced]\nlist_page_size = 0\n").unwrap();
        let err = store.load().unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        assert!(err.to_string().contains("list_page_size"), "{err}");
    }

    #[test]
    fn update_rejects_invalid_settings() {
        let (_dir, store) = store();
        let mut config = AppConfig::default();
        config.settings.transfer.part_size_mib = 1;
        let err = replace(&store, &config).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(!store.path().exists());
    }

    #[test]
    fn update_never_touches_a_file_it_cannot_read() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let newer = "version = 2\n\n[[accounts]]\nid = \"new\"\nlabel = \"N\"\naccess_key_id = \"A\"\nsecret_ref = \"s\"\n";
        let files: [&[u8]; 4] = [
            newer.as_bytes(),
            b"version = 0\n",
            b"[settings.advanced]\nlist_page_size = 0\n",
            &[0xff, 0xfe, 0x00],
        ];
        for contents in files {
            fs::write(store.path(), contents).unwrap();
            let mut called = false;
            let err = store
                .update(|config| {
                    called = true;
                    config.settings.interface.flat_browsing = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(matches!(err, Error::Config { .. }), "{err}");
            assert_eq!(err.kind(), ErrorKind::Io);
            assert!(!called);
            assert_eq!(fs::read(store.path()).unwrap(), contents);
        }
    }

    #[test]
    fn a_stale_snapshot_never_drops_concurrent_changes() {
        // The desktop app loads the configuration at startup ...
        let (_dir, store) = store();
        let snapshot = store.load().unwrap();
        assert!(!snapshot.settings.interface.flat_browsing);
        // ... the CLI adds an account meanwhile ...
        store
            .update(|config| {
                config.accounts.push(account(1));
                Ok(())
            })
            .unwrap();
        // ... and the desktop app then changes a setting: the change is
        // applied to the current file, not to its old snapshot.
        store
            .update(|config| {
                config.settings.interface.flat_browsing = true;
                Ok(())
            })
            .unwrap();
        let after = store.load().unwrap();
        assert_eq!(after.accounts, vec![account(1)]);
        assert!(after.settings.interface.flat_browsing);
    }

    #[test]
    fn update_failure_writes_nothing() {
        let (_dir, store) = store();
        replace(&store, &AppConfig::default()).unwrap();
        let before = fs::read_to_string(store.path()).unwrap();
        let err = store
            .update(|config| {
                config.accounts.push(account(1));
                Err::<(), _>(Error::Conflict("label already used".into()))
            })
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
        let err = store
            .update(|config| {
                config.settings.advanced.list_page_size = 0;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert_eq!(fs::read_to_string(store.path()).unwrap(), before);
    }

    #[test]
    fn concurrent_updates_do_not_lose_changes() {
        let (_dir, store) = store();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        store
                            .update(|config| {
                                config.accounts.push(account(t * 100 + i));
                                Ok(())
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(store.load().unwrap().accounts.len(), 80);
    }

    #[test]
    fn update_returns_the_closure_value() {
        let (_dir, store) = store();
        let count = store
            .update(|config| {
                config.accounts.push(account(1));
                Ok(config.accounts.len())
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(store.load().unwrap().accounts, vec![account(1)]);
    }

    #[test]
    fn the_installation_id_is_created_on_first_write_and_kept() {
        let (_dir, store) = store();
        // Reading creates nothing, and a file written before ids existed has none.
        assert_eq!(store.load().unwrap().installation_id, None);
        assert!(!store.path().exists());
        let mut seen = None;
        store
            .update(|config| {
                seen = config.installation_id.clone();
                Ok(())
            })
            .unwrap();
        // The id exists before the change runs and is what gets written.
        let id = seen.expect("an id before the change");
        assert_eq!(
            store.load().unwrap().installation_id.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(
            text.contains(&format!("installation_id = \"{id}\"")),
            "{text}"
        );

        // Later writes keep it, also a change that replaces the whole
        // configuration (the id is then not carried over by the change).
        store
            .update(|config| {
                config.accounts.push(account(1));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.load().unwrap().installation_id.as_deref(),
            Some(id.as_str())
        );
        replace(&store, &AppConfig::default()).unwrap();
        let after = store.load().unwrap().installation_id.unwrap();
        assert_ne!(
            after, id,
            "a replaced configuration starts a new installation"
        );
        let other = ConfigStore::at(store.path().with_file_name("other.toml"));
        other.update(|_| Ok(())).unwrap();
        assert_ne!(other.load().unwrap().installation_id.unwrap(), after);
    }

    #[test]
    fn an_older_file_gets_an_id_at_its_first_write_without_losing_anything() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(
            store.path(),
            "[[accounts]]\nid = \"a\"\nlabel = \"L\"\naccess_key_id = \"K\"\nsecret_ref = \"s\"\n",
        )
        .unwrap();
        let before = store.load().unwrap();
        assert_eq!(before.installation_id, None);
        store
            .update(|config| {
                config.settings.interface.flat_browsing = true;
                Ok(())
            })
            .unwrap();
        let after = store.load().unwrap();
        assert!(after.installation_id.is_some());
        assert_eq!(after.accounts, before.accounts);
        assert!(after.settings.interface.flat_browsing);
    }

    #[test]
    fn invalid_installation_ids_are_reported_and_never_overwritten() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        for id in [
            "",
            "short",
            "has/slash/in-it-1234",
            "with space in it 1234",
            &"a".repeat(49),
        ] {
            let text = format!("installation_id = \"{id}\"\n");
            fs::write(store.path(), &text).unwrap();
            let err = store.load().unwrap_err();
            assert!(matches!(err, Error::Config { .. }), "{id:?}: {err}");
            assert!(err.to_string().contains("installation_id"), "{err}");
            assert!(store.update(|_| Ok(())).is_err());
            assert_eq!(fs::read_to_string(store.path()).unwrap(), text);
        }
        fs::write(store.path(), "installation_id = \"ok-id_123\"\n").unwrap();
        assert_eq!(
            store.load().unwrap().installation_id.as_deref(),
            Some("ok-id_123")
        );
    }

    #[test]
    fn dirs_under_root() {
        let dirs = Dirs::under("/portable/bb");
        assert_eq!(dirs.config_dir, PathBuf::from("/portable/bb"));
        assert_eq!(dirs.data_dir, PathBuf::from("/portable/bb/data"));
        assert_eq!(dirs.log_dir, PathBuf::from("/portable/bb/logs"));
        assert_eq!(dirs.crash_dir, PathBuf::from("/portable/bb/crash"));
        assert_eq!(
            dirs.config_file(),
            PathBuf::from("/portable/bb/config.toml")
        );
    }

    #[test]
    fn override_is_absolute_and_empty_means_platform() {
        let dirs = Dirs::from_override(Some(OsString::from("relative/bb"))).unwrap();
        assert!(dirs.config_dir.is_absolute());
        assert!(dirs.config_dir.ends_with("relative/bb"));
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::from_override(Some(tmp.path().as_os_str().to_owned())).unwrap();
        assert_eq!(dirs.config_dir, tmp.path());
        if let Ok(platform) = Dirs::platform() {
            assert_eq!(
                Dirs::from_override(Some(OsString::new())).unwrap(),
                platform
            );
            assert_eq!(Dirs::from_override(None).unwrap(), platform);
            assert!(platform.config_dir.is_absolute());
        }
    }

    #[test]
    fn create_all_makes_every_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(tmp.path().join("root"));
        dirs.create_all().unwrap();
        for dir in [
            &dirs.config_dir,
            &dirs.data_dir,
            &dirs.log_dir,
            &dirs.crash_dir,
        ] {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        dirs.create_all().unwrap();
    }

    #[test]
    fn store_at_relative_file_name_uses_current_dir_parent() {
        let store = ConfigStore::at("config.toml");
        assert_eq!(store.dir(), Path::new("."));
    }
}
