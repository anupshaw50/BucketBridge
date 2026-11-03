//! Application settings (the `Settings` type of the desktop IPC contract).
//!
//! Every struct uses `#[serde(default)]`, so a partial or older settings
//! table loads with the documented defaults filled in, and unknown fields are
//! ignored. [`Settings::validate`] rejects out-of-range values; it is called
//! when the configuration is loaded and before it is saved.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::LogLevel;

/// 1 MiB in bytes.
pub const MIB: u64 = 1024 * 1024;

/// Smallest multipart part size S3 accepts (except for the last part).
pub const MIN_PART_SIZE_MIB: u64 = 5;

/// Largest multipart part size S3 accepts, and the largest single `PutObject`.
pub const MAX_PART_SIZE_MIB: u64 = 5 * 1024;

/// All settings pages.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// General page.
    pub general: GeneralSettings,
    /// Interface page.
    pub interface: InterfaceSettings,
    /// Advanced page (network behaviour).
    pub advanced: AdvancedSettings,
    /// Data integrity page.
    pub data_integrity: DataIntegritySettings,
    /// Logging page.
    pub logging: LoggingSettings,
    /// Proxy page.
    pub proxy: ProxySettings,
    /// Transfer queue settings.
    pub transfer: TransferSettings,
    /// Optional URL shortener for share links.
    pub shortener: ShortenerSettings,
}

/// General settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// Ask before deleting. Default `true`.
    pub confirm_delete: bool,
    /// Download folder; `None` asks every time. Default `None`.
    pub default_download_dir: Option<PathBuf>,
    /// Applications chosen in "Open with", most recent first (at most
    /// [`MAX_OPEN_WITH_RECENT`]). Default empty.
    pub open_with_recent: Vec<String>,
}

/// How many applications `GeneralSettings::open_with_recent` keeps.
pub const MAX_OPEN_WITH_RECENT: usize = 8;

impl GeneralSettings {
    /// Records `app` as the most recently used "Open with" application:
    /// moved to the front when it is already listed, the list cut to
    /// [`MAX_OPEN_WITH_RECENT`].
    pub fn remember_open_with(&mut self, app: &str) {
        self.open_with_recent.retain(|known| known != app);
        self.open_with_recent.insert(0, app.to_owned());
        self.open_with_recent.truncate(MAX_OPEN_WITH_RECENT);
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings {
            confirm_delete: true,
            default_download_dir: None,
            open_with_recent: Vec::new(),
        }
    }
}

/// Colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    /// Light theme.
    Light,
    /// Dark theme.
    Dark,
}

/// Interface settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceSettings {
    /// Default `System`.
    pub theme: Theme,
    /// List objects recursively instead of folder by folder. Default `false`.
    pub flat_browsing: bool,
    /// Default `true`.
    pub show_transfer_panel: bool,
    /// Default `false`.
    pub show_log_panel: bool,
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        InterfaceSettings {
            theme: Theme::System,
            flat_browsing: false,
            show_transfer_panel: true,
            show_log_panel: false,
        }
    }
}

/// Network behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Attempts per request, including the first (SDK standard retry
    /// mode). Default 5, range 1..=20.
    pub max_attempts: u32,
    /// TCP and TLS connect timeout. Default 10, range 1..=300.
    pub connect_timeout_secs: u64,
    /// Time allowed from sending a request until its response headers
    /// arrive, for requests that send no large body (listings, HEAD,
    /// deletes, ...); uploads and server-side copies rely on stalled-stream
    /// protection instead. Default 60, range 1..=3600.
    pub read_timeout_secs: u64,
    /// Keys per listing request. Default 1000, range 1..=1000.
    pub list_page_size: u32,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        AdvancedSettings {
            max_attempts: 5,
            connect_timeout_secs: 10,
            read_timeout_secs: 60,
            list_page_size: 1000,
        }
    }
}

/// Checksum algorithm for uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgorithm {
    /// CRC-32 (the SDK default).
    #[default]
    Crc32,
    /// CRC-32C.
    Crc32c,
    /// CRC-64/NVME.
    Crc64nvme,
    /// SHA-1.
    Sha1,
    /// SHA-256.
    Sha256,
    /// Only send a checksum when the API requires one.
    None,
}

/// Data integrity settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataIntegritySettings {
    /// Checksum for uploads. Default `Crc32`.
    pub checksum_algorithm: ChecksumAlgorithm,
    /// Validate response checksums on downloads. Default `true`.
    pub validate_downloads: bool,
    /// Keep modification times: a downloaded file gets the time of the
    /// object (the `mtime` user metadata an upload stored, else
    /// `Last-Modified`), and an upload stores the local modification time in
    /// the `mtime` user metadata. Default `true`.
    pub preserve_mtime: bool,
}

impl Default for DataIntegritySettings {
    fn default() -> Self {
        DataIntegritySettings {
            checksum_algorithm: ChecksumAlgorithm::Crc32,
            validate_downloads: true,
            preserve_mtime: true,
        }
    }
}

/// What a transfer does when its destination already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfExists {
    /// Ask for every conflict (the desktop app shows a dialog; the command
    /// line asks on a terminal and otherwise refuses).
    Ask,
    /// Replace the destination. The default (the behaviour of version 0.1).
    #[default]
    Overwrite,
    /// Keep the destination and report the item as skipped.
    Skip,
    /// Replace the destination only when the source is newer.
    OverwriteIfNewer,
    /// Keep both: the new item gets a free name (`name (1).ext`).
    Rename,
}

impl IfExists {
    /// The name used in JSON and in group markers.
    pub fn as_str(self) -> &'static str {
        match self {
            IfExists::Ask => "ask",
            IfExists::Overwrite => "overwrite",
            IfExists::Skip => "skip",
            IfExists::OverwriteIfNewer => "overwrite_if_newer",
            IfExists::Rename => "rename",
        }
    }
}

impl std::str::FromStr for IfExists {
    type Err = Error;

    /// Accepts the JSON names plus the command-line spelling `newer`.
    fn from_str(text: &str) -> Result<Self> {
        match text {
            "ask" => Ok(IfExists::Ask),
            "overwrite" => Ok(IfExists::Overwrite),
            "skip" => Ok(IfExists::Skip),
            "newer" | "overwrite_if_newer" => Ok(IfExists::OverwriteIfNewer),
            "rename" => Ok(IfExists::Rename),
            other => Err(Error::Usage(format!(
                "unknown overwrite policy '{other}'; use ask, overwrite, skip, newer or rename"
            ))),
        }
    }
}

/// Event-log settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingSettings {
    /// Minimum level recorded. Default `Information`.
    pub level: LogLevel,
    /// Also write the log to files in the log folder. Default `true`.
    pub file_logging: bool,
    /// Size at which a log file is rotated. Default 10, range 1..=1024.
    pub max_file_mib: u32,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        LoggingSettings {
            level: LogLevel::Information,
            file_logging: true,
            max_file_mib: 10,
        }
    }
}

/// Proxy protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    /// HTTP proxy (CONNECT for HTTPS).
    #[default]
    Http,
    /// SOCKS5 proxy.
    Socks5,
}

/// Proxy settings. The password is kept in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    /// Default `false`.
    pub enabled: bool,
    /// Default `Http`.
    pub kind: ProxyKind,
    /// Proxy host name or address. Default `""`; required when enabled.
    pub host: String,
    /// Proxy port. Default 8080, range 1..=65535.
    pub port: u16,
    /// User name for proxy authentication. Default `None`.
    pub username: Option<String>,
    /// Secret-store key of the proxy password. Default `None`.
    pub password_ref: Option<String>,
    /// Hosts that bypass the proxy: names, `*.suffix`, or CIDR ranges.
    pub bypass: Vec<String>,
}

impl Default for ProxySettings {
    fn default() -> Self {
        ProxySettings {
            enabled: false,
            kind: ProxyKind::Http,
            host: String::new(),
            port: 8080,
            username: None,
            password_ref: None,
            bypass: Vec::new(),
        }
    }
}

/// The URL shortening services a share link can be sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortenerProvider {
    /// No shortener: share links are never sent to a third party. The default.
    #[default]
    None,
    /// Bitly (needs an access token).
    Bitly,
    /// TinyURL (an API token is optional).
    Tinyurl,
}

/// URL shortener settings. Off by default: nothing is sent anywhere until a
/// provider is chosen, and then only when the user presses Shorten.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortenerSettings {
    /// The service, or `none`. Default `none`.
    pub provider: ShortenerProvider,
    /// Secret-store key of the API token. Default `None`.
    pub token_ref: Option<String>,
}

/// Transfer queue settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferSettings {
    /// Tasks running at once. Default 4, range 1..=32.
    pub max_concurrent_tasks: u32,
    /// Parts in flight per multipart task. Default 4, range 1..=32.
    pub max_parts_per_task: u32,
    /// Objects of at least this size use multipart. Default 8, range 5..=5120.
    pub multipart_threshold_mib: u64,
    /// Part size, grown automatically so no upload needs more than 10,000
    /// parts. Default 8, range 5..=5120.
    pub part_size_mib: u64,
    /// Bandwidth cap shared by all tasks in KiB/s; `None` is unlimited.
    /// Range 1..=104857600 (100 GiB/s).
    pub bandwidth_limit_kibps: Option<u64>,
    /// How often the queue retries a failed part or object, on top of the
    /// SDK's per-request retries. Default 3, range 0..=20.
    pub retry_attempts: u32,
    /// What to do when a destination exists, unless an operation says
    /// otherwise. Default `overwrite`.
    pub if_exists: IfExists,
}

impl Default for TransferSettings {
    fn default() -> Self {
        TransferSettings {
            max_concurrent_tasks: 4,
            max_parts_per_task: 4,
            multipart_threshold_mib: 8,
            part_size_mib: 8,
            bandwidth_limit_kibps: None,
            retry_attempts: 3,
            if_exists: IfExists::Overwrite,
        }
    }
}

struct Checker {
    problems: Vec<String>,
}

impl Checker {
    fn range<T: PartialOrd + std::fmt::Display>(&mut self, name: &str, value: T, min: T, max: T) {
        if value < min || value > max {
            self.problems.push(format!(
                "{name} must be between {min} and {max} (got {value})"
            ));
        }
    }

    fn require(&mut self, ok: bool, problem: impl FnOnce() -> String) {
        if !ok {
            self.problems.push(problem());
        }
    }
}

impl Settings {
    /// Checks every value against its documented range. All problems are
    /// reported together in one [`Error::Usage`].
    pub fn validate(&self) -> Result<()> {
        let mut c = Checker {
            problems: Vec::new(),
        };

        let a = &self.advanced;
        c.range("advanced.max_attempts", a.max_attempts, 1, 20);
        c.range(
            "advanced.connect_timeout_secs",
            a.connect_timeout_secs,
            1,
            300,
        );
        c.range("advanced.read_timeout_secs", a.read_timeout_secs, 1, 3600);
        c.range("advanced.list_page_size", a.list_page_size, 1, 1000);

        c.range("logging.max_file_mib", self.logging.max_file_mib, 1, 1024);

        let p = &self.proxy;
        c.range("proxy.port", p.port, 1, u16::MAX);
        c.require(!p.enabled || !p.host.trim().is_empty(), || {
            "proxy.host must be set when the proxy is enabled".to_owned()
        });
        // The host is never echoed: it may be a password typed into the wrong field.
        c.require(
            !p.host.chars().any(char::is_whitespace) || p.host.trim().is_empty(),
            || "proxy.host must not contain whitespace".to_owned(),
        );
        c.require(!p.host.contains('@'), || {
            "proxy.host must not contain '@': enter the user name and password in their own fields"
                .to_owned()
        });
        c.require(p.username.as_deref().is_none_or(|u| !u.is_empty()), || {
            "proxy.username must be null or non-empty".to_owned()
        });
        for (i, entry) in p.bypass.iter().enumerate() {
            c.require(!entry.trim().is_empty(), || {
                format!("proxy.bypass[{i}] is empty")
            });
        }

        let t = &self.transfer;
        c.range(
            "transfer.max_concurrent_tasks",
            t.max_concurrent_tasks,
            1,
            32,
        );
        c.range("transfer.max_parts_per_task", t.max_parts_per_task, 1, 32);
        c.range(
            "transfer.multipart_threshold_mib",
            t.multipart_threshold_mib,
            MIN_PART_SIZE_MIB,
            MAX_PART_SIZE_MIB,
        );
        c.range(
            "transfer.part_size_mib",
            t.part_size_mib,
            MIN_PART_SIZE_MIB,
            MAX_PART_SIZE_MIB,
        );
        if let Some(limit) = t.bandwidth_limit_kibps {
            c.range(
                "transfer.bandwidth_limit_kibps",
                limit,
                1,
                100 * 1024 * 1024,
            );
        }
        c.range("transfer.retry_attempts", t.retry_attempts, 0, 20);

        if c.problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Usage(format!(
                "invalid settings: {}",
                c.problems.join("; ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_with_recent_is_most_recent_first_unique_and_bounded() {
        let mut general = GeneralSettings::default();
        assert!(general.open_with_recent.is_empty());
        general.remember_open_with("/a");
        general.remember_open_with("/b");
        general.remember_open_with("/a");
        assert_eq!(general.open_with_recent, ["/a", "/b"]);
        for n in 0..20 {
            general.remember_open_with(&format!("/app{n}"));
        }
        assert_eq!(general.open_with_recent.len(), MAX_OPEN_WITH_RECENT);
        assert_eq!(general.open_with_recent[0], "/app19");
        let old: Settings =
            serde_json::from_value(json!({ "general": { "confirm_delete": false } })).unwrap();
        assert!(old.general.open_with_recent.is_empty());
    }

    #[test]
    fn defaults_match_the_contract() {
        let value = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(
            value,
            json!({
                "general": { "confirm_delete": true, "default_download_dir": null, "open_with_recent": [] },
                "interface": {
                    "theme": "system",
                    "flat_browsing": false,
                    "show_transfer_panel": true,
                    "show_log_panel": false
                },
                "advanced": {
                    "max_attempts": 5,
                    "connect_timeout_secs": 10,
                    "read_timeout_secs": 60,
                    "list_page_size": 1000
                },
                "data_integrity": { "checksum_algorithm": "crc32", "validate_downloads": true, "preserve_mtime": true },
                "logging": { "level": "information", "file_logging": true, "max_file_mib": 10 },
                "proxy": {
                    "enabled": false,
                    "kind": "http",
                    "host": "",
                    "port": 8080,
                    "username": null,
                    "password_ref": null,
                    "bypass": []
                },
                "transfer": {
                    "max_concurrent_tasks": 4,
                    "max_parts_per_task": 4,
                    "multipart_threshold_mib": 8,
                    "part_size_mib": 8,
                    "bandwidth_limit_kibps": null,
                    "retry_attempts": 3,
                    "if_exists": "overwrite"
                },
                "shortener": { "provider": "none", "token_ref": null }
            })
        );
        Settings::default().validate().unwrap();
    }

    #[test]
    fn enum_spellings() {
        for (alg, text) in [
            (ChecksumAlgorithm::Crc32, "crc32"),
            (ChecksumAlgorithm::Crc32c, "crc32c"),
            (ChecksumAlgorithm::Crc64nvme, "crc64nvme"),
            (ChecksumAlgorithm::Sha1, "sha1"),
            (ChecksumAlgorithm::Sha256, "sha256"),
            (ChecksumAlgorithm::None, "none"),
        ] {
            assert_eq!(serde_json::to_value(alg).unwrap(), json!(text));
            assert_eq!(
                serde_json::from_value::<ChecksumAlgorithm>(json!(text)).unwrap(),
                alg
            );
        }
        assert_eq!(serde_json::to_value(Theme::Dark).unwrap(), json!("dark"));
        assert_eq!(serde_json::to_value(Theme::Light).unwrap(), json!("light"));
        assert_eq!(
            serde_json::to_value(ProxyKind::Socks5).unwrap(),
            json!("socks5")
        );
    }

    #[test]
    fn partial_input_is_filled_with_defaults() {
        let settings: Settings = serde_json::from_value(json!({
            "advanced": { "max_attempts": 2 },
            "transfer": { "bandwidth_limit_kibps": 512 },
            "unknown_section": { "x": 1 }
        }))
        .unwrap();
        assert_eq!(settings.advanced.max_attempts, 2);
        assert_eq!(settings.advanced.read_timeout_secs, 60);
        assert_eq!(settings.transfer.bandwidth_limit_kibps, Some(512));
        assert_eq!(settings.transfer.part_size_mib, 8);
        assert_eq!(settings.general, GeneralSettings::default());
        let empty: Settings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, Settings::default());
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let mut s = Settings::default();
        s.advanced.list_page_size = 0;
        s.advanced.max_attempts = 0;
        s.transfer.part_size_mib = 4;
        s.transfer.max_concurrent_tasks = 0;
        s.proxy.port = 0;
        let err = s.validate().unwrap_err();
        let text = err.to_string();
        assert_eq!(err.kind(), crate::error::ErrorKind::Usage);
        for expected in [
            "advanced.list_page_size must be between 1 and 1000 (got 0)",
            "advanced.max_attempts must be between 1 and 20 (got 0)",
            "transfer.part_size_mib must be between 5 and 5120 (got 4)",
            "transfer.max_concurrent_tasks must be between 1 and 32 (got 0)",
            "proxy.port must be between 1 and 65535 (got 0)",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text}");
        }
    }

    #[test]
    fn boundaries_are_inclusive() {
        let mut s = Settings::default();
        s.advanced.list_page_size = 1;
        s.advanced.max_attempts = 20;
        s.advanced.connect_timeout_secs = 300;
        s.advanced.read_timeout_secs = 3600;
        s.transfer.part_size_mib = 5;
        s.transfer.multipart_threshold_mib = 5120;
        s.transfer.retry_attempts = 0;
        s.transfer.bandwidth_limit_kibps = Some(1);
        s.proxy.port = 65535;
        s.validate().unwrap();

        s.advanced.list_page_size = 1001;
        assert!(s.validate().is_err());
        s.advanced.list_page_size = 1000;
        s.transfer.part_size_mib = 5121;
        assert!(s.validate().is_err());
        s.transfer.part_size_mib = 8;
        s.transfer.bandwidth_limit_kibps = Some(0);
        assert!(s.validate().is_err());
    }

    #[test]
    fn proxy_rules() {
        let mut s = Settings::default();
        s.proxy.enabled = true;
        assert!(
            s.validate()
                .unwrap_err()
                .to_string()
                .contains("proxy.host must be set")
        );
        s.proxy.host = "proxy.example.com".into();
        s.validate().unwrap();
        s.proxy.host = "bad host".into();
        assert!(s.validate().is_err());
        // A password typed into the host field is rejected without being echoed.
        for host in [
            "alice:hunter2@proxy.example.com",
            "hunter2@proxy.example.com",
            "p@ss",
            "a b hunter2",
        ] {
            s.proxy.host = host.into();
            let message = s.validate().unwrap_err().to_string();
            assert!(!message.contains("hunter2"), "{message}");
            assert!(!message.contains("p@ss"), "{message}");
            assert!(message.contains("proxy.host"), "{message}");
        }
        s.proxy.host = "proxy.example.com".into();
        s.proxy.username = Some(String::new());
        assert!(s.validate().is_err());
        s.proxy.username = Some("user".into());
        s.proxy.bypass = vec!["*.example.com".into(), " ".into()];
        assert!(
            s.validate()
                .unwrap_err()
                .to_string()
                .contains("proxy.bypass[1] is empty")
        );
    }

    #[test]
    fn port_out_of_u16_range_fails_to_parse() {
        let err =
            serde_json::from_value::<Settings>(json!({ "proxy": { "port": 70000 } })).unwrap_err();
        assert!(err.to_string().contains("70000"), "{err}");
    }
}
