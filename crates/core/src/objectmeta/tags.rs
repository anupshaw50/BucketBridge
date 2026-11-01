//! Object tags: reading, replacing, deleting and editing them in batches.

use std::collections::{BTreeMap, BTreeSet};

use aws_sdk_s3::types::{Tag, Tagging};
use serde::{Deserialize, Serialize};

use super::{BatchOptions, ObjectChange, run_batch, validate_object_key};
use crate::error::{Error, Result};
use crate::ops::{S3, build_error};
use crate::s3uri::{validate_bucket_name, validate_key};

/// Most tags an object can have.
pub const MAX_TAGS: usize = 10;

/// Longest tag key, in characters.
pub const MAX_TAG_KEY_CHARS: usize = 128;

/// Longest tag value, in characters.
pub const MAX_TAG_VALUE_CHARS: usize = 256;

/// Checks one tag against the S3 rules: a key of 1 to 128 and a value of at
/// most 256 characters, made of letters, digits, spaces and `+ - = . _ : / @`;
/// keys do not start with `aws:` (reserved).
pub fn validate_tag(key: &str, value: &str) -> Result<()> {
    validate_tag_key(key)?;
    check_tag_text("tag value", value, MAX_TAG_VALUE_CHARS)
}

fn validate_tag_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::Usage("a tag key cannot be empty".into()));
    }
    check_tag_text("tag key", key, MAX_TAG_KEY_CHARS)?;
    if key.to_ascii_lowercase().starts_with("aws:") {
        return Err(Error::Usage(format!(
            "the tag key '{key}' starts with 'aws:', which is reserved"
        )));
    }
    Ok(())
}

fn check_tag_text(what: &str, text: &str, max_chars: usize) -> Result<()> {
    if text.chars().count() > max_chars {
        return Err(Error::Usage(format!(
            "the {what} '{}' is longer than {max_chars} characters",
            text.escape_debug()
        )));
    }
    if let Some(bad) = text.chars().find(|c| {
        !(c.is_alphanumeric() || matches!(c, ' ' | '+' | '-' | '=' | '.' | '_' | ':' | '/' | '@'))
    }) {
        return Err(Error::Usage(format!(
            "the {what} '{}' contains {bad:?}; use letters, digits, spaces and + - = . _ : / @",
            text.escape_debug()
        )));
    }
    Ok(())
}

/// Checks a whole tag set: at most [`MAX_TAGS`] tags, each valid.
pub fn validate_tags(tags: &BTreeMap<String, String>) -> Result<()> {
    if tags.len() > MAX_TAGS {
        return Err(Error::Usage(format!(
            "{} tags are too many; an object can have at most {MAX_TAGS}",
            tags.len()
        )));
    }
    for (key, value) in tags {
        validate_tag(key, value)?;
    }
    Ok(())
}

/// A change to the tags of one or many objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TagEdit {
    /// Makes `tags` the whole tag set; an empty set deletes every tag.
    Replace {
        /// The new tag set.
        tags: BTreeMap<String, String>,
    },
    /// Adds or overwrites the tags in `set` and removes the keys in `remove`;
    /// the other tags of each object stay.
    Merge {
        /// Tags to add or overwrite.
        #[serde(default)]
        set: BTreeMap<String, String>,
        /// Keys of the tags to remove.
        #[serde(default)]
        remove: BTreeSet<String>,
    },
}

impl TagEdit {
    /// Checks the edit without looking at any object.
    pub fn validate(&self) -> Result<()> {
        match self {
            TagEdit::Replace { tags } => validate_tags(tags),
            TagEdit::Merge { set, remove } => {
                if set.len() > MAX_TAGS {
                    return Err(Error::Usage(format!(
                        "{} tags are too many; an object can have at most {MAX_TAGS}",
                        set.len()
                    )));
                }
                for (key, value) in set {
                    validate_tag(key, value)?;
                }
                for key in remove {
                    validate_tag_key(key)?;
                }
                if let Some(both) = set.keys().find(|key| remove.contains(*key)) {
                    return Err(Error::Usage(format!(
                        "the tag '{both}' is both set and removed"
                    )));
                }
                Ok(())
            }
        }
    }

    /// The tag set that results from applying the edit to `current`.
    pub fn apply(&self, current: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
        let wanted = match self {
            TagEdit::Replace { tags } => tags.clone(),
            TagEdit::Merge { set, remove } => {
                let mut wanted = current.clone();
                wanted.retain(|key, _| !remove.contains(key));
                wanted.extend(set.iter().map(|(k, v)| (k.clone(), v.clone())));
                wanted
            }
        };
        validate_tags(&wanted)?;
        Ok(wanted)
    }
}

impl S3 {
    /// The tags of an object (`GetObjectTagging`); empty when it has none.
    pub async fn get_object_tags(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<BTreeMap<String, String>> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        let out = self
            .client_for(bucket)
            .await?
            .get_object_tagging()
            .bucket(bucket)
            .key(key)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("reading the tags of s3://{bucket}/{key}"))
            })?;
        Ok(out
            .tag_set()
            .iter()
            .map(|tag| (tag.key().to_owned(), tag.value().to_owned()))
            .collect())
    }

    /// Replaces the tags of an object (`PutObjectTagging`). An empty set
    /// deletes them (`DeleteObjectTagging`), which every server accepts.
    pub async fn put_object_tags(
        &self,
        bucket: &str,
        key: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        validate_tags(tags)?;
        if tags.is_empty() {
            return self.delete_object_tags(bucket, key).await;
        }
        let tag_set = tags
            .iter()
            .map(|(key, value)| Tag::builder().key(key).value(value).build())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(build_error)?;
        let tagging = Tagging::builder()
            .set_tag_set(Some(tag_set))
            .build()
            .map_err(build_error)?;
        self.client_for(bucket)
            .await?
            .put_object_tagging()
            .bucket(bucket)
            .key(key)
            .tagging(tagging)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| Error::from(err).with_context(format!("tagging s3://{bucket}/{key}")))?;
        Ok(())
    }

    /// Deletes every tag of an object (`DeleteObjectTagging`).
    pub async fn delete_object_tags(&self, bucket: &str, key: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        // DeleteObjectTagging has no request-payer parameter.
        self.client_for(bucket)
            .await?
            .delete_object_tagging()
            .bucket(bucket)
            .key(key)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("deleting the tags of s3://{bucket}/{key}"))
            })?;
        Ok(())
    }

    /// Applies a [`TagEdit`] to each object in `keys`, up to
    /// `options.concurrency` at a time, and reports every key.
    ///
    /// The edit is checked first: an invalid tag fails the whole call before
    /// anything is sent. [`TagEdit::Replace`] writes without reading;
    /// [`TagEdit::Merge`] reads each object's tags and reports an object that
    /// already has the wanted tags as unchanged. A merge that would leave an
    /// object with more than 10 tags fails for that object only. With
    /// `options.dry_run` nothing is written.
    pub async fn update_object_tags(
        &self,
        bucket: &str,
        keys: &[String],
        edit: &TagEdit,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        edit.validate()?;
        run_batch(keys, options, |key| async move {
            validate_object_key(&key)?;
            let wanted = match edit {
                TagEdit::Replace { tags } => tags.clone(),
                TagEdit::Merge { .. } => {
                    let current = self.get_object_tags(bucket, &key).await?;
                    let wanted = edit.apply(&current)?;
                    if wanted == current {
                        return Ok(ObjectChange::unchanged(&key, None));
                    }
                    wanted
                }
            };
            if !options.dry_run {
                self.put_object_tags(bucket, &key, &wanted).await?;
            }
            Ok(ObjectChange::changed(&key, None))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use serde_json::json;

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn valid_tags_follow_the_s3_character_rules() {
        let longest_value = "v".repeat(MAX_TAG_VALUE_CHARS);
        let longest_key = "k".repeat(MAX_TAG_KEY_CHARS);
        for (key, value) in [
            ("env", "prod"),
            ("team name", "data eng"),
            ("cost-center", "a+b=c.d_e:f/g@h"),
            ("ünï", "valeur"),
            ("empty", ""),
            ("k", longest_value.as_str()),
            (longest_key.as_str(), "v"),
        ] {
            validate_tag(key, value).unwrap_or_else(|err| panic!("{key}={value}: {err}"));
        }
        let too_long_value = "v".repeat(MAX_TAG_VALUE_CHARS + 1);
        let too_long_key = "k".repeat(MAX_TAG_KEY_CHARS + 1);
        for (key, value) in [
            ("", "v"),
            ("k", too_long_value.as_str()),
            (too_long_key.as_str(), "v"),
            ("bad,key", "v"),
            ("k", "semi;colon"),
            ("k", "new\nline"),
            ("k", "#hash"),
            ("aws:cloudformation:stack", "v"),
            ("AWS:Reserved", "v"),
        ] {
            assert_eq!(
                validate_tag(key, value).unwrap_err().kind(),
                ErrorKind::Usage,
                "{key}={value}"
            );
        }
    }

    #[test]
    fn at_most_ten_tags() {
        let ten: BTreeMap<String, String> =
            (0..10).map(|i| (format!("k{i}"), "v".to_owned())).collect();
        assert!(validate_tags(&ten).is_ok());
        let mut eleven = ten;
        eleven.insert("k10".into(), "v".into());
        let err = validate_tags(&eleven).unwrap_err();
        assert!(err.to_string().contains("11 tags"), "{err}");
    }

    #[test]
    fn replace_sets_the_whole_set() {
        let edit = TagEdit::Replace {
            tags: tags(&[("a", "1")]),
        };
        edit.validate().unwrap();
        assert_eq!(
            edit.apply(&tags(&[("old", "x")])).unwrap(),
            tags(&[("a", "1")])
        );
        let clear = TagEdit::Replace {
            tags: BTreeMap::new(),
        };
        assert!(clear.apply(&tags(&[("old", "x")])).unwrap().is_empty());
    }

    #[test]
    fn merge_adds_overwrites_and_removes() {
        let edit = TagEdit::Merge {
            set: tags(&[("env", "prod"), ("new", "1")]),
            remove: BTreeSet::from(["temp".to_owned(), "absent".to_owned()]),
        };
        edit.validate().unwrap();
        let current = tags(&[("env", "dev"), ("temp", "x"), ("keep", "k")]);
        assert_eq!(
            edit.apply(&current).unwrap(),
            tags(&[("env", "prod"), ("new", "1"), ("keep", "k")])
        );
    }

    #[test]
    fn a_merge_beyond_ten_tags_is_refused_for_that_object() {
        let current: BTreeMap<String, String> =
            (0..9).map(|i| (format!("k{i}"), "v".to_owned())).collect();
        let edit = TagEdit::Merge {
            set: tags(&[("a", "1"), ("b", "2")]),
            remove: BTreeSet::new(),
        };
        edit.validate().unwrap();
        assert_eq!(edit.apply(&current).unwrap_err().kind(), ErrorKind::Usage);
    }

    #[test]
    fn invalid_edits_are_usage_errors() {
        let bad_value = TagEdit::Replace {
            tags: tags(&[("k", "no;good")]),
        };
        assert_eq!(bad_value.validate().unwrap_err().kind(), ErrorKind::Usage);
        let both = TagEdit::Merge {
            set: tags(&[("k", "v")]),
            remove: BTreeSet::from(["k".to_owned()]),
        };
        assert!(both.validate().unwrap_err().to_string().contains("both"));
        let bad_remove = TagEdit::Merge {
            set: BTreeMap::new(),
            remove: BTreeSet::from(["aws:x".to_owned()]),
        };
        assert!(bad_remove.validate().is_err());
        let too_many = TagEdit::Merge {
            set: (0..11).map(|i| (format!("k{i}"), "v".to_owned())).collect(),
            remove: BTreeSet::new(),
        };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn the_json_shape_is_tagged_by_mode() {
        let replace: TagEdit =
            serde_json::from_value(json!({ "mode": "replace", "tags": { "a": "1" } })).unwrap();
        assert_eq!(
            replace,
            TagEdit::Replace {
                tags: tags(&[("a", "1")])
            }
        );
        let merge: TagEdit = serde_json::from_value(
            json!({ "mode": "merge", "set": { "a": "1" }, "remove": ["b", "c"] }),
        )
        .unwrap();
        assert_eq!(
            merge,
            TagEdit::Merge {
                set: tags(&[("a", "1")]),
                remove: BTreeSet::from(["b".to_owned(), "c".to_owned()])
            }
        );
        // `set` and `remove` may be left out.
        let bare: TagEdit = serde_json::from_value(json!({ "mode": "merge" })).unwrap();
        assert_eq!(
            bare,
            TagEdit::Merge {
                set: BTreeMap::new(),
                remove: BTreeSet::new()
            }
        );
        assert!(serde_json::from_value::<TagEdit>(json!({ "mode": "append" })).is_err());
        assert_eq!(
            serde_json::to_value(&replace).unwrap(),
            json!({ "mode": "replace", "tags": { "a": "1" } })
        );
    }
}
