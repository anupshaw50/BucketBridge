//! Bucket configuration: the policy, the public access block, versioning,
//! server access logging, static website hosting and transfer acceleration of
//! a bucket, read and written through [`S3`], plus the external buckets that
//! an account pins to its bucket list.
//!
//! # Sections
//!
//! Every setting has its own `get_*`, `put_*` and, where S3 has one,
//! `delete_*` (or `disable_*`) method on [`S3`]. A getter returns `None` (or
//! the "unset" state) when the bucket has nothing configured, so "not
//! configured" is an answer, never an error. [`S3::get_bucket_config`] reads
//! all sections at once and reports each one on its own as a [`Section`]: a
//! section the server refuses (`AccessDenied`) or does not implement
//! (`NotImplemented`, common on S3-compatible servers) never hides the
//! others.
//!
//! # Bucket policies
//!
//! [`check_policy`] validates a policy document the way an editor needs it:
//! every problem is reported with its path (`Statement[1].Effect`), not just
//! the first one. [`format_policy`] pretty-prints a document and keeps its
//! keys in the order the user wrote them. [`S3::put_bucket_policy`] validates
//! before it sends anything and sends the document without whitespace, which
//! is also how its size is measured against the 20 KiB limit of S3.
//!
//! # Website hosting, logging, versioning, acceleration
//!
//! The website types validate S3's rules before a request is sent
//! ([`WebsiteConfig::validate`]). Changing the logging target keeps the
//! target grants and the log object key format that the bucket already has
//! (they are not part of the model), so editing the prefix never resets
//! them. Versioning cannot be switched off once it was enabled, only
//! suspended, and MFA delete is read-only. Transfer acceleration exists on
//! AWS only; accounts with a custom endpoint get a clean "not supported".
//!
//! # External buckets
//!
//! A bucket the account can use but not list (`ListBuckets` is denied, or the
//! bucket belongs to another account) can be pinned to the account:
//! [`pin_bucket`], [`unpin_bucket`] and [`pinned_buckets`] keep the names in
//! `config.toml` (`pinned_buckets`, per account id), and
//! [`S3::list_buckets_with_pinned`] merges them into the bucket list with the
//! kind [`BucketKind::External`]. These functions block (they use the
//! configuration file); async callers use `spawn_blocking`.

use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::fmt;

use aws_sdk_s3::types::{
    AccelerateConfiguration, BucketAccelerateStatus, BucketLoggingStatus, BucketVersioningStatus,
    ErrorDocument, IndexDocument, LoggingEnabled, MfaDeleteStatus, Protocol,
    PublicAccessBlockConfiguration, RedirectAllRequestsTo, VersioningConfiguration,
    WebsiteConfiguration,
};
use aws_sdk_s3::types::{Condition as SdkCondition, Redirect as SdkRedirect};
use aws_sdk_s3::types::{RoutingRule as SdkRoutingRule, TargetObjectKeyFormat};
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, ConfigStore};
use crate::error::{BbError, Error, ErrorKind, Result, ServiceError};
use crate::model::{Bucket, BucketKind};
use crate::ops::S3;
use crate::s3uri::{MAX_KEY_BYTES, validate_bucket_name};

/// Largest bucket policy S3 accepts: 20 KiB. The policy is measured as it is
/// sent, without whitespace.
pub const MAX_POLICY_BYTES: usize = 20 * 1024;

/// Most routing rules one website configuration may have.
pub const MAX_ROUTING_RULES: usize = 50;

/// The policy language versions S3 accepts.
const POLICY_VERSIONS: [&str; 2] = ["2012-10-17", "2008-10-17"];

/// The elements of a policy document.
const POLICY_ELEMENTS: [&str; 3] = ["Id", "Statement", "Version"];

/// The elements of one statement.
const STATEMENT_ELEMENTS: [&str; 9] = [
    "Sid",
    "Effect",
    "Principal",
    "NotPrincipal",
    "Action",
    "NotAction",
    "Resource",
    "NotResource",
    "Condition",
];

/// Why transfer acceleration is not offered for an account.
const ACCELERATION_UNSUPPORTED: &str =
    "Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint.";

// ---- model ----

/// One independently read part of a [`BucketConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Section<T> {
    /// The section was read; `value` says what is configured.
    Loaded {
        /// What the bucket has configured.
        value: T,
    },
    /// The server does not implement this part of the bucket API.
    Unsupported {
        /// Why, for display.
        message: String,
    },
    /// Reading failed: access denied, a network failure, and so on.
    Failed {
        /// The failure, in the shape of every IPC error.
        error: BbError,
    },
}

impl<T> Section<T> {
    /// Turns the outcome of reading one section into a [`Section`]: a value
    /// is loaded, a server that does not implement the API (see
    /// [`is_unsupported`]) is unsupported, anything else is a failure.
    /// `feature` names the setting in the message ("bucket policies").
    pub fn from_result(result: Result<T>, feature: &str) -> Self {
        match result {
            Ok(value) => Section::Loaded { value },
            Err(error) if is_unsupported(&error) => Section::Unsupported {
                message: format!("This server does not support {feature}{}.", detail(&error)),
            },
            Err(error) => Section::Failed {
                error: error.to_payload(),
            },
        }
    }
}

/// The server's code and status in parentheses, for messages; empty without
/// either.
fn detail(error: &Error) -> String {
    let Some(service) = error.service() else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(code) = service.code() {
        parts.push(code.to_owned());
    }
    if let Some(status) = service.status() {
        parts.push(format!("HTTP {status}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// True for the answers of a server that does not implement an optional
/// bucket API: HTTP 501 or 405, or the codes `NotImplemented`,
/// `XNotImplemented` and `MethodNotAllowed`.
pub fn is_unsupported(error: &Error) -> bool {
    error.service().is_some_and(|service| {
        service.is_not_implemented()
            || service.status() == Some(405)
            || matches!(service.code(), Some("XNotImplemented" | "MethodNotAllowed"))
    })
}

/// Every configuration section of one bucket, read independently
/// ([`S3::get_bucket_config`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketConfig {
    /// The bucket policy as pretty-printed JSON; `null` without a policy.
    pub policy: Section<Option<String>>,
    /// The public access block; `null` when none is configured.
    pub public_access_block: Section<Option<PublicAccessBlock>>,
    /// Versioning state and MFA delete.
    pub versioning: Section<BucketVersioning>,
    /// Server access logging; `null` while it is off.
    pub logging: Section<Option<LoggingTarget>>,
    /// Static website hosting; `null` while the bucket is not a website.
    pub website: Section<Option<WebsiteConfig>>,
    /// Transfer acceleration (AWS only).
    pub acceleration: Section<AccelerationStatus>,
}

/// The four toggles of the public access block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PublicAccessBlock {
    /// Reject requests that would add a public ACL.
    pub block_public_acls: bool,
    /// Ignore every public ACL on the bucket and its objects.
    pub ignore_public_acls: bool,
    /// Reject bucket policies that grant public access.
    pub block_public_policy: bool,
    /// Restrict access to buckets with a public policy to the owner's
    /// account and AWS services.
    pub restrict_public_buckets: bool,
}

/// Whether versioning is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersioningStatus {
    /// Versioning was never enabled. It cannot be set back to this state.
    #[default]
    Unversioned,
    /// New object versions are kept.
    Enabled,
    /// No new versions are kept; existing versions stay.
    Suspended,
}

/// The versioning state of a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BucketVersioning {
    /// Whether versioning is enabled, suspended, or was never enabled.
    pub status: VersioningStatus,
    /// Whether deleting versions needs an MFA code. Read-only: it is never
    /// changed from here.
    pub mfa_delete: bool,
}

/// Where server access logs of a bucket are delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingTarget {
    /// The bucket that receives the log objects.
    pub target_bucket: String,
    /// Prefix of the log object keys, possibly empty.
    pub target_prefix: String,
}

/// Static website hosting of a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebsiteConfig {
    /// Serve the bucket as a website.
    Hosting {
        /// Object served for requests to a folder, such as `index.html`.
        index_document: String,
        /// Object served for errors, such as `error.html`.
        error_document: Option<String>,
        /// Redirects applied by conditions on the request.
        #[serde(default)]
        routing_rules: Vec<RoutingRule>,
    },
    /// Redirect every request to another host.
    Redirect {
        /// The host that receives the requests.
        host_name: String,
        /// The protocol of the redirect; the request's own when `null`.
        protocol: Option<RedirectProtocol>,
    },
}

/// The protocol of a website redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectProtocol {
    /// `http`.
    Http,
    /// `https`.
    Https,
}

/// One routing rule of a website: when the condition holds, the request is
/// redirected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingRule {
    /// When the rule applies; always, without one.
    pub condition: Option<RoutingCondition>,
    /// What happens to a matching request.
    pub redirect: RoutingRedirect,
}

/// When a routing rule applies. At least one field is set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoutingCondition {
    /// The request key starts with this prefix.
    pub key_prefix_equals: Option<String>,
    /// The request fails with this HTTP error code (a 4xx or 5xx code).
    pub http_error_code_returned_equals: Option<String>,
}

/// The redirect of a routing rule. At least one field is set, and at most one
/// of the two `replace_key_*` fields.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RoutingRedirect {
    /// The protocol of the redirect.
    pub protocol: Option<RedirectProtocol>,
    /// The host that receives the request.
    pub host_name: Option<String>,
    /// Replaces the key prefix of the request (the one the condition
    /// matched).
    pub replace_key_prefix_with: Option<String>,
    /// Replaces the whole key of the request.
    pub replace_key_with: Option<String>,
    /// The HTTP status of the redirect, a 3xx code (301 by default).
    pub http_redirect_code: Option<String>,
}

/// Whether transfer acceleration is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccelerationStatus {
    /// Never configured. It cannot be set back to this state.
    #[default]
    Unset,
    /// Transfers can use the accelerate endpoint.
    Enabled,
    /// The accelerate endpoint is off.
    Suspended,
}

// ---- policy documents ----

/// One problem found in a policy document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIssue {
    /// Where the problem is, such as `Statement[1].Effect`; empty for the
    /// document as a whole.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl PolicyIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        PolicyIssue {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PolicyIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// A JSON value whose objects keep their keys in document order. `serde_json`
/// sorts the keys of its own map, which would reorder a policy when it is
/// formatted.
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// How the value reads in a message ("a string", "an object").
    fn describe(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "a boolean",
            Json::Number(_) => "a number",
            Json::String(_) => "a string",
            Json::Array(_) => "an array",
            Json::Object(_) => "an object",
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;

        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Json;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Json, E> {
                Ok(Json::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Json, E> {
                Ok(Json::Number(value.into()))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Json, E> {
                Ok(Json::Number(value.into()))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Json, E> {
                serde_json::Number::from_f64(value)
                    .map(Json::Number)
                    .ok_or_else(|| E::custom("a number that JSON cannot represent"))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Json, E> {
                Ok(Json::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Json, E> {
                Ok(Json::String(value))
            }

            fn visit_unit<E>(self) -> std::result::Result<Json, E> {
                Ok(Json::Null)
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Json, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Json::Array(items))
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Json, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Json>()? {
                    entries.push(entry);
                }
                Ok(Json::Object(entries))
            }
        }

        deserializer.deserialize_any(JsonVisitor)
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Json::Null => serializer.serialize_unit(),
            Json::Bool(value) => serializer.serialize_bool(*value),
            Json::Number(number) => number.serialize(serializer),
            Json::String(text) => serializer.serialize_str(text),
            Json::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Json::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// Parses policy text; the syntax error, with its line and column, is the
/// only issue of a document that is not JSON.
fn parse_policy(text: &str) -> std::result::Result<Json, PolicyIssue> {
    if text.trim().is_empty() {
        return Err(PolicyIssue::new("", "the policy is empty"));
    }
    serde_json::from_str::<Json>(text)
        .map_err(|error| PolicyIssue::new("", format!("not valid JSON: {error}")))
}

/// The document without whitespace, as it is sent and measured.
fn compact(document: &Json) -> String {
    // Serializing a tree of strings, numbers and containers cannot fail; the
    // empty text would be caught as an invalid document by the caller.
    serde_json::to_string(document).unwrap_or_default()
}

/// Checks a bucket policy document and reports every problem found, each with
/// its path; an empty list means the policy is valid.
///
/// The rules: the text is JSON and an object; `Version` is `2012-10-17` or
/// `2008-10-17`; `Statement` is a non-empty list of objects (or one object);
/// each statement has an `Effect` of `Allow` or `Deny`, exactly one of
/// `Principal` and `NotPrincipal`, exactly one of `Action` and `NotAction`,
/// exactly one of `Resource` and `NotResource`, an optional `Condition`, and
/// a `Sid` that no other statement uses; no element is unknown or repeated;
/// and the document is at most [`MAX_POLICY_BYTES`] long without whitespace.
/// S3 has the last word: this catches what is certainly wrong, early and
/// precisely.
pub fn check_policy(text: &str) -> Vec<PolicyIssue> {
    let document = match parse_policy(text) {
        Ok(document) => document,
        Err(issue) => return vec![issue],
    };
    let mut issues = Vec::new();
    let size = compact(&document).len();
    if size > MAX_POLICY_BYTES {
        issues.push(PolicyIssue::new(
            "",
            format!(
                "the policy is {size} bytes without whitespace; S3 accepts at most \
                 {MAX_POLICY_BYTES} bytes (20 KiB)"
            ),
        ));
    }
    check_document(&document, &mut issues);
    issues
}

/// Validates a policy ([`check_policy`]) and returns it without whitespace,
/// the form [`S3::put_bucket_policy`] sends. Fails with `usage`, listing
/// every problem.
pub fn compact_policy(text: &str) -> Result<String> {
    let issues = check_policy(text);
    if !issues.is_empty() {
        let problems: Vec<String> = issues.iter().map(PolicyIssue::to_string).collect();
        return Err(Error::Usage(format!(
            "invalid bucket policy: {}",
            problems.join("; ")
        )));
    }
    // Valid, so it parses.
    parse_policy(text)
        .map(|document| compact(&document))
        .map_err(|issue| Error::Usage(issue.to_string()))
}

/// Pretty-prints a policy with two-space indentation. The keys keep the order
/// of the text. Only the JSON syntax has to be right, so a policy that is
/// still being written can be formatted; fails with `usage` otherwise.
pub fn format_policy(text: &str) -> Result<String> {
    let document = parse_policy(text).map_err(|issue| Error::Usage(issue.to_string()))?;
    serde_json::to_string_pretty(&document)
        .map_err(|error| Error::Internal(format!("formatting a policy: {error}")))
}

fn first<'a>(entries: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn check_document(document: &Json, issues: &mut Vec<PolicyIssue>) {
    let Json::Object(entries) = document else {
        issues.push(PolicyIssue::new(
            "",
            format!(
                "the policy must be a JSON object, not {}",
                document.describe()
            ),
        ));
        return;
    };
    check_duplicates(document, "", issues);
    for (key, _) in entries {
        if !POLICY_ELEMENTS.contains(&key.as_str()) {
            issues.push(PolicyIssue::new(
                "",
                format!("unknown element {key:?}; a policy has Version, Id and Statement"),
            ));
        }
    }
    match first(entries, "Version") {
        None => issues.push(PolicyIssue::new(
            "",
            "missing \"Version\"; add \"Version\": \"2012-10-17\"",
        )),
        Some(Json::String(version)) if POLICY_VERSIONS.contains(&version.as_str()) => {}
        Some(Json::String(version)) => issues.push(PolicyIssue::new(
            "Version",
            format!("must be \"2012-10-17\" or \"2008-10-17\" (found {version:?})"),
        )),
        Some(other) => issues.push(PolicyIssue::new(
            "Version",
            format!("must be a string, not {}", other.describe()),
        )),
    }
    if let Some(id) = first(entries, "Id")
        && !matches!(id, Json::String(_))
    {
        issues.push(PolicyIssue::new(
            "Id",
            format!("must be a string, not {}", id.describe()),
        ));
    }
    let mut sids: Vec<(String, String)> = Vec::new();
    match first(entries, "Statement") {
        None => issues.push(PolicyIssue::new("", "missing \"Statement\"")),
        Some(Json::Array(statements)) if statements.is_empty() => issues.push(PolicyIssue::new(
            "Statement",
            "must contain at least one statement",
        )),
        Some(Json::Array(statements)) => {
            for (index, statement) in statements.iter().enumerate() {
                check_statement(statement, &format!("Statement[{index}]"), &mut sids, issues);
            }
        }
        // IAM allows one statement without the list around it.
        Some(statement @ Json::Object(_)) => {
            check_statement(statement, "Statement", &mut sids, issues)
        }
        Some(other) => issues.push(PolicyIssue::new(
            "Statement",
            format!(
                "must be a list of statements (or one statement), not {}",
                other.describe()
            ),
        )),
    }
}

/// Reports an element that occurs twice in one object, anywhere in the
/// document: which of the two counts would be a guess.
fn check_duplicates(value: &Json, path: &str, issues: &mut Vec<PolicyIssue>) {
    match value {
        Json::Object(entries) => {
            let mut seen = BTreeSet::new();
            for (key, child) in entries {
                if !seen.insert(key.as_str()) {
                    issues.push(PolicyIssue::new(path, format!("duplicate element {key:?}")));
                }
                check_duplicates(child, &child_path(path, key), issues);
            }
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_duplicates(item, &format!("{path}[{index}]"), issues);
            }
        }
        _ => {}
    }
}

fn check_statement(
    statement: &Json,
    path: &str,
    sids: &mut Vec<(String, String)>,
    issues: &mut Vec<PolicyIssue>,
) {
    let Json::Object(entries) = statement else {
        issues.push(PolicyIssue::new(
            path,
            format!("must be an object, not {}", statement.describe()),
        ));
        return;
    };
    for (key, _) in entries {
        if !STATEMENT_ELEMENTS.contains(&key.as_str()) {
            issues.push(PolicyIssue::new(
                path,
                format!(
                    "unknown element {key:?}; a statement has {}",
                    STATEMENT_ELEMENTS.join(", ")
                ),
            ));
        }
    }
    match first(entries, "Sid") {
        None => {}
        Some(Json::String(sid)) => match sids.iter().find(|(used, _)| used == sid) {
            Some((_, earlier)) => issues.push(PolicyIssue::new(
                child_path(path, "Sid"),
                format!("{sid:?} is already used by {earlier}; Sid values must be unique"),
            )),
            None => sids.push((sid.clone(), path.to_owned())),
        },
        Some(other) => issues.push(PolicyIssue::new(
            child_path(path, "Sid"),
            format!("must be a string, not {}", other.describe()),
        )),
    }
    match first(entries, "Effect") {
        None => issues.push(PolicyIssue::new(
            path,
            "missing \"Effect\"; use \"Allow\" or \"Deny\"",
        )),
        Some(Json::String(effect)) if effect == "Allow" || effect == "Deny" => {}
        Some(Json::String(effect)) => issues.push(PolicyIssue::new(
            child_path(path, "Effect"),
            format!("must be \"Allow\" or \"Deny\" (found {effect:?})"),
        )),
        Some(other) => issues.push(PolicyIssue::new(
            child_path(path, "Effect"),
            format!("must be \"Allow\" or \"Deny\", not {}", other.describe()),
        )),
    }
    for (plain, negated) in [
        ("Principal", "NotPrincipal"),
        ("Action", "NotAction"),
        ("Resource", "NotResource"),
    ] {
        match (first(entries, plain), first(entries, negated)) {
            (None, None) => issues.push(PolicyIssue::new(
                path,
                format!("needs \"{plain}\" or \"{negated}\""),
            )),
            (Some(_), Some(_)) => issues.push(PolicyIssue::new(
                path,
                format!("cannot have both \"{plain}\" and \"{negated}\""),
            )),
            _ => {}
        }
    }
    for name in ["Principal", "NotPrincipal"] {
        if let Some(value) = first(entries, name) {
            check_principal(value, &child_path(path, name), issues);
        }
    }
    for name in ["Action", "NotAction", "Resource", "NotResource"] {
        if let Some(value) = first(entries, name) {
            check_string_list(value, &child_path(path, name), issues);
        }
    }
    if let Some(condition) = first(entries, "Condition") {
        check_condition(condition, &child_path(path, "Condition"), issues);
    }
}

/// A non-empty string, or a non-empty list of non-empty strings.
fn check_string_list(value: &Json, path: &str, issues: &mut Vec<PolicyIssue>) {
    match value {
        Json::String(text) if text.is_empty() => {
            issues.push(PolicyIssue::new(path, "must not be empty"));
        }
        Json::String(_) => {}
        Json::Array(items) if items.is_empty() => {
            issues.push(PolicyIssue::new(path, "must not be an empty list"));
        }
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    Json::String(text) if !text.is_empty() => {}
                    Json::String(_) => issues.push(PolicyIssue::new(
                        format!("{path}[{index}]"),
                        "must not be empty",
                    )),
                    other => issues.push(PolicyIssue::new(
                        format!("{path}[{index}]"),
                        format!("must be a string, not {}", other.describe()),
                    )),
                }
            }
        }
        other => issues.push(PolicyIssue::new(
            path,
            format!(
                "must be a string or a list of strings, not {}",
                other.describe()
            ),
        )),
    }
}

/// `"*"`, or an object that maps principal types (`AWS`, `Service`, ...) to
/// strings or lists of strings.
fn check_principal(value: &Json, path: &str, issues: &mut Vec<PolicyIssue>) {
    match value {
        Json::String(text) if text == "*" => {}
        Json::String(text) => issues.push(PolicyIssue::new(
            path,
            format!(
                "must be \"*\" or an object such as {{\"AWS\": \"arn:aws:iam::123456789012:root\"}} \
                 (found {text:?})"
            ),
        )),
        Json::Object(entries) if entries.is_empty() => {
            issues.push(PolicyIssue::new(path, "must not be empty"));
        }
        Json::Object(entries) => {
            for (kind, principals) in entries {
                check_string_list(principals, &child_path(path, kind), issues);
            }
        }
        other => issues.push(PolicyIssue::new(
            path,
            format!("must be \"*\" or an object, not {}", other.describe()),
        )),
    }
}

/// An object that maps condition operators to objects that map condition keys
/// to a value or a list of values.
fn check_condition(value: &Json, path: &str, issues: &mut Vec<PolicyIssue>) {
    let Json::Object(operators) = value else {
        issues.push(PolicyIssue::new(
            path,
            format!(
                "must be an object that maps condition operators to condition keys, not {}",
                value.describe()
            ),
        ));
        return;
    };
    for (operator, block) in operators {
        let operator_path = child_path(path, operator);
        let Json::Object(keys) = block else {
            issues.push(PolicyIssue::new(
                &operator_path,
                format!(
                    "must be an object that maps condition keys to values, not {}",
                    block.describe()
                ),
            ));
            continue;
        };
        for (key, condition_value) in keys {
            let key_path = child_path(&operator_path, key);
            match condition_value {
                Json::String(_) | Json::Number(_) | Json::Bool(_) => {}
                Json::Array(items) if items.is_empty() => {
                    issues.push(PolicyIssue::new(key_path, "must not be an empty list"));
                }
                Json::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        if !matches!(item, Json::String(_) | Json::Number(_) | Json::Bool(_)) {
                            issues.push(PolicyIssue::new(
                                format!("{key_path}[{index}]"),
                                format!(
                                    "must be a string, number or boolean, not {}",
                                    item.describe()
                                ),
                            ));
                        }
                    }
                }
                other => issues.push(PolicyIssue::new(
                    key_path,
                    format!(
                        "must be a string, number, boolean or a list of them, not {}",
                        other.describe()
                    ),
                )),
            }
        }
    }
}

// ---- website rules ----

impl WebsiteConfig {
    /// Checks S3's rules for a website configuration and reports every
    /// problem in one `usage` error.
    ///
    /// Hosting needs an index document without a slash; an error document is
    /// a non-empty key; there are at most [`MAX_ROUTING_RULES`] routing
    /// rules. A condition names a key prefix or an HTTP error code (4xx or
    /// 5xx); a redirect sets at least one of protocol, host, replacement key
    /// or prefix, and redirect code (3xx), and never both a replacement key
    /// and a replacement prefix. A redirect of all requests needs a host.
    pub fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();
        match self {
            WebsiteConfig::Hosting {
                index_document,
                error_document,
                routing_rules,
            } => {
                if index_document.is_empty() {
                    problems.push("the index document is empty".to_owned());
                } else if index_document.contains('/') {
                    problems.push(format!(
                        "the index document {index_document:?} must not contain a slash"
                    ));
                } else if index_document.len() > MAX_KEY_BYTES {
                    problems.push(format!(
                        "the index document is longer than {MAX_KEY_BYTES} bytes"
                    ));
                }
                if let Some(error_document) = error_document
                    && (error_document.is_empty() || error_document.len() > MAX_KEY_BYTES)
                {
                    problems.push(format!(
                        "the error document must be a key of 1 to {MAX_KEY_BYTES} bytes"
                    ));
                }
                if routing_rules.len() > MAX_ROUTING_RULES {
                    problems.push(format!(
                        "there are {} routing rules; S3 accepts at most {MAX_ROUTING_RULES}",
                        routing_rules.len()
                    ));
                }
                for (index, rule) in routing_rules.iter().enumerate() {
                    rule.check(index + 1, &mut problems);
                }
            }
            WebsiteConfig::Redirect { host_name, .. } => {
                if let Some(problem) = host_problem(host_name) {
                    problems.push(format!("the redirect host: {problem}"));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Usage(format!(
                "invalid website configuration: {}",
                problems.join("; ")
            )))
        }
    }
}

impl RoutingRule {
    fn check(&self, number: usize, problems: &mut Vec<String>) {
        let rule = format!("routing rule {number}");
        if let Some(condition) = &self.condition {
            let prefix = condition
                .key_prefix_equals
                .as_deref()
                .filter(|prefix| !prefix.is_empty());
            let code = condition.http_error_code_returned_equals.as_deref();
            if prefix.is_none() && code.is_none() {
                problems.push(format!(
                    "{rule}: the condition needs a key prefix or an HTTP error code"
                ));
            }
            if let Some(code) = code
                && !status_in(code, '4'..='5')
            {
                problems.push(format!(
                    "{rule}: the error code {code:?} must be a 4xx or 5xx HTTP status"
                ));
            }
        }
        let redirect = &self.redirect;
        if redirect.protocol.is_none()
            && redirect.host_name.is_none()
            && redirect.replace_key_prefix_with.is_none()
            && redirect.replace_key_with.is_none()
            && redirect.http_redirect_code.is_none()
        {
            problems.push(format!(
                "{rule}: the redirect needs a protocol, host, replacement key or prefix, or redirect code"
            ));
        }
        if redirect.replace_key_prefix_with.is_some() && redirect.replace_key_with.is_some() {
            problems.push(format!(
                "{rule}: a redirect replaces the key prefix or the whole key, not both"
            ));
        }
        if let Some(host) = &redirect.host_name
            && let Some(problem) = host_problem(host)
        {
            problems.push(format!("{rule}: the redirect host: {problem}"));
        }
        if let Some(code) = &redirect.http_redirect_code
            && !status_in(code, '3'..='3')
        {
            problems.push(format!(
                "{rule}: the redirect code {code:?} must be a 3xx HTTP status"
            ));
        }
    }
}

/// Three digits, the first within `first`.
fn status_in(code: &str, first: std::ops::RangeInclusive<char>) -> bool {
    let mut chars = code.chars();
    code.len() == 3
        && chars.next().is_some_and(|c| first.contains(&c))
        && chars.all(|c| c.is_ascii_digit())
}

/// Why `host` cannot be a redirect host: empty, or with a scheme, a path or
/// whitespace.
fn host_problem(host: &str) -> Option<String> {
    if host.is_empty() {
        Some("is empty".to_owned())
    } else if host.contains("://") || host.contains('/') {
        Some(format!(
            "{host:?} must be a host name without a scheme or path"
        ))
    } else if host.chars().any(char::is_whitespace) || host.len() > 255 {
        Some(format!("{host:?} is not a host name"))
    } else {
        None
    }
}

// ---- S3 operations ----

pub(crate) fn context(action: &str, bucket: &str) -> String {
    format!("{action} of bucket '{bucket}'")
}

pub(crate) fn build_error(error: aws_sdk_s3::error::BuildError) -> Error {
    Error::Internal(format!("failed to build an S3 request: {error}"))
}

/// The value of a read, or `None` when the server answered with the code
/// that means "nothing is configured".
pub(crate) fn read_optional<T, E, R>(
    reply: std::result::Result<T, SdkError<E, R>>,
    absent: &str,
    context: String,
) -> Result<Option<T>>
where
    E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
    R: fmt::Debug + Send + Sync + 'static,
{
    match reply {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let error = Error::from(error);
            if error.service().and_then(ServiceError::code) == Some(absent) {
                Ok(None)
            } else {
                Err(error.with_context(context))
            }
        }
    }
}

/// Success, also when the server says there was nothing to delete.
pub(crate) fn delete_optional<T, E, R>(
    reply: std::result::Result<T, SdkError<E, R>>,
    absent: &str,
    context: String,
) -> Result<()>
where
    E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
    R: fmt::Debug + Send + Sync + 'static,
{
    read_optional(reply, absent, context).map(|_| ())
}

/// A policy as the editor shows it: pretty-printed, or untouched when the
/// server sent something that is not JSON.
fn display_policy(raw: &str) -> String {
    format_policy(raw).unwrap_or_else(|_| raw.to_owned())
}

impl S3 {
    /// Reads every configuration section of `bucket` at once; each one is
    /// reported on its own ([`Section`]), so a section that is refused or not
    /// implemented never hides the others. Fails as a whole only when the
    /// bucket cannot be reached at all (invalid name, no answer from the
    /// server).
    pub async fn get_bucket_config(&self, bucket: &str) -> Result<BucketConfig> {
        validate_bucket_name(bucket)?;
        // Resolves the bucket's region once, so that a network failure is
        // reported once and not by each of the six requests.
        self.client_for(bucket).await?;
        let (policy, public_access_block, versioning, logging, website, acceleration) = tokio::join!(
            self.get_bucket_policy(bucket),
            self.get_public_access_block(bucket),
            self.get_bucket_versioning(bucket),
            self.get_bucket_logging(bucket),
            self.get_bucket_website(bucket),
            self.acceleration_section(bucket),
        );
        Ok(BucketConfig {
            policy: Section::from_result(policy, "bucket policies"),
            public_access_block: Section::from_result(
                public_access_block,
                "the public access block",
            ),
            versioning: Section::from_result(versioning, "versioning"),
            logging: Section::from_result(logging, "server access logging"),
            website: Section::from_result(website, "static website hosting"),
            acceleration,
        })
    }

    /// The bucket policy, pretty-printed; `None` when the bucket has none.
    pub async fn get_bucket_policy(&self, bucket: &str) -> Result<Option<String>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_policy()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchBucketPolicy",
            context("reading the policy", bucket),
        )?;
        Ok(out.and_then(|out| out.policy().map(display_policy)))
    }

    /// Replaces the bucket policy. The document is validated first
    /// ([`check_policy`]; a `usage` error lists every problem) and sent
    /// without whitespace.
    pub async fn put_bucket_policy(&self, bucket: &str, policy: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        let document = compact_policy(policy)?;
        self.client_for(bucket)
            .await?
            .put_bucket_policy()
            .bucket(bucket)
            .policy(document)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the policy", bucket))
            })?;
        Ok(())
    }

    /// Deletes the bucket policy; succeeds when there is none.
    pub async fn delete_bucket_policy(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_policy()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchBucketPolicy",
            context("deleting the policy", bucket),
        )
    }

    /// The public access block; `None` when the bucket has none (S3 then
    /// blocks nothing).
    pub async fn get_public_access_block(&self, bucket: &str) -> Result<Option<PublicAccessBlock>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_public_access_block()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchPublicAccessBlockConfiguration",
            context("reading the public access block", bucket),
        )?;
        Ok(out.map(|out| {
            let config = out.public_access_block_configuration();
            let flag = |value: Option<bool>| value.unwrap_or(false);
            PublicAccessBlock {
                block_public_acls: flag(config.and_then(|c| c.block_public_acls())),
                ignore_public_acls: flag(config.and_then(|c| c.ignore_public_acls())),
                block_public_policy: flag(config.and_then(|c| c.block_public_policy())),
                restrict_public_buckets: flag(config.and_then(|c| c.restrict_public_buckets())),
            }
        }))
    }

    /// Replaces the public access block; all four toggles are sent.
    pub async fn put_public_access_block(
        &self,
        bucket: &str,
        block: &PublicAccessBlock,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        let config = PublicAccessBlockConfiguration::builder()
            .block_public_acls(block.block_public_acls)
            .ignore_public_acls(block.ignore_public_acls)
            .block_public_policy(block.block_public_policy)
            .restrict_public_buckets(block.restrict_public_buckets)
            .build();
        self.client_for(bucket)
            .await?
            .put_public_access_block()
            .bucket(bucket)
            .public_access_block_configuration(config)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the public access block", bucket))
            })?;
        Ok(())
    }

    /// Removes the public access block; succeeds when there is none.
    pub async fn delete_public_access_block(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_public_access_block()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchPublicAccessBlockConfiguration",
            context("removing the public access block", bucket),
        )
    }

    /// The versioning state of the bucket and whether MFA delete is on.
    pub async fn get_bucket_versioning(&self, bucket: &str) -> Result<BucketVersioning> {
        let out = self
            .client_for(bucket)
            .await?
            .get_bucket_versioning()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("reading the versioning state", bucket))
            })?;
        let status = match out.status() {
            Some(BucketVersioningStatus::Enabled) => VersioningStatus::Enabled,
            Some(BucketVersioningStatus::Suspended) => VersioningStatus::Suspended,
            _ => VersioningStatus::Unversioned,
        };
        Ok(BucketVersioning {
            status,
            mfa_delete: matches!(out.mfa_delete(), Some(MfaDeleteStatus::Enabled)),
        })
    }

    /// Enables or suspends versioning. [`VersioningStatus::Unversioned`] is
    /// refused (`usage`): S3 cannot switch versioning off again. MFA delete is
    /// not sent, so it stays as it is.
    pub async fn put_bucket_versioning(
        &self,
        bucket: &str,
        status: VersioningStatus,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        let status = match status {
            VersioningStatus::Enabled => BucketVersioningStatus::Enabled,
            VersioningStatus::Suspended => BucketVersioningStatus::Suspended,
            VersioningStatus::Unversioned => {
                return Err(Error::Usage(
                    "versioning cannot be switched off once it was enabled; choose Enabled or Suspended"
                        .into(),
                ));
            }
        };
        self.client_for(bucket)
            .await?
            .put_bucket_versioning()
            .bucket(bucket)
            .versioning_configuration(VersioningConfiguration::builder().status(status).build())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the versioning state", bucket))
            })?;
        Ok(())
    }

    /// Where the bucket delivers its server access logs; `None` while
    /// logging is off.
    pub async fn get_bucket_logging(&self, bucket: &str) -> Result<Option<LoggingTarget>> {
        let out = self
            .client_for(bucket)
            .await?
            .get_bucket_logging()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("reading the access logging", bucket))
            })?;
        Ok(out.logging_enabled().map(|enabled| LoggingTarget {
            target_bucket: enabled.target_bucket().to_owned(),
            target_prefix: enabled.target_prefix().to_owned(),
        }))
    }

    /// Turns server access logging on, or changes its target. The target
    /// grants and the log object key format the bucket already has are kept,
    /// so the current configuration is read first.
    ///
    /// The target bucket must exist, be in the same region and allow log
    /// delivery; S3 reports it when it does not.
    pub async fn put_bucket_logging(&self, bucket: &str, target: &LoggingTarget) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_bucket_name(&target.target_bucket)
            .map_err(|error| error.with_context("the log target bucket"))?;
        if target.target_prefix.len() > MAX_KEY_BYTES {
            return Err(Error::Usage(format!(
                "the log prefix is {} bytes long; at most {MAX_KEY_BYTES} bytes are allowed",
                target.target_prefix.len()
            )));
        }
        let client = self.client_for(bucket).await?;
        let current = client
            .get_bucket_logging()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("reading the access logging", bucket))
            })?;
        let (grants, key_format): (_, Option<TargetObjectKeyFormat>) =
            match current.logging_enabled() {
                Some(enabled) => (
                    Some(enabled.target_grants().to_vec()).filter(|grants| !grants.is_empty()),
                    enabled.target_object_key_format().cloned(),
                ),
                None => (None, None),
            };
        let enabled = LoggingEnabled::builder()
            .target_bucket(&target.target_bucket)
            .target_prefix(&target.target_prefix)
            .set_target_grants(grants)
            .set_target_object_key_format(key_format)
            .build()
            .map_err(build_error)?;
        client
            .put_bucket_logging()
            .bucket(bucket)
            .bucket_logging_status(
                BucketLoggingStatus::builder()
                    .logging_enabled(enabled)
                    .build(),
            )
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the access logging", bucket))
            })?;
        Ok(())
    }

    /// Turns server access logging off.
    pub async fn disable_bucket_logging(&self, bucket: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        self.client_for(bucket)
            .await?
            .put_bucket_logging()
            .bucket(bucket)
            .bucket_logging_status(BucketLoggingStatus::builder().build())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("turning off the access logging", bucket))
            })?;
        Ok(())
    }

    /// The website configuration; `None` when the bucket is not a website.
    pub async fn get_bucket_website(&self, bucket: &str) -> Result<Option<WebsiteConfig>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_website()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchWebsiteConfiguration",
            context("reading the website configuration", bucket),
        )?;
        Ok(out.and_then(|out| {
            if let Some(redirect) = out.redirect_all_requests_to() {
                return Some(WebsiteConfig::Redirect {
                    host_name: redirect.host_name().to_owned(),
                    protocol: redirect.protocol().and_then(protocol_from_sdk),
                });
            }
            // A server that answers without either part has no website.
            let index = out.index_document()?;
            Some(WebsiteConfig::Hosting {
                index_document: index.suffix().to_owned(),
                error_document: out.error_document().map(|doc| doc.key().to_owned()),
                routing_rules: out.routing_rules().iter().map(rule_from_sdk).collect(),
            })
        }))
    }

    /// Replaces the website configuration, after checking it
    /// ([`WebsiteConfig::validate`]).
    pub async fn put_bucket_website(&self, bucket: &str, website: &WebsiteConfig) -> Result<()> {
        validate_bucket_name(bucket)?;
        website.validate()?;
        let config = website_to_sdk(website)?;
        self.client_for(bucket)
            .await?
            .put_bucket_website()
            .bucket(bucket)
            .website_configuration(config)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the website configuration", bucket))
            })?;
        Ok(())
    }

    /// Turns website hosting off; succeeds when it was off.
    pub async fn delete_bucket_website(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_website()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchWebsiteConfiguration",
            context("removing the website configuration", bucket),
        )
    }

    /// True when the account can use transfer acceleration: AWS accounts
    /// only, not accounts with a custom endpoint.
    pub fn supports_acceleration(&self) -> bool {
        self.bucket_kind() != BucketKind::External
    }

    /// The transfer acceleration state. Accounts with a custom endpoint get a
    /// `usage` error saying it is not supported, without a request.
    pub async fn get_bucket_acceleration(&self, bucket: &str) -> Result<AccelerationStatus> {
        validate_bucket_name(bucket)?;
        self.require_acceleration()?;
        let out = self
            .client_for(bucket)
            .await?
            .get_bucket_accelerate_configuration()
            .bucket(bucket)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("reading transfer acceleration", bucket))
            })?;
        Ok(match out.status() {
            Some(BucketAccelerateStatus::Enabled) => AccelerationStatus::Enabled,
            Some(BucketAccelerateStatus::Suspended) => AccelerationStatus::Suspended,
            _ => AccelerationStatus::Unset,
        })
    }

    /// Enables or suspends transfer acceleration. [`AccelerationStatus::Unset`]
    /// is refused (`usage`), as is a bucket whose name has dots (S3 needs a
    /// DNS-compatible name without periods) and an account with a custom
    /// endpoint.
    pub async fn put_bucket_acceleration(
        &self,
        bucket: &str,
        status: AccelerationStatus,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        self.require_acceleration()?;
        let status = match status {
            AccelerationStatus::Enabled => {
                if bucket.contains('.') {
                    return Err(Error::Usage(format!(
                        "transfer acceleration needs a bucket name without dots; '{bucket}' has some"
                    )));
                }
                BucketAccelerateStatus::Enabled
            }
            AccelerationStatus::Suspended => BucketAccelerateStatus::Suspended,
            AccelerationStatus::Unset => {
                return Err(Error::Usage(
                    "transfer acceleration cannot be unset once it was configured; choose Enabled or Suspended"
                        .into(),
                ));
            }
        };
        self.client_for(bucket)
            .await?
            .put_bucket_accelerate_configuration()
            .bucket(bucket)
            .accelerate_configuration(AccelerateConfiguration::builder().status(status).build())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving transfer acceleration", bucket))
            })?;
        Ok(())
    }

    fn require_acceleration(&self) -> Result<()> {
        if self.supports_acceleration() {
            Ok(())
        } else {
            Err(Error::Usage(ACCELERATION_UNSUPPORTED.into()))
        }
    }

    async fn acceleration_section(&self, bucket: &str) -> Section<AccelerationStatus> {
        if !self.supports_acceleration() {
            return Section::Unsupported {
                message: ACCELERATION_UNSUPPORTED.to_owned(),
            };
        }
        Section::from_result(
            self.get_bucket_acceleration(bucket).await,
            "transfer acceleration",
        )
    }

    /// Checks that `bucket` exists, without needing to be allowed to use it:
    /// `HeadBucket` answering 200, 301, 307 or 403 (or 501, from a server
    /// without the call) counts, since the bucket is there. A missing bucket
    /// is `not_found`; every other failure is returned as it is.
    pub async fn verify_bucket_exists(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .head_bucket()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        match reply {
            Ok(_) => Ok(()),
            Err(error) => {
                let error = Error::from(error);
                let exists = error.kind() == ErrorKind::Auth
                    || error.service().is_some_and(|service| {
                        matches!(service.status(), Some(301 | 307)) || service.is_not_implemented()
                    });
                if exists {
                    Ok(())
                } else if error.kind() == ErrorKind::NotFound {
                    Err(Error::NotFound(format!("bucket '{bucket}' does not exist")))
                } else {
                    Err(error.with_context(format!("checking bucket '{bucket}'")))
                }
            }
        }
    }

    /// Lists the account's buckets like [`S3::list_buckets`] and adds the
    /// `pinned` external buckets that are not in the list, as
    /// [`BucketKind::External`] entries without region, date or owner. When
    /// the server refuses to list buckets (`AccessDenied`) and buckets are
    /// pinned, the pinned buckets are all there is to show, so they are
    /// returned alone; without any pinned bucket the refusal is the error.
    pub async fn list_buckets_with_pinned(&self, pinned: &[String]) -> Result<Vec<Bucket>> {
        match self.list_buckets().await {
            Ok(listed) => Ok(merge_pinned(listed, pinned)),
            Err(error) if !pinned.is_empty() && is_listing_denied(&error) => {
                Ok(merge_pinned(Vec::new(), pinned))
            }
            Err(error) => Err(error),
        }
    }
}

/// True when the server refused to list the buckets (`AccessDenied`).
fn is_listing_denied(error: &Error) -> bool {
    error.service().and_then(ServiceError::code) == Some("AccessDenied")
}

// ---- SDK conversions ----

fn protocol_to_sdk(protocol: RedirectProtocol) -> Protocol {
    match protocol {
        RedirectProtocol::Http => Protocol::Http,
        RedirectProtocol::Https => Protocol::Https,
    }
}

fn protocol_from_sdk(protocol: &Protocol) -> Option<RedirectProtocol> {
    match protocol {
        Protocol::Http => Some(RedirectProtocol::Http),
        Protocol::Https => Some(RedirectProtocol::Https),
        _ => None,
    }
}

fn rule_from_sdk(rule: &SdkRoutingRule) -> RoutingRule {
    let redirect = rule.redirect();
    RoutingRule {
        condition: rule.condition().map(|condition| RoutingCondition {
            key_prefix_equals: condition.key_prefix_equals().map(str::to_owned),
            http_error_code_returned_equals: condition
                .http_error_code_returned_equals()
                .map(str::to_owned),
        }),
        redirect: RoutingRedirect {
            protocol: redirect
                .and_then(|redirect| redirect.protocol())
                .and_then(protocol_from_sdk),
            host_name: redirect
                .and_then(|redirect| redirect.host_name())
                .map(str::to_owned),
            replace_key_prefix_with: redirect
                .and_then(|redirect| redirect.replace_key_prefix_with())
                .map(str::to_owned),
            replace_key_with: redirect
                .and_then(|redirect| redirect.replace_key_with())
                .map(str::to_owned),
            http_redirect_code: redirect
                .and_then(|redirect| redirect.http_redirect_code())
                .map(str::to_owned),
        },
    }
}

fn rule_to_sdk(rule: &RoutingRule) -> SdkRoutingRule {
    let condition = rule.condition.as_ref().map(|condition| {
        SdkCondition::builder()
            .set_key_prefix_equals(condition.key_prefix_equals.clone())
            .set_http_error_code_returned_equals(condition.http_error_code_returned_equals.clone())
            .build()
    });
    let redirect = &rule.redirect;
    SdkRoutingRule::builder()
        .set_condition(condition)
        .redirect(
            SdkRedirect::builder()
                .set_protocol(redirect.protocol.map(protocol_to_sdk))
                .set_host_name(redirect.host_name.clone())
                .set_replace_key_prefix_with(redirect.replace_key_prefix_with.clone())
                .set_replace_key_with(redirect.replace_key_with.clone())
                .set_http_redirect_code(redirect.http_redirect_code.clone())
                .build(),
        )
        .build()
}

fn website_to_sdk(website: &WebsiteConfig) -> Result<WebsiteConfiguration> {
    match website {
        WebsiteConfig::Hosting {
            index_document,
            error_document,
            routing_rules,
        } => {
            let mut config = WebsiteConfiguration::builder().index_document(
                IndexDocument::builder()
                    .suffix(index_document)
                    .build()
                    .map_err(build_error)?,
            );
            if let Some(error_document) = error_document {
                config = config.error_document(
                    ErrorDocument::builder()
                        .key(error_document)
                        .build()
                        .map_err(build_error)?,
                );
            }
            for rule in routing_rules {
                config = config.routing_rules(rule_to_sdk(rule));
            }
            Ok(config.build())
        }
        WebsiteConfig::Redirect {
            host_name,
            protocol,
        } => Ok(WebsiteConfiguration::builder()
            .redirect_all_requests_to(
                RedirectAllRequestsTo::builder()
                    .host_name(host_name)
                    .set_protocol(protocol.map(protocol_to_sdk))
                    .build()
                    .map_err(build_error)?,
            )
            .build()),
    }
}

// ---- external buckets ----

/// Adds the pinned buckets that `listed` does not contain, as
/// [`BucketKind::External`] entries without region, date or owner, and
/// sorts the result by name. A bucket that is listed keeps its listed entry.
pub fn merge_pinned(listed: Vec<Bucket>, pinned: &[String]) -> Vec<Bucket> {
    let mut buckets = listed;
    let known: BTreeSet<String> = buckets.iter().map(|bucket| bucket.name.clone()).collect();
    buckets.extend(
        pinned
            .iter()
            .filter(|name| !known.contains(*name))
            .map(|name| Bucket {
                name: name.clone(),
                region: None,
                creation_date: None,
                owner: None,
                kind: BucketKind::External,
            }),
    );
    buckets.sort_by(|a, b| a.name.cmp(&b.name));
    buckets.dedup_by(|a, b| a.name == b.name);
    buckets
}

/// The names pinned to `account_id` in `config`: sorted, without duplicates,
/// and without entries that are not bucket names (an edited file).
pub fn pinned_buckets(config: &AppConfig, account_id: &str) -> Vec<String> {
    let names: BTreeSet<&String> = config
        .pinned_buckets
        .get(account_id)
        .map(|names| {
            names
                .iter()
                .filter(|name| validate_bucket_name(name).is_ok())
                .collect()
        })
        .unwrap_or_default();
    names.into_iter().cloned().collect()
}

/// The names pinned to `account_id`, read from the configuration file
/// ([`pinned_buckets`]). Fails with `not_found` for an unknown account.
/// Blocking.
pub fn load_pinned_buckets(store: &ConfigStore, account_id: &str) -> Result<Vec<String>> {
    let config = store.load()?;
    require_account(&config, account_id)?;
    Ok(pinned_buckets(&config, account_id))
}

/// Pins `name` to the account. Pinning a bucket that is pinned already
/// changes nothing. Fails with `usage` for an invalid bucket name and
/// `not_found` for an unknown account. Blocking.
pub fn pin_bucket(store: &ConfigStore, account_id: &str, name: &str) -> Result<()> {
    validate_bucket_name(name)?;
    store.update(|config| {
        require_account(config, account_id)?;
        let pinned = config
            .pinned_buckets
            .entry(account_id.to_owned())
            .or_default();
        if !pinned.iter().any(|existing| existing == name) {
            pinned.push(name.to_owned());
            pinned.sort();
        }
        Ok(())
    })
}

/// Removes the pin of `name`. Fails with `not_found` for an unknown account
/// and for a bucket that is not pinned to it. Blocking.
pub fn unpin_bucket(store: &ConfigStore, account_id: &str, name: &str) -> Result<()> {
    store.update(|config| {
        require_account(config, account_id)?;
        let pinned = config.pinned_buckets.get_mut(account_id);
        let before = pinned.as_ref().map_or(0, |names| names.len());
        if let Some(names) = pinned {
            names.retain(|existing| existing != name);
        }
        let after = config
            .pinned_buckets
            .get(account_id)
            .map_or(0, |names| names.len());
        if after == before {
            return Err(Error::NotFound(format!(
                "bucket '{name}' is not pinned to this account"
            )));
        }
        if after == 0 {
            config.pinned_buckets.remove(account_id);
        }
        Ok(())
    })
}

fn require_account(config: &AppConfig, account_id: &str) -> Result<()> {
    if config
        .accounts
        .iter()
        .any(|account| account.id == account_id)
    {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "account {account_id} does not exist"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::config::Dirs;
    use crate::model::Account;

    const PUBLIC_READ: &str = r#"{
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "PublicRead",
            "Effect": "Allow",
            "Principal": "*",
            "Action": ["s3:GetObject"],
            "Resource": "arn:aws:s3:::example-bucket/*"
        }]
    }"#;

    fn messages(text: &str) -> Vec<String> {
        check_policy(text)
            .iter()
            .map(PolicyIssue::to_string)
            .collect()
    }

    /// A policy with `change` applied to its only statement.
    fn with_statement(change: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut policy: serde_json::Value = serde_json::from_str(PUBLIC_READ).unwrap();
        change(&mut policy["Statement"][0]);
        policy.to_string()
    }

    // ---- policy validation ----

    #[test]
    fn a_valid_policy_has_no_issues() {
        assert_eq!(messages(PUBLIC_READ), Vec::<String>::new());
        // The elements that S3 documents all pass.
        let full = json!({
            "Version": "2008-10-17",
            "Id": "full",
            "Statement": [
                {
                    "Sid": "A",
                    "Effect": "Deny",
                    "NotPrincipal": {"AWS": ["arn:aws:iam::123456789012:root"]},
                    "NotAction": "s3:GetObject",
                    "NotResource": ["arn:aws:s3:::b", "arn:aws:s3:::b/*"],
                    "Condition": {
                        "Bool": {"aws:SecureTransport": "false"},
                        "IpAddress": {"aws:SourceIp": ["203.0.113.0/24", "198.51.100.7/32"]},
                        "NumericLessThan": {"s3:max-keys": 10}
                    }
                },
                {
                    "Effect": "Allow",
                    "Principal": {"Service": "logging.s3.amazonaws.com"},
                    "Action": "s3:PutObject",
                    "Resource": "arn:aws:s3:::b/*"
                }
            ]
        });
        assert_eq!(messages(&full.to_string()), Vec::<String>::new());
    }

    #[test]
    fn one_statement_may_stand_without_a_list() {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": {
                "Effect": "Allow", "Principal": "*",
                "Action": "s3:GetObject", "Resource": "arn:aws:s3:::b/*"
            }
        });
        assert_eq!(messages(&policy.to_string()), Vec::<String>::new());
        let broken = json!({"Version": "2012-10-17", "Statement": {"Effect": "Maybe"}});
        assert!(
            messages(&broken.to_string())
                .iter()
                .any(|m| m.starts_with("Statement.Effect: must be"))
        );
    }

    #[test]
    fn json_syntax_errors_carry_the_position() {
        let issues = check_policy("{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n}");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "");
        assert!(
            issues[0].message.starts_with("not valid JSON: ")
                && issues[0].message.contains("line 4 column 1"),
            "{}",
            issues[0]
        );
        assert_eq!(messages("   \n"), ["the policy is empty"]);
        assert_eq!(messages(""), ["the policy is empty"]);
        assert!(messages("{} extra")[0].contains("trailing characters"));
    }

    #[test]
    fn the_policy_must_be_an_object() {
        assert_eq!(
            messages("[]"),
            ["the policy must be a JSON object, not an array"]
        );
        assert_eq!(
            messages("\"text\""),
            ["the policy must be a JSON object, not a string"]
        );
        assert_eq!(
            messages("42"),
            ["the policy must be a JSON object, not a number"]
        );
        assert_eq!(
            messages("null"),
            ["the policy must be a JSON object, not null"]
        );
    }

    #[test]
    fn version_and_statement_are_required() {
        let found = messages("{}");
        assert!(
            found.contains(&"missing \"Version\"; add \"Version\": \"2012-10-17\"".to_owned()),
            "{found:?}"
        );
        assert!(
            found.contains(&"missing \"Statement\"".to_owned()),
            "{found:?}"
        );
        let found = messages(r#"{"Version": "2020-01-01", "Statement": []}"#);
        assert_eq!(
            found,
            [
                "Version: must be \"2012-10-17\" or \"2008-10-17\" (found \"2020-01-01\")",
                "Statement: must contain at least one statement"
            ]
        );
        assert_eq!(
            messages(r#"{"Version": 2012, "Statement": "x"}"#),
            [
                "Version: must be a string, not a number",
                "Statement: must be a list of statements (or one statement), not a string"
            ]
        );
        assert_eq!(
            messages(r#"{"Version": "2012-10-17", "Id": 7, "Statement": [5]}"#),
            [
                "Id: must be a string, not a number",
                "Statement[0]: must be an object, not a number"
            ]
        );
    }

    #[test]
    fn unknown_elements_are_reported_with_the_valid_ones() {
        let found = messages(r#"{"Version": "2012-10-17", "Statment": [], "Statement": []}"#);
        assert!(
            found.contains(
                &"unknown element \"Statment\"; a policy has Version, Id and Statement".to_owned()
            ),
            "{found:?}"
        );
        let policy = with_statement(|statement| {
            statement["Efect"] = json!("Allow");
        });
        let found = messages(&policy);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with(
                "Statement[0]: unknown element \"Efect\"; a statement has Sid, Effect, Principal"
            ),
            "{found:?}"
        );
    }

    #[test]
    fn effect_principal_action_and_resource_are_checked_per_statement() {
        let policy = with_statement(|statement| {
            statement["Effect"] = json!("allow");
        });
        assert_eq!(
            messages(&policy),
            ["Statement[0].Effect: must be \"Allow\" or \"Deny\" (found \"allow\")"]
        );
        let policy = with_statement(|statement| {
            statement["Effect"] = json!(true);
        });
        assert_eq!(
            messages(&policy),
            ["Statement[0].Effect: must be \"Allow\" or \"Deny\", not a boolean"]
        );
        let policy = with_statement(|statement| {
            let object = statement.as_object_mut().unwrap();
            object.remove("Effect");
            object.remove("Principal");
            object.remove("Action");
            object.remove("Resource");
        });
        assert_eq!(
            messages(&policy),
            [
                "Statement[0]: missing \"Effect\"; use \"Allow\" or \"Deny\"",
                "Statement[0]: needs \"Principal\" or \"NotPrincipal\"",
                "Statement[0]: needs \"Action\" or \"NotAction\"",
                "Statement[0]: needs \"Resource\" or \"NotResource\""
            ]
        );
        let policy = with_statement(|statement| {
            statement["NotPrincipal"] = json!("*");
            statement["NotAction"] = json!("s3:*");
            statement["NotResource"] = json!("*");
        });
        assert_eq!(
            messages(&policy),
            [
                "Statement[0]: cannot have both \"Principal\" and \"NotPrincipal\"",
                "Statement[0]: cannot have both \"Action\" and \"NotAction\"",
                "Statement[0]: cannot have both \"Resource\" and \"NotResource\""
            ]
        );
    }

    #[test]
    fn values_have_the_documented_shapes() {
        let policy = with_statement(|statement| {
            statement["Principal"] = json!("someone");
            statement["Action"] = json!([]);
            statement["Resource"] = json!(["ok", "", 7]);
        });
        let found = messages(&policy);
        assert!(
            found[0].starts_with("Statement[0].Principal: must be \"*\" or an object such as"),
            "{found:?}"
        );
        assert_eq!(
            &found[1..],
            [
                "Statement[0].Action: must not be an empty list",
                "Statement[0].Resource[1]: must not be empty",
                "Statement[0].Resource[2]: must be a string, not a number"
            ]
        );
        let policy = with_statement(|statement| {
            statement["Principal"] = json!({});
            statement["Action"] = json!(5);
            statement["Resource"] = json!("");
        });
        assert_eq!(
            messages(&policy),
            [
                "Statement[0].Principal: must not be empty",
                "Statement[0].Action: must be a string or a list of strings, not a number",
                "Statement[0].Resource: must not be empty"
            ]
        );
        let policy = with_statement(|statement| {
            statement["Principal"] = json!({"AWS": [1]});
        });
        assert_eq!(
            messages(&policy),
            ["Statement[0].Principal.AWS[0]: must be a string, not a number"]
        );
        let policy = with_statement(|statement| {
            statement["Principal"] = json!(["*"]);
        });
        assert_eq!(
            messages(&policy),
            ["Statement[0].Principal: must be \"*\" or an object, not an array"]
        );
    }

    #[test]
    fn conditions_map_operators_to_keys_to_values() {
        let policy = with_statement(|statement| {
            statement["Condition"] = json!("x");
        });
        assert_eq!(
            messages(&policy),
            [
                "Statement[0].Condition: must be an object that maps condition operators to condition keys, not a string"
            ]
        );
        let policy = with_statement(|statement| {
            statement["Condition"] = json!({
                "Bool": "false",
                "StringEquals": {"s3:prefix": [], "s3:x-amz-acl": {"a": 1}, "k": [["nested"]]}
            });
        });
        assert_eq!(
            messages(&policy),
            [
                "Statement[0].Condition.Bool: must be an object that maps condition keys to values, not a string",
                "Statement[0].Condition.StringEquals.s3:prefix: must not be an empty list",
                "Statement[0].Condition.StringEquals.s3:x-amz-acl: must be a string, number, boolean or a list of them, not an object",
                "Statement[0].Condition.StringEquals.k[0]: must be a string, number or boolean, not an array"
            ]
        );
    }

    #[test]
    fn sids_must_be_unique_and_strings() {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [
                {"Sid": "One", "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": "*"},
                {"Sid": "Two", "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": "*"},
                {"Sid": "One", "Effect": "Deny", "Principal": "*", "Action": "s3:GetObject", "Resource": "*"},
                {"Sid": 4, "Effect": "Deny", "Principal": "*", "Action": "s3:GetObject", "Resource": "*"}
            ]
        });
        assert_eq!(
            messages(&policy.to_string()),
            [
                "Statement[2].Sid: \"One\" is already used by Statement[0]; Sid values must be unique",
                "Statement[3].Sid: must be a string, not a number"
            ]
        );
    }

    #[test]
    fn repeated_elements_are_reported() {
        let found = messages(
            r#"{"Version": "2012-10-17", "Version": "2008-10-17", "Statement": [
                {"Effect": "Allow", "Effect": "Deny", "Principal": "*", "Action": "a", "Resource": "b",
                 "Condition": {"Bool": {"k": "1", "k": "2"}}}]}"#,
        );
        assert_eq!(
            found,
            [
                "duplicate element \"Version\"",
                "Statement[0]: duplicate element \"Effect\"",
                "Statement[0].Condition.Bool: duplicate element \"k\""
            ]
        );
    }

    #[test]
    fn the_size_limit_is_measured_without_whitespace() {
        let resource = |n: usize| format!("arn:aws:s3:::b/{}", "x".repeat(n));
        let at_most = |bytes: usize| {
            // The document around the resource is 111 bytes long.
            let probe = json!({"Version": "2012-10-17", "Statement": [{
                "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": resource(0)}]})
            .to_string();
            let padding = bytes - probe.len();
            json!({"Version": "2012-10-17", "Statement": [{
                "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject", "Resource": resource(padding)}]})
            .to_string()
        };
        let fits = at_most(MAX_POLICY_BYTES);
        assert_eq!(fits.len(), MAX_POLICY_BYTES);
        assert_eq!(messages(&fits), Vec::<String>::new());
        let too_big = at_most(MAX_POLICY_BYTES + 1);
        assert_eq!(
            messages(&too_big),
            [
                "the policy is 20481 bytes without whitespace; S3 accepts at most 20480 bytes (20 KiB)"
            ]
        );
        // Indentation does not count: the same document pretty-printed is
        // over the limit as text but valid.
        let pretty = format_policy(&fits).unwrap();
        assert!(pretty.len() > MAX_POLICY_BYTES);
        assert_eq!(messages(&pretty), Vec::<String>::new());
        assert_eq!(compact_policy(&pretty).unwrap(), fits);
    }

    #[test]
    fn compact_policy_reports_every_problem_in_one_usage_error() {
        let error = compact_policy(r#"{"Statement": [{"Effect": "x"}]}"#).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        let text = error.to_string();
        assert!(text.starts_with("invalid bucket policy: "), "{text}");
        assert!(text.contains("missing \"Version\""), "{text}");
        assert!(text.contains("Statement[0].Effect: must be"), "{text}");
        assert!(
            text.contains("needs \"Principal\" or \"NotPrincipal\""),
            "{text}"
        );
        assert!(text.contains("; "), "{text}");
        assert_eq!(
            compact_policy(PUBLIC_READ).unwrap(),
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"PublicRead","Effect":"Allow","Principal":"*","Action":["s3:GetObject"],"Resource":"arn:aws:s3:::example-bucket/*"}]}"#
        );
    }

    // ---- policy formatting ----

    #[test]
    fn formatting_keeps_the_order_of_the_keys() {
        let text = r#"{"Version":"2012-10-17","Statement":[{"Sid":"A","Effect":"Allow","Principal":"*","Action":["s3:GetObject","s3:ListBucket"],"Resource":"arn:aws:s3:::b/*","Condition":{"Bool":{"aws:SecureTransport":"true"}}}]}"#;
        let pretty = format_policy(text).unwrap();
        assert_eq!(
            pretty,
            r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "A",
      "Effect": "Allow",
      "Principal": "*",
      "Action": [
        "s3:GetObject",
        "s3:ListBucket"
      ],
      "Resource": "arn:aws:s3:::b/*",
      "Condition": {
        "Bool": {
          "aws:SecureTransport": "true"
        }
      }
    }
  ]
}"#
        );
        // Formatting twice changes nothing, and the meaning is the same.
        assert_eq!(format_policy(&pretty).unwrap(), pretty);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pretty).unwrap(),
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
    }

    #[test]
    fn formatting_needs_valid_json_only() {
        // Structurally wrong but JSON: formatted all the same, so a policy that is
        // still being written can be tidied up.
        assert_eq!(format_policy("[1,{}]").unwrap(), "[\n  1,\n  {}\n]");
        assert_eq!(format_policy(r#"{"a":[]}"#).unwrap(), "{\n  \"a\": []\n}");
        let error = format_policy("{\"a\": ").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(error.to_string().starts_with("not valid JSON: "), "{error}");
        assert_eq!(
            format_policy("").unwrap_err().to_string(),
            "the policy is empty"
        );
        // Numbers, escapes and unicode survive.
        let text =
            r#"{"n":1.5,"big":18446744073709551615,"neg":-3,"s":"a\"b\né","t":true,"z":null}"#;
        let pretty = format_policy(text).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pretty).unwrap(),
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
        assert!(pretty.contains("18446744073709551615"), "{pretty}");
    }

    #[test]
    fn display_policy_falls_back_to_the_raw_text() {
        assert_eq!(display_policy("{\"a\":1}"), "{\n  \"a\": 1\n}");
        assert_eq!(display_policy("not json"), "not json");
    }

    // ---- sections ----

    #[test]
    fn section_json_shapes() {
        let loaded: Section<Option<String>> = Section::Loaded { value: None };
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            json!({"type": "loaded", "value": null})
        );
        let unsupported: Section<bool> = Section::Unsupported {
            message: "no".into(),
        };
        assert_eq!(
            serde_json::to_value(&unsupported).unwrap(),
            json!({"type": "unsupported", "message": "no"})
        );
        let failed: Section<bool> = Section::Failed {
            error: BbError {
                code: ErrorKind::Auth,
                message: "denied".into(),
            },
        };
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            value,
            json!({"type": "failed", "error": {"code": "auth", "message": "denied"}})
        );
        assert_eq!(
            serde_json::from_value::<Section<bool>>(value).unwrap(),
            failed
        );
    }

    #[test]
    fn section_from_result_sorts_failures() {
        let loaded = Section::from_result(Ok(5), "things");
        assert_eq!(loaded, Section::Loaded { value: 5 });
        let failed = Section::<i32>::from_result(Err(Error::Auth("denied".into())), "things");
        assert_eq!(
            failed,
            Section::Failed {
                error: BbError {
                    code: ErrorKind::Auth,
                    message: "denied".into()
                }
            }
        );
        // Only server answers can mean "not implemented".
        assert!(!is_unsupported(&Error::Usage("501".into())));
        assert_eq!(detail(&Error::Remote("x".into())), "");
    }

    #[test]
    fn json_shapes_of_the_models() {
        assert_eq!(
            serde_json::to_value(PublicAccessBlock {
                block_public_acls: true,
                ..PublicAccessBlock::default()
            })
            .unwrap(),
            json!({
                "block_public_acls": true, "ignore_public_acls": false,
                "block_public_policy": false, "restrict_public_buckets": false
            })
        );
        assert_eq!(
            serde_json::to_value(BucketVersioning {
                status: VersioningStatus::Suspended,
                mfa_delete: true
            })
            .unwrap(),
            json!({"status": "suspended", "mfa_delete": true})
        );
        assert_eq!(
            serde_json::to_value(VersioningStatus::Unversioned).unwrap(),
            json!("unversioned")
        );
        assert_eq!(
            serde_json::to_value(AccelerationStatus::Unset).unwrap(),
            json!("unset")
        );
        assert_eq!(
            serde_json::to_value(LoggingTarget {
                target_bucket: "logs".into(),
                target_prefix: "a/".into()
            })
            .unwrap(),
            json!({"target_bucket": "logs", "target_prefix": "a/"})
        );
        let hosting = WebsiteConfig::Hosting {
            index_document: "index.html".into(),
            error_document: None,
            routing_rules: vec![RoutingRule {
                condition: Some(RoutingCondition {
                    key_prefix_equals: Some("docs/".into()),
                    http_error_code_returned_equals: None,
                }),
                redirect: RoutingRedirect {
                    replace_key_prefix_with: Some("documents/".into()),
                    protocol: Some(RedirectProtocol::Https),
                    ..RoutingRedirect::default()
                },
            }],
        };
        let value = serde_json::to_value(&hosting).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "hosting",
                "index_document": "index.html",
                "error_document": null,
                "routing_rules": [{
                    "condition": {"key_prefix_equals": "docs/", "http_error_code_returned_equals": null},
                    "redirect": {
                        "protocol": "https", "host_name": null,
                        "replace_key_prefix_with": "documents/", "replace_key_with": null,
                        "http_redirect_code": null
                    }
                }]
            })
        );
        assert_eq!(
            serde_json::from_value::<WebsiteConfig>(value).unwrap(),
            hosting
        );
        let redirect: WebsiteConfig = serde_json::from_value(
            json!({"type": "redirect", "host_name": "example.com", "protocol": null}),
        )
        .unwrap();
        assert_eq!(
            redirect,
            WebsiteConfig::Redirect {
                host_name: "example.com".into(),
                protocol: None
            }
        );
        // The routing rules may be left out.
        let minimal: WebsiteConfig = serde_json::from_value(
            json!({"type": "hosting", "index_document": "i.html", "error_document": null}),
        )
        .unwrap();
        assert!(
            matches!(minimal, WebsiteConfig::Hosting { routing_rules, .. } if routing_rules.is_empty())
        );
    }

    // ---- website rules ----

    fn hosting(rules: Vec<RoutingRule>) -> WebsiteConfig {
        WebsiteConfig::Hosting {
            index_document: "index.html".into(),
            error_document: Some("error.html".into()),
            routing_rules: rules,
        }
    }

    fn rule(condition: Option<RoutingCondition>, redirect: RoutingRedirect) -> RoutingRule {
        RoutingRule {
            condition,
            redirect,
        }
    }

    fn problem(website: &WebsiteConfig) -> String {
        let error = website.validate().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        error.to_string()
    }

    #[test]
    fn valid_websites_pass() {
        hosting(Vec::new()).validate().unwrap();
        hosting(vec![
            rule(
                Some(RoutingCondition {
                    key_prefix_equals: Some("docs/".into()),
                    http_error_code_returned_equals: None,
                }),
                RoutingRedirect {
                    replace_key_prefix_with: Some("documents/".into()),
                    ..RoutingRedirect::default()
                },
            ),
            rule(
                Some(RoutingCondition {
                    key_prefix_equals: None,
                    http_error_code_returned_equals: Some("404".into()),
                }),
                RoutingRedirect {
                    protocol: Some(RedirectProtocol::Https),
                    host_name: Some("example.com:8443".into()),
                    replace_key_with: Some("not-found.html".into()),
                    http_redirect_code: Some("307".into()),
                    ..RoutingRedirect::default()
                },
            ),
            rule(
                None,
                RoutingRedirect {
                    http_redirect_code: Some("301".into()),
                    ..RoutingRedirect::default()
                },
            ),
        ])
        .validate()
        .unwrap();
        WebsiteConfig::Redirect {
            host_name: "example.com".into(),
            protocol: Some(RedirectProtocol::Http),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn hosting_documents_are_checked() {
        let mut website = hosting(Vec::new());
        if let WebsiteConfig::Hosting {
            index_document,
            error_document,
            ..
        } = &mut website
        {
            *index_document = String::new();
            *error_document = Some(String::new());
        }
        assert_eq!(
            problem(&website),
            "invalid website configuration: the index document is empty; \
             the error document must be a key of 1 to 1024 bytes"
        );
        if let WebsiteConfig::Hosting { index_document, .. } = &mut website {
            *index_document = "docs/index.html".into();
        }
        assert!(
            problem(&website)
                .contains("the index document \"docs/index.html\" must not contain a slash"),
            "{}",
            problem(&website)
        );
    }

    #[test]
    fn routing_rules_are_checked_one_by_one() {
        let rules = vec![
            rule(
                Some(RoutingCondition::default()),
                RoutingRedirect::default(),
            ),
            rule(
                Some(RoutingCondition {
                    key_prefix_equals: None,
                    http_error_code_returned_equals: Some("200".into()),
                }),
                RoutingRedirect {
                    replace_key_prefix_with: Some("a".into()),
                    replace_key_with: Some("b".into()),
                    host_name: Some("https://example.com/x".into()),
                    http_redirect_code: Some("404".into()),
                    ..RoutingRedirect::default()
                },
            ),
        ];
        let text = problem(&hosting(rules));
        for expected in [
            "routing rule 1: the condition needs a key prefix or an HTTP error code",
            "routing rule 1: the redirect needs a protocol, host, replacement key or prefix, or redirect code",
            "routing rule 2: the error code \"200\" must be a 4xx or 5xx HTTP status",
            "routing rule 2: a redirect replaces the key prefix or the whole key, not both",
            "routing rule 2: the redirect host: \"https://example.com/x\" must be a host name without a scheme or path",
            "routing rule 2: the redirect code \"404\" must be a 3xx HTTP status",
        ] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
    }

    #[test]
    fn at_most_fifty_routing_rules() {
        let one = || {
            rule(
                None,
                RoutingRedirect {
                    host_name: Some("example.com".into()),
                    ..RoutingRedirect::default()
                },
            )
        };
        hosting((0..MAX_ROUTING_RULES).map(|_| one()).collect())
            .validate()
            .unwrap();
        assert!(
            problem(&hosting((0..=MAX_ROUTING_RULES).map(|_| one()).collect()))
                .contains("there are 51 routing rules; S3 accepts at most 50")
        );
    }

    #[test]
    fn a_redirect_needs_a_plain_host() {
        for host in [
            "",
            "https://example.com",
            "example.com/path",
            "ex ample.com",
        ] {
            let website = WebsiteConfig::Redirect {
                host_name: host.into(),
                protocol: None,
            };
            assert!(
                problem(&website).contains("the redirect host: "),
                "{host:?}"
            );
        }
        assert!(status_in("301", '3'..='3'));
        assert!(!status_in("30", '3'..='3'));
        assert!(!status_in("3a1", '3'..='3'));
        assert!(status_in("503", '4'..='5'));
    }

    // ---- operations that fail before any request ----

    fn offline(endpoint: Option<&str>) -> S3 {
        use crate::client::{ClientOptions, Credentials};
        let account = Account {
            id: "a".into(),
            label: "A".into(),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_ref: "s".into(),
            endpoint: endpoint.map(str::to_owned),
            path_style: endpoint.is_some(),
            ..Account::default()
        };
        S3::new(
            &account,
            &Credentials::new("AKIDEXAMPLE", "secret", None),
            &ClientOptions::default(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn invalid_requests_are_refused_before_anything_is_sent() {
        let s3 = offline(None);
        for (what, error) in [
            (
                "versioning off",
                s3.put_bucket_versioning("b-1", VersioningStatus::Unversioned)
                    .await
                    .unwrap_err(),
            ),
            (
                "acceleration unset",
                s3.put_bucket_acceleration("b-1", AccelerationStatus::Unset)
                    .await
                    .unwrap_err(),
            ),
            (
                "acceleration on a dotted name",
                s3.put_bucket_acceleration("my.bucket", AccelerationStatus::Enabled)
                    .await
                    .unwrap_err(),
            ),
            (
                "an invalid policy",
                s3.put_bucket_policy("b-1", "{").await.unwrap_err(),
            ),
            (
                "an invalid website",
                s3.put_bucket_website(
                    "b-1",
                    &WebsiteConfig::Redirect {
                        host_name: String::new(),
                        protocol: None,
                    },
                )
                .await
                .unwrap_err(),
            ),
            (
                "an invalid bucket name",
                s3.put_public_access_block("not a bucket", &PublicAccessBlock::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "an invalid log target",
                s3.put_bucket_logging(
                    "b-1",
                    &LoggingTarget {
                        target_bucket: "bad/target".into(),
                        target_prefix: String::new(),
                    },
                )
                .await
                .unwrap_err(),
            ),
            (
                "a log prefix that is too long",
                s3.put_bucket_logging(
                    "b-1",
                    &LoggingTarget {
                        target_bucket: "logs".into(),
                        target_prefix: "p".repeat(MAX_KEY_BYTES + 1),
                    },
                )
                .await
                .unwrap_err(),
            ),
        ] {
            assert_eq!(error.kind(), ErrorKind::Usage, "{what}: {error}");
        }
        let error = s3
            .put_bucket_versioning("b-1", VersioningStatus::Unversioned)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("cannot be switched off"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn accounts_with_a_custom_endpoint_report_acceleration_as_unsupported() {
        let s3 = offline(Some("http://127.0.0.1:9"));
        assert!(!s3.supports_acceleration());
        assert!(offline(None).supports_acceleration());
        let error = s3.get_bucket_acceleration("b-1").await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(error.to_string(), ACCELERATION_UNSUPPORTED);
        let error = s3
            .put_bucket_acceleration("b-1", AccelerationStatus::Enabled)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), ACCELERATION_UNSUPPORTED);
        assert_eq!(
            s3.acceleration_section("b-1").await,
            Section::Unsupported {
                message: ACCELERATION_UNSUPPORTED.into()
            }
        );
    }

    // ---- external buckets ----

    fn named(name: &str) -> Bucket {
        Bucket {
            name: name.into(),
            region: Some("eu-west-1".into()),
            creation_date: None,
            owner: Some("owner".into()),
            kind: BucketKind::Aws,
        }
    }

    #[test]
    fn pinned_buckets_join_the_list_as_external() {
        let merged = merge_pinned(
            vec![named("b-listed"), named("a-listed")],
            &[
                "z-shared".to_owned(),
                "b-listed".to_owned(),
                "m-shared".to_owned(),
            ],
        );
        let summary: Vec<(&str, BucketKind)> = merged
            .iter()
            .map(|bucket| (bucket.name.as_str(), bucket.kind))
            .collect();
        assert_eq!(
            summary,
            [
                ("a-listed", BucketKind::Aws),
                ("b-listed", BucketKind::Aws),
                ("m-shared", BucketKind::External),
                ("z-shared", BucketKind::External),
            ]
        );
        // A listed bucket keeps what the server said about it.
        assert_eq!(merged[1].region.as_deref(), Some("eu-west-1"));
        // A pinned one has nothing but its name.
        assert_eq!(merged[2].region, None);
        assert_eq!(merged[2].owner, None);
        assert_eq!(merged[2].creation_date, None);
        assert_eq!(merge_pinned(Vec::new(), &[]), Vec::<Bucket>::new());
        // Duplicates in the file do not show twice.
        assert_eq!(
            merge_pinned(Vec::new(), &["x-b".to_owned(), "x-b".to_owned()]).len(),
            1
        );
    }

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(&Dirs::under(dir.path().join("cfg")));
        store
            .update(|config| {
                for id in ["acc-1", "acc-2"] {
                    config.accounts.push(Account {
                        id: id.into(),
                        label: id.into(),
                        access_key_id: "AKIDEXAMPLE".into(),
                        secret_ref: format!("{id}/secret"),
                        ..Account::default()
                    });
                }
                Ok(())
            })
            .unwrap();
        (dir, store)
    }

    #[test]
    fn pins_are_kept_per_account_in_the_configuration() {
        let (_dir, store) = store();
        pin_bucket(&store, "acc-1", "shared-data").unwrap();
        pin_bucket(&store, "acc-1", "another-one").unwrap();
        pin_bucket(&store, "acc-1", "shared-data").unwrap();
        pin_bucket(&store, "acc-2", "elsewhere").unwrap();
        let config = store.load().unwrap();
        assert_eq!(
            pinned_buckets(&config, "acc-1"),
            ["another-one", "shared-data"]
        );
        assert_eq!(pinned_buckets(&config, "acc-2"), ["elsewhere"]);
        assert_eq!(pinned_buckets(&config, "unknown"), Vec::<String>::new());

        // The file holds them under the account id.
        let text = std::fs::read_to_string(store.path()).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(
            table["pinned_buckets"]["acc-1"]
                .as_array()
                .unwrap()
                .iter()
                .map(|name| name.as_str().unwrap())
                .collect::<Vec<_>>(),
            ["another-one", "shared-data"]
        );
        assert_eq!(
            table["pinned_buckets"]["acc-2"][0].as_str(),
            Some("elsewhere")
        );

        unpin_bucket(&store, "acc-1", "shared-data").unwrap();
        let config = store.load().unwrap();
        assert_eq!(pinned_buckets(&config, "acc-1"), ["another-one"]);
        assert_eq!(pinned_buckets(&config, "acc-2"), ["elsewhere"]);
        unpin_bucket(&store, "acc-1", "another-one").unwrap();
        assert!(!store.load().unwrap().pinned_buckets.contains_key("acc-1"));
    }

    #[test]
    fn pins_survive_other_configuration_changes() {
        let (_dir, store) = store();
        pin_bucket(&store, "acc-1", "shared-data").unwrap();
        // Another writer works on a stale idea of the file: its change is
        // applied to what is on disk, which still has the pin.
        store
            .update(|config| {
                config.settings.interface.flat_browsing = true;
                config.default_account_id = Some("acc-2".into());
                Ok(())
            })
            .unwrap();
        let config = store.load().unwrap();
        assert_eq!(pinned_buckets(&config, "acc-1"), ["shared-data"]);
        assert!(config.settings.interface.flat_browsing);
        // A file from before pins loads with none.
        std::fs::write(
            store.path(),
            "[[accounts]]\nid = \"a\"\nlabel = \"L\"\naccess_key_id = \"K\"\nsecret_ref = \"s\"\n",
        )
        .unwrap();
        assert!(store.load().unwrap().pinned_buckets.is_empty());
    }

    #[test]
    fn pinning_validates_the_name_and_the_account() {
        let (_dir, store) = store();
        for name in ["", "has space", "a/b", "-start", &"n".repeat(256)] {
            let error = pin_bucket(&store, "acc-1", name).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{name:?}");
        }
        let error = pin_bucket(&store, "no-such", "shared-data").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let error = unpin_bucket(&store, "no-such", "shared-data").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        let error = unpin_bucket(&store, "acc-1", "never-pinned").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert!(error.to_string().contains("is not pinned"), "{error}");
        assert!(store.load().unwrap().pinned_buckets.is_empty());
    }

    #[test]
    fn invalid_entries_of_an_edited_file_are_left_out() {
        let (_dir, store) = store();
        store
            .update(|config| {
                config.pinned_buckets.insert(
                    "acc-1".into(),
                    vec![
                        "good-one".into(),
                        "bad name".into(),
                        String::new(),
                        "good-one".into(),
                    ],
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(
            pinned_buckets(&store.load().unwrap(), "acc-1"),
            ["good-one"]
        );
    }
}
