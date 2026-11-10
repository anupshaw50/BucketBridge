//! Building an S3 client for an account.
//!
//! The client is configured explicitly from an [`Account`], [`Credentials`]
//! and [`ClientOptions`]; nothing is read from the environment, AWS config
//! files or instance metadata, so a BucketBridge account never picks up
//! unrelated credentials by accident.
//!
//! # Retry and timeout policy
//!
//! * SDK standard retry mode with `max_attempts` attempts per request
//!   (default 5): exponential backoff with jitter, for throttling, transient
//!   HTTP statuses and connection errors.
//! * A connect timeout (default 10 s) on every request.
//! * No read timeout on the client. The SDK measures its read timeout from
//!   the start of a request until the response headers arrive, which
//!   includes sending the request body, and S3 answers an upload only after
//!   receiving all of it: any upload slower than the timeout would fail.
//!   [`ClientOptions::read_timeout`] (default 60 s) is applied per request
//!   instead, to requests that neither send nor receive a large body (see
//!   [`crate::S3::metadata_timeout`]).
//! * Stalled-stream protection on request and response bodies, with a grace
//!   period of [`STALL_GRACE_PERIOD`] (configurable through
//!   [`ClientOptions::stalled_stream`]). It covers uploads, downloads and the
//!   response bodies of long server-side calls.
//! * No operation or attempt timeout, so a long transfer is never cut off
//!   because of its size.
//!
//! # HTTP client
//!
//! Without [`ClientOptions::http_client`] the SDK's default HTTPS client is
//! used (hyper with rustls). That client honours the `HTTP_PROXY`,
//! `HTTPS_PROXY` and `NO_PROXY` environment variables; inject a client to
//! apply the proxy settings of the app instead.
//!
//! # Content-MD5
//!
//! For the operations where S3 requires an integrity header (DeleteObjects,
//! PutObjectTagging, PutBucketLifecycleConfiguration, ...), the SDK sends a
//! CRC32 checksum header only. Some S3-compatible servers still insist on
//! `Content-MD5` there, so every client also sends `Content-MD5` with those
//! requests (AWS accepts both).

use std::fmt;
use std::time::Duration;

use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    AppName, BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedHttpClient, StalledStreamProtectionConfig,
};
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::interceptors::Intercept;
use aws_smithy_runtime_api::client::interceptors::context::BeforeTransmitInterceptorContextMut;
use aws_smithy_runtime_api::client::orchestrator::Metadata;
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::config_bag::ConfigBag;
use md5::{Digest, Md5};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};
use crate::model::Account;
use crate::settings::{ChecksumAlgorithm, Settings};

/// Name reported to the provider in the user agent.
const APP_NAME: &str = "bucketbridge";

/// How long a request or response body may transfer nothing before
/// stalled-stream protection fails the request (the default of
/// [`ClientOptions::stalled_stream`]).
///
/// The SDK measures throughput over a 1 s window and then starts this grace
/// period, so a stream fails after 20 to 21 s without a single byte. A
/// throttled upload body must therefore yield data at least every few
/// seconds; a pause of 20 s or more fails the request. A download is only
/// considered stalled while it is being read: a reader that pauses (for
/// example to apply a bandwidth limit) never trips it.
pub const STALL_GRACE_PERIOD: Duration = Duration::from_secs(20);

/// Operations for which S3 requires an integrity header; the client adds
/// `Content-MD5` to them (see the module documentation).
const CONTENT_MD5_OPERATIONS: &[&str] = &[
    "DeleteObjects",
    "PutBucketAcl",
    "PutBucketCors",
    "PutBucketEncryption",
    "PutBucketLifecycleConfiguration",
    "PutBucketLogging",
    "PutBucketOwnershipControls",
    "PutBucketPolicy",
    "PutBucketReplication",
    "PutBucketRequestPayment",
    "PutBucketTagging",
    "PutBucketVersioning",
    "PutBucketWebsite",
    "PutObjectAcl",
    "PutObjectLegalHold",
    "PutObjectLockConfiguration",
    "PutObjectRetention",
    "PutObjectTagging",
    "PutPublicAccessBlock",
];

/// Credentials for one account, held in memory only.
///
/// `Debug` never prints the secret or the session token, and every field is
/// zeroed when the value is dropped.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct Credentials {
    /// Access key ID.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Session token for temporary credentials.
    pub session_token: Option<String>,
}

impl Credentials {
    /// Bundles the three parts.
    pub fn new(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
        session_token: Option<String>,
    ) -> Self {
        Credentials {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token,
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Network and integrity options for a client.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Attempts per request including the first (standard retry mode).
    pub max_attempts: u32,
    /// TCP and TLS connect timeout.
    pub connect_timeout: Duration,
    /// Read timeout of requests that neither send nor receive a large body
    /// (listings, HEAD, deletes, multipart bookkeeping, ...): the time from
    /// sending the request until the response headers arrive. It is not set
    /// on the client; [`crate::S3`] applies it per request (see
    /// [`crate::S3::metadata_timeout`]). Uploads, downloads and long
    /// server-side calls rely on [`ClientOptions::stalled_stream`] instead.
    pub read_timeout: Duration,
    /// Upload checksum setting; `None` sends checksums only when an API
    /// requires one, any other value lets the SDK send them whenever an API
    /// supports them. The algorithm itself is chosen per request by the
    /// transfer code.
    pub checksum_algorithm: ChecksumAlgorithm,
    /// Validate response checksums whenever the server returns one.
    pub validate_downloads: bool,
    /// Stalled-stream protection for request and response bodies; enabled
    /// with a grace period of [`STALL_GRACE_PERIOD`] by default.
    pub stalled_stream: StalledStreamProtectionConfig,
    /// HTTP client to use instead of the SDK default, for example a
    /// proxy-aware one.
    pub http_client: Option<SharedHttpClient>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions::from_settings(&Settings::default())
    }
}

impl ClientOptions {
    /// Takes retry, timeout and checksum options from the settings.
    pub fn from_settings(settings: &Settings) -> Self {
        let advanced = &settings.advanced;
        ClientOptions {
            max_attempts: advanced.max_attempts,
            connect_timeout: Duration::from_secs(advanced.connect_timeout_secs),
            read_timeout: Duration::from_secs(advanced.read_timeout_secs),
            checksum_algorithm: settings.data_integrity.checksum_algorithm,
            validate_downloads: settings.data_integrity.validate_downloads,
            stalled_stream: StalledStreamProtectionConfig::enabled()
                .grace_period(STALL_GRACE_PERIOD)
                .build(),
            http_client: None,
        }
    }

    fn request_checksum_calculation(&self) -> RequestChecksumCalculation {
        match self.checksum_algorithm {
            ChecksumAlgorithm::None => RequestChecksumCalculation::WhenRequired,
            _ => RequestChecksumCalculation::WhenSupported,
        }
    }

    fn response_checksum_validation(&self) -> ResponseChecksumValidation {
        if self.validate_downloads {
            ResponseChecksumValidation::WhenSupported
        } else {
            ResponseChecksumValidation::WhenRequired
        }
    }
}

/// Builds an S3 client for `account` with explicit static credentials.
///
/// The client has a connect timeout but no read timeout (see the module
/// documentation); apply [`ClientOptions::read_timeout`] per request with
/// [`crate::S3::metadata_timeout`].
///
/// Fails with [`Error::Usage`] for an empty access key or secret, an invalid
/// region, an invalid endpoint URL or `max_attempts == 0`.
pub fn build_client(
    account: &Account,
    credentials: &Credentials,
    options: &ClientOptions,
) -> Result<aws_sdk_s3::Client> {
    if credentials.access_key_id.trim().is_empty() {
        return Err(Error::Usage("the access key ID is empty".into()));
    }
    if credentials.secret_access_key.is_empty() {
        return Err(Error::Usage("the secret access key is empty".into()));
    }
    let sdk_credentials = aws_sdk_s3::config::Credentials::new(
        credentials.access_key_id.clone(),
        credentials.secret_access_key.clone(),
        credentials.session_token.clone(),
        None,
        "bucketbridge-account",
    );
    build_client_with_provider(
        account,
        SharedCredentialsProvider::new(sdk_credentials),
        options,
    )
}

/// Builds an S3 client for `account` whose credentials come from `provider`
/// (a refreshing source such as an assumed role, an AWS profile, IAM
/// Identity Center or the instance role; see [`crate::creds`]). The SDK
/// caches what the provider returns until shortly before it expires and
/// then asks it again. Everything else is as for [`build_client`].
pub fn build_client_with_provider(
    account: &Account,
    provider: SharedCredentialsProvider,
    options: &ClientOptions,
) -> Result<aws_sdk_s3::Client> {
    validate_region(&account.region)?;
    if let Some(endpoint) = &account.endpoint {
        validate_endpoint(endpoint)?;
    }
    if options.max_attempts == 0 {
        return Err(Error::Usage("max_attempts must be at least 1".into()));
    }
    account
        .options
        .check(account.endpoint.as_deref(), &account.region)?;
    let app_name = AppName::new(APP_NAME)
        .map_err(|err| Error::Internal(format!("invalid app name: {err}")))?;

    let mut builder = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .app_name(app_name)
        .region(Region::new(account.region.clone()))
        .credentials_provider(provider)
        .retry_config(RetryConfig::standard().with_max_attempts(options.max_attempts))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(options.connect_timeout)
                .build(),
        )
        .request_checksum_calculation(options.request_checksum_calculation())
        .response_checksum_validation(options.response_checksum_validation())
        .stalled_stream_protection(options.stalled_stream.clone())
        .interceptor(ContentMd5)
        .force_path_style(account.path_style);
    if let Some(endpoint) = &account.endpoint {
        builder = builder.endpoint_url(endpoint.trim_end_matches('/'));
    } else if account.options.dual_stack {
        // The SDK resolves `s3.dualstack.<region>.<suffix>` itself, with
        // the suffix of the region's partition.
        builder = builder.use_dual_stack(true);
    }
    if let Some(http_client) = &options.http_client {
        builder = builder.http_client(http_client.clone());
    }
    Ok(aws_sdk_s3::Client::from_conf(builder.build()))
}

/// A per-request configuration override that sets the read timeout (the
/// connect timeout and everything else stay as configured on the client).
pub(crate) fn read_timeout_override(read_timeout: Duration) -> aws_sdk_s3::config::Builder {
    aws_sdk_s3::config::Builder::default()
        .timeout_config(TimeoutConfig::builder().read_timeout(read_timeout).build())
}

/// Adds `Content-MD5` to requests of [`CONTENT_MD5_OPERATIONS`] whose body
/// is held in memory, unless the caller set one already.
#[derive(Debug)]
struct ContentMd5;

impl Intercept for ContentMd5 {
    fn name(&self) -> &'static str {
        "BucketBridgeContentMd5"
    }

    fn modify_before_signing(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _runtime_components: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> std::result::Result<(), BoxError> {
        let required = cfg
            .load::<Metadata>()
            .is_some_and(|operation| CONTENT_MD5_OPERATIONS.contains(&operation.name()));
        if !required {
            return Ok(());
        }
        let request = context.request_mut();
        if request.headers().contains_key("content-md5") {
            return Ok(());
        }
        let Some(body) = request.body().bytes() else {
            return Ok(());
        };
        let digest = aws_smithy_types::base64::encode(Md5::digest(body));
        request.headers_mut().try_insert("content-md5", digest)?;
        Ok(())
    }
}

/// Checks a region name: non-empty ASCII letters, digits, `-`, `_` and `.`
/// (S3-compatible providers use names such as `auto` or `garage`).
pub fn validate_region(region: &str) -> Result<()> {
    if region.is_empty() {
        return Err(Error::Usage("the region is empty".into()));
    }
    if let Some(bad) = region
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(Error::Usage(format!(
            "the region '{region}' contains the character {bad:?}"
        )));
    }
    Ok(())
}

/// Checks a custom endpoint: an absolute `http://` or `https://` URL with a
/// host, and neither a query, a fragment nor user information.
///
/// Error messages show the URL through [`redact_endpoint`], so credentials
/// typed into the URL never reach the UI or a log file.
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    let invalid = |why: &str| {
        Err(Error::Usage(format!(
            "invalid endpoint URL '{}': {why}",
            redact_endpoint(endpoint)
        )))
    };
    if endpoint.trim() != endpoint || endpoint.is_empty() {
        return invalid("it is empty or has surrounding whitespace");
    }
    let uri: http::Uri = match endpoint.parse() {
        Ok(uri) => uri,
        Err(err) => return invalid(&err.to_string()),
    };
    match uri.scheme_str() {
        Some(scheme)
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") => {}
        _ => return invalid("it must start with http:// or https://"),
    }
    let Some(authority) = uri.authority() else {
        return invalid("it has no host");
    };
    if authority.host().is_empty() {
        return invalid("it has no host");
    }
    if authority.as_str().contains('@') {
        return invalid("credentials must not be part of the URL");
    }
    if uri.query().is_some() || endpoint.contains('#') {
        return invalid("it must not contain a query or a fragment");
    }
    Ok(())
}

/// An endpoint URL fit for messages and logs: user information
/// (`user:password@`) is replaced by `<redacted>@`.
///
/// Works on malformed input too: everything between the scheme separator
/// (or the start, without one) and the last `@` is hidden, so a password
/// containing characters a URL does not allow is never shown either.
pub fn redact_endpoint(endpoint: &str) -> String {
    let Some(at) = endpoint.rfind('@') else {
        return endpoint.to_owned();
    };
    let start = endpoint[..at].find("://").map_or(0, |i| i + 3);
    format!("{}<redacted>{}", &endpoint[..start], &endpoint[at..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn account() -> Account {
        Account {
            id: "a".into(),
            label: "A".into(),
            access_key_id: "AKIAEXAMPLE".into(),
            secret_ref: "s".into(),
            region: "eu-west-1".into(),
            ..Account::default()
        }
    }

    /// URL of a ListBuckets request made for `region` on AWS (no endpoint).
    async fn request_url(region: &str) -> String {
        let mut acct = account();
        acct.region = region.into();
        let (http, receiver) = aws_smithy_http_client::test_util::capture_request(None);
        let options = ClientOptions {
            http_client: Some(SharedHttpClient::new(http)),
            ..ClientOptions::default()
        };
        let client = build_client(&acct, &Credentials::new("AK", "SK", None), &options).unwrap();
        let _ = client.list_buckets().send().await;
        receiver.expect_request().uri().to_owned()
    }

    #[tokio::test]
    async fn endpoints_follow_the_partition_of_the_region() {
        for region in [
            "us-east-1",
            "eu-west-1",
            "cn-north-1",
            "cn-northwest-1",
            "us-gov-west-1",
        ] {
            let url = request_url(region).await;
            let expected = crate::regions::s3_endpoint(region);
            assert!(
                url.starts_with(&expected) || url.starts_with("https://s3.amazonaws.com"),
                "{region}: {url} vs {expected}"
            );
            assert!(
                url.contains(crate::regions::partition_of(region).dns_suffix()),
                "{url}"
            );
        }
        assert!(request_url("cn-north-1").await.contains("amazonaws.com.cn"));
        assert!(
            !request_url("us-gov-west-1")
                .await
                .contains("amazonaws.com.cn")
        );
    }

    #[tokio::test]
    async fn dual_stack_uses_the_dual_stack_endpoint_of_the_partition() {
        for (region, suffix) in [
            ("eu-west-1", "amazonaws.com"),
            ("cn-north-1", "amazonaws.com.cn"),
        ] {
            let mut acct = account();
            acct.region = region.into();
            acct.options.dual_stack = true;
            let (http, receiver) = aws_smithy_http_client::test_util::capture_request(None);
            let options = ClientOptions {
                http_client: Some(SharedHttpClient::new(http)),
                ..ClientOptions::default()
            };
            let client =
                build_client(&acct, &Credentials::new("AK", "SK", None), &options).unwrap();
            let _ = client.list_buckets().send().await;
            let url = receiver.expect_request().uri().to_owned();
            assert!(url.contains(".dualstack."), "{url}");
            assert!(url.contains(&format!(".{region}.{suffix}")), "{url}");
        }
    }

    #[test]
    fn network_options_are_refused_where_they_cannot_work() {
        let creds = Credentials::new("AK", "SK", None);
        let mut acct = account();
        acct.endpoint = Some("https://minio.example.test".into());
        acct.options.dual_stack = true;
        let err = build_client(&acct, &creds, &ClientOptions::default()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        acct.options.dual_stack = false;
        acct.options.accelerate = true;
        assert!(build_client(&acct, &creds, &ClientOptions::default()).is_err());
        let mut china = account();
        china.region = "cn-north-1".into();
        china.options.accelerate = true;
        let err = build_client(&china, &creds, &ClientOptions::default()).unwrap_err();
        assert!(err.to_string().contains("China"), "{err}");
        china.options.accelerate = false;
        china.options.dual_stack = true;
        assert!(build_client(&china, &creds, &ClientOptions::default()).is_ok());
    }

    #[test]
    fn debug_redacts_secrets() {
        let creds = Credentials::new(
            "AKIAEXAMPLE",
            "very-secret-value",
            Some("session-token-value".into()),
        );
        let text = format!("{creds:?}");
        assert!(text.contains("AKIAEXAMPLE"), "{text}");
        assert!(!text.contains("very-secret-value"), "{text}");
        assert!(!text.contains("session-token-value"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
        let no_token = format!("{:?}", Credentials::new("A", "B", None));
        assert!(no_token.contains("session_token: None"), "{no_token}");
    }

    #[test]
    fn zeroize_clears_every_field() {
        let mut creds = Credentials::new("AKIAEXAMPLE", "secret", Some("token".into()));
        creds.zeroize();
        assert!(creds.access_key_id.is_empty());
        assert!(creds.secret_access_key.is_empty());
        assert_eq!(creds.session_token, None);
    }

    #[test]
    fn options_follow_settings() {
        let mut settings = Settings::default();
        settings.advanced.max_attempts = 2;
        settings.advanced.connect_timeout_secs = 3;
        settings.advanced.read_timeout_secs = 4;
        settings.data_integrity.checksum_algorithm = ChecksumAlgorithm::None;
        settings.data_integrity.validate_downloads = false;
        let options = ClientOptions::from_settings(&settings);
        assert_eq!(options.max_attempts, 2);
        assert_eq!(options.connect_timeout, Duration::from_secs(3));
        assert_eq!(options.read_timeout, Duration::from_secs(4));
        assert_eq!(
            options.request_checksum_calculation(),
            RequestChecksumCalculation::WhenRequired
        );
        assert_eq!(
            options.response_checksum_validation(),
            ResponseChecksumValidation::WhenRequired
        );

        let defaults = ClientOptions::default();
        assert_eq!(defaults.max_attempts, 5);
        assert_eq!(
            defaults.request_checksum_calculation(),
            RequestChecksumCalculation::WhenSupported
        );
        assert_eq!(
            defaults.response_checksum_validation(),
            ResponseChecksumValidation::WhenSupported
        );
        assert!(defaults.stalled_stream.upload_enabled());
        assert!(defaults.stalled_stream.download_enabled());
        assert_eq!(defaults.stalled_stream.grace_period(), STALL_GRACE_PERIOD);
        assert_eq!(defaults.read_timeout, Duration::from_secs(60));
    }

    #[test]
    fn client_config_reflects_account_and_options() {
        let mut acct = account();
        acct.endpoint = Some("http://127.0.0.1:9000/".into());
        acct.path_style = true;
        let options = ClientOptions {
            max_attempts: 3,
            connect_timeout: Duration::from_secs(7),
            read_timeout: Duration::from_secs(8),
            ..ClientOptions::default()
        };
        let client = build_client(&acct, &Credentials::new("AK", "SK", None), &options).unwrap();
        let config = client.config();
        assert_eq!(config.region().map(Region::as_ref), Some("eu-west-1"));
        assert_eq!(
            config.retry_config().map(RetryConfig::max_attempts),
            Some(3)
        );
        let timeouts = config.timeout_config().unwrap();
        assert_eq!(timeouts.connect_timeout(), Some(Duration::from_secs(7)));
        // The read timeout is applied per request, never on the client: it
        // would also limit the time to upload a request body.
        assert_eq!(timeouts.read_timeout(), None);
        assert_eq!(timeouts.operation_timeout(), None);
        assert_eq!(timeouts.operation_attempt_timeout(), None);
        assert!(
            config
                .interceptors()
                .any(|interceptor| interceptor.name() == ContentMd5.name())
        );
        assert_eq!(
            config.request_checksum_calculation(),
            Some(&RequestChecksumCalculation::WhenSupported)
        );
        assert_eq!(
            config.response_checksum_validation(),
            Some(&ResponseChecksumValidation::WhenSupported)
        );
        assert!(
            config
                .stalled_stream_protection()
                .is_some_and(StalledStreamProtectionConfig::is_enabled)
        );
        assert_eq!(config.app_name().map(AppName::as_ref), Some(APP_NAME));
    }

    #[test]
    fn invalid_inputs_are_usage_errors() {
        let options = ClientOptions::default();
        let creds = Credentials::new("AK", "SK", None);
        let check =
            |acct: &Account, creds: &Credentials, options: &ClientOptions, expected: &str| {
                let err = build_client(acct, creds, options).unwrap_err();
                assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
                assert!(err.to_string().contains(expected), "{err}");
            };
        check(
            &account(),
            &Credentials::new(" ", "SK", None),
            &options,
            "access key ID is empty",
        );
        check(
            &account(),
            &Credentials::new("AK", "", None),
            &options,
            "secret access key is empty",
        );
        let mut acct = account();
        acct.region = String::new();
        check(&acct, &creds, &options, "region is empty");
        acct.region = "eu west".into();
        check(&acct, &creds, &options, "contains the character ' '");
        let mut acct = account();
        acct.endpoint = Some("ftp://example.com".into());
        check(&acct, &creds, &options, "http:// or https://");
        let zero = ClientOptions {
            max_attempts: 0,
            ..ClientOptions::default()
        };
        check(&account(), &creds, &zero, "max_attempts");
    }

    #[test]
    fn endpoint_validation() {
        for ok in [
            "http://127.0.0.1:9000",
            "https://s3.example.com",
            "https://s3.example.com/",
            "http://[::1]:9000",
            "HTTPS://S3.EXAMPLE.COM",
            "https://example.com/base/path",
        ] {
            assert!(validate_endpoint(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            " https://example.com",
            "example.com",
            "s3.example.com:9000",
            "ftp://example.com",
            "https://",
            "https://user:pass@example.com",
            "https://example.com?x=1",
            "https://example.com/#frag",
            "http://exa mple.com",
        ] {
            assert!(validate_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn read_timeout_override_sets_only_the_read_timeout() {
        let config = read_timeout_override(Duration::from_secs(9)).build();
        let timeouts = config.timeout_config().unwrap();
        assert_eq!(timeouts.read_timeout(), Some(Duration::from_secs(9)));
        assert_eq!(timeouts.connect_timeout(), None);
        assert_eq!(timeouts.operation_timeout(), None);
        assert_eq!(timeouts.operation_attempt_timeout(), None);
        assert_eq!(config.region(), None);
    }

    #[test]
    fn endpoint_errors_never_show_credentials() {
        for endpoint in [
            "https://AKIDEXAMPLE:wJalrSECRETKEY@s3.example.com",
            "https://AKIDEXAMPLE:wJalrSECRETKEY@s3.example.com/base?x=1",
            " https://AKIDEXAMPLE:wJalrSECRETKEY@s3.example.com",
            "https://AKIDEXAMPLE:wJalr SECRET/KEY@s3.example.com",
            "ftp://AKIDEXAMPLE:wJalrSECRETKEY@s3.example.com",
            "AKIDEXAMPLE:wJalrSECRETKEY@s3.example.com:9000",
        ] {
            let err = validate_endpoint(endpoint).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage);
            let text = err.to_string();
            assert!(!text.contains("AKIDEXAMPLE"), "{text}");
            assert!(!text.contains("SECRET"), "{text}");
            assert!(text.contains("<redacted>@s3.example.com"), "{text}");

            let mut acct = account();
            acct.endpoint = Some(endpoint.to_owned());
            let err = build_client(
                &acct,
                &Credentials::new("AK", "SK", None),
                &ClientOptions::default(),
            )
            .unwrap_err();
            assert!(!err.to_string().contains("SECRET"), "{err}");
        }
        // Other messages still show the URL.
        let err = validate_endpoint("ftp://s3.example.com").unwrap_err();
        assert!(err.to_string().contains("'ftp://s3.example.com'"), "{err}");
    }

    #[test]
    fn redaction_hides_user_information_only() {
        assert_eq!(
            redact_endpoint("https://user:pass@host:9000/base"),
            "https://<redacted>@host:9000/base"
        );
        assert_eq!(redact_endpoint("http://u@h"), "http://<redacted>@h");
        assert_eq!(redact_endpoint("u:p@h"), "<redacted>@h");
        assert_eq!(redact_endpoint("u:p@x@h"), "<redacted>@h");
        assert_eq!(redact_endpoint("user@host://x"), "<redacted>@host://x");
        assert_eq!(
            redact_endpoint("https://s3.example.com/"),
            "https://s3.example.com/"
        );
        assert_eq!(redact_endpoint(""), "");
    }

    #[test]
    fn region_validation() {
        for ok in ["us-east-1", "auto", "garage", "eu_central.1"] {
            assert!(validate_region(ok).is_ok(), "{ok}");
        }
        for bad in ["", "us east", "us/east", "région"] {
            assert!(validate_region(bad).is_err(), "{bad}");
        }
    }
}
