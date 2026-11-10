//! Optional URL shorteners (T13): Bitly and TinyURL.
//!
//! Nothing here runs unless the user chose a provider in the settings
//! ([`ShortenerProvider`]) and presses Shorten on a share link: the
//! application never contacts a shortener on its own.
//! Pressing Shorten sends the whole link, including the signature of a
//! presigned URL, to the provider, which is why the UI says so. The link of
//! a shortener is not revoked when the presigned URL expires.
//!
//! The provider's API token lives in the secret store like the proxy
//! password ([`crate::AccountManager::set_shortener_token`]) and is never
//! logged: it is only placed in the `Authorization` header and no error
//! message echoes it or the provider's response body.
//!
//! Requests go through the proxy-aware HTTP client of the session, so the
//! proxy settings apply.
//!
//! | Provider | Request |
//! |---|---|
//! | Bitly | `POST https://api-ssl.bitly.com/v4/shorten`, `Authorization: Bearer <token>`, `{"long_url": ...}`; the answer's `link` |
//! | TinyURL with a token | `POST https://api.tinyurl.com/create`, `Authorization: Bearer <token>`, `{"url": ...}`; the answer's `data.tiny_url` |
//! | TinyURL without a token | `GET https://tinyurl.com/api-create.php?url=...`; the answer is the short link as text |

use std::time::Duration;

use aws_smithy_async::rt::sleep::TokioSleep;
use aws_smithy_async::time::SystemTimeSource;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorSettings, SharedHttpClient,
};
use aws_smithy_runtime_api::client::orchestrator::HttpRequest;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponentsBuilder;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::byte_stream::ByteStream;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::secrets::SecretString;
use crate::settings::ShortenerProvider;

/// Bitly's API endpoint.
pub const BITLY_ENDPOINT: &str = "https://api-ssl.bitly.com/v4/shorten";
/// TinyURL's API endpoint (with a token).
pub const TINYURL_ENDPOINT: &str = "https://api.tinyurl.com/create";
/// TinyURL's token-less endpoint.
pub const TINYURL_PLAIN_ENDPOINT: &str = "https://tinyurl.com/api-create.php";

/// Longest link sent to a shortener, in bytes.
pub const MAX_LONG_URL_BYTES: usize = 8192;
/// Longest answer read from a shortener, in bytes.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

/// The display name of a provider; empty for [`ShortenerProvider::None`].
pub fn provider_name(provider: ShortenerProvider) -> &'static str {
    match provider {
        ShortenerProvider::None => "",
        ShortenerProvider::Bitly => "Bitly",
        ShortenerProvider::Tinyurl => "TinyURL",
    }
}

/// The endpoints of a provider; tests point them at a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Bitly's `v4/shorten`.
    pub bitly: String,
    /// TinyURL's `create`.
    pub tinyurl: String,
    /// TinyURL's token-less `api-create.php`.
    pub tinyurl_plain: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            bitly: BITLY_ENDPOINT.to_owned(),
            tinyurl: TINYURL_ENDPOINT.to_owned(),
            tinyurl_plain: TINYURL_PLAIN_ENDPOINT.to_owned(),
        }
    }
}

/// Shortens `long_url` with `provider` at the real endpoints.
pub async fn shorten(
    client: &SharedHttpClient,
    provider: ShortenerProvider,
    token: Option<&SecretString>,
    long_url: &str,
) -> Result<String> {
    shorten_at(client, &Endpoints::default(), provider, token, long_url).await
}

/// Shortens `long_url` with `provider` at `endpoints`.
///
/// Fails with `usage` for no provider, a missing Bitly token or a link that
/// is not an `http(s)` URL; `auth` when the provider rejects the token;
/// `remote` for anything else (rate limit, outage, an unusable answer).
pub async fn shorten_at(
    client: &SharedHttpClient,
    endpoints: &Endpoints,
    provider: ShortenerProvider,
    token: Option<&SecretString>,
    long_url: &str,
) -> Result<String> {
    check_long_url(long_url)?;
    let name = provider_name(provider);
    let token = token.filter(|t| !t.is_empty());
    let request = match (provider, token) {
        (ShortenerProvider::None, _) => {
            return Err(Error::Usage("no URL shortener is configured".into()));
        }
        (ShortenerProvider::Bitly, None) => {
            return Err(Error::Usage(
                "Bitly needs an access token: add it in the settings".into(),
            ));
        }
        (ShortenerProvider::Bitly, Some(token)) => {
            post_json(&endpoints.bitly, token, &json!({ "long_url": long_url }))?
        }
        (ShortenerProvider::Tinyurl, Some(token)) => {
            post_json(&endpoints.tinyurl, token, &json!({ "url": long_url }))?
        }
        (ShortenerProvider::Tinyurl, None) => {
            let encoded = utf8_percent_encode(long_url, NON_ALPHANUMERIC);
            let uri = format!("{}?url={encoded}", endpoints.tinyurl_plain);
            HttpRequest::get(uri).map_err(|_| bad_endpoint(name))?
        }
    };
    let (status, body) = send(client, request, name, MAX_RESPONSE_BYTES).await?;
    match status {
        200 | 201 => {}
        401 | 403 => {
            return Err(Error::Auth(format!(
                "{name} rejected the access token (HTTP {status})"
            )));
        }
        429 => {
            return Err(Error::Remote(format!(
                "{name} is limiting requests (HTTP 429): try again later"
            )));
        }
        _ => {
            return Err(Error::Remote(format!(
                "{name} could not shorten the link (HTTP {status})"
            )));
        }
    }
    let link = match (provider, token) {
        (ShortenerProvider::Tinyurl, None) => body.trim().to_owned(),
        _ => {
            let value: Value = serde_json::from_str(&body)
                .map_err(|_| Error::Remote(format!("{name} sent an answer that is not JSON")))?;
            let field = match provider {
                ShortenerProvider::Bitly => value.get("link"),
                _ => value.get("data").and_then(|data| data.get("tiny_url")),
            };
            field.and_then(Value::as_str).unwrap_or_default().to_owned()
        }
    };
    if !is_http_url(&link) {
        return Err(Error::Remote(format!("{name} did not send a short link")));
    }
    Ok(link)
}

fn check_long_url(url: &str) -> Result<()> {
    if !is_http_url(url) {
        return Err(Error::Usage(
            "only http and https links can be shortened".into(),
        ));
    }
    if url.len() > MAX_LONG_URL_BYTES {
        return Err(Error::Usage(format!(
            "the link is longer than {MAX_LONG_URL_BYTES} bytes"
        )));
    }
    Ok(())
}

fn is_http_url(text: &str) -> bool {
    let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !text.chars().any(|c| c.is_control() || c.is_whitespace())
        && text.len() > "http://x".len()
}

fn bad_endpoint(name: &str) -> Error {
    Error::Internal(format!("the {name} endpoint is not a valid URL"))
}

fn post_json(endpoint: &str, token: &SecretString, body: &Value) -> Result<HttpRequest> {
    let name = "the shortener";
    let bytes = serde_json::to_vec(body)
        .map_err(|error| Error::Internal(format!("encoding the request: {error}")))?;
    let mut request = HttpRequest::new(SdkBody::from(bytes));
    request.set_method("POST").map_err(|_| bad_endpoint(name))?;
    request.set_uri(endpoint).map_err(|_| bad_endpoint(name))?;
    let headers = request.headers_mut();
    headers.insert("content-type", "application/json");
    headers.insert("accept", "application/json");
    headers.insert("authorization", format!("Bearer {}", token.expose()));
    Ok(request)
}

/// Sends one request through the client and reads the answer (at most
/// `max_bytes` of it), within a timeout. `name` is the service, for the
/// messages. Also used by the release check ([`crate::release`]).
pub(crate) async fn send(
    client: &SharedHttpClient,
    request: HttpRequest,
    name: &str,
    max_bytes: usize,
) -> Result<(u16, String)> {
    let settings = HttpConnectorSettings::builder()
        .connect_timeout(TIMEOUT)
        .read_timeout(TIMEOUT)
        .build();
    let components = RuntimeComponentsBuilder::for_tests()
        .with_sleep_impl(Some(TokioSleep::new()))
        .with_time_source(Some(SystemTimeSource::new()))
        .build()
        .map_err(|error| Error::Internal(format!("preparing the request: {error}")))?;
    let connector = client.http_connector(&settings, &components);
    let failed = |what: &str| Error::Remote(format!("could not reach {name}: {what}"));
    let response = tokio::time::timeout(TIMEOUT * 2, connector.call(request))
        .await
        .map_err(|_| failed("timed out"))?
        .map_err(|error| {
            // The error chain can name the proxy or host, never the token (it
            // is only in a header), but keep the text short anyway.
            if crate::proxy::is_proxy_auth_failure(&error) {
                Error::Auth("the proxy rejected the credentials".into())
            } else {
                failed(&error.to_string())
            }
        })?;
    let status = response.status().as_u16();
    let bytes = tokio::time::timeout(TIMEOUT * 2, ByteStream::new(response.into_body()).collect())
        .await
        .map_err(|_| failed("timed out"))?
        .map_err(|_| failed("the answer was cut off"))?
        .into_bytes();
    if bytes.len() > max_bytes {
        return Err(Error::Remote(format!(
            "{name} sent an answer that is too long"
        )));
    }
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_links_pass() {
        assert!(is_http_url("https://example.com/a?b=c"));
        assert!(is_http_url("HTTP://example.com/a"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url("https://a b"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url(""));
        assert!(check_long_url(&format!("https://x/{}", "a".repeat(9000))).is_err());
        assert!(check_long_url("https://x/ok").is_ok());
    }

    #[test]
    fn provider_names() {
        assert_eq!(provider_name(ShortenerProvider::Bitly), "Bitly");
        assert_eq!(provider_name(ShortenerProvider::Tinyurl), "TinyURL");
        assert_eq!(provider_name(ShortenerProvider::None), "");
    }
}
