//! IAM Identity Center (AWS SSO).
//!
//! An [`Sso`](crate::model::CredentialSource::Sso) account names a portal
//! (start URL and region), an AWS account and a permission set (role). Its
//! credentials come from `GetRoleCredentials`, authorized by the portal
//! access token that a sign-in produced.
//!
//! * [`login`]: the OIDC device authorization flow, and renewing a token
//!   with its refresh token.
//! * [`cache`]: the token cache shared with the AWS CLI
//!   (`~/.aws/sso/cache`). The token is stored nowhere else.
//! * this module: choosing the account and role
//!   ([`list_accounts`], [`list_roles`]) and [`SsoCredentials`], the
//!   refreshing credentials provider of an account.
//!
//! The portal region decides the partition (commercial, GovCloud, China):
//! the SDK resolves the OIDC and portal endpoints from it.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials as SdkCredentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{self, ProvideCredentials, future};
use aws_sdk_sso::error::ProvideErrorMetadata;
use aws_smithy_async::time::SharedTimeSource;
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{SsoConfigHook, SsoOidcConfigHook};
use crate::client::validate_region;
use crate::error::{Error, Result};

pub mod cache;
pub mod login;

pub use cache::{CacheKey, SsoToken};
pub use login::{DeviceLogin, Poll};

/// An access token is renewed this long before it expires.
pub const TOKEN_MARGIN: Duration = Duration::from_secs(60);

/// Role credentials are fetched again this long before they expire.
pub const CREDENTIALS_MARGIN: Duration = Duration::from_secs(5 * 60);

const PROVIDER_NAME: &str = "BucketBridgeSso";

/// An Identity Center portal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Portal {
    /// Start URL (`https://<directory>.awsapps.com/start`).
    pub start_url: String,
    /// Region Identity Center runs in.
    pub region: String,
    /// `sso-session` name, when the sign-in is cached under one.
    pub session_name: Option<String>,
}

impl Portal {
    /// A portal, validated: an https start URL and a region name.
    pub fn new(start_url: &str, region: &str, session_name: Option<&str>) -> Result<Self> {
        let start_url = start_url.trim();
        if !start_url.starts_with("https://") || start_url.len() < 12 {
            return Err(Error::Usage(
                "the IAM Identity Center start URL must start with https://".into(),
            ));
        }
        validate_region(region)?;
        Ok(Portal {
            start_url: start_url.to_owned(),
            region: region.to_owned(),
            session_name: session_name
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    /// The token cache entry of this portal.
    pub fn cache_key(&self) -> CacheKey {
        CacheKey::new(&self.start_url, self.session_name.as_deref())
    }
}

/// The OIDC client of a portal region.
pub fn oidc_client(
    region: &str,
    http_client: SharedHttpClient,
    hook: Option<&SsoOidcConfigHook>,
) -> aws_sdk_ssooidc::Client {
    let mut builder = aws_sdk_ssooidc::Config::builder()
        .behavior_version(aws_sdk_ssooidc::config::BehaviorVersion::latest())
        .region(aws_sdk_ssooidc::config::Region::new(region.to_owned()))
        .http_client(http_client);
    if let Some(hook) = hook {
        builder = hook(builder);
    }
    aws_sdk_ssooidc::Client::from_conf(builder.build())
}

/// The portal API client of a portal region.
pub fn portal_client(
    region: &str,
    http_client: SharedHttpClient,
    hook: Option<&SsoConfigHook>,
) -> aws_sdk_sso::Client {
    let mut builder = aws_sdk_sso::Config::builder()
        .behavior_version(aws_sdk_sso::config::BehaviorVersion::latest())
        .region(aws_sdk_sso::config::Region::new(region.to_owned()))
        .http_client(http_client);
    if let Some(hook) = hook {
        builder = hook(builder);
    }
    aws_sdk_sso::Client::from_conf(builder.build())
}

fn to_chrono(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

fn not_signed_in() -> Error {
    Error::Auth("not signed in to IAM Identity Center (or the session ended): sign in again".into())
}

/// The cached token of `portal`, renewed first when it is about to expire
/// and a refresh token exists. Fails with an `auth` error when there is no
/// sign-in, or it ended and cannot be renewed.
pub async fn usable_token(
    oidc: &aws_sdk_ssooidc::Client,
    portal: &Portal,
    cache_dir: &std::path::Path,
    now: DateTime<Utc>,
) -> Result<SsoToken> {
    let key = portal.cache_key();
    let Some(token) = cache::read_token(cache_dir, &key)? else {
        return Err(not_signed_in());
    };
    let margin = chrono::Duration::from_std(TOKEN_MARGIN).unwrap_or_default();
    if token.expires_at - margin > now {
        return Ok(token);
    }
    let renewed = login::refresh_token(oidc, portal, &token, now).await?;
    cache::write_token(cache_dir, &key, &renewed)?;
    Ok(renewed)
}

/// Whether a portal has a usable sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInStatus {
    /// A token exists and is valid now, or can be renewed.
    pub signed_in: bool,
    /// When the access token expires (RFC 3339), if there is one.
    pub expires_at: Option<String>,
    /// Whether an expired token can be renewed without the browser.
    pub can_refresh: bool,
}

/// Reads the sign-in state of `portal` from the cache without calling AWS.
pub fn sign_in_status(
    cache_dir: &std::path::Path,
    portal: &Portal,
    now: DateTime<Utc>,
) -> Result<SignInStatus> {
    let token = cache::read_token(cache_dir, &portal.cache_key())?;
    Ok(match token {
        None => SignInStatus {
            signed_in: false,
            expires_at: None,
            can_refresh: false,
        },
        Some(token) => {
            let can_refresh = token.refresh_token.is_some()
                && token.client_id.is_some()
                && token.client_secret.is_some()
                && token
                    .registration_expires_at
                    .is_none_or(|expires| expires > now);
            SignInStatus {
                signed_in: token.expires_at > now || can_refresh,
                expires_at: Some(token.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
                can_refresh,
            }
        }
    })
}

/// An AWS account the signed-in user can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoAccount {
    /// 12-digit account id.
    pub account_id: String,
    /// Account name.
    pub account_name: String,
    /// Email address of the account root.
    pub email_address: Option<String>,
}

fn unauthorized_or<E, R>(error: aws_sdk_sso::error::SdkError<E, R>, call: &str) -> Error
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    R: std::fmt::Debug + Send + Sync + 'static,
{
    match error.code() {
        Some("UnauthorizedException") => Error::Auth(
            "the IAM Identity Center sign-in was rejected or ended: sign in again".into(),
        ),
        _ => Error::from(error).with_context(format!("SSO {call}")),
    }
}

/// Lists the accounts the token can reach, sorted by name.
pub async fn list_accounts(
    portal: &aws_sdk_sso::Client,
    token: &SsoToken,
) -> Result<Vec<SsoAccount>> {
    let mut pages = portal
        .list_accounts()
        .access_token(token.access_token.expose())
        .into_paginator()
        .send();
    let mut accounts = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|error| unauthorized_or(error, "ListAccounts"))?;
        accounts.extend(page.account_list().iter().filter_map(|info| {
            Some(SsoAccount {
                account_id: info.account_id()?.to_owned(),
                account_name: info.account_name().unwrap_or_default().to_owned(),
                email_address: info.email_address().map(str::to_owned),
            })
        }));
    }
    accounts.sort_by(|a, b| {
        a.account_name
            .to_lowercase()
            .cmp(&b.account_name.to_lowercase())
            .then_with(|| a.account_id.cmp(&b.account_id))
    });
    Ok(accounts)
}

/// Lists the roles (permission sets) of one account, sorted by name.
pub async fn list_roles(
    portal: &aws_sdk_sso::Client,
    token: &SsoToken,
    account_id: &str,
) -> Result<Vec<String>> {
    let mut pages = portal
        .list_account_roles()
        .access_token(token.access_token.expose())
        .account_id(account_id)
        .into_paginator()
        .send();
    let mut roles = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|error| unauthorized_or(error, "ListAccountRoles"))?;
        roles.extend(
            page.role_list()
                .iter()
                .filter_map(|info| info.role_name().map(str::to_owned)),
        );
    }
    roles.sort();
    Ok(roles)
}

/// A credentials provider for one role of one account: it reads the cached
/// sign-in (renewing it when needed) and calls `GetRoleCredentials`, and
/// fetches the credentials again before they expire.
pub struct SsoCredentials {
    portal: Portal,
    oidc: aws_sdk_ssooidc::Client,
    sso: aws_sdk_sso::Client,
    cache_dir: PathBuf,
    account_id: String,
    role_name: String,
    time: SharedTimeSource,
    current: Mutex<Option<SdkCredentials>>,
}

impl SsoCredentials {
    /// The provider of `role_name` in `account_id`, signed in to `portal`.
    pub fn new(
        portal: Portal,
        oidc: aws_sdk_ssooidc::Client,
        sso: aws_sdk_sso::Client,
        cache_dir: PathBuf,
        account_id: &str,
        role_name: &str,
        time: SharedTimeSource,
    ) -> Self {
        Self {
            portal,
            oidc,
            sso,
            cache_dir,
            account_id: account_id.to_owned(),
            role_name: role_name.to_owned(),
            time,
            current: Mutex::new(None),
        }
    }

    async fn load(&self) -> provider::Result {
        let mut current = self.current.lock().await;
        let now = self.time.now();
        if let Some(credentials) = current.as_ref()
            && credentials
                .expiry()
                .is_some_and(|expiry| now + CREDENTIALS_MARGIN < expiry)
        {
            return Ok(credentials.clone());
        }
        let credentials = self
            .fetch(now)
            .await
            .map_err(CredentialsError::provider_error)?;
        *current = Some(credentials.clone());
        Ok(credentials)
    }

    async fn fetch(&self, now: SystemTime) -> Result<SdkCredentials> {
        let token = usable_token(&self.oidc, &self.portal, &self.cache_dir, to_chrono(now)).await?;
        let output = self
            .sso
            .get_role_credentials()
            .access_token(token.access_token.expose())
            .account_id(&self.account_id)
            .role_name(&self.role_name)
            .send()
            .await
            .map_err(|error| match error.code() {
                Some("ResourceNotFoundException") => Error::NotFound(format!(
                    "role {:?} was not found in AWS account {}",
                    self.role_name, self.account_id
                )),
                _ => unauthorized_or(error, "GetRoleCredentials"),
            })?;
        let issued = output.role_credentials.ok_or_else(|| {
            Error::Remote("SSO GetRoleCredentials returned no credentials".into())
        })?;
        let (Some(access_key_id), Some(secret), Some(session_token)) = (
            issued.access_key_id,
            issued.secret_access_key,
            issued.session_token,
        ) else {
            return Err(Error::Remote(
                "SSO GetRoleCredentials returned incomplete credentials".into(),
            ));
        };
        let expires_at = u64::try_from(issued.expiration)
            .ok()
            .map(|millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis))
            .ok_or_else(|| Error::Remote("SSO GetRoleCredentials returned a bad expiry".into()))?;
        Ok(SdkCredentials::new(
            access_key_id,
            secret,
            Some(session_token),
            Some(expires_at),
            PROVIDER_NAME,
        ))
    }
}

impl std::fmt::Debug for SsoCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoCredentials")
            .field("portal", &self.portal)
            .field("account_id", &self.account_id)
            .field("role_name", &self.role_name)
            .finish_non_exhaustive()
    }
}

impl ProvideCredentials for SsoCredentials {
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

    use aws_sdk_sso::operation::get_role_credentials::{
        GetRoleCredentialsError, GetRoleCredentialsOutput,
    };
    use aws_sdk_sso::operation::list_account_roles::ListAccountRolesOutput;
    use aws_sdk_sso::operation::list_accounts::ListAccountsOutput;
    use aws_sdk_sso::types::{AccountInfo, RoleCredentials, RoleInfo};
    use aws_sdk_ssooidc::operation::create_token::CreateTokenOutput;
    use aws_smithy_mocks::{Rule, RuleMode, mock};

    use aws_smithy_async::time::TimeSource;

    use super::*;
    use crate::secrets::SecretString;

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

    fn coded(code: &str) -> aws_smithy_types::error::ErrorMetadata {
        aws_smithy_types::error::ErrorMetadata::builder()
            .code(code)
            .build()
    }

    fn portal() -> Portal {
        Portal::new(
            "https://example.awsapps.com/start",
            "eu-west-1",
            Some("corp"),
        )
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn signed_in(expires_in: i64) -> SsoToken {
        SsoToken {
            access_token: SecretString::from("access-1"),
            expires_at: now() + chrono::Duration::seconds(expires_in),
            refresh_token: Some(SecretString::from("refresh-1")),
            client_id: Some("client-1".into()),
            client_secret: Some(SecretString::from("client-secret-1")),
            registration_expires_at: DateTime::from_timestamp(1_900_000_000, 0),
            region: Some("eu-west-1".into()),
            start_url: Some("https://example.awsapps.com/start".into()),
        }
    }

    fn role_credentials(key: &str, expires_at_secs: i64) -> Rule {
        mock!(aws_sdk_sso::Client::get_role_credentials)
            .match_requests(|request| {
                request.account_id() == Some("123456789012")
                    && request.role_name() == Some("Admin")
                    && request.access_token().is_some()
            })
            .then_output({
                let key = key.to_owned();
                move || {
                    GetRoleCredentialsOutput::builder()
                        .role_credentials(
                            RoleCredentials::builder()
                                .access_key_id(&key)
                                .secret_access_key("role-secret-value")
                                .session_token("role-token-value")
                                .expiration(expires_at_secs * 1000)
                                .build(),
                        )
                        .build()
                }
            })
    }

    struct Setup {
        _dir: tempfile::TempDir,
        cache: PathBuf,
        clock: Clock,
    }

    fn setup(token: Option<SsoToken>) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        if let Some(token) = token {
            cache::write_token(&cache, &portal().cache_key(), &token).unwrap();
        }
        Setup {
            _dir: dir,
            cache,
            clock: Clock::new(),
        }
    }

    fn provider(
        setup: &Setup,
        oidc: aws_sdk_ssooidc::Client,
        sso: aws_sdk_sso::Client,
    ) -> SsoCredentials {
        SsoCredentials::new(
            portal(),
            oidc,
            sso,
            setup.cache.clone(),
            "123456789012",
            "Admin",
            setup.clock.shared(),
        )
    }

    #[tokio::test]
    async fn signs_with_role_credentials_of_the_cached_sign_in() {
        let setup = setup(Some(signed_in(3600)));
        let rule = role_credentials("ASIASSO1", 1_800_000_000 + 3600);
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&rule]);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let provider = provider(&setup, oidc, sso);
        let credentials = provider.provide_credentials().await.unwrap();
        assert_eq!(credentials.access_key_id(), "ASIASSO1");
        assert_eq!(credentials.session_token(), Some("role-token-value"));
        // Reused until close to expiry.
        setup.clock.advance(1000);
        provider.provide_credentials().await.unwrap();
        assert_eq!(rule.num_calls(), 1);
        let text = format!("{provider:?}");
        assert!(!text.contains("access-1") && !text.contains("secret"));
    }

    #[tokio::test]
    async fn fetches_new_role_credentials_before_they_expire() {
        let setup = setup(Some(signed_in(8 * 3600)));
        let first = role_credentials("ASIASSO1", 1_800_000_000 + 3600);
        let second = role_credentials("ASIASSO2", 1_800_000_000 + 7200);
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&first, &second]);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let provider = provider(&setup, oidc, sso);
        assert_eq!(
            provider
                .provide_credentials()
                .await
                .unwrap()
                .access_key_id(),
            "ASIASSO1"
        );
        setup.clock.advance(3600 - 300 + 1);
        assert_eq!(
            provider
                .provide_credentials()
                .await
                .unwrap()
                .access_key_id(),
            "ASIASSO2"
        );
    }

    #[tokio::test]
    async fn no_sign_in_asks_the_user_to_sign_in() {
        let setup = setup(None);
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, []);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let error = provider(&setup, oidc, sso)
            .provide_credentials()
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("sign in"), "{error:?}");
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_and_the_cache_updated() {
        let setup = setup(Some(signed_in(-5)));
        let refresh = mock!(aws_sdk_ssooidc::Client::create_token)
            .match_requests(|request| {
                request.grant_type() == Some("refresh_token")
                    && request.refresh_token() == Some("refresh-1")
            })
            .then_output(|| {
                CreateTokenOutput::builder()
                    .access_token("access-2")
                    .refresh_token("refresh-2")
                    .expires_in(28_800)
                    .build()
            });
        let rule = mock!(aws_sdk_sso::Client::get_role_credentials)
            .match_requests(|request| request.access_token() == Some("access-2"))
            .then_output(|| {
                GetRoleCredentialsOutput::builder()
                    .role_credentials(
                        RoleCredentials::builder()
                            .access_key_id("ASIASSO3")
                            .secret_access_key("s")
                            .session_token("t")
                            .expiration((1_800_000_000 + 3600) * 1000)
                            .build(),
                    )
                    .build()
            });
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&rule]);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, [&refresh]);
        let credentials = provider(&setup, oidc, sso)
            .provide_credentials()
            .await
            .unwrap();
        assert_eq!(credentials.access_key_id(), "ASIASSO3");
        let cached = cache::read_token(&setup.cache, &portal().cache_key())
            .unwrap()
            .unwrap();
        assert_eq!(cached.access_token.expose(), "access-2");
        assert_eq!(cached.refresh_token.unwrap().expose(), "refresh-2");
    }

    #[tokio::test]
    async fn an_expired_token_without_a_refresh_token_needs_a_new_sign_in() {
        let mut token = signed_in(-5);
        token.refresh_token = None;
        let setup = setup(Some(token));
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, []);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let error = provider(&setup, oidc, sso)
            .provide_credentials()
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("sign in again"), "{error:?}");
    }

    #[tokio::test]
    async fn a_rejected_token_is_an_auth_error() {
        let setup = setup(Some(signed_in(3600)));
        let rule = mock!(aws_sdk_sso::Client::get_role_credentials)
            .then_error(|| GetRoleCredentialsError::generic(coded("UnauthorizedException")));
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&rule]);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let error = provider(&setup, oidc, sso)
            .provide_credentials()
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("sign in again"), "{error:?}");
    }

    #[tokio::test]
    async fn an_unknown_role_is_reported_with_its_name() {
        let setup = setup(Some(signed_in(3600)));
        let rule = mock!(aws_sdk_sso::Client::get_role_credentials)
            .then_error(|| GetRoleCredentialsError::generic(coded("ResourceNotFoundException")));
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&rule]);
        let oidc = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let error = provider(&setup, oidc, sso)
            .provide_credentials()
            .await
            .unwrap_err();
        assert!(format!("{error:?}").contains("Admin"), "{error:?}");
    }

    #[tokio::test]
    async fn lists_accounts_and_roles_with_the_access_token() {
        let accounts = mock!(aws_sdk_sso::Client::list_accounts)
            .match_requests(|request| request.access_token() == Some("access-1"))
            .then_output(|| {
                ListAccountsOutput::builder()
                    .account_list(
                        AccountInfo::builder()
                            .account_id("222222222222")
                            .account_name("Zeta")
                            .email_address("z@example.com")
                            .build(),
                    )
                    .account_list(
                        AccountInfo::builder()
                            .account_id("111111111111")
                            .account_name("alpha")
                            .build(),
                    )
                    .build()
            });
        let roles = mock!(aws_sdk_sso::Client::list_account_roles)
            .match_requests(|request| request.account_id() == Some("111111111111"))
            .then_output(|| {
                ListAccountRolesOutput::builder()
                    .role_list(
                        RoleInfo::builder()
                            .role_name("ReadOnly")
                            .account_id("111111111111")
                            .build(),
                    )
                    .role_list(
                        RoleInfo::builder()
                            .role_name("Admin")
                            .account_id("111111111111")
                            .build(),
                    )
                    .build()
            });
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&accounts, &roles]);
        let token = signed_in(3600);
        let listed = list_accounts(&sso, &token).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|a| a.account_id.as_str())
                .collect::<Vec<_>>(),
            ["111111111111", "222222222222"]
        );
        assert_eq!(listed[1].email_address.as_deref(), Some("z@example.com"));
        let names = list_roles(&sso, &token, "111111111111").await.unwrap();
        assert_eq!(names, ["Admin", "ReadOnly"]);
    }

    #[tokio::test]
    async fn listing_with_a_rejected_token_asks_to_sign_in_again() {
        let rule = mock!(aws_sdk_sso::Client::list_accounts).then_error(|| {
            aws_sdk_sso::operation::list_accounts::ListAccountsError::generic(coded(
                "UnauthorizedException",
            ))
        });
        let sso = mock_client!(aws_sdk_sso, RuleMode::Sequential, [&rule]);
        let error = list_accounts(&sso, &signed_in(3600)).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
    }

    #[test]
    fn sign_in_status_reads_the_cache_only() {
        let setup = setup(None);
        let status = sign_in_status(&setup.cache, &portal(), now()).unwrap();
        assert!(!status.signed_in && status.expires_at.is_none());
        cache::write_token(&setup.cache, &portal().cache_key(), &signed_in(3600)).unwrap();
        let status = sign_in_status(&setup.cache, &portal(), now()).unwrap();
        assert!(status.signed_in && status.can_refresh);
        assert!(status.expires_at.is_some());
        let mut expired = signed_in(-10);
        expired.refresh_token = None;
        cache::write_token(&setup.cache, &portal().cache_key(), &expired).unwrap();
        let status = sign_in_status(&setup.cache, &portal(), now()).unwrap();
        assert!(!status.signed_in && !status.can_refresh);
    }

    #[test]
    fn portals_are_validated() {
        assert!(Portal::new("http://x.example.com/start", "us-east-1", None).is_err());
        assert!(Portal::new("https://x.awsapps.com/start", "bad region", None).is_err());
        let gov = Portal::new("https://x.awsapps.com/start", "us-gov-west-1", None).unwrap();
        assert_eq!(gov.region, "us-gov-west-1");
        let china = Portal::new("https://x.awsapps.cn/start", "cn-north-1", None).unwrap();
        assert_eq!(china.cache_key().session_name, None);
    }
}
