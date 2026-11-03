//! Sign-in to IAM Identity Center with the OIDC device authorization flow.
//!
//! 1. [`start_login`] registers a public OIDC client (`RegisterClient`) and
//!    asks for a device authorization (`StartDeviceAuthorization`). The
//!    result carries the **user code** and the **verification URL** to show
//!    and open in a browser.
//! 2. [`poll_login`] exchanges the device code for a token (`CreateToken`).
//!    It answers [`Poll::Pending`] while the user has not approved yet
//!    (`AuthorizationPendingException`), lengthens the polling interval on
//!    `SlowDownException`, and fails with an `auth` error when the request
//!    is denied or the code expired.
//! 3. [`refresh_token`] renews an expired access token with the refresh
//!    token of the sign-in, without the browser.
//!
//! Nothing is written here: the caller stores the token with
//! [`super::cache::write_token`].

use std::time::Duration;

use aws_sdk_ssooidc::error::ProvideErrorMetadata;
use chrono::{DateTime, Utc};

use super::Portal;
use super::cache::SsoToken;
use crate::error::{Error, Result};
use crate::secrets::SecretString;

/// Name the OIDC client registers under.
pub const CLIENT_NAME: &str = "bucketbridge";

/// The scope that allows listing accounts and roles and getting role
/// credentials.
pub const SCOPE: &str = "sso:account:access";

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const REFRESH_GRANT: &str = "refresh_token";

/// Polling interval when the service does not name one.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// How much `SlowDownException` lengthens the interval.
pub const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// A sign-in that waits for the user: what to show, and what to poll with.
pub struct DeviceLogin {
    pub(crate) portal: Portal,
    client_id: String,
    client_secret: SecretString,
    registration_expires_at: Option<DateTime<Utc>>,
    device_code: SecretString,
    /// The code the user types at the verification page.
    pub user_code: String,
    /// The page to open.
    pub verification_uri: String,
    /// The page with the code filled in, when the service offers it.
    pub verification_uri_complete: Option<String>,
    /// When the code stops working.
    pub expires_at: DateTime<Utc>,
    /// How long to wait between polls (grows on `slow_down`).
    pub interval: Duration,
}

impl std::fmt::Debug for DeviceLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceLogin")
            .field("portal", &self.portal)
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_at", &self.expires_at)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// Result of one poll.
#[derive(Debug)]
pub enum Poll {
    /// Not approved yet: poll again after the login's `interval`.
    Pending,
    /// Approved: the token to cache.
    Complete(Box<SsoToken>),
}

fn code_of<E, R>(error: &aws_sdk_ssooidc::error::SdkError<E, R>) -> Option<String>
where
    E: ProvideErrorMetadata,
{
    error.code().map(str::to_owned)
}

/// Registers the OIDC client and starts the device authorization for
/// `portal`.
pub async fn start_login(
    oidc: &aws_sdk_ssooidc::Client,
    portal: &Portal,
    now: DateTime<Utc>,
) -> Result<DeviceLogin> {
    let registered = oidc
        .register_client()
        .client_name(CLIENT_NAME)
        .client_type("public")
        .scopes(SCOPE)
        .grant_types(DEVICE_GRANT)
        .grant_types(REFRESH_GRANT)
        .issuer_url(&portal.start_url)
        .send()
        .await
        .map_err(|error| Error::from(error).with_context("SSO RegisterClient"))?;
    let client_id = registered
        .client_id
        .ok_or_else(|| missing("RegisterClient", "a client id"))?;
    let client_secret = SecretString::from(
        registered
            .client_secret
            .ok_or_else(|| missing("RegisterClient", "a client secret"))?,
    );
    let registration_expires_at = (registered.client_secret_expires_at > 0)
        .then(|| DateTime::from_timestamp(registered.client_secret_expires_at, 0))
        .flatten();

    let started = oidc
        .start_device_authorization()
        .client_id(&client_id)
        .client_secret(client_secret.expose())
        .start_url(&portal.start_url)
        .send()
        .await
        .map_err(|error| Error::from(error).with_context("SSO StartDeviceAuthorization"))?;
    let device_code = SecretString::from(
        started
            .device_code
            .ok_or_else(|| missing("StartDeviceAuthorization", "a device code"))?,
    );
    let user_code = started
        .user_code
        .ok_or_else(|| missing("StartDeviceAuthorization", "a user code"))?;
    let verification_uri = started
        .verification_uri
        .ok_or_else(|| missing("StartDeviceAuthorization", "a verification URL"))?;
    let lifetime = i64::from(started.expires_in.max(1));
    let interval = match started.interval {
        seconds if seconds > 0 => Duration::from_secs(u64::from(seconds.unsigned_abs())),
        _ => DEFAULT_INTERVAL,
    };
    Ok(DeviceLogin {
        portal: portal.clone(),
        client_id,
        client_secret,
        registration_expires_at,
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete: started.verification_uri_complete,
        expires_at: now + chrono::Duration::seconds(lifetime),
        interval,
    })
}

fn missing(call: &str, what: &str) -> Error {
    Error::Remote(format!("SSO {call} did not return {what}"))
}

/// Polls `CreateToken` once.
pub async fn poll_login(
    oidc: &aws_sdk_ssooidc::Client,
    login: &mut DeviceLogin,
    now: DateTime<Utc>,
) -> Result<Poll> {
    if now >= login.expires_at {
        return Err(Error::Auth(
            "the sign-in code expired: start the sign-in again".into(),
        ));
    }
    let outcome = oidc
        .create_token()
        .client_id(&login.client_id)
        .client_secret(login.client_secret.expose())
        .grant_type(DEVICE_GRANT)
        .device_code(login.device_code.expose())
        .send()
        .await;
    match outcome {
        Ok(output) => {
            let token = token_from(
                &login.portal,
                output.access_token,
                output.expires_in,
                output.refresh_token,
                Some(login.client_id.clone()),
                Some(login.client_secret.clone()),
                login.registration_expires_at,
                now,
            )?;
            Ok(Poll::Complete(Box::new(token)))
        }
        Err(error) => match code_of(&error).as_deref() {
            Some("AuthorizationPendingException") => Ok(Poll::Pending),
            Some("SlowDownException") => {
                login.interval += SLOW_DOWN_STEP;
                Ok(Poll::Pending)
            }
            Some("AccessDeniedException") => {
                Err(Error::Auth("the sign-in was denied in the browser".into()))
            }
            Some("ExpiredTokenException") => Err(Error::Auth(
                "the sign-in code expired: start the sign-in again".into(),
            )),
            _ => Err(Error::from(error).with_context("SSO CreateToken")),
        },
    }
}

/// Renews `token` with its refresh token. Fails with an `auth` error telling
/// the user to sign in again when the token cannot be renewed (no refresh
/// token, the client registration expired, or the service refuses it).
pub async fn refresh_token(
    oidc: &aws_sdk_ssooidc::Client,
    portal: &Portal,
    token: &SsoToken,
    now: DateTime<Utc>,
) -> Result<SsoToken> {
    let sign_in_again = || Error::Auth("the SSO session expired: sign in again".into());
    let (Some(refresh), Some(client_id), Some(client_secret)) = (
        token.refresh_token.as_ref(),
        token.client_id.as_ref(),
        token.client_secret.as_ref(),
    ) else {
        return Err(sign_in_again());
    };
    if token
        .registration_expires_at
        .is_some_and(|expires| expires <= now)
    {
        return Err(sign_in_again());
    }
    let outcome = oidc
        .create_token()
        .client_id(client_id)
        .client_secret(client_secret.expose())
        .grant_type(REFRESH_GRANT)
        .refresh_token(refresh.expose())
        .send()
        .await;
    match outcome {
        Ok(output) => token_from(
            portal,
            output.access_token,
            output.expires_in,
            // The service may rotate the refresh token; keep the old one
            // when it does not.
            output
                .refresh_token
                .or_else(|| Some(refresh.expose().to_owned())),
            token.client_id.clone(),
            token.client_secret.clone(),
            token.registration_expires_at,
            now,
        ),
        Err(error) => match code_of(&error).as_deref() {
            Some(
                "InvalidGrantException"
                | "ExpiredTokenException"
                | "AccessDeniedException"
                | "InvalidClientException"
                | "UnauthorizedClientException",
            ) => Err(sign_in_again()),
            _ => Err(Error::from(error).with_context("SSO CreateToken (refresh)")),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn token_from(
    portal: &Portal,
    access_token: Option<String>,
    expires_in: i32,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<SecretString>,
    registration_expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<SsoToken> {
    let access_token = access_token.ok_or_else(|| missing("CreateToken", "an access token"))?;
    Ok(SsoToken {
        access_token: SecretString::from(access_token),
        expires_at: now + chrono::Duration::seconds(i64::from(expires_in.max(0))),
        refresh_token: refresh_token.map(SecretString::from),
        client_id,
        client_secret,
        registration_expires_at,
        region: Some(portal.region.clone()),
        start_url: Some(portal.start_url.clone()),
    })
}

#[cfg(test)]
mod tests {
    use aws_sdk_ssooidc::operation::create_token::{CreateTokenError, CreateTokenOutput};
    use aws_sdk_ssooidc::operation::register_client::RegisterClientOutput;
    use aws_sdk_ssooidc::operation::start_device_authorization::StartDeviceAuthorizationOutput;
    use aws_smithy_mocks::{Rule, RuleMode, mock};

    use super::*;

    fn coded(code: &str) -> aws_smithy_types::error::ErrorMetadata {
        aws_smithy_types::error::ErrorMetadata::builder()
            .code(code)
            .build()
    }

    fn portal() -> Portal {
        Portal {
            start_url: "https://example.awsapps.com/start".into(),
            region: "eu-west-1".into(),
            session_name: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn register() -> Rule {
        mock!(aws_sdk_ssooidc::Client::register_client)
            .match_requests(|request| {
                request.client_name() == Some(CLIENT_NAME)
                    && request.client_type() == Some("public")
                    && request.scopes() == [SCOPE]
                    && request.issuer_url() == Some("https://example.awsapps.com/start")
            })
            .then_output(|| {
                RegisterClientOutput::builder()
                    .client_id("client-1")
                    .client_secret("client-secret-1")
                    .client_secret_expires_at(1_900_000_000)
                    .build()
            })
    }

    fn start() -> Rule {
        mock!(aws_sdk_ssooidc::Client::start_device_authorization)
            .match_requests(|request| {
                request.client_id() == Some("client-1")
                    && request.client_secret() == Some("client-secret-1")
                    && request.start_url() == Some("https://example.awsapps.com/start")
            })
            .then_output(|| {
                StartDeviceAuthorizationOutput::builder()
                    .device_code("device-code-1")
                    .user_code("ABCD-EFGH")
                    .verification_uri("https://device.sso.eu-west-1.amazonaws.com/")
                    .verification_uri_complete(
                        "https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH",
                    )
                    .expires_in(600)
                    .interval(5)
                    .build()
            })
    }

    async fn started() -> (aws_sdk_ssooidc::Client, DeviceLogin) {
        let (register, start) = (register(), start());
        let client = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, [&register, &start]);
        let login = start_login(&client, &portal(), now()).await.unwrap();
        (client, login)
    }

    fn token_output(access: &str, refresh: Option<&str>) -> CreateTokenOutput {
        CreateTokenOutput::builder()
            .access_token(access)
            .expires_in(28_800)
            .set_refresh_token(refresh.map(str::to_owned))
            .build()
    }

    #[tokio::test]
    async fn start_registers_a_client_and_returns_the_code_to_show() {
        let (_, login) = started().await;
        assert_eq!(login.user_code, "ABCD-EFGH");
        assert_eq!(
            login.verification_uri,
            "https://device.sso.eu-west-1.amazonaws.com/"
        );
        assert_eq!(
            login.verification_uri_complete.as_deref(),
            Some("https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH")
        );
        assert_eq!(login.interval, Duration::from_secs(5));
        assert_eq!(login.expires_at, now() + chrono::Duration::seconds(600));
        let debug = format!("{login:?}");
        assert!(!debug.contains("device-code-1") && !debug.contains("client-secret-1"));
    }

    #[tokio::test]
    async fn polling_walks_through_pending_and_slow_down_to_a_token() {
        let (register, start) = (register(), start());
        let pending = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("AuthorizationPendingException")));
        let slow = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("SlowDownException")));
        let done = mock!(aws_sdk_ssooidc::Client::create_token)
            .match_requests(|request| {
                request.grant_type() == Some(DEVICE_GRANT)
                    && request.device_code() == Some("device-code-1")
                    && request.client_id() == Some("client-1")
                    && request.client_secret() == Some("client-secret-1")
            })
            .then_output(|| token_output("access-1", Some("refresh-1")));
        let client = mock_client!(
            aws_sdk_ssooidc,
            RuleMode::Sequential,
            [&register, &start, &pending, &slow, &done]
        );
        let mut login = start_login(&client, &portal(), now()).await.unwrap();
        assert!(matches!(
            poll_login(&client, &mut login, now()).await.unwrap(),
            Poll::Pending
        ));
        assert_eq!(login.interval, Duration::from_secs(5));
        assert!(matches!(
            poll_login(&client, &mut login, now()).await.unwrap(),
            Poll::Pending
        ));
        assert_eq!(login.interval, Duration::from_secs(10));
        let Poll::Complete(token) = poll_login(&client, &mut login, now()).await.unwrap() else {
            panic!("expected a token");
        };
        assert_eq!(token.access_token.expose(), "access-1");
        assert_eq!(token.refresh_token.as_ref().unwrap().expose(), "refresh-1");
        assert_eq!(token.expires_at, now() + chrono::Duration::seconds(28_800));
        assert_eq!(token.client_id.as_deref(), Some("client-1"));
        assert_eq!(token.region.as_deref(), Some("eu-west-1"));
        assert_eq!(
            token.registration_expires_at,
            DateTime::from_timestamp(1_900_000_000, 0)
        );
    }

    #[tokio::test]
    async fn a_denied_sign_in_is_an_auth_error() {
        let (register, start) = (register(), start());
        let denied = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("AccessDeniedException")));
        let client = mock_client!(
            aws_sdk_ssooidc,
            RuleMode::Sequential,
            [&register, &start, &denied]
        );
        let mut login = start_login(&client, &portal(), now()).await.unwrap();
        let error = poll_login(&client, &mut login, now()).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
        assert!(error.to_string().contains("denied"), "{error}");
    }

    #[tokio::test]
    async fn an_expired_device_code_is_an_auth_error() {
        let (register, start) = (register(), start());
        let expired = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("ExpiredTokenException")));
        let client = mock_client!(
            aws_sdk_ssooidc,
            RuleMode::Sequential,
            [&register, &start, &expired]
        );
        let mut login = start_login(&client, &portal(), now()).await.unwrap();
        let error = poll_login(&client, &mut login, now()).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
        assert!(error.to_string().contains("expired"), "{error}");
        // The local deadline is enforced without calling the service.
        let late = now() + chrono::Duration::seconds(601);
        let error = poll_login(&client, &mut login, late).await.unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
    }

    #[tokio::test]
    async fn other_failures_keep_their_context() {
        let (register, start) = (register(), start());
        let broken = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("InvalidGrantException")));
        let client = mock_client!(
            aws_sdk_ssooidc,
            RuleMode::Sequential,
            [&register, &start, &broken]
        );
        let mut login = start_login(&client, &portal(), now()).await.unwrap();
        let error = poll_login(&client, &mut login, now()).await.unwrap_err();
        assert!(error.to_string().contains("CreateToken"), "{error}");
    }

    fn signed_in(refresh: Option<&str>) -> SsoToken {
        SsoToken {
            access_token: SecretString::from("old-access"),
            expires_at: now() - chrono::Duration::seconds(10),
            refresh_token: refresh.map(SecretString::from),
            client_id: Some("client-1".into()),
            client_secret: Some(SecretString::from("client-secret-1")),
            registration_expires_at: DateTime::from_timestamp(1_900_000_000, 0),
            region: Some("eu-west-1".into()),
            start_url: Some("https://example.awsapps.com/start".into()),
        }
    }

    #[tokio::test]
    async fn refresh_uses_the_refresh_token() {
        let rule = mock!(aws_sdk_ssooidc::Client::create_token)
            .match_requests(|request| {
                request.grant_type() == Some(REFRESH_GRANT)
                    && request.refresh_token() == Some("refresh-1")
                    && request.client_id() == Some("client-1")
            })
            .then_output(|| token_output("new-access", None));
        let client = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, [&rule]);
        let renewed = refresh_token(&client, &portal(), &signed_in(Some("refresh-1")), now())
            .await
            .unwrap();
        assert_eq!(renewed.access_token.expose(), "new-access");
        // The old refresh token is kept when the service sends no new one.
        assert_eq!(renewed.refresh_token.unwrap().expose(), "refresh-1");
        assert_eq!(
            renewed.expires_at,
            now() + chrono::Duration::seconds(28_800)
        );
    }

    #[tokio::test]
    async fn refresh_without_what_it_needs_asks_to_sign_in_again() {
        let client = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, []);
        let no_refresh = refresh_token(&client, &portal(), &signed_in(None), now())
            .await
            .unwrap_err();
        assert_eq!(no_refresh.kind(), crate::ErrorKind::Auth);
        let mut stale = signed_in(Some("r"));
        stale.registration_expires_at = DateTime::from_timestamp(1_700_000_000, 0);
        let expired = refresh_token(&client, &portal(), &stale, now())
            .await
            .unwrap_err();
        assert!(expired.to_string().contains("sign in again"), "{expired}");
    }

    #[tokio::test]
    async fn a_refused_refresh_token_asks_to_sign_in_again() {
        let rule = mock!(aws_sdk_ssooidc::Client::create_token)
            .then_error(|| CreateTokenError::generic(coded("InvalidGrantException")));
        let client = mock_client!(aws_sdk_ssooidc, RuleMode::Sequential, [&rule]);
        let error = refresh_token(&client, &portal(), &signed_in(Some("r")), now())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
        assert!(error.to_string().contains("sign in again"), "{error}");
    }
}
