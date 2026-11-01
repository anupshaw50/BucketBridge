//! HTTP headers and user metadata of an object.
//!
//! S3 stores `Content-Type`, `Cache-Control`, `Content-Disposition`,
//! `Content-Encoding` (and `Content-Language`, `Expires`) and user metadata
//! (`x-amz-meta-*`) with the object. They can only be changed by copying the
//! object onto itself with `MetadataDirective: REPLACE`, which replaces
//! all of them at once: [`HeaderChanges`] is a patch, and every object gets
//! the full set that results from applying it to what the object has now.

use std::collections::{BTreeMap, BTreeSet};

use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_smithy_types::date_time::Format;
use serde::{Deserialize, Serialize};

use super::inplace::{Edit, Plan};
use super::sse::CustomerKey;
use super::{BatchOptions, ObjectChange, run_batch};
use crate::error::{Error, Result};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// Most bytes of user metadata an object can carry: the UTF-8 size of all
/// keys and values together.
pub const MAX_USER_META_BYTES: usize = 2048;

/// Longest value accepted for a standard header.
pub const MAX_HEADER_VALUE_BYTES: usize = 2048;

/// What to do with one standard header.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HeaderChange {
    /// Leave the header as it is on every object.
    #[default]
    Keep,
    /// Give the header this value.
    Set {
        /// The new value.
        value: String,
    },
    /// Remove the header.
    Remove,
}

impl HeaderChange {
    fn apply(&self, current: &Option<String>) -> Option<String> {
        match self {
            HeaderChange::Keep => current.clone(),
            HeaderChange::Set { value } => Some(value.clone()),
            HeaderChange::Remove => None,
        }
    }
}

/// A patch to the HTTP headers and user metadata of objects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderChanges {
    /// `Content-Type`. Removing it leaves S3's default, `binary/octet-stream`.
    pub content_type: HeaderChange,
    /// `Cache-Control`.
    pub cache_control: HeaderChange,
    /// `Content-Disposition`.
    pub content_disposition: HeaderChange,
    /// `Content-Encoding`.
    pub content_encoding: HeaderChange,
    /// `x-amz-website-redirect-location`: `/path` or an `http(s)://` URL.
    pub website_redirect: HeaderChange,
    /// User metadata to add or overwrite (keys without `x-amz-meta-`; S3
    /// stores them in lowercase).
    pub user_meta_set: BTreeMap<String, String>,
    /// Keys of the user metadata to remove.
    pub user_meta_remove: BTreeSet<String>,
}

impl HeaderChanges {
    /// Checks the patch without looking at any object. The size limit of the
    /// user metadata depends on what each object has and is checked per object.
    pub fn validate(&self) -> Result<()> {
        for (name, change) in [
            ("Content-Type", &self.content_type),
            ("Cache-Control", &self.cache_control),
            ("Content-Disposition", &self.content_disposition),
            ("Content-Encoding", &self.content_encoding),
        ] {
            if let HeaderChange::Set { value } = change {
                check_header_value(name, value)?;
                if name == "Content-Type" {
                    check_media_type(value)?;
                }
            }
        }
        if let HeaderChange::Set { value } = &self.website_redirect {
            check_header_value("the website redirect location", value)?;
            let ok = value.starts_with('/')
                || value.starts_with("http://")
                || value.starts_with("https://");
            if !ok {
                return Err(Error::Usage(format!(
                    "'{}' is not a website redirect location; it must start with / or http:// or https://",
                    value.escape_debug()
                )));
            }
        }
        let mut set = BTreeSet::new();
        for (key, value) in &self.user_meta_set {
            check_meta_key(key)?;
            check_header_value(&format!("the user metadata '{key}'"), value)?;
            if !set.insert(key.to_ascii_lowercase()) {
                return Err(Error::Usage(format!(
                    "the user metadata key '{key}' is given twice (keys are not case sensitive)"
                )));
            }
        }
        for key in &self.user_meta_remove {
            check_meta_key(key)?;
            if set.contains(&key.to_ascii_lowercase()) {
                return Err(Error::Usage(format!(
                    "the user metadata '{key}' is both set and removed"
                )));
            }
        }
        Ok(())
    }
}

/// Visible ASCII only (S3 requires US-ASCII in headers), no control
/// characters, so a value can never end a header line and start another.
fn check_header_value(what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Usage(format!(
            "the value of {what} is empty; remove the header instead"
        )));
    }
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(Error::Usage(format!(
            "the value of {what} is {} bytes; at most {MAX_HEADER_VALUE_BYTES} are allowed",
            value.len()
        )));
    }
    if let Some(bad) = value.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(Error::Usage(format!(
            "the value of {what} contains {bad:?}; S3 headers accept printable ASCII characters only"
        )));
    }
    Ok(())
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

/// `type/subtype`, optionally followed by `; parameters`.
fn check_media_type(value: &str) -> Result<()> {
    let media = value.split(';').next().unwrap_or_default().trim();
    match media.split_once('/') {
        Some((kind, subtype))
            if !kind.is_empty()
                && !subtype.is_empty()
                && kind.chars().all(is_token_char)
                && subtype.chars().all(is_token_char) =>
        {
            Ok(())
        }
        _ => Err(Error::Usage(format!(
            "'{}' is not a media type; expected type/subtype such as text/plain; charset=utf-8",
            value.escape_debug()
        ))),
    }
}

fn check_meta_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::Usage("a user metadata key cannot be empty".into()));
    }
    if let Some(bad) = key.chars().find(|c| !is_token_char(*c)) {
        return Err(Error::Usage(format!(
            "the user metadata key '{}' contains {bad:?}; use letters, digits and ! # $ % & ' * + - . ^ _ ` | ~",
            key.escape_debug()
        )));
    }
    Ok(())
}

/// The UTF-8 size S3 counts against [`MAX_USER_META_BYTES`].
pub(crate) fn user_meta_bytes(meta: &BTreeMap<String, String>) -> usize {
    meta.iter()
        .map(|(key, value)| key.len() + value.len())
        .sum()
}

/// Every header that an in-place copy replaces, as one complete set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HeaderSet {
    pub content_type: Option<String>,
    pub cache_control: Option<String>,
    pub content_disposition: Option<String>,
    pub content_encoding: Option<String>,
    pub content_language: Option<String>,
    pub website_redirect: Option<String>,
    pub expires: Option<aws_smithy_types::DateTime>,
    pub user_meta: BTreeMap<String, String>,
}

impl HeaderSet {
    /// The headers of an existing object.
    pub(crate) fn from_head(head: &HeadObjectOutput) -> Self {
        HeaderSet {
            content_type: head.content_type().map(str::to_owned),
            cache_control: head.cache_control().map(str::to_owned),
            content_disposition: head.content_disposition().map(str::to_owned),
            content_encoding: head.content_encoding().map(str::to_owned),
            content_language: head.content_language().map(str::to_owned),
            website_redirect: head.website_redirect_location().map(str::to_owned),
            expires: head
                .expires_string()
                .and_then(|text| aws_smithy_types::DateTime::from_str(text, Format::HttpDate).ok()),
            user_meta: head
                .metadata()
                .map(|meta| {
                    meta.iter()
                        .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The set after applying `changes`. Fails when the user metadata would
    /// exceed [`MAX_USER_META_BYTES`].
    pub(crate) fn changed(&self, changes: &HeaderChanges) -> Result<HeaderSet> {
        let mut user_meta = self.user_meta.clone();
        for key in &changes.user_meta_remove {
            user_meta.remove(&key.to_ascii_lowercase());
        }
        for (key, value) in &changes.user_meta_set {
            user_meta.insert(key.to_ascii_lowercase(), value.clone());
        }
        let size = user_meta_bytes(&user_meta);
        if size > MAX_USER_META_BYTES {
            return Err(Error::Usage(format!(
                "the user metadata would be {size} bytes; S3 allows at most {MAX_USER_META_BYTES} bytes for all keys and values together"
            )));
        }
        Ok(HeaderSet {
            content_type: changes.content_type.apply(&self.content_type),
            cache_control: changes.cache_control.apply(&self.cache_control),
            content_disposition: changes.content_disposition.apply(&self.content_disposition),
            content_encoding: changes.content_encoding.apply(&self.content_encoding),
            content_language: self.content_language.clone(),
            website_redirect: changes.website_redirect.apply(&self.website_redirect),
            expires: self.expires,
            user_meta,
        })
    }
}

impl S3 {
    /// Edits HTTP headers and user metadata of each object in `keys`: every
    /// object is copied onto itself with `MetadataDirective: REPLACE` and the
    /// full set that results from applying `changes` to its current headers.
    /// Storage class, encryption, tags and the other headers (`Content-Language`,
    /// `Expires`) are kept; the website redirect is kept unless `changes` names it.
    ///
    /// An object that already has the wanted headers is reported unchanged
    /// and nothing is sent for it. Objects larger than 5 GiB are copied in
    /// parts. For an object encrypted with SSE-C pass its key as `sse_c`.
    /// The copy replaces the object (on a versioned bucket it adds a new
    /// version); a bucket with ACLs enabled gets the object's ACL re-applied.
    pub async fn update_object_headers(
        &self,
        bucket: &str,
        keys: &[String],
        changes: &HeaderChanges,
        sse_c: Option<&CustomerKey>,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        changes.validate()?;
        run_batch(keys, options, |key| async move {
            self.edit_object(bucket, &key, "headers", sse_c, options.dry_run, |head| {
                let current = HeaderSet::from_head(head);
                let wanted = current.changed(changes)?;
                if wanted == current {
                    return Ok(Plan::unchanged(None));
                }
                if head.missing_meta().is_some_and(|missing| missing > 0) {
                    return Err(Error::Conflict(
                        "S3 could not return some of the user metadata (it is not plain ASCII), and replacing the headers would delete it; nothing was changed".into(),
                    ));
                }
                Ok(Plan::change(
                    None,
                    Edit {
                        replace: Some(wanted),
                        ..Edit::default()
                    },
                ))
            })
            .await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use serde_json::json;

    fn set(value: &str) -> HeaderChange {
        HeaderChange::Set {
            value: value.to_owned(),
        }
    }

    fn meta(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn head() -> HeadObjectOutput {
        HeadObjectOutput::builder()
            .content_type("text/plain")
            .cache_control("max-age=60")
            .content_disposition("inline")
            .content_encoding("gzip")
            .content_language("en")
            .expires_string("Fri, 21 Dec 2012 00:00:00 GMT")
            .metadata("Owner", "team")
            .metadata("stage", "raw")
            .build()
    }

    #[test]
    fn a_head_response_becomes_a_header_set() {
        let current = HeaderSet::from_head(&head());
        assert_eq!(current.content_type.as_deref(), Some("text/plain"));
        assert_eq!(current.cache_control.as_deref(), Some("max-age=60"));
        assert_eq!(current.content_disposition.as_deref(), Some("inline"));
        assert_eq!(current.content_encoding.as_deref(), Some("gzip"));
        assert_eq!(current.content_language.as_deref(), Some("en"));
        assert_eq!(current.expires.map(|t| t.secs()), Some(1_356_048_000));
        // Keys are lowercase, as S3 stores them.
        assert_eq!(
            current.user_meta,
            meta(&[("owner", "team"), ("stage", "raw")])
        );
    }

    #[test]
    fn a_patch_changes_only_what_it_names() {
        let current = HeaderSet::from_head(&head());
        let changes = HeaderChanges {
            content_type: set("application/json"),
            cache_control: HeaderChange::Remove,
            user_meta_set: meta(&[("Stage", "clean"), ("new", "1")]),
            user_meta_remove: BTreeSet::from(["OWNER".to_owned(), "absent".to_owned()]),
            ..HeaderChanges::default()
        };
        changes.validate().unwrap();
        let wanted = current.changed(&changes).unwrap();
        assert_eq!(wanted.content_type.as_deref(), Some("application/json"));
        assert_eq!(wanted.cache_control, None);
        assert_eq!(wanted.content_disposition.as_deref(), Some("inline"));
        assert_eq!(wanted.content_encoding.as_deref(), Some("gzip"));
        // The headers the patch cannot name are kept.
        assert_eq!(wanted.content_language.as_deref(), Some("en"));
        assert_eq!(wanted.expires, current.expires);
        assert_eq!(wanted.user_meta, meta(&[("new", "1"), ("stage", "clean")]));
        assert_ne!(wanted, current);
        // An empty patch changes nothing.
        assert_eq!(current.changed(&HeaderChanges::default()).unwrap(), current);
        // A patch that sets what is already there changes nothing either.
        let same = HeaderChanges {
            content_type: set("text/plain"),
            user_meta_set: meta(&[("owner", "team")]),
            ..HeaderChanges::default()
        };
        assert_eq!(current.changed(&same).unwrap(), current);
    }

    #[test]
    fn user_metadata_is_limited_to_2048_bytes_in_total() {
        let current = HeaderSet {
            user_meta: meta(&[("a", &"x".repeat(1000))]),
            ..HeaderSet::default()
        };
        let fits = HeaderChanges {
            user_meta_set: meta(&[("b", &"y".repeat(1045))]),
            ..HeaderChanges::default()
        };
        // 1 + 1000 + 1 + 1045 = 2047 bytes.
        assert!(current.changed(&fits).is_ok());
        let exactly = HeaderChanges {
            user_meta_set: meta(&[("b", &"y".repeat(1046))]),
            ..HeaderChanges::default()
        };
        assert!(current.changed(&exactly).is_ok(), "2048 bytes are allowed");
        let over = HeaderChanges {
            user_meta_set: meta(&[("b", &"y".repeat(1047))]),
            ..HeaderChanges::default()
        };
        let err = current.changed(&over).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("2049 bytes"), "{err}");
        // Removing metadata in the same patch makes room.
        let swap = HeaderChanges {
            user_meta_set: meta(&[("b", &"y".repeat(1047))]),
            user_meta_remove: BTreeSet::from(["a".to_owned()]),
            ..HeaderChanges::default()
        };
        assert!(current.changed(&swap).is_ok());
        assert_eq!(user_meta_bytes(&meta(&[("ab", "cde")])), 5);
        // Bytes, not characters.
        assert_eq!(user_meta_bytes(&meta(&[("k", "é")])), 3);
    }

    #[test]
    fn header_values_must_be_printable_ascii() {
        for bad in ["", "   ", "line\nbreak", "tab\there", "café", "nul\0"] {
            let changes = HeaderChanges {
                cache_control: set(bad),
                ..HeaderChanges::default()
            };
            assert_eq!(
                changes.validate().unwrap_err().kind(),
                ErrorKind::Usage,
                "{bad:?}"
            );
        }
        let long = HeaderChanges {
            content_disposition: set(&"a".repeat(MAX_HEADER_VALUE_BYTES + 1)),
            ..HeaderChanges::default()
        };
        assert!(long.validate().is_err());
        let ok = HeaderChanges {
            content_disposition: set(r#"attachment; filename="report 1.pdf""#),
            cache_control: set("public, max-age=31536000, immutable"),
            ..HeaderChanges::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn content_types_are_checked() {
        for good in [
            "text/plain",
            "application/json; charset=utf-8",
            "image/svg+xml",
            "binary/octet-stream",
        ] {
            let changes = HeaderChanges {
                content_type: set(good),
                ..HeaderChanges::default()
            };
            assert!(changes.validate().is_ok(), "{good}");
        }
        for bad in [
            "text",
            "/plain",
            "text/",
            "te xt/plain",
            "a/b/c;",
            "; charset=utf-8",
        ] {
            let changes = HeaderChanges {
                content_type: set(bad),
                ..HeaderChanges::default()
            };
            assert!(changes.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn user_metadata_keys_are_header_tokens() {
        for good in ["owner", "Cost-Center", "a.b_c", "x1"] {
            let changes = HeaderChanges {
                user_meta_set: meta(&[(good, "v")]),
                ..HeaderChanges::default()
            };
            assert!(changes.validate().is_ok(), "{good}");
        }
        for bad in ["", "two words", "a:b", "ünï", "a/b", "tab\t", "a,b", "(x)"] {
            let changes = HeaderChanges {
                user_meta_set: meta(&[(bad, "v")]),
                ..HeaderChanges::default()
            };
            assert!(changes.validate().is_err(), "{bad:?}");
            let removal = HeaderChanges {
                user_meta_remove: BTreeSet::from([bad.to_owned()]),
                ..HeaderChanges::default()
            };
            assert!(removal.validate().is_err(), "{bad:?}");
        }
        let value = HeaderChanges {
            user_meta_set: meta(&[("k", "ünï")]),
            ..HeaderChanges::default()
        };
        assert!(value.validate().is_err());
    }

    #[test]
    fn keys_are_case_insensitive_so_duplicates_and_contradictions_are_refused() {
        let twice = HeaderChanges {
            user_meta_set: meta(&[("Owner", "a"), ("owner", "b")]),
            ..HeaderChanges::default()
        };
        assert!(twice.validate().unwrap_err().to_string().contains("twice"));
        let both = HeaderChanges {
            user_meta_set: meta(&[("owner", "a")]),
            user_meta_remove: BTreeSet::from(["OWNER".to_owned()]),
            ..HeaderChanges::default()
        };
        assert!(both.validate().unwrap_err().to_string().contains("both"));
    }

    #[test]
    fn the_json_shape_has_an_action_per_header() {
        let changes: HeaderChanges = serde_json::from_value(json!({
            "content_type": { "action": "set", "value": "text/css" },
            "cache_control": { "action": "remove" },
            "content_disposition": { "action": "keep" },
            "content_encoding": { "action": "keep" },
            "user_meta_set": { "k": "v" },
            "user_meta_remove": ["gone"]
        }))
        .unwrap();
        assert_eq!(changes.content_type, set("text/css"));
        assert_eq!(changes.cache_control, HeaderChange::Remove);
        assert_eq!(changes.content_disposition, HeaderChange::Keep);
        assert_eq!(changes.user_meta_set, meta(&[("k", "v")]));
        assert_eq!(
            changes.user_meta_remove,
            BTreeSet::from(["gone".to_owned()])
        );
        // Every field may be left out; it then keeps the header.
        let empty: HeaderChanges = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, HeaderChanges::default());
        assert!(
            serde_json::from_value::<HeaderChanges>(
                json!({ "content_type": { "action": "clear" } })
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(HeaderChange::Keep).unwrap(),
            json!({ "action": "keep" })
        );
    }
}
