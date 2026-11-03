//! Error model shared by the core, the CLI and the desktop host.
//!
//! Every failure is an [`Error`]. Its [`ErrorKind`] is the stable category that
//! the desktop IPC contract exposes as `BbError.code` and that the CLI maps to
//! an exit code (see [`ErrorKind::exit_code`]). An [`Error`] serializes to the
//! `{ "code": ..., "message": ... }` shape documented in `docs/desktop-api.md`.
//!
//! AWS SDK failures (`SdkError<E, R>` from any `aws-sdk-*` crate, S3 and STS
//! alike) convert into [`Error::Service`] through `From`, keeping the service
//! error code, message, HTTP status and request IDs in a [`ServiceError`].
//! A request that failed because the proxy rejected its credentials is an
//! [`ErrorKind::Auth`] failure (see [`crate::proxy::is_proxy_auth_failure`]).

use std::any::Any;
use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;

use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use serde::{Deserialize, Serialize, Serializer};

/// Result alias used throughout the core.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Stable error category.
///
/// The serialized form (`"usage"`, `"not_found"`, ...) is the `BbErrorCode` of
/// the desktop IPC contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Invalid input from the caller.
    Usage,
    /// Missing or invalid credentials, access denied, or a wrong master password.
    Auth,
    /// A master password is set and the secrets are locked.
    Locked,
    /// The account has an MFA serial but no MFA session.
    MfaRequired,
    /// A bucket, key, account, task or plan does not exist.
    NotFound,
    /// Bucket not empty, label already used, destination exists, and similar.
    Conflict,
    /// Any other S3 or network failure.
    Remote,
    /// Local filesystem failure (including an unreadable configuration file).
    Io,
    /// The operation was cancelled.
    Cancelled,
    /// A bug or an unexpected internal state.
    Internal,
}

impl ErrorKind {
    /// Every kind, in declaration order.
    pub const ALL: [ErrorKind; 10] = [
        ErrorKind::Usage,
        ErrorKind::Auth,
        ErrorKind::Locked,
        ErrorKind::MfaRequired,
        ErrorKind::NotFound,
        ErrorKind::Conflict,
        ErrorKind::Remote,
        ErrorKind::Io,
        ErrorKind::Cancelled,
        ErrorKind::Internal,
    ];

    /// The wire code, identical to the serde representation.
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::Usage => "usage",
            ErrorKind::Auth => "auth",
            ErrorKind::Locked => "locked",
            ErrorKind::MfaRequired => "mfa_required",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Remote => "remote",
            ErrorKind::Io => "io",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::Internal => "internal",
        }
    }

    /// Process exit code used by the `bb` CLI for this kind.
    ///
    /// `usage` 1; `remote`, `not_found`, `conflict`, `io` and `internal` 2;
    /// `auth`, `locked`, `mfa_required` 3; `cancelled` 130 (the shell
    /// convention for an interrupted command). Success is 0.
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorKind::Usage => 1,
            ErrorKind::Remote
            | ErrorKind::NotFound
            | ErrorKind::Conflict
            | ErrorKind::Io
            | ErrorKind::Internal => 2,
            ErrorKind::Auth | ErrorKind::Locked | ErrorKind::MfaRequired => 3,
            ErrorKind::Cancelled => 130,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The JSON shape of an error crossing the IPC boundary (`BbError`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BbError {
    /// Error category.
    pub code: ErrorKind,
    /// Human-readable description.
    pub message: String,
}

/// Any BucketBridge failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid input from the caller.
    #[error("{0}")]
    Usage(String),
    /// Missing or invalid credentials, or a wrong master password.
    #[error("{0}")]
    Auth(String),
    /// Secrets are locked behind the master password.
    #[error("secrets are locked; unlock them with the master password first")]
    Locked,
    /// The account requires an MFA session that does not exist yet.
    #[error("{0}")]
    MfaRequired(String),
    /// Something that was referenced does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The request conflicts with the current state.
    #[error("{0}")]
    Conflict(String),
    /// A remote failure that did not come from an SDK call.
    #[error("{0}")]
    Remote(String),
    /// A failed AWS SDK call (S3, STS, ...); the kind is derived from the
    /// service error code and HTTP status.
    #[error(transparent)]
    Service(Box<ServiceError>),
    /// A local I/O failure.
    #[error("{context}: {source}")]
    Io {
        /// What was being done, for example `"reading /path/config.toml"`.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A configuration file exists but cannot be used.
    #[error("invalid configuration file {}: {message}", path.display())]
    Config {
        /// The offending file.
        path: PathBuf,
        /// Why it was rejected.
        message: String,
    },
    /// The operation was cancelled.
    #[error("operation cancelled")]
    Cancelled,
    /// A bug or an unexpected internal state.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Builds an [`Error::Io`] with a description of what was being done.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// The stable category of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::Usage(_) => ErrorKind::Usage,
            Error::Auth(_) => ErrorKind::Auth,
            Error::Locked => ErrorKind::Locked,
            Error::MfaRequired(_) => ErrorKind::MfaRequired,
            Error::NotFound(_) => ErrorKind::NotFound,
            Error::Conflict(_) => ErrorKind::Conflict,
            Error::Remote(_) => ErrorKind::Remote,
            Error::Service(service) => service.kind(),
            Error::Io { .. } | Error::Config { .. } => ErrorKind::Io,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::Internal(_) => ErrorKind::Internal,
        }
    }

    /// CLI exit code for this error (see [`ErrorKind::exit_code`]).
    pub fn exit_code(&self) -> u8 {
        self.kind().exit_code()
    }

    /// The SDK failure details, when this error came from an SDK call.
    pub fn service(&self) -> Option<&ServiceError> {
        match self {
            Error::Service(service) => Some(service),
            _ => None,
        }
    }

    /// The `{code, message}` payload sent over IPC.
    pub fn to_payload(&self) -> BbError {
        BbError {
            code: self.kind(),
            message: self.to_string(),
        }
    }

    /// Prefixes the message with `context` (for example the S3 URI involved),
    /// keeping the kind unchanged.
    #[must_use]
    pub fn with_context(self, context: impl fmt::Display) -> Self {
        match self {
            Error::Usage(m) => Error::Usage(format!("{context}: {m}")),
            Error::Auth(m) => Error::Auth(format!("{context}: {m}")),
            Error::MfaRequired(m) => Error::MfaRequired(format!("{context}: {m}")),
            Error::NotFound(m) => Error::NotFound(format!("{context}: {m}")),
            Error::Conflict(m) => Error::Conflict(format!("{context}: {m}")),
            Error::Remote(m) => Error::Remote(format!("{context}: {m}")),
            Error::Internal(m) => Error::Internal(format!("{context}: {m}")),
            Error::Service(mut service) => {
                service.context = Some(match service.context.take() {
                    Some(inner) => format!("{context}: {inner}"),
                    None => context.to_string(),
                });
                Error::Service(service)
            }
            Error::Io {
                context: inner,
                source,
            } => Error::Io {
                context: format!("{context}: {inner}"),
                source,
            },
            other @ (Error::Locked | Error::Cancelled | Error::Config { .. }) => other,
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.to_payload().serialize(serializer)
    }
}

/// Attaches context to `std::io::Result` values.
pub trait IoContext<T> {
    /// Converts an I/O error into [`Error::Io`] with the given context.
    fn io_context<C: Into<String>>(self, context: impl FnOnce() -> C) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn io_context<C: Into<String>>(self, context: impl FnOnce() -> C) -> Result<T> {
        self.map_err(|source| Error::io(context(), source))
    }
}

/// Details of a failed AWS SDK call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    kind: ErrorKind,
    code: Option<String>,
    message: Option<String>,
    status: Option<u16>,
    request_id: Option<String>,
    extended_request_id: Option<String>,
    context: Option<String>,
}

impl ServiceError {
    /// Category derived from the code and status (see [`classify`]).
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Service error code such as `NoSuchKey`, when the service sent one.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Service-provided message, or a description of a transport failure.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// HTTP status of the response, when one was received.
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// `x-amz-request-id` (or `x-amzn-requestid`) of the failed request.
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// S3 extended request ID (`x-amz-id-2`), needed when contacting AWS support.
    pub fn extended_request_id(&self) -> Option<&str> {
        self.extended_request_id.as_deref()
    }

    /// True when the server reported that it does not implement the
    /// operation (HTTP 501 or code `NotImplemented`), as some S3-compatible
    /// servers do for optional APIs.
    pub fn is_not_implemented(&self) -> bool {
        self.status == Some(501) || self.code.as_deref() == Some("NotImplemented")
    }

    fn from_sdk<E, R>(err: &SdkError<E, R>) -> Self
    where
        E: ProvideErrorMetadata + StdError + 'static,
        R: fmt::Debug + 'static,
    {
        let raw = err
            .raw_response()
            .and_then(|raw| (raw as &dyn Any).downcast_ref::<HttpResponse>());
        let status = raw.map(|raw| raw.status().as_u16());
        let header = |name: &str| {
            raw.and_then(|raw| raw.headers().get(name))
                .map(str::to_owned)
        };
        let header_request_id =
            || header("x-amz-request-id").or_else(|| header("x-amzn-requestid"));

        match err {
            SdkError::ServiceError(_) => {
                let meta = err.meta();
                let code = meta.code().map(str::to_owned);
                let message = meta.message().map(str::to_owned).filter(|m| !m.is_empty());
                ServiceError {
                    kind: classify(code.as_deref(), status),
                    code,
                    message,
                    status,
                    request_id: meta
                        .extra("aws_request_id")
                        .map(str::to_owned)
                        .or_else(header_request_id),
                    extended_request_id: meta
                        .extra("s3_extended_request_id")
                        .map(str::to_owned)
                        .or_else(|| header("x-amz-id-2")),
                    context: None,
                }
            }
            SdkError::ResponseError(_) => ServiceError {
                kind: classify(None, status),
                code: None,
                message: Some(format!(
                    "the response could not be understood: {}",
                    source_chain(err)
                )),
                status,
                request_id: header_request_id(),
                extended_request_id: header("x-amz-id-2"),
                context: None,
            },
            SdkError::ConstructionFailure(_) => {
                Self::transport(ErrorKind::Usage, "the request could not be built", err)
            }
            SdkError::TimeoutError(_) => {
                Self::transport(ErrorKind::Remote, "the request timed out", err)
            }
            SdkError::DispatchFailure(_) if crate::proxy::is_proxy_auth_failure(err) => {
                Self::transport(ErrorKind::Auth, "proxy authentication failed", err)
            }
            SdkError::DispatchFailure(_) if has_credentials_failure(err) => Self::transport(
                ErrorKind::Auth,
                "the credentials of the account could not be loaded",
                err,
            ),
            SdkError::DispatchFailure(_) => Self::transport(
                ErrorKind::Remote,
                "the request failed before a response was received",
                err,
            ),
            _ => Self::transport(ErrorKind::Remote, "the request failed", err),
        }
    }

    fn transport(kind: ErrorKind, what: &str, err: &dyn StdError) -> Self {
        let chain = source_chain(err);
        ServiceError {
            kind,
            code: None,
            message: Some(if chain.is_empty() {
                what.to_owned()
            } else {
                format!("{what}: {chain}")
            }),
            status: None,
            request_id: None,
            extended_request_id: None,
            context: None,
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(context) = &self.context {
            write!(f, "{context}: ")?;
        }
        let headline = self
            .message
            .as_deref()
            .or(self.code.as_deref())
            .unwrap_or("the service returned an error");
        f.write_str(headline)?;

        let mut details = Vec::new();
        if self.message.is_some()
            && let Some(code) = &self.code
        {
            details.push(code.clone());
        }
        if let Some(status) = self.status {
            details.push(format!("HTTP {status}"));
        }
        if let Some(request_id) = &self.request_id {
            details.push(format!("request id {request_id}"));
        }
        if !details.is_empty() {
            write!(f, " ({})", details.join(", "))?;
        }
        if let Some(hint) = self.code.as_deref().and_then(hint) {
            write!(f, "; {hint}")?;
        }
        Ok(())
    }
}

impl StdError for ServiceError {}

impl<E, R> From<SdkError<E, R>> for Error
where
    E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
    R: fmt::Debug + Send + Sync + 'static,
{
    fn from(err: SdkError<E, R>) -> Self {
        Error::Service(Box::new(ServiceError::from_sdk(&err)))
    }
}

/// Codes that mean the caller is not authenticated or not authorized.
const AUTH_CODES: &[&str] = &[
    "InvalidAccessKeyId",
    "SignatureDoesNotMatch",
    "ExpiredToken",
    "InvalidToken",
    "AccessDenied",
    // STS and other query-protocol services.
    "InvalidClientTokenId",
    "ExpiredTokenException",
    "UnrecognizedClientException",
    "MissingAuthenticationToken",
    "AccessDeniedException",
    // IAM Identity Center portal APIs: the sign-in token was rejected.
    "UnauthorizedException",
];

/// Codes that mean the referenced resource does not exist.
const NOT_FOUND_CODES: &[&str] = &[
    "NoSuchBucket",
    "NoSuchKey",
    "NoSuchUpload",
    "NoSuchVersion",
    "NotFound",
];

/// Codes that mean the request conflicts with the resource state.
const CONFLICT_CODES: &[&str] = &[
    "BucketNotEmpty",
    "BucketAlreadyExists",
    "BucketAlreadyOwnedByYou",
    // HTTP 403, but about the object, not the credentials: reading an
    // archived (GLACIER, DEEP_ARCHIVE) object that is not restored, or an
    // Intelligent-Tiering object in an archive tier.
    "InvalidObjectState",
    "ObjectNotInActiveTierError",
];

/// Codes sent with HTTP 403 that are not about credentials or permissions
/// and must not fall back to [`ErrorKind::Auth`].
const REMOTE_CODES: &[&str] = &[
    // The clock of this computer is off; see `hint`.
    "RequestTimeTooSkewed",
    // Server access logging into a bucket of another region.
    "CrossLocationLoggingProhibited",
];

/// Classifies a service failure from its error code and HTTP status.
///
/// The code wins over the status (S3 reports `ExpiredToken` with HTTP 400,
/// and `InvalidObjectState` or `RequestTimeTooSkewed` with HTTP 403, for
/// example). Unknown codes fall back to the status: 401 and 403 are
/// [`ErrorKind::Auth`], 404 is [`ErrorKind::NotFound`], 409 is
/// [`ErrorKind::Conflict`]; everything else is [`ErrorKind::Remote`].
pub fn classify(code: Option<&str>, status: Option<u16>) -> ErrorKind {
    if let Some(code) = code {
        if AUTH_CODES.contains(&code) {
            return ErrorKind::Auth;
        }
        if NOT_FOUND_CODES.contains(&code) {
            return ErrorKind::NotFound;
        }
        if CONFLICT_CODES.contains(&code) {
            return ErrorKind::Conflict;
        }
        if REMOTE_CODES.contains(&code) {
            return ErrorKind::Remote;
        }
    }
    match status {
        Some(401 | 403) => ErrorKind::Auth,
        Some(404) => ErrorKind::NotFound,
        Some(409) => ErrorKind::Conflict,
        _ => ErrorKind::Remote,
    }
}

/// Advice appended to the message of some service errors.
fn hint(code: &str) -> Option<&'static str> {
    match code {
        "RequestTimeTooSkewed" => Some(
            "the clock of this computer differs too much from the server's; \
             correct its date, time and time zone",
        ),
        "InvalidObjectState" | "ObjectNotInActiveTierError" => {
            Some("the object is archived; restore it before reading it")
        }
        _ => None,
    }
}

/// Joins the messages of the `source()` chain below `err`, skipping messages
/// that repeat the previous one (connectors often wrap the same text twice).
/// Whether a credentials provider failed somewhere in the causes of `err`
/// (an assumed role, an AWS profile, IAM Identity Center or the instance
/// role that could not give credentials).
fn has_credentials_failure(err: &dyn StdError) -> bool {
    let mut next = err.source();
    while let Some(current) = next {
        if current
            .downcast_ref::<aws_credential_types::provider::error::CredentialsError>()
            .is_some()
        {
            return true;
        }
        next = current.source();
    }
    false
}

fn source_chain(err: &dyn StdError) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut next = err.source();
    while let Some(current) = next {
        let text = current.to_string();
        let repeated = parts.last().is_some_and(|last| last.contains(&text));
        if !text.is_empty() && !repeated {
            parts.push(text);
        }
        next = current.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::operation::head_object::HeadObjectError;
    use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
    use aws_sdk_s3::types::error::NoSuchBucket;
    use aws_smithy_runtime_api::client::result::ConnectorError;
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::body::SdkBody;
    use aws_smithy_types::error::ErrorMetadata;

    fn response(status: u16, headers: &[(&'static str, &'static str)]) -> HttpResponse {
        let mut response = HttpResponse::new(
            StatusCode::try_from(status).expect("valid status"),
            SdkBody::empty(),
        );
        for (name, value) in headers {
            response.headers_mut().insert(*name, *value);
        }
        response
    }

    fn service_error(code: &str, message: Option<&str>, status: u16) -> Error {
        let mut meta = ErrorMetadata::builder()
            .code(code)
            .custom("aws_request_id", "REQ123");
        if let Some(message) = message {
            meta = meta.message(message);
        }
        let err = ListObjectsV2Error::generic(meta.build());
        Error::from(SdkError::service_error(err, response(status, &[])))
    }

    #[test]
    fn wire_codes_match_serde_and_contract() {
        let expected = [
            "usage",
            "auth",
            "locked",
            "mfa_required",
            "not_found",
            "conflict",
            "remote",
            "io",
            "cancelled",
            "internal",
        ];
        for (kind, code) in ErrorKind::ALL.iter().zip(expected) {
            assert_eq!(kind.code(), code);
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(code));
            let back: ErrorKind = serde_json::from_value(serde_json::json!(code)).unwrap();
            assert_eq!(back, *kind);
        }
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        assert_eq!(ErrorKind::Usage.exit_code(), 1);
        for kind in [
            ErrorKind::Remote,
            ErrorKind::NotFound,
            ErrorKind::Conflict,
            ErrorKind::Io,
        ] {
            assert_eq!(kind.exit_code(), 2, "{kind}");
        }
        for kind in [ErrorKind::Auth, ErrorKind::Locked, ErrorKind::MfaRequired] {
            assert_eq!(kind.exit_code(), 3, "{kind}");
        }
        assert_eq!(ErrorKind::Internal.exit_code(), 2);
        assert_eq!(ErrorKind::Cancelled.exit_code(), 130);
    }

    #[test]
    fn classify_prefers_code_over_status() {
        assert_eq!(classify(Some("ExpiredToken"), Some(400)), ErrorKind::Auth);
        assert_eq!(classify(Some("InvalidToken"), Some(400)), ErrorKind::Auth);
        assert_eq!(
            classify(Some("InvalidAccessKeyId"), Some(403)),
            ErrorKind::Auth
        );
        assert_eq!(
            classify(Some("SignatureDoesNotMatch"), Some(403)),
            ErrorKind::Auth
        );
        assert_eq!(classify(Some("AccessDenied"), None), ErrorKind::Auth);
        assert_eq!(
            classify(Some("NoSuchBucket"), Some(404)),
            ErrorKind::NotFound
        );
        assert_eq!(classify(Some("NoSuchKey"), None), ErrorKind::NotFound);
        assert_eq!(
            classify(Some("NoSuchUpload"), Some(404)),
            ErrorKind::NotFound
        );
        assert_eq!(classify(Some("NotFound"), None), ErrorKind::NotFound);
        assert_eq!(
            classify(Some("BucketNotEmpty"), Some(409)),
            ErrorKind::Conflict
        );
        assert_eq!(
            classify(Some("BucketAlreadyExists"), None),
            ErrorKind::Conflict
        );
        assert_eq!(
            classify(Some("BucketAlreadyOwnedByYou"), None),
            ErrorKind::Conflict
        );
    }

    #[test]
    fn forbidden_codes_that_are_not_about_credentials() {
        // An archived object that is not restored is a state conflict.
        assert_eq!(
            classify(Some("InvalidObjectState"), Some(403)),
            ErrorKind::Conflict
        );
        assert_eq!(
            classify(Some("ObjectNotInActiveTierError"), Some(403)),
            ErrorKind::Conflict
        );
        // A wrong clock or a cross-region logging target is not an
        // authentication problem.
        assert_eq!(
            classify(Some("RequestTimeTooSkewed"), Some(403)),
            ErrorKind::Remote
        );
        assert_eq!(
            classify(Some("CrossLocationLoggingProhibited"), Some(403)),
            ErrorKind::Remote
        );
        // Credential and permission codes stay `auth`.
        for code in [
            "AccessDenied",
            "InvalidAccessKeyId",
            "SignatureDoesNotMatch",
            "AccountProblem",
            "AllAccessDisabled",
            "InvalidSecurity",
        ] {
            assert_eq!(classify(Some(code), Some(403)), ErrorKind::Auth, "{code}");
        }

        let err = service_error(
            "RequestTimeTooSkewed",
            Some("The difference between the request time and the current time is too large."),
            403,
        );
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert_eq!(err.exit_code(), 2);
        let text = err.to_string();
        assert!(text.contains("RequestTimeTooSkewed, HTTP 403"), "{text}");
        assert!(
            text.contains("correct its date, time and time zone"),
            "{text}"
        );

        let err = service_error(
            "InvalidObjectState",
            Some("The operation is not valid for the object's storage class"),
            403,
        )
        .with_context("s3://b/archived.bin");
        assert_eq!(err.kind(), ErrorKind::Conflict);
        let text = err.to_string();
        assert!(text.starts_with("s3://b/archived.bin: "), "{text}");
        assert!(text.contains("restore it before reading it"), "{text}");

        // No hint for other codes.
        let err = service_error("AccessDenied", Some("Access Denied"), 403);
        assert_eq!(
            err.to_string(),
            "Access Denied (AccessDenied, HTTP 403, request id REQ123)"
        );
    }

    #[test]
    fn classify_falls_back_to_status() {
        assert_eq!(classify(Some("AccountProblem"), Some(403)), ErrorKind::Auth);
        assert_eq!(classify(None, Some(401)), ErrorKind::Auth);
        assert_eq!(classify(Some("Forbidden"), Some(403)), ErrorKind::Auth);
        assert_eq!(
            classify(Some("NoSuchLifecycleConfiguration"), Some(404)),
            ErrorKind::NotFound
        );
        assert_eq!(
            classify(Some("OperationAborted"), Some(409)),
            ErrorKind::Conflict
        );
        assert_eq!(classify(Some("SlowDown"), Some(503)), ErrorKind::Remote);
        assert_eq!(
            classify(Some("InternalError"), Some(500)),
            ErrorKind::Remote
        );
        assert_eq!(
            classify(Some("InvalidBucketName"), Some(400)),
            ErrorKind::Remote
        );
        assert_eq!(classify(None, None), ErrorKind::Remote);
    }

    #[test]
    fn service_error_keeps_code_message_status_and_request_ids() {
        let meta = ErrorMetadata::builder()
            .code("NoSuchBucket")
            .message("The specified bucket does not exist")
            .custom("aws_request_id", "REQ-1")
            .custom("s3_extended_request_id", "EXT-1")
            .build();
        let err = ListObjectsV2Error::NoSuchBucket(NoSuchBucket::builder().meta(meta).build());
        let err = Error::from(SdkError::service_error(err, response(404, &[])));
        assert_eq!(err.kind(), ErrorKind::NotFound);
        let service = err.service().expect("service details");
        assert_eq!(service.code(), Some("NoSuchBucket"));
        assert_eq!(
            service.message(),
            Some("The specified bucket does not exist")
        );
        assert_eq!(service.status(), Some(404));
        assert_eq!(service.request_id(), Some("REQ-1"));
        assert_eq!(service.extended_request_id(), Some("EXT-1"));
        assert_eq!(
            err.to_string(),
            "The specified bucket does not exist (NoSuchBucket, HTTP 404, request id REQ-1)"
        );
    }

    #[test]
    fn request_id_falls_back_to_response_headers() {
        let err = HeadObjectError::generic(ErrorMetadata::builder().code("Forbidden").build());
        let raw = response(
            403,
            &[("x-amz-request-id", "HDR-9"), ("x-amz-id-2", "HOST-9")],
        );
        let err = Error::from(SdkError::service_error(err, raw));
        assert_eq!(err.kind(), ErrorKind::Auth);
        let service = err.service().expect("service details");
        assert_eq!(service.request_id(), Some("HDR-9"));
        assert_eq!(service.extended_request_id(), Some("HOST-9"));
        assert_eq!(err.to_string(), "Forbidden (HTTP 403, request id HDR-9)");
    }

    #[test]
    fn auth_and_conflict_service_errors() {
        assert_eq!(
            service_error("SignatureDoesNotMatch", Some("bad signature"), 403).kind(),
            ErrorKind::Auth
        );
        assert_eq!(
            service_error("BucketNotEmpty", Some("not empty"), 409).kind(),
            ErrorKind::Conflict
        );
        assert_eq!(
            service_error("InternalError", None, 500).kind(),
            ErrorKind::Remote
        );
    }

    #[test]
    fn transport_failures_are_classified() {
        let dispatch: SdkError<ListObjectsV2Error, HttpResponse> =
            SdkError::dispatch_failure(ConnectorError::io("connection refused".into()));
        let err = Error::from(dispatch);
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert!(err.to_string().contains("connection refused"), "{err}");

        let timeout: SdkError<ListObjectsV2Error, HttpResponse> =
            SdkError::timeout_error("read timed out");
        let err = Error::from(timeout);
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert!(err.to_string().contains("timed out"), "{err}");

        let construction: SdkError<ListObjectsV2Error, HttpResponse> =
            SdkError::construction_failure("bucket name is invalid");
        let err = Error::from(construction);
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("bucket name is invalid"), "{err}");

        let response_error: SdkError<ListObjectsV2Error, HttpResponse> =
            SdkError::response_error("truncated body", response(403, &[]));
        let err = Error::from(response_error);
        assert_eq!(err.kind(), ErrorKind::Auth);
        assert_eq!(err.service().and_then(ServiceError::status), Some(403));
    }

    #[test]
    fn generic_conversion_accepts_other_raw_response_types() {
        // Event-stream errors carry a raw message instead of an HTTP response.
        let err: SdkError<ListObjectsV2Error, String> = SdkError::service_error(
            ListObjectsV2Error::generic(ErrorMetadata::builder().code("AccessDenied").build()),
            "raw event".to_owned(),
        );
        let err = Error::from(err);
        assert_eq!(err.kind(), ErrorKind::Auth);
        assert_eq!(err.service().and_then(ServiceError::status), None);
    }

    #[test]
    fn errors_serialize_to_the_bb_error_shape() {
        let err = Error::NotFound("bucket missing".into());
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            serde_json::json!({ "code": "not_found", "message": "bucket missing" })
        );
        let err = Error::io("reading x", std::io::Error::other("boom"));
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            serde_json::json!({ "code": "io", "message": "reading x: boom" })
        );
        let payload: BbError =
            serde_json::from_value(serde_json::json!({ "code": "mfa_required", "message": "m" }))
                .unwrap();
        assert_eq!(payload.code, ErrorKind::MfaRequired);
    }

    #[test]
    fn with_context_prefixes_without_changing_kind() {
        let err = service_error("NoSuchKey", None, 404).with_context("s3://b/k");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert_eq!(
            err.to_string(),
            "s3://b/k: NoSuchKey (HTTP 404, request id REQ123)"
        );

        let err = Error::Conflict("exists".into()).with_context("s3://b/k");
        assert_eq!(err.kind(), ErrorKind::Conflict);
        assert_eq!(err.to_string(), "s3://b/k: exists");

        let err = Error::io("reading", std::io::Error::other("x")).with_context("cfg");
        assert_eq!(err.to_string(), "cfg: reading: x");
        assert_eq!(
            Error::Cancelled.with_context("ignored").to_string(),
            "operation cancelled"
        );
    }

    #[test]
    fn io_context_helper() {
        let result: std::io::Result<()> =
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        let err = result.io_context(|| "opening file").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        assert_eq!(err.to_string(), "opening file: gone");
    }

    #[test]
    fn not_implemented_detection() {
        let err = service_error("NotImplemented", Some("nope"), 501);
        assert!(err.service().is_some_and(ServiceError::is_not_implemented));
        let err = service_error("AccessDenied", None, 403);
        assert!(!err.service().is_some_and(ServiceError::is_not_implemented));
    }
}
