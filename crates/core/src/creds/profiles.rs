//! Profiles of the AWS shared config and credentials files.
//!
//! BucketBridge reads `~/.aws/config` and `~/.aws/credentials` (or the
//! files named by `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE`) to
//! list profiles and to sign requests for an
//! [`AwsProfile`](crate::model::CredentialSource::AwsProfile) account.
//! Importing a profile creates an account that only holds the profile
//! name: no key is copied into BucketBridge, and a change to the files (a
//! rotated key, a new role) is picked up the next time the account's client
//! is built.
//!
//! Signing uses the official `aws-config` profile provider, so everything
//! the AWS CLI does for a profile works: static keys, `role_arn` with
//! `source_profile` or `credential_source`, `credential_process`, web
//! identity and IAM Identity Center profiles (`sso_session` or the legacy
//! `sso_start_url`).

use std::borrow::Cow;

use aws_config::profile::{Profile, ProfileFileCredentialsProvider, ProfileSet};
use aws_config::provider_config::ProviderConfig;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_runtime::env_config::file::{
    EnvConfigFileKind as ProfileFileKind, EnvConfigFiles as ProfileFiles,
};
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use aws_types::os_shim_internal::{Env, Fs};
use serde::{Deserialize, Serialize};

use super::AwsFiles;
use crate::error::{Error, Result};

/// How a profile gets its credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsProfileKind {
    /// `aws_access_key_id` and `aws_secret_access_key` in the file.
    StaticKeys,
    /// `role_arn` with a `source_profile` or `credential_source`.
    AssumeRole,
    /// IAM Identity Center (`sso_session` or `sso_start_url`).
    Sso,
    /// `credential_process`.
    CredentialProcess,
    /// `web_identity_token_file`.
    WebIdentity,
    /// Nothing BucketBridge can sign with.
    Incomplete,
}

/// One profile of the AWS files, without any secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwsProfile {
    /// Profile name.
    pub name: String,
    /// How the profile gets its credentials.
    pub kind: AwsProfileKind,
    /// The profile's `region`, if it has one.
    pub region: Option<String>,
    /// `role_arn` of a role profile.
    pub role_arn: Option<String>,
    /// `source_profile` of a role profile.
    pub source_profile: Option<String>,
    /// IAM Identity Center start URL (from the profile or its `sso-session`).
    pub sso_start_url: Option<String>,
    /// IAM Identity Center account id.
    pub sso_account_id: Option<String>,
    /// IAM Identity Center role name.
    pub sso_role_name: Option<String>,
    /// Whether BucketBridge can use the profile.
    pub importable: bool,
    /// Why a profile is not importable.
    pub note: Option<String>,
}

fn profile_files(files: &AwsFiles) -> ProfileFiles {
    // A file that does not exist is not an error for the AWS CLI; the SDK
    // would fail on it, so only existing files are named.
    let mut builder = ProfileFiles::builder();
    let mut any = false;
    for (kind, path) in [
        (ProfileFileKind::Config, files.config()),
        (ProfileFileKind::Credentials, files.credentials()),
    ] {
        if path.is_file() {
            builder = builder.with_file(kind, path);
            any = true;
        }
    }
    if !any {
        builder = builder.with_contents(ProfileFileKind::Config, "");
    }
    builder.build()
}

async fn load(files: &AwsFiles) -> Result<ProfileSet> {
    // An empty environment: only the explicit files are read.
    aws_config::profile::load(
        &Fs::real(),
        &Env::from_slice(&[]),
        &profile_files(files),
        None::<Cow<'static, str>>,
    )
    .await
    .map_err(|error| Error::Usage(format!("cannot read the AWS files: {error}")))
}

/// Lists the profiles of the AWS files, sorted by name. Missing files give
/// an empty list; an unreadable or malformed file is a usage error.
pub async fn list_profiles(files: &AwsFiles) -> Result<Vec<AwsProfile>> {
    let set = load(files).await?;
    let mut names: Vec<&str> = set.profiles().collect();
    names.sort_unstable();
    Ok(names
        .into_iter()
        .filter_map(|name| set.get_profile(name).map(|profile| describe(&set, profile)))
        .collect())
}

/// Describes one profile, or `None` when it does not exist.
pub async fn find_profile(files: &AwsFiles, name: &str) -> Result<Option<AwsProfile>> {
    let set = load(files).await?;
    Ok(set.get_profile(name).map(|profile| describe(&set, profile)))
}

fn describe(set: &ProfileSet, profile: &Profile) -> AwsProfile {
    let get = |key: &str| profile.get(key).map(str::to_owned);
    let session = profile
        .get("sso_session")
        .and_then(|name| set.sso_session(name));
    let sso_start_url = get("sso_start_url").or_else(|| {
        session
            .and_then(|session| session.get("sso_start_url"))
            .map(str::to_owned)
    });
    let is_sso = profile.get("sso_session").is_some() || profile.get("sso_start_url").is_some();
    let kind = if is_sso {
        AwsProfileKind::Sso
    } else if profile.get("web_identity_token_file").is_some() {
        AwsProfileKind::WebIdentity
    } else if profile.get("role_arn").is_some() {
        AwsProfileKind::AssumeRole
    } else if profile.get("credential_process").is_some() {
        AwsProfileKind::CredentialProcess
    } else if profile.get("aws_access_key_id").is_some()
        && profile.get("aws_secret_access_key").is_some()
    {
        AwsProfileKind::StaticKeys
    } else {
        AwsProfileKind::Incomplete
    };
    let note = match kind {
        AwsProfileKind::Incomplete => Some(
            "the profile has no credentials, role, SSO or credential_process setting".to_owned(),
        ),
        AwsProfileKind::AssumeRole
            if profile.get("source_profile").is_none()
                && profile.get("credential_source").is_none() =>
        {
            Some("the role profile has no source_profile or credential_source".to_owned())
        }
        _ => None,
    };
    AwsProfile {
        name: profile.name().to_owned(),
        kind,
        region: get("region"),
        role_arn: get("role_arn"),
        source_profile: get("source_profile"),
        sso_start_url,
        sso_account_id: get("sso_account_id"),
        sso_role_name: get("sso_role_name"),
        importable: note.is_none(),
        note,
    }
}

/// A credentials provider that reads `profile` from the AWS files.
///
/// `region` is the region of the STS and SSO calls a role or Identity
/// Center profile needs (the profile's own `region` wins), `http_client`
/// the proxy-aware client of the application.
pub fn profile_provider(
    profile: &str,
    files: &AwsFiles,
    http_client: SharedHttpClient,
    region: &str,
) -> SharedCredentialsProvider {
    let config = ProviderConfig::without_region()
        .with_http_client(http_client)
        .with_region(Some(aws_sdk_sts::config::Region::new(region.to_owned())));
    SharedCredentialsProvider::new(
        ProfileFileCredentialsProvider::builder()
            .configure(&config)
            .profile_name(profile)
            .profile_files(profile_files(files))
            .build(),
    )
}

/// A profile for the import picker: the profile and the account that
/// already links it, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwsProfileEntry {
    /// The profile.
    #[serde(flatten)]
    pub profile: AwsProfile,
    /// Id of the account that is linked to this profile already.
    pub account_id: Option<String>,
}

/// A profile that was not imported, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedProfile {
    /// Profile name.
    pub profile: String,
    /// Why nothing was created for it.
    pub reason: String,
}

/// Result of importing profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// The accounts that were created, one per imported profile.
    pub imported: Vec<crate::model::Account>,
    /// The profiles that were left out.
    pub skipped: Vec<SkippedProfile>,
}

/// A label for an imported profile that no account uses yet: the profile
/// name, then `name (AWS)`, `name (AWS 2)`, ...
pub fn free_label(taken: &[String], profile: &str) -> String {
    let used = |label: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(label));
    if !used(profile) {
        return profile.to_owned();
    }
    let mut candidate = format!("{profile} (AWS)");
    let mut n = 2;
    while used(&candidate) {
        candidate = format!("{profile} (AWS {n})");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use aws_credential_types::provider::ProvideCredentials;

    use super::*;

    const CONFIG: &str = "\
[default]
region = eu-west-1

[profile dev]
region = us-west-2
aws_access_key_id = AKIDEV
aws_secret_access_key = dev-secret-value

[profile role]
role_arn = arn:aws:iam::123456789012:role/Admin
source_profile = dev

[profile orphan-role]
role_arn = arn:aws:iam::123456789012:role/Admin

[profile sso-new]
sso_session = corp
sso_account_id = 123456789012
sso_role_name = Dev
region = eu-central-1

[profile sso-old]
sso_start_url = https://old.example.com/start
sso_region = us-east-1
sso_account_id = 210987654321
sso_role_name = Ops

[profile proc]
credential_process = echo nothing

[profile web]
web_identity_token_file = /tmp/token
role_arn = arn:aws:iam::123456789012:role/Web

[sso-session corp]
sso_start_url = https://corp.example.com/start
sso_region = eu-central-1
";

    const CREDENTIALS: &str = "\
[default]
aws_access_key_id = AKIADEFAULT
aws_secret_access_key = default-secret-value

[ci]
aws_access_key_id = AKIACI
aws_secret_access_key = ci-secret-value
aws_session_token = ci-token-value
";

    fn files(dir: &tempfile::TempDir) -> AwsFiles {
        std::fs::write(dir.path().join("config"), CONFIG).unwrap();
        std::fs::write(dir.path().join("credentials"), CREDENTIALS).unwrap();
        AwsFiles::in_dir(dir.path())
    }

    fn http() -> SharedHttpClient {
        aws_smithy_runtime_api::client::http::SharedHttpClient::new(
            aws_smithy_http_client::test_util::NeverClient::new(),
        )
    }

    #[tokio::test]
    async fn lists_every_kind_of_profile_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = list_profiles(&files(&dir)).await.unwrap();
        let by_name = |name: &str| profiles.iter().find(|p| p.name == name).unwrap();
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "ci",
                "default",
                "dev",
                "orphan-role",
                "proc",
                "role",
                "sso-new",
                "sso-old",
                "web"
            ]
        );
        assert_eq!(by_name("ci").kind, AwsProfileKind::StaticKeys);
        assert_eq!(by_name("default").kind, AwsProfileKind::StaticKeys);
        assert_eq!(by_name("default").region.as_deref(), Some("eu-west-1"));
        assert_eq!(by_name("dev").region.as_deref(), Some("us-west-2"));
        assert_eq!(by_name("role").kind, AwsProfileKind::AssumeRole);
        assert_eq!(by_name("role").source_profile.as_deref(), Some("dev"));
        assert!(by_name("role").importable);
        assert!(!by_name("orphan-role").importable);
        assert!(by_name("orphan-role").note.is_some());
        let sso = by_name("sso-new");
        assert_eq!(sso.kind, AwsProfileKind::Sso);
        assert_eq!(
            sso.sso_start_url.as_deref(),
            Some("https://corp.example.com/start")
        );
        assert_eq!(by_name("sso-old").kind, AwsProfileKind::Sso);
        assert_eq!(by_name("proc").kind, AwsProfileKind::CredentialProcess);
        assert_eq!(by_name("web").kind, AwsProfileKind::WebIdentity);
        let json = serde_json::to_string(&profiles).unwrap();
        for secret in ["dev-secret-value", "default-secret-value", "ci-token-value"] {
            assert!(!json.contains(secret), "{secret} leaked");
        }
    }

    #[tokio::test]
    async fn missing_files_list_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = list_profiles(&AwsFiles::in_dir(dir.path())).await.unwrap();
        assert!(profiles.is_empty());
        assert!(
            find_profile(&AwsFiles::in_dir(dir.path()), "x")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_credentials_file_wins_for_keys_of_a_shared_name() {
        let dir = tempfile::tempdir().unwrap();
        let provider = profile_provider("default", &files(&dir), http(), "eu-west-1");
        let credentials = provider.provide_credentials().await.unwrap();
        assert_eq!(credentials.access_key_id(), "AKIADEFAULT");
        assert!(!format!("{credentials:?}").contains("default-secret-value"));
    }

    #[tokio::test]
    async fn a_profile_with_a_session_token_signs_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let provider = profile_provider("ci", &files(&dir), http(), "us-east-1");
        let credentials = provider.provide_credentials().await.unwrap();
        assert_eq!(credentials.access_key_id(), "AKIACI");
        assert_eq!(credentials.session_token(), Some("ci-token-value"));
    }

    #[tokio::test]
    async fn a_missing_profile_gives_no_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(&dir);
        let provider = profile_provider("nope", &files, http(), "us-east-1");
        assert!(provider.provide_credentials().await.is_err());
        assert!(find_profile(&files, "nope").await.unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_credential_process_profile_runs_its_command() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("creds.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"Version\":1,\"AccessKeyId\":\"AKIAPROC\",\"SecretAccessKey\":\"proc-secret\"}'\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            dir.path().join("config"),
            format!(
                "[profile proc]\ncredential_process = {}\n",
                script.display()
            ),
        )
        .unwrap();
        let provider = profile_provider("proc", &AwsFiles::in_dir(dir.path()), http(), "us-east-1");
        let credentials = provider.provide_credentials().await.unwrap();
        assert_eq!(credentials.access_key_id(), "AKIAPROC");
    }

    #[tokio::test]
    async fn a_malformed_file_is_reported_without_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("credentials"),
            "[default\naws_secret_access_key = hunter2-secret\n",
        )
        .unwrap();
        let error = list_profiles(&AwsFiles::in_dir(dir.path()))
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("hunter2-secret"), "{error}");
    }
}
