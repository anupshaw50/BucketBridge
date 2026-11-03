//! Credential sources other than static keys.
//!
//! An [`Account`](crate::model::Account) whose source is not
//! [`CredentialSource::StaticKeys`](crate::model::CredentialSource) has no
//! secret of its own in BucketBridge. Its SDK clients are built on a
//! refreshing [`SharedCredentialsProvider`]: the SDK asks the provider for
//! credentials, keeps them until shortly before they expire and then asks
//! again, so a long transfer or a long-open window never uses expired keys.
//!
//! | Source | Provider | Where the secrets live |
//! |---|---|---|
//! | `aws_profile` | [`profiles::profile_provider`] (the official `aws-config` profile provider) | the AWS files, read at use |
//! | `assume_role` | [`assume::AssumeRoleCredentials`] | memory only |
//! | `sso` | [`sso::SsoCredentials`] | the AWS CLI token cache, memory |
//! | `instance_role` | [`instance::instance_provider`] (IMDSv2 or container credentials) | memory only |
//!
//! Nothing here writes a secret to `config.toml`, and no type here prints
//! one: the SDK's `Credentials` redact themselves in `Debug`, and the SSO
//! token types do the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A client of an SDK crate answered by mock rules (the `mock_client!` of
/// `aws-smithy-mocks` needs the SDK's `test-util` feature, which this
/// workspace does not turn on).
#[cfg(test)]
macro_rules! mock_client {
    ($sdk:ident, $mode:expr, [$($rule:expr),* $(,)?]) => {{
        #[allow(unused_mut)]
        let mut interceptor = aws_smithy_mocks::MockResponseInterceptor::new().rule_mode($mode);
        $( interceptor = interceptor.with_rule($rule); )*
        let config = $sdk::Config::builder()
            .behavior_version($sdk::config::BehaviorVersion::latest())
            .region($sdk::config::Region::new("us-east-1"))
            .credentials_provider($sdk::config::Credentials::new("AKIAMOCK", "mock", None, None, "test"))
            .interceptor(interceptor)
            .http_client(aws_smithy_mocks::create_mock_http_client())
            .build();
        $sdk::Client::from_conf(config)
    }};
}

pub mod assume;
pub mod instance;
pub mod profiles;
pub mod sso;

/// Customizes the SSO OIDC client configuration, for example to inject a
/// test client.
pub type SsoOidcConfigHook =
    Arc<dyn Fn(aws_sdk_ssooidc::config::Builder) -> aws_sdk_ssooidc::config::Builder + Send + Sync>;

/// Customizes the SSO portal client configuration, for example to inject a
/// test client.
pub type SsoConfigHook =
    Arc<dyn Fn(aws_sdk_sso::config::Builder) -> aws_sdk_sso::config::Builder + Send + Sync>;

/// Where the AWS shared files live.
///
/// [`AwsFiles::from_env`] follows the AWS CLI: `AWS_CONFIG_FILE` and
/// `AWS_SHARED_CREDENTIALS_FILE` name the config and credentials files
/// (default `~/.aws/config` and `~/.aws/credentials`); the SSO token cache
/// is always `~/.aws/sso/cache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsFiles {
    config: PathBuf,
    credentials: PathBuf,
    sso_cache: PathBuf,
}

impl AwsFiles {
    /// The files named by the environment, or the defaults under the home
    /// directory.
    pub fn from_env() -> Self {
        Self::with_lookup(|name| std::env::var(name).ok(), home_dir())
    }

    /// Like [`AwsFiles::from_env`] with an explicit variable lookup and home
    /// directory (tests).
    pub fn with_lookup(get: impl Fn(&str) -> Option<String>, home: Option<PathBuf>) -> Self {
        let aws_dir = home.unwrap_or_default().join(".aws");
        let pick = |name: &str, default: &str| -> PathBuf {
            match get(name).filter(|value| !value.trim().is_empty()) {
                Some(value) => expand_home(&value, &aws_dir),
                None => aws_dir.join(default),
            }
        };
        AwsFiles {
            config: pick("AWS_CONFIG_FILE", "config"),
            credentials: pick("AWS_SHARED_CREDENTIALS_FILE", "credentials"),
            sso_cache: aws_dir.join("sso").join("cache"),
        }
    }

    /// The three locations under one directory (`config`, `credentials`,
    /// `sso/cache`), for tests and sandboxes.
    pub fn in_dir(dir: &Path) -> Self {
        AwsFiles {
            config: dir.join("config"),
            credentials: dir.join("credentials"),
            sso_cache: dir.join("sso").join("cache"),
        }
    }

    /// The AWS config file.
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// The AWS credentials file.
    pub fn credentials(&self) -> &Path {
        &self.credentials
    }

    /// The directory of the SSO token cache.
    pub fn sso_cache(&self) -> &Path {
        &self.sso_cache
    }
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Expands a leading `~/` the way the AWS CLI does (`aws_dir` is
/// `<home>/.aws`).
fn expand_home(value: &str, aws_dir: &Path) -> PathBuf {
    match (value.strip_prefix("~/"), aws_dir.parent()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(value),
    }
}

/// An error and its causes in one line (`outer: inner: ...`).
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut next = error.source();
    while let Some(cause) = next {
        let text = cause.to_string();
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        next = cause.source();
    }
    parts.join(": ")
}

/// Test and embedding hooks of the credential sources.
#[derive(Clone, Default)]
pub struct CredsHooks {
    /// Where the AWS files are; default [`AwsFiles::from_env`].
    pub aws_files: Option<AwsFiles>,
    /// Applied to every SSO OIDC client.
    pub sso_oidc: Option<SsoOidcConfigHook>,
    /// Applied to every SSO portal client.
    pub sso: Option<SsoConfigHook>,
    /// Instance metadata endpoint (`http://host:port`) instead of
    /// `169.254.169.254`.
    pub imds_endpoint: Option<String>,
}

impl CredsHooks {
    /// The AWS file locations to use.
    pub fn files(&self) -> AwsFiles {
        self.aws_files.clone().unwrap_or_else(AwsFiles::from_env)
    }
}

impl std::fmt::Debug for CredsHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredsHooks")
            .field("aws_files", &self.aws_files)
            .field("sso_oidc", &self.sso_oidc.is_some())
            .field("sso", &self.sso.is_some())
            .field("imds_endpoint", &self.imds_endpoint)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_follow_the_environment_variables() {
        let home = PathBuf::from("/home/u");
        let defaults = AwsFiles::with_lookup(|_| None, Some(home.clone()));
        assert_eq!(defaults.config(), Path::new("/home/u/.aws/config"));
        assert_eq!(
            defaults.credentials(),
            Path::new("/home/u/.aws/credentials")
        );
        assert_eq!(defaults.sso_cache(), Path::new("/home/u/.aws/sso/cache"));

        let custom = AwsFiles::with_lookup(
            |name| match name {
                "AWS_CONFIG_FILE" => Some("/etc/aws/cfg".into()),
                "AWS_SHARED_CREDENTIALS_FILE" => Some("~/creds".into()),
                _ => None,
            },
            Some(home),
        );
        assert_eq!(custom.config(), Path::new("/etc/aws/cfg"));
        assert_eq!(custom.credentials(), Path::new("/home/u/creds"));
        assert_eq!(custom.sso_cache(), Path::new("/home/u/.aws/sso/cache"));
    }

    #[test]
    fn empty_variables_are_ignored() {
        let files = AwsFiles::with_lookup(|_| Some("  ".into()), Some(PathBuf::from("/h")));
        assert_eq!(files.config(), Path::new("/h/.aws/config"));
    }
}
