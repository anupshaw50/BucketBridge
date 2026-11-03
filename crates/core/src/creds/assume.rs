//! Assumed roles: temporary credentials from STS `AssumeRole`.
//!
//! An [`AssumeRole`](crate::model::CredentialSource::AssumeRole) account
//! signs the `AssumeRole` call with the credentials of its source account
//! (any kind of account, including another assumed role) and then signs S3
//! requests with the role session it receives.
//!
//! * **Without MFA** [`AssumeRoleCredentials`] is a refreshing provider: it
//!   keeps the session in memory and assumes the role again once less than
//!   [`REFRESH_MARGIN`] of it is left, so requests never go out with
//!   credentials that are about to expire.
//! * **With an MFA device** the role's trust policy needs a fresh code, so
//!   nothing can refresh on its own. The session is created by
//!   [`Session::mfa_login`](crate::Session::mfa_login) with the code,
//!   kept like any other MFA session ([`crate::mfa::MfaSessions`]), and the
//!   account reports `mfa_required` again when it runs out.
//!
//! Session names, durations and external ids are validated when the account
//! is saved (see [`crate::accounts`]).

use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials as SdkCredentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{self, ProvideCredentials, future};
use aws_smithy_async::time::SharedTimeSource;
use tokio::sync::Mutex;

use crate::client::Credentials;
use crate::error::{Error, Result};
use crate::mfa::{MfaSession, check_code, validate_mfa_serial};
use crate::model::{Account, CredentialSource};

/// A session is replaced this long before it expires.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

const PROVIDER_NAME: &str = "BucketBridgeAssumeRole";

/// The role to assume and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpec {
    /// Role ARN.
    pub role_arn: String,
    /// Role session name.
    pub session_name: String,
    /// Session length in seconds.
    pub duration_secs: u32,
    /// External id of the trust policy.
    pub external_id: Option<String>,
}

impl RoleSpec {
    /// The role of an [`CredentialSource::AssumeRole`] source, with the id
    /// of the account that signs the call.
    pub fn from_source(source: &CredentialSource) -> Option<(String, RoleSpec)> {
        match source {
            CredentialSource::AssumeRole {
                source_account_id,
                role_arn,
                session_name,
                duration_secs,
                external_id,
                ..
            } => Some((
                source_account_id.clone(),
                RoleSpec {
                    role_arn: role_arn.clone(),
                    session_name: session_name.clone(),
                    duration_secs: *duration_secs,
                    external_id: external_id.clone(),
                },
            )),
            _ => None,
        }
    }
}

/// What STS returned for one `AssumeRole` call.
struct Assumed {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
    expires_at: SystemTime,
}

async fn assume(
    client: &aws_sdk_sts::Client,
    spec: &RoleSpec,
    mfa: Option<(&str, &str)>,
) -> Result<Assumed> {
    let duration = i32::try_from(spec.duration_secs)
        .map_err(|_| Error::Usage("the role session length is out of range".into()))?;
    let mut request = client
        .assume_role()
        .role_arn(&spec.role_arn)
        .role_session_name(&spec.session_name)
        .duration_seconds(duration)
        .set_external_id(spec.external_id.clone());
    if let Some((serial, code)) = mfa {
        request = request.serial_number(serial).token_code(code);
    }
    let output = request
        .send()
        .await
        .map_err(|error| Error::from(error).with_context("STS AssumeRole"))?;
    let issued = output
        .credentials
        .ok_or_else(|| Error::Remote("STS AssumeRole returned no credentials".into()))?;
    let expires_at = SystemTime::try_from(issued.expiration)
        .map_err(|_| Error::Remote("STS AssumeRole returned an invalid expiry".into()))?;
    Ok(Assumed {
        access_key_id: issued.access_key_id,
        secret_access_key: issued.secret_access_key,
        session_token: issued.session_token,
        expires_at,
    })
}

/// Assumes the role of `account` with an MFA code and returns the session
/// for [`crate::mfa::MfaSessions`]. `client` signs with the credentials of
/// the source account (see [`crate::mfa::sts_config_with_provider`]).
pub async fn request_role_session(
    account: &Account,
    client: &aws_sdk_sts::Client,
    code: &str,
) -> Result<MfaSession> {
    let (_, spec) = RoleSpec::from_source(&account.source)
        .ok_or_else(|| Error::Usage("the account does not assume a role".into()))?;
    let serial = account
        .source
        .role_mfa_serial()
        .ok_or_else(|| Error::Usage("the role has no MFA device configured".into()))?;
    validate_mfa_serial(serial)?;
    check_code(code)?;
    let assumed = assume(client, &spec, Some((serial, code))).await?;
    let credentials = Credentials::new(
        assumed.access_key_id.as_str(),
        assumed.secret_access_key.as_str(),
        Some(assumed.session_token.clone()),
    );
    Ok(MfaSession::new(credentials, assumed.expires_at).issued_for(account))
}

/// A credentials provider that assumes a role and refreshes the session
/// before it expires.
pub struct AssumeRoleCredentials {
    client: aws_sdk_sts::Client,
    spec: RoleSpec,
    time: SharedTimeSource,
    current: Mutex<Option<SdkCredentials>>,
}

impl AssumeRoleCredentials {
    /// A provider that calls STS with `client` (signed by the source
    /// account) and measures expiry with `time`.
    pub fn new(client: aws_sdk_sts::Client, spec: RoleSpec, time: SharedTimeSource) -> Self {
        Self {
            client,
            spec,
            time,
            current: Mutex::new(None),
        }
    }

    async fn load(&self) -> provider::Result {
        let mut current = self.current.lock().await;
        let now = self.time.now();
        if let Some(credentials) = current.as_ref() {
            let fresh = credentials
                .expiry()
                .is_some_and(|expiry| now + REFRESH_MARGIN < expiry);
            if fresh {
                return Ok(credentials.clone());
            }
        }
        let assumed = assume(&self.client, &self.spec, None)
            .await
            .map_err(CredentialsError::provider_error)?;
        let credentials = SdkCredentials::new(
            assumed.access_key_id,
            assumed.secret_access_key,
            Some(assumed.session_token),
            Some(assumed.expires_at),
            PROVIDER_NAME,
        );
        *current = Some(credentials.clone());
        Ok(credentials)
    }
}

impl std::fmt::Debug for AssumeRoleCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssumeRoleCredentials")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

impl ProvideCredentials for AssumeRoleCredentials {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(self.load())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use aws_sdk_sts::operation::assume_role::{AssumeRoleError, AssumeRoleOutput};
    use aws_sdk_sts::types::Credentials as StsCredentials;
    use aws_smithy_mocks::{Rule, RuleMode, mock};
    use aws_smithy_types::DateTime;

    use aws_smithy_async::time::TimeSource;

    use super::*;

    /// A clock the test moves by hand.
    #[derive(Debug, Clone)]
    struct Clock(Arc<AtomicU64>);

    impl Clock {
        fn new() -> Self {
            Clock(Arc::new(AtomicU64::new(1_800_000_000)))
        }
        fn advance(&self, secs: u64) {
            self.0.fetch_add(secs, Ordering::SeqCst);
        }
        fn shared(&self) -> SharedTimeSource {
            SharedTimeSource::new(self.clone())
        }
    }

    impl TimeSource for Clock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }

    fn spec() -> RoleSpec {
        RoleSpec {
            role_arn: "arn:aws:iam::123456789012:role/Writer".into(),
            session_name: "bucketbridge".into(),
            duration_secs: 3600,
            external_id: Some("ext-1".into()),
        }
    }

    fn issued(key: &str, expires_at: u64) -> AssumeRoleOutput {
        AssumeRoleOutput::builder()
            .credentials(
                StsCredentials::builder()
                    .access_key_id(key)
                    .secret_access_key("role-secret-value")
                    .session_token("role-token-value")
                    .expiration(DateTime::from_secs(expires_at as i64))
                    .build()
                    .unwrap(),
            )
            .build()
    }

    fn grants(key: &'static str, expires_at: u64) -> Rule {
        mock!(aws_sdk_sts::Client::assume_role)
            .match_requests(|request| {
                request.role_arn() == Some("arn:aws:iam::123456789012:role/Writer")
                    && request.role_session_name() == Some("bucketbridge")
                    && request.duration_seconds() == Some(3600)
                    && request.external_id() == Some("ext-1")
                    && request.serial_number().is_none()
            })
            .then_output(move || issued(key, expires_at))
    }

    #[tokio::test]
    async fn assumes_the_role_once_and_reuses_the_session() {
        let clock = Clock::new();
        let rule = grants("ASIAROLE1", 1_800_000_000 + 3600);
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&rule]);
        let provider = AssumeRoleCredentials::new(client, spec(), clock.shared());
        let first = provider.provide_credentials().await.unwrap();
        assert_eq!(first.access_key_id(), "ASIAROLE1");
        assert_eq!(first.session_token(), Some("role-token-value"));
        clock.advance(3000);
        let again = provider.provide_credentials().await.unwrap();
        assert_eq!(again.access_key_id(), "ASIAROLE1");
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn refreshes_before_the_session_expires() {
        let clock = Clock::new();
        let first = grants("ASIAROLE1", 1_800_000_000 + 3600);
        let second = grants("ASIAROLE2", 1_800_000_000 + 3300 + 3600);
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&first, &second]);
        let provider = AssumeRoleCredentials::new(client, spec(), clock.shared());
        assert_eq!(
            provider
                .provide_credentials()
                .await
                .unwrap()
                .access_key_id(),
            "ASIAROLE1"
        );
        // Just outside the margin: still the first session.
        clock.advance(3600 - REFRESH_MARGIN.as_secs() - 1);
        assert_eq!(
            provider
                .provide_credentials()
                .await
                .unwrap()
                .access_key_id(),
            "ASIAROLE1"
        );
        // Inside the margin: a new session.
        clock.advance(2);
        assert_eq!(
            provider
                .provide_credentials()
                .await
                .unwrap()
                .access_key_id(),
            "ASIAROLE2"
        );
        assert_eq!(first.num_calls(), 1);
        assert_eq!(second.num_calls(), 1);
    }

    #[tokio::test]
    async fn a_denied_assume_role_is_reported_and_retried_next_time() {
        let clock = Clock::new();
        let denied = mock!(aws_sdk_sts::Client::assume_role).then_error(|| {
            AssumeRoleError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDenied")
                    .message("not authorized to assume the role")
                    .build(),
            )
        });
        let granted = grants("ASIAROLE3", 1_800_000_000 + 3600);
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&denied, &granted]);
        let provider = AssumeRoleCredentials::new(client, spec(), clock.shared());
        let error = provider.provide_credentials().await.unwrap_err();
        assert!(format!("{error:?}").contains("AssumeRole"), "{error:?}");
        let ok = provider.provide_credentials().await.unwrap();
        assert_eq!(ok.access_key_id(), "ASIAROLE3");
    }

    fn mfa_account() -> Account {
        Account {
            id: "role-account".into(),
            label: "Role".into(),
            source: CredentialSource::AssumeRole {
                source_account_id: "base".into(),
                role_arn: "arn:aws:iam::123456789012:role/Writer".into(),
                session_name: "bucketbridge".into(),
                duration_secs: 3600,
                external_id: Some("ext-1".into()),
                mfa_serial: Some("arn:aws:iam::123456789012:mfa/me".into()),
            },
            mfa_serial: Some("arn:aws:iam::123456789012:mfa/me".into()),
            ..Account::default()
        }
    }

    #[tokio::test]
    async fn an_mfa_role_sends_the_device_and_the_code() {
        let rule = mock!(aws_sdk_sts::Client::assume_role)
            .match_requests(|request| {
                request.serial_number() == Some("arn:aws:iam::123456789012:mfa/me")
                    && request.token_code() == Some("123456")
                    && request.external_id() == Some("ext-1")
            })
            .then_output(|| issued("ASIAMFA", 1_900_000_000));
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&rule]);
        let session = request_role_session(&mfa_account(), &client, "123456")
            .await
            .unwrap();
        assert_eq!(session.credentials().access_key_id, "ASIAMFA");
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn a_wrong_mfa_code_is_an_auth_error() {
        let rule = mock!(aws_sdk_sts::Client::assume_role).then_error(|| {
            AssumeRoleError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDenied")
                    .message("MultiFactorAuthentication failed with invalid MFA one time pass code")
                    .build(),
            )
        });
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&rule]);
        let error = request_role_session(&mfa_account(), &client, "000000")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth, "{error}");
    }

    #[tokio::test]
    async fn a_malformed_code_never_reaches_sts() {
        let rule = grants("ASIAUNUSED", 1_900_000_000);
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, [&rule]);
        let error = request_role_session(&mfa_account(), &client, "12ab")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Usage);
        assert_eq!(rule.num_calls(), 0);
    }

    #[tokio::test]
    async fn an_account_without_a_device_cannot_log_in_with_a_code() {
        let mut account = mfa_account();
        account.source = CredentialSource::AssumeRole {
            source_account_id: "base".into(),
            role_arn: "arn:aws:iam::123456789012:role/Writer".into(),
            session_name: "bucketbridge".into(),
            duration_secs: 3600,
            external_id: None,
            mfa_serial: None,
        };
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, []);
        let error = request_role_session(&account, &client, "123456")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Usage);
    }

    #[test]
    fn debug_output_has_no_secrets() {
        let clock = Clock::new();
        let client = mock_client!(aws_sdk_sts, RuleMode::Sequential, []);
        let provider = AssumeRoleCredentials::new(client, spec(), clock.shared());
        let text = format!("{provider:?}");
        assert!(text.contains("Writer") && !text.contains("secret"));
    }
}
