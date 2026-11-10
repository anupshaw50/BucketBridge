//! Default encryption and tags of a bucket (cost allocation tags), read and
//! written through [`S3`].
//!
//! A getter returns `None` (encryption) or an empty set (tags) when the
//! bucket has nothing configured. Everything that can be checked without the
//! server is checked before a request is sent.

use std::collections::BTreeMap;

use aws_sdk_s3::types::{
    ServerSideEncryption, ServerSideEncryptionByDefault, ServerSideEncryptionConfiguration,
    ServerSideEncryptionRule, Tag, Tagging,
};
use serde::{Deserialize, Serialize};

use crate::bucketcfg::{build_error, context, delete_optional, read_optional};
use crate::error::{Error, Result};
use crate::objectmeta::{EncryptionChange, validate_tag};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// Most tags a bucket can have (objects: 10).
pub const MAX_BUCKET_TAGS: usize = 50;

/// The algorithm of a bucket's default encryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BucketSse {
    /// SSE-S3: keys managed by S3.
    #[serde(rename = "AES256")]
    Aes256,
    /// SSE-KMS.
    #[serde(rename = "aws:kms")]
    Kms,
    /// Dual-layer SSE-KMS. Shown when a bucket has it; not offered for new
    /// settings.
    #[serde(rename = "aws:kms:dsse")]
    KmsDsse,
}

/// The default encryption of a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketEncryption {
    /// How new objects are encrypted.
    pub algorithm: BucketSse,
    /// KMS key ID, key ARN, alias name or alias ARN. `None` for SSE-S3, and
    /// for SSE-KMS with the AWS managed key `aws/s3`.
    pub kms_key_id: Option<String>,
    /// Whether an S3 Bucket Key reduces the calls to KMS (SSE-KMS only).
    pub bucket_key_enabled: bool,
}

impl BucketEncryption {
    /// Checks the setting without asking S3: only SSE-S3 and SSE-KMS can be
    /// set; a key and a bucket key belong to SSE-KMS; the key is a key ID,
    /// ARN or alias.
    pub fn validate(&self) -> Result<()> {
        match self.algorithm {
            BucketSse::Aes256 => {
                if self.kms_key_id.is_some() {
                    return Err(Error::Usage(
                        "a KMS key can only be given with SSE-KMS".into(),
                    ));
                }
                if self.bucket_key_enabled {
                    return Err(Error::Usage(
                        "a bucket key can only be used with SSE-KMS".into(),
                    ));
                }
                Ok(())
            }
            BucketSse::Kms => EncryptionChange::Kms {
                key_id: self.kms_key_id.clone(),
                bucket_key: self.bucket_key_enabled,
            }
            .validate(),
            BucketSse::KmsDsse => Err(Error::Usage(
                "dual-layer SSE-KMS cannot be set here; use SSE-S3 or SSE-KMS".into(),
            )),
        }
    }
}

/// Checks a bucket tag set: at most [`MAX_BUCKET_TAGS`] tags, each valid
/// (keys of 1 to 128 and values of up to 256 characters, no `aws:` prefix).
pub fn validate_bucket_tags(tags: &BTreeMap<String, String>) -> Result<()> {
    if tags.len() > MAX_BUCKET_TAGS {
        return Err(Error::Usage(format!(
            "{} tags are too many; a bucket can have at most {MAX_BUCKET_TAGS}",
            tags.len()
        )));
    }
    for (key, value) in tags {
        validate_tag(key, value)?;
    }
    Ok(())
}

impl S3 {
    /// The default encryption of the bucket; `None` when none is configured
    /// (new objects are then encrypted with SSE-S3 on AWS).
    pub async fn get_bucket_encryption(&self, bucket: &str) -> Result<Option<BucketEncryption>> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .get_bucket_encryption()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "ServerSideEncryptionConfigurationNotFoundError",
            context("reading the default encryption", bucket),
        )?;
        Ok(out.and_then(|out| {
            let rule = out
                .server_side_encryption_configuration()?
                .rules()
                .first()?
                .clone();
            let default = rule.apply_server_side_encryption_by_default()?;
            let algorithm = match default.sse_algorithm() {
                ServerSideEncryption::Aes256 => BucketSse::Aes256,
                ServerSideEncryption::AwsKms => BucketSse::Kms,
                ServerSideEncryption::AwsKmsDsse => BucketSse::KmsDsse,
                _ => return None,
            };
            Some(BucketEncryption {
                algorithm,
                kms_key_id: default.kms_master_key_id().map(str::to_owned),
                bucket_key_enabled: rule.bucket_key_enabled().unwrap_or(false),
            })
        }))
    }

    /// Sets the default encryption of the bucket.
    pub async fn put_bucket_encryption(
        &self,
        bucket: &str,
        encryption: &BucketEncryption,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        encryption.validate()?;
        let sdk = match encryption.algorithm {
            BucketSse::Aes256 => ServerSideEncryption::Aes256,
            _ => ServerSideEncryption::AwsKms,
        };
        let default = ServerSideEncryptionByDefault::builder()
            .sse_algorithm(sdk)
            .set_kms_master_key_id(encryption.kms_key_id.clone())
            .build()
            .map_err(build_error)?;
        let mut rule =
            ServerSideEncryptionRule::builder().apply_server_side_encryption_by_default(default);
        if encryption.algorithm == BucketSse::Kms {
            rule = rule.bucket_key_enabled(encryption.bucket_key_enabled);
        }
        let config = ServerSideEncryptionConfiguration::builder()
            .rules(rule.build())
            .build()
            .map_err(build_error)?;
        self.client_for(bucket)
            .await?
            .put_bucket_encryption()
            .bucket(bucket)
            .server_side_encryption_configuration(config)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the default encryption", bucket))
            })?;
        Ok(())
    }

    /// Removes the default encryption setting; succeeds when there is none.
    /// New objects then get the encryption S3 applies on its own (SSE-S3 on
    /// AWS).
    pub async fn delete_bucket_encryption(&self, bucket: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .delete_bucket_encryption()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "ServerSideEncryptionConfigurationNotFoundError",
            context("removing the default encryption", bucket),
        )
    }

    /// The tags of the bucket; empty when it has none.
    pub async fn get_bucket_tags(&self, bucket: &str) -> Result<BTreeMap<String, String>> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .get_bucket_tagging()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(reply, "NoSuchTagSet", context("reading the tags", bucket))?;
        Ok(out
            .map(|out| {
                out.tag_set()
                    .iter()
                    .map(|tag| (tag.key().to_owned(), tag.value().to_owned()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Replaces the tags of the bucket. An empty set removes them.
    pub async fn put_bucket_tags(
        &self,
        bucket: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_bucket_tags(tags)?;
        if tags.is_empty() {
            return self.delete_bucket_tags(bucket).await;
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
            .put_bucket_tagging()
            .bucket(bucket)
            .tagging(tagging)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| Error::from(error).with_context(context("saving the tags", bucket)))?;
        Ok(())
    }

    /// Removes every tag of the bucket; succeeds when there are none.
    pub async fn delete_bucket_tags(&self, bucket: &str) -> Result<()> {
        validate_bucket_name(bucket)?;
        let reply = self
            .client_for(bucket)
            .await?
            .delete_bucket_tagging()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(reply, "NoSuchTagSet", context("removing the tags", bucket))
    }
}
