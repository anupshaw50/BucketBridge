//! The HTTP client shared by every AWS SDK client, with proxy support.
//!
//! [`build_http_client`] turns `settings.proxy` plus the proxy password into
//! one [`SharedHttpClient`]. Give the same client to every S3 and STS client:
//! they then share its connection pools.
//!
//! # Routes
//!
//! - **Proxy disabled**: the SDK's own hyper 1.x client (rustls with
//!   aws-lc-rs, the OS trust roots, connection pooling) with the SDK's
//!   default proxy behaviour: the `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`
//!   and `NO_PROXY` environment variables apply, as for any SDK client.
//!   [`HttpClientBuilder::environment_proxy`] turns that off.
//! - **HTTP proxy** (settings enabled; the environment is then ignored): the
//!   SDK's built-in proxy support
//!   (`aws_smithy_http_client::proxy::ProxyConfig`). An `https` endpoint is
//!   reached through an HTTP `CONNECT` tunnel and TLS runs end to end inside
//!   it, so the proxy never sees the requests. A plain `http` endpoint (a
//!   local S3-compatible server, for example) is forwarded: requests go to the
//!   proxy in absolute form, as HTTP requires. Credentials are sent with
//!   Basic authentication (`Proxy-Authorization`).
//! - **SOCKS5 proxy**: the SDK supports no SOCKS and has no public hook for a
//!   custom transport, so this module builds an equivalent client from the
//!   same parts: hyper-util's pooled client, rustls with aws-lc-rs, the same
//!   cipher suites, the same trust roots and ALPN (`h2`, `http/1.1`), over a
//!   SOCKS5 tunnel (RFC 1928) with optional username and password
//!   (RFC 1929). Host names are resolved by the proxy (like `socks5h`), so no
//!   DNS query leaves the machine directly.
//!
//! # Bypass list
//!
//! Each request is routed by the host of its URL; listed hosts connect
//! directly, through the SDK's normal client. Entries (case-insensitive):
//!
//! | Entry | Matches |
//! |---|---|
//! | `s3.example.com` | exactly that host |
//! | `*.example.com` or `.example.com` | any subdomain, not `example.com` itself |
//! | `10.1.2.3`, `::1`, `[::1]` | that IP address in the URL |
//! | `10.0.0.0/8`, `fd00::/8` | IP addresses in the range |
//! | `localhost` | `localhost`, `*.localhost`, `127.0.0.0/8` and `::1` |
//! | `*` | everything |
//!
//! IP and range entries only match URLs that contain an IP address: host
//! names are never resolved to decide the route. Loopback addresses are not
//! bypassed implicitly, because through an SSH-forwarded SOCKS proxy they
//! mean the remote machine; add `localhost` to reach local servers directly.
//!
//! # Failures
//!
//! Rejected proxy credentials (HTTP 407 or a SOCKS5 refusal) surface as a
//! non-retryable dispatch failure; [`is_proxy_auth_failure`] recognizes them
//! anywhere in an error's source chain so they can be reported as
//! authentication errors. Proxy credentials travel in cleartext to the proxy
//! (Basic authentication, RFC 1929), as both protocols define.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use aws_smithy_async::future::timeout::Timeout;
use aws_smithy_async::rt::sleep::{AsyncSleep, SharedAsyncSleep, default_async_sleep};
use aws_smithy_http_client::Connector;
use aws_smithy_http_client::proxy::ProxyConfig;
use aws_smithy_http_client::tls::rustls_provider::CryptoMode;
use aws_smithy_http_client::tls::{Provider, TlsContext, TrustStore};
use aws_smithy_runtime_api::client::connection::{CaptureSmithyConnection, ConnectionMetadata};
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::{
    RuntimeComponents, RuntimeComponentsBuilder,
};
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hyper_util::client::legacy::Client as PooledClient;
use hyper_util::client::legacy::connect::{CaptureConnection, capture_connection};
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_service::Service;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::{Error, Result};
use crate::settings::{ProxyKind, ProxySettings};

type BoxError = Box<dyn StdError + Send + Sync>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = std::result::Result<T, BoxError>> + Send>>;

/// Longest proxy user name or password accepted for HTTP proxies, in bytes.
const MAX_HTTP_CREDENTIAL_LEN: usize = 1024;

/// Characters left as-is when credentials are embedded in a proxy URL:
/// RFC 3986 unreserved characters. Everything else is percent-encoded, so any
/// user name or password survives the round trip through the URL.
const USERINFO: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Display text of hyper-util's error for a `407` answer to `CONNECT`.
const TUNNEL_407: &str = "tunnel error: proxy authorization required";

/// Builds the HTTP client for every SDK client from `settings.proxy` and the
/// proxy password (read from the vault through `password_ref`). The password
/// is only used together with a user name.
///
/// Fails with [`ErrorKind::Usage`](crate::ErrorKind::Usage) when the settings are invalid. May block
/// briefly the first time a SOCKS5 client is built, to load the OS trust
/// roots; call it from a blocking thread or at startup.
pub fn build_http_client(
    settings: &ProxySettings,
    proxy_password: Option<&str>,
) -> Result<SharedHttpClient> {
    HttpClientBuilder::new()
        .proxy(settings, proxy_password)?
        .build()
}

/// Checks `settings.proxy` (host, port, user name, bypass entries) without
/// building a client. Disabled settings are always valid.
pub fn validate_proxy_settings(settings: &ProxySettings) -> Result<()> {
    ProxyPlan::from_settings(settings, None).map(|_| ())
}

/// Whether `error`, or any error in its source chain, says that the proxy
/// rejected or required credentials. Use it to report such dispatch failures
/// as authentication errors.
pub fn is_proxy_auth_failure(error: &(dyn StdError + 'static)) -> bool {
    error_chain(error).any(|error| {
        error.is::<ProxyAuthRejected>()
            || matches!(
                error.downcast_ref::<SocksError>(),
                Some(SocksError::Auth(_))
            )
            || error.to_string() == TUNNEL_407
    })
}

/// Builder of the shared HTTP client, for callers that need more than
/// [`build_http_client`]: extra trusted root certificates (a private CA of
/// an S3-compatible server), no OS trust roots, or no environment proxy.
#[derive(Default)]
pub struct HttpClientBuilder {
    proxy: Option<ProxyPlan>,
    extra_roots: Vec<Vec<u8>>,
    without_native_roots: bool,
    without_environment_proxy: bool,
}

impl HttpClientBuilder {
    /// No proxy, OS trust roots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses `settings` (ignored when disabled) and the proxy password.
    pub fn proxy(mut self, settings: &ProxySettings, password: Option<&str>) -> Result<Self> {
        self.proxy = ProxyPlan::from_settings(settings, password)?;
        Ok(self)
    }

    /// Trusts the CA certificates in `pem` in addition to the OS roots.
    pub fn add_root_certificates_pem(mut self, pem: &[u8]) -> Result<Self> {
        let certificates = parse_pem_certificates(pem)?;
        let mut check = rustls::RootCertStore::empty();
        for certificate in certificates {
            check
                .add(certificate)
                .map_err(|error| invalid_certificate(&error.to_string()))?;
        }
        self.extra_roots.push(pem.to_vec());
        Ok(self)
    }

    /// Whether the OS trust roots are used (default `true`).
    pub fn native_roots(mut self, enabled: bool) -> Self {
        self.without_native_roots = !enabled;
        self
    }

    /// Whether the proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`,
    /// `ALL_PROXY`, `NO_PROXY`) apply while the proxy settings are disabled
    /// (default `true`, like the SDK's own client). Enabled proxy settings
    /// always take precedence over the environment.
    pub fn environment_proxy(mut self, enabled: bool) -> Self {
        self.without_environment_proxy = !enabled;
        self
    }

    /// Builds the client.
    pub fn build(self) -> Result<SharedHttpClient> {
        let roots = TlsRoots {
            native: !self.without_native_roots,
            extra_pem: self.extra_roots,
        };
        let tls_context = roots.sdk_tls_context()?;
        let Some(plan) = self.proxy else {
            if self.without_environment_proxy {
                return Ok(aws_smithy_http_client::Builder::new()
                    .tls_provider(Provider::Rustls(CryptoMode::AwsLc))
                    .tls_context(tls_context)
                    .build_https());
            }
            return Ok(SharedHttpClient::new(ProxyHttpClient {
                bypass: Arc::new(BypassList::default()),
                route: ProxyRoute::Environment,
                tls_context,
                connectors: Mutex::new(HashMap::new()),
            }));
        };
        let route = match plan.kind {
            ProxyKind::Http => ProxyRoute::Http(http_proxy_config(&plan)?),
            ProxyKind::Socks5 => ProxyRoute::Socks5(Arc::new(SocksRoute {
                host: plan.host.clone(),
                port: plan.port,
                auth: plan.auth.clone(),
                tls: roots.rustls_config()?,
            })),
        };
        Ok(SharedHttpClient::new(ProxyHttpClient {
            bypass: Arc::clone(&plan.bypass),
            route,
            tls_context,
            connectors: Mutex::new(HashMap::new()),
        }))
    }
}

impl fmt::Debug for HttpClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpClientBuilder")
            .field("proxy", &self.proxy)
            .field("extra_roots", &self.extra_roots.len())
            .field("native_roots", &!self.without_native_roots)
            .field("environment_proxy", &!self.without_environment_proxy)
            .finish()
    }
}

/// Hosts that connect directly instead of through the proxy. See the
/// [module docs](self) for the entry syntax.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BypassList {
    rules: Vec<BypassRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BypassRule {
    Everything,
    Host(String),
    Subdomains(String),
    Address(IpAddr),
    Network(IpAddr, u8),
    Localhost,
}

impl BypassList {
    /// Parses the entries of `settings.proxy.bypass`; blank entries are
    /// skipped, invalid ones fail with [`ErrorKind::Usage`](crate::ErrorKind::Usage).
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self> {
        let mut rules = Vec::with_capacity(entries.len());
        for entry in entries {
            let entry = entry.as_ref().trim();
            if !entry.is_empty() {
                rules.push(parse_bypass_rule(entry)?);
            }
        }
        Ok(Self { rules })
    }

    /// Whether the list has no entries.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether requests to `host` (a URL host; IPv6 with or without
    /// brackets) connect directly.
    pub fn matches_host(&self, host: &str) -> bool {
        let host = strip_brackets(host);
        if let Ok(address) = host.parse::<IpAddr>() {
            let address = canonical_ip(address);
            return self.rules.iter().any(|rule| match rule {
                BypassRule::Everything => true,
                BypassRule::Address(listed) => *listed == address,
                BypassRule::Network(base, prefix) => in_network(address, *base, *prefix),
                BypassRule::Localhost => address.is_loopback(),
                BypassRule::Host(_) | BypassRule::Subdomains(_) => false,
            });
        }
        let name = host.to_ascii_lowercase();
        let name = name.strip_suffix('.').unwrap_or(&name);
        self.rules.iter().any(|rule| match rule {
            BypassRule::Everything => true,
            BypassRule::Host(listed) => listed == name,
            BypassRule::Subdomains(suffix) => is_subdomain(name, suffix),
            BypassRule::Localhost => name == "localhost" || is_subdomain(name, "localhost"),
            BypassRule::Address(_) | BypassRule::Network(..) => false,
        })
    }

    /// Whether a request to `uri` connects directly. A URL without a host
    /// never bypasses the proxy.
    fn matches_uri(&self, uri: &str) -> bool {
        !self.rules.is_empty()
            && uri
                .parse::<http::Uri>()
                .ok()
                .and_then(|uri| uri.host().map(|host| self.matches_host(host)))
                .unwrap_or(false)
    }
}

fn parse_bypass_rule(entry: &str) -> Result<BypassRule> {
    if entry == "*" {
        return Ok(BypassRule::Everything);
    }
    let lower = entry.to_ascii_lowercase();
    if let Some((base, prefix)) = lower.split_once('/') {
        let base: IpAddr = strip_brackets(base)
            .parse()
            .map_err(|_| invalid_bypass(entry, "a range needs an IP address before the '/'"))?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        let prefix = prefix
            .parse::<u8>()
            .ok()
            .filter(|prefix| *prefix <= max)
            .ok_or_else(|| invalid_bypass(entry, "the prefix length is out of range"))?;
        let (base, prefix) = canonical_network(base, prefix);
        return Ok(BypassRule::Network(network_base(base, prefix), prefix));
    }
    if let Ok(address) = strip_brackets(&lower).parse::<IpAddr>() {
        return Ok(BypassRule::Address(canonical_ip(address)));
    }
    if lower == "localhost" {
        return Ok(BypassRule::Localhost);
    }
    if let Some(suffix) = lower.strip_prefix("*.").or_else(|| lower.strip_prefix('.')) {
        return Ok(BypassRule::Subdomains(check_bypass_name(suffix, entry)?));
    }
    Ok(BypassRule::Host(check_bypass_name(&lower, entry)?))
}

fn check_bypass_name(name: &str, entry: &str) -> Result<String> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.contains(':') {
        return Err(invalid_bypass(entry, "ports are not supported"));
    }
    if name.contains('*') {
        return Err(invalid_bypass(
            entry,
            "'*' is only allowed alone or as a leading '*.'",
        ));
    }
    if !is_host_name(name) {
        return Err(invalid_bypass(
            entry,
            "not a host name, IP address or range",
        ));
    }
    Ok(name.to_owned())
}

fn is_subdomain(name: &str, suffix: &str) -> bool {
    name.len() > suffix.len() + 1
        && name.ends_with(suffix)
        && name.as_bytes()[name.len() - suffix.len() - 1] == b'.'
}

fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// IPv4-mapped IPv6 addresses compare as IPv4.
fn canonical_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

/// An IPv4-mapped IPv6 range (`::ffff:a.b.c.d/96` or longer) is the IPv4
/// range it contains, since addresses are compared as IPv4 too.
fn canonical_network(base: IpAddr, prefix: u8) -> (IpAddr, u8) {
    match base {
        IpAddr::V6(v6) if prefix >= 96 => match v6.to_ipv4_mapped() {
            Some(v4) => (IpAddr::V4(v4), prefix - 96),
            None => (base, prefix),
        },
        _ => (base, prefix),
    }
}

fn network_base(base: IpAddr, prefix: u8) -> IpAddr {
    match base {
        IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & v4_mask(prefix)).into()),
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & v6_mask(prefix)).into()),
    }
}

fn in_network(address: IpAddr, base: IpAddr, prefix: u8) -> bool {
    match (address, base) {
        (IpAddr::V4(address), IpAddr::V4(base)) => {
            u32::from(address) & v4_mask(prefix) == u32::from(base)
        }
        (IpAddr::V6(address), IpAddr::V6(base)) => {
            u128::from(address) & v6_mask(prefix) == u128::from(base)
        }
        _ => false,
    }
}

fn v4_mask(prefix: u8) -> u32 {
    u32::MAX
        .checked_shl(32 - u32::from(prefix.min(32)))
        .unwrap_or(0)
}

fn v6_mask(prefix: u8) -> u128 {
    u128::MAX
        .checked_shl(128 - u32::from(prefix.min(128)))
        .unwrap_or(0)
}

/// Validated proxy settings.
pub(crate) struct ProxyPlan {
    pub(crate) kind: ProxyKind,
    /// Host name (lowercase) or IP address, without brackets.
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) auth: Option<ProxyAuth>,
    pub(crate) bypass: Arc<BypassList>,
}

impl ProxyPlan {
    pub(crate) fn from_settings(
        settings: &ProxySettings,
        password: Option<&str>,
    ) -> Result<Option<Self>> {
        if !settings.enabled {
            return Ok(None);
        }
        let host = proxy_host(&settings.host)?;
        if settings.port == 0 {
            return Err(invalid_proxy("the proxy port must be 1 to 65535"));
        }
        let bypass = BypassList::parse(&settings.bypass)?;
        let auth = match settings.username.as_deref().filter(|name| !name.is_empty()) {
            Some(username) => Some(ProxyAuth::new(
                settings.kind,
                username,
                password.unwrap_or(""),
            )?),
            None => None,
        };
        Ok(Some(Self {
            kind: settings.kind,
            host,
            port: settings.port,
            auth,
            bypass: Arc::new(bypass),
        }))
    }

    /// `host:port`, with brackets around an IPv6 address.
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Debug for ProxyPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyPlan")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("auth", &self.auth)
            .field("bypass", &self.bypass)
            .finish()
    }
}

/// The host of the proxy settings. A rejected value is never echoed in the
/// error: it may be a password typed into the host field.
fn proxy_host(host: &str) -> Result<String> {
    let host = host.trim();
    if host.is_empty() {
        return Err(invalid_proxy("the proxy host is empty"));
    }
    if host.contains('@') {
        return Err(invalid_proxy(
            "the proxy host must not contain '@': enter the user name and password in their own fields",
        ));
    }
    if let Ok(address) = strip_brackets(host).parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let name = host.to_ascii_lowercase();
    let name = name.strip_suffix('.').unwrap_or(&name);
    if is_host_name(name) {
        Ok(name.to_owned())
    } else {
        Err(invalid_proxy(
            "the proxy host is not a host name or IP address (give the host without a scheme, port or credentials)",
        ))
    }
}

/// Proxy credentials; wiped from memory on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ProxyAuth {
    username: String,
    password: String,
}

impl ProxyAuth {
    pub(crate) fn new(kind: ProxyKind, username: &str, password: &str) -> Result<Self> {
        match kind {
            ProxyKind::Http => {
                if username.contains(':') {
                    return Err(invalid_proxy(
                        "an HTTP proxy user name must not contain ':'",
                    ));
                }
                if username.len() > MAX_HTTP_CREDENTIAL_LEN
                    || password.len() > MAX_HTTP_CREDENTIAL_LEN
                {
                    return Err(invalid_proxy(
                        "the proxy user name and password must be at most 1024 bytes",
                    ));
                }
            }
            ProxyKind::Socks5 => {
                if username.len() > 255 || password.len() > 255 {
                    return Err(invalid_proxy(
                        "a SOCKS5 user name and password must be at most 255 bytes",
                    ));
                }
            }
        }
        Ok(Self {
            username: username.to_owned(),
            password: password.to_owned(),
        })
    }
}

impl fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The SDK's proxy configuration for an HTTP proxy that takes all traffic
/// (the bypass list is applied before a request reaches it).
fn http_proxy_config(plan: &ProxyPlan) -> Result<ProxyConfig> {
    let url = format!("http://{}", plan.authority());
    let config = ProxyConfig::all(url.as_str())
        .map_err(|error| invalid_proxy(&format!("invalid proxy address: {error}")))?;
    let Some(auth) = &plan.auth else {
        check_proxy_url(&url, None)?;
        return Ok(config);
    };
    // The SDK embeds the credentials in a proxy URL and hyper-util decodes
    // them again; percent-encoding keeps every character intact.
    let username = Zeroizing::new(utf8_percent_encode(&auth.username, USERINFO).to_string());
    let password = Zeroizing::new(utf8_percent_encode(&auth.password, USERINFO).to_string());
    check_proxy_url(&url, Some((auth, username.as_str(), password.as_str())))?;
    Ok(config.with_basic_auth(username.as_str(), password.as_str()))
}

/// Fail-closed check: the proxy URL as the SDK hands it to hyper-util must
/// intercept every request and carry exactly the configured credentials.
/// Otherwise hyper-util would silently connect directly.
fn check_proxy_url(url: &str, auth: Option<(&ProxyAuth, &str, &str)>) -> Result<()> {
    let base = url
        .parse::<http::Uri>()
        .map_err(|_| invalid_proxy("invalid proxy address"))?
        .to_string();
    let full = Zeroizing::new(match auth {
        Some((_, username, password)) => {
            let rest = base.strip_prefix("http://").unwrap_or(&base);
            format!("http://{username}:{password}@{rest}")
        }
        None => base,
    });
    let matcher = Matcher::builder().all(full.as_str()).build();
    for probe in [
        "https://bucketbridge.invalid/",
        "http://bucketbridge.invalid/",
    ] {
        let probe = probe
            .parse::<http::Uri>()
            .map_err(|_| proxy_setup_failed())?;
        let intercept = matcher.intercept(&probe).ok_or_else(proxy_setup_failed)?;
        let expected = auth.map(|(auth, _, _)| basic_auth_value(auth));
        let configured = intercept.basic_auth().map(|value| value.as_bytes());
        if configured != expected.as_ref().map(|value| value.as_bytes()) {
            return Err(proxy_setup_failed());
        }
    }
    Ok(())
}

pub(crate) fn basic_auth_value(auth: &ProxyAuth) -> Zeroizing<String> {
    let credentials = Zeroizing::new(format!("{}:{}", auth.username, auth.password));
    Zeroizing::new(format!("Basic {}", STANDARD.encode(credentials.as_bytes())))
}

/// Trust roots for both TLS stacks.
struct TlsRoots {
    native: bool,
    extra_pem: Vec<Vec<u8>>,
}

impl TlsRoots {
    fn sdk_tls_context(&self) -> Result<TlsContext> {
        if !self.native && self.extra_pem.is_empty() {
            return Err(no_trust_roots(
                "the OS roots are turned off and no certificate was added",
            ));
        }
        let mut trust_store = TrustStore::default().with_native_roots(self.native);
        for pem in &self.extra_pem {
            trust_store = trust_store.with_pem_certificate(pem.clone());
        }
        TlsContext::builder()
            .with_trust_store(trust_store)
            .build()
            .map_err(|error| invalid_certificate(&error.to_string()))
    }

    /// The rustls configuration of the SOCKS5 route, equivalent to the SDK's.
    fn rustls_config(&self) -> Result<Arc<rustls::ClientConfig>> {
        let mut roots = rustls::RootCertStore::empty();
        if self.native {
            roots.add_parsable_certificates(native_roots().iter().cloned());
        }
        for pem in &self.extra_pem {
            for certificate in parse_pem_certificates(pem)? {
                roots
                    .add(certificate)
                    .map_err(|error| invalid_certificate(&error.to_string()))?;
            }
        }
        if roots.is_empty() {
            return Err(no_trust_roots(
                "the OS store provided none and no certificate was added",
            ));
        }
        let mut config =
            rustls::ClientConfig::builder_with_provider(Arc::new(sdk_crypto_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|error| tls_setup_failed(&error.to_string()))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// The rustls client configuration of the SOCKS5 route (the OS trust roots
/// and the cipher suites of the SDK's client), with `extra_pem` trusted as
/// well and no ALPN. The diagnostics use it for their TLS handshake, so the
/// verdict is the one the real client would get.
pub(crate) fn probe_tls_config(extra_pem: &[Vec<u8>]) -> Result<Arc<rustls::ClientConfig>> {
    let roots = TlsRoots {
        native: true,
        extra_pem: extra_pem.to_vec(),
    };
    let mut config = (*roots.rustls_config()?).clone();
    config.alpn_protocols.clear();
    Ok(Arc::new(config))
}

/// The crypto provider the SDK's own client uses (see [`probe_tls_config`]).
pub(crate) fn probe_crypto_provider() -> rustls::crypto::CryptoProvider {
    sdk_crypto_provider()
}

/// aws-lc-rs restricted to the cipher suites the SDK's own client allows.
fn sdk_crypto_provider() -> rustls::crypto::CryptoProvider {
    const SUITES: [rustls::CipherSuite; 7] = [
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    ];
    let base = rustls::crypto::aws_lc_rs::default_provider();
    let cipher_suites = SUITES
        .iter()
        .filter_map(|wanted| {
            base.cipher_suites
                .iter()
                .find(|suite| suite.suite() == *wanted)
                .copied()
        })
        .collect();
    rustls::crypto::CryptoProvider {
        cipher_suites,
        ..base
    }
}

/// The OS trust roots, loaded once per process (slow on some systems).
fn native_roots() -> &'static [CertificateDer<'static>] {
    static ROOTS: OnceLock<Vec<CertificateDer<'static>>> = OnceLock::new();
    ROOTS.get_or_init(|| rustls_native_certs::load_native_certs().certs)
}

fn parse_pem_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_slice_iter(pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| invalid_certificate(&error.to_string()))?;
    if certificates.is_empty() {
        return Err(invalid_certificate("no certificate found in the PEM data"));
    }
    Ok(certificates)
}

/// Connector settings that need their own connector: the connect and read
/// timeouts.
type ConnectorKey = (Option<Duration>, Option<Duration>);

enum ProxyRoute {
    /// Proxy settings disabled: the SDK's environment-variable proxies.
    Environment,
    Http(ProxyConfig),
    Socks5(Arc<SocksRoute>),
}

/// The proxy-aware [`HttpClient`]: one routing connector per connector
/// settings (the timeouts), like the SDK's own client.
struct ProxyHttpClient {
    bypass: Arc<BypassList>,
    route: ProxyRoute,
    tls_context: TlsContext,
    connectors: Mutex<HashMap<ConnectorKey, SharedHttpConnector>>,
}

impl ProxyHttpClient {
    fn connectors(&self) -> MutexGuard<'_, HashMap<ConnectorKey, SharedHttpConnector>> {
        // Inserting into the map is the only update: never inconsistent.
        self.connectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn sdk_connector(
        &self,
        settings: &HttpConnectorSettings,
        sleep: Option<SharedAsyncSleep>,
        proxy: Option<ProxyConfig>,
    ) -> SharedHttpConnector {
        let mut builder = Connector::builder().connector_settings(settings.clone());
        builder.set_sleep_impl(sleep);
        builder.set_proxy_config(proxy);
        SharedHttpConnector::new(
            builder
                .tls_provider(Provider::Rustls(CryptoMode::AwsLc))
                .tls_context(self.tls_context.clone())
                .build(),
        )
    }
}

impl HttpClient for ProxyHttpClient {
    fn http_connector(
        &self,
        settings: &HttpConnectorSettings,
        components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        let key = (settings.connect_timeout(), settings.read_timeout());
        let mut connectors = self.connectors();
        if let Some(connector) = connectors.get(&key) {
            return connector.clone();
        }
        let sleep = components.sleep_impl().or_else(default_async_sleep);
        let (direct, proxied, http_proxy) = match &self.route {
            ProxyRoute::Environment => {
                let connector = self.sdk_connector(settings, sleep, Some(ProxyConfig::from_env()));
                (connector.clone(), connector, true)
            }
            ProxyRoute::Http(config) => (
                self.sdk_connector(settings, sleep.clone(), None),
                self.sdk_connector(settings, sleep, Some(config.clone())),
                true,
            ),
            ProxyRoute::Socks5(route) => (
                self.sdk_connector(settings, sleep.clone(), None),
                SharedHttpConnector::new(SocksHttpConnector::new(
                    Arc::clone(route),
                    settings,
                    sleep,
                )),
                false,
            ),
        };
        let connector = SharedHttpConnector::new(RoutingConnector {
            bypass: Arc::clone(&self.bypass),
            direct,
            proxied,
            http_proxy,
        });
        connectors.insert(key, connector.clone());
        connector
    }

    fn validate_base_client_config(
        &self,
        _: &RuntimeComponentsBuilder,
        _: &ConfigBag,
    ) -> std::result::Result<(), BoxError> {
        // Like the SDK's own client: build a connector once while the SDK
        // client is being configured, so the OS trust roots are loaded now
        // and not on an async worker during the first request.
        let warm_up = HttpConnectorSettings::builder().build();
        drop(self.sdk_connector(&warm_up, None, None));
        Ok(())
    }
}

impl fmt::Debug for ProxyHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let route = match &self.route {
            ProxyRoute::Environment => "environment",
            ProxyRoute::Http(_) => "http",
            ProxyRoute::Socks5(_) => "socks5",
        };
        f.debug_struct("ProxyHttpClient")
            .field("route", &route)
            .field("bypass", &self.bypass)
            .finish_non_exhaustive()
    }
}

/// Sends each request directly or through the proxy, by its host.
struct RoutingConnector {
    bypass: Arc<BypassList>,
    direct: SharedHttpConnector,
    proxied: SharedHttpConnector,
    http_proxy: bool,
}

impl HttpConnector for RoutingConnector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        if self.bypass.matches_uri(request.uri()) {
            return self.direct.call(request);
        }
        let response = self.proxied.call(request);
        let http_proxy = self.http_proxy;
        HttpConnectorFuture::new(async move {
            match response.await {
                // Only a proxy answers 407: the credentials were rejected.
                Ok(response) if http_proxy && response.status().as_u16() == 407 => {
                    Err(proxy_auth_rejected(None))
                }
                Ok(response) => Ok(response),
                Err(error) if is_proxy_auth_failure(&error) => {
                    Err(proxy_auth_rejected(Some(error.into_source())))
                }
                Err(error) => Err(error),
            }
        })
    }
}

impl fmt::Debug for RoutingConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RoutingConnector")
            .field("bypass", &self.bypass)
            .field("http_proxy", &self.http_proxy)
            .finish_non_exhaustive()
    }
}

/// The proxy rejected (or required) credentials.
#[derive(Debug)]
struct ProxyAuthRejected {
    source: Option<BoxError>,
}

impl fmt::Display for ProxyAuthRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the proxy rejected the proxy credentials, or requires credentials")
    }
}

impl StdError for ProxyAuthRejected {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

fn proxy_auth_rejected(source: Option<BoxError>) -> ConnectorError {
    // Not retryable: the same credentials fail again.
    ConnectorError::other(Box::new(ProxyAuthRejected { source }), None)
}

/// Where and how the SOCKS5 route connects.
struct SocksRoute {
    host: String,
    port: u16,
    auth: Option<ProxyAuth>,
    tls: Arc<rustls::ClientConfig>,
}

/// The SOCKS5 route as an [`HttpConnector`]: a pooled hyper client over
/// SOCKS5 tunnels, with the SDK's connect and read timeout semantics.
struct SocksHttpConnector {
    client: PooledClient<ConnectTimeout<hyper_rustls::HttpsConnector<SocksTunnel>>, SdkBody>,
    read_timeout: Option<(Duration, SharedAsyncSleep)>,
}

impl SocksHttpConnector {
    fn new(
        route: Arc<SocksRoute>,
        settings: &HttpConnectorSettings,
        sleep: Option<SharedAsyncSleep>,
    ) -> Self {
        let tls = Arc::clone(&route.tls);
        let https = hyper_rustls::HttpsConnector::from((SocksTunnel { route }, tls));
        let connector = ConnectTimeout {
            inner: https,
            timeout: settings.connect_timeout().zip(sleep.clone()),
        };
        let mut builder = PooledClient::builder(TokioExecutor::new());
        builder.pool_timer(TokioTimer::new());
        Self {
            client: builder.build(connector),
            read_timeout: settings.read_timeout().zip(sleep),
        }
    }
}

impl HttpConnector for SocksHttpConnector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let mut request = match request.try_into_http1x() {
            Ok(request) => request,
            Err(error) => {
                return HttpConnectorFuture::ready(Err(ConnectorError::user(error.into())));
            }
        };
        let capture = capture_connection(&mut request);
        if let Some(smithy) = request.extensions().get::<CaptureSmithyConnection>() {
            smithy.set_connection_retriever(move || connection_metadata(&capture));
        }
        let response = self.client.request(request);
        let read_timeout = self.read_timeout.clone();
        HttpConnectorFuture::new(async move {
            let response = match read_timeout {
                Some((duration, sleep)) => {
                    match Timeout::new(response, sleep.sleep(duration)).await {
                        Ok(response) => response,
                        Err(_) => {
                            return Err(ConnectorError::timeout(Box::new(TimedOut {
                                what: "waiting for the response",
                                after: duration,
                            })));
                        }
                    }
                }
                None => response.await,
            };
            let response = response.map_err(classify_socks_route_error)?;
            HttpResponse::try_from(response.map(SdkBody::from_body_1_x))
                .map_err(|error| ConnectorError::other(error.into(), None))
        })
    }
}

impl fmt::Debug for SocksHttpConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocksHttpConnector")
            .field("read_timeout", &self.read_timeout.as_ref().map(|(d, _)| d))
            .finish_non_exhaustive()
    }
}

/// Lets the SDK's retry logic poison a broken pooled connection.
fn connection_metadata(capture: &CaptureConnection) -> Option<ConnectionMetadata> {
    capture.connection_metadata().as_ref()?;
    let capture = capture.clone();
    Some(
        ConnectionMetadata::builder()
            .proxied(true)
            .poison_fn(move || {
                if let Some(connected) = capture.connection_metadata().as_ref() {
                    connected.poison();
                }
            })
            .build(),
    )
}

/// Maps hyper errors like the SDK's client does: connection-level and
/// I/O failures are retryable; rejected credentials, protocol and TLS
/// failures are not.
fn classify_socks_route_error(error: hyper_util::client::legacy::Error) -> ConnectorError {
    let error: BoxError = Box::new(error);
    match failure_class(error.as_ref()) {
        FailureClass::Timeout => ConnectorError::timeout(error),
        FailureClass::Transient => ConnectorError::io(error),
        FailureClass::User => ConnectorError::user(error),
        FailureClass::Permanent => ConnectorError::other(error, None),
    }
}

enum FailureClass {
    Timeout,
    Transient,
    User,
    Permanent,
}

fn failure_class(error: &(dyn StdError + 'static)) -> FailureClass {
    let chain = || error_chain(error);
    if chain().any(|e| e.is::<TimedOut>()) {
        return FailureClass::Timeout;
    }
    if let Some(socks) = chain().find_map(|e| e.downcast_ref::<SocksError>()) {
        return if socks.is_transient() {
            FailureClass::Transient
        } else {
            FailureClass::Permanent
        };
    }
    if chain().any(|e| e.is::<rustls::Error>()) {
        return FailureClass::Permanent;
    }
    if let Some(hyper_error) = chain().find_map(|e| e.downcast_ref::<hyper::Error>()) {
        if hyper_error.is_timeout() {
            return FailureClass::Timeout;
        }
        if hyper_error.is_user() {
            return FailureClass::User;
        }
        if hyper_error.is_closed()
            || hyper_error.is_canceled()
            || hyper_error.is_incomplete_message()
        {
            return FailureClass::Transient;
        }
    }
    let connect_failed = chain()
        .find_map(|e| e.downcast_ref::<hyper_util::client::legacy::Error>())
        .is_some_and(hyper_util::client::legacy::Error::is_connect);
    if connect_failed || chain().any(|e| e.is::<std::io::Error>()) {
        return FailureClass::Transient;
    }
    FailureClass::Permanent
}

/// The error chain, looking into `io::Error` payloads too: their
/// `source()` skips the payload itself.
fn error_chain<'a>(
    error: &'a (dyn StdError + 'static),
) -> impl Iterator<Item = &'a (dyn StdError + 'static)> {
    std::iter::successors(Some(error), |&error| {
        match error.downcast_ref::<std::io::Error>() {
            Some(io) => io.get_ref().map(|inner| inner as &(dyn StdError + 'static)),
            None => error.source(),
        }
    })
}

#[derive(Debug)]
struct TimedOut {
    what: &'static str,
    after: Duration,
}

impl fmt::Display for TimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "timed out {} after {:?}", self.what, self.after)
    }
}

impl StdError for TimedOut {}

/// Applies the connect timeout to the whole connection setup: TCP to the
/// proxy, the SOCKS5 handshake and TLS.
#[derive(Clone)]
struct ConnectTimeout<C> {
    inner: C,
    timeout: Option<(Duration, SharedAsyncSleep)>,
}

impl<C> Service<http::Uri> for ConnectTimeout<C>
where
    C: Service<http::Uri, Error = BoxError>,
    C::Future: Send + 'static,
    C::Response: Send + 'static,
{
    type Response = C::Response;
    type Error = BoxError;
    type Future = BoxFuture<C::Response>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), BoxError>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, destination: http::Uri) -> Self::Future {
        let connecting = self.inner.call(destination);
        let timeout = self.timeout.clone();
        Box::pin(async move {
            match timeout {
                Some((duration, sleep)) => Timeout::new(connecting, sleep.sleep(duration))
                    .await
                    .map_err(|_| -> BoxError {
                        Box::new(TimedOut {
                            what: "connecting through the SOCKS5 proxy",
                            after: duration,
                        })
                    })?,
                None => connecting.await,
            }
        })
    }
}

/// TCP connection to the destination through the SOCKS5 proxy.
#[derive(Clone)]
struct SocksTunnel {
    route: Arc<SocksRoute>,
}

impl Service<http::Uri> for SocksTunnel {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = BoxFuture<TokioIo<TcpStream>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: http::Uri) -> Self::Future {
        let route = Arc::clone(&self.route);
        Box::pin(async move {
            let (host, port) = socks_destination(&destination)?;
            let mut stream = TcpStream::connect((route.host.as_str(), route.port))
                .await
                .map_err(SocksError::Unreachable)?;
            stream.set_nodelay(true).map_err(SocksError::Io)?;
            socks5_connect(&mut stream, &host, port, route.auth.as_ref()).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

/// Destination host (without brackets) and port; the port defaults by
/// scheme.
fn socks_destination(destination: &http::Uri) -> std::result::Result<(String, u16), SocksError> {
    let host = destination
        .host()
        .ok_or(SocksError::Destination("the request has no host"))?;
    let port = match (destination.port_u16(), destination.scheme_str()) {
        (Some(port), _) => port,
        (None, Some("https")) => 443,
        (None, Some("http")) => 80,
        (None, _) => return Err(SocksError::Destination("unsupported URL scheme")),
    };
    Ok((strip_brackets(host).to_owned(), port))
}

const SOCKS_VERSION: u8 = 0x05;
const METHOD_NONE: u8 = 0x00;
const METHOD_USER_PASS: u8 = 0x02;
const METHOD_NO_ACCEPTABLE: u8 = 0xFF;
const USER_PASS_VERSION: u8 = 0x01;
const COMMAND_CONNECT: u8 = 0x01;
const ADDRESS_IPV4: u8 = 0x01;
const ADDRESS_DOMAIN: u8 = 0x03;
const ADDRESS_IPV6: u8 = 0x04;

/// SOCKS5 client handshake (RFC 1928) with username and password
/// authentication (RFC 1929). Host names are sent to the proxy unresolved.
pub(crate) async fn socks5_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> std::result::Result<(), SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let greeting: &[u8] = match auth {
        Some(_) => &[SOCKS_VERSION, 2, METHOD_NONE, METHOD_USER_PASS],
        None => &[SOCKS_VERSION, 1, METHOD_NONE],
    };
    stream.write_all(greeting).await?;
    let mut choice = [0u8; 2];
    stream.read_exact(&mut choice).await?;
    if choice[0] != SOCKS_VERSION {
        return Err(SocksError::Protocol("the proxy is not a SOCKS5 server"));
    }
    match (choice[1], auth) {
        (METHOD_NONE, _) => {}
        (METHOD_USER_PASS, Some(auth)) => socks5_authenticate(stream, auth).await?,
        (METHOD_NO_ACCEPTABLE, Some(_)) => {
            return Err(SocksError::Auth(
                "the proxy accepts none of the offered authentication methods",
            ));
        }
        (METHOD_NO_ACCEPTABLE, None) => {
            return Err(SocksError::Auth(
                "the proxy requires a user name and password",
            ));
        }
        _ => {
            return Err(SocksError::Protocol(
                "the proxy chose an authentication method that was not offered",
            ));
        }
    }

    let mut request = Vec::with_capacity(6 + 1 + 255);
    request.extend_from_slice(&[SOCKS_VERSION, COMMAND_CONNECT, 0x00]);
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            request.push(ADDRESS_IPV4);
            request.extend_from_slice(&address.octets());
        }
        Ok(IpAddr::V6(address)) => {
            request.push(ADDRESS_IPV6);
            request.extend_from_slice(&address.octets());
        }
        Err(_) => {
            let length = u8::try_from(host.len())
                .ok()
                .filter(|length| *length > 0)
                .ok_or(SocksError::Destination("host names must be 1 to 255 bytes"))?;
            request.push(ADDRESS_DOMAIN);
            request.push(length);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;

    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await?;
    if reply[0] != SOCKS_VERSION {
        return Err(SocksError::Protocol("invalid reply to CONNECT"));
    }
    if reply[1] != 0x00 {
        return Err(SocksError::Rejected(reply[1]));
    }
    let bound_address_len = match reply[3] {
        ADDRESS_IPV4 => 4,
        ADDRESS_IPV6 => 16,
        ADDRESS_DOMAIN => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await?;
            usize::from(length[0])
        }
        _ => return Err(SocksError::Protocol("invalid address type in the reply")),
    };
    let mut bound = vec![0u8; bound_address_len + 2];
    stream.read_exact(&mut bound).await?;
    Ok(())
}

async fn socks5_authenticate<S>(
    stream: &mut S,
    auth: &ProxyAuth,
) -> std::result::Result<(), SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let username = auth.username.as_bytes();
    let password = auth.password.as_bytes();
    let (Ok(username_len), Ok(password_len)) =
        (u8::try_from(username.len()), u8::try_from(password.len()))
    else {
        return Err(SocksError::Auth(
            "the user name and password must be at most 255 bytes",
        ));
    };
    // Sized exactly, so the buffer never reallocates and leaves no copy.
    let mut message = Zeroizing::new(Vec::with_capacity(3 + username.len() + password.len()));
    message.push(USER_PASS_VERSION);
    message.push(username_len);
    message.extend_from_slice(username);
    message.push(password_len);
    message.extend_from_slice(password);
    stream.write_all(&message).await?;
    let mut status = [0u8; 2];
    stream.read_exact(&mut status).await?;
    if status[0] != USER_PASS_VERSION {
        return Err(SocksError::Protocol(
            "invalid reply to the authentication request",
        ));
    }
    if status[1] != 0x00 {
        return Err(SocksError::Auth(
            "the proxy rejected the user name or password",
        ));
    }
    Ok(())
}

/// A SOCKS5 failure.
#[derive(Debug)]
pub(crate) enum SocksError {
    /// The proxy rejected or required credentials.
    Auth(&'static str),
    /// The proxy does not speak SOCKS5 as expected.
    Protocol(&'static str),
    /// The proxy could not connect to the destination (RFC 1928 reply code).
    Rejected(u8),
    /// The destination cannot be expressed in SOCKS5.
    Destination(&'static str),
    /// The proxy cannot be reached.
    Unreachable(std::io::Error),
    /// I/O error during the handshake.
    Io(std::io::Error),
}

impl SocksError {
    /// Whether a new connection attempt might succeed.
    fn is_transient(&self) -> bool {
        match self {
            Self::Unreachable(_) | Self::Io(_) => true,
            // General failure, network or host unreachable, refused, TTL.
            Self::Rejected(code) => matches!(code, 0x01 | 0x03 | 0x04 | 0x05 | 0x06),
            Self::Auth(_) | Self::Protocol(_) | Self::Destination(_) => false,
        }
    }
}

impl fmt::Display for SocksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(reason) => write!(f, "SOCKS5 proxy authentication failed: {reason}"),
            Self::Protocol(reason) => write!(f, "SOCKS5 protocol error: {reason}"),
            Self::Rejected(code) => {
                let reason = match code {
                    0x01 => "general failure",
                    0x02 => "connection not allowed by the proxy's rules",
                    0x03 => "network unreachable",
                    0x04 => "host unreachable",
                    0x05 => "connection refused",
                    0x06 => "TTL expired",
                    0x07 => "command not supported",
                    0x08 => "address type not supported",
                    _ => "unknown reply code",
                };
                write!(
                    f,
                    "the SOCKS5 proxy could not connect to the destination: {reason} ({code:#04x})"
                )
            }
            Self::Destination(reason) => write!(f, "invalid SOCKS5 destination: {reason}"),
            Self::Unreachable(_) => f.write_str("cannot connect to the SOCKS5 proxy"),
            Self::Io(_) => f.write_str("I/O error during the SOCKS5 handshake"),
        }
    }
}

impl StdError for SocksError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Unreachable(error) | Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SocksError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

// Error constructors of this module.

fn invalid_proxy(reason: &str) -> Error {
    Error::Usage(format!("invalid proxy settings: {reason}"))
}

fn invalid_bypass(entry: &str, reason: &str) -> Error {
    Error::Usage(format!("invalid proxy bypass entry {entry:?}: {reason}"))
}

fn invalid_certificate(reason: &str) -> Error {
    Error::Usage(format!("invalid root certificate: {reason}"))
}

fn no_trust_roots(reason: &str) -> Error {
    Error::Usage(format!("no trusted root certificates: {reason}"))
}

fn tls_setup_failed(reason: &str) -> Error {
    Error::Internal(format!("cannot configure TLS: {reason}"))
}

fn proxy_setup_failed() -> Error {
    Error::Internal("the proxy configuration could not be applied safely".to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aws_smithy_async::rt::sleep::TokioSleep;
    use aws_smithy_async::time::SystemTimeSource;
    use aws_smithy_runtime_api::client::runtime_components::RuntimeComponentsBuilder;
    use aws_smithy_types::byte_stream::ByteStream;
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    use crate::error::ErrorKind;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::*;

    const TARGET_HOST: &str = "target.test";
    const USERNAME: &str = "proxy user";
    const PASSWORD: &str = "p@ss:w/rd #%?&=+ \u{e4}";

    // ---- test PKI ----------------------------------------------------------

    struct TestPki {
        ca_pem: String,
        server: Arc<rustls::ServerConfig>,
    }

    fn test_pki() -> TestPki {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let server_cert = CertificateParams::new(vec![
            TARGET_HOST.to_owned(),
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])
        .unwrap()
        .signed_by(&server_key, &ca)
        .unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![server_cert.der().clone()], key)
        .unwrap();
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        TestPki {
            ca_pem: ca.pem(),
            server: Arc::new(server),
        }
    }

    // ---- target server -----------------------------------------------------

    #[derive(Debug, Clone)]
    struct SeenRequest {
        line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl SeenRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// A keep-alive HTTP/1.1 server (plain or TLS) answering every request
    /// with `status` and `body`.
    struct Target {
        port: u16,
        connections: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<SeenRequest>>>,
    }

    impl Target {
        async fn start(tls: Option<Arc<rustls::ServerConfig>>, status: u16, body: &str) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (count, seen, body) = (connections.clone(), requests.clone(), body.to_owned());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    count.fetch_add(1, Ordering::SeqCst);
                    let (seen, body, tls) = (seen.clone(), body.clone(), tls.clone());
                    tokio::spawn(async move {
                        match tls {
                            Some(config) => {
                                if let Ok(stream) = TlsAcceptor::from(config).accept(stream).await {
                                    serve_http(stream, seen, status, body).await;
                                }
                            }
                            None => serve_http(stream, seen, status, body).await,
                        }
                    });
                }
            });
            Self {
                port,
                connections,
                requests,
            }
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<SeenRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    async fn serve_http<S: AsyncRead + AsyncWrite + Unpin>(
        mut stream: S,
        seen: Arc<Mutex<Vec<SeenRequest>>>,
        status: u16,
        body: String,
    ) {
        let mut buffer = Vec::new();
        while let Some((head, rest)) = read_head(&mut stream, std::mem::take(&mut buffer)).await {
            let (line, headers) = parse_head(&head);
            let length = headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            let mut request_body = rest;
            while request_body.len() < length {
                let mut chunk = [0u8; 4096];
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => request_body.extend_from_slice(&chunk[..n]),
                }
            }
            buffer = request_body.split_off(length);
            seen.lock().unwrap().push(SeenRequest {
                line,
                headers,
                body: request_body,
            });
            let response = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: text/xml\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    /// Reads up to the end of a request or response head. Returns the head
    /// and any bytes read beyond it; `None` at end of stream.
    async fn read_head<S: AsyncRead + Unpin>(
        stream: &mut S,
        mut buffer: Vec<u8>,
    ) -> Option<(String, Vec<u8>)> {
        loop {
            if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let rest = buffer.split_off(end + 4);
                return Some((String::from_utf8_lossy(&buffer).into_owned(), rest));
            }
            if buffer.len() > 64 * 1024 {
                return None;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn parse_head(head: &str) -> (String, Vec<(String, String)>) {
        let mut lines = head.split("\r\n").filter(|l| !l.is_empty());
        let line = lines.next().unwrap_or_default().to_owned();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
            .collect();
        (line, headers)
    }

    /// Test host names resolve to the loopback address, as only the proxy
    /// would resolve them.
    fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
        let host = strip_brackets(host);
        if host == TARGET_HOST {
            return Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        }
        host.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, port))
    }

    // ---- HTTP proxy --------------------------------------------------------

    #[derive(Debug, Clone)]
    struct ProxyRequest {
        line: String,
        authorization: Option<String>,
    }

    /// An HTTP proxy: CONNECT tunnels and absolute-form forwarding, with
    /// optional Basic authentication.
    struct HttpProxy {
        port: u16,
        connections: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<ProxyRequest>>>,
    }

    impl HttpProxy {
        async fn start(credentials: Option<(&str, &str)>) -> Self {
            let expected = credentials.map(|(user, password)| {
                format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
            });
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (count, seen) = (connections.clone(), requests.clone());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(proxy_http(stream, seen.clone(), expected.clone()));
                }
            });
            Self {
                port,
                connections,
                requests,
            }
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<ProxyRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    async fn proxy_http(
        mut client: TcpStream,
        seen: Arc<Mutex<Vec<ProxyRequest>>>,
        expected: Option<String>,
    ) {
        let Some((head, rest)) = read_head(&mut client, Vec::new()).await else {
            return;
        };
        let (line, headers) = parse_head(&head);
        let authorization = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("proxy-authorization"))
            .map(|(_, v)| v.clone());
        seen.lock().unwrap().push(ProxyRequest {
            line: line.clone(),
            authorization: authorization.clone(),
        });
        if expected.is_some() && authorization != expected {
            let _ = client
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                      proxy-authenticate: Basic realm=\"test\"\r\ncontent-length: 0\r\n\r\n",
                )
                .await;
            return;
        }
        let mut parts = line.split(' ');
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if method == "CONNECT" {
            let Some((host, port)) = target.rsplit_once(':') else {
                return;
            };
            let Some(address) = resolve(host, port.parse().unwrap_or(0)) else {
                return;
            };
            let Ok(mut upstream) = TcpStream::connect(address).await else {
                return;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            return;
        }
        // Forward proxying: rewrite the absolute-form target.
        let Ok(uri) = target.parse::<http::Uri>() else {
            return;
        };
        let (Some(host), Some(port)) = (uri.host(), uri.port_u16()) else {
            return;
        };
        let Some(address) = resolve(host, port) else {
            return;
        };
        let Ok(mut upstream) = TcpStream::connect(address).await else {
            return;
        };
        let path = uri.path_and_query().map_or("/", |p| p.as_str());
        let mut forwarded = format!("{method} {path} HTTP/1.1\r\n");
        for (name, value) in &headers {
            if !name.eq_ignore_ascii_case("proxy-authorization") {
                forwarded.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        forwarded.push_str("\r\n");
        if upstream.write_all(forwarded.as_bytes()).await.is_err()
            || upstream.write_all(&rest).await.is_err()
        {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }

    // ---- SOCKS5 proxy ------------------------------------------------------

    /// A SOCKS5 proxy with optional username and password authentication
    /// that records the requested destinations.
    struct Socks5Proxy {
        port: u16,
        connections: Arc<AtomicUsize>,
        destinations: Arc<Mutex<Vec<(String, u16)>>>,
    }

    impl Socks5Proxy {
        async fn start(credentials: Option<(&str, &str)>) -> Self {
            let credentials =
                credentials.map(|(u, p)| (u.as_bytes().to_vec(), p.as_bytes().to_vec()));
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let connections = Arc::new(AtomicUsize::new(0));
            let destinations = Arc::new(Mutex::new(Vec::new()));
            let (count, seen) = (connections.clone(), destinations.clone());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(proxy_socks(stream, seen.clone(), credentials.clone()));
                }
            });
            Self {
                port,
                connections,
                destinations,
            }
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }

        fn destinations(&self) -> Vec<(String, u16)> {
            self.destinations.lock().unwrap().clone()
        }
    }

    async fn proxy_socks(
        mut client: TcpStream,
        seen: Arc<Mutex<Vec<(String, u16)>>>,
        credentials: Option<(Vec<u8>, Vec<u8>)>,
    ) -> std::io::Result<()> {
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await?;
        let mut methods = vec![0u8; usize::from(head[1])];
        client.read_exact(&mut methods).await?;
        match &credentials {
            Some((user, password)) => {
                if !methods.contains(&METHOD_USER_PASS) {
                    return client
                        .write_all(&[SOCKS_VERSION, METHOD_NO_ACCEPTABLE])
                        .await;
                }
                client.write_all(&[SOCKS_VERSION, METHOD_USER_PASS]).await?;
                let mut version_and_len = [0u8; 2];
                client.read_exact(&mut version_and_len).await?;
                let mut got_user = vec![0u8; usize::from(version_and_len[1])];
                client.read_exact(&mut got_user).await?;
                let mut password_len = [0u8; 1];
                client.read_exact(&mut password_len).await?;
                let mut got_password = vec![0u8; usize::from(password_len[0])];
                client.read_exact(&mut got_password).await?;
                if &got_user != user || &got_password != password {
                    return client.write_all(&[USER_PASS_VERSION, 0x01]).await;
                }
                client.write_all(&[USER_PASS_VERSION, 0x00]).await?;
            }
            None => client.write_all(&[SOCKS_VERSION, METHOD_NONE]).await?,
        }
        let mut request = [0u8; 4];
        client.read_exact(&mut request).await?;
        let host = match request[3] {
            ADDRESS_IPV4 => {
                let mut octets = [0u8; 4];
                client.read_exact(&mut octets).await?;
                Ipv4Addr::from(octets).to_string()
            }
            ADDRESS_IPV6 => {
                let mut octets = [0u8; 16];
                client.read_exact(&mut octets).await?;
                std::net::Ipv6Addr::from(octets).to_string()
            }
            _ => {
                let mut length = [0u8; 1];
                client.read_exact(&mut length).await?;
                let mut name = vec![0u8; usize::from(length[0])];
                client.read_exact(&mut name).await?;
                String::from_utf8_lossy(&name).into_owned()
            }
        };
        let mut port = [0u8; 2];
        client.read_exact(&mut port).await?;
        let port = u16::from_be_bytes(port);
        seen.lock().unwrap().push((host.clone(), port));
        let upstream = match resolve(&host, port) {
            Some(address) => TcpStream::connect(address).await.ok(),
            None => None,
        };
        let Some(mut upstream) = upstream else {
            return client
                .write_all(&[SOCKS_VERSION, 0x05, 0, ADDRESS_IPV4, 0, 0, 0, 0, 0, 0])
                .await;
        };
        client
            .write_all(&[
                SOCKS_VERSION,
                0x00,
                0,
                ADDRESS_IPV4,
                127,
                0,
                0,
                1,
                0x1f,
                0x90,
            ])
            .await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        Ok(())
    }

    // ---- client helpers ----------------------------------------------------

    fn settings(
        kind: ProxyKind,
        port: u16,
        username: Option<&str>,
        bypass: &[&str],
    ) -> ProxySettings {
        ProxySettings {
            enabled: true,
            kind,
            host: "127.0.0.1".to_owned(),
            port,
            username: username.map(str::to_owned),
            password_ref: username.map(|_| "proxy/password/1".to_owned()),
            bypass: bypass.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// A client trusting the test CA. The proxy environment variables of the
    /// machine running the tests are ignored, so results do not depend on it.
    fn client(pki: &TestPki, settings: &ProxySettings, password: Option<&str>) -> SharedHttpClient {
        HttpClientBuilder::new()
            .proxy(settings, password)
            .unwrap()
            .native_roots(false)
            .environment_proxy(false)
            .add_root_certificates_pem(pki.ca_pem.as_bytes())
            .unwrap()
            .build()
            .unwrap()
    }

    fn components() -> RuntimeComponents {
        RuntimeComponentsBuilder::for_tests()
            .with_sleep_impl(Some(TokioSleep::new()))
            .with_time_source(Some(SystemTimeSource::new()))
            .build()
            .unwrap()
    }

    fn connector(client: &SharedHttpClient) -> SharedHttpConnector {
        let settings = HttpConnectorSettings::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5))
            .build();
        client.http_connector(&settings, &components())
    }

    async fn get(
        client: &SharedHttpClient,
        uri: &str,
    ) -> std::result::Result<(u16, String), ConnectorError> {
        let response = connector(client)
            .call(HttpRequest::get(uri).unwrap())
            .await?;
        let status = response.status().as_u16();
        let body = ByteStream::new(response.into_body())
            .collect()
            .await
            .unwrap();
        Ok((
            status,
            String::from_utf8(body.into_bytes().to_vec()).unwrap(),
        ))
    }

    /// Sequential requests in the pooling checks.
    const REQUESTS: usize = 4;

    /// hyper returns a connection to its pool from a background task once
    /// the response is complete; give that task a moment before reusing.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // ---- end-to-end tests --------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_proxy_tunnels_https_with_basic_auth() {
        let pki = test_pki();
        let target = Target::start(Some(pki.server.clone()), 200, "hello over tls").await;
        let proxy = HttpProxy::start(Some((USERNAME, PASSWORD))).await;
        let client = client(
            &pki,
            &settings(ProxyKind::Http, proxy.port, Some(USERNAME), &[]),
            Some(PASSWORD),
        );

        let uri = format!("https://{TARGET_HOST}:{}/hello", target.port);
        for _ in 0..REQUESTS {
            assert_eq!(
                get(&client, &uri).await.unwrap(),
                (200, "hello over tls".to_owned())
            );
            settle().await;
        }
        // Tunnels are pooled and reused.
        assert!(proxy.connections() < REQUESTS, "{}", proxy.connections());
        assert_eq!(target.connections(), proxy.connections());

        let expected = format!(
            "Basic {}",
            STANDARD.encode(format!("{USERNAME}:{PASSWORD}"))
        );
        for seen in proxy.requests() {
            assert_eq!(
                seen.line,
                format!("CONNECT {TARGET_HOST}:{} HTTP/1.1", target.port)
            );
            assert_eq!(seen.authorization.as_deref(), Some(expected.as_str()));
        }
        let requests = target.requests();
        assert_eq!(requests.len(), REQUESTS);
        assert_eq!(requests[0].line, "GET /hello HTTP/1.1");
        assert_eq!(requests[0].header("proxy-authorization"), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_proxy_forwards_plain_http() {
        let pki = test_pki();
        let target = Target::start(None, 200, "hello in the clear").await;
        let proxy = HttpProxy::start(Some((USERNAME, PASSWORD))).await;
        let client = client(
            &pki,
            &settings(ProxyKind::Http, proxy.port, Some(USERNAME), &[]),
            Some(PASSWORD),
        );

        let uri = format!("http://{TARGET_HOST}:{}/plain?x=1", target.port);
        assert_eq!(
            get(&client, &uri).await.unwrap(),
            (200, "hello in the clear".to_owned())
        );
        let seen = proxy.requests();
        assert_eq!(seen[0].line, format!("GET {uri} HTTP/1.1"));
        assert!(
            seen[0]
                .authorization
                .as_deref()
                .is_some_and(|a| a.starts_with("Basic "))
        );
        assert_eq!(target.requests()[0].line, "GET /plain?x=1 HTTP/1.1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_proxy_rejects_wrong_or_missing_credentials() {
        let pki = test_pki();
        let tls_target = Target::start(Some(pki.server.clone()), 200, "secret").await;
        let plain_target = Target::start(None, 200, "secret").await;
        let proxy = HttpProxy::start(Some((USERNAME, PASSWORD))).await;
        for (username, password) in [(Some(USERNAME), Some("wrong")), (None, None)] {
            let client = client(
                &pki,
                &settings(ProxyKind::Http, proxy.port, username, &[]),
                password,
            );
            for uri in [
                format!("https://{TARGET_HOST}:{}/", tls_target.port),
                format!("http://{TARGET_HOST}:{}/", plain_target.port),
            ] {
                let error = get(&client, &uri).await.unwrap_err();
                assert!(is_proxy_auth_failure(&error), "{uri}: {error:?}");
                assert!(error.is_other(), "not retryable: {error:?}");
            }
        }
        assert_eq!(tls_target.connections(), 0);
        assert_eq!(plain_target.connections(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socks5_proxy_with_auth_reaches_https_and_http() {
        let pki = test_pki();
        let tls_target = Target::start(Some(pki.server.clone()), 200, "socks tls").await;
        let plain_target = Target::start(None, 200, "socks plain").await;
        let proxy = Socks5Proxy::start(Some((USERNAME, PASSWORD))).await;
        let client = client(
            &pki,
            &settings(ProxyKind::Socks5, proxy.port, Some(USERNAME), &[]),
            Some(PASSWORD),
        );

        let https = format!("https://{TARGET_HOST}:{}/a", tls_target.port);
        assert_eq!(
            get(&client, &https).await.unwrap(),
            (200, "socks tls".to_owned())
        );
        settle().await;
        for _ in 1..REQUESTS {
            assert_eq!(get(&client, &https).await.unwrap().0, 200);
            settle().await;
        }
        let http = format!("http://{TARGET_HOST}:{}/b", plain_target.port);
        assert_eq!(
            get(&client, &http).await.unwrap(),
            (200, "socks plain".to_owned())
        );

        // Host names reach the proxy unresolved; the TLS tunnel is pooled.
        let destinations = proxy.destinations();
        let tls_tunnels = destinations
            .iter()
            .filter(|d| **d == (TARGET_HOST.to_owned(), tls_target.port))
            .count();
        assert!((1..REQUESTS).contains(&tls_tunnels), "{destinations:?}");
        assert_eq!(destinations.len(), tls_tunnels + 1, "{destinations:?}");
        assert!(destinations.contains(&(TARGET_HOST.to_owned(), plain_target.port)));
        assert_eq!(proxy.connections(), destinations.len());
        assert_eq!(tls_target.requests().len(), REQUESTS);
        assert_eq!(tls_target.requests()[0].line, "GET /a HTTP/1.1");
        assert_eq!(plain_target.requests()[0].line, "GET /b HTTP/1.1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socks5_proxy_rejects_wrong_or_missing_credentials() {
        let pki = test_pki();
        let target = Target::start(None, 200, "secret").await;
        let proxy = Socks5Proxy::start(Some((USERNAME, PASSWORD))).await;
        for (username, password) in [(Some(USERNAME), Some("wrong")), (None, None)] {
            let client = client(
                &pki,
                &settings(ProxyKind::Socks5, proxy.port, username, &[]),
                password,
            );
            let error = get(&client, &format!("http://{TARGET_HOST}:{}/", target.port))
                .await
                .unwrap_err();
            assert!(is_proxy_auth_failure(&error), "{error:?}");
            assert!(error.is_other(), "not retryable: {error:?}");
        }
        assert_eq!(target.connections(), 0);
        assert!(proxy.destinations().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socks5_proxy_without_auth_and_with_ip_destinations() {
        let pki = test_pki();
        let target = Target::start(Some(pki.server.clone()), 200, "by address").await;
        let proxy = Socks5Proxy::start(None).await;
        let client = client(
            &pki,
            &settings(ProxyKind::Socks5, proxy.port, None, &[]),
            None,
        );
        let uri = format!("https://127.0.0.1:{}/", target.port);
        assert_eq!(get(&client, &uri).await.unwrap().1, "by address");
        assert_eq!(
            proxy.destinations(),
            vec![("127.0.0.1".to_owned(), target.port)]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bypassed_hosts_connect_directly() {
        let pki = test_pki();
        let target = Target::start(None, 200, "direct").await;
        let tls_target = Target::start(Some(pki.server.clone()), 200, "direct tls").await;
        let http_proxy = HttpProxy::start(None).await;
        let socks_proxy = Socks5Proxy::start(None).await;
        let bypass = ["127.0.0.0/8", "localhost"];
        for (kind, port) in [
            (ProxyKind::Http, http_proxy.port),
            (ProxyKind::Socks5, socks_proxy.port),
        ] {
            let client = client(&pki, &settings(kind, port, None, &bypass), None);
            for uri in [
                format!("http://127.0.0.1:{}/", target.port),
                format!("http://localhost:{}/", target.port),
                format!("https://localhost:{}/", tls_target.port),
            ] {
                assert_eq!(get(&client, &uri).await.unwrap().0, 200, "{uri}");
            }
            // Not bypassed: through the proxy.
            let through = format!("http://{TARGET_HOST}:{}/", target.port);
            assert_eq!(get(&client, &through).await.unwrap().1, "direct");
        }
        assert_eq!(http_proxy.connections(), 1);
        assert_eq!(socks_proxy.connections(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_proxy_connects_directly() {
        let pki = test_pki();
        let target = Target::start(Some(pki.server.clone()), 200, "no proxy").await;
        let proxy = HttpProxy::start(None).await;
        let mut disabled = settings(ProxyKind::Http, proxy.port, None, &[]);
        disabled.enabled = false;
        disabled.host = String::new();
        let client = client(&pki, &disabled, None);
        let uri = format!("https://localhost:{}/", target.port);
        assert_eq!(get(&client, &uri).await.unwrap().1, "no proxy");
        assert_eq!(proxy.connections(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unreachable_socks_proxy_is_retryable_io() {
        let pki = test_pki();
        let unused = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        let client = client(&pki, &settings(ProxyKind::Socks5, port, None, &[]), None);
        let error = get(&client, "http://target.test:1/").await.unwrap_err();
        assert!(error.is_io(), "{error:?}");
        assert!(!is_proxy_auth_failure(&error));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn untrusted_certificate_fails_through_the_tunnel() {
        let pki = test_pki();
        let other_pki = test_pki();
        let target = Target::start(Some(pki.server.clone()), 200, "x").await;
        let socks = Socks5Proxy::start(None).await;
        let http = HttpProxy::start(None).await;
        let uri = format!("https://{TARGET_HOST}:{}/", target.port);
        // SOCKS5 route: a certificate failure is not retried.
        let socks_client = client(
            &other_pki,
            &settings(ProxyKind::Socks5, socks.port, None, &[]),
            None,
        );
        let error = get(&socks_client, &uri).await.unwrap_err();
        assert!(!error.is_io() && !error.is_timeout(), "{error:?}");
        // HTTP route: the SDK's own client classifies it.
        let http_client = client(
            &other_pki,
            &settings(ProxyKind::Http, http.port, None, &[]),
            None,
        );
        assert!(get(&http_client, &uri).await.is_err());
        assert!(target.requests().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mfa_login_goes_through_the_proxy() {
        const RESPONSE: &str = "<GetSessionTokenResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">\
            <GetSessionTokenResult><Credentials>\
            <AccessKeyId>ASIAVIAPROXYEXAMPLE</AccessKeyId>\
            <SecretAccessKey>temporary-secret-via-proxy</SecretAccessKey>\
            <SessionToken>temporary-token-via-proxy</SessionToken>\
            <Expiration>2099-01-01T00:00:00Z</Expiration>\
            </Credentials></GetSessionTokenResult>\
            <ResponseMetadata><RequestId>e2e</RequestId></ResponseMetadata>\
            </GetSessionTokenResponse>";
        let pki = test_pki();
        let target = Target::start(Some(pki.server.clone()), 200, RESPONSE).await;
        let proxy = HttpProxy::start(Some((USERNAME, PASSWORD))).await;
        let http_client = client(
            &pki,
            &settings(ProxyKind::Http, proxy.port, Some(USERNAME), &[]),
            Some(PASSWORD),
        );
        let account = crate::model::Account {
            source: crate::model::CredentialSource::StaticKeys,
            id: "account-e2e".to_owned(),
            label: "e2e".to_owned(),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_ref: "account-e2e/secret/1".to_owned(),
            session_token_ref: None,
            region: "us-east-1".to_owned(),
            endpoint: Some(format!("https://{TARGET_HOST}:{}", target.port)),
            path_style: true,
            request_payer: false,
            mfa_serial: Some("arn:aws:iam::123456789012:mfa/example-device".to_owned()),
        };
        let long_term = crate::client::Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: None,
        };
        let sts = crate::mfa::sts_client(&account, &long_term, Some(http_client)).unwrap();
        let sessions = crate::mfa::MfaSessions::new();
        crate::mfa::login(&sessions, &account, &sts, "123456", None)
            .await
            .unwrap();
        let credentials = sessions.credentials(&account).unwrap();
        assert_eq!(credentials.access_key_id, "ASIAVIAPROXYEXAMPLE");
        assert_eq!(
            proxy.requests()[0].line,
            format!("CONNECT {TARGET_HOST}:{} HTTP/1.1", target.port)
        );
        let request = &target.requests()[0];
        assert_eq!(request.line, "POST / HTTP/1.1");
        let body = String::from_utf8_lossy(&request.body).into_owned();
        assert!(body.contains("Action=GetSessionToken") && body.contains("TokenCode=123456"));
        assert!(!body.contains("wJalr"));
    }

    // ---- unit tests --------------------------------------------------------

    #[test]
    fn bypass_rules() {
        let list = BypassList::parse(&[
            "s3.example.com",
            "*.internal.test",
            ".corp.test",
            "10.0.0.0/8",
            "192.168.1.7",
            "FD00::/8",
            "[::1]",
            "Localhost",
            "  ",
            "Trailing.Dot.Test.",
        ])
        .unwrap();
        for host in [
            "s3.example.com",
            "S3.EXAMPLE.COM",
            "s3.example.com.",
            "a.internal.test",
            "a.b.internal.test",
            "x.corp.test",
            "10.1.2.3",
            "192.168.1.7",
            "[fd12::1]",
            "fd12::1",
            "[::1]",
            "localhost",
            "minio.localhost",
            "127.0.0.1",
            "127.9.9.9",
            "[::ffff:10.9.9.9]",
            "trailing.dot.test",
        ] {
            assert!(list.matches_host(host), "{host} should bypass");
        }
        for host in [
            "example.com",
            "x.s3.example.com",
            "internal.test",
            "corp.test",
            "xcorp.test",
            "11.0.0.1",
            "192.168.1.8",
            "fe80::1",
            "localhost.example.com",
            "evil-internal.test",
            "",
        ] {
            assert!(!list.matches_host(host), "{host} should use the proxy");
        }
        assert!(
            BypassList::parse(&["*"])
                .unwrap()
                .matches_host("anything.test")
        );
        assert!(!BypassList::default().matches_host("localhost"));
        assert!(list.matches_uri("https://bucket.a.internal.test:9000/key"));
        assert!(!list.matches_uri("not a uri"));
    }

    #[test]
    fn invalid_bypass_entries() {
        for entry in [
            "example.com:8080",
            "http://example.com",
            "ex*mple.com",
            "*example.com",
            "a..b",
            "10.0.0.0/33",
            "fd00::/129",
            "example.com/8",
            "10.0.0.0/x",
            "*.",
            "under score.test",
        ] {
            let error = BypassList::parse(&[entry]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{entry}");
            assert!(error.to_string().contains(entry), "{error}");
        }
    }

    #[test]
    fn proxy_settings_validation() {
        let base = settings(ProxyKind::Http, 3128, None, &[]);
        assert!(validate_proxy_settings(&base).is_ok());
        let disabled = ProxySettings {
            bypass: vec!["bad:entry".to_owned()],
            ..ProxySettings::default()
        };
        assert!(validate_proxy_settings(&disabled).is_ok());

        let cases: Vec<fn(&mut ProxySettings)> = vec![
            |s| s.host = String::new(),
            |s| s.host = "http://proxy.test".to_owned(),
            |s| s.host = "proxy.test:3128".to_owned(),
            |s| s.host = "proxy test".to_owned(),
            |s| s.host = "user:pw@proxy.test".to_owned(),
            |s| s.host = "pw@proxy.test".to_owned(),
            |s| s.port = 0,
            |s| s.bypass = vec!["bad:entry".to_owned()],
            |s| s.username = Some("user:name".to_owned()),
        ];
        for (index, mutate) in cases.iter().enumerate() {
            let mut invalid = base.clone();
            mutate(&mut invalid);
            assert_eq!(
                validate_proxy_settings(&invalid).unwrap_err().kind(),
                ErrorKind::Usage,
                "case {index}"
            );
        }
        // A password typed into the host field is rejected without being echoed.
        for host in [
            "alice:s3cr3t-proxy-pw@proxy.example.com",
            "s3cr3t-proxy-pw@proxy",
            "alice:s3cr3t-proxy-pw",
            "http://alice:s3cr3t-proxy-pw@proxy.example.com:3128",
        ] {
            let mut invalid = base.clone();
            invalid.host = host.to_owned();
            let error = validate_proxy_settings(&invalid).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{host}");
            assert!(!error.to_string().contains("s3cr3t-proxy-pw"), "{error}");
            assert!(error.to_string().contains("proxy host"), "{error}");
            // Nor does building a client echo it.
            let error = build_http_client(&invalid, None).unwrap_err();
            assert!(!error.to_string().contains("s3cr3t-proxy-pw"), "{error}");
        }
        let mut socks = settings(ProxyKind::Socks5, 1080, Some(&"u".repeat(256)), &[]);
        assert!(validate_proxy_settings(&socks).is_err());
        socks.username = Some("user:with:colons".to_owned());
        assert!(validate_proxy_settings(&socks).is_ok());
        let mut v6 = base.clone();
        v6.host = "[::1]".to_owned();
        let plan = ProxyPlan::from_settings(&v6, None).unwrap().unwrap();
        assert_eq!(plan.authority(), "[::1]:3128");
        let mut empty_user = base;
        empty_user.username = Some(String::new());
        assert!(
            ProxyPlan::from_settings(&empty_user, Some("x"))
                .unwrap()
                .unwrap()
                .auth
                .is_none()
        );
    }

    #[test]
    fn credentials_survive_the_proxy_url() {
        for password in [PASSWORD, "", "%41", "a@b:c/d?e#f[g]h", "\u{1f511}"] {
            let mut settings = settings(ProxyKind::Http, 3128, Some("user.name-1_~"), &[]);
            settings.host = "proxy.example.test".to_owned();
            let plan = ProxyPlan::from_settings(&settings, Some(password))
                .unwrap()
                .unwrap();
            assert!(http_proxy_config(&plan).is_ok(), "{password:?}");
        }
    }

    #[test]
    fn debug_output_has_no_password() {
        let builder = HttpClientBuilder::new()
            .proxy(
                &settings(ProxyKind::Http, 3128, Some(USERNAME), &[]),
                Some(PASSWORD),
            )
            .unwrap();
        let rendered = format!("{builder:?}");
        assert!(!rendered.contains("p@ss"), "{rendered}");
        let client = builder.native_roots(true).build().unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("p@ss"), "{rendered}");
    }

    #[test]
    fn root_certificates_are_validated() {
        assert_eq!(
            HttpClientBuilder::new()
                .add_root_certificates_pem(b"not a certificate")
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
        assert_eq!(
            HttpClientBuilder::new()
                .native_roots(false)
                .build()
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn socks_destination_defaults_ports_by_scheme() {
        let cases = [
            ("http://example.test/", ("example.test", 80)),
            ("https://example.test/", ("example.test", 443)),
            ("http://example.test:9000/", ("example.test", 9000)),
            ("https://[::1]/", ("::1", 443)),
        ];
        for (uri, (host, port)) in cases {
            let parsed = socks_destination(&uri.parse().unwrap()).unwrap();
            assert_eq!(parsed, (host.to_owned(), port), "{uri}");
        }
        assert!(socks_destination(&"ftp://example.test/".parse().unwrap()).is_err());
    }

    /// Runs the client handshake against a scripted proxy and returns what
    /// the client sent.
    async fn scripted_handshake(
        host: &str,
        auth: Option<&ProxyAuth>,
        replies: &[u8],
    ) -> (std::result::Result<(), SocksError>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(4096);
        server.write_all(replies).await.unwrap();
        // End of the scripted replies: the client sees EOF after them.
        server.shutdown().await.unwrap();
        let result = socks5_connect(&mut client, host, 443, auth).await;
        drop(client);
        let mut sent = Vec::new();
        server.read_to_end(&mut sent).await.unwrap();
        (result, sent)
    }

    #[tokio::test]
    async fn socks5_handshake_messages() {
        let success = [
            SOCKS_VERSION,
            0,
            SOCKS_VERSION,
            0,
            0,
            ADDRESS_IPV4,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        let (result, sent) = scripted_handshake("example.test", None, &success).await;
        result.unwrap();
        let mut expected = vec![
            SOCKS_VERSION,
            1,
            METHOD_NONE,
            SOCKS_VERSION,
            COMMAND_CONNECT,
            0,
            ADDRESS_DOMAIN,
            12,
        ];
        expected.extend_from_slice(b"example.test");
        expected.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(sent, expected);

        let (result, sent) = scripted_handshake("::1", None, &success).await;
        result.unwrap();
        assert_eq!(sent[6], ADDRESS_IPV6);
        assert_eq!(sent.len(), 3 + 4 + 16 + 2);

        let auth = ProxyAuth::new(ProxyKind::Socks5, "user", "secret").unwrap();
        let replies = [
            SOCKS_VERSION,
            METHOD_USER_PASS,
            USER_PASS_VERSION,
            0,
            SOCKS_VERSION,
            0,
            0,
            ADDRESS_DOMAIN,
            3,
            b'a',
            b'b',
            b'c',
            0,
            0,
        ];
        let (result, sent) = scripted_handshake("10.1.2.3", Some(&auth), &replies).await;
        result.unwrap();
        assert_eq!(
            &sent[..4],
            &[SOCKS_VERSION, 2, METHOD_NONE, METHOD_USER_PASS]
        );
        assert_eq!(&sent[4..17], b"\x01\x04user\x06secret");
        assert_eq!(
            &sent[17..21],
            &[SOCKS_VERSION, COMMAND_CONNECT, 0, ADDRESS_IPV4]
        );
    }

    #[tokio::test]
    async fn socks5_handshake_failures() {
        let auth = ProxyAuth::new(ProxyKind::Socks5, "user", "secret").unwrap();
        let cases: Vec<(Option<&ProxyAuth>, Vec<u8>, &str)> = vec![
            (
                Some(&auth),
                vec![SOCKS_VERSION, METHOD_USER_PASS, USER_PASS_VERSION, 1],
                "authentication failed",
            ),
            (
                None,
                vec![SOCKS_VERSION, METHOD_NO_ACCEPTABLE],
                "requires a user name",
            ),
            (None, vec![SOCKS_VERSION, METHOD_USER_PASS], "not offered"),
            (None, vec![4, 0], "not a SOCKS5 server"),
            (
                None,
                vec![
                    SOCKS_VERSION,
                    0,
                    SOCKS_VERSION,
                    0x05,
                    0,
                    ADDRESS_IPV4,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
                "connection refused",
            ),
            (
                None,
                vec![SOCKS_VERSION, 0, SOCKS_VERSION, 0, 0, 0x09],
                "invalid address type",
            ),
            (None, vec![SOCKS_VERSION], "I/O error"),
        ];
        for (auth, replies, expected) in cases {
            let (result, _) = scripted_handshake("example.test", auth, &replies).await;
            let error = result.unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
        let long_host = "a".repeat(256);
        let (result, _) = scripted_handshake(&long_host, None, &[SOCKS_VERSION, 0]).await;
        assert!(matches!(result, Err(SocksError::Destination(_))));
        assert!(!SocksError::Auth("x").is_transient());
        assert!(SocksError::Rejected(0x05).is_transient());
        assert!(!SocksError::Rejected(0x02).is_transient());
    }

    #[test]
    fn proxy_auth_detection_walks_the_chain() {
        let rejected = proxy_auth_rejected(None);
        assert!(is_proxy_auth_failure(&rejected));
        let wrapped = std::io::Error::other(SocksError::Auth("rejected"));
        assert!(is_proxy_auth_failure(&wrapped));
        let other = std::io::Error::other(SocksError::Rejected(0x05));
        assert!(!is_proxy_auth_failure(&other));
    }

    #[test]
    fn handles_are_thread_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HttpClientBuilder>();
        assert_send_sync::<BypassList>();
    }

    #[test]
    fn mapped_ranges_match_ipv4_hosts() {
        let list = BypassList::parse(&["::ffff:192.0.2.0/120", "[::ffff:198.51.100.7]"]).unwrap();
        for host in ["192.0.2.77", "[::ffff:192.0.2.1]", "198.51.100.7"] {
            assert!(list.matches_host(host), "{host} should bypass");
        }
        for host in ["192.0.3.1", "198.51.100.8", "[::1]"] {
            assert!(!list.matches_host(host), "{host} should use the proxy");
        }
        assert_eq!(
            parse_bypass_rule("::ffff:10.0.0.0/104").unwrap(),
            BypassRule::Network(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 0)), 8)
        );
        assert_eq!(
            parse_bypass_rule("fd00::/8").unwrap(),
            BypassRule::Network("fd00::".parse().unwrap(), 8)
        );
    }

    /// Set in the child process of
    /// `disabled_settings_follow_the_environment_by_default`: `default` or
    /// `ignore`.
    const ENV_PROXY_CHILD: &str = "BB_TEST_ENV_PROXY_CHILD";
    const ENV_PROXY_TARGET_PORT: &str = "BB_TEST_ENV_PROXY_TARGET_PORT";

    /// The child half of `disabled_settings_follow_the_environment_by_default`:
    /// sends one request with the proxy settings disabled. It does nothing
    /// unless the parent started it with the markers (the environment of this
    /// process cannot be changed from safe code, so the parent starts a copy of
    /// the test binary with `HTTP_PROXY` set).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn environment_proxy_child() {
        let Ok(mode) = std::env::var(ENV_PROXY_CHILD) else {
            return;
        };
        let port = std::env::var(ENV_PROXY_TARGET_PORT).unwrap();
        let builder = HttpClientBuilder::new()
            .proxy(&ProxySettings::default(), None)
            .unwrap();
        let client = match mode.as_str() {
            "default" => builder,
            "ignore" => builder.environment_proxy(false),
            other => panic!("unknown mode {other}"),
        }
        .build()
        .unwrap();
        // Through the proxy the target is reached by a name that only the
        // proxy resolves (see `resolve`); directly, by its address.
        let host = if mode == "default" {
            TARGET_HOST
        } else {
            "127.0.0.1"
        };
        let result = get(&client, &format!("http://{host}:{port}/environment")).await;
        assert_eq!(
            result.unwrap(),
            (200, "via the environment proxy".to_owned())
        );
    }

    /// Disabled proxy settings leave routing to the standard proxy variables
    /// (as the SDK's own client does), unless
    /// [`HttpClientBuilder::environment_proxy`] turns that off. Checked by what
    /// a proxy named in `HTTP_PROXY` sees, in a child process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_settings_follow_the_environment_by_default() {
        if std::env::var_os(ENV_PROXY_CHILD).is_some() {
            return;
        }
        let target = Target::start(None, 200, "via the environment proxy").await;
        for (mode, uses_proxy) in [("default", true), ("ignore", false)] {
            let proxy = HttpProxy::start(None).await;
            let url = format!("http://127.0.0.1:{}", proxy.port);
            let target_port = target.port.to_string();
            let output = tokio::task::spawn_blocking(move || {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "proxy::tests::environment_proxy_child",
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(ENV_PROXY_CHILD, mode)
                    .env(ENV_PROXY_TARGET_PORT, target_port)
                    .env("HTTP_PROXY", &url)
                    .env("http_proxy", &url)
                    .env_remove("NO_PROXY")
                    .env_remove("no_proxy")
                    .env_remove("ALL_PROXY")
                    .env_remove("all_proxy")
                    .output()
            })
            .await
            .unwrap()
            .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{mode}: {stdout}{stderr}"
            );
            let seen = proxy.requests();
            if uses_proxy {
                assert_eq!(seen.len(), 1, "{mode}: {seen:?}");
                assert_eq!(
                    seen[0].line,
                    format!(
                        "GET http://{TARGET_HOST}:{}/environment HTTP/1.1",
                        target.port
                    )
                );
            } else {
                assert!(seen.is_empty(), "{mode}: {seen:?}");
            }
        }
    }

    #[test]
    fn proxy_auth_failures_are_auth_errors() {
        use aws_sdk_s3::operation::list_buckets::ListBucketsError;
        use aws_smithy_runtime_api::client::result::SdkError;
        let rejected: SdkError<ListBucketsError, HttpResponse> =
            SdkError::dispatch_failure(proxy_auth_rejected(None));
        let error = Error::from(rejected);
        assert_eq!(error.kind(), ErrorKind::Auth);
        assert!(error.to_string().contains("proxy"), "{error}");
        let unrelated: SdkError<ListBucketsError, HttpResponse> = SdkError::dispatch_failure(
            ConnectorError::io(Box::new(std::io::Error::other("connection refused"))),
        );
        assert_eq!(Error::from(unrelated).kind(), ErrorKind::Remote);
    }
}
