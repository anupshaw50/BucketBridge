//! `s3://bucket/key` URIs and S3 naming rules.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::model::BucketKind;

/// Maximum length of an object key in bytes (UTF-8).
pub const MAX_KEY_BYTES: usize = 1024;

/// A bucket plus a key or key prefix, written `s3://bucket/key`.
///
/// The key may be empty (the whole bucket) or end with `/` (a prefix). It is
/// kept exactly as written: no percent-decoding and no normalisation of
/// repeated slashes, because S3 keys are opaque strings.
///
/// Formatting always produces `s3://bucket/` followed by the key, so
/// `s3://bucket` parses and prints as `s3://bucket/`. Serde uses the same
/// string form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct S3Uri {
    bucket: String,
    key: String,
}

impl S3Uri {
    /// The URI scheme including the separator.
    pub const SCHEME: &'static str = "s3://";

    /// Builds a URI from parts, validating the bucket name and key length.
    pub fn new(bucket: impl Into<String>, key: impl Into<String>) -> Result<Self> {
        let bucket = bucket.into();
        let key = key.into();
        validate_bucket_name(&bucket)?;
        if key.len() > MAX_KEY_BYTES {
            return Err(Error::Usage(format!(
                "key is {} bytes long; S3 keys are at most {MAX_KEY_BYTES} bytes",
                key.len()
            )));
        }
        Ok(S3Uri { bucket, key })
    }

    /// Parses `s3://bucket`, `s3://bucket/` or `s3://bucket/key`.
    ///
    /// The scheme is matched case-insensitively.
    pub fn parse(input: &str) -> Result<Self> {
        let rest = input
            .get(..Self::SCHEME.len())
            .filter(|scheme| scheme.eq_ignore_ascii_case(Self::SCHEME))
            .map(|_| &input[Self::SCHEME.len()..])
            .ok_or_else(|| {
                Error::Usage(format!(
                    "'{input}' is not an S3 URI; expected s3://bucket or s3://bucket/key"
                ))
            })?;
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(Error::Usage(format!("S3 URI '{input}' has no bucket name")));
        }
        S3Uri::new(bucket, key).map_err(|err| err.with_context(format!("S3 URI '{input}'")))
    }

    /// Bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Key or prefix; empty for the whole bucket.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// True when the URI names the whole bucket.
    pub fn is_bucket_root(&self) -> bool {
        self.key.is_empty()
    }

    /// True when the key is empty or ends with `/`.
    pub fn is_prefix(&self) -> bool {
        self.key.is_empty() || self.key.ends_with('/')
    }

    /// Splits into `(bucket, key)`.
    pub fn into_parts(self) -> (String, String) {
        (self.bucket, self.key)
    }
}

impl fmt::Display for S3Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}/{}", Self::SCHEME, self.bucket, self.key)
    }
}

impl FromStr for S3Uri {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        S3Uri::parse(s)
    }
}

impl Serialize for S3Uri {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for S3Uri {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        S3Uri::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Checks a bucket name used to address an existing bucket.
///
/// Accepts the legacy rules that some existing buckets still follow: 1 to
/// 255 characters of ASCII letters, digits, `.`, `_` and `-`, starting with a
/// letter or digit. Anything else (slashes, spaces, `..`) could change the
/// request path and is rejected.
pub fn validate_bucket_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Usage("bucket name is empty".into()));
    }
    if name.len() > 255 {
        return Err(Error::Usage(format!(
            "bucket name is {} characters long; the maximum is 255",
            name.len()
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(Error::Usage(format!(
            "bucket name '{name}' contains the character {bad:?}; only letters, digits, '.', '_' and '-' are allowed"
        )));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(Error::Usage(format!(
            "bucket name '{name}' must start with a letter or digit"
        )));
    }
    Ok(())
}

/// Checks the name of a bucket about to be created, using the current S3
/// naming rules: 3 to 63 characters of lowercase letters, digits, `.` and
/// `-`; starts and ends with a letter or digit; no `..`; not an IPv4
/// address. For AWS accounts the reserved prefixes and suffixes are rejected
/// as well (`xn--`, `sthree-`, `amzn-s3-demo-`, `-s3alias`, `--ol-s3`,
/// `.mrap`, `--x-s3`, `--table-s3`).
pub fn validate_new_bucket_name(name: &str, kind: BucketKind) -> Result<()> {
    let fail = |why: &str| Err(Error::Usage(format!("invalid bucket name '{name}': {why}")));
    if !(3..=63).contains(&name.len()) {
        return fail("must be 3 to 63 characters long");
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-')))
    {
        return fail(&format!(
            "the character {bad:?} is not allowed (use lowercase letters, digits, '.' and '-')"
        ));
    }
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !name.starts_with(alnum) || !name.ends_with(alnum) {
        return fail("must start and end with a lowercase letter or digit");
    }
    if name.contains("..") {
        return fail("must not contain two adjacent periods");
    }
    if name.parse::<std::net::Ipv4Addr>().is_ok() {
        return fail("must not be formatted as an IP address");
    }
    if kind == BucketKind::Aws {
        for prefix in ["xn--", "sthree-", "amzn-s3-demo-"] {
            if name.starts_with(prefix) {
                return fail(&format!("the prefix '{prefix}' is reserved"));
            }
        }
        for suffix in ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"] {
            if name.ends_with(suffix) {
                return fail(&format!("the suffix '{suffix}' is reserved"));
            }
        }
    }
    Ok(())
}

/// Checks an object key: non-empty and at most [`MAX_KEY_BYTES`] bytes.
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::Usage("object key is empty".into()));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(Error::Usage(format!(
            "object key is {} bytes long; S3 keys are at most {MAX_KEY_BYTES} bytes",
            key.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn parses_bucket_and_key() {
        let uri = S3Uri::parse("s3://photos/2024/a.jpg").unwrap();
        assert_eq!(uri.bucket(), "photos");
        assert_eq!(uri.key(), "2024/a.jpg");
        assert!(!uri.is_prefix());
        assert_eq!(uri.to_string(), "s3://photos/2024/a.jpg");
    }

    #[test]
    fn bucket_root_forms() {
        for input in ["s3://photos", "s3://photos/", "S3://photos/"] {
            let uri = S3Uri::parse(input).unwrap();
            assert_eq!(uri.bucket(), "photos");
            assert_eq!(uri.key(), "");
            assert!(uri.is_bucket_root());
            assert!(uri.is_prefix());
            assert_eq!(uri.to_string(), "s3://photos/");
        }
    }

    #[test]
    fn keys_are_kept_verbatim() {
        let uri = S3Uri::parse("s3://b/dir//with space/%41/").unwrap();
        assert_eq!(uri.key(), "dir//with space/%41/");
        assert!(uri.is_prefix());
        let uri = S3Uri::parse("s3://b//leading").unwrap();
        assert_eq!(uri.key(), "/leading");
        let uri = S3Uri::parse("s3://b/ünïcödé/ファイル").unwrap();
        assert_eq!(uri.key(), "ünïcödé/ファイル");
        assert_eq!(uri.to_string(), "s3://b/ünïcödé/ファイル");
    }

    #[test]
    fn rejects_malformed_input_precisely() {
        let cases = [
            ("", "not an S3 URI"),
            ("bucket/key", "not an S3 URI"),
            ("s3:/bucket", "not an S3 URI"),
            ("http://bucket/key", "not an S3 URI"),
            ("s3://", "has no bucket name"),
            ("s3:///key", "has no bucket name"),
            ("s3://bad bucket/key", "contains the character ' '"),
            ("s3://bad?bucket", "contains the character '?'"),
            ("s3://-bucket/", "must start with a letter or digit"),
            ("s3://../x", "must start with a letter or digit"),
        ];
        for (input, expected) in cases {
            let err = S3Uri::parse(input).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{input}");
            assert!(err.to_string().contains(expected), "{input}: {err}");
        }
        let long_key = format!("s3://b/{}", "k".repeat(MAX_KEY_BYTES + 1));
        assert!(
            S3Uri::parse(&long_key)
                .unwrap_err()
                .to_string()
                .contains("at most 1024 bytes")
        );
        let max_key = format!("s3://b/{}", "k".repeat(MAX_KEY_BYTES));
        assert!(S3Uri::parse(&max_key).is_ok());
    }

    #[test]
    fn multibyte_input_near_the_scheme_does_not_panic() {
        assert!(S3Uri::parse("ü").is_err());
        assert!(S3Uri::parse("s3:ü//b").is_err());
        assert!(S3Uri::parse("éééé").is_err());
    }

    #[test]
    fn serde_uses_the_string_form() {
        let uri = S3Uri::new("bucket", "photos/").unwrap();
        assert_eq!(
            serde_json::to_value(&uri).unwrap(),
            serde_json::json!("s3://bucket/photos/")
        );
        let back: S3Uri = serde_json::from_value(serde_json::json!("s3://bucket/photos/")).unwrap();
        assert_eq!(back, uri);
        assert!(serde_json::from_value::<S3Uri>(serde_json::json!("bucket/photos/")).is_err());
        assert_eq!(
            "s3://a/b".parse::<S3Uri>().unwrap(),
            S3Uri::new("a", "b").unwrap()
        );
    }

    #[test]
    fn legacy_bucket_names_are_addressable() {
        for name in ["a", "My_Legacy.Bucket-1", "b", &"x".repeat(255)] {
            assert!(validate_bucket_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            "a/b",
            "a b",
            ".",
            "..",
            "_x",
            &"x".repeat(256),
            "a\\b",
            "a\u{0}b",
        ] {
            assert!(validate_bucket_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn new_bucket_rules() {
        for name in ["abc", "my-bucket.logs", "a1b2c3", &"a".repeat(63)] {
            assert!(
                validate_new_bucket_name(name, BucketKind::Aws).is_ok(),
                "{name}"
            );
        }
        let cases = [
            ("ab", "3 to 63"),
            ("Upper", "not allowed"),
            ("under_score", "not allowed"),
            ("-start", "start and end"),
            ("end.", "start and end"),
            ("a..b", "adjacent periods"),
            ("192.168.5.4", "IP address"),
            ("xn--abc", "reserved"),
            ("sthree-abc", "reserved"),
            ("amzn-s3-demo-x", "reserved"),
            ("abc-s3alias", "reserved"),
            ("abc--ol-s3", "reserved"),
            ("abc.mrap", "reserved"),
            ("abc--x-s3", "reserved"),
            ("abc--table-s3", "reserved"),
        ];
        for (name, expected) in cases {
            let err = validate_new_bucket_name(name, BucketKind::Aws).unwrap_err();
            assert!(err.to_string().contains(expected), "{name}: {err}");
        }
        assert!(validate_new_bucket_name(&"a".repeat(64), BucketKind::Aws).is_err());
        assert!(validate_new_bucket_name("xn--abc", BucketKind::External).is_ok());
        assert!(validate_new_bucket_name("Upper", BucketKind::External).is_err());
    }

    #[test]
    fn key_rules() {
        assert!(validate_key("a").is_ok());
        assert!(validate_key(&"k".repeat(MAX_KEY_BYTES)).is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key(&"k".repeat(MAX_KEY_BYTES + 1)).is_err());
        // Multibyte characters count in bytes.
        assert!(validate_key(&"é".repeat(513)).is_err());
    }
}
