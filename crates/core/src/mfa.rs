//! MFA sessions: temporary credentials from STS `GetSessionToken`.
//!
//! An account with an `mfa_serial` signs its requests with temporary
//! credentials obtained with the code of its MFA device:
//!
//! 1. `mfa_login` gives the account's long-term keys, its MFA serial and the
//!    six digit code to [`get_session_token`] (or [`login`], which also checks
//!    the account and stores the result).
//! 2. STS returns temporary credentials. [`MfaSessions`] keeps them **in
//!    memory only**, per account, until their expiry minus a safety margin
//!    ([`DEFAULT_EXPIRY_MARGIN`]).
//! 3. Requests take the credentials from [`MfaSessions::credentials`], which
//!    fails with [`ErrorKind::MfaRequired`](crate::ErrorKind::MfaRequired) when the account has no session or
//!    the session is about to expire; the UI then asks for a new code.
//!
//! # What a session is bound to
//!
//! A session is only used for the account state it was issued for: its access
//! key id, the references of its secret and session token, its endpoint and
//! its MFA serial. [`MfaSessions::credentials`] drops a session whose account
//! no longer matches (an endpoint change would otherwise send the temporary
//! credentials of one server to another; a rotation or a new MFA device, made
//! by another process, would keep the old session alive).
//!
//! A login that is waiting for STS must not outlive what happens meanwhile:
//! [`MfaSessions::generation`] is read before the login starts, and
//! [`MfaSessions::insert_since`] stores the session only if nothing it
//! depends on ([`MfaSessions::clear`], [`MfaSessions::remove`] of that account)
//! happened since.
//!
//! # STS endpoint
//!
//! - **AWS accounts** (`endpoint` is `None`): the regional STS endpoint of
//!   the account's region, resolved by the SDK (for example
//!   `https://sts.eu-west-1.amazonaws.com`, or the China and GovCloud
//!   equivalents). Tokens from regional endpoints are valid in every region,
//!   and the account's region is already known to be reachable.
//! - **Accounts with a custom endpoint**: the account's own endpoint URL,
//!   signed for the account's region. S3-compatible servers that implement STS
//!   serve it at the same address as S3. Requests for such an account are
//!   never sent to AWS, which would expose its access key id to a third
//!   party. A server without STS support makes the login fail with
//!   [`ErrorKind::Remote`](crate::ErrorKind::Remote).
//!
//! The STS client uses only the given static credentials (no environment,
//! profile or instance credentials), retries nothing (a code is meant to be
//! used once, and a retried request could be rejected for reusing it) and
//! should be given the proxy-aware HTTP client from [`crate::proxy`].

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use aws_sdk_sts::config::retry::RetryConfig;
use aws_sdk_sts::config::timeout::TimeoutConfig;
use aws_sdk_sts::config::{BehaviorVersion, Region, SharedHttpClient};
use aws_sdk_sts::operation::get_session_token::GetSessionTokenOutput;
use aws_smithy_async::time::{SharedTimeSource, SystemTimeSource, TimeSource};

use crate::client::{Credentials, validate_endpoint, validate_region};
use crate::error::{Error, Result};
use crate::model::Account;

/// How long before its expiry a session stops being used.
pub const DEFAULT_EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);

/// Shortest session STS accepts.
pub const MIN_SESSION_DURATION: Duration = Duration::from_secs(900);

/// Longest session STS accepts (36 hours; one hour for root credentials).
pub const MAX_SESSION_DURATION: Duration = Duration::from_secs(129_600);

/// Default end-to-end timeout of one STS call.
pub const STS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

const CREDENTIALS_PROVIDER_NAME: &str = "BucketBridge";

/// What an MFA session was issued for: the fields of an account that decide
/// whose credentials STS signed for, and where they are sent.
#[derive(Clone, PartialEq, Eq)]
struct Binding {
    access_key_id: String,
    secret_ref: String,
    session_token_ref: Option<String>,
    endpoint: Option<String>,
    mfa_serial: Option<String>,
}

impl Binding {
    fn of(account: &Account) -> Self {
        Self {
            access_key_id: account.access_key_id.clone(),
            secret_ref: account.secret_ref.clone(),
            session_token_ref: account.session_token_ref.clone(),
            endpoint: account.endpoint.clone(),
            mfa_serial: account.mfa_serial.clone(),
        }
    }
}

/// Temporary credentials of one MFA session.
#[derive(Clone)]
pub struct MfaSession {
    credentials: Credentials,
    expires_at: SystemTime,
    /// The account state STS issued it for; `None` for a session built by
    /// hand, which takes the account it is inserted for.
    binding: Option<Binding>,
}

impl MfaSession {
    /// A session from credentials and their expiry.
    pub fn new(credentials: Credentials, expires_at: SystemTime) -> Self {
        Self {
            credentials,
            expires_at,
            binding: None,
        }
    }

    fn issued_for(mut self, account: &Account) -> Self {
        self.binding = Some(Binding::of(account));
        self
    }

    /// The temporary credentials, including the session token.
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// When STS lets the credentials expire.
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}

impl fmt::Debug for MfaSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MfaSession")
            .field("credentials", &self.credentials)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Where `GetSessionToken` is sent for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StsEndpoint {
    /// The SDK's regional AWS endpoint for `region`.
    Aws {
        /// The account's region.
        region: String,
    },
    /// The account's own endpoint, signed for `region`.
    Custom {
        /// The account's endpoint URL.
        url: String,
        /// The account's region (the signing region).
        region: String,
    },
}

/// Decides the STS endpoint of `account` (see the [module docs](self)).
///
/// A custom endpoint and its region are checked like S3 checks them
/// ([`validate_endpoint`], [`validate_region`]); an AWS region must also be
/// a valid host label, since it becomes part of the STS host name.
pub fn sts_endpoint(account: &Account) -> Result<StsEndpoint> {
    let region = account.region.as_str();
    match account.endpoint.as_deref() {
        None => {
            check_aws_region(region)?;
            Ok(StsEndpoint::Aws {
                region: region.to_owned(),
            })
        }
        Some(url) => {
            validate_endpoint(url)?;
            validate_region(region)?;
            Ok(StsEndpoint::Custom {
                url: url.trim_end_matches('/').to_owned(),
                region: region.to_owned(),
            })
        }
    }
}

/// STS client configuration for `account` signed with its long-term
/// `credentials`: endpoint per [`sts_endpoint`], retries disabled,
/// [`STS_OPERATION_TIMEOUT`]. Callers add the HTTP client (and may override
/// the timeouts) before building.
pub fn sts_config(
    account: &Account,
    credentials: &Credentials,
) -> Result<aws_sdk_sts::config::Builder> {
    let endpoint = sts_endpoint(account)?;
    let static_credentials = aws_sdk_sts::config::Credentials::new(
        credentials.access_key_id.as_str(),
        credentials.secret_access_key.as_str(),
        credentials.session_token.clone(),
        None,
        CREDENTIALS_PROVIDER_NAME,
    );
    let (region, url) = match endpoint {
        StsEndpoint::Aws { region } => (region, None),
        StsEndpoint::Custom { url, region } => (region, Some(url)),
    };
    let mut builder = aws_sdk_sts::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region))
        .credentials_provider(static_credentials)
        .retry_config(RetryConfig::disabled())
        .timeout_config(
            TimeoutConfig::builder()
                .operation_timeout(STS_OPERATION_TIMEOUT)
                .build(),
        );
    if let Some(url) = url {
        builder = builder.endpoint_url(url);
    }
    Ok(builder)
}

/// An STS client for `account` using `http_client`: pass the proxy-aware
/// client of [`crate::proxy`]. `None` uses the SDK's default client, which
/// honours the `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` variables.
pub fn sts_client(
    account: &Account,
    credentials: &Credentials,
    http_client: Option<SharedHttpClient>,
) -> Result<aws_sdk_sts::Client> {
    let mut builder = sts_config(account, credentials)?;
    if let Some(http_client) = http_client {
        builder = builder.http_client(http_client);
    }
    Ok(aws_sdk_sts::Client::from_conf(builder.build()))
}

/// Calls STS `GetSessionToken` with an MFA device.
///
/// `serial` is the device serial number or ARN, `code` the six digits it
/// shows, `duration` the requested session length (`None` lets STS choose:
/// 12 hours for IAM users, 1 hour for root credentials).
///
/// Errors: malformed input is a usage error (STS is not called). STS and
/// network failures go through the crate's `SdkError` conversion: a rejected
/// code or rejected keys (`AccessDenied`, `InvalidClientTokenId`, ...) are
/// auth errors, rejected proxy credentials too; anything else is remote.
pub async fn get_session_token(
    client: &aws_sdk_sts::Client,
    serial: &str,
    code: &str,
    duration: Option<Duration>,
) -> Result<MfaSession> {
    validate_mfa_serial(serial)?;
    check_code(code)?;
    let duration_seconds = duration.map(check_duration).transpose()?;
    let output = client
        .get_session_token()
        .serial_number(serial)
        .token_code(code)
        .set_duration_seconds(duration_seconds)
        .send()
        .await
        .map_err(|error| Error::from(error).with_context("STS GetSessionToken"))?;
    session_from_output(output)
}

/// Checks an MFA device serial number or ARN: 9 to 256 characters of
/// letters, digits and `_+=/:,.@-`, as STS requires.
pub fn validate_mfa_serial(serial: &str) -> Result<()> {
    let valid_chars = serial.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'+' | b'=' | b'/' | b':' | b',' | b'.' | b'@' | b'-'
            )
    });
    if (9..=256).contains(&serial.len()) && valid_chars {
        Ok(())
    } else {
        Err(invalid_serial())
    }
}

/// `mfa_login` for one account: checks that it has an MFA device, calls STS
/// with `client` (from [`sts_client`]) and stores the session in `sessions`.
/// Returns when the session stops being used.
///
/// The session is stored only if `sessions` was not cleared, and the account's
/// session not removed, while STS was answering (see
/// [`MfaSessions::generation`]); otherwise it fails with
/// [`ErrorKind::Conflict`](crate::ErrorKind::Conflict).
pub async fn login(
    sessions: &MfaSessions,
    account: &Account,
    client: &aws_sdk_sts::Client,
    code: &str,
    duration: Option<Duration>,
) -> Result<SystemTime> {
    let generation = sessions.generation();
    let session = request_session(account, client, code, duration).await?;
    sessions.insert_since(account, generation, session)
}

/// The STS half of [`login`]: checks that the account has an MFA device and
/// asks STS for a session, bound to the account's current state. Store it with
/// [`MfaSessions::insert_since`], passing the generation read before this call.
pub async fn request_session(
    account: &Account,
    client: &aws_sdk_sts::Client,
    code: &str,
    duration: Option<Duration>,
) -> Result<MfaSession> {
    let serial = account
        .mfa_serial
        .as_deref()
        .filter(|serial| !serial.is_empty())
        .ok_or_else(no_mfa_device)?;
    let session = get_session_token(client, serial, code, duration).await?;
    Ok(session.issued_for(account))
}

/// In-memory MFA sessions per account id, valid until expiry minus a safety
/// margin. Thread-safe; share one instance behind an `Arc`.
pub struct MfaSessions {
    state: Mutex<State>,
    clock: SharedTimeSource,
    margin: Duration,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, Stored>,
    /// Counts the times sessions were forgotten (see
    /// [`MfaSessions::generation`]).
    tick: u64,
    /// `tick` of the last [`MfaSessions::clear`].
    cleared_at: u64,
    /// `tick` of the last [`MfaSessions::remove`], per account.
    removed_at: HashMap<String, u64>,
}

struct Stored {
    session: MfaSession,
    binding: Binding,
}

impl State {
    /// Whether nothing a login of `account_id` depends on was forgotten since
    /// `generation`.
    fn unchanged_since(&self, generation: u64, account_id: &str) -> bool {
        self.cleared_at <= generation
            && self
                .removed_at
                .get(account_id)
                .is_none_or(|removed| *removed <= generation)
    }
}

impl MfaSessions {
    /// Sessions measured with the system clock and [`DEFAULT_EXPIRY_MARGIN`].
    pub fn new() -> Self {
        Self::with_clock(SystemTimeSource::new(), DEFAULT_EXPIRY_MARGIN)
    }

    /// Sessions measured with `clock`, dropped `margin` before they expire.
    pub fn with_clock(clock: impl TimeSource + 'static, margin: Duration) -> Self {
        Self {
            state: Mutex::new(State::default()),
            clock: SharedTimeSource::new(clock),
            margin,
        }
    }

    /// A marker of "now" for [`MfaSessions::insert_since`]: read it before a
    /// login starts (before the account's credentials are read), and the
    /// session is stored only if [`MfaSessions::clear`] was not called, and the
    /// account's session not removed, in between.
    pub fn generation(&self) -> u64 {
        self.state().tick
    }

    /// Stores the session of `account`, replacing any previous one, bound to
    /// the account's current state. Returns when it stops being used. Fails
    /// with [`ErrorKind::Remote`](crate::ErrorKind::Remote), storing nothing, when it is already within the
    /// margin of its expiry (usually a wrong system clock), and with
    /// [`ErrorKind::Conflict`](crate::ErrorKind::Conflict) when it was issued for a different state of the
    /// account (see [`request_session`]).
    pub fn insert(&self, account: &Account, session: MfaSession) -> Result<SystemTime> {
        self.insert_since(account, self.generation(), session)
    }

    /// Like [`MfaSessions::insert`], but fails with
    /// [`ErrorKind::Conflict`](crate::ErrorKind::Conflict), storing nothing, when sessions were cleared, or
    /// this account's session removed, since `generation` was read: the login
    /// that produced the session was overtaken by a lock, an account change or
    /// the removal of the account.
    pub fn insert_since(
        &self,
        account: &Account,
        generation: u64,
        session: MfaSession,
    ) -> Result<SystemTime> {
        let binding = Binding::of(account);
        if session
            .binding
            .as_ref()
            .is_some_and(|issued_for| *issued_for != binding)
        {
            return Err(account_changed());
        }
        let now = self.clock.now();
        let until = match self.usable_until(&session) {
            Some(until) if now < until => until,
            _ => return Err(session_already_expired()),
        };
        let mut state = self.state();
        if !state.unchanged_since(generation, &account.id) {
            return Err(login_interrupted());
        }
        state
            .sessions
            .insert(account.id.clone(), Stored { session, binding });
        Ok(until)
    }

    /// The credentials of the account's session.
    ///
    /// Fails with [`ErrorKind::MfaRequired`](crate::ErrorKind::MfaRequired) when there is no session, when it
    /// was issued for another state of the account (its keys, endpoint or MFA
    /// device changed) or when it is within the margin of its expiry; such a
    /// session is removed.
    pub fn credentials(&self, account: &Account) -> Result<Credentials> {
        self.active(account).map(|(credentials, _)| credentials)
    }

    /// The credentials of the account's session and when it stops being used,
    /// read together (see [`MfaSessions::credentials`]).
    pub fn active(&self, account: &Account) -> Result<(Credentials, SystemTime)> {
        let now = self.clock.now();
        let mut state = self.state();
        let Some(stored) = state.sessions.get(&account.id) else {
            return Err(mfa_required());
        };
        if stored.binding != Binding::of(account) {
            state.sessions.remove(&account.id);
            return Err(mfa_session_stale());
        }
        match self.usable_until(&stored.session) {
            Some(until) if now < until => Ok((stored.session.credentials.clone(), until)),
            _ => {
                state.sessions.remove(&account.id);
                Err(mfa_session_expired())
            }
        }
    }

    /// When the account's session stops being used, if it is still usable.
    /// Looks at the id only: whether the session still matches the account is
    /// decided by [`MfaSessions::credentials`].
    pub fn usable_until_for(&self, account_id: &str) -> Option<SystemTime> {
        let now = self.clock.now();
        self.state()
            .sessions
            .get(account_id)
            .and_then(|stored| self.usable_until(&stored.session))
            .filter(|until| now < *until)
    }

    /// Forgets the account's session (account removed, keys rotated, MFA
    /// device changed) and makes a login of this account that is waiting for
    /// STS fail instead of storing its session.
    pub fn remove(&self, account_id: &str) {
        let mut state = self.state();
        state.tick += 1;
        let tick = state.tick;
        state.removed_at.insert(account_id.to_owned(), tick);
        state.sessions.remove(account_id);
    }

    /// Forgets every session, and makes every login that is waiting for STS
    /// fail instead of storing its session.
    pub fn clear(&self) {
        let mut state = self.state();
        state.tick += 1;
        state.cleared_at = state.tick;
        state.removed_at.clear();
        state.sessions.clear();
    }

    fn usable_until(&self, session: &MfaSession) -> Option<SystemTime> {
        session.expires_at.checked_sub(self.margin)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Critical sections leave the maps consistent at every step: never
        // inconsistent, even after a panic.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for MfaSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MfaSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MfaSessions")
            .field("sessions", &self.state().sessions.len())
            .field("margin", &self.margin)
            .finish_non_exhaustive()
    }
}

fn session_from_output(output: GetSessionTokenOutput) -> Result<MfaSession> {
    let issued = output.credentials.ok_or_else(no_credentials_returned)?;
    let expires_at = SystemTime::try_from(issued.expiration).map_err(|_| invalid_expiration())?;
    // Move the strings out instead of copying them.
    let credentials = Credentials {
        access_key_id: issued.access_key_id,
        secret_access_key: issued.secret_access_key,
        session_token: Some(issued.session_token),
    };
    Ok(MfaSession::new(credentials, expires_at))
}

fn check_code(code: &str) -> Result<()> {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(invalid_code())
    }
}

fn check_duration(duration: Duration) -> Result<i32> {
    let whole_seconds = duration.subsec_nanos() == 0;
    if !whole_seconds || !(MIN_SESSION_DURATION..=MAX_SESSION_DURATION).contains(&duration) {
        return Err(invalid_duration());
    }
    i32::try_from(duration.as_secs()).map_err(|_| invalid_duration())
}

fn check_aws_region(region: &str) -> Result<()> {
    let valid = !region.is_empty()
        && region.len() <= 63
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !region.starts_with('-')
        && !region.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(invalid_region(region))
    }
}

// Error constructors of this module.

fn mfa_required() -> Error {
    Error::MfaRequired(
        "this account requires MFA: enter the current code of its MFA device".to_owned(),
    )
}

fn mfa_session_expired() -> Error {
    Error::MfaRequired("the MFA session of this account has expired: enter a new code".to_owned())
}

fn mfa_session_stale() -> Error {
    Error::MfaRequired(
        "the MFA session no longer matches this account (its keys, endpoint or MFA device \
         changed): enter a new code"
            .to_owned(),
    )
}

fn account_changed() -> Error {
    Error::Conflict(
        "the account changed while the MFA login was in progress: enter a new code".to_owned(),
    )
}

fn login_interrupted() -> Error {
    Error::Conflict(
        "the secrets were locked, or the account was changed or removed, while the MFA login \
         was in progress: enter a new code"
            .to_owned(),
    )
}

fn session_already_expired() -> Error {
    Error::Remote(
        "STS returned a session that has already expired: check the system clock".to_owned(),
    )
}

fn no_mfa_device() -> Error {
    Error::Usage("this account has no MFA device: set its MFA serial first".to_owned())
}

fn no_credentials_returned() -> Error {
    Error::Remote("STS returned no credentials".to_owned())
}

fn invalid_expiration() -> Error {
    Error::Remote("STS returned credentials with an invalid expiry".to_owned())
}

fn invalid_code() -> Error {
    Error::Usage("the MFA code must be the 6 digits shown by the MFA device".to_owned())
}

fn invalid_serial() -> Error {
    Error::Usage(
        "the MFA serial must be a device serial number or ARN of 9 to 256 characters".to_owned(),
    )
}

fn invalid_duration() -> Error {
    Error::Usage("the MFA session duration must be whole seconds from 900 to 129600".to_owned())
}

fn invalid_region(region: &str) -> Error {
    Error::Usage(format!("{region:?} is not a valid AWS region for STS"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use aws_sdk_sts::operation::get_session_token::GetSessionTokenOutput;
    use aws_sdk_sts::types::Credentials as StsCredentials;
    use aws_smithy_mocks::{
        MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock,
    };
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::DateTime;
    use aws_smithy_types::body::SdkBody;

    use super::*;
    use crate::error::ErrorKind;
    use aws_smithy_runtime_api::client::orchestrator::HttpResponse;

    const SERIAL: &str = "arn:aws:iam::123456789012:mfa/example-device";
    const T0: u64 = 1_800_000_000;
    const ACCESS_DENIED: &str = r#"<ErrorResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <Error>
    <Type>Sender</Type>
    <Code>AccessDenied</Code>
    <Message>MultiFactorAuthentication failed with invalid MFA one time pass code. </Message>
  </Error>
  <RequestId>4f2a7d0e-0000-4000-8000-000000000001</RequestId>
</ErrorResponse>"#;

    /// A clock the test moves by hand.
    #[derive(Debug, Clone)]
    struct ManualClock(Arc<Mutex<SystemTime>>);

    impl ManualClock {
        fn at(secs: u64) -> Self {
            Self(Arc::new(Mutex::new(
                SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            )))
        }

        fn set(&self, secs: u64) {
            *self.0.lock().unwrap() = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        }
    }

    impl TimeSource for ManualClock {
        fn now(&self) -> SystemTime {
            *self.0.lock().unwrap()
        }
    }

    fn account(id: &str, region: &str, endpoint: Option<&str>) -> Account {
        Account {
            id: id.to_owned(),
            label: format!("label {id}"),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_ref: format!("{id}/secret/1"),
            session_token_ref: None,
            region: region.to_owned(),
            endpoint: endpoint.map(str::to_owned),
            path_style: false,
            request_payer: false,
            mfa_serial: Some(SERIAL.to_owned()),
        }
    }

    fn long_term() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: None,
        }
    }

    fn issued(suffix: &str, expires: u64) -> GetSessionTokenOutput {
        GetSessionTokenOutput::builder()
            .credentials(
                StsCredentials::builder()
                    .access_key_id(format!("ASIATEMPORARY{suffix}"))
                    .secret_access_key(format!("temporary-secret-{suffix}"))
                    .session_token(format!("temporary-token-{suffix}"))
                    .expiration(DateTime::from_secs(expires as i64))
                    .build()
                    .unwrap(),
            )
            .build()
    }

    fn mock_client(account: &Account, rules: &[&Rule]) -> aws_sdk_sts::Client {
        let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::Sequential);
        for rule in rules {
            interceptor = interceptor.with_rule(rule);
        }
        let config = sts_config(account, &long_term())
            .unwrap()
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
            .build();
        aws_sdk_sts::Client::from_conf(config)
    }

    fn success_rule(code: &'static str, suffix: &'static str, expires: u64) -> Rule {
        mock!(aws_sdk_sts::Client::get_session_token)
            .match_requests(move |input| {
                input.serial_number() == Some(SERIAL)
                    && input.token_code() == Some(code)
                    && input.duration_seconds().is_none()
            })
            .then_output(move || issued(suffix, expires))
    }

    #[tokio::test]
    async fn login_stores_a_session() {
        let clock = ManualClock::at(T0);
        let sessions = MfaSessions::with_clock(clock.clone(), DEFAULT_EXPIRY_MARGIN);
        let account = account("account-a", "eu-west-1", None);
        let rule = success_rule("123456", "A", T0 + 3600);
        let client = mock_client(&account, &[&rule]);

        assert_eq!(
            sessions.credentials(&account).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
        let usable_until = login(&sessions, &account, &client, "123456", None)
            .await
            .unwrap();
        assert_eq!(rule.num_calls(), 1);
        assert_eq!(
            usable_until,
            SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + 3600) - DEFAULT_EXPIRY_MARGIN
        );
        let credentials = sessions.credentials(&account).unwrap();
        assert_eq!(credentials.access_key_id, "ASIATEMPORARYA");
        assert_eq!(credentials.secret_access_key, "temporary-secret-A");
        assert_eq!(
            credentials.session_token.as_deref(),
            Some("temporary-token-A")
        );
        assert_eq!(sessions.usable_until_for(&account.id), Some(usable_until));
        let debug = format!("{sessions:?} {credentials:?}");
        assert!(!debug.contains("temporary-secret"));
        assert!(!debug.contains("temporary-token"));
    }

    #[tokio::test]
    async fn invalid_code_is_an_auth_error() {
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        let account = account("account-a", "us-east-1", None);
        let rule = mock!(aws_sdk_sts::Client::get_session_token).then_http_response(|| {
            HttpResponse::new(
                StatusCode::try_from(403).unwrap(),
                SdkBody::from(ACCESS_DENIED),
            )
        });
        let client = mock_client(&account, &[&rule]);
        let error = login(&sessions, &account, &client, "000000", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Auth);
        let message = error.to_string();
        assert!(message.contains("MultiFactorAuthentication"), "{message}");
        assert!(message.starts_with("STS GetSessionToken"), "{message}");
        assert_eq!(rule.num_calls(), 1);
        assert_eq!(
            sessions.credentials(&account).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
    }

    #[tokio::test]
    async fn other_sts_errors() {
        let account = account("account-a", "us-east-1", None);
        let cases = [
            (403_u16, "InvalidClientTokenId", ErrorKind::Auth),
            (403, "SignatureDoesNotMatch", ErrorKind::Auth),
            (400, "Throttling", ErrorKind::Remote),
            (500, "InternalFailure", ErrorKind::Remote),
        ];
        for (status, code, expected) in cases {
            let body = format!(
                "<ErrorResponse><Error><Type>Sender</Type><Code>{code}</Code>\
                 <Message>example</Message></Error><RequestId>r</RequestId></ErrorResponse>"
            );
            let rule =
                mock!(aws_sdk_sts::Client::get_session_token).then_http_response(move || {
                    HttpResponse::new(
                        StatusCode::try_from(status).unwrap(),
                        SdkBody::from(body.clone()),
                    )
                });
            let client = mock_client(&account, &[&rule]);
            let error = get_session_token(&client, SERIAL, "123456", None)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), expected, "{code}");
            assert_eq!(rule.num_calls(), 1, "{code}: no retries");
        }
    }

    #[tokio::test]
    async fn expired_session_requires_mfa_again() {
        let clock = ManualClock::at(T0);
        let sessions = MfaSessions::with_clock(clock.clone(), DEFAULT_EXPIRY_MARGIN);
        let account = account("account-a", "us-east-1", None);
        let first = success_rule("123456", "A", T0 + 3600);
        let second = success_rule("654321", "B", T0 + 7200);
        let client = mock_client(&account, &[&first, &second]);
        login(&sessions, &account, &client, "123456", None)
            .await
            .unwrap();

        let margin = DEFAULT_EXPIRY_MARGIN.as_secs();
        clock.set(T0 + 3600 - margin - 1);
        assert!(sessions.credentials(&account).is_ok());
        clock.set(T0 + 3600 - margin);
        let error = sessions.credentials(&account).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MfaRequired);
        assert!(error.to_string().contains("expired"));
        // The expired session was dropped; going back in time does not revive it.
        clock.set(T0);
        assert_eq!(
            sessions.credentials(&account).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
        assert_eq!(sessions.usable_until_for(&account.id), None);

        // A new code gives a new session.
        login(&sessions, &account, &client, "654321", None)
            .await
            .unwrap();
        assert_eq!(
            sessions.credentials(&account).unwrap().access_key_id,
            "ASIATEMPORARYB"
        );
    }

    #[tokio::test]
    async fn two_accounts_have_independent_sessions() {
        let clock = ManualClock::at(T0);
        let sessions = MfaSessions::with_clock(clock.clone(), DEFAULT_EXPIRY_MARGIN);
        let a = account("account-a", "us-east-1", None);
        let b = account("account-b", "eu-central-1", Some("https://s3.example.test"));
        let rule_a = success_rule("111111", "A", T0 + 3600);
        let rule_b = success_rule("222222", "B", T0 + 7200);
        let client_a = mock_client(&a, &[&rule_a]);
        let client_b = mock_client(&b, &[&rule_b]);
        let (result_a, result_b) = tokio::join!(
            login(&sessions, &a, &client_a, "111111", None),
            login(&sessions, &b, &client_b, "222222", None)
        );
        result_a.unwrap();
        result_b.unwrap();
        assert_eq!(
            sessions.credentials(&a).unwrap().access_key_id,
            "ASIATEMPORARYA"
        );
        assert_eq!(
            sessions.credentials(&b).unwrap().access_key_id,
            "ASIATEMPORARYB"
        );

        clock.set(T0 + 3600);
        assert_eq!(
            sessions.credentials(&a).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
        assert!(sessions.credentials(&b).is_ok());
        sessions.remove(&b.id);
        assert_eq!(
            sessions.credentials(&b).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
    }

    #[tokio::test]
    async fn malformed_input_never_reaches_sts() {
        let account_with_mfa = account("account-a", "us-east-1", None);
        let rule = success_rule("123456", "A", T0 + 3600);
        let client = mock_client(&account_with_mfa, &[&rule]);
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        for code in ["", "12345", "1234567", "12345a", " 123456", "\u{663}23456"] {
            let error = login(&sessions, &account_with_mfa, &client, code, None)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{code:?}");
        }
        for serial in ["short", "has space in it", &"a".repeat(257)] {
            let error = get_session_token(&client, serial, "123456", None)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{serial:?}");
        }
        for duration in [
            Duration::from_secs(899),
            Duration::from_secs(129_601),
            Duration::from_millis(900_500),
        ] {
            let error = get_session_token(&client, SERIAL, "123456", Some(duration))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{duration:?}");
        }
        let mut no_mfa = account_with_mfa.clone();
        no_mfa.mfa_serial = None;
        let error = login(&sessions, &no_mfa, &client, "123456", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(rule.num_calls(), 0);
    }

    #[tokio::test]
    async fn requested_duration_is_sent() {
        let account = account("account-a", "us-east-1", None);
        let rule = mock!(aws_sdk_sts::Client::get_session_token)
            .match_requests(|input| input.duration_seconds() == Some(3600))
            .then_output(|| issued("A", T0 + 3600));
        let client = mock_client(&account, &[&rule]);
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        login(
            &sessions,
            &account,
            &client,
            "123456",
            Some(Duration::from_secs(3600)),
        )
        .await
        .unwrap();
        assert_eq!(rule.num_calls(), 1);
    }

    #[tokio::test]
    async fn already_expired_session_is_not_stored() {
        // The local clock is far ahead of STS.
        let sessions = MfaSessions::with_clock(ManualClock::at(T0 + 86_400), DEFAULT_EXPIRY_MARGIN);
        let account = account("account-a", "us-east-1", None);
        let rule = success_rule("123456", "A", T0 + 3600);
        let client = mock_client(&account, &[&rule]);
        let error = login(&sessions, &account, &client, "123456", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Remote);
        assert!(error.to_string().contains("clock"));
        assert_eq!(
            sessions.credentials(&account).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
    }

    #[test]
    fn endpoint_decision() {
        assert_eq!(
            sts_endpoint(&account("a", "eu-west-1", None)).unwrap(),
            StsEndpoint::Aws {
                region: "eu-west-1".to_owned()
            }
        );
        assert_eq!(
            sts_endpoint(&account("a", "eu-west-1", Some("https://s3.example.test/"))).unwrap(),
            StsEndpoint::Custom {
                url: "https://s3.example.test".to_owned(),
                region: "eu-west-1".to_owned()
            }
        );
        assert_eq!(
            sts_endpoint(&account("a", "garage", Some("http://127.0.0.1:3900"))).unwrap(),
            StsEndpoint::Custom {
                url: "http://127.0.0.1:3900".to_owned(),
                region: "garage".to_owned()
            }
        );
        for (region, endpoint) in [
            ("", None),
            ("EU-WEST-1", None),
            ("eu west", None),
            ("-bad", None),
            ("us-east-1", Some("ftp://example.test")),
            ("us-east-1", Some("not a url")),
            ("us-east-1", Some("https://user:pass@example.test")),
            ("us-east-1", Some("")),
            ("", Some("https://example.test")),
        ] {
            let error = sts_endpoint(&account("a", region, endpoint)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{region:?} {endpoint:?}");
        }
    }

    /// Sends one GetSessionToken through a capturing HTTP client and returns
    /// the request the SDK produced.
    async fn captured_request(
        account: &Account,
    ) -> aws_smithy_runtime_api::client::orchestrator::HttpRequest {
        let (http_client, receiver) = aws_smithy_http_client::test_util::capture_request(None);
        let client = sts_client(
            account,
            &long_term(),
            Some(SharedHttpClient::new(http_client)),
        )
        .unwrap();
        // The canned empty response does not parse; only the request matters.
        let _ = get_session_token(&client, SERIAL, "123456", None).await;
        receiver.expect_request()
    }

    fn body_text(request: &aws_smithy_runtime_api::client::orchestrator::HttpRequest) -> String {
        String::from_utf8(request.body().bytes().unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn aws_accounts_use_the_regional_sts_endpoint() {
        let request = captured_request(&account("a", "eu-west-1", None)).await;
        assert_eq!(request.uri(), "https://sts.eu-west-1.amazonaws.com/");
        let authorization = request.headers().get("authorization").unwrap();
        assert!(authorization.contains("Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(authorization.contains("/eu-west-1/sts/aws4_request"));
        let body = body_text(&request);
        assert!(body.contains("Action=GetSessionToken"));
        assert!(body.contains("TokenCode=123456"));
        assert!(body.contains("SerialNumber=arn"));
        assert!(body.contains("example-device"));
        assert!(!body.contains("wJalr"));
    }

    #[tokio::test]
    async fn custom_endpoint_accounts_never_call_aws() {
        let request = captured_request(&account(
            "a",
            "us-east-1",
            Some("https://s3.example.test:9000"),
        ))
        .await;
        assert_eq!(request.uri(), "https://s3.example.test:9000/");
        let authorization = request.headers().get("authorization").unwrap();
        assert!(authorization.contains("/us-east-1/sts/aws4_request"));
        assert!(body_text(&request).contains("Action=GetSessionToken"));
    }

    /// A change to an account, for the binding tests.
    type Change = Box<dyn Fn(&mut Account)>;

    /// The account as it is stored, for the binding tests.
    fn stored_account() -> Account {
        let mut account = account("account-a", "eu-west-1", Some("https://s3.example.test"));
        account.session_token_ref = Some("account-a/session-token/1".to_owned());
        account
    }

    #[tokio::test]
    async fn a_session_is_used_only_for_the_account_state_it_was_issued_for() {
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        let original = stored_account();
        let rule = success_rule("123456", "A", T0 + 3600);
        let client = mock_client(&original, &[&rule]);
        login(&sessions, &original, &client, "123456", None)
            .await
            .unwrap();
        assert!(sessions.credentials(&original).is_ok());

        // Everything that decides whose credentials STS signed for, and where
        // they go, invalidates the session; each change drops it.
        let changes: Vec<(&str, Change)> = vec![
            (
                "endpoint",
                Box::new(|a| a.endpoint = Some("https://s3.other.test".to_owned())),
            ),
            ("no endpoint", Box::new(|a| a.endpoint = None)),
            (
                "access key",
                Box::new(|a| a.access_key_id = "AKIAROTATEDEXAMPLE01".to_owned()),
            ),
            (
                "secret",
                Box::new(|a| a.secret_ref = "account-a/secret/2".to_owned()),
            ),
            (
                "token",
                Box::new(|a| a.session_token_ref = Some("account-a/session-token/2".to_owned())),
            ),
            ("token removed", Box::new(|a| a.session_token_ref = None)),
            (
                "serial",
                Box::new(|a| {
                    a.mfa_serial = Some("arn:aws:iam::123456789012:mfa/new-device".to_owned())
                }),
            ),
            ("serial removed", Box::new(|a| a.mfa_serial = None)),
        ];
        for (what, change) in &changes {
            login(&sessions, &original, &client_for(&original), "123456", None)
                .await
                .unwrap();
            let mut changed = original.clone();
            change(&mut changed);
            let error = sessions.credentials(&changed).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::MfaRequired, "{what}");
            assert!(
                error.to_string().contains("no longer matches"),
                "{what}: {error}"
            );
            // The mismatch dropped it for good, also for the original account.
            assert_eq!(sessions.usable_until_for(&original.id), None, "{what}");
            assert_eq!(
                sessions.credentials(&original).unwrap_err().kind(),
                ErrorKind::MfaRequired,
                "{what}"
            );
        }

        // What does not matter for the credentials does not drop the session.
        login(&sessions, &original, &client_for(&original), "123456", None)
            .await
            .unwrap();
        let mut renamed = original.clone();
        renamed.label = "other label".to_owned();
        renamed.region = "eu-central-1".to_owned();
        renamed.path_style = true;
        renamed.request_payer = true;
        assert!(sessions.credentials(&renamed).is_ok());
    }

    /// A client that answers `GetSessionToken` for any code, for tests that
    /// log in several times.
    fn client_for(account: &Account) -> aws_sdk_sts::Client {
        let rule = mock!(aws_sdk_sts::Client::get_session_token)
            .match_requests(|_| true)
            .then_output(|| issued("A", T0 + 3600));
        let interceptor = MockResponseInterceptor::new()
            .rule_mode(RuleMode::MatchAny)
            .with_rule(&rule);
        let config = sts_config(account, &long_term())
            .unwrap()
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
            .build();
        aws_sdk_sts::Client::from_conf(config)
    }

    #[test]
    fn a_session_issued_for_another_account_state_is_not_stored() {
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        let original = stored_account();
        let session = || {
            MfaSession::new(
                Credentials::new(
                    "ASIATEMPORARYA",
                    "temporary-secret-A",
                    Some("temporary-token-A".to_owned()),
                ),
                SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + 3600),
            )
        };
        let issued = session().issued_for(&original);
        let mut moved = original.clone();
        moved.endpoint = Some("https://s3.other.test".to_owned());
        assert_eq!(
            sessions.insert(&moved, issued.clone()).unwrap_err().kind(),
            ErrorKind::Conflict
        );
        assert_eq!(sessions.usable_until_for(&original.id), None);
        sessions.insert(&original, issued).unwrap();
        // A session built by hand takes the account it is inserted for.
        sessions.insert(&moved, session()).unwrap();
        assert!(sessions.credentials(&moved).is_ok());
        assert_eq!(
            sessions.credentials(&original).unwrap_err().kind(),
            ErrorKind::MfaRequired
        );
    }

    #[tokio::test]
    async fn a_login_overtaken_by_a_clear_or_a_removal_is_not_stored() {
        let sessions = MfaSessions::with_clock(ManualClock::at(T0), DEFAULT_EXPIRY_MARGIN);
        let a = account("account-a", "us-east-1", None);
        let b = account("account-b", "us-east-1", None);
        let issue = |account: &Account| {
            MfaSession::new(
                Credentials::new(
                    "ASIATEMPORARYA",
                    "temporary-secret-A",
                    Some("temporary-token-A".to_owned()),
                ),
                SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + 3600),
            )
            .issued_for(account)
        };

        // Cleared while the login waits for STS (a lock).
        let generation = sessions.generation();
        sessions.clear();
        let error = sessions
            .insert_since(&a, generation, issue(&a))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
        assert_eq!(sessions.usable_until_for(&a.id), None);

        // Removed while it waits (a rotation, a new MFA device, a removal).
        let generation = sessions.generation();
        sessions.remove(&a.id);
        assert_eq!(
            sessions
                .insert_since(&a, generation, issue(&a))
                .unwrap_err()
                .kind(),
            ErrorKind::Conflict
        );
        // Another account's removal does not matter.
        let generation = sessions.generation();
        sessions.remove(&b.id);
        sessions.insert_since(&a, generation, issue(&a)).unwrap();
        assert!(sessions.credentials(&a).is_ok());

        // A login that started after the removal is fine, also for the removed one.
        let generation = sessions.generation();
        sessions.insert_since(&b, generation, issue(&b)).unwrap();
        assert!(sessions.credentials(&b).is_ok());
        // Clearing makes every earlier ticket stale, whatever happened before.
        let generation = sessions.generation();
        sessions.remove(&a.id);
        sessions.clear();
        for account in [&a, &b] {
            assert_eq!(
                sessions
                    .insert_since(account, generation, issue(account))
                    .unwrap_err()
                    .kind(),
                ErrorKind::Conflict
            );
        }
        sessions.insert(&a, issue(&a)).unwrap();
    }

    #[tokio::test]
    async fn login_stores_nothing_when_the_sessions_were_cleared_meanwhile() {
        let sessions = Arc::new(MfaSessions::with_clock(
            ManualClock::at(T0),
            DEFAULT_EXPIRY_MARGIN,
        ));
        let account = account("account-a", "us-east-1", None);
        let rule = success_rule("123456", "A", T0 + 3600);
        let client = mock_client(&account, &[&rule]);
        // The first thing `login` does is read the generation: clearing after
        // it, while STS is being called, must win. Emulate the order with the
        // building blocks `login` is made of.
        let generation = sessions.generation();
        let session = request_session(&account, &client, "123456", None)
            .await
            .unwrap();
        sessions.clear();
        assert_eq!(
            sessions
                .insert_since(&account, generation, session)
                .unwrap_err()
                .kind(),
            ErrorKind::Conflict
        );
        assert_eq!(rule.num_calls(), 1);
        assert_eq!(sessions.usable_until_for(&account.id), None);
    }

    #[test]
    fn handles_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MfaSessions>();
        assert_send_sync::<MfaSession>();
    }
}
