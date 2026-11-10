//! Cloning a bucket: a new bucket with the chosen settings of an existing one,
//! and the copy of its objects through the transfer queue.
//!
//! * [`S3::preview_bucket_clone`] reads the eight settings a clone can copy
//!   (policy, CORS, lifecycle, versioning, tags, default encryption, website,
//!   public access block) and says for each one whether the source has it, so
//!   the user sees what will be copied before anything is created.
//! * [`S3::create_bucket_clone`] reads the chosen settings from the source,
//!   creates the new bucket (in the region asked for, else the one of the
//!   source) and applies them. A setting that cannot be read or applied is
//!   reported on its own and never stops the others. A bucket that already
//!   exists is a `conflict` and nothing is changed.
//! * [`crate::engine::enqueue_clone_copy`] queues the server-side copy of the
//!   objects (all of them, or those under a prefix) with their keys unchanged.
//!
//! The settings are applied in an order that lets them take effect: the public
//! access block of the source first (a new bucket blocks public access by
//! default, which would refuse a public policy), then the policy, the rest
//! after it. A policy names its bucket in resource ARNs; the name of the
//! source is replaced by the name of the new bucket. Settings are applied
//! before the objects are copied, so a policy that denies the copying
//! principal blocks the copy ([`ClonePreviewWarning::PolicyDenies`]).

use aws_sdk_s3::types::{
    ServerSideEncryption, ServerSideEncryptionByDefault, ServerSideEncryptionConfiguration,
    ServerSideEncryptionRule, Tag, Tagging,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bucketcfg::{
    BucketVersioning, PublicAccessBlock, VersioningStatus, WebsiteConfig, build_error, context,
    is_unsupported, read_optional,
};
use crate::bucketrules::{CorsConfig, LifecycleConfig};
use crate::error::{BbError, Error, Result};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// A setting a clone can copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneSetting {
    /// The bucket policy.
    Policy,
    /// The CORS rules.
    Cors,
    /// The lifecycle rules.
    Lifecycle,
    /// Versioning (enabled or suspended).
    Versioning,
    /// The bucket tags (cost allocation).
    Tags,
    /// The default encryption.
    Encryption,
    /// Static website hosting.
    Website,
    /// The public access block.
    PublicAccessBlock,
}

impl CloneSetting {
    /// Every setting, in the order they are listed and applied.
    pub const ALL: [CloneSetting; 8] = [
        CloneSetting::Versioning,
        CloneSetting::Encryption,
        CloneSetting::PublicAccessBlock,
        CloneSetting::Policy,
        CloneSetting::Website,
        CloneSetting::Cors,
        CloneSetting::Tags,
        CloneSetting::Lifecycle,
    ];

    /// The `snake_case` name (the IPC and CLI spelling).
    pub fn as_str(self) -> &'static str {
        match self {
            CloneSetting::Policy => "policy",
            CloneSetting::Cors => "cors",
            CloneSetting::Lifecycle => "lifecycle",
            CloneSetting::Versioning => "versioning",
            CloneSetting::Tags => "tags",
            CloneSetting::Encryption => "encryption",
            CloneSetting::Website => "website",
            CloneSetting::PublicAccessBlock => "public_access_block",
        }
    }
}

impl std::str::FromStr for CloneSetting {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        CloneSetting::ALL
            .into_iter()
            .find(|setting| setting.as_str() == text)
            .ok_or_else(|| {
                Error::Usage(format!(
                    "unknown setting '{text}'; use one of {}",
                    CloneSetting::ALL.map(CloneSetting::as_str).join(", ")
                ))
            })
    }
}

/// What the source bucket has for one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingState {
    /// The source has it; a clone copies it.
    Configured,
    /// The source has nothing configured.
    NotConfigured,
    /// The server does not implement this setting.
    Unsupported,
    /// Reading it failed (access denied, a network failure).
    Failed,
}

/// One setting of the source as the preview shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingPreview {
    /// Which setting.
    pub setting: CloneSetting,
    /// Whether the source has it.
    pub state: SettingState,
    /// How many items it holds: policy statements, CORS rules, lifecycle
    /// rules, tags; `None` where a count means nothing.
    pub count: Option<u32>,
    /// A short technical value: the versioning status, the encryption
    /// algorithm, the website's index document; `None` where there is none.
    pub detail: Option<String>,
    /// Why it was not read, for `unsupported` and `failed`.
    pub error: Option<BbError>,
}

/// Something to know before cloning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClonePreviewWarning {
    /// The policy has `Deny` statements. Settings are applied before the
    /// objects are copied, so such a policy can block the copy.
    PolicyDenies,
    /// The default encryption uses a KMS key; the key must exist in the
    /// region of the new bucket (or be a multi-region key).
    KmsKey,
    /// Versioning is on in the source: the clone copies the current version
    /// of every object, not the older versions.
    VersionsNotCopied,
}

/// What a clone of a bucket would copy ([`S3::preview_bucket_clone`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClonePreview {
    /// The source bucket.
    pub source_bucket: String,
    /// The region of the source bucket; `None` when the server does not say.
    pub region: Option<String>,
    /// The eight settings, in [`CloneSetting::ALL`] order.
    pub settings: Vec<SettingPreview>,
    /// Things to know.
    pub warnings: Vec<ClonePreviewWarning>,
}

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneRequest {
    /// The bucket to take the settings from.
    pub source_bucket: String,
    /// The new bucket.
    pub new_bucket: String,
    /// Where to create it; the region of the source when `None`.
    pub region: Option<String>,
    /// The settings to copy.
    pub settings: Vec<CloneSetting>,
}

/// What became of one setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyState {
    /// Copied to the new bucket.
    Applied,
    /// The source has nothing configured, so there was nothing to copy.
    Skipped,
    /// Reading it from the source or applying it failed (see `error`).
    Failed,
}

/// The result for one setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingResult {
    /// Which setting.
    pub setting: CloneSetting,
    /// What became of it.
    pub state: ApplyState,
    /// Why it failed.
    pub error: Option<BbError>,
}

/// The new bucket and what was copied to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneOutcome {
    /// The new bucket.
    pub bucket: String,
    /// The region it was created in; `None` for the account region of a
    /// server that has no regions.
    pub region: Option<String>,
    /// One result per requested setting, in application order.
    pub results: Vec<SettingResult>,
}

/// The default encryption of a bucket, reduced to what a clone copies.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Encryption {
    algorithm: String,
    kms_key_id: Option<String>,
    bucket_key: Option<bool>,
}

/// A setting read from the source.
enum Data {
    Policy(Option<String>),
    Cors(Option<CorsConfig>),
    Lifecycle(Option<LifecycleConfig>),
    Versioning(BucketVersioning),
    Tags(Vec<(String, String)>),
    Encryption(Option<Encryption>),
    Website(Option<WebsiteConfig>),
    PublicAccessBlock(Option<PublicAccessBlock>),
}

impl S3 {
    /// Reads what a clone of `bucket` would copy. Every setting is read on
    /// its own: one that is refused or not implemented never hides the
    /// others. Fails as a whole only when the bucket cannot be reached at all.
    pub async fn preview_bucket_clone(&self, bucket: &str) -> Result<ClonePreview> {
        validate_bucket_name(bucket)?;
        let region = self.clone_source_region(bucket).await?;
        let reads = futures::future::join_all(
            CloneSetting::ALL
                .into_iter()
                .map(|setting| self.read_clone_setting(bucket, setting)),
        )
        .await;
        let mut warnings = Vec::new();
        let mut settings = Vec::new();
        for (setting, read) in CloneSetting::ALL.into_iter().zip(reads) {
            let preview = match read {
                Ok(data) => {
                    if let Some(warning) = warning_of(&data) {
                        warnings.push(warning);
                    }
                    describe(setting, &data)
                }
                Err(error) => SettingPreview {
                    setting,
                    state: if is_unsupported(&error) {
                        SettingState::Unsupported
                    } else {
                        SettingState::Failed
                    },
                    count: None,
                    detail: None,
                    error: Some(error.to_payload()),
                },
            };
            settings.push(preview);
        }
        warnings.sort_by_key(|warning| *warning as u8);
        Ok(ClonePreview {
            source_bucket: bucket.to_owned(),
            region,
            settings,
            warnings,
        })
    }

    /// Creates `request.new_bucket` and copies the chosen settings of
    /// `request.source_bucket` to it (see the [module documentation](self)).
    ///
    /// The settings are read before anything is created, so a source that
    /// cannot be reached creates nothing. A bucket that exists is a
    /// `conflict`. After the bucket exists every setting reports its own
    /// result.
    pub async fn create_bucket_clone(&self, request: &CloneRequest) -> Result<CloneOutcome> {
        validate_bucket_name(&request.source_bucket)?;
        if request.new_bucket == request.source_bucket {
            return Err(Error::Usage(
                "the new bucket must have a different name than the source".into(),
            ));
        }
        let mut wanted: Vec<CloneSetting> = CloneSetting::ALL
            .into_iter()
            .filter(|setting| request.settings.contains(setting))
            .collect();
        wanted.dedup();
        let region = match &request.region {
            Some(region) => Some(region.clone()),
            None => self.clone_source_region(&request.source_bucket).await?,
        };
        let reads = futures::future::join_all(
            wanted
                .iter()
                .map(|setting| self.read_clone_setting(&request.source_bucket, *setting)),
        )
        .await;
        self.create_bucket(&request.new_bucket, region.as_deref())
            .await?;
        let mut results = Vec::with_capacity(wanted.len());
        for (setting, read) in wanted.into_iter().zip(reads) {
            let applied = match read {
                Ok(data) => {
                    self.apply_clone_setting(&request.source_bucket, &request.new_bucket, data)
                        .await
                }
                Err(error) => Err(error),
            };
            results.push(match applied {
                Ok(true) => SettingResult {
                    setting,
                    state: ApplyState::Applied,
                    error: None,
                },
                Ok(false) => SettingResult {
                    setting,
                    state: ApplyState::Skipped,
                    error: None,
                },
                Err(error) => SettingResult {
                    setting,
                    state: ApplyState::Failed,
                    error: Some(error.to_payload()),
                },
            });
        }
        Ok(CloneOutcome {
            bucket: request.new_bucket.clone(),
            region,
            results,
        })
    }

    /// The region a bucket's requests go to: its own region on AWS, the
    /// account region elsewhere (`None` when the client has none).
    async fn clone_source_region(&self, bucket: &str) -> Result<Option<String>> {
        let client = self.client_for(bucket).await?;
        Ok(client.config().region().map(|r| r.as_ref().to_owned()))
    }

    async fn read_clone_setting(&self, bucket: &str, setting: CloneSetting) -> Result<Data> {
        Ok(match setting {
            CloneSetting::Policy => Data::Policy(self.get_bucket_policy(bucket).await?),
            CloneSetting::Cors => Data::Cors(self.get_bucket_cors(bucket).await?),
            CloneSetting::Lifecycle => Data::Lifecycle(self.get_bucket_lifecycle(bucket).await?),
            CloneSetting::Versioning => Data::Versioning(self.get_bucket_versioning(bucket).await?),
            CloneSetting::Tags => Data::Tags(self.read_clone_tags(bucket).await?),
            CloneSetting::Encryption => Data::Encryption(self.read_clone_encryption(bucket).await?),
            CloneSetting::Website => Data::Website(self.get_bucket_website(bucket).await?),
            CloneSetting::PublicAccessBlock => {
                Data::PublicAccessBlock(self.get_public_access_block(bucket).await?)
            }
        })
    }

    /// Applies one setting read from `source` to `new`; `Ok(false)` when the
    /// source had nothing configured.
    async fn apply_clone_setting(&self, source: &str, new: &str, data: Data) -> Result<bool> {
        match data {
            Data::Policy(None) | Data::Cors(None) | Data::Lifecycle(None) | Data::Website(None) => {
                Ok(false)
            }
            Data::Encryption(None) => Ok(false),
            Data::Versioning(versioning) => match versioning.status {
                VersioningStatus::Unversioned => Ok(false),
                status => self.put_bucket_versioning(new, status).await.map(|()| true),
            },
            Data::Tags(tags) if tags.is_empty() => Ok(false),
            Data::Policy(Some(policy)) => {
                let policy = rename_bucket_in_policy(&policy, source, new);
                self.put_bucket_policy(new, &policy).await.map(|()| true)
            }
            Data::Cors(Some(cors)) => self.put_bucket_cors(new, &cors).await.map(|()| true),
            Data::Lifecycle(Some(lifecycle)) => self
                .put_bucket_lifecycle(new, &lifecycle)
                .await
                .map(|()| true),
            Data::Website(Some(website)) => {
                self.put_bucket_website(new, &website).await.map(|()| true)
            }
            Data::Tags(tags) => self.write_clone_tags(new, &tags).await.map(|()| true),
            Data::Encryption(Some(encryption)) => self
                .write_clone_encryption(new, &encryption)
                .await
                .map(|()| true),
            Data::PublicAccessBlock(Some(block)) => self
                .put_public_access_block(new, &block)
                .await
                .map(|()| true),
            // A bucket the source did not block: remove the block a new
            // bucket starts with, so that the clone behaves like the source
            // (a public policy needs it).
            Data::PublicAccessBlock(None) => {
                self.delete_public_access_block(new).await.map(|()| true)
            }
        }
    }

    async fn read_clone_tags(&self, bucket: &str) -> Result<Vec<(String, String)>> {
        let client = self.client_for(bucket).await?;
        let reply = client
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

    async fn write_clone_tags(&self, bucket: &str, tags: &[(String, String)]) -> Result<()> {
        let set = tags
            .iter()
            .map(|(key, value)| Tag::builder().key(key).value(value).build())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(build_error)?;
        let tagging = Tagging::builder()
            .set_tag_set(Some(set))
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

    async fn read_clone_encryption(&self, bucket: &str) -> Result<Option<Encryption>> {
        let client = self.client_for(bucket).await?;
        let reply = client
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
        Ok(out
            .and_then(|out| {
                out.server_side_encryption_configuration()
                    .and_then(|config| config.rules().first().cloned())
            })
            .and_then(|rule| {
                let default = rule.apply_server_side_encryption_by_default()?;
                Some(Encryption {
                    algorithm: default.sse_algorithm().as_str().to_owned(),
                    kms_key_id: default.kms_master_key_id().map(str::to_owned),
                    bucket_key: rule.bucket_key_enabled(),
                })
            }))
    }

    async fn write_clone_encryption(&self, bucket: &str, encryption: &Encryption) -> Result<()> {
        let default = ServerSideEncryptionByDefault::builder()
            .sse_algorithm(ServerSideEncryption::from(encryption.algorithm.as_str()))
            .set_kms_master_key_id(encryption.kms_key_id.clone())
            .build()
            .map_err(build_error)?;
        let rule = ServerSideEncryptionRule::builder()
            .apply_server_side_encryption_by_default(default)
            .set_bucket_key_enabled(encryption.bucket_key)
            .build();
        let config = ServerSideEncryptionConfiguration::builder()
            .rules(rule)
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
}

/// The preview line of a setting that was read.
fn describe(setting: CloneSetting, data: &Data) -> SettingPreview {
    let (configured, count, detail) = match data {
        Data::Policy(policy) => (policy.is_some(), policy.as_deref().map(statements_in), None),
        Data::Cors(cors) => (
            cors.is_some(),
            cors.as_ref().map(|c| count(c.rules.len())),
            None,
        ),
        Data::Lifecycle(lifecycle) => (
            lifecycle.is_some(),
            lifecycle.as_ref().map(|l| count(l.rules.len())),
            None,
        ),
        Data::Versioning(versioning) => (
            versioning.status != VersioningStatus::Unversioned,
            None,
            match versioning.status {
                VersioningStatus::Enabled => Some("enabled".to_owned()),
                VersioningStatus::Suspended => Some("suspended".to_owned()),
                VersioningStatus::Unversioned => None,
            },
        ),
        Data::Tags(tags) => (!tags.is_empty(), Some(count(tags.len())), None),
        Data::Encryption(encryption) => (
            encryption.is_some(),
            None,
            encryption.as_ref().map(|e| e.algorithm.clone()),
        ),
        Data::Website(website) => (
            website.is_some(),
            None,
            website.as_ref().map(|website| match website {
                WebsiteConfig::Hosting { index_document, .. } => index_document.clone(),
                WebsiteConfig::Redirect { host_name, .. } => host_name.clone(),
            }),
        ),
        Data::PublicAccessBlock(block) => (
            block.is_some(),
            block.as_ref().map(|b| {
                count(
                    [
                        b.block_public_acls,
                        b.ignore_public_acls,
                        b.block_public_policy,
                        b.restrict_public_buckets,
                    ]
                    .into_iter()
                    .filter(|on| *on)
                    .count(),
                )
            }),
            None,
        ),
    };
    SettingPreview {
        setting,
        state: if configured {
            SettingState::Configured
        } else {
            SettingState::NotConfigured
        },
        count: if configured { count } else { None },
        detail,
        error: None,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn statements_in(policy: &str) -> u32 {
    serde_json::from_str::<Value>(policy)
        .ok()
        .and_then(|doc| match doc.get("Statement") {
            Some(Value::Array(items)) => Some(count(items.len())),
            Some(Value::Object(_)) => Some(1),
            _ => None,
        })
        .unwrap_or(0)
}

fn warning_of(data: &Data) -> Option<ClonePreviewWarning> {
    match data {
        Data::Policy(Some(policy)) if policy_denies(policy) => {
            Some(ClonePreviewWarning::PolicyDenies)
        }
        Data::Encryption(Some(encryption)) if encryption.kms_key_id.is_some() => {
            Some(ClonePreviewWarning::KmsKey)
        }
        Data::Versioning(versioning) if versioning.status == VersioningStatus::Enabled => {
            Some(ClonePreviewWarning::VersionsNotCopied)
        }
        _ => None,
    }
}

/// Whether a policy document has a statement with `"Effect": "Deny"`.
fn policy_denies(policy: &str) -> bool {
    let Ok(doc) = serde_json::from_str::<Value>(policy) else {
        return false;
    };
    let denies =
        |statement: &Value| statement.get("Effect").and_then(Value::as_str) == Some("Deny");
    match doc.get("Statement") {
        Some(Value::Array(items)) => items.iter().any(denies),
        Some(statement @ Value::Object(_)) => denies(statement),
        _ => false,
    }
}

/// `policy` with the bucket `from` named in resource ARNs (`arn:aws:s3:::from`
/// and `arn:aws:s3:::from/...`, in any partition) renamed to `to`. Text that
/// only starts with the name (`from-logs`) is left alone. A document that is
/// not JSON is returned unchanged.
pub fn rename_bucket_in_policy(policy: &str, from: &str, to: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<Value>(policy) else {
        return policy.to_owned();
    };
    rename_in_value(&mut doc, from, to);
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| policy.to_owned())
}

fn rename_in_value(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) => *text = rename_in_text(text, from, to),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| rename_in_value(item, from, to)),
        Value::Object(map) => map
            .values_mut()
            .for_each(|item| rename_in_value(item, from, to)),
        _ => {}
    }
}

fn rename_in_text(text: &str, from: &str, to: &str) -> String {
    let needle = format!(":s3:::{from}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&needle) {
        let end = at + needle.len();
        let boundary = rest[end..]
            .chars()
            .next()
            .is_none_or(|next| next == '/' || next == '*');
        out.push_str(&rest[..at]);
        out.push_str(":s3:::");
        out.push_str(if boundary { to } else { from });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_names_round_trip() {
        for setting in CloneSetting::ALL {
            assert_eq!(setting.as_str().parse::<CloneSetting>().unwrap(), setting);
            assert_eq!(
                serde_json::to_value(setting).unwrap(),
                serde_json::json!(setting.as_str())
            );
        }
        assert!("nonsense".parse::<CloneSetting>().is_err());
    }

    #[test]
    fn a_policy_names_the_new_bucket() {
        let policy = r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Principal":"*","Action":"s3:GetObject",
             "Resource":["arn:aws:s3:::src/*","arn:aws:s3:::src","arn:aws:s3:::src-logs/*","arn:aws-cn:s3:::src/x"]},
            {"Effect":"Deny","Principal":"*","Action":"s3:*","Resource":"arn:aws:s3:::src/private/*",
             "Condition":{"StringEquals":{"aws:SourceArn":"arn:aws:s3:::src"}}}]}"#;
        let renamed = rename_bucket_in_policy(policy, "src", "dst");
        let doc: Value = serde_json::from_str(&renamed).unwrap();
        assert_eq!(
            doc["Statement"][0]["Resource"],
            serde_json::json!([
                "arn:aws:s3:::dst/*",
                "arn:aws:s3:::dst",
                "arn:aws:s3:::src-logs/*",
                "arn:aws-cn:s3:::dst/x"
            ])
        );
        assert_eq!(
            doc["Statement"][1]["Resource"],
            "arn:aws:s3:::dst/private/*"
        );
        assert_eq!(
            doc["Statement"][1]["Condition"]["StringEquals"]["aws:SourceArn"],
            "arn:aws:s3:::dst"
        );
        assert!(policy_denies(policy));
        assert_eq!(statements_in(policy), 2);
        // Not JSON: untouched.
        assert_eq!(rename_bucket_in_policy("not json", "a", "b"), "not json");
    }

    #[test]
    fn a_preview_line_counts_what_the_source_has() {
        let versioning = describe(
            CloneSetting::Versioning,
            &Data::Versioning(BucketVersioning {
                status: VersioningStatus::Suspended,
                mfa_delete: false,
            }),
        );
        assert_eq!(versioning.state, SettingState::Configured);
        assert_eq!(versioning.detail.as_deref(), Some("suspended"));
        let none = describe(
            CloneSetting::Versioning,
            &Data::Versioning(BucketVersioning::default()),
        );
        assert_eq!(none.state, SettingState::NotConfigured);
        let tags = describe(
            CloneSetting::Tags,
            &Data::Tags(vec![("a".into(), "b".into())]),
        );
        assert_eq!(
            (tags.state, tags.count),
            (SettingState::Configured, Some(1))
        );
        let block = describe(
            CloneSetting::PublicAccessBlock,
            &Data::PublicAccessBlock(Some(PublicAccessBlock {
                block_public_acls: true,
                block_public_policy: true,
                ..PublicAccessBlock::default()
            })),
        );
        assert_eq!(block.count, Some(2));
        let kms = Data::Encryption(Some(Encryption {
            algorithm: "aws:kms".into(),
            kms_key_id: Some("k".into()),
            bucket_key: Some(true),
        }));
        assert_eq!(warning_of(&kms), Some(ClonePreviewWarning::KmsKey));
        assert_eq!(
            describe(CloneSetting::Encryption, &kms).detail.as_deref(),
            Some("aws:kms")
        );
    }
}
