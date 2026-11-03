//! The IAM Identity Center token cache, in the AWS CLI's format.
//!
//! The AWS CLI and SDKs keep the sign-in token of an Identity Center portal
//! in `~/.aws/sso/cache/<sha1>.json`, where `<sha1>` is the hex SHA-1 of
//!
//! * the `sso-session` name, when the profile uses an `sso-session` section,
//! * the start URL otherwise (the legacy profile format).
//!
//! BucketBridge reads and writes the same files, so a sign-in made with
//! `aws sso login` works in BucketBridge and the other way round. This is
//! the only place a token is stored: the file is created with mode 0600
//! (directory 0700) on Unix, replaced atomically, and never copied into
//! `config.toml`, the keychain or a log. Fields BucketBridge does not know
//! are kept when the file is rewritten.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

use crate::error::{Error, Result};
use crate::secrets::SecretString;

/// Names the cache file of a portal: the `sso-session` name when there is
/// one, the start URL otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Start URL of the portal.
    pub start_url: String,
    /// `sso-session` name, for profiles that use one.
    pub session_name: Option<String>,
}

impl CacheKey {
    /// A key for a portal.
    pub fn new(start_url: &str, session_name: Option<&str>) -> Self {
        CacheKey {
            start_url: start_url.to_owned(),
            session_name: session_name
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        }
    }

    /// The file name (`<sha1 hex>.json`).
    pub fn file_name(&self) -> String {
        let subject = self.session_name.as_deref().unwrap_or(&self.start_url);
        let digest = Sha1::digest(subject.as_bytes());
        format!("{}.json", hex::encode(digest))
    }

    /// The cache file under `dir`.
    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }
}

/// A cached sign-in: the access token and what is needed to refresh it.
#[derive(Clone)]
pub struct SsoToken {
    /// Bearer token of the portal APIs.
    pub access_token: SecretString,
    /// When the access token stops working.
    pub expires_at: DateTime<Utc>,
    /// Refresh token, when the sign-in issued one.
    pub refresh_token: Option<SecretString>,
    /// OIDC client registration used for the sign-in.
    pub client_id: Option<String>,
    /// Secret of the OIDC client registration.
    pub client_secret: Option<SecretString>,
    /// When the client registration expires.
    pub registration_expires_at: Option<DateTime<Utc>>,
    /// Region of the portal.
    pub region: Option<String>,
    /// Start URL of the portal.
    pub start_url: Option<String>,
}

impl std::fmt::Debug for SsoToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("registration_expires_at", &self.registration_expires_at)
            .field("region", &self.region)
            .field("start_url", &self.start_url)
            .finish()
    }
}

/// The JSON of a cache file (camelCase, as the AWS CLI writes it).
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TokenFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registration_expires_at: Option<String>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// Parses the timestamps of the cache: RFC 3339, or the `...UTC` form the
/// older AWS CLI wrote.
fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    let normalized = text
        .strip_suffix("UTC")
        .map_or_else(|| text.to_owned(), |rest| format!("{rest}Z"));
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn format_time(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl TokenFile {
    fn into_token(self) -> Option<SsoToken> {
        Some(SsoToken {
            access_token: SecretString::from(self.access_token?),
            expires_at: parse_time(&self.expires_at?)?,
            refresh_token: self.refresh_token.map(SecretString::from),
            client_id: self.client_id,
            client_secret: self.client_secret.map(SecretString::from),
            registration_expires_at: self.registration_expires_at.as_deref().and_then(parse_time),
            region: self.region,
            start_url: self.start_url,
        })
    }

    fn apply(&mut self, token: &SsoToken) {
        self.access_token = Some(token.access_token.expose().to_owned());
        self.expires_at = Some(format_time(token.expires_at));
        self.refresh_token = token
            .refresh_token
            .as_ref()
            .map(|value| value.expose().to_owned());
        self.client_id = token.client_id.clone();
        self.client_secret = token
            .client_secret
            .as_ref()
            .map(|value| value.expose().to_owned());
        self.registration_expires_at = token.registration_expires_at.map(format_time);
        self.region = token.region.clone();
        self.start_url = token.start_url.clone();
    }
}

/// Reads the cached token of a portal. `Ok(None)` when there is no usable
/// file (missing, not JSON, or without an access token and expiry): the
/// caller then asks the user to sign in.
pub fn read_token(dir: &Path, key: &CacheKey) -> Result<Option<SsoToken>> {
    let path = key.path(dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::io(
                format!("reading the SSO token cache {}", path.display()),
                error,
            ));
        }
    };
    Ok(serde_json::from_slice::<TokenFile>(&bytes)
        .ok()
        .and_then(TokenFile::into_token))
}

/// Writes the token of a portal, keeping unknown fields of an existing
/// file. The file is created with mode 0600 and replaced atomically.
pub fn write_token(dir: &Path, key: &CacheKey, token: &SsoToken) -> Result<()> {
    let path = key.path(dir);
    let mut file = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TokenFile>(&bytes).ok())
        .unwrap_or_default();
    file.apply(token);
    let mut json = serde_json::to_vec_pretty(&file)
        .map_err(|error| Error::Internal(format!("encoding the SSO token cache: {error}")))?;
    json.push(b'\n');
    create_private_dir(dir)?;
    write_private_file(&path, &json)
}

/// Deletes the cached token of a portal (sign out). A missing file is fine.
pub fn remove_token(dir: &Path, key: &CacheKey) -> Result<()> {
    let path = key.path(dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io(
            format!("removing the SSO token cache {}", path.display()),
            error,
        )),
    }
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let context = || format!("creating {}", dir.display());
    fs::create_dir_all(dir).map_err(|error| Error::io(context(), error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Only tighten a directory BucketBridge may have just created: the
        // AWS CLI creates it 0700 too, and a user-chosen mode is respected
        // unless it lets others in.
        if let Ok(meta) = fs::metadata(dir) {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                    .map_err(|error| Error::io(context(), error))?;
            }
        }
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let context = || format!("writing the SSO token cache {}", path.display());
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A temporary name next to the target, so the rename stays on one
    // filesystem; never a guessable name an attacker could pre-create (the
    // file is opened with create_new).
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| Error::Internal(error.to_string()))?;
    let temp = dir.join(format!(".bb-{}.tmp", hex::encode(suffix)));
    let mut file = options
        .open(&temp)
        .map_err(|error| Error::io(context(), error))?;
    let written = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| Error::io(context(), error));
    drop(file);
    if let Err(error) = written {
        let _cleanup = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _cleanup = fs::remove_file(&temp);
        return Err(Error::io(context(), error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(access: &str) -> SsoToken {
        SsoToken {
            access_token: SecretString::from(access),
            expires_at: DateTime::parse_from_rfc3339("2030-01-02T03:04:05Z")
                .unwrap()
                .with_timezone(&Utc),
            refresh_token: Some(SecretString::from("refresh-value")),
            client_id: Some("client-id".into()),
            client_secret: Some(SecretString::from("client-secret-value")),
            registration_expires_at: Some(
                DateTime::parse_from_rfc3339("2030-04-05T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            region: Some("eu-west-1".into()),
            start_url: Some("https://example.awsapps.com/start".into()),
        }
    }

    #[test]
    fn file_names_follow_the_aws_cli_hashing_rules() {
        // `printf 'https://d-1234567890.awsapps.com/start' | sha1sum` and
        // `printf 'my-sso' | sha1sum`, the names the AWS CLI uses.
        let legacy = CacheKey::new("https://d-1234567890.awsapps.com/start", None);
        assert_eq!(
            legacy.file_name(),
            format!(
                "{}.json",
                hex::encode(Sha1::digest(b"https://d-1234567890.awsapps.com/start"))
            )
        );
        let session = CacheKey::new("https://d-1234567890.awsapps.com/start", Some("my-sso"));
        assert_eq!(
            session.file_name(),
            "0ad374308c5a4e22f723adf10145eafad7c4031c.json"
        );
        assert_ne!(legacy.file_name(), session.file_name());
        assert_eq!(
            CacheKey::new("https://x/start", Some("")).file_name(),
            CacheKey::new("https://x/start", None).file_name()
        );
    }

    #[test]
    fn a_token_round_trips_in_the_cli_format() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("sso").join("cache");
        let key = CacheKey::new("https://example.awsapps.com/start", Some("corp"));
        assert!(read_token(&cache, &key).unwrap().is_none());
        write_token(&cache, &key, &token("access-value")).unwrap();
        let text = fs::read_to_string(key.path(&cache)).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["accessToken"], "access-value");
        assert_eq!(json["expiresAt"], "2030-01-02T03:04:05Z");
        assert_eq!(json["refreshToken"], "refresh-value");
        assert_eq!(json["clientId"], "client-id");
        assert_eq!(json["clientSecret"], "client-secret-value");
        assert_eq!(json["registrationExpiresAt"], "2030-04-05T00:00:00Z");
        assert_eq!(json["region"], "eu-west-1");
        assert_eq!(json["startUrl"], "https://example.awsapps.com/start");
        let back = read_token(&cache, &key).unwrap().unwrap();
        assert_eq!(back.access_token.expose(), "access-value");
        assert_eq!(back.expires_at, token("x").expires_at);
        assert_eq!(back.refresh_token.unwrap().expose(), "refresh-value");
    }

    #[cfg(unix)]
    #[test]
    fn the_cache_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let key = CacheKey::new("https://example.awsapps.com/start", None);
        write_token(&cache, &key, &token("a")).unwrap();
        let file_mode = fs::metadata(key.path(&cache)).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600);
        let dir_mode = fs::metadata(&cache).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o077, 0);
        // A rewrite stays private and leaves no temporary file behind.
        write_token(&cache, &key, &token("b")).unwrap();
        let mode = fs::metadata(key.path(&cache)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new("https://example.awsapps.com/start", None);
        fs::write(
            key.path(dir.path()),
            r#"{"accessToken":"old","expiresAt":"2030-01-01T00:00:00Z","somethingNew":{"a":1}}"#,
        )
        .unwrap();
        write_token(dir.path(), &key, &token("new")).unwrap();
        let json: Value = serde_json::from_slice(&fs::read(key.path(dir.path())).unwrap()).unwrap();
        assert_eq!(json["somethingNew"]["a"], 1);
        assert_eq!(json["accessToken"], "new");
    }

    #[test]
    fn the_older_utc_timestamp_form_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new("https://example.awsapps.com/start", None);
        fs::write(
            key.path(dir.path()),
            r#"{"accessToken":"t","expiresAt":"2030-01-01T00:00:00UTC"}"#,
        )
        .unwrap();
        let token = read_token(dir.path(), &key).unwrap().unwrap();
        assert_eq!(token.expires_at.timestamp(), 1_893_456_000);
        assert!(token.refresh_token.is_none());
    }

    #[test]
    fn unusable_files_read_as_signed_out() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new("https://example.awsapps.com/start", None);
        for content in [
            "not json",
            "{}",
            r#"{"accessToken":"t"}"#,
            r#"{"accessToken":"t","expiresAt":"soon"}"#,
        ] {
            fs::write(key.path(dir.path()), content).unwrap();
            assert!(read_token(dir.path(), &key).unwrap().is_none(), "{content}");
        }
    }

    #[test]
    fn debug_output_has_no_secrets() {
        let text = format!("{:?}", token("access-value"));
        for secret in ["access-value", "refresh-value", "client-secret-value"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    #[test]
    fn sign_out_removes_the_file_and_tolerates_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new("https://example.awsapps.com/start", None);
        write_token(dir.path(), &key, &token("a")).unwrap();
        remove_token(dir.path(), &key).unwrap();
        assert!(!key.path(dir.path()).exists());
        remove_token(dir.path(), &key).unwrap();
    }
}
