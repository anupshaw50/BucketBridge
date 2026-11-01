//! Diagnostics (task T12): probes an account step by step and reports, for
//! each step, how long it took, whether it passed, failed or was skipped, and
//! why.
//!
//! The steps run in this order ([`StepId`]):
//!
//! 1. `dns`: resolves the host name of the endpoint;
//! 2. `tcp`: opens a TCP connection, trying every address in turn;
//! 3. `proxy`: asks the proxy to open a tunnel to the endpoint (HTTP
//!    `CONNECT` or SOCKS5 `CONNECT`), when a proxy carries the traffic;
//! 4. `tls`: the TLS handshake (rustls, with the OS trust roots like the real
//!    client), then the certificate's subject, issuer and expiry;
//! 5. `credentials`: one signed `ListBuckets` request that tells an invalid
//!    access key, a wrong secret, a clock that is off and a key that may not
//!    list buckets apart;
//! 6. `region`: `HeadBucket` on a named bucket and its `x-amz-bucket-region`
//!    header.
//!
//! When a proxy carries the traffic (the app's proxy settings, or the proxy
//! variables of the environment while the settings are disabled, unless the
//! endpoint is on the bypass list) `dns` and `tcp` probe the proxy host,
//! `proxy` the tunnel to the endpoint, and `tls` runs inside that tunnel, so a
//! TLS-inspecting proxy shows its own certificate. A failed network step
//! (`dns`, `tcp`, `proxy`, `tls`) skips the steps after it; a `credentials`
//! step that got no answer at all skips `region`. Every step has a stable
//! `code` for scripts and tests; the values are documented in
//! `docs/desktop-api.md`.
//!
//! The signed requests go through the [`S3`] handle that is given, so they
//! use the account's credentials, proxy-aware HTTP client and endpoint, one
//! attempt each and a bounded time ([`Timeouts`]). A report never holds a
//! secret: not the secret key, a session token, the proxy password or the user
//! information of an endpoint URL.
//!
//! [`probe_account`] and [`probe_input`] run the probe for a stored account
//! and for an unsaved [`AccountInput`](crate::model::AccountInput) (the
//! add-account wizard); [`run`] is the engine behind both.

use std::fmt::Write as _;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::head_bucket::HeadBucketError;
use aws_sdk_s3::operation::list_buckets::ListBucketsError;
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::interceptors::Intercept;
use aws_smithy_runtime_api::client::interceptors::context::BeforeDeserializationInterceptorContextRef;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::config_bag::ConfigBag;
use aws_smithy_types::date_time::Format as HttpDateFormat;
use chrono::{DateTime, NaiveDate, Utc};
use hyper_util::client::proxy::matcher::Matcher;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use zeroize::Zeroizing;

use crate::client::{ClientOptions, Credentials, redact_endpoint};
use crate::error::{Error, Result};
use crate::model::{Account, AccountInput};
use crate::ops::S3;
use crate::proxy::{
    ProxyAuth, ProxyPlan, SocksError, basic_auth_value, probe_crypto_provider, probe_tls_config,
    socks5_connect,
};
use crate::s3uri::validate_bucket_name;
use crate::secrets::SecretString;
use crate::session::Session;
use crate::settings::{ProxyKind, ProxySettings, Settings};

/// How long a signed request may take at most, whatever the read timeout
/// setting says: a diagnosis should not make the user wait a minute.
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest answer of a proxy to `CONNECT` that is read.
const MAX_PROXY_HEAD: usize = 16 * 1024;

/// A certificate that expires within this many days is called out.
const EXPIRY_WARNING_DAYS: i64 = 14;

/// Most addresses listed in a step's detail.
const MAX_LISTED_ADDRESSES: usize = 8;

/// Header the server stamps its answers with.
const DATE_HEADER: &str = "date";

/// Response header naming a bucket's region.
const BUCKET_REGION_HEADER: &str = "x-amz-bucket-region";

/// A step of the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    /// Resolving the host name.
    Dns,
    /// Opening the TCP connection.
    Tcp,
    /// Opening a tunnel through the proxy.
    Proxy,
    /// The TLS handshake and the certificate.
    Tls,
    /// A signed request with the account's credentials.
    Credentials,
    /// The region of a named bucket.
    Region,
}

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step succeeded.
    Pass,
    /// The step found a problem.
    Fail,
    /// The step did not run, or had nothing to check; the summary says why.
    Skip,
}

/// The result of one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Which step.
    pub id: StepId,
    /// How it ended.
    pub status: StepStatus,
    /// Wall-clock milliseconds the step took; 0 for a skipped step.
    pub elapsed_ms: u64,
    /// One sentence: what was checked and how it ended.
    pub summary: String,
    /// Addresses, certificate, clock offset, a hint.
    pub detail: Option<String>,
    /// Stable short identifier of the outcome (`resolved`, `refused`,
    /// `untrusted_issuer`, `clock_skew`, ...).
    pub code: String,
}

/// The result of a probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The endpoint that was probed (user information hidden).
    pub target: String,
    /// The signing region of the account.
    pub region: String,
    /// The bucket of the region probe.
    pub bucket: Option<String>,
    /// When the probe started.
    pub started_at: DateTime<Utc>,
    /// Wall-clock milliseconds the whole probe took.
    pub elapsed_ms: u64,
    /// No step failed (skipped steps do not count).
    pub ok: bool,
    /// All six steps, in the order of [`StepId`].
    pub steps: Vec<Step>,
}

impl Report {
    /// The step with the given id.
    pub fn step(&self, id: StepId) -> Option<&Step> {
        self.steps.iter().find(|step| step.id == id)
    }
}

/// How long each part of the probe may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Each of: the host name lookup, the TCP connection, the tunnel and the
    /// TLS handshake.
    pub connect: Duration,
    /// Each signed request.
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            request: MAX_REQUEST_TIMEOUT,
        }
    }
}

impl Timeouts {
    /// The connect timeout of the settings; the read timeout of the
    /// settings, at most 15 seconds, for the signed requests.
    pub fn from_settings(settings: &Settings) -> Self {
        Timeouts {
            connect: Duration::from_secs(settings.advanced.connect_timeout_secs),
            request: Duration::from_secs(settings.advanced.read_timeout_secs)
                .min(MAX_REQUEST_TIMEOUT),
        }
    }
}

/// How the traffic reaches the endpoint: the proxy settings, the proxy
/// variables of the environment and the roots the TLS handshake trusts.
#[derive(Default)]
pub struct Network {
    /// The app's proxy settings.
    pub proxy: ProxySettings,
    /// The proxy password the settings use, if any.
    pub proxy_password: Option<SecretString>,
    /// The proxy variables of the environment (`Matcher::from_env()`), which
    /// apply while the settings are disabled; `None` ignores them.
    pub environment_proxy: Option<Matcher>,
    /// Certificates (PEM) trusted in addition to the OS roots, for example
    /// the private CA of a test server.
    pub extra_roots_pem: Vec<Vec<u8>>,
}

impl Network {
    /// The network of the app: its proxy settings and password, and the
    /// proxy variables of the environment when `environment` is set.
    pub fn from_settings(
        settings: &Settings,
        proxy_password: Option<SecretString>,
        environment: bool,
    ) -> Self {
        Network {
            proxy: settings.proxy.clone(),
            proxy_password,
            environment_proxy: environment.then(Matcher::from_env),
            extra_roots_pem: Vec::new(),
        }
    }
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Network")
            .field("proxy_enabled", &self.proxy.enabled)
            .field("environment_proxy", &self.environment_proxy.is_some())
            .field("extra_roots", &self.extra_roots_pem.len())
            .finish_non_exhaustive()
    }
}

/// Probes a stored account. Fails like [`Session::s3_for`] (`not_found`,
/// `locked`, `mfa_required`) and with `usage` for an invalid bucket name; a
/// problem with the account or the network is a result in the report.
pub async fn probe_account(
    session: &Session,
    account_id: &str,
    bucket: Option<&str>,
) -> Result<Report> {
    if let Some(bucket) = bucket {
        validate_bucket_name(bucket)?;
    }
    let s3 = session.s3_for(account_id).await?;
    let account = session
        .list_accounts()
        .await?
        .accounts
        .into_iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| Error::NotFound(format!("account {account_id} does not exist")))?;
    let settings = session.settings().await?;
    let (_, password) = session.proxy_credentials().await?;
    let network = Network::from_settings(&settings, password, session.environment_proxy());
    run(
        &s3,
        &account,
        bucket,
        &network,
        Timeouts::from_settings(&settings),
    )
    .await
}

/// Probes an account that is not stored (the add-account wizard): nothing is
/// saved, the label and the MFA device are ignored. Fails with `usage` for an
/// invalid region, endpoint, bucket name or empty keys.
pub async fn probe_input(
    session: &Session,
    input: &AccountInput,
    bucket: Option<&str>,
) -> Result<Report> {
    if let Some(bucket) = bucket {
        validate_bucket_name(bucket)?;
    }
    let account = Account {
        id: "unsaved".to_owned(),
        label: input.label.clone(),
        access_key_id: input.access_key_id.clone(),
        secret_ref: String::new(),
        session_token_ref: None,
        region: input.region.clone(),
        endpoint: input.endpoint.clone(),
        path_style: input.path_style,
        request_payer: input.request_payer,
        mfa_serial: None,
    };
    let credentials = Credentials::new(
        input.access_key_id.clone(),
        input.secret_access_key.clone(),
        input.session_token.clone(),
    );
    let settings = session.settings().await?;
    let mut options = ClientOptions::from_settings(&settings);
    options.http_client = Some(session.http_client().await?);
    let s3 = S3::new(&account, &credentials, &options)?;
    drop(credentials);
    let (_, password) = session.proxy_credentials().await?;
    let network = Network::from_settings(&settings, password, session.environment_proxy());
    run(
        &s3,
        &account,
        bucket,
        &network,
        Timeouts::from_settings(&settings),
    )
    .await
}

/// Runs the probe for `account` through `s3` (a handle for that account),
/// with the region probe for `bucket` when one is named.
///
/// Fails only with `usage`: an invalid bucket name, an unusable endpoint or
/// invalid proxy settings. Everything the probe finds out is in the report.
pub async fn run(
    s3: &S3,
    account: &Account,
    bucket: Option<&str>,
    network: &Network,
    timeouts: Timeouts,
) -> Result<Report> {
    if let Some(bucket) = bucket {
        validate_bucket_name(bucket)?;
    }
    let started_at = Utc::now();
    let clock = Instant::now();
    let endpoint = Endpoint::of(account).await?;
    let route = Route::choose(network, &endpoint)?;
    let mut steps = Vec::with_capacity(6);

    // dns and tcp probe the proxy host when a proxy carries the traffic.
    let (hop_host, hop_port) = match &route {
        Route::Via(hop) => (hop.host.as_str(), hop.port),
        Route::Direct(_) => (endpoint.host.as_str(), endpoint.port),
    };
    let what = if matches!(route, Route::Via(_)) {
        "the proxy"
    } else {
        "the endpoint"
    };
    let (step, addresses) = probe_dns(hop_host, hop_port, what, timeouts.connect).await;
    steps.push(step);
    let (step, stream) = probe_tcp(
        addresses.as_deref(),
        hop_host,
        hop_port,
        what,
        timeouts.connect,
    )
    .await;
    steps.push(step);
    let (step, stream) = probe_proxy(stream, &route, &endpoint, timeouts.connect).await;
    steps.push(step);
    // `stream` is now the connection to the endpoint, direct or tunnelled.
    let network_ok = steps.iter().all(|step| step.status != StepStatus::Fail);
    let link = Link {
        route: &route,
        addresses: addresses.as_deref().unwrap_or_default(),
        endpoint: &endpoint,
        timeouts,
    };
    steps.push(probe_tls(stream, &endpoint, network, &link, network_ok).await);

    let network_ok = steps.iter().all(|step| step.status != StepStatus::Fail);
    let (step, answered) = probe_credentials(s3, network_ok, timeouts.request).await;
    steps.push(step);
    steps.push(
        probe_region(
            s3,
            account,
            bucket,
            network_ok && answered,
            timeouts.request,
        )
        .await,
    );

    Ok(Report {
        target: redact_endpoint(&endpoint.url),
        region: account.region.clone(),
        bucket: bucket.map(str::to_owned),
        started_at,
        elapsed_ms: millis(clock.elapsed()),
        ok: steps.iter().all(|step| step.status != StepStatus::Fail),
        steps,
    })
}

// ---- outcomes and steps ----

/// What a probe found, before its timing is added.
struct Outcome {
    status: StepStatus,
    code: &'static str,
    summary: String,
    detail: Option<String>,
}

impl Outcome {
    fn pass(code: &'static str, summary: impl Into<String>) -> Self {
        Outcome {
            status: StepStatus::Pass,
            code,
            summary: summary.into(),
            detail: None,
        }
    }

    fn fail(code: &'static str, summary: impl Into<String>) -> Self {
        Outcome {
            status: StepStatus::Fail,
            ..Outcome::pass(code, summary)
        }
    }

    fn skip(code: &'static str, summary: impl Into<String>) -> Self {
        Outcome {
            status: StepStatus::Skip,
            ..Outcome::pass(code, summary)
        }
    }

    fn blocked(why: &str) -> Self {
        Outcome::skip("blocked", format!("Skipped: {why}."))
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn into_step(self, id: StepId, started: Instant) -> Step {
        Step {
            id,
            elapsed_ms: if self.status == StepStatus::Skip {
                0
            } else {
                millis(started.elapsed())
            },
            status: self.status,
            summary: self.summary,
            detail: self.detail,
            code: self.code.to_owned(),
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

// ---- the endpoint and the route ----

/// The URL requests go to, split up.
struct Endpoint {
    url: String,
    host: String,
    port: u16,
    https: bool,
    uri: http::Uri,
}

impl Endpoint {
    /// The custom endpoint of the account, or the S3 endpoint of its region
    /// as the SDK's own rules name it.
    async fn of(account: &Account) -> Result<Self> {
        let url = match &account.endpoint {
            Some(endpoint) => endpoint.trim_end_matches('/').to_owned(),
            None => aws_endpoint(account).await,
        };
        let uri: http::Uri = url.parse().map_err(|_| {
            Error::Usage(format!("invalid endpoint URL '{}'", redact_endpoint(&url)))
        })?;
        let https = !uri
            .scheme_str()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http"));
        let host = uri
            .host()
            .map(|host| host.trim_matches(['[', ']']).to_owned())
            .filter(|host| !host.is_empty())
            .ok_or_else(|| {
                Error::Usage(format!(
                    "the endpoint '{}' has no host",
                    redact_endpoint(&url)
                ))
            })?;
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        Ok(Endpoint {
            url,
            host,
            port,
            https,
            uri,
        })
    }

    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// The endpoint the SDK picks for ListBuckets in the account's region.
async fn aws_endpoint(account: &Account) -> String {
    use aws_sdk_s3::config::endpoint::{DefaultResolver, Params, ResolveEndpoint};
    let params = Params::builder()
        .region(account.region.clone())
        .use_fips(false)
        .use_dual_stack(false)
        .accelerate(false)
        .force_path_style(account.path_style)
        .build();
    if let Ok(params) = params
        && let Ok(endpoint) = DefaultResolver::new().resolve_endpoint(&params).await
    {
        return endpoint.url().trim_end_matches('/').to_owned();
    }
    format!("https://s3.{}.amazonaws.com", account.region)
}

/// How the traffic gets to the endpoint.
enum Route {
    /// Straight to the endpoint.
    Direct(Direct),
    /// Through a proxy.
    Via(Box<ProxyHop>),
}

enum Direct {
    /// No proxy applies.
    NoProxy,
    /// A proxy is configured, but the endpoint is on its bypass list.
    Bypassed,
}

struct ProxyHop {
    kind: ProxyKind,
    host: String,
    port: u16,
    login: Login,
    /// Where the proxy comes from, for the summaries.
    origin: &'static str,
}

enum Login {
    None,
    /// The whole `Proxy-Authorization` header value.
    Basic(Zeroizing<String>),
    Socks(ProxyAuth),
}

impl Route {
    fn choose(network: &Network, endpoint: &Endpoint) -> Result<Route> {
        let password = network.proxy_password.as_ref().map(SecretString::expose);
        if let Some(plan) = ProxyPlan::from_settings(&network.proxy, password)? {
            if plan.bypass.matches_host(&endpoint.host) {
                return Ok(Route::Direct(Direct::Bypassed));
            }
            let login = match (&plan.auth, plan.kind) {
                (None, _) => Login::None,
                (Some(auth), ProxyKind::Http) => Login::Basic(basic_auth_value(auth)),
                (Some(auth), ProxyKind::Socks5) => Login::Socks(auth.clone()),
            };
            return Ok(Route::Via(Box::new(ProxyHop {
                kind: plan.kind,
                host: plan.host.clone(),
                port: plan.port,
                login,
                origin: "the proxy settings",
            })));
        }
        let Some(intercept) = network
            .environment_proxy
            .as_ref()
            .and_then(|matcher| matcher.intercept(&endpoint.uri))
        else {
            return Ok(Route::Direct(Direct::NoProxy));
        };
        let uri = intercept.uri();
        let scheme = uri.scheme_str().unwrap_or("http");
        let kind = if scheme.starts_with("socks") {
            ProxyKind::Socks5
        } else {
            ProxyKind::Http
        };
        let host = uri
            .host()
            .map(|host| host.trim_matches(['[', ']']).to_owned())
            .unwrap_or_default();
        let default_port = match scheme {
            "https" => 443,
            s if s.starts_with("socks") => 1080,
            _ => 80,
        };
        let login = match kind {
            ProxyKind::Http => intercept
                .basic_auth()
                .and_then(|value| value.to_str().ok())
                .map_or(Login::None, |value| {
                    Login::Basic(Zeroizing::new(value.to_owned()))
                }),
            ProxyKind::Socks5 => match intercept.raw_auth() {
                Some((user, password)) => Login::Socks(ProxyAuth::new(kind, user, password)?),
                None => Login::None,
            },
        };
        Ok(Route::Via(Box::new(ProxyHop {
            kind,
            host,
            port: uri.port_u16().unwrap_or(default_port),
            login,
            origin: "the environment variables",
        })))
    }
}

/// What the TLS probe needs to open the connection again (to read the
/// certificate of a server that failed verification).
struct Link<'a> {
    route: &'a Route,
    addresses: &'a [SocketAddr],
    endpoint: &'a Endpoint,
    timeouts: Timeouts,
}

// ---- dns ----

async fn probe_dns(
    host: &str,
    port: u16,
    what: &str,
    limit: Duration,
) -> (Step, Option<Vec<SocketAddr>>) {
    let started = Instant::now();
    if let Ok(ip) = host.parse::<IpAddr>() {
        let outcome = Outcome::skip(
            "ip_address",
            format!("{what} is the IP address {ip}: no lookup is needed."),
        );
        return (
            outcome.into_step(StepId::Dns, started),
            Some(vec![SocketAddr::new(ip, port)]),
        );
    }
    match timeout(limit, tokio::net::lookup_host((host, port))).await {
        Ok(Ok(found)) => {
            let addresses: Vec<SocketAddr> = found.collect();
            if addresses.is_empty() {
                let outcome = Outcome::fail(
                    "not_found",
                    format!("{host} has no addresses: the name does not resolve."),
                );
                return (outcome.into_step(StepId::Dns, started), None);
            }
            let outcome = Outcome::pass(
                "resolved",
                format!(
                    "{host} resolves to {}.",
                    plural(addresses.len(), "address", "addresses")
                ),
            )
            .detail(list_addresses(&addresses));
            (outcome.into_step(StepId::Dns, started), Some(addresses))
        }
        Ok(Err(error)) => {
            let (code, why) = lookup_failure(&error);
            let outcome = Outcome::fail(code, format!("Could not resolve {host}: {why}."))
                .detail("Check the host name and the computer's network and DNS settings.");
            (outcome.into_step(StepId::Dns, started), None)
        }
        Err(_) => {
            let outcome = Outcome::fail(
                "timeout",
                format!(
                    "Resolving {host} took longer than {} seconds.",
                    limit.as_secs()
                ),
            )
            .detail("The DNS server did not answer: check the network and DNS settings.");
            (outcome.into_step(StepId::Dns, started), None)
        }
    }
}

/// `getaddrinfo` failures carry no error kind; the message is all there is.
fn lookup_failure(error: &io::Error) -> (&'static str, String) {
    let text = error.to_string();
    let lower = text.to_lowercase();
    let unknown = [
        "not known",
        "no such host",
        "nodename nor servname",
        "no address associated",
        "name or service",
        "11001",
        "non-existent",
    ];
    if unknown.iter().any(|needle| lower.contains(needle)) {
        ("not_found", "the name does not exist".to_owned())
    } else {
        ("failed", text)
    }
}

fn list_addresses(addresses: &[SocketAddr]) -> String {
    let mut text = addresses
        .iter()
        .take(MAX_LISTED_ADDRESSES)
        .map(|address| address.ip().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if addresses.len() > MAX_LISTED_ADDRESSES {
        let _ = write!(text, " and {} more", addresses.len() - MAX_LISTED_ADDRESSES);
    }
    text
}

// ---- tcp ----

async fn probe_tcp(
    addresses: Option<&[SocketAddr]>,
    host: &str,
    port: u16,
    what: &str,
    limit: Duration,
) -> (Step, Option<TcpStream>) {
    let started = Instant::now();
    let Some(addresses) = addresses else {
        let outcome = Outcome::blocked("the host name did not resolve");
        return (outcome.into_step(StepId::Tcp, started), None);
    };
    match connect(addresses, limit).await {
        Ok((stream, address, failed)) => {
            let mut outcome =
                Outcome::pass("connected", format!("Connected to {what} at {address}."));
            if !failed.is_empty() {
                outcome = outcome.detail(format!(
                    "Other addresses failed first: {}.",
                    failed.join("; ")
                ));
            }
            (outcome.into_step(StepId::Tcp, started), Some(stream))
        }
        Err((code, failed)) => {
            let why = match code {
                "refused" => "the connection was refused",
                "timeout" => "the connection timed out",
                "unreachable" => "the network or host is unreachable",
                _ => "the connection failed",
            };
            let outcome = Outcome::fail(
                code,
                format!("Could not connect to {what} at {host}:{port}: {why}."),
            )
            .detail(failed.join("; "));
            (outcome.into_step(StepId::Tcp, started), None)
        }
    }
}

/// Connects to the first address that answers, within `limit` in total.
/// Returns the connection, its address and what failed before; or the code of
/// the last failure and what failed.
async fn connect(
    addresses: &[SocketAddr],
    limit: Duration,
) -> std::result::Result<(TcpStream, SocketAddr, Vec<String>), (&'static str, Vec<String>)> {
    let deadline = Instant::now() + limit;
    let mut failed = Vec::new();
    let mut code = "failed";
    for address in addresses {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            code = "timeout";
            break;
        }
        match timeout(left, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok((stream, *address, failed)),
            Ok(Err(error)) => {
                let (kind, text) = connect_failure(&error);
                code = kind;
                failed.push(format!("{address}: {text}"));
            }
            Err(_) => {
                code = "timeout";
                failed.push(format!("{address}: timed out"));
                break;
            }
        }
    }
    Err((code, failed))
}

fn connect_failure(error: &io::Error) -> (&'static str, String) {
    match error.kind() {
        io::ErrorKind::ConnectionRefused => ("refused", "refused".to_owned()),
        io::ErrorKind::TimedOut => ("timeout", "timed out".to_owned()),
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
            ("unreachable", "unreachable".to_owned())
        }
        _ => ("failed", error.to_string()),
    }
}

// ---- proxy ----

async fn probe_proxy(
    stream: Option<TcpStream>,
    route: &Route,
    endpoint: &Endpoint,
    limit: Duration,
) -> (Step, Option<TcpStream>) {
    let started = Instant::now();
    let hop = match route {
        Route::Direct(Direct::NoProxy) => {
            let outcome = Outcome::skip(
                "none",
                "No proxy is configured: the endpoint is reached directly.",
            );
            return (outcome.into_step(StepId::Proxy, started), stream);
        }
        Route::Direct(Direct::Bypassed) => {
            let outcome = Outcome::skip(
                "direct",
                format!(
                    "{} is on the proxy bypass list: it is reached directly.",
                    endpoint.host
                ),
            );
            return (outcome.into_step(StepId::Proxy, started), stream);
        }
        Route::Via(hop) => hop,
    };
    let Some(mut stream) = stream else {
        let outcome = Outcome::blocked("the proxy could not be reached");
        return (outcome.into_step(StepId::Proxy, started), None);
    };
    if !endpoint.https && hop.kind == ProxyKind::Http {
        let outcome = Outcome::pass(
            "forwarded",
            "The proxy accepts connections; requests to this plain-HTTP endpoint are forwarded through it.",
        );
        return (outcome.into_step(StepId::Proxy, started), None);
    }
    let result = timeout(limit, open_tunnel(&mut stream, hop, endpoint)).await;
    let outcome = match result {
        Ok(Ok(())) => Outcome::pass(
            "tunnel_open",
            format!(
                "The proxy from {} opened a tunnel to {} ({}).",
                hop.origin,
                endpoint.authority(),
                tunnel_kind(hop.kind)
            ),
        ),
        Ok(Err(failure)) => failure.into_outcome(hop, endpoint),
        Err(_) => Outcome::fail(
            "timeout",
            format!(
                "The proxy did not answer the request for a tunnel within {} seconds.",
                limit.as_secs()
            ),
        ),
    };
    let ok = outcome.status == StepStatus::Pass;
    (
        outcome.into_step(StepId::Proxy, started),
        ok.then_some(stream),
    )
}

fn tunnel_kind(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Http => "HTTP CONNECT",
        ProxyKind::Socks5 => "SOCKS5",
    }
}

/// Why a tunnel could not be opened.
enum TunnelFailure {
    /// The proxy answered with this HTTP status.
    Status(u16, String),
    /// The proxy wants credentials (SOCKS5), or rejected them.
    Auth(String),
    /// The SOCKS5 proxy could not connect to the endpoint.
    Rejected(String),
    /// The answer was not what a proxy of this kind says.
    Protocol(String),
    /// The connection broke.
    Io(io::Error),
}

impl TunnelFailure {
    fn into_outcome(self, hop: &ProxyHop, endpoint: &Endpoint) -> Outcome {
        let authority = endpoint.authority();
        match self {
            TunnelFailure::Status(407, _) => Outcome::fail(
                "auth_required",
                "The proxy requires credentials, or rejected the ones configured (HTTP 407).",
            )
            .detail("Enter the proxy user name and password in the settings (Proxy)."),
            TunnelFailure::Status(status, reason) => Outcome::fail(
                "rejected",
                format!(
                    "The proxy refused to open a tunnel to {authority} (HTTP {status}{}).",
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(" {reason}")
                    }
                ),
            )
            .detail("Proxies often allow CONNECT to port 443 only, or only to listed hosts."),
            TunnelFailure::Auth(text) => Outcome::fail("auth_required", format!("{text}."))
                .detail("Enter the proxy user name and password in the settings (Proxy)."),
            TunnelFailure::Rejected(text) => Outcome::fail("rejected", format!("{text}."))
                .detail("The proxy may not allow connections to this host or port."),
            TunnelFailure::Protocol(text) => Outcome::fail(
                "protocol",
                format!(
                    "{text}. Is {}:{} a {} proxy?",
                    hop.host,
                    hop.port,
                    match hop.kind {
                        ProxyKind::Http => "HTTP",
                        ProxyKind::Socks5 => "SOCKS5",
                    }
                ),
            ),
            TunnelFailure::Io(error) => Outcome::fail(
                "failed",
                format!("The connection to the proxy broke while a tunnel was requested: {error}."),
            ),
        }
    }
}

async fn open_tunnel(
    stream: &mut TcpStream,
    hop: &ProxyHop,
    endpoint: &Endpoint,
) -> std::result::Result<(), TunnelFailure> {
    match hop.kind {
        ProxyKind::Http => http_connect(stream, endpoint, &hop.login).await,
        ProxyKind::Socks5 => {
            let auth = match &hop.login {
                Login::Socks(auth) => Some(auth),
                _ => None,
            };
            socks5_connect(stream, &endpoint.host, endpoint.port, auth)
                .await
                .map_err(|error| match error {
                    SocksError::Auth(_) => TunnelFailure::Auth(error.to_string()),
                    SocksError::Rejected(_) | SocksError::Destination(_) => {
                        TunnelFailure::Rejected(error.to_string())
                    }
                    SocksError::Protocol(_) => TunnelFailure::Protocol(error.to_string()),
                    SocksError::Unreachable(source) | SocksError::Io(source) => {
                        TunnelFailure::Io(source)
                    }
                })
        }
    }
}

/// `CONNECT host:port`, then the status line of the answer.
async fn http_connect(
    stream: &mut TcpStream,
    endpoint: &Endpoint,
    login: &Login,
) -> std::result::Result<(), TunnelFailure> {
    let authority = endpoint.authority();
    let mut request = Zeroizing::new(format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: bucketbridge-diagnostics\r\n"
    ));
    if let Login::Basic(value) = login {
        request.push_str("Proxy-Authorization: ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(TunnelFailure::Io)?;
    let mut head: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > MAX_PROXY_HEAD {
            return Err(TunnelFailure::Protocol(
                "The proxy's answer is not an HTTP response".into(),
            ));
        }
        let read = stream.read(&mut chunk).await.map_err(TunnelFailure::Io)?;
        if read == 0 {
            return Err(TunnelFailure::Protocol(
                "The proxy closed the connection without answering".into(),
            ));
        }
        head.extend_from_slice(&chunk[..read]);
    }
    let line_end = head
        .iter()
        .position(|byte| *byte == b'\r')
        .unwrap_or(head.len());
    let line = String::from_utf8_lossy(&head[..line_end]);
    let mut words = line.split_whitespace();
    let status = match (
        words.next(),
        words.next().and_then(|code| code.parse::<u16>().ok()),
    ) {
        (Some(version), Some(status)) if version.starts_with("HTTP/") => status,
        _ => {
            return Err(TunnelFailure::Protocol(
                "The proxy's answer is not an HTTP response".into(),
            ));
        }
    };
    if status == 200 {
        return Ok(());
    }
    let reason = words.collect::<Vec<_>>().join(" ");
    Err(TunnelFailure::Status(status, reason))
}

// ---- tls ----

async fn probe_tls(
    stream: Option<TcpStream>,
    endpoint: &Endpoint,
    network: &Network,
    link: &Link<'_>,
    network_ok: bool,
) -> Step {
    let started = Instant::now();
    if !endpoint.https {
        let outcome = Outcome::skip(
            "plain_http",
            "The endpoint uses plain HTTP: there is no TLS handshake.",
        );
        return outcome.into_step(StepId::Tls, started);
    }
    let Some(stream) = stream.filter(|_| network_ok) else {
        let outcome = Outcome::blocked("the connection to the endpoint could not be opened");
        return outcome.into_step(StepId::Tls, started);
    };
    let config = match probe_tls_config(&network.extra_roots_pem) {
        Ok(config) => config,
        Err(error) => {
            let outcome = Outcome::fail("failed", format!("TLS could not be set up: {error}."));
            return outcome.into_step(StepId::Tls, started);
        }
    };
    let Ok(name) = ServerName::try_from(endpoint.host.clone()) else {
        let outcome = Outcome::fail(
            "failed",
            format!("'{}' is not a valid server name for TLS.", endpoint.host),
        );
        return outcome.into_step(StepId::Tls, started);
    };
    let limit = link.timeouts.connect;
    let handshake = timeout(limit, TlsConnector::from(config).connect(name, stream)).await;
    let outcome = match handshake {
        Ok(Ok(tls)) => {
            let (_, connection) = tls.get_ref();
            let version = connection.protocol_version().map(version_name);
            let suite = connection
                .negotiated_cipher_suite()
                .map(|suite| format!("{:?}", suite.suite()));
            let certificate = connection
                .peer_certificates()
                .and_then(|chain| chain.first())
                .and_then(|der| Certificate::parse(der.as_ref()));
            handshake_outcome(&endpoint.host, version, suite, certificate)
        }
        Ok(Err(error)) => {
            let (mut outcome, certificate_problem) = tls_failure(&error, &endpoint.host);
            if certificate_problem && let Some(certificate) = peek_certificate(link).await {
                let shown = describe_certificate(&certificate);
                outcome.detail = Some(match outcome.detail.take() {
                    Some(detail) => format!("The server presented:\n{shown}\n{detail}"),
                    None => format!("The server presented:\n{shown}"),
                });
            }
            outcome
        }
        Err(_) => Outcome::fail(
            "timeout",
            format!(
                "The TLS handshake with {} took longer than {} seconds.",
                endpoint.host,
                limit.as_secs()
            ),
        ),
    };
    outcome.into_step(StepId::Tls, started)
}

fn version_name(version: rustls::ProtocolVersion) -> String {
    match version {
        rustls::ProtocolVersion::TLSv1_3 => "TLS 1.3".to_owned(),
        rustls::ProtocolVersion::TLSv1_2 => "TLS 1.2".to_owned(),
        other => format!("{other:?}"),
    }
}

fn handshake_outcome(
    host: &str,
    version: Option<String>,
    suite: Option<String>,
    certificate: Option<Certificate>,
) -> Outcome {
    let version = version.unwrap_or_else(|| "TLS".to_owned());
    let mut summary = format!("{version} handshake with {host} succeeded.");
    let mut detail = String::new();
    if let Some(suite) = suite {
        let _ = writeln!(detail, "Cipher suite: {suite}");
    }
    match certificate {
        Some(certificate) => {
            let days = (certificate.not_after - Utc::now()).num_days();
            if days < EXPIRY_WARNING_DAYS {
                let _ = write!(
                    summary,
                    " The certificate expires in {}.",
                    plural(usize::try_from(days.max(0)).unwrap_or(0), "day", "days")
                );
            }
            detail.push_str(&describe_certificate(&certificate));
        }
        None => detail.push_str("The server's certificate could not be read."),
    }
    Outcome::pass("handshake", summary).detail(detail.trim_end().to_owned())
}

/// Maps a failed handshake. The flag says that the server presented a
/// certificate that was not accepted (its details are worth showing).
fn tls_failure(error: &io::Error, host: &str) -> (Outcome, bool) {
    let rustls_error = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    let Some(rustls_error) = rustls_error else {
        let hung_up = matches!(
            error.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
        );
        let text = if hung_up {
            format!(
                "{host} closed the connection during the TLS handshake. Is it a TLS endpoint? Check the scheme and the port."
            )
        } else {
            format!("The TLS handshake with {host} failed: {error}.")
        };
        return (Outcome::fail("failed", text), false);
    };
    match rustls_error {
        rustls::Error::InvalidCertificate(problem) => (certificate_failure(problem, host), true),
        rustls::Error::AlertReceived(alert) => (
            Outcome::fail(
                "protocol",
                format!("{host} rejected the TLS handshake ({alert:?})."),
            )
            .detail("The server may not support a TLS version or cipher suite this client offers."),
            false,
        ),
        rustls::Error::PeerIncompatible(problem) => (
            Outcome::fail(
                "protocol",
                format!("{host} is incompatible with this client's TLS settings ({problem:?})."),
            ),
            false,
        ),
        other => (
            Outcome::fail(
                "protocol",
                format!("The TLS handshake with {host} failed: {other}."),
            )
            .detail("Check that the endpoint URL uses the right scheme and port."),
            false,
        ),
    }
}

fn certificate_failure(problem: &CertificateError, host: &str) -> Outcome {
    match problem {
        CertificateError::UnknownIssuer => Outcome::fail(
            "untrusted_issuer",
            format!("The certificate of {host} was issued by an authority this computer does not trust."),
        )
        .detail(
            "A private CA, a self-signed certificate or a TLS-inspecting proxy causes this. Install the CA certificate in the operating system's trust store.",
        ),
        CertificateError::Expired => Outcome::fail(
            "expired",
            format!("The certificate of {host} has expired."),
        )
        .detail("If it has not, check the date, time and time zone of this computer."),
        CertificateError::ExpiredContext { not_after, .. } => Outcome::fail(
            "expired",
            format!(
                "The certificate of {host} expired on {}.",
                format_unix(not_after.as_secs())
            ),
        )
        .detail("If it has not, check the date, time and time zone of this computer."),
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            Outcome::fail(
                "not_yet_valid",
                format!("The certificate of {host} is not valid yet."),
            )
            .detail("Check the date, time and time zone of this computer.")
        }
        CertificateError::NotValidForName => Outcome::fail(
            "name_mismatch",
            format!("The certificate is not valid for the name {host}."),
        ),
        CertificateError::NotValidForNameContext { presented, .. } => Outcome::fail(
            "name_mismatch",
            format!("The certificate is not valid for the name {host}."),
        )
        .detail(format!("It is valid for: {}.", presented.join(", "))),
        other => Outcome::fail(
            "bad_certificate",
            format!("The certificate of {host} was rejected: {other:?}."),
        ),
    }
}

fn format_unix(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .map_or_else(
            || "an unknown date".to_owned(),
            |time| time.format("%Y-%m-%d").to_string(),
        )
}

fn describe_certificate(certificate: &Certificate) -> String {
    let days = (certificate.not_after - Utc::now()).num_days();
    let validity = if days >= 0 {
        format!(
            "expires in {}",
            plural(usize::try_from(days).unwrap_or(0), "day", "days")
        )
    } else {
        format!(
            "expired {} ago",
            plural(usize::try_from(-days).unwrap_or(0), "day", "days")
        )
    };
    format!(
        "Certificate subject: {}\nIssuer: {}\nValid from {} to {} UTC ({validity})",
        certificate.subject,
        certificate.issuer,
        certificate.not_before.format("%Y-%m-%d"),
        certificate.not_after.format("%Y-%m-%d"),
    )
}

/// Opens the connection once more and reads the certificate the server
/// presents without verifying it: only the handshake runs and nothing is
/// sent, so the answer to "which certificate does it present" costs no
/// secrecy. Best effort: `None` when anything fails.
async fn peek_certificate(link: &Link<'_>) -> Option<Certificate> {
    let limit = link.timeouts.connect;
    let (mut stream, _, _) = connect(link.addresses, limit).await.ok()?;
    if let Route::Via(hop) = link.route {
        timeout(limit, open_tunnel(&mut stream, hop, link.endpoint))
            .await
            .ok()?
            .ok()?;
    }
    let seen = Arc::new(Mutex::new(None));
    let provider = Arc::new(probe_crypto_provider());
    let verifier = CaptureCertificate {
        seen: Arc::clone(&seen),
        algorithms: provider.signature_verification_algorithms,
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = ServerName::try_from(link.endpoint.host.clone()).ok()?;
    let handshake = timeout(
        limit,
        TlsConnector::from(Arc::new(config)).connect(name, stream),
    )
    .await;
    drop(handshake);
    let der = seen.lock().unwrap_or_else(PoisonError::into_inner).take()?;
    Certificate::parse(&der)
}

/// Accepts any certificate and keeps the first one it was offered. Used only
/// by [`peek_certificate`], which sends no data.
#[derive(Debug)]
struct CaptureCertificate {
    seen: Arc<Mutex<Option<Vec<u8>>>>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for CaptureCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(end_entity.to_vec());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

// ---- credentials ----

/// The per-request configuration of a probe: one attempt, a bounded time.
fn probe_config(limit: Duration) -> aws_sdk_s3::config::Builder {
    aws_sdk_s3::config::Builder::default()
        .retry_config(RetryConfig::disabled())
        .timeout_config(TimeoutConfig::builder().operation_timeout(limit).build())
}

/// Remembers the `Date` header of the server's answer.
#[derive(Debug, Clone, Default)]
struct ServerDate(Arc<Mutex<Option<String>>>);

impl ServerDate {
    fn seen(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Intercept for ServerDate {
    fn name(&self) -> &'static str {
        "BucketBridgeServerDate"
    }

    fn read_before_deserialization(
        &self,
        context: &BeforeDeserializationInterceptorContextRef<'_>,
        _runtime_components: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> std::result::Result<(), BoxError> {
        if let Some(date) = context.response().headers().get(DATE_HEADER) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(date.to_owned());
        }
        Ok(())
    }
}

/// This computer's clock minus the server's, in seconds, from a `Date`
/// header; `None` when there is none or it cannot be read.
fn clock_offset(date: Option<&str>) -> Option<i64> {
    let server = aws_smithy_types::DateTime::from_str(date?, HttpDateFormat::HttpDate).ok()?;
    Some(Utc::now().timestamp() - server.secs())
}

fn clock_sentence(offset: i64) -> String {
    let magnitude = offset.unsigned_abs();
    let amount = if magnitude >= 120 {
        format!("{} minutes", magnitude / 60)
    } else {
        format!("{magnitude} seconds")
    };
    match offset {
        0 => "This computer's clock matches the server's.".to_owned(),
        o if o > 0 => format!("This computer's clock is {amount} ahead of the server's."),
        _ => format!("This computer's clock is {amount} behind the server's."),
    }
}

/// The signed request. The flag says that the server answered at all.
async fn probe_credentials(s3: &S3, network_ok: bool, limit: Duration) -> (Step, bool) {
    let started = Instant::now();
    if !network_ok {
        let outcome = Outcome::blocked("the connection to the endpoint could not be opened");
        return (outcome.into_step(StepId::Credentials, started), false);
    }
    let date = ServerDate::default();
    let result = s3
        .client()
        .list_buckets()
        .max_buckets(1)
        .customize()
        .config_override(probe_config(limit))
        .interceptor(date.clone())
        .send()
        .await;
    let (outcome, answered) = match result {
        Ok(_) => {
            let mut outcome = Outcome::pass("valid", "The server accepted the credentials.");
            if let Some(offset) = clock_offset(date.seen().as_deref()) {
                outcome = outcome.detail(format!("Clock check: {}", clock_sentence(offset)));
            }
            (outcome, true)
        }
        Err(error) => credentials_failure(error, limit),
    };
    (outcome.into_step(StepId::Credentials, started), answered)
}

fn credentials_failure(
    error: SdkError<ListBucketsError, HttpResponse>,
    limit: Duration,
) -> (Outcome, bool) {
    let timed_out = matches!(error, SdkError::TimeoutError(_));
    let date = error
        .raw_response()
        .and_then(|response| response.headers().get(DATE_HEADER))
        .map(str::to_owned);
    let error = Error::from(error);
    if timed_out {
        return (
            Outcome::fail(
                "timeout",
                format!(
                    "The server did not answer the signed request within {} seconds.",
                    limit.as_secs()
                ),
            ),
            false,
        );
    }
    let Some(service) = error.service() else {
        return (Outcome::fail("failed", error.to_string()), false);
    };
    let message = error.to_string();
    let code = service.code();
    let answered = service.status().is_some();
    let offset = clock_offset(date.as_deref());
    let outcome = match code {
        Some("InvalidAccessKeyId") => Outcome::fail(
            "invalid_access_key",
            "The server does not know this access key ID.",
        )
        .detail("Check the access key ID for typing mistakes, whether the key was deleted, and that the endpoint is the one that issued it."),
        Some("SignatureDoesNotMatch") => Outcome::fail(
            "bad_signature",
            "The server rejected the signature: the secret access key is wrong.",
        )
        .detail("Check the secret access key (and the session token, for temporary credentials), the region and the endpoint."),
        Some("RequestTimeTooSkewed" | "RequestExpired") => {
            let how = offset.map_or_else(
                || "This computer's clock differs too much from the server's.".to_owned(),
                clock_sentence,
            );
            Outcome::fail(
                "clock_skew",
                format!("{how} S3 rejects requests whose time differs by more than 15 minutes."),
            )
            .detail("Correct the date, time and time zone of this computer.")
        }
        Some("ExpiredToken" | "ExpiredTokenException") => Outcome::fail(
            "expired_token",
            "The session token has expired.",
        )
        .detail("Get new temporary credentials."),
        Some("InvalidToken" | "InvalidClientTokenId") => Outcome::fail(
            "invalid_token",
            "The server rejected the session token.",
        ),
        Some("AuthorizationHeaderMalformed") => Outcome::fail(
            "wrong_region",
            "The server rejected the request's signing region.",
        )
        .detail(message.clone()),
        Some("AccessDenied") => Outcome::pass(
            "access_denied",
            "The server accepted the credentials, but this key is not allowed to list buckets.",
        )
        .detail("The key is valid; it only lacks the s3:ListAllMyBuckets permission."),
        Some("PermanentRedirect" | "TemporaryRedirect") => Outcome::fail(
            "redirect",
            "The endpoint redirects to another location.",
        )
        .detail("Check the endpoint URL and the region of the account."),
        None if error.kind() == crate::error::ErrorKind::Auth => Outcome::fail(
            "proxy_auth",
            "The proxy rejected the proxy credentials, or requires credentials.",
        )
        .detail("Enter the proxy user name and password in the settings (Proxy)."),
        _ if matches!(service.status(), Some(301 | 307)) => Outcome::fail(
            "redirect",
            "The endpoint redirects to another location.",
        )
        .detail("Check the endpoint URL and the region of the account."),
        _ => Outcome::fail("failed", message),
    };
    (outcome, answered)
}

// ---- region ----

async fn probe_region(
    s3: &S3,
    account: &Account,
    bucket: Option<&str>,
    can_ask: bool,
    limit: Duration,
) -> Step {
    let started = Instant::now();
    let Some(bucket) = bucket else {
        let outcome = Outcome::skip(
            "no_bucket",
            "No bucket was named: the region probe was not run.",
        );
        return outcome.into_step(StepId::Region, started);
    };
    if !can_ask {
        let outcome = Outcome::blocked("the server did not answer the signed request");
        return outcome.into_step(StepId::Region, started);
    }
    let result = s3
        .client()
        .head_bucket()
        .bucket(bucket)
        .customize()
        .config_override(probe_config(limit))
        .send()
        .await;
    let outcome = match result {
        Ok(out) => {
            let region = out
                .bucket_region()
                .map(str::trim)
                .filter(|region| !region.is_empty())
                .map(str::to_owned);
            found_region(bucket, region, &account.region, account.endpoint.is_some())
        }
        Err(error) => region_error(bucket, error, limit),
    };
    outcome.into_step(StepId::Region, started)
}

fn found_region(
    bucket: &str,
    region: Option<String>,
    account_region: &str,
    custom: bool,
) -> Outcome {
    match region {
        Some(region) if region == account_region => Outcome::pass(
            "found",
            format!("Bucket '{bucket}' is in {region}, the account's region."),
        ),
        Some(region) => Outcome::pass(
            "found",
            format!(
                "Bucket '{bucket}' is in {region}; the account's region is {account_region}."
            ),
        )
        .detail(if custom {
            format!("This endpoint sends every request with the account region {account_region}.")
        } else {
            format!("Requests for this bucket are sent to {region} automatically.")
        }),
        None => Outcome::pass(
            "no_region_header",
            format!("Bucket '{bucket}' exists and is accessible."),
        )
        .detail(format!(
            "The server reports no bucket region; requests use the account's region {account_region}."
        )),
    }
}

/// What a failed `HeadBucket` says: the bucket's region from the answer's
/// header, whether it exists and whether the account may see it.
fn region_error(
    bucket: &str,
    error: SdkError<HeadBucketError, HttpResponse>,
    limit: Duration,
) -> Outcome {
    let timed_out = matches!(error, SdkError::TimeoutError(_));
    let raw = error.raw_response();
    let status = raw.map(|response| response.status().as_u16());
    let region = raw
        .and_then(|response| response.headers().get(BUCKET_REGION_HEADER))
        .map(|region| region.trim().to_owned())
        .filter(|region| !region.is_empty());
    let error = Error::from(error);
    if timed_out {
        return Outcome::fail(
            "timeout",
            format!(
                "The server did not answer the request for bucket '{bucket}' within {} seconds.",
                limit.as_secs()
            ),
        );
    }
    let known = region
        .as_ref()
        .map(|region| format!("It is in {region}."))
        .unwrap_or_default();
    match status {
        // S3 answers a request to the wrong region with a redirect that
        // names the right one.
        Some(301 | 307 | 400) if region.is_some() => Outcome::pass(
            "found",
            format!(
                "Bucket '{bucket}' is in {}; the endpoint redirected to it.",
                region.unwrap_or_default()
            ),
        ),
        Some(403) => Outcome::fail(
            "access_denied",
            format!("Access to bucket '{bucket}' is denied. {known}")
                .trim_end()
                .to_owned(),
        )
        .detail(
            "The bucket exists, but this account may not list it or the bucket policy denies it.",
        ),
        Some(404) => Outcome::fail(
            "bucket_not_found",
            format!("Bucket '{bucket}' does not exist on this endpoint."),
        )
        .detail("Check the spelling, and the account and endpoint."),
        _ => Outcome::fail("failed", error.to_string()),
    }
}

// ---- the certificate ----

/// The few fields of an X.509 certificate the diagnostics show.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Certificate {
    subject: String,
    issuer: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
}

impl Certificate {
    /// Reads subject, issuer and validity from a DER certificate; `None` for
    /// anything that is not one.
    ///
    /// ```text
    /// Certificate  ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
    /// tbsCertificate ::= SEQUENCE { [0] version OPTIONAL, serialNumber, signature,
    ///                               issuer, validity, subject, ... }
    /// ```
    fn parse(der: &[u8]) -> Option<Certificate> {
        let (_, certificate) = Der::new(der).next(SEQUENCE)?;
        let (_, tbs) = Der::new(certificate).next(SEQUENCE)?;
        let mut fields = Der::new(tbs);
        let mut first = fields.next_any()?;
        if first.0 == CONTEXT_0 {
            first = fields.next_any()?;
        }
        if first.0 != INTEGER {
            return None;
        }
        fields.next(SEQUENCE)?;
        let (_, issuer) = fields.next(SEQUENCE)?;
        let (_, validity) = fields.next(SEQUENCE)?;
        let (_, subject) = fields.next(SEQUENCE)?;
        let mut times = Der::new(validity);
        let not_before = parse_time(times.next_any()?)?;
        let not_after = parse_time(times.next_any()?)?;
        Some(Certificate {
            subject: parse_name(subject)?,
            issuer: parse_name(issuer)?,
            not_before,
            not_after,
        })
    }
}

const INTEGER: u8 = 0x02;
const OBJECT_IDENTIFIER: u8 = 0x06;
const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const CONTEXT_0: u8 = 0xA0;

/// A reader of DER elements: tag, length, content. Lengths are checked
/// against what is left, so it never reads outside its input.
struct Der<'a> {
    bytes: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Der { bytes }
    }

    /// The next element, whatever its tag.
    fn next_any(&mut self) -> Option<(u8, &'a [u8])> {
        let (&tag, rest) = self.bytes.split_first()?;
        let (&first, rest) = rest.split_first()?;
        let (length, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            let count = usize::from(first & 0x7f);
            if count == 0 || count > 4 || rest.len() < count {
                return None;
            }
            let (length_bytes, rest) = rest.split_at(count);
            let length = length_bytes
                .iter()
                .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
            (length, rest)
        };
        if length > rest.len() {
            return None;
        }
        let (content, rest) = rest.split_at(length);
        self.bytes = rest;
        Some((tag, content))
    }

    /// The next element, which must have `tag`.
    fn next(&mut self, tag: u8) -> Option<(u8, &'a [u8])> {
        self.next_any().filter(|(found, _)| *found == tag)
    }
}

fn parse_time((tag, text): (u8, &[u8])) -> Option<DateTime<Utc>> {
    let text = std::str::from_utf8(text).ok()?.strip_suffix('Z')?;
    let digits = |from: usize, to: usize| -> Option<u32> { text.get(from..to)?.parse().ok() };
    let (year, rest) = match tag {
        UTC_TIME if text.len() == 12 => {
            let year = digits(0, 2)?;
            (
                i32::try_from(if year >= 50 { 1900 + year } else { 2000 + year }).ok()?,
                2,
            )
        }
        GENERALIZED_TIME if text.len() == 14 => (i32::try_from(digits(0, 4)?).ok()?, 4),
        _ => return None,
    };
    let date = NaiveDate::from_ymd_opt(year, digits(rest, rest + 2)?, digits(rest + 2, rest + 4)?)?;
    let time = date.and_hms_opt(
        digits(rest + 4, rest + 6)?,
        digits(rest + 6, rest + 8)?,
        digits(rest + 8, rest + 10)?,
    )?;
    Some(time.and_utc())
}

/// `CN=example.com, O=Example Inc, C=US`, in the order of the certificate.
fn parse_name(name: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut relative_names = Der::new(name);
    while !relative_names.bytes.is_empty() {
        let (_, set) = relative_names.next(SET)?;
        let mut attributes = Der::new(set);
        while !attributes.bytes.is_empty() {
            let (_, attribute) = attributes.next(SEQUENCE)?;
            let mut pair = Der::new(attribute);
            let (_, oid) = pair.next(OBJECT_IDENTIFIER)?;
            let (value_tag, value) = pair.next_any()?;
            parts.push(format!(
                "{}={}",
                attribute_name(oid),
                attribute_value(value_tag, value)
            ));
        }
    }
    Some(parts.join(", "))
}

fn attribute_name(oid: &[u8]) -> String {
    match oid {
        [0x55, 0x04, 0x03] => "CN".to_owned(),
        [0x55, 0x04, 0x05] => "serialNumber".to_owned(),
        [0x55, 0x04, 0x06] => "C".to_owned(),
        [0x55, 0x04, 0x07] => "L".to_owned(),
        [0x55, 0x04, 0x08] => "ST".to_owned(),
        [0x55, 0x04, 0x0a] => "O".to_owned(),
        [0x55, 0x04, 0x0b] => "OU".to_owned(),
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01] => "emailAddress".to_owned(),
        other => dotted(other),
    }
}

/// The dotted form of an object identifier, `2.5.4.45`.
fn dotted(oid: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut value = 0u64;
    for (index, byte) in oid.iter().enumerate() {
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            if index < oid.len() && parts.is_empty() {
                parts.push((value / 40).min(2).to_string());
                parts.push((value - 40 * (value / 40).min(2)).to_string());
            } else {
                parts.push(value.to_string());
            }
            value = 0;
        }
    }
    parts.join(".")
}

/// Text of a name attribute: UTF-8, printable and IA5 strings as they are,
/// UCS-2 (BMPString) decoded; anything else as hexadecimal.
fn attribute_value(tag: u8, value: &[u8]) -> String {
    match tag {
        0x0c | 0x13 | 0x16 | 0x14 => String::from_utf8_lossy(value).into_owned(),
        0x1e => {
            let units: Vec<u16> = value
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => value.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

#[cfg(test)]
mod tests;
