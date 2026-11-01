//! Tests of the diagnostics: the certificate reader, the route choice, every
//! network step against local servers (a TLS server with a private CA, an HTTP
//! and a SOCKS5 proxy, closed ports), the classification of the signed
//! requests' failures, and the report. The probes of a real S3 server and of
//! slow or silent ones are in `tests/tests/tools.rs`.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};

use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::error::ErrorMetadata;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::TimeZone;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType, IsCa, KeyPair,
    date_time_ymd,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::error::ErrorKind;

const SHORT: Duration = Duration::from_secs(3);

// ---- the certificate reader ----

fn self_signed(params: CertificateParams) -> Vec<u8> {
    let key = KeyPair::generate().unwrap();
    params.self_signed(&key).unwrap().der().to_vec()
}

fn named(entries: &[(DnType, &str)], names: &[&str]) -> CertificateParams {
    let mut params = CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut name = DistinguishedName::new();
    for (kind, value) in entries {
        name.push(kind.clone(), *value);
    }
    params.distinguished_name = name;
    params
}

#[test]
fn certificate_fields_are_read_from_a_real_certificate() {
    let mut params = named(
        &[
            (DnType::CommonName, "Test Server"),
            (DnType::OrganizationName, "Example Org"),
            (DnType::OrganizationalUnitName, "Storage"),
            (DnType::CountryName, "US"),
        ],
        &["test.example"],
    );
    params.not_before = date_time_ymd(2024, 3, 5);
    params.not_after = date_time_ymd(2031, 12, 31);
    let certificate = Certificate::parse(&self_signed(params)).unwrap();
    for part in ["CN=Test Server", "O=Example Org", "OU=Storage", "C=US"] {
        assert!(
            certificate.subject.contains(part),
            "{}",
            certificate.subject
        );
    }
    // Self-signed: the issuer is the subject.
    assert_eq!(certificate.issuer, certificate.subject);
    assert_eq!(
        certificate.not_before,
        Utc.with_ymd_and_hms(2024, 3, 5, 0, 0, 0).unwrap()
    );
    assert_eq!(
        certificate.not_after,
        Utc.with_ymd_and_hms(2031, 12, 31, 0, 0, 0).unwrap()
    );
}

#[test]
fn dates_from_2050_on_are_generalized_times() {
    let mut params = named(&[(DnType::CommonName, "Far Future")], &["far.example"]);
    params.not_before = date_time_ymd(2049, 12, 31);
    params.not_after = date_time_ymd(2061, 2, 3);
    let certificate = Certificate::parse(&self_signed(params)).unwrap();
    assert_eq!(
        certificate.not_before,
        Utc.with_ymd_and_hms(2049, 12, 31, 0, 0, 0).unwrap()
    );
    assert_eq!(
        certificate.not_after,
        Utc.with_ymd_and_hms(2061, 2, 3, 0, 0, 0).unwrap()
    );
}

#[test]
fn names_keep_their_non_ascii_text() {
    let params = named(
        &[(DnType::CommonName, "Z\u{fc}rich B\u{fc}ro \u{65e5}\u{672c}")],
        &["zurich.example"],
    );
    let certificate = Certificate::parse(&self_signed(params)).unwrap();
    assert_eq!(
        certificate.subject,
        "CN=Z\u{fc}rich B\u{fc}ro \u{65e5}\u{672c}"
    );
}

#[test]
fn broken_certificates_are_rejected_without_panicking() {
    let der = self_signed(named(&[(DnType::CommonName, "x")], &["x.example"]));
    assert!(Certificate::parse(&der).is_some());
    // Every truncation, and every prefix with a wrong length, is refused.
    for length in 0..der.len() {
        assert!(Certificate::parse(&der[..length]).is_none(), "{length}");
    }
    let mut longer = der.clone();
    longer.extend_from_slice(&[0; 3]);
    assert!(
        Certificate::parse(&longer).is_some(),
        "trailing bytes are ignored"
    );
    for garbage in [
        &[][..],
        &[0x30][..],
        &[0x30, 0x80][..],
        &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff][..],
        &[0x30, 0x03, 0x02, 0x01][..],
        b"-----BEGIN CERTIFICATE-----",
        &[0xff; 64][..],
    ] {
        assert!(Certificate::parse(garbage).is_none(), "{garbage:?}");
    }
    // A flipped byte anywhere either still parses or is refused; it never
    // panics.
    for at in 0..der.len() {
        let mut broken = der.clone();
        broken[at] ^= 0xff;
        let _ = Certificate::parse(&broken);
    }
}

#[test]
fn der_times_follow_the_two_digit_year_rule() {
    let at = |year, month, day, h, m, s| Utc.with_ymd_and_hms(year, month, day, h, m, s).unwrap();
    assert_eq!(
        parse_time((UTC_TIME, b"491231235959Z")),
        Some(at(2049, 12, 31, 23, 59, 59))
    );
    assert_eq!(
        parse_time((UTC_TIME, b"500101000000Z")),
        Some(at(1950, 1, 1, 0, 0, 0))
    );
    assert_eq!(
        parse_time((GENERALIZED_TIME, b"20600102030405Z")),
        Some(at(2060, 1, 2, 3, 4, 5))
    );
    for (tag, text) in [
        (UTC_TIME, &b"491331235959Z"[..]),
        (UTC_TIME, b"491231235959"),
        (UTC_TIME, b"4912312359Z"),
        (UTC_TIME, b"491231256059Z"),
        (GENERALIZED_TIME, b"2060010203040Z"),
        (GENERALIZED_TIME, b"20601302030405Z"),
        (INTEGER, b"491231235959Z"),
    ] {
        assert_eq!(
            parse_time((tag, text)),
            None,
            "{}",
            String::from_utf8_lossy(text)
        );
    }
}

#[test]
fn object_identifiers_and_string_types() {
    assert_eq!(dotted(&[0x55, 0x04, 0x2d]), "2.5.4.45");
    assert_eq!(
        dotted(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]),
        "1.2.840.113549.1.1.11"
    );
    assert_eq!(attribute_name(&[0x55, 0x04, 0x03]), "CN");
    assert_eq!(attribute_name(&[0x55, 0x04, 0x2d]), "2.5.4.45");
    assert_eq!(
        attribute_name(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01]),
        "emailAddress"
    );
    assert_eq!(attribute_value(0x0c, "caf\u{e9}".as_bytes()), "caf\u{e9}");
    assert_eq!(attribute_value(0x13, b"Printable"), "Printable");
    assert_eq!(attribute_value(0x1e, &[0x00, 0x41, 0x00, 0xe9]), "A\u{e9}");
    assert_eq!(attribute_value(0x04, &[0xde, 0xad]), "dead");
}

// ---- the route ----

fn account_at(endpoint: Option<&str>) -> Account {
    Account {
        id: "test".to_owned(),
        label: "Test".to_owned(),
        access_key_id: "AKIDEXAMPLE".to_owned(),
        secret_ref: "test/secret".to_owned(),
        region: "us-east-1".to_owned(),
        endpoint: endpoint.map(str::to_owned),
        path_style: true,
        ..Account::default()
    }
}

async fn endpoint(url: &str) -> Endpoint {
    Endpoint::of(&account_at(Some(url))).await.unwrap()
}

fn proxy_settings(kind: ProxyKind, host: &str, port: u16) -> ProxySettings {
    ProxySettings {
        enabled: true,
        kind,
        host: host.to_owned(),
        port,
        ..ProxySettings::default()
    }
}

fn network_with(proxy: ProxySettings) -> Network {
    Network {
        proxy,
        ..Network::default()
    }
}

#[tokio::test]
async fn endpoints_are_split_into_host_port_and_scheme() {
    let aws = Endpoint::of(&Account {
        region: "eu-west-1".to_owned(),
        ..account_at(None)
    })
    .await
    .unwrap();
    assert_eq!(aws.url, "https://s3.eu-west-1.amazonaws.com");
    assert_eq!(
        (aws.host.as_str(), aws.port, aws.https),
        ("s3.eu-west-1.amazonaws.com", 443, true)
    );

    let custom = endpoint("http://127.0.0.1:9000/").await;
    assert_eq!(custom.url, "http://127.0.0.1:9000");
    assert_eq!(
        (custom.host.as_str(), custom.port, custom.https),
        ("127.0.0.1", 9000, false)
    );
    assert_eq!(custom.authority(), "127.0.0.1:9000");

    let v6 = endpoint("http://[::1]:9000").await;
    assert_eq!(v6.host, "::1");
    assert_eq!(v6.authority(), "[::1]:9000");

    let plain = endpoint("https://storage.example.com").await;
    assert_eq!((plain.port, plain.https), (443, true));
    let default_http = endpoint("http://storage.example.com").await;
    assert_eq!(default_http.port, 80);

    let err = Endpoint::of(&account_at(Some("not a url")))
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn the_route_follows_the_settings_the_bypass_list_and_the_environment() {
    let s3 = endpoint("https://s3.example.com").await;

    // Nothing configured: direct.
    assert!(matches!(
        Route::choose(&Network::default(), &s3).unwrap(),
        Route::Direct(Direct::NoProxy)
    ));

    // An HTTP proxy with credentials: Basic authentication, nothing else.
    let mut settings = proxy_settings(ProxyKind::Http, "Proxy.Example.com", 3128);
    settings.username = Some("proxy-user".to_owned());
    let network = Network {
        proxy_password: Some(SecretString::from("p@ss:word")),
        ..network_with(settings)
    };
    let Route::Via(hop) = Route::choose(&network, &s3).unwrap() else {
        panic!("a proxy was expected")
    };
    assert_eq!(
        (hop.kind, hop.host.as_str(), hop.port),
        (ProxyKind::Http, "proxy.example.com", 3128)
    );
    assert_eq!(hop.origin, "the proxy settings");
    let Login::Basic(header) = &hop.login else {
        panic!("basic authentication was expected")
    };
    assert_eq!(
        header.as_str(),
        format!("Basic {}", STANDARD.encode("proxy-user:p@ss:word"))
    );

    // SOCKS5 with credentials.
    let mut settings = proxy_settings(ProxyKind::Socks5, "10.0.0.5", 1080);
    settings.username = Some("u".to_owned());
    let network = Network {
        proxy_password: Some(SecretString::from("secret")),
        ..network_with(settings)
    };
    let Route::Via(hop) = Route::choose(&network, &s3).unwrap() else {
        panic!("a proxy was expected")
    };
    assert_eq!(hop.kind, ProxyKind::Socks5);
    assert!(matches!(hop.login, Login::Socks(_)));
    // Without a user name there is no login.
    let Route::Via(hop) = Route::choose(
        &network_with(proxy_settings(ProxyKind::Http, "10.0.0.5", 8080)),
        &s3,
    )
    .unwrap() else {
        panic!("a proxy was expected")
    };
    assert!(matches!(hop.login, Login::None));

    // The bypass list sends listed hosts straight to the endpoint.
    let mut settings = proxy_settings(ProxyKind::Http, "proxy.example.com", 3128);
    settings.bypass = vec!["*.example.com".to_owned()];
    assert!(matches!(
        Route::choose(&network_with(settings.clone()), &s3).unwrap(),
        Route::Direct(Direct::Bypassed)
    ));
    let other = endpoint("https://storage.other.org").await;
    assert!(matches!(
        Route::choose(&network_with(settings), &other).unwrap(),
        Route::Via(_)
    ));

    // Invalid settings are a usage error.
    let err = Route::choose(
        &network_with(proxy_settings(ProxyKind::Http, "user@host", 80)),
        &s3,
    )
    .err()
    .unwrap();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn the_environment_proxy_applies_only_while_the_settings_are_off() {
    let s3 = endpoint("https://s3.example.com").await;
    let plain = endpoint("http://s3.example.com").await;
    let environment = |build: fn(hyper_util::client::proxy::matcher::Builder) -> Matcher| Network {
        environment_proxy: Some(build(Matcher::builder())),
        ..Network::default()
    };

    let network = environment(|b| b.https("http://user:pw@envproxy.test:8080").build());
    let Route::Via(hop) = Route::choose(&network, &s3).unwrap() else {
        panic!("the environment proxy was expected")
    };
    assert_eq!(
        (hop.kind, hop.host.as_str(), hop.port),
        (ProxyKind::Http, "envproxy.test", 8080)
    );
    assert_eq!(hop.origin, "the environment variables");
    assert!(matches!(hop.login, Login::Basic(_)));
    // The https variable does not carry plain-http endpoints.
    assert!(matches!(
        Route::choose(&network, &plain).unwrap(),
        Route::Direct(Direct::NoProxy)
    ));

    // ALL_PROXY with a SOCKS5 URL and the default port.
    let network = environment(|b| b.all("socks5h://envsocks.test").build());
    let Route::Via(hop) = Route::choose(&network, &s3).unwrap() else {
        panic!("the environment proxy was expected")
    };
    assert_eq!((hop.kind, hop.port), (ProxyKind::Socks5, 1080));

    // NO_PROXY keeps its hosts direct.
    let network = environment(|b| {
        b.all("http://envproxy.test:8080")
            .no("s3.example.com")
            .build()
    });
    assert!(matches!(
        Route::choose(&network, &s3).unwrap(),
        Route::Direct(Direct::NoProxy)
    ));

    // Enabled settings win over the environment.
    let network = Network {
        environment_proxy: Some(Matcher::builder().all("http://envproxy.test:8080").build()),
        ..network_with(proxy_settings(ProxyKind::Http, "settings.test", 3128))
    };
    let Route::Via(hop) = Route::choose(&network, &s3).unwrap() else {
        panic!("a proxy was expected")
    };
    assert_eq!(hop.host, "settings.test");
}

#[test]
fn networks_and_timeouts_come_from_the_settings() {
    let mut settings = Settings::default();
    settings.advanced.connect_timeout_secs = 4;
    settings.advanced.read_timeout_secs = 3600;
    assert_eq!(
        Timeouts::from_settings(&settings),
        Timeouts {
            connect: Duration::from_secs(4),
            request: MAX_REQUEST_TIMEOUT
        }
    );
    settings.advanced.read_timeout_secs = 5;
    assert_eq!(
        Timeouts::from_settings(&settings).request,
        Duration::from_secs(5)
    );

    settings.proxy = proxy_settings(ProxyKind::Http, "proxy.example.com", 3128);
    let network = Network::from_settings(&settings, Some(SecretString::from("hunter2")), false);
    assert!(network.proxy.enabled && network.environment_proxy.is_none());
    assert!(
        Network::from_settings(&settings, None, true)
            .environment_proxy
            .is_some()
    );
    let text = format!("{network:?}");
    assert!(!text.contains("hunter2"), "{text}");
}

// ---- servers for the network steps ----

async fn bind() -> (TcpListener, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// A port nothing listens on.
async fn closed_port() -> u16 {
    bind().await.1
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
}

fn serve<F, Fut>(listener: TcpListener, handler: F)
where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handler(stream));
        }
    });
}

/// A validity period as (year, month, day) from and to.
type Years = ((i32, u8, u8), (i32, u8, u8));

/// A private CA and servers whose certificates it signs.
struct Pki {
    ca: CertifiedIssuer<'static, KeyPair>,
}

impl Pki {
    fn new() -> Self {
        let mut params = named(&[(DnType::CommonName, "Test Root CA")], &[]);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        Pki {
            ca: CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap(),
        }
    }

    fn ca_pem(&self) -> Vec<u8> {
        self.ca.pem().into_bytes()
    }

    /// A server configuration with a certificate for `names`, signed by this
    /// CA, valid for the given years (`None`: the default, always valid).
    fn server(&self, names: &[&str], years: Option<Years>) -> Arc<rustls::ServerConfig> {
        let mut params = named(&[(DnType::CommonName, "Test Server")], names);
        if let Some((from, to)) = years {
            params.not_before = date_time_ymd(from.0, from.1, from.2);
            params.not_after = date_time_ymd(to.0, to.1, to.2);
        }
        let key = KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &self.ca).unwrap();
        server_config(certificate.der().clone(), &key)
    }

    /// A server configuration whose certificate no CA signed.
    fn self_signed_server(names: &[&str]) -> Arc<rustls::ServerConfig> {
        let params = named(&[(DnType::CommonName, "Self Signed")], names);
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        server_config(certificate.der().clone(), &key)
    }
}

fn server_config(
    certificate: rustls::pki_types::CertificateDer<'static>,
    key: &KeyPair,
) -> Arc<rustls::ServerConfig> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap(),
    )
}

/// Serves TLS with `config`; every connection waits for the client to hang
/// up. Returns the port and the number of connections accepted so far.
async fn tls_server(config: Arc<rustls::ServerConfig>) -> (u16, Arc<AtomicUsize>) {
    let (listener, port) = bind().await;
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    serve(listener, move |stream| {
        counted.fetch_add(1, Ordering::SeqCst);
        let acceptor = TlsAcceptor::from(Arc::clone(&config));
        async move {
            if let Ok(mut tls) = acceptor.accept(stream).await {
                let mut buffer = [0u8; 64];
                let _ = tls.read(&mut buffer).await;
            }
        }
    });
    (port, connections)
}

/// What a test proxy does with a request.
#[derive(Clone)]
enum Proxy {
    /// Relays to `target`; with `require` it first insists on that
    /// `Proxy-Authorization` header value (HTTP) or user and password
    /// (SOCKS5).
    Tunnel {
        target: SocketAddr,
        require: Option<(String, String)>,
    },
    /// HTTP: sends these bytes, then closes. SOCKS5: a failed request with
    /// this reply code.
    Reply(&'static [u8], u8),
    /// Closes without a word.
    Silent,
}

struct TestProxy {
    port: u16,
    /// The request heads (HTTP) or `host:port` destinations (SOCKS5) seen.
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TestProxy {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

async fn http_proxy(behavior: Proxy) -> TestProxy {
    let (listener, port) = bind().await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    serve(listener, move |mut stream| {
        let (behavior, log) = (behavior.clone(), Arc::clone(&log));
        async move {
            let mut head = Vec::new();
            let mut chunk = [0u8; 256];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => head.extend_from_slice(&chunk[..read]),
                }
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            log.lock().unwrap().push(head.clone());
            match behavior {
                Proxy::Silent => {}
                Proxy::Reply(bytes, _) => {
                    let _ = stream.write_all(bytes).await;
                }
                Proxy::Tunnel { target, require } => {
                    if let Some((header, _)) = &require
                        && !head.contains(&format!("Proxy-Authorization: {header}\r\n"))
                    {
                        let _ = stream
                            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\n\r\n")
                            .await;
                        return;
                    }
                    let Ok(mut upstream) = TcpStream::connect(target).await else {
                        let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                        return;
                    };
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await;
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                }
            }
        }
    });
    TestProxy { port, seen }
}

async fn socks5_proxy(behavior: Proxy) -> TestProxy {
    let (listener, port) = bind().await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    serve(listener, move |stream| {
        let (behavior, log) = (behavior.clone(), Arc::clone(&log));
        async move {
            let _ = socks5_session(stream, behavior, log).await;
        }
    });
    TestProxy { port, seen }
}

async fn socks5_session(
    mut stream: TcpStream,
    behavior: Proxy,
    log: Arc<std::sync::Mutex<Vec<String>>>,
) -> io::Result<()> {
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    let mut methods = vec![0u8; usize::from(greeting[1])];
    stream.read_exact(&mut methods).await?;
    let (target, require, code) = match behavior {
        Proxy::Silent => return Ok(()),
        Proxy::Reply(_, code) => (None, None, code),
        Proxy::Tunnel { target, require } => (Some(target), require, 0),
    };
    match require {
        Some((user, password)) => {
            if !methods.contains(&0x02) {
                return stream.write_all(&[5, 0xff]).await;
            }
            stream.write_all(&[5, 2]).await?;
            let mut head = [0u8; 2];
            stream.read_exact(&mut head).await?;
            let mut name = vec![0u8; usize::from(head[1])];
            stream.read_exact(&mut name).await?;
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await?;
            let mut secret = vec![0u8; usize::from(length[0])];
            stream.read_exact(&mut secret).await?;
            if name != user.as_bytes() || secret != password.as_bytes() {
                return stream.write_all(&[1, 1]).await;
            }
            stream.write_all(&[1, 0]).await?;
        }
        None => stream.write_all(&[5, 0]).await?,
    }
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    let host = match request[3] {
        3 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await?;
            let mut name = vec![0u8; usize::from(length[0])];
            stream.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        1 => {
            let mut address = [0u8; 4];
            stream.read_exact(&mut address).await?;
            Ipv4Addr::from(address).to_string()
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    log.lock()
        .unwrap()
        .push(format!("{host}:{}", u16::from_be_bytes(port)));
    let reply = |code: u8| [5, code, 0, 1, 0, 0, 0, 0, 0, 0];
    match target {
        Some(target) => {
            let mut upstream = TcpStream::connect(target).await?;
            stream.write_all(&reply(0)).await?;
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        }
        None => stream.write_all(&reply(code)).await?,
    }
    Ok(())
}

/// A connection to `port` on the loopback address.
async fn connected(port: u16) -> TcpStream {
    TcpStream::connect(loopback(port)).await.unwrap()
}

fn via(kind: ProxyKind, login: Login, port: u16) -> Route {
    Route::Via(Box::new(ProxyHop {
        kind,
        host: "127.0.0.1".to_owned(),
        port,
        login,
        origin: "the proxy settings",
    }))
}

fn basic(user: &str, password: &str) -> (Login, String) {
    let header = format!("Basic {}", STANDARD.encode(format!("{user}:{password}")));
    (Login::Basic(Zeroizing::new(header.clone())), header)
}

// ---- dns ----

#[tokio::test]
async fn an_ip_address_needs_no_lookup() {
    let (step, addresses) = probe_dns("192.0.2.7", 9000, "the endpoint", SHORT).await;
    assert_eq!((step.id, step.status), (StepId::Dns, StepStatus::Skip));
    assert_eq!(step.code, "ip_address");
    assert_eq!(step.elapsed_ms, 0);
    assert_eq!(addresses, Some(vec!["192.0.2.7:9000".parse().unwrap()]));
    let (_, addresses) = probe_dns("::1", 9000, "the proxy", SHORT).await;
    assert_eq!(addresses, Some(vec!["[::1]:9000".parse().unwrap()]));
}

#[tokio::test]
async fn localhost_resolves() {
    let (step, addresses) = probe_dns("localhost", 9000, "the endpoint", SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Pass, "resolved")
    );
    assert!(
        step.summary.starts_with("localhost resolves to "),
        "{}",
        step.summary
    );
    let addresses = addresses.unwrap();
    assert!(
        !addresses.is_empty()
            && addresses
                .iter()
                .all(|a| a.ip().is_loopback() && a.port() == 9000)
    );
    assert!(step.detail.unwrap().contains("127.0.0.1") || addresses.iter().any(|a| a.is_ipv6()));
}

#[tokio::test]
async fn a_name_that_cannot_exist_does_not_resolve() {
    // RFC 2606: .invalid never resolves.
    let (step, addresses) = probe_dns(
        "bucketbridge-diagnostics.invalid",
        443,
        "the endpoint",
        SHORT,
    )
    .await;
    assert_eq!((step.id, step.status), (StepId::Dns, StepStatus::Fail));
    assert!(
        ["not_found", "failed", "timeout"].contains(&step.code.as_str()),
        "{}",
        step.code
    );
    assert!(addresses.is_none());
    assert!(
        step.summary.contains("bucketbridge-diagnostics.invalid"),
        "{}",
        step.summary
    );
    assert!(step.detail.unwrap().contains("DNS"));
}

#[test]
fn lookup_failures_are_told_apart_by_their_text() {
    for text in [
        "failed to lookup address information: nodename nor servname provided, or not known",
        "failed to lookup address information: Name or service not known",
        "No such host is known. (os error 11001)",
        "failed to lookup address information: No address associated with hostname",
    ] {
        assert_eq!(
            lookup_failure(&io::Error::other(text)).0,
            "not_found",
            "{text}"
        );
    }
    for text in [
        "failed to lookup address information: Temporary failure in name resolution",
        "something else",
    ] {
        assert_eq!(
            lookup_failure(&io::Error::other(text)).0,
            "failed",
            "{text}"
        );
    }
}

// ---- tcp ----

#[tokio::test]
async fn tcp_connects_and_names_the_address() {
    let (listener, port) = bind().await;
    serve(listener, |_stream| async {});
    let addresses = [loopback(port)];
    let (step, stream) =
        probe_tcp(Some(&addresses), "127.0.0.1", port, "the endpoint", SHORT).await;
    assert_eq!(
        (step.id, step.status, step.code.as_str()),
        (StepId::Tcp, StepStatus::Pass, "connected")
    );
    assert!(
        step.summary.contains(&format!("127.0.0.1:{port}")),
        "{}",
        step.summary
    );
    assert!(step.detail.is_none());
    assert!(stream.is_some());
}

#[tokio::test]
async fn a_refused_connection_is_reported_with_every_attempt() {
    let port = closed_port().await;
    let addresses = [loopback(port)];
    let (step, stream) = probe_tcp(Some(&addresses), "127.0.0.1", port, "the proxy", SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "refused")
    );
    assert!(
        step.summary.contains("the proxy") && step.summary.contains("refused"),
        "{}",
        step.summary
    );
    assert!(step.detail.unwrap().contains(&format!("127.0.0.1:{port}")));
    assert!(stream.is_none());
}

#[tokio::test]
async fn the_next_address_is_tried_when_one_fails() {
    let dead = closed_port().await;
    let (listener, alive) = bind().await;
    serve(listener, |_stream| async {});
    let addresses = [loopback(dead), loopback(alive)];
    let (step, stream) = probe_tcp(
        Some(&addresses),
        "example.test",
        alive,
        "the endpoint",
        SHORT,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Pass, "connected")
    );
    assert!(
        step.summary.contains(&format!("127.0.0.1:{alive}")),
        "{}",
        step.summary
    );
    let detail = step.detail.unwrap();
    assert!(
        detail.contains(&format!("127.0.0.1:{dead}: refused")),
        "{detail}"
    );
    assert!(stream.is_some());
}

#[tokio::test]
async fn tcp_is_skipped_when_the_name_did_not_resolve() {
    let (step, stream) = probe_tcp(None, "x.invalid", 443, "the endpoint", SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str(), step.elapsed_ms),
        (StepStatus::Skip, "blocked", 0)
    );
    assert!(stream.is_none());
}

#[test]
fn connect_failures_are_classified_by_kind() {
    for (kind, code) in [
        (io::ErrorKind::ConnectionRefused, "refused"),
        (io::ErrorKind::TimedOut, "timeout"),
        (io::ErrorKind::HostUnreachable, "unreachable"),
        (io::ErrorKind::NetworkUnreachable, "unreachable"),
        (io::ErrorKind::PermissionDenied, "failed"),
    ] {
        assert_eq!(connect_failure(&io::Error::from(kind)).0, code, "{kind:?}");
    }
}

// ---- proxy ----

#[tokio::test]
async fn without_a_proxy_the_step_is_skipped_and_the_connection_passes_through() {
    let (listener, port) = bind().await;
    serve(listener, |_stream| async {});
    let target = endpoint(&format!("http://127.0.0.1:{port}")).await;
    for (route, code) in [
        (Route::Direct(Direct::NoProxy), "none"),
        (Route::Direct(Direct::Bypassed), "direct"),
    ] {
        let (step, stream) = probe_proxy(Some(connected(port).await), &route, &target, SHORT).await;
        assert_eq!(
            (step.id, step.status, step.code.as_str()),
            (StepId::Proxy, StepStatus::Skip, code)
        );
        assert_eq!(step.elapsed_ms, 0);
        assert!(stream.is_some());
    }
    let (_, stream) = probe_proxy(None, &Route::Direct(Direct::NoProxy), &target, SHORT).await;
    assert!(stream.is_none());
}

#[tokio::test]
async fn an_unreachable_proxy_blocks_the_tunnel() {
    let target = endpoint("https://s3.example.com").await;
    let route = via(ProxyKind::Http, Login::None, 1);
    let (step, stream) = probe_proxy(None, &route, &target, SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Skip, "blocked")
    );
    assert!(stream.is_none());
}

#[tokio::test]
async fn an_http_proxy_opens_a_tunnel_and_receives_the_credentials() {
    let (listener, upstream) = bind().await;
    serve(listener, |_stream| async {});
    let (login, header) = basic("proxy-user", "p@ss:w0rd");
    let proxy = http_proxy(Proxy::Tunnel {
        target: loopback(upstream),
        require: Some((header, String::new())),
    })
    .await;
    let target = endpoint("https://target.test:8443").await;
    let route = via(ProxyKind::Http, login, proxy.port);
    let (step, stream) =
        probe_proxy(Some(connected(proxy.port).await), &route, &target, SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Pass, "tunnel_open")
    );
    assert!(
        step.summary.contains("target.test:8443") && step.summary.contains("HTTP CONNECT"),
        "{}",
        step.summary
    );
    assert!(
        step.summary.contains("the proxy settings"),
        "{}",
        step.summary
    );
    assert!(stream.is_some());
    let seen = proxy.seen();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].starts_with("CONNECT target.test:8443 HTTP/1.1\r\n"),
        "{}",
        seen[0]
    );
    assert!(
        seen[0].contains("Host: target.test:8443\r\n"),
        "{}",
        seen[0]
    );
    // The credentials went to the proxy and nowhere else.
    for text in [&step.summary, step.detail.as_deref().unwrap_or_default()] {
        assert!(
            !text.contains("p@ss:w0rd") && !text.contains("Basic "),
            "{text}"
        );
    }
}

#[tokio::test]
async fn an_http_proxy_that_says_no_is_understood() {
    let target = endpoint("https://s3.example.com").await;
    let run = |behavior: Proxy, login: Login| {
        let target = &target;
        async move {
            let proxy = http_proxy(behavior).await;
            let route = via(ProxyKind::Http, login, proxy.port);
            probe_proxy(Some(connected(proxy.port).await), &route, target, SHORT).await
        }
    };

    // 407: no credentials configured, or wrong ones.
    let (step, stream) = run(
        Proxy::Tunnel {
            target: loopback(1),
            require: Some(("Basic eHg6eHg=".to_owned(), String::new())),
        },
        Login::None,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "auth_required")
    );
    assert!(step.summary.contains("407"), "{}", step.summary);
    assert!(step.detail.unwrap().contains("settings"));
    assert!(stream.is_none());

    // 403 with a reason.
    let (step, _) = run(
        Proxy::Reply(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n", 0),
        Login::None,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "rejected")
    );
    assert!(
        step.summary.contains("s3.example.com:443") && step.summary.contains("403 Forbidden"),
        "{}",
        step.summary
    );

    // Not HTTP at all, and a proxy that hangs up.
    for behavior in [
        Proxy::Reply(b"SSH-2.0-OpenSSH_9.0\r\n\r\n", 0),
        Proxy::Silent,
    ] {
        let (step, _) = run(behavior, Login::None).await;
        assert_eq!(
            (step.status, step.code.as_str()),
            (StepStatus::Fail, "protocol")
        );
        assert!(step.summary.contains("HTTP proxy"), "{}", step.summary);
    }
}

#[tokio::test]
async fn a_proxy_that_never_answers_times_out() {
    let (listener, port) = bind().await;
    serve(listener, |mut stream| async move {
        let mut sink = [0u8; 256];
        while matches!(stream.read(&mut sink).await, Ok(read) if read > 0) {}
    });
    let target = endpoint("https://s3.example.com").await;
    let route = via(ProxyKind::Http, Login::None, port);
    let (step, _) = probe_proxy(
        Some(connected(port).await),
        &route,
        &target,
        Duration::from_millis(300),
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "timeout")
    );
    assert!(step.elapsed_ms >= 250, "{}", step.elapsed_ms);
}

#[tokio::test]
async fn plain_http_endpoints_are_forwarded_by_an_http_proxy_not_tunnelled() {
    let proxy = http_proxy(Proxy::Silent).await;
    let target = endpoint("http://storage.example.com:9000").await;
    let route = via(ProxyKind::Http, Login::None, proxy.port);
    let (step, stream) =
        probe_proxy(Some(connected(proxy.port).await), &route, &target, SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Pass, "forwarded")
    );
    assert!(stream.is_none());
    assert!(proxy.seen().is_empty(), "no CONNECT is sent for plain HTTP");
}

#[tokio::test]
async fn a_socks5_proxy_resolves_the_name_itself() {
    let (listener, upstream) = bind().await;
    serve(listener, |_stream| async {});
    let proxy = socks5_proxy(Proxy::Tunnel {
        target: loopback(upstream),
        require: Some(("socks-user".to_owned(), "socks-secret".to_owned())),
    })
    .await;
    let target = endpoint("https://target.test:8443").await;
    let auth = ProxyAuth::new(ProxyKind::Socks5, "socks-user", "socks-secret").unwrap();
    let route = via(ProxyKind::Socks5, Login::Socks(auth), proxy.port);
    let (step, stream) =
        probe_proxy(Some(connected(proxy.port).await), &route, &target, SHORT).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Pass, "tunnel_open")
    );
    assert!(step.summary.contains("SOCKS5"), "{}", step.summary);
    assert!(stream.is_some());
    // The proxy got the name, unresolved.
    assert_eq!(proxy.seen(), ["target.test:8443"]);
}

#[tokio::test]
async fn a_socks5_proxy_that_says_no_is_understood() {
    let target = endpoint("https://s3.example.com").await;
    let attempt = |behavior: Proxy, login: Login| {
        let target = &target;
        async move {
            let proxy = socks5_proxy(behavior).await;
            let route = via(ProxyKind::Socks5, login, proxy.port);
            probe_proxy(Some(connected(proxy.port).await), &route, target, SHORT)
                .await
                .0
        }
    };

    // Credentials wanted but none configured, and wrong ones.
    let wanted = Proxy::Tunnel {
        target: loopback(1),
        require: Some(("socks-user".to_owned(), "socks-secret".to_owned())),
    };
    let step = attempt(wanted.clone(), Login::None).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "auth_required")
    );
    let wrong = ProxyAuth::new(ProxyKind::Socks5, "socks-user", "wrong").unwrap();
    let step = attempt(wanted, Login::Socks(wrong)).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "auth_required")
    );

    // The proxy cannot connect to the endpoint (reply 0x05, refused).
    let step = attempt(Proxy::Reply(b"", 0x05), Login::None).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "rejected")
    );
    assert!(
        step.summary.contains("connection refused"),
        "{}",
        step.summary
    );

    // Not a SOCKS5 server.
    let step = attempt(Proxy::Silent, Login::None).await;
    assert_eq!(step.status, StepStatus::Fail);
}

// ---- tls ----

fn link<'a>(route: &'a Route, addresses: &'a [SocketAddr], endpoint: &'a Endpoint) -> Link<'a> {
    Link {
        route,
        addresses,
        endpoint,
        timeouts: Timeouts {
            connect: SHORT,
            request: SHORT,
        },
    }
}

fn trusting(pki: &Pki) -> Network {
    Network {
        extra_roots_pem: vec![pki.ca_pem()],
        ..Network::default()
    }
}

/// Runs the TLS step against a server on `port` for the host `localhost`.
async fn tls_step(port: u16, network: &Network) -> Step {
    let target = endpoint(&format!("https://localhost:{port}")).await;
    let addresses = [loopback(port)];
    let route = Route::Direct(Direct::NoProxy);
    let stream = connected(port).await;
    probe_tls(
        Some(stream),
        &target,
        network,
        &link(&route, &addresses, &target),
        true,
    )
    .await
}

#[tokio::test]
async fn the_handshake_shows_the_certificate() {
    let pki = Pki::new();
    let (port, _) =
        tls_server(pki.server(&["localhost"], Some(((2024, 1, 1), (2035, 6, 15))))).await;
    let step = tls_step(port, &trusting(&pki)).await;
    assert_eq!(
        (step.id, step.status, step.code.as_str()),
        (StepId::Tls, StepStatus::Pass, "handshake")
    );
    assert!(
        step.summary
            .starts_with("TLS 1.3 handshake with localhost succeeded."),
        "{}",
        step.summary
    );
    let detail = step.detail.unwrap();
    assert!(
        detail.contains("Certificate subject: CN=Test Server"),
        "{detail}"
    );
    assert!(detail.contains("Issuer: CN=Test Root CA"), "{detail}");
    assert!(
        detail.contains("Valid from 2024-01-01 to 2035-06-15 UTC (expires in "),
        "{detail}"
    );
    assert!(detail.contains("Cipher suite: TLS13_"), "{detail}");
    assert!(
        !step.summary.contains("expires in"),
        "a distant expiry is not called out"
    );
}

#[tokio::test]
async fn a_certificate_that_expires_soon_is_called_out() {
    let pki = Pki::new();
    let soon = Utc::now() + chrono::Duration::days(5);
    let to = (
        chrono::Datelike::year(&soon),
        u8::try_from(chrono::Datelike::month(&soon)).unwrap(),
        u8::try_from(chrono::Datelike::day(&soon)).unwrap(),
    );
    let (port, _) = tls_server(pki.server(&["localhost"], Some(((2024, 1, 1), to)))).await;
    let step = tls_step(port, &trusting(&pki)).await;
    assert_eq!(step.status, StepStatus::Pass);
    assert!(
        step.summary.contains("The certificate expires in 4 days")
            || step.summary.contains("The certificate expires in 5 days"),
        "{}",
        step.summary
    );
}

#[tokio::test]
async fn an_untrusted_issuer_is_reported_with_what_the_server_presented() {
    let pki = Pki::new();
    let (port, connections) = tls_server(pki.server(&["localhost"], None)).await;
    // No extra roots: the private CA is not trusted.
    let step = tls_step(port, &Network::default()).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "untrusted_issuer")
    );
    assert!(step.summary.contains("does not trust"), "{}", step.summary);
    let detail = step.detail.unwrap();
    assert!(detail.contains("The server presented:"), "{detail}");
    assert!(
        detail.contains("Certificate subject: CN=Test Server"),
        "{detail}"
    );
    assert!(detail.contains("Issuer: CN=Test Root CA"), "{detail}");
    assert!(detail.contains("Install the CA certificate"), "{detail}");
    // The certificate was read on a second connection.
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_self_signed_certificate_is_untrusted_too() {
    let (port, _) = tls_server(Pki::self_signed_server(&["localhost"])).await;
    let step = tls_step(port, &Network::default()).await;
    assert_eq!(step.code, "untrusted_issuer");
    let detail = step.detail.unwrap();
    assert!(
        detail.contains("Certificate subject: CN=Self Signed"),
        "{detail}"
    );
    assert!(detail.contains("Issuer: CN=Self Signed"), "{detail}");
}

#[tokio::test]
async fn an_expired_certificate_names_its_date() {
    let pki = Pki::new();
    let (port, _) =
        tls_server(pki.server(&["localhost"], Some(((2020, 1, 1), (2021, 1, 1))))).await;
    let step = tls_step(port, &trusting(&pki)).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "expired")
    );
    assert!(
        step.summary.contains("expired on 2021-01-01"),
        "{}",
        step.summary
    );
    let detail = step.detail.unwrap();
    assert!(
        detail.contains("Valid from 2020-01-01 to 2021-01-01 UTC (expired "),
        "{detail}"
    );
    assert!(detail.contains("date, time and time zone"), "{detail}");
}

#[tokio::test]
async fn a_certificate_that_is_not_valid_yet_is_reported() {
    let pki = Pki::new();
    let (port, _) =
        tls_server(pki.server(&["localhost"], Some(((2999, 1, 1), (3000, 1, 1))))).await;
    let step = tls_step(port, &trusting(&pki)).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "not_yet_valid")
    );
    assert!(step.detail.unwrap().contains("Valid from 2999-01-01"));
}

#[tokio::test]
async fn a_certificate_for_another_name_lists_the_names_it_is_valid_for() {
    let pki = Pki::new();
    let (port, _) = tls_server(pki.server(&["other.example", "www.other.example"], None)).await;
    let step = tls_step(port, &trusting(&pki)).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "name_mismatch")
    );
    assert!(
        step.summary.contains("not valid for the name localhost"),
        "{}",
        step.summary
    );
    let detail = step.detail.unwrap();
    assert!(
        detail.contains("other.example") && detail.contains("www.other.example"),
        "{detail}"
    );
}

#[tokio::test]
async fn a_server_that_does_not_speak_tls_is_recognised() {
    let (listener, port) = bind().await;
    serve(listener, |mut stream| async move {
        let mut buffer = [0u8; 512];
        let _ = stream.read(&mut buffer).await;
        let _ = stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await;
    });
    let step = tls_step(port, &Network::default()).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "protocol")
    );
    assert!(
        step.summary.contains("handshake with localhost failed"),
        "{}",
        step.summary
    );
    assert!(step.detail.unwrap().contains("scheme and port"));

    // A server that hangs up during the handshake.
    let (listener, port) = bind().await;
    serve(listener, |_stream| async {});
    let step = tls_step(port, &Network::default()).await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "failed")
    );
    assert!(
        step.summary.contains("closed the connection"),
        "{}",
        step.summary
    );
}

#[tokio::test]
async fn a_silent_server_times_out_the_handshake() {
    let (listener, port) = bind().await;
    serve(listener, |mut stream| async move {
        let mut sink = [0u8; 512];
        while matches!(stream.read(&mut sink).await, Ok(read) if read > 0) {}
    });
    let target = endpoint(&format!("https://localhost:{port}")).await;
    let addresses = [loopback(port)];
    let route = Route::Direct(Direct::NoProxy);
    let mut shortened = link(&route, &addresses, &target);
    shortened.timeouts.connect = Duration::from_millis(300);
    let step = probe_tls(
        Some(connected(port).await),
        &target,
        &Network::default(),
        &shortened,
        true,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Fail, "timeout")
    );
    assert!(step.elapsed_ms >= 250, "{}", step.elapsed_ms);
}

#[tokio::test]
async fn tls_is_skipped_for_plain_http_and_after_a_network_failure() {
    let plain = endpoint("http://127.0.0.1:9000").await;
    let addresses = [loopback(9000)];
    let route = Route::Direct(Direct::NoProxy);
    let step = probe_tls(
        None,
        &plain,
        &Network::default(),
        &link(&route, &addresses, &plain),
        true,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str(), step.elapsed_ms),
        (StepStatus::Skip, "plain_http", 0)
    );

    let secure = endpoint("https://s3.example.com").await;
    let step = probe_tls(
        None,
        &secure,
        &Network::default(),
        &link(&route, &addresses, &secure),
        false,
    )
    .await;
    assert_eq!(
        (step.status, step.code.as_str()),
        (StepStatus::Skip, "blocked")
    );
}

// ---- the whole probe over the network steps ----

fn s3_for(account: &Account) -> S3 {
    S3::new(
        account,
        &Credentials::new(
            "AKIDEXAMPLE",
            "SUPERSECRETKEY123",
            Some("SESSIONTOKEN456".into()),
        ),
        &ClientOptions {
            max_attempts: 1,
            ..ClientOptions::default()
        },
    )
    .unwrap()
}

fn ids(report: &Report) -> Vec<StepId> {
    report.steps.iter().map(|step| step.id).collect()
}

const ORDER: [StepId; 6] = [
    StepId::Dns,
    StepId::Tcp,
    StepId::Proxy,
    StepId::Tls,
    StepId::Credentials,
    StepId::Region,
];

fn quick() -> Timeouts {
    Timeouts {
        connect: SHORT,
        request: SHORT,
    }
}

#[tokio::test]
async fn a_refused_endpoint_fails_one_step_and_skips_the_rest() {
    let port = closed_port().await;
    let account = account_at(Some(&format!("http://127.0.0.1:{port}")));
    let s3 = s3_for(&account);
    let report = run(
        &s3,
        &account,
        Some("my-bucket"),
        &Network::default(),
        quick(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&report), ORDER);
    let steps: Vec<(StepStatus, &str)> = report
        .steps
        .iter()
        .map(|step| (step.status, step.code.as_str()))
        .collect();
    assert_eq!(
        steps,
        [
            (StepStatus::Skip, "ip_address"),
            (StepStatus::Fail, "refused"),
            (StepStatus::Skip, "none"),
            (StepStatus::Skip, "plain_http"),
            (StepStatus::Skip, "blocked"),
            (StepStatus::Skip, "blocked"),
        ]
    );
    assert!(!report.ok);
    assert_eq!(report.target, format!("http://127.0.0.1:{port}"));
    assert_eq!(
        (report.region.as_str(), report.bucket.as_deref()),
        ("us-east-1", Some("my-bucket"))
    );
    for step in &report.steps {
        if step.status == StepStatus::Skip {
            assert_eq!(step.elapsed_ms, 0, "{:?}", step.id);
        }
    }
    assert!(report.step(StepId::Tcp).unwrap().elapsed_ms <= report.elapsed_ms);
}

#[tokio::test]
async fn the_region_step_without_a_bucket_says_so() {
    let port = closed_port().await;
    let account = account_at(Some(&format!("http://127.0.0.1:{port}")));
    let report = run(
        &s3_for(&account),
        &account,
        None,
        &Network::default(),
        quick(),
    )
    .await
    .unwrap();
    let region = report.step(StepId::Region).unwrap();
    assert_eq!(
        (region.status, region.code.as_str()),
        (StepStatus::Skip, "no_bucket")
    );
    assert_eq!(report.bucket, None);
}

#[tokio::test]
async fn an_invalid_bucket_name_is_a_usage_error() {
    let account = account_at(Some("http://127.0.0.1:1"));
    let err = run(
        &s3_for(&account),
        &account,
        Some("bad bucket"),
        &Network::default(),
        quick(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn the_report_holds_no_secret_and_hides_the_user_information_of_the_endpoint() {
    let port = closed_port().await;
    let mut account = account_at(Some(&format!("http://127.0.0.1:{port}")));
    let s3 = s3_for(&account);
    // The report shows what the account's endpoint says, without credentials.
    account.endpoint = Some(format!("http://admin:EndpointPassword9@127.0.0.1:{port}"));
    let mut settings = proxy_settings(ProxyKind::Http, "127.0.0.1", port);
    settings.username = Some("proxy-user".to_owned());
    settings.bypass = vec!["127.0.0.1".to_owned()];
    let network = Network {
        proxy_password: Some(SecretString::from("ProxyPassword7")),
        ..network_with(settings)
    };
    let report = run(&s3, &account, Some("b"), &network, quick())
        .await
        .unwrap();
    assert_eq!(report.target, format!("http://<redacted>@127.0.0.1:{port}"));
    let json = serde_json::to_string(&report).unwrap();
    let debug = format!("{report:?}");
    for secret in [
        "SUPERSECRETKEY123",
        "SESSIONTOKEN456",
        "EndpointPassword9",
        "ProxyPassword7",
        "proxy-user",
    ] {
        assert!(
            !json.contains(secret) && !debug.contains(secret),
            "{secret} leaked: {json}"
        );
    }
}

#[tokio::test]
async fn the_probe_goes_through_an_http_proxy_to_a_tls_endpoint() {
    let pki = Pki::new();
    let (tls_port, _) = tls_server(pki.server(&["localhost"], None)).await;
    let proxy = http_proxy(Proxy::Tunnel {
        target: loopback(tls_port),
        require: None,
    })
    .await;
    let account = account_at(Some(&format!("https://localhost:{tls_port}")));
    let network = Network {
        extra_roots_pem: vec![pki.ca_pem()],
        ..network_with(proxy_settings(ProxyKind::Http, "127.0.0.1", proxy.port))
    };
    let report = run(&s3_for(&account), &account, None, &network, quick())
        .await
        .unwrap();
    assert_eq!(ids(&report), ORDER);
    let step = |id| report.step(id).unwrap();
    // dns and tcp are about the proxy host, here an address.
    assert_eq!(step(StepId::Dns).code, "ip_address");
    assert!(
        step(StepId::Dns).summary.contains("the proxy"),
        "{}",
        step(StepId::Dns).summary
    );
    assert_eq!(step(StepId::Tcp).code, "connected");
    assert!(
        step(StepId::Tcp)
            .summary
            .contains(&format!("127.0.0.1:{}", proxy.port))
    );
    assert_eq!(step(StepId::Proxy).code, "tunnel_open");
    // The handshake ran inside the tunnel.
    assert_eq!(
        (step(StepId::Tls).status, step(StepId::Tls).code.as_str()),
        (StepStatus::Pass, "handshake")
    );
    assert!(proxy.seen()[0].starts_with(&format!("CONNECT localhost:{tls_port} HTTP/1.1")));
    // The signed request is not proxied by this test's S3 handle and cannot
    // trust the private CA, so the credentials step fails; that is its
    // business, not the network's.
    assert_eq!(step(StepId::Credentials).status, StepStatus::Fail);
    assert!(!report.ok);
}

#[tokio::test]
async fn the_probe_goes_through_a_socks5_proxy_and_reports_a_tls_inspecting_proxy() {
    let pki = Pki::new();
    // The "origin" presents a certificate of the proxy's own CA.
    let (tls_port, _) = tls_server(pki.server(&["target.test"], None)).await;
    let proxy = socks5_proxy(Proxy::Tunnel {
        target: loopback(tls_port),
        require: None,
    })
    .await;
    let account = account_at(Some(&format!("https://target.test:{tls_port}")));
    // This computer does not trust that CA.
    let network = network_with(proxy_settings(ProxyKind::Socks5, "127.0.0.1", proxy.port));
    let report = run(&s3_for(&account), &account, None, &network, quick())
        .await
        .unwrap();
    let step = |id| report.step(id).unwrap();
    assert_eq!(step(StepId::Proxy).code, "tunnel_open");
    assert_eq!(step(StepId::Tls).code, "untrusted_issuer");
    assert!(
        step(StepId::Tls)
            .detail
            .as_deref()
            .unwrap()
            .contains("Issuer: CN=Test Root CA")
    );
    // The proxy was asked for the name, twice: the handshake and the reading
    // of the certificate afterwards.
    assert_eq!(
        proxy.seen(),
        [
            format!("target.test:{tls_port}"),
            format!("target.test:{tls_port}")
        ]
    );
    // A failed network step skips the signed requests.
    assert_eq!(step(StepId::Credentials).code, "blocked");
    assert_eq!(step(StepId::Region).code, "no_bucket");
}

#[tokio::test]
async fn a_bypassed_endpoint_is_reached_directly_even_with_a_proxy() {
    let pki = Pki::new();
    let (tls_port, _) = tls_server(pki.server(&["localhost"], None)).await;
    let dead_proxy = closed_port().await;
    let account = account_at(Some(&format!("https://localhost:{tls_port}")));
    let mut settings = proxy_settings(ProxyKind::Http, "127.0.0.1", dead_proxy);
    settings.bypass = vec!["localhost".to_owned()];
    let network = Network {
        extra_roots_pem: vec![pki.ca_pem()],
        ..network_with(settings)
    };
    let report = run(&s3_for(&account), &account, None, &network, quick())
        .await
        .unwrap();
    let step = |id| report.step(id).unwrap();
    assert_eq!(step(StepId::Dns).code, "resolved");
    assert!(
        step(StepId::Dns).summary.contains("the endpoint")
            || step(StepId::Dns).summary.starts_with("localhost")
    );
    assert_eq!(step(StepId::Tcp).code, "connected");
    assert_eq!(step(StepId::Proxy).code, "direct");
    assert_eq!(step(StepId::Tls).code, "handshake");
}

#[tokio::test]
async fn an_unreachable_proxy_blocks_everything_after_it() {
    let dead_proxy = closed_port().await;
    let account = account_at(Some("https://s3.example.com"));
    let network = network_with(proxy_settings(ProxyKind::Http, "127.0.0.1", dead_proxy));
    let report = run(&s3_for(&account), &account, Some("b"), &network, quick())
        .await
        .unwrap();
    let step = |id| report.step(id).unwrap();
    assert_eq!(step(StepId::Tcp).code, "refused");
    assert!(
        step(StepId::Tcp).summary.contains("the proxy"),
        "{}",
        step(StepId::Tcp).summary
    );
    for id in [
        StepId::Proxy,
        StepId::Tls,
        StepId::Credentials,
        StepId::Region,
    ] {
        assert_eq!(
            (step(id).status, step(id).code.as_str()),
            (StepStatus::Skip, "blocked"),
            "{id:?}"
        );
    }
}

#[test]
fn the_report_has_the_documented_json_shape() {
    let report = Report {
        target: "https://s3.example.com".to_owned(),
        region: "eu-west-1".to_owned(),
        bucket: None,
        started_at: Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap(),
        elapsed_ms: 12,
        ok: true,
        steps: vec![Step {
            id: StepId::Credentials,
            status: StepStatus::Pass,
            elapsed_ms: 7,
            summary: "The server accepted the credentials.".to_owned(),
            detail: None,
            code: "valid".to_owned(),
        }],
    };
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "target": "https://s3.example.com",
            "region": "eu-west-1",
            "bucket": null,
            "started_at": "2026-10-29T08:15:00Z",
            "elapsed_ms": 12,
            "ok": true,
            "steps": [{
                "id": "credentials",
                "status": "pass",
                "elapsed_ms": 7,
                "summary": "The server accepted the credentials.",
                "detail": null,
                "code": "valid"
            }]
        })
    );
    assert_eq!(serde_json::from_value::<Report>(value).unwrap(), report);
    for (id, name) in [
        (StepId::Dns, "dns"),
        (StepId::Tcp, "tcp"),
        (StepId::Proxy, "proxy"),
        (StepId::Tls, "tls"),
        (StepId::Credentials, "credentials"),
        (StepId::Region, "region"),
    ] {
        assert_eq!(serde_json::to_value(id).unwrap(), serde_json::json!(name));
    }
    for (status, name) in [
        (StepStatus::Pass, "pass"),
        (StepStatus::Fail, "fail"),
        (StepStatus::Skip, "skip"),
    ] {
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!(name)
        );
    }
}

// ---- the signed requests' outcomes ----

fn response(status: u16, headers: &[(&'static str, String)]) -> HttpResponse {
    let mut response = HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::empty());
    for (name, value) in headers {
        response.headers_mut().insert(*name, value.clone());
    }
    response
}

fn list_failure(
    code: &str,
    status: u16,
    headers: &[(&'static str, String)],
) -> SdkError<ListBucketsError, HttpResponse> {
    let meta = ErrorMetadata::builder()
        .code(code)
        .message(format!("{code} message"))
        .custom("aws_request_id", "REQ1")
        .build();
    SdkError::service_error(ListBucketsError::generic(meta), response(status, headers))
}

fn http_date(time: DateTime<Utc>) -> String {
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[test]
fn credential_failures_are_told_apart() {
    let limit = SHORT;
    let check = |error: SdkError<ListBucketsError, HttpResponse>| credentials_failure(error, limit);
    let cases = [
        (
            "InvalidAccessKeyId",
            403,
            StepStatus::Fail,
            "invalid_access_key",
        ),
        (
            "SignatureDoesNotMatch",
            403,
            StepStatus::Fail,
            "bad_signature",
        ),
        ("ExpiredToken", 400, StepStatus::Fail, "expired_token"),
        ("InvalidToken", 400, StepStatus::Fail, "invalid_token"),
        (
            "AuthorizationHeaderMalformed",
            400,
            StepStatus::Fail,
            "wrong_region",
        ),
        ("PermanentRedirect", 301, StepStatus::Fail, "redirect"),
        ("SlowDown", 503, StepStatus::Fail, "failed"),
        // Access denied means the key was accepted: it passes.
        ("AccessDenied", 403, StepStatus::Pass, "access_denied"),
    ];
    for (code, status, expected, step_code) in cases {
        let (outcome, answered) = check(list_failure(code, status, &[]));
        assert_eq!(
            (outcome.status, outcome.code),
            (expected, step_code),
            "{code}"
        );
        assert!(answered, "{code}: the server answered");
        assert!(!outcome.summary.is_empty());
    }
    let (outcome, _) = check(list_failure("AccessDenied", 403, &[]));
    assert!(
        outcome.summary.contains("not allowed to list buckets"),
        "{}",
        outcome.summary
    );
    // An unknown code on a redirect status is still a redirect.
    assert_eq!(check(list_failure("Whatever", 307, &[])).0.code, "redirect");
    // The server's own words survive for unknown codes.
    let (outcome, _) = check(list_failure("SlowDown", 503, &[]));
    assert!(
        outcome.summary.contains("SlowDown message"),
        "{}",
        outcome.summary
    );
}

#[test]
fn clock_skew_names_the_difference_when_the_server_sent_its_date() {
    let behind = Utc::now() - chrono::Duration::minutes(25);
    let (outcome, answered) = credentials_failure(
        list_failure("RequestTimeTooSkewed", 403, &[("date", http_date(behind))]),
        SHORT,
    );
    assert!(answered);
    assert_eq!(
        (outcome.status, outcome.code),
        (StepStatus::Fail, "clock_skew")
    );
    assert!(
        outcome.summary.contains("25 minutes ahead of the server's"),
        "{}",
        outcome.summary
    );
    assert!(
        outcome.summary.contains("15 minutes"),
        "{}",
        outcome.summary
    );
    assert!(outcome.detail.unwrap().contains("date, time and time zone"));

    let ahead = Utc::now() + chrono::Duration::hours(2);
    let (outcome, _) = credentials_failure(
        list_failure("RequestTimeTooSkewed", 403, &[("date", http_date(ahead))]),
        SHORT,
    );
    assert!(
        outcome.summary.contains("120 minutes behind the server's")
            || outcome.summary.contains("119 minutes behind the server's"),
        "{}",
        outcome.summary
    );

    // Without a date header the message is still useful.
    let (outcome, _) = credentials_failure(list_failure("RequestTimeTooSkewed", 403, &[]), SHORT);
    assert_eq!(outcome.code, "clock_skew");
    assert!(
        outcome.summary.contains("differs too much"),
        "{}",
        outcome.summary
    );
}

#[test]
fn the_clock_offset_is_read_from_the_date_header() {
    let offset =
        clock_offset(Some(&http_date(Utc::now() - chrono::Duration::seconds(90)))).unwrap();
    assert!((89..=92).contains(&offset), "{offset}");
    assert_eq!(clock_offset(Some("not a date")), None);
    assert_eq!(clock_offset(None), None);
    assert_eq!(
        clock_sentence(0),
        "This computer's clock matches the server's."
    );
    assert_eq!(
        clock_sentence(45),
        "This computer's clock is 45 seconds ahead of the server's."
    );
    assert_eq!(
        clock_sentence(-45),
        "This computer's clock is 45 seconds behind the server's."
    );
    assert_eq!(
        clock_sentence(1200),
        "This computer's clock is 20 minutes ahead of the server's."
    );
    assert_eq!(
        clock_sentence(-7200),
        "This computer's clock is 120 minutes behind the server's."
    );
}

#[test]
fn a_request_without_an_answer_is_a_timeout_or_a_failure() {
    let (outcome, answered) = credentials_failure(
        SdkError::timeout_error(io::Error::other("deadline passed")),
        Duration::from_secs(7),
    );
    assert_eq!(
        (outcome.status, outcome.code),
        (StepStatus::Fail, "timeout")
    );
    assert!(outcome.summary.contains("7 seconds"), "{}", outcome.summary);
    assert!(!answered);

    let refused = ConnectorError::io(Box::new(io::Error::from(io::ErrorKind::ConnectionRefused)));
    let (outcome, answered) = credentials_failure(SdkError::dispatch_failure(refused), SHORT);
    assert_eq!((outcome.status, outcome.code), (StepStatus::Fail, "failed"));
    assert!(!answered, "a dispatch failure has no answer");

    // A proxy that rejects its credentials surfaces as an auth failure
    // without a service code.
    let rejected = ConnectorError::io(Box::new(io::Error::other(
        "tunnel error: proxy authorization required",
    )));
    let (outcome, answered) = credentials_failure(SdkError::dispatch_failure(rejected), SHORT);
    assert_eq!(
        (outcome.status, outcome.code),
        (StepStatus::Fail, "proxy_auth")
    );
    assert!(!answered);
}

fn head_failure(
    status: u16,
    headers: &[(&'static str, String)],
) -> SdkError<HeadBucketError, HttpResponse> {
    let meta = ErrorMetadata::builder().code("Forbidden").build();
    SdkError::service_error(HeadBucketError::generic(meta), response(status, headers))
}

#[test]
fn region_outcomes_follow_the_answer_and_the_region_header() {
    let region = |name: &str| vec![("x-amz-bucket-region", name.to_owned())];

    // Found: the same region, another region on AWS, another on a custom endpoint, none reported.
    let found = found_region("data", Some("eu-west-1".into()), "eu-west-1", false);
    assert_eq!((found.status, found.code), (StepStatus::Pass, "found"));
    assert!(
        found.summary.contains("the account's region"),
        "{}",
        found.summary
    );
    let elsewhere = found_region("data", Some("ap-south-1".into()), "us-east-1", false);
    assert!(
        elsewhere
            .summary
            .contains("is in ap-south-1; the account's region is us-east-1"),
        "{}",
        elsewhere.summary
    );
    assert!(
        elsewhere
            .detail
            .unwrap()
            .contains("sent to ap-south-1 automatically")
    );
    let custom = found_region("data", Some("ap-south-1".into()), "us-east-1", true);
    assert!(
        custom
            .detail
            .unwrap()
            .contains("every request with the account region us-east-1")
    );
    let silent = found_region("data", None, "us-east-1", true);
    assert_eq!(
        (silent.status, silent.code),
        (StepStatus::Pass, "no_region_header")
    );

    // Failures.
    let denied = region_error("data", head_failure(403, &region("eu-west-1")), SHORT);
    assert_eq!(
        (denied.status, denied.code),
        (StepStatus::Fail, "access_denied")
    );
    assert!(
        denied
            .summary
            .contains("Access to bucket 'data' is denied. It is in eu-west-1."),
        "{}",
        denied.summary
    );
    let denied = region_error("data", head_failure(403, &[]), SHORT);
    assert_eq!(denied.summary, "Access to bucket 'data' is denied.");
    let missing = region_error("data", head_failure(404, &[]), SHORT);
    assert_eq!(
        (missing.status, missing.code),
        (StepStatus::Fail, "bucket_not_found")
    );
    // A redirect names the right region: that is the answer.
    let moved = region_error("data", head_failure(301, &region("eu-central-1")), SHORT);
    assert_eq!((moved.status, moved.code), (StepStatus::Pass, "found"));
    assert!(moved.summary.contains("eu-central-1"), "{}", moved.summary);
    let odd = region_error("data", head_failure(301, &[]), SHORT);
    assert_eq!((odd.status, odd.code), (StepStatus::Fail, "failed"));
    let slow = region_error(
        "data",
        SdkError::timeout_error(io::Error::other("slow")),
        Duration::from_secs(9),
    );
    assert_eq!((slow.status, slow.code), (StepStatus::Fail, "timeout"));
    assert!(slow.summary.contains("9 seconds"), "{}", slow.summary);
}
