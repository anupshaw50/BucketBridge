//! Bucket rules: lifecycle, Intelligent-Tiering, CORS (T07) and cross-region
//! replication (T04), read and written through [`S3`].
//!
//! # Models and validation
//!
//! Each setting has a plain model ([`LifecycleConfig`], [`TieringConfig`],
//! [`CorsConfig`], [`ReplicationConfig`]) that serializes to the JSON the
//! editors show (unknown fields are rejected, so a typo is reported instead
//! of ignored) and a `check` method that lists every problem with its path
//! (`rules[1].transitions[0].days`) as [`RuleIssue`]s. `validate` turns the
//! list into one `usage` error. [`S3`] validates before it sends anything
//! (`put_*`), so an invalid document never leaves the machine.
//!
//! # Sections
//!
//! [`S3::get_bucket_rules`] reads the four settings at once and reports each
//! one on its own as a [`Section`], like
//! [`S3::get_bucket_config`](crate::bucketcfg), so a server that does not
//! implement one of them (or refuses it) never hides the others. A getter
//! returns `None` (or an empty list) when nothing is configured.
//!
//! # Replication
//!
//! Replication needs versioning on the source bucket;
//! [`S3::put_bucket_replication`] reads the versioning state first and
//! refuses with a clear `usage` error when it is not enabled. The model
//! covers the common rule fields; the settings it does not carry (replica
//! modification sync, KMS source selection, encryption of replicas,
//! replication time control, metrics, ACL translation, existing object
//! replication) are copied from the rule with the same id that the bucket has
//! now, so editing a rule never resets them.

use std::collections::BTreeSet;

use aws_sdk_s3::types::{
    AbortIncompleteMultipartUpload, BucketLifecycleConfiguration, CorsConfiguration,
    CorsRule as SdkCorsRule, DeleteMarkerReplication, DeleteMarkerReplicationStatus, Destination,
    ExpirationStatus, IntelligentTieringAccessTier, IntelligentTieringAndOperator,
    IntelligentTieringConfiguration, IntelligentTieringFilter, IntelligentTieringStatus,
    LifecycleExpiration, LifecycleRule as SdkLifecycleRule, LifecycleRuleAndOperator,
    LifecycleRuleFilter, NoncurrentVersionExpiration, NoncurrentVersionTransition,
    ReplicationConfiguration, ReplicationRule as SdkReplicationRule, ReplicationRuleAndOperator,
    ReplicationRuleFilter, ReplicationRuleStatus, StorageClass, Tag as SdkTag, Tiering, Transition,
    TransitionStorageClass,
};
use aws_smithy_types::DateTime;
use aws_smithy_types::date_time::Format;
use serde::{Deserialize, Serialize};

use crate::bucketcfg::{
    Section, VersioningStatus, build_error, context, delete_optional, read_optional,
};
use crate::error::{Error, Result};
use crate::ops::S3;
use crate::s3uri::{MAX_KEY_BYTES, validate_bucket_name};

/// Most lifecycle rules (and replication rules) one configuration may have.
pub const MAX_RULES: usize = 1000;

/// Most CORS rules one configuration may have.
pub const MAX_CORS_RULES: usize = 100;

/// Longest rule id S3 accepts, in characters.
pub const MAX_RULE_ID_CHARS: usize = 255;

/// Longest Intelligent-Tiering configuration id, in characters.
pub const MAX_TIERING_ID_CHARS: usize = 64;

/// Most tags a filter may have.
pub const MAX_FILTER_TAGS: usize = 10;

/// The storage classes an object can transition to.
pub const TRANSITION_CLASSES: [&str; 6] = [
    "STANDARD_IA",
    "ONEZONE_IA",
    "INTELLIGENT_TIERING",
    "GLACIER_IR",
    "GLACIER",
    "DEEP_ARCHIVE",
];

/// The storage classes a replica can be stored in.
pub const REPLICA_CLASSES: [&str; 8] = [
    "STANDARD",
    "REDUCED_REDUNDANCY",
    "STANDARD_IA",
    "ONEZONE_IA",
    "INTELLIGENT_TIERING",
    "GLACIER",
    "GLACIER_IR",
    "DEEP_ARCHIVE",
];

/// The HTTP methods a CORS rule may allow.
pub const CORS_METHODS: [&str; 5] = ["GET", "PUT", "POST", "DELETE", "HEAD"];

// ---- shared model ----

/// One problem found in a rule document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleIssue {
    /// Where the problem is, such as `rules[0].expiration.days`; empty for
    /// the document as a whole.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl RuleIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        RuleIssue {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RuleIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Turns a list of issues into one `usage` error (`Ok` when it is empty).
fn issues_to_result(what: &str, issues: &[RuleIssue]) -> Result<()> {
    if issues.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = issues.iter().map(RuleIssue::to_string).collect();
    Err(Error::Usage(format!("invalid {what}: {}", list.join("; "))))
}

/// Whether a rule is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    /// The rule is applied.
    #[default]
    Enabled,
    /// The rule is kept but not applied.
    Disabled,
}

/// An object tag used in a filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    /// The tag key (1 to 128 characters).
    pub key: String,
    /// The tag value (up to 256 characters).
    pub value: String,
}

/// A filter of prefix and tags (Intelligent-Tiering and replication).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectFilter {
    /// Objects whose key starts with this prefix.
    pub prefix: Option<String>,
    /// Objects that carry all of these tags.
    pub tags: Vec<Tag>,
}

fn check_prefix(prefix: Option<&String>, path: &str, issues: &mut Vec<RuleIssue>) {
    if let Some(prefix) = prefix
        && prefix.len() > MAX_KEY_BYTES
    {
        issues.push(RuleIssue::new(
            format!("{path}.prefix"),
            format!("is longer than {MAX_KEY_BYTES} bytes"),
        ));
    }
}

fn check_tags(tags: &[Tag], path: &str, issues: &mut Vec<RuleIssue>) {
    if tags.len() > MAX_FILTER_TAGS {
        issues.push(RuleIssue::new(
            format!("{path}.tags"),
            format!(
                "has {} tags; a filter takes at most {MAX_FILTER_TAGS}",
                tags.len()
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, tag) in tags.iter().enumerate() {
        let at = format!("{path}.tags[{index}]");
        let length = tag.key.chars().count();
        if length == 0 || length > 128 {
            issues.push(RuleIssue::new(
                format!("{at}.key"),
                "must be 1 to 128 characters",
            ));
        } else if !seen.insert(tag.key.as_str()) {
            issues.push(RuleIssue::new(
                format!("{at}.key"),
                format!("the tag key {:?} is used twice", tag.key),
            ));
        }
        if tag.value.chars().count() > 256 {
            issues.push(RuleIssue::new(
                format!("{at}.value"),
                "must be at most 256 characters",
            ));
        }
    }
}

impl ObjectFilter {
    fn check(&self, path: &str, issues: &mut Vec<RuleIssue>) {
        check_prefix(self.prefix.as_ref(), path, issues);
        check_tags(&self.tags, path, issues);
    }
}

fn check_rule_id(
    id: Option<&String>,
    path: &str,
    seen: &mut BTreeSet<String>,
    issues: &mut Vec<RuleIssue>,
) {
    let Some(id) = id else {
        return;
    };
    if id.is_empty() {
        issues.push(RuleIssue::new(
            format!("{path}.id"),
            "must not be empty (leave it out to let S3 choose one)",
        ));
    } else if id.chars().count() > MAX_RULE_ID_CHARS {
        issues.push(RuleIssue::new(
            format!("{path}.id"),
            format!("is longer than {MAX_RULE_ID_CHARS} characters"),
        ));
    } else if !seen.insert(id.clone()) {
        issues.push(RuleIssue::new(
            format!("{path}.id"),
            format!("the id {id:?} is used by another rule"),
        ));
    }
}

fn tag_to_sdk(tag: &Tag) -> Result<SdkTag> {
    SdkTag::builder()
        .key(&tag.key)
        .value(&tag.value)
        .build()
        .map_err(build_error)
}

fn tags_to_sdk(tags: &[Tag]) -> Result<Vec<SdkTag>> {
    tags.iter().map(tag_to_sdk).collect()
}

fn tag_from_sdk(tag: &SdkTag) -> Tag {
    Tag {
        key: tag.key().to_owned(),
        value: tag.value().to_owned(),
    }
}

fn tags_from_sdk(tags: &[SdkTag]) -> Vec<Tag> {
    tags.iter().map(tag_from_sdk).collect()
}

fn days_to_sdk(days: u32, path: &str) -> Result<i32> {
    i32::try_from(days).map_err(|_| Error::Usage(format!("{path}: {days} is too large")))
}

/// Parses a `YYYY-MM-DD` date into the midnight UTC instant S3 requires.
fn parse_date(text: &str) -> Option<DateTime> {
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    if text.len() != 10 {
        return None;
    }
    DateTime::from_str(&format!("{text}T00:00:00Z"), Format::DateTime).ok()
}

/// The date part of an instant S3 returned, as `YYYY-MM-DD`.
fn date_from_sdk(date: &DateTime) -> Option<String> {
    date.fmt(Format::DateTime)
        .ok()
        .map(|text| text.chars().take(10).collect())
}

fn check_date(text: &str, path: &str, issues: &mut Vec<RuleIssue>) {
    if parse_date(text).is_none() {
        issues.push(RuleIssue::new(
            path,
            format!("{text:?} is not a date; use YYYY-MM-DD (S3 runs the action at midnight UTC)"),
        ));
    }
}

/// Parses a JSON document of one of the models and lists its problems: a
/// document that is not valid JSON, or does not have the shape of the model
/// (a misspelled or unknown field, a wrong type), is a single issue with the
/// position; otherwise the model's own `check` runs.
fn check_json<T: serde::de::DeserializeOwned>(
    text: &str,
    check: impl FnOnce(&T) -> Vec<RuleIssue>,
) -> Vec<RuleIssue> {
    if text.trim().is_empty() {
        return vec![RuleIssue::new("", "the document is empty")];
    }
    match serde_json::from_str::<T>(text) {
        Ok(value) => check(&value),
        Err(error) => vec![RuleIssue::new("", format!("not valid: {error}"))],
    }
}

/// Problems of a lifecycle document given as JSON text ([`check_json`]).
pub fn check_lifecycle_json(text: &str) -> Vec<RuleIssue> {
    check_json(text, LifecycleConfig::check)
}

/// Problems of a CORS document given as JSON text.
pub fn check_cors_json(text: &str) -> Vec<RuleIssue> {
    check_json(text, CorsConfig::check)
}

/// Problems of an Intelligent-Tiering document given as JSON text.
pub fn check_tiering_json(text: &str) -> Vec<RuleIssue> {
    check_json(text, TieringConfig::check)
}

/// Problems of a replication document given as JSON text (the versioning of
/// the source is checked when it is saved).
pub fn check_replication_json(text: &str) -> Vec<RuleIssue> {
    check_json(text, ReplicationConfig::check)
}

// ---- lifecycle ----

/// The lifecycle rules of a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    /// The rules, evaluated together by S3.
    pub rules: Vec<LifecycleRule>,
}

/// One lifecycle rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleRule {
    /// The rule id (unique, up to 255 characters); S3 picks one without it.
    pub id: Option<String>,
    /// Whether the rule is applied.
    pub status: RuleStatus,
    /// Which objects the rule covers; every object without a condition.
    pub filter: LifecycleFilter,
    /// Moves current versions to another storage class.
    pub transitions: Vec<LifecycleTransition>,
    /// Expires current versions, or removes expired delete markers.
    pub expiration: Option<ExpirationAction>,
    /// Moves noncurrent versions to another storage class.
    pub noncurrent_version_transitions: Vec<NoncurrentTransition>,
    /// Permanently deletes noncurrent versions.
    pub noncurrent_version_expiration: Option<NoncurrentExpiration>,
    /// Aborts multipart uploads that were not completed after this many days.
    pub abort_incomplete_multipart_upload_days: Option<u32>,
}

/// Which objects a lifecycle rule covers: all conditions must hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleFilter {
    /// The key starts with this prefix.
    pub prefix: Option<String>,
    /// The object carries all of these tags.
    pub tags: Vec<Tag>,
    /// The object is larger than this many bytes.
    pub object_size_greater_than: Option<i64>,
    /// The object is smaller than this many bytes.
    pub object_size_less_than: Option<i64>,
}

/// A transition of current versions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleTransition {
    /// Days after creation (set this or `date`).
    pub days: Option<u32>,
    /// A date, `YYYY-MM-DD`, midnight UTC (set this or `days`).
    pub date: Option<String>,
    /// The target class, one of [`TRANSITION_CLASSES`].
    pub storage_class: String,
}

/// The expiration of current versions. Set exactly one field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpirationAction {
    /// Days after creation.
    pub days: Option<u32>,
    /// A date, `YYYY-MM-DD`, midnight UTC.
    pub date: Option<String>,
    /// Removes delete markers that have no noncurrent versions left.
    pub expired_object_delete_marker: bool,
}

/// A transition of noncurrent versions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoncurrentTransition {
    /// Days after the version became noncurrent.
    pub noncurrent_days: u32,
    /// Newest noncurrent versions that stay where they are (at least 1).
    pub newer_noncurrent_versions: Option<u32>,
    /// The target class, one of [`TRANSITION_CLASSES`].
    pub storage_class: String,
}

/// The expiration of noncurrent versions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoncurrentExpiration {
    /// Days after the version became noncurrent (at least 1).
    pub noncurrent_days: Option<u32>,
    /// Newest noncurrent versions that are kept (at least 1).
    pub newer_noncurrent_versions: Option<u32>,
}

fn check_transition_class(class: &str, path: &str, issues: &mut Vec<RuleIssue>) {
    if !TRANSITION_CLASSES.contains(&class) {
        issues.push(RuleIssue::new(
            format!("{path}.storage_class"),
            format!(
                "{class:?} is not a transition target; use one of {}",
                TRANSITION_CLASSES.join(", ")
            ),
        ));
    }
}

impl LifecycleFilter {
    /// The number of conditions that are set.
    fn conditions(&self) -> usize {
        usize::from(self.prefix.is_some())
            + self.tags.len()
            + usize::from(self.object_size_greater_than.is_some())
            + usize::from(self.object_size_less_than.is_some())
    }

    fn check(&self, path: &str, issues: &mut Vec<RuleIssue>) {
        check_prefix(self.prefix.as_ref(), path, issues);
        check_tags(&self.tags, path, issues);
        for (name, value) in [
            ("object_size_greater_than", self.object_size_greater_than),
            ("object_size_less_than", self.object_size_less_than),
        ] {
            if value.is_some_and(|bytes| bytes < 0) {
                issues.push(RuleIssue::new(
                    format!("{path}.{name}"),
                    "must not be negative",
                ));
            }
        }
        if let (Some(greater), Some(less)) =
            (self.object_size_greater_than, self.object_size_less_than)
            && greater >= less
        {
            issues.push(RuleIssue::new(
                format!("{path}.object_size_less_than"),
                format!("must be larger than object_size_greater_than ({greater})"),
            ));
        }
    }
}

impl LifecycleRule {
    fn check(&self, index: usize, issues: &mut Vec<RuleIssue>) {
        let path = format!("rules[{index}]");
        self.filter.check(&format!("{path}.filter"), issues);
        let has_tags = !self.filter.tags.is_empty();

        let has_action = !self.transitions.is_empty()
            || self.expiration.is_some()
            || !self.noncurrent_version_transitions.is_empty()
            || self.noncurrent_version_expiration.is_some()
            || self.abort_incomplete_multipart_upload_days.is_some();
        if !has_action {
            issues.push(RuleIssue::new(
                path.clone(),
                "has no action; add a transition, an expiration, a noncurrent version action or an abort of incomplete multipart uploads",
            ));
        }

        let mut classes = BTreeSet::new();
        let mut transition_days = Vec::new();
        for (n, transition) in self.transitions.iter().enumerate() {
            let at = format!("{path}.transitions[{n}]");
            match (&transition.days, &transition.date) {
                (Some(_), Some(_)) => issues.push(RuleIssue::new(
                    at.clone(),
                    "set either days or date, not both",
                )),
                (None, None) => issues.push(RuleIssue::new(at.clone(), "set days or date")),
                (Some(days), None) => {
                    transition_days.push(*days);
                    if matches!(
                        transition.storage_class.as_str(),
                        "STANDARD_IA" | "ONEZONE_IA"
                    ) && *days < 30
                    {
                        issues.push(RuleIssue::new(
                            format!("{at}.days"),
                            format!(
                                "objects stay at least 30 days in STANDARD before they move to {}",
                                transition.storage_class
                            ),
                        ));
                    }
                }
                (None, Some(date)) => check_date(date, &format!("{at}.date"), issues),
            }
            check_transition_class(&transition.storage_class, &at, issues);
            if !classes.insert(transition.storage_class.as_str()) {
                issues.push(RuleIssue::new(
                    format!("{at}.storage_class"),
                    format!(
                        "{} is the target of another transition of this rule",
                        transition.storage_class
                    ),
                ));
            }
        }

        if let Some(expiration) = &self.expiration {
            let at = format!("{path}.expiration");
            let set = usize::from(expiration.days.is_some())
                + usize::from(expiration.date.is_some())
                + usize::from(expiration.expired_object_delete_marker);
            if set == 0 {
                issues.push(RuleIssue::new(
                    at.clone(),
                    "set days, date or expired_object_delete_marker",
                ));
            } else if set > 1 {
                issues.push(RuleIssue::new(
                    at.clone(),
                    "set only one of days, date and expired_object_delete_marker",
                ));
            }
            if let Some(days) = expiration.days {
                if days == 0 {
                    issues.push(RuleIssue::new(format!("{at}.days"), "must be at least 1"));
                } else if let Some(longest) = transition_days.iter().max()
                    && days <= *longest
                {
                    issues.push(RuleIssue::new(
                        format!("{at}.days"),
                        format!(
                            "must be greater than the days of every transition (a transition uses {longest})"
                        ),
                    ));
                }
            }
            if let Some(date) = &expiration.date {
                check_date(date, &format!("{at}.date"), issues);
            }
            if expiration.expired_object_delete_marker && has_tags {
                issues.push(RuleIssue::new(
                    format!("{at}.expired_object_delete_marker"),
                    "cannot be combined with tags in the filter",
                ));
            }
        }

        let mut noncurrent_classes = BTreeSet::new();
        for (n, transition) in self.noncurrent_version_transitions.iter().enumerate() {
            let at = format!("{path}.noncurrent_version_transitions[{n}]");
            check_transition_class(&transition.storage_class, &at, issues);
            if !noncurrent_classes.insert(transition.storage_class.as_str()) {
                issues.push(RuleIssue::new(
                    format!("{at}.storage_class"),
                    format!(
                        "{} is the target of another noncurrent transition of this rule",
                        transition.storage_class
                    ),
                ));
            }
            if matches!(
                transition.storage_class.as_str(),
                "STANDARD_IA" | "ONEZONE_IA"
            ) && transition.noncurrent_days < 30
            {
                issues.push(RuleIssue::new(
                    format!("{at}.noncurrent_days"),
                    format!(
                        "versions stay at least 30 days in STANDARD before they move to {}",
                        transition.storage_class
                    ),
                ));
            }
            if transition.newer_noncurrent_versions == Some(0) {
                issues.push(RuleIssue::new(
                    format!("{at}.newer_noncurrent_versions"),
                    "must be at least 1",
                ));
            }
        }

        if let Some(expiration) = &self.noncurrent_version_expiration {
            let at = format!("{path}.noncurrent_version_expiration");
            match expiration.noncurrent_days {
                None => issues.push(RuleIssue::new(
                    format!("{at}.noncurrent_days"),
                    "is required",
                )),
                Some(0) => issues.push(RuleIssue::new(
                    format!("{at}.noncurrent_days"),
                    "must be at least 1",
                )),
                Some(_) => {}
            }
            if expiration.newer_noncurrent_versions == Some(0) {
                issues.push(RuleIssue::new(
                    format!("{at}.newer_noncurrent_versions"),
                    "must be at least 1",
                ));
            }
        }

        if let Some(days) = self.abort_incomplete_multipart_upload_days {
            let at = format!("{path}.abort_incomplete_multipart_upload_days");
            if days == 0 {
                issues.push(RuleIssue::new(at, "must be at least 1"));
            } else if has_tags {
                issues.push(RuleIssue::new(
                    at,
                    "cannot be combined with tags in the filter",
                ));
            }
        }
    }
}

impl LifecycleConfig {
    /// Every problem of the document, each with its path. Empty means S3's
    /// rules for lifecycle configurations hold.
    ///
    /// There are 1 to [`MAX_RULES`] rules with distinct ids. A rule has an
    /// action; a transition sets days or a date and a target in
    /// [`TRANSITION_CLASSES`] (STANDARD_IA and ONEZONE_IA need 30 days or
    /// more, a class appears once, and an expiration lies behind every
    /// transition); an expiration sets exactly one of days, date and the
    /// delete marker cleanup; noncurrent actions need their days; the tag
    /// filter excludes the delete marker cleanup and the multipart abort;
    /// sizes are not negative and the lower bound is below the upper one.
    pub fn check(&self) -> Vec<RuleIssue> {
        let mut issues = Vec::new();
        if self.rules.is_empty() {
            issues.push(RuleIssue::new(
                "rules",
                "add at least one rule; to remove every rule, delete the configuration",
            ));
        }
        if self.rules.len() > MAX_RULES {
            issues.push(RuleIssue::new(
                "rules",
                format!(
                    "has {} rules; S3 accepts at most {MAX_RULES}",
                    self.rules.len()
                ),
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, rule) in self.rules.iter().enumerate() {
            check_rule_id(
                rule.id.as_ref(),
                &format!("rules[{index}]"),
                &mut seen,
                &mut issues,
            );
            rule.check(index, &mut issues);
        }
        issues
    }

    /// [`LifecycleConfig::check`] as one `usage` error.
    pub fn validate(&self) -> Result<()> {
        issues_to_result("lifecycle configuration", &self.check())
    }
}

fn lifecycle_to_sdk(config: &LifecycleConfig) -> Result<BucketLifecycleConfiguration> {
    let mut out = BucketLifecycleConfiguration::builder();
    for (index, rule) in config.rules.iter().enumerate() {
        let path = format!("rules[{index}]");
        let mut sdk = SdkLifecycleRule::builder()
            .status(match rule.status {
                RuleStatus::Enabled => ExpirationStatus::Enabled,
                RuleStatus::Disabled => ExpirationStatus::Disabled,
            })
            .filter(lifecycle_filter_to_sdk(&rule.filter)?);
        if let Some(id) = &rule.id {
            sdk = sdk.id(id);
        }
        for (n, transition) in rule.transitions.iter().enumerate() {
            let at = format!("{path}.transitions[{n}]");
            let mut t = Transition::builder().storage_class(TransitionStorageClass::from(
                transition.storage_class.as_str(),
            ));
            if let Some(days) = transition.days {
                t = t.days(days_to_sdk(days, &at)?);
            }
            if let Some(date) = &transition.date {
                t = t.set_date(parse_date(date));
            }
            sdk = sdk.transitions(t.build());
        }
        if let Some(expiration) = &rule.expiration {
            let at = format!("{path}.expiration");
            let mut e = LifecycleExpiration::builder();
            if let Some(days) = expiration.days {
                e = e.days(days_to_sdk(days, &at)?);
            }
            if let Some(date) = &expiration.date {
                e = e.set_date(parse_date(date));
            }
            if expiration.expired_object_delete_marker {
                e = e.expired_object_delete_marker(true);
            }
            sdk = sdk.expiration(e.build());
        }
        for (n, transition) in rule.noncurrent_version_transitions.iter().enumerate() {
            let at = format!("{path}.noncurrent_version_transitions[{n}]");
            let mut t = NoncurrentVersionTransition::builder()
                .noncurrent_days(days_to_sdk(transition.noncurrent_days, &at)?)
                .storage_class(TransitionStorageClass::from(
                    transition.storage_class.as_str(),
                ));
            if let Some(newer) = transition.newer_noncurrent_versions {
                t = t.newer_noncurrent_versions(days_to_sdk(newer, &at)?);
            }
            sdk = sdk.noncurrent_version_transitions(t.build());
        }
        if let Some(expiration) = &rule.noncurrent_version_expiration {
            let at = format!("{path}.noncurrent_version_expiration");
            let mut e = NoncurrentVersionExpiration::builder();
            if let Some(days) = expiration.noncurrent_days {
                e = e.noncurrent_days(days_to_sdk(days, &at)?);
            }
            if let Some(newer) = expiration.newer_noncurrent_versions {
                e = e.newer_noncurrent_versions(days_to_sdk(newer, &at)?);
            }
            sdk = sdk.noncurrent_version_expiration(e.build());
        }
        if let Some(days) = rule.abort_incomplete_multipart_upload_days {
            sdk = sdk.abort_incomplete_multipart_upload(
                AbortIncompleteMultipartUpload::builder()
                    .days_after_initiation(days_to_sdk(
                        days,
                        &format!("{path}.abort_incomplete_multipart_upload_days"),
                    )?)
                    .build(),
            );
        }
        out = out.rules(sdk.build().map_err(build_error)?);
    }
    out.build().map_err(build_error)
}

/// One condition stands alone; several are combined with `And`.
fn lifecycle_filter_to_sdk(filter: &LifecycleFilter) -> Result<LifecycleRuleFilter> {
    let mut out = LifecycleRuleFilter::builder();
    if filter.conditions() > 1 {
        let mut and = LifecycleRuleAndOperator::builder()
            .set_prefix(filter.prefix.clone())
            .set_object_size_greater_than(filter.object_size_greater_than)
            .set_object_size_less_than(filter.object_size_less_than);
        for tag in tags_to_sdk(&filter.tags)? {
            and = and.tags(tag);
        }
        out = out.and(and.build());
    } else if let Some(tag) = filter.tags.first() {
        out = out.tag(tag_to_sdk(tag)?);
    } else {
        out = out
            .set_prefix(filter.prefix.clone())
            .set_object_size_greater_than(filter.object_size_greater_than)
            .set_object_size_less_than(filter.object_size_less_than);
    }
    Ok(out.build())
}

fn lifecycle_filter_from_sdk(
    filter: Option<&LifecycleRuleFilter>,
    legacy_prefix: Option<&str>,
) -> LifecycleFilter {
    let Some(filter) = filter else {
        return LifecycleFilter {
            prefix: legacy_prefix.filter(|p| !p.is_empty()).map(str::to_owned),
            ..LifecycleFilter::default()
        };
    };
    if let Some(and) = filter.and() {
        return LifecycleFilter {
            prefix: and.prefix().map(str::to_owned),
            tags: tags_from_sdk(and.tags()),
            object_size_greater_than: and.object_size_greater_than(),
            object_size_less_than: and.object_size_less_than(),
        };
    }
    LifecycleFilter {
        prefix: filter.prefix().map(str::to_owned),
        tags: filter.tag().map(tag_from_sdk).into_iter().collect(),
        object_size_greater_than: filter.object_size_greater_than(),
        object_size_less_than: filter.object_size_less_than(),
    }
}

/// Rules written long ago still carry the deprecated top-level prefix; it is read as the filter prefix.
#[allow(deprecated)]
fn lifecycle_from_sdk(rules: &[SdkLifecycleRule]) -> LifecycleConfig {
    LifecycleConfig {
        rules: rules
            .iter()
            .map(|rule| LifecycleRule {
                id: rule.id().map(str::to_owned),
                status: if *rule.status() == ExpirationStatus::Enabled {
                    RuleStatus::Enabled
                } else {
                    RuleStatus::Disabled
                },
                filter: lifecycle_filter_from_sdk(rule.filter(), rule.prefix()),
                transitions: rule
                    .transitions()
                    .iter()
                    .map(|t| LifecycleTransition {
                        days: t.days().and_then(|d| u32::try_from(d).ok()),
                        date: t.date().and_then(date_from_sdk),
                        storage_class: t
                            .storage_class()
                            .map(|c| c.as_str().to_owned())
                            .unwrap_or_default(),
                    })
                    .collect(),
                expiration: rule.expiration().map(|e| ExpirationAction {
                    days: e.days().and_then(|d| u32::try_from(d).ok()),
                    date: e.date().and_then(date_from_sdk),
                    expired_object_delete_marker: e.expired_object_delete_marker() == Some(true),
                }),
                noncurrent_version_transitions: rule
                    .noncurrent_version_transitions()
                    .iter()
                    .map(|t| NoncurrentTransition {
                        noncurrent_days: t
                            .noncurrent_days()
                            .and_then(|d| u32::try_from(d).ok())
                            .unwrap_or_default(),
                        newer_noncurrent_versions: t
                            .newer_noncurrent_versions()
                            .and_then(|d| u32::try_from(d).ok()),
                        storage_class: t
                            .storage_class()
                            .map(|c| c.as_str().to_owned())
                            .unwrap_or_default(),
                    })
                    .collect(),
                noncurrent_version_expiration: rule.noncurrent_version_expiration().map(|e| {
                    NoncurrentExpiration {
                        noncurrent_days: e.noncurrent_days().and_then(|d| u32::try_from(d).ok()),
                        newer_noncurrent_versions: e
                            .newer_noncurrent_versions()
                            .and_then(|d| u32::try_from(d).ok()),
                    }
                }),
                abort_incomplete_multipart_upload_days: rule
                    .abort_incomplete_multipart_upload()
                    .and_then(|a| a.days_after_initiation())
                    .and_then(|d| u32::try_from(d).ok()),
            })
            .collect(),
    }
}

// ---- Intelligent-Tiering ----

/// One Intelligent-Tiering archive configuration of a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TieringConfig {
    /// The configuration id (1 to 64 characters, unique in the bucket).
    pub id: String,
    /// Whether the configuration is applied.
    pub status: RuleStatus,
    /// The objects it covers; all Intelligent-Tiering objects without one.
    pub filter: ObjectFilter,
    /// The archive tiers and the days without access before each one.
    pub tierings: Vec<TieringStep>,
}

/// Which archive tier a step moves objects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AccessTier {
    /// Archive Access: 90 to 730 days.
    ArchiveAccess,
    /// Deep Archive Access: 180 to 730 days.
    DeepArchiveAccess,
}

impl AccessTier {
    /// The name S3 uses.
    pub fn name(self) -> &'static str {
        match self {
            AccessTier::ArchiveAccess => "ARCHIVE_ACCESS",
            AccessTier::DeepArchiveAccess => "DEEP_ARCHIVE_ACCESS",
        }
    }

    /// The smallest and largest number of days of the tier.
    pub fn day_range(self) -> (u32, u32) {
        match self {
            AccessTier::ArchiveAccess => (90, 730),
            AccessTier::DeepArchiveAccess => (180, 730),
        }
    }
}

/// One archive step of an Intelligent-Tiering configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieringStep {
    /// The tier.
    pub access_tier: AccessTier,
    /// Days without access before objects move to the tier.
    pub days: u32,
}

impl TieringConfig {
    /// Every problem of the configuration, each with its path.
    ///
    /// The id has 1 to 64 characters; there is one or two steps, each tier
    /// at most once, with 90 to 730 days (Archive Access) or 180 to 730 days
    /// (Deep Archive Access).
    pub fn check(&self) -> Vec<RuleIssue> {
        let mut issues = Vec::new();
        let length = self.id.chars().count();
        if length == 0 || length > MAX_TIERING_ID_CHARS {
            issues.push(RuleIssue::new(
                "id",
                format!("must be 1 to {MAX_TIERING_ID_CHARS} characters"),
            ));
        }
        self.filter.check("filter", &mut issues);
        if self.tierings.is_empty() {
            issues.push(RuleIssue::new(
                "tierings",
                "add at least one archive tier (ARCHIVE_ACCESS or DEEP_ARCHIVE_ACCESS)",
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, step) in self.tierings.iter().enumerate() {
            let at = format!("tierings[{index}]");
            if !seen.insert(step.access_tier) {
                issues.push(RuleIssue::new(
                    format!("{at}.access_tier"),
                    format!("{} is configured twice", step.access_tier.name()),
                ));
            }
            let (low, high) = step.access_tier.day_range();
            if step.days < low || step.days > high {
                issues.push(RuleIssue::new(
                    format!("{at}.days"),
                    format!(
                        "{} takes {low} to {high} days (found {})",
                        step.access_tier.name(),
                        step.days
                    ),
                ));
            }
        }
        issues
    }

    /// [`TieringConfig::check`] as one `usage` error.
    pub fn validate(&self) -> Result<()> {
        issues_to_result("Intelligent-Tiering configuration", &self.check())
    }
}

fn tiering_to_sdk(config: &TieringConfig) -> Result<IntelligentTieringConfiguration> {
    let mut filter = IntelligentTieringFilter::builder();
    let conditions = usize::from(config.filter.prefix.is_some()) + config.filter.tags.len();
    if conditions > 1 {
        let mut and =
            IntelligentTieringAndOperator::builder().set_prefix(config.filter.prefix.clone());
        for tag in tags_to_sdk(&config.filter.tags)? {
            and = and.tags(tag);
        }
        filter = filter.and(and.build());
    } else if let Some(tag) = config.filter.tags.first() {
        filter = filter.tag(tag_to_sdk(tag)?);
    } else {
        filter = filter.set_prefix(config.filter.prefix.clone());
    }
    let mut out = IntelligentTieringConfiguration::builder()
        .id(&config.id)
        .status(match config.status {
            RuleStatus::Enabled => IntelligentTieringStatus::Enabled,
            RuleStatus::Disabled => IntelligentTieringStatus::Disabled,
        });
    if conditions > 0 {
        out = out.filter(filter.build());
    }
    for step in &config.tierings {
        out = out.tierings(
            Tiering::builder()
                .days(days_to_sdk(step.days, "tierings")?)
                .access_tier(IntelligentTieringAccessTier::from(step.access_tier.name()))
                .build()
                .map_err(build_error)?,
        );
    }
    out.build().map_err(build_error)
}

fn tiering_from_sdk(config: &IntelligentTieringConfiguration) -> TieringConfig {
    let filter = match config.filter() {
        None => ObjectFilter::default(),
        Some(filter) => match filter.and() {
            Some(and) => ObjectFilter {
                prefix: and.prefix().map(str::to_owned),
                tags: tags_from_sdk(and.tags()),
            },
            None => ObjectFilter {
                prefix: filter.prefix().map(str::to_owned),
                tags: filter.tag().map(tag_from_sdk).into_iter().collect(),
            },
        },
    };
    TieringConfig {
        id: config.id().to_owned(),
        status: if *config.status() == IntelligentTieringStatus::Enabled {
            RuleStatus::Enabled
        } else {
            RuleStatus::Disabled
        },
        filter,
        tierings: config
            .tierings()
            .iter()
            .filter_map(|t| {
                let access_tier = match t.access_tier().as_str() {
                    "ARCHIVE_ACCESS" => AccessTier::ArchiveAccess,
                    "DEEP_ARCHIVE_ACCESS" => AccessTier::DeepArchiveAccess,
                    _ => return None,
                };
                Some(TieringStep {
                    access_tier,
                    days: u32::try_from(t.days()).unwrap_or_default(),
                })
            })
            .collect(),
    }
}

// ---- CORS ----

/// The CORS rules of a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    /// The rules; the first one that matches a request answers it.
    pub rules: Vec<CorsRule>,
}

/// One CORS rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsRule {
    /// The rule id (up to 255 characters); optional.
    pub id: Option<String>,
    /// The origins that may call the bucket; each has at most one `*`.
    pub allowed_origins: Vec<String>,
    /// The methods, from [`CORS_METHODS`].
    pub allowed_methods: Vec<String>,
    /// The request headers that may be sent; each has at most one `*`.
    pub allowed_headers: Vec<String>,
    /// The response headers the browser may read.
    pub expose_headers: Vec<String>,
    /// How long a browser caches the preflight answer, in seconds.
    pub max_age_seconds: Option<u32>,
}

impl CorsConfig {
    /// Every problem of the document, each with its path.
    ///
    /// There are 1 to [`MAX_CORS_RULES`] rules; each allows at least one
    /// origin and one method of [`CORS_METHODS`]; origins and allowed headers
    /// hold at most one `*` wildcard; no entry is empty.
    pub fn check(&self) -> Vec<RuleIssue> {
        let mut issues = Vec::new();
        if self.rules.is_empty() {
            issues.push(RuleIssue::new(
                "rules",
                "add at least one rule; to remove every rule, delete the configuration",
            ));
        }
        if self.rules.len() > MAX_CORS_RULES {
            issues.push(RuleIssue::new(
                "rules",
                format!(
                    "has {} rules; S3 accepts at most {MAX_CORS_RULES}",
                    self.rules.len()
                ),
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, rule) in self.rules.iter().enumerate() {
            let path = format!("rules[{index}]");
            check_rule_id(rule.id.as_ref(), &path, &mut seen, &mut issues);
            if rule.allowed_origins.is_empty() {
                issues.push(RuleIssue::new(
                    format!("{path}.allowed_origins"),
                    "allow at least one origin (use * for any)",
                ));
            }
            for (n, origin) in rule.allowed_origins.iter().enumerate() {
                let at = format!("{path}.allowed_origins[{n}]");
                if origin.trim().is_empty() {
                    issues.push(RuleIssue::new(at, "must not be empty"));
                } else if origin.matches('*').count() > 1 {
                    issues.push(RuleIssue::new(
                        at,
                        format!("{origin:?} has more than one * wildcard"),
                    ));
                } else if origin.chars().any(char::is_whitespace) {
                    issues.push(RuleIssue::new(
                        at,
                        format!("{origin:?} must not contain spaces"),
                    ));
                }
            }
            if rule.allowed_methods.is_empty() {
                issues.push(RuleIssue::new(
                    format!("{path}.allowed_methods"),
                    format!("allow at least one method of {}", CORS_METHODS.join(", ")),
                ));
            }
            let mut methods = BTreeSet::new();
            for (n, method) in rule.allowed_methods.iter().enumerate() {
                let at = format!("{path}.allowed_methods[{n}]");
                if !CORS_METHODS.contains(&method.as_str()) {
                    issues.push(RuleIssue::new(
                        at,
                        format!(
                            "{method:?} is not allowed; use one of {} (upper case)",
                            CORS_METHODS.join(", ")
                        ),
                    ));
                } else if !methods.insert(method.as_str()) {
                    issues.push(RuleIssue::new(at, format!("{method} is listed twice")));
                }
            }
            for (n, header) in rule.allowed_headers.iter().enumerate() {
                let at = format!("{path}.allowed_headers[{n}]");
                if header.trim().is_empty() {
                    issues.push(RuleIssue::new(at, "must not be empty"));
                } else if header.matches('*').count() > 1 {
                    issues.push(RuleIssue::new(
                        at,
                        format!("{header:?} has more than one * wildcard"),
                    ));
                }
            }
            for (n, header) in rule.expose_headers.iter().enumerate() {
                if header.trim().is_empty() {
                    issues.push(RuleIssue::new(
                        format!("{path}.expose_headers[{n}]"),
                        "must not be empty",
                    ));
                }
            }
            if rule
                .max_age_seconds
                .is_some_and(|s| i32::try_from(s).is_err())
            {
                issues.push(RuleIssue::new(
                    format!("{path}.max_age_seconds"),
                    "is too large",
                ));
            }
        }
        issues
    }

    /// [`CorsConfig::check`] as one `usage` error.
    pub fn validate(&self) -> Result<()> {
        issues_to_result("CORS configuration", &self.check())
    }
}

fn cors_to_sdk(config: &CorsConfig) -> Result<CorsConfiguration> {
    let mut out = CorsConfiguration::builder();
    for (index, rule) in config.rules.iter().enumerate() {
        let mut sdk = SdkCorsRule::builder()
            .set_id(rule.id.clone())
            .set_allowed_origins(Some(rule.allowed_origins.clone()))
            .set_allowed_methods(Some(rule.allowed_methods.clone()));
        if !rule.allowed_headers.is_empty() {
            sdk = sdk.set_allowed_headers(Some(rule.allowed_headers.clone()));
        }
        if !rule.expose_headers.is_empty() {
            sdk = sdk.set_expose_headers(Some(rule.expose_headers.clone()));
        }
        if let Some(seconds) = rule.max_age_seconds {
            sdk = sdk.max_age_seconds(days_to_sdk(
                seconds,
                &format!("rules[{index}].max_age_seconds"),
            )?);
        }
        out = out.cors_rules(sdk.build().map_err(build_error)?);
    }
    out.build().map_err(build_error)
}

fn cors_from_sdk(rules: &[SdkCorsRule]) -> CorsConfig {
    CorsConfig {
        rules: rules
            .iter()
            .map(|rule| CorsRule {
                id: rule.id().map(str::to_owned),
                allowed_origins: rule.allowed_origins().to_vec(),
                allowed_methods: rule.allowed_methods().to_vec(),
                allowed_headers: rule.allowed_headers().to_vec(),
                expose_headers: rule.expose_headers().to_vec(),
                max_age_seconds: rule.max_age_seconds().and_then(|s| u32::try_from(s).ok()),
            })
            .collect(),
    }
}

// ---- replication ----

/// The replication configuration of a source bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReplicationConfig {
    /// The ARN of the IAM role S3 assumes to replicate objects.
    pub role: String,
    /// The rules.
    pub rules: Vec<ReplicationRule>,
}

/// One replication rule.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReplicationRule {
    /// The rule id (unique, up to 255 characters); optional.
    pub id: Option<String>,
    /// The priority; when rules overlap, the higher number wins. Unique.
    pub priority: u32,
    /// Whether the rule is applied.
    pub status: RuleStatus,
    /// The objects it replicates; every object without a condition.
    pub filter: ObjectFilter,
    /// The ARN of the destination bucket (`arn:aws:s3:::name`).
    pub destination_bucket: String,
    /// The storage class of the replicas; the source's own when `null`.
    pub storage_class: Option<String>,
    /// Whether delete markers are replicated too.
    pub delete_marker_replication: bool,
}

/// The bucket name in an S3 bucket ARN (`arn:aws:s3:::name`).
pub fn bucket_from_arn(arn: &str) -> Option<&str> {
    let rest = arn.strip_prefix("arn:")?;
    let (partition, rest) = rest.split_once(':')?;
    let name = rest.strip_prefix("s3:::")?;
    (!partition.is_empty() && !name.is_empty() && !name.contains('/')).then_some(name)
}

impl ReplicationConfig {
    /// Every problem of the document, each with its path.
    ///
    /// The role is an IAM role ARN; there are 1 to [`MAX_RULES`] rules with
    /// distinct ids and distinct priorities; each destination is an S3 bucket
    /// ARN with a valid bucket name; the storage class is one of
    /// [`REPLICA_CLASSES`].
    pub fn check(&self) -> Vec<RuleIssue> {
        let mut issues = Vec::new();
        if self.role.is_empty() {
            issues.push(RuleIssue::new(
                "role",
                "is required: the ARN of the IAM role S3 uses to replicate (arn:aws:iam::<account>:role/<name>)",
            ));
        } else if !self.role.starts_with("arn:") || !self.role.contains(":iam:") {
            issues.push(RuleIssue::new(
                "role",
                format!(
                    "{:?} is not an IAM role ARN (arn:aws:iam::<account>:role/<name>)",
                    self.role
                ),
            ));
        }
        if self.rules.is_empty() {
            issues.push(RuleIssue::new(
                "rules",
                "add at least one rule; to stop replicating, delete the configuration",
            ));
        }
        if self.rules.len() > MAX_RULES {
            issues.push(RuleIssue::new(
                "rules",
                format!(
                    "has {} rules; S3 accepts at most {MAX_RULES}",
                    self.rules.len()
                ),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut priorities = BTreeSet::new();
        for (index, rule) in self.rules.iter().enumerate() {
            let path = format!("rules[{index}]");
            check_rule_id(rule.id.as_ref(), &path, &mut ids, &mut issues);
            if !priorities.insert(rule.priority) {
                issues.push(RuleIssue::new(
                    format!("{path}.priority"),
                    format!("the priority {} is used by another rule", rule.priority),
                ));
            }
            if i32::try_from(rule.priority).is_err() {
                issues.push(RuleIssue::new(format!("{path}.priority"), "is too large"));
            }
            rule.filter.check(&format!("{path}.filter"), &mut issues);
            match bucket_from_arn(&rule.destination_bucket) {
                None => issues.push(RuleIssue::new(
                    format!("{path}.destination_bucket"),
                    format!(
                        "{:?} is not a bucket ARN; use arn:aws:s3:::<bucket>",
                        rule.destination_bucket
                    ),
                )),
                Some(name) => {
                    if let Err(error) = validate_bucket_name(name) {
                        issues.push(RuleIssue::new(
                            format!("{path}.destination_bucket"),
                            error.to_string(),
                        ));
                    }
                }
            }
            if let Some(class) = &rule.storage_class
                && !REPLICA_CLASSES.contains(&class.as_str())
            {
                issues.push(RuleIssue::new(
                    format!("{path}.storage_class"),
                    format!(
                        "{class:?} is not a replica storage class; use one of {}",
                        REPLICA_CLASSES.join(", ")
                    ),
                ));
            }
        }
        issues
    }

    /// [`ReplicationConfig::check`] as one `usage` error.
    pub fn validate(&self) -> Result<()> {
        issues_to_result("replication configuration", &self.check())
    }
}

fn replication_to_sdk(
    config: &ReplicationConfig,
    current: Option<&ReplicationConfiguration>,
) -> Result<ReplicationConfiguration> {
    let mut out = ReplicationConfiguration::builder().role(&config.role);
    for rule in &config.rules {
        let before = current.and_then(|c| {
            c.rules()
                .iter()
                .find(|old| old.id().is_some() && old.id() == rule.id.as_deref())
        });
        let mut destination = Destination::builder().bucket(&rule.destination_bucket);
        if let Some(class) = &rule.storage_class {
            destination = destination.storage_class(StorageClass::from(class.as_str()));
        }
        let mut sdk = SdkReplicationRule::builder()
            .status(match rule.status {
                RuleStatus::Enabled => ReplicationRuleStatus::Enabled,
                RuleStatus::Disabled => ReplicationRuleStatus::Disabled,
            })
            .priority(days_to_sdk(rule.priority, "priority")?)
            .filter(replication_filter_to_sdk(&rule.filter)?)
            .delete_marker_replication(
                DeleteMarkerReplication::builder()
                    .status(if rule.delete_marker_replication {
                        DeleteMarkerReplicationStatus::Enabled
                    } else {
                        DeleteMarkerReplicationStatus::Disabled
                    })
                    .build(),
            );
        if let Some(id) = &rule.id {
            sdk = sdk.id(id);
        }
        if let Some(old) = before {
            if let Some(old) = old.destination() {
                destination = destination
                    .set_account(old.account().map(str::to_owned))
                    .set_access_control_translation(old.access_control_translation().cloned())
                    .set_encryption_configuration(old.encryption_configuration().cloned())
                    .set_replication_time(old.replication_time().cloned())
                    .set_metrics(old.metrics().cloned());
            }
            sdk = sdk
                .set_source_selection_criteria(old.source_selection_criteria().cloned())
                .set_existing_object_replication(old.existing_object_replication().cloned());
        }
        out = out.rules(
            sdk.destination(destination.build().map_err(build_error)?)
                .build()
                .map_err(build_error)?,
        );
    }
    out.build().map_err(build_error)
}

fn replication_filter_to_sdk(filter: &ObjectFilter) -> Result<ReplicationRuleFilter> {
    let mut out = ReplicationRuleFilter::builder();
    let conditions = usize::from(filter.prefix.is_some()) + filter.tags.len();
    if conditions > 1 {
        let mut and = ReplicationRuleAndOperator::builder().set_prefix(filter.prefix.clone());
        for tag in tags_to_sdk(&filter.tags)? {
            and = and.tags(tag);
        }
        out = out.and(and.build());
    } else if let Some(tag) = filter.tags.first() {
        out = out.tag(tag_to_sdk(tag)?);
    } else {
        // An empty prefix is the form S3 accepts for "every object".
        out = out.prefix(filter.prefix.clone().unwrap_or_default());
    }
    Ok(out.build())
}

/// Rules written long ago still carry the deprecated top-level prefix; it is read as the filter prefix.
#[allow(deprecated)]
fn replication_from_sdk(config: &ReplicationConfiguration) -> ReplicationConfig {
    ReplicationConfig {
        role: config.role().to_owned(),
        rules: config
            .rules()
            .iter()
            .map(|rule| {
                let filter = match rule.filter() {
                    Some(filter) => match filter.and() {
                        Some(and) => ObjectFilter {
                            prefix: and.prefix().map(str::to_owned),
                            tags: tags_from_sdk(and.tags()),
                        },
                        None => ObjectFilter {
                            prefix: filter.prefix().filter(|p| !p.is_empty()).map(str::to_owned),
                            tags: filter.tag().map(tag_from_sdk).into_iter().collect(),
                        },
                    },
                    None => ObjectFilter {
                        prefix: rule.prefix().filter(|p| !p.is_empty()).map(str::to_owned),
                        tags: Vec::new(),
                    },
                };
                ReplicationRule {
                    id: rule.id().map(str::to_owned),
                    priority: rule
                        .priority()
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or_default(),
                    status: if *rule.status() == ReplicationRuleStatus::Enabled {
                        RuleStatus::Enabled
                    } else {
                        RuleStatus::Disabled
                    },
                    filter,
                    destination_bucket: rule
                        .destination()
                        .map(|d| d.bucket().to_owned())
                        .unwrap_or_default(),
                    storage_class: rule
                        .destination()
                        .and_then(|d| d.storage_class())
                        .map(|c| c.as_str().to_owned()),
                    delete_marker_replication: rule
                        .delete_marker_replication()
                        .and_then(|d| d.status())
                        .is_some_and(|s| *s == DeleteMarkerReplicationStatus::Enabled),
                }
            })
            .collect(),
    }
}

// ---- the four settings of a bucket ----

/// The rule settings of one bucket, each read independently
/// ([`S3::get_bucket_rules`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketRules {
    /// CORS rules; `null` when none are configured.
    pub cors: Section<Option<CorsConfig>>,
    /// Lifecycle rules; `null` when none are configured.
    pub lifecycle: Section<Option<LifecycleConfig>>,
    /// Intelligent-Tiering configurations (possibly none).
    pub tiering: Section<Vec<TieringConfig>>,
    /// Replication; `null` when the bucket does not replicate.
    pub replication: Section<Option<ReplicationConfig>>,
}

impl S3 {
    /// Reads CORS, lifecycle, Intelligent-Tiering and replication of
    /// `bucket` at once; each one is reported on its own ([`Section`]).
    pub async fn get_bucket_rules(&self, bucket: &str) -> Result<BucketRules> {
        validate_bucket_name(bucket)?;
        self.client_for(bucket).await?;
        let (cors, lifecycle, tiering, replication) = tokio::join!(
            self.get_bucket_cors(bucket),
            self.get_bucket_lifecycle(bucket),
            self.list_bucket_tiering(bucket),
            self.get_bucket_replication(bucket),
        );
        Ok(BucketRules {
            cors: Section::from_result(cors, "CORS"),
            lifecycle: Section::from_result(lifecycle, "lifecycle rules"),
            tiering: Section::from_result(tiering, "Intelligent-Tiering"),
            replication: Section::from_result(replication, "replication"),
        })
    }

    // ---- lifecycle ----

    /// The lifecycle rules; `None` when the bucket has none.
    pub async fn get_bucket_lifecycle(&self, bucket: &str) -> Result<Option<LifecycleConfig>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_lifecycle_configuration()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchLifecycleConfiguration",
            context("reading the lifecycle rules", bucket),
        )?;
        Ok(out.map(|out| lifecycle_from_sdk(out.rules())))
    }

    /// Replaces the lifecycle rules after checking them
    /// ([`LifecycleConfig::check`]).
    pub async fn put_bucket_lifecycle(&self, bucket: &str, config: &LifecycleConfig) -> Result<()> {
        validate_bucket_name(bucket)?;
        config.validate()?;
        let sdk = lifecycle_to_sdk(config)?;
        self.client_for(bucket)
            .await?
            .put_bucket_lifecycle_configuration()
            .bucket(bucket)
            .lifecycle_configuration(sdk)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the lifecycle rules", bucket))
            })?;
        Ok(())
    }

    /// Deletes every lifecycle rule; succeeds when there are none.
    pub async fn delete_bucket_lifecycle(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_lifecycle()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchLifecycleConfiguration",
            context("deleting the lifecycle rules", bucket),
        )
    }

    // ---- Intelligent-Tiering ----

    /// Every Intelligent-Tiering configuration of the bucket, sorted by id.
    pub async fn list_bucket_tiering(&self, bucket: &str) -> Result<Vec<TieringConfig>> {
        let client = self.client_for(bucket).await?;
        let mut all = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let reply = client
                .list_bucket_intelligent_tiering_configurations()
                .bucket(bucket)
                .set_continuation_token(token.clone())
                .customize()
                .config_override(self.metadata_timeout())
                .send()
                .await;
            let Some(out) = read_optional(
                reply,
                "NoSuchConfiguration",
                context("listing the Intelligent-Tiering configurations", bucket),
            )?
            else {
                break;
            };
            all.extend(
                out.intelligent_tiering_configuration_list()
                    .iter()
                    .map(tiering_from_sdk),
            );
            match out.next_continuation_token() {
                Some(next)
                    if out.is_truncated() == Some(true) && Some(next) != token.as_deref() =>
                {
                    token = Some(next.to_owned());
                }
                _ => break,
            }
        }
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all)
    }

    /// One Intelligent-Tiering configuration; `None` when the id is unknown.
    pub async fn get_bucket_tiering(
        &self,
        bucket: &str,
        id: &str,
    ) -> Result<Option<TieringConfig>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_intelligent_tiering_configuration()
            .bucket(bucket)
            .id(id)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchConfiguration",
            context("reading the Intelligent-Tiering configuration", bucket),
        )?;
        Ok(out.and_then(|out| {
            out.intelligent_tiering_configuration()
                .map(tiering_from_sdk)
        }))
    }

    /// Creates or replaces the configuration with `config.id`, after
    /// checking it ([`TieringConfig::check`]).
    pub async fn put_bucket_tiering(&self, bucket: &str, config: &TieringConfig) -> Result<()> {
        validate_bucket_name(bucket)?;
        config.validate()?;
        let sdk = tiering_to_sdk(config)?;
        self.client_for(bucket)
            .await?
            .put_bucket_intelligent_tiering_configuration()
            .bucket(bucket)
            .id(&config.id)
            .intelligent_tiering_configuration(sdk)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context(
                    "saving the Intelligent-Tiering configuration",
                    bucket,
                ))
            })?;
        Ok(())
    }

    /// Deletes one Intelligent-Tiering configuration; succeeds when the id
    /// does not exist.
    pub async fn delete_bucket_tiering(&self, bucket: &str, id: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_intelligent_tiering_configuration()
            .bucket(bucket)
            .id(id)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchConfiguration",
            context("deleting the Intelligent-Tiering configuration", bucket),
        )
    }

    // ---- CORS ----

    /// The CORS rules; `None` when the bucket has none.
    pub async fn get_bucket_cors(&self, bucket: &str) -> Result<Option<CorsConfig>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_cors()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "NoSuchCORSConfiguration",
            context("reading the CORS rules", bucket),
        )?;
        Ok(out.map(|out| cors_from_sdk(out.cors_rules())))
    }

    /// Replaces the CORS rules after checking them ([`CorsConfig::check`]).
    pub async fn put_bucket_cors(&self, bucket: &str, config: &CorsConfig) -> Result<()> {
        validate_bucket_name(bucket)?;
        config.validate()?;
        let sdk = cors_to_sdk(config)?;
        self.client_for(bucket)
            .await?
            .put_bucket_cors()
            .bucket(bucket)
            .cors_configuration(sdk)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(context("saving the CORS rules", bucket))
            })?;
        Ok(())
    }

    /// Deletes every CORS rule; succeeds when there are none.
    pub async fn delete_bucket_cors(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_cors()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "NoSuchCORSConfiguration",
            context("deleting the CORS rules", bucket),
        )
    }

    // ---- replication ----

    async fn replication_raw(&self, bucket: &str) -> Result<Option<ReplicationConfiguration>> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_replication()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "ReplicationConfigurationNotFoundError",
            context("reading the replication configuration", bucket),
        )?;
        Ok(out.and_then(|out| out.replication_configuration().cloned()))
    }

    /// The replication configuration; `None` when the bucket does not
    /// replicate.
    pub async fn get_bucket_replication(&self, bucket: &str) -> Result<Option<ReplicationConfig>> {
        Ok(self
            .replication_raw(bucket)
            .await?
            .as_ref()
            .map(replication_from_sdk))
    }

    /// Replaces the replication configuration. The document is checked
    /// first ([`ReplicationConfig::check`]) and versioning must be enabled
    /// on `bucket` (a `usage` error says how to fix it). Settings of existing
    /// rules that the model does not carry are kept (see the module
    /// documentation).
    pub async fn put_bucket_replication(
        &self,
        bucket: &str,
        config: &ReplicationConfig,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        config.validate()?;
        let versioning = self.get_bucket_versioning(bucket).await?;
        if versioning.status != VersioningStatus::Enabled {
            let state = match versioning.status {
                VersioningStatus::Suspended => "suspended",
                _ => "not enabled",
            };
            return Err(Error::Usage(format!(
                "replication needs versioning on the source bucket, and versioning of '{bucket}' is {state}; enable versioning in the bucket properties first"
            )));
        }
        let current = self.replication_raw(bucket).await?;
        let sdk = replication_to_sdk(config, current.as_ref())?;
        self.client_for(bucket)
            .await?
            .put_bucket_replication()
            .bucket(bucket)
            .replication_configuration(sdk)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error)
                    .with_context(context("saving the replication configuration", bucket))
            })?;
        Ok(())
    }

    /// Stops replication; succeeds when the bucket did not replicate.
    pub async fn delete_bucket_replication(&self, bucket: &str) -> Result<()> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .delete_bucket_replication()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        delete_optional(
            reply,
            "ReplicationConfigurationNotFoundError",
            context("deleting the replication configuration", bucket),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(issues: &[RuleIssue]) -> Vec<String> {
        issues.iter().map(RuleIssue::to_string).collect()
    }

    fn rule_with_expiration(days: u32) -> LifecycleRule {
        LifecycleRule {
            id: Some("expire".into()),
            expiration: Some(ExpirationAction {
                days: Some(days),
                ..ExpirationAction::default()
            }),
            ..LifecycleRule::default()
        }
    }

    fn lifecycle(rule: LifecycleRule) -> LifecycleConfig {
        LifecycleConfig { rules: vec![rule] }
    }

    #[test]
    fn a_valid_lifecycle_has_no_issues() {
        let config = LifecycleConfig {
            rules: vec![
                LifecycleRule {
                    id: Some("archive".into()),
                    filter: LifecycleFilter {
                        prefix: Some("logs/".into()),
                        tags: vec![Tag {
                            key: "tier".into(),
                            value: "cold".into(),
                        }],
                        object_size_greater_than: Some(128),
                        object_size_less_than: Some(1_000_000),
                    },
                    transitions: vec![
                        LifecycleTransition {
                            days: Some(30),
                            storage_class: "STANDARD_IA".into(),
                            ..LifecycleTransition::default()
                        },
                        LifecycleTransition {
                            date: Some("2030-01-31".into()),
                            storage_class: "GLACIER".into(),
                            ..LifecycleTransition::default()
                        },
                    ],
                    expiration: Some(ExpirationAction {
                        days: Some(365),
                        ..ExpirationAction::default()
                    }),
                    noncurrent_version_transitions: vec![NoncurrentTransition {
                        noncurrent_days: 7,
                        newer_noncurrent_versions: Some(2),
                        storage_class: "GLACIER_IR".into(),
                    }],
                    noncurrent_version_expiration: Some(NoncurrentExpiration {
                        noncurrent_days: Some(90),
                        newer_noncurrent_versions: Some(3),
                    }),
                    ..LifecycleRule::default()
                },
                LifecycleRule {
                    id: Some("clean".into()),
                    expiration: Some(ExpirationAction {
                        expired_object_delete_marker: true,
                        ..ExpirationAction::default()
                    }),
                    abort_incomplete_multipart_upload_days: Some(7),
                    ..LifecycleRule::default()
                },
            ],
        };
        assert_eq!(paths(&config.check()), Vec::<String>::new());
        config.validate().unwrap();
    }

    #[test]
    fn lifecycle_problems_carry_their_paths() {
        assert_eq!(
            paths(&LifecycleConfig::default().check()),
            ["rules: add at least one rule; to remove every rule, delete the configuration"]
        );
        assert_eq!(
            paths(&lifecycle(LifecycleRule::default()).check()),
            [
                "rules[0]: has no action; add a transition, an expiration, a noncurrent version action or an abort of incomplete multipart uploads"
            ]
        );
        let config = lifecycle(LifecycleRule {
            transitions: vec![
                LifecycleTransition {
                    days: Some(10),
                    storage_class: "STANDARD_IA".into(),
                    ..LifecycleTransition::default()
                },
                LifecycleTransition {
                    days: Some(5),
                    date: Some("2030-01-01".into()),
                    storage_class: "STANDARD".into(),
                },
                LifecycleTransition {
                    storage_class: "GLACIER".into(),
                    ..LifecycleTransition::default()
                },
                LifecycleTransition {
                    date: Some("31/01/2030".into()),
                    storage_class: "GLACIER".into(),
                    ..LifecycleTransition::default()
                },
            ],
            ..LifecycleRule::default()
        });
        let found = paths(&config.check());
        assert_eq!(
            found,
            [
                "rules[0].transitions[0].days: objects stay at least 30 days in STANDARD before they move to STANDARD_IA",
                "rules[0].transitions[1]: set either days or date, not both",
                "rules[0].transitions[1].storage_class: \"STANDARD\" is not a transition target; use one of STANDARD_IA, ONEZONE_IA, INTELLIGENT_TIERING, GLACIER_IR, GLACIER, DEEP_ARCHIVE",
                "rules[0].transitions[2]: set days or date",
                "rules[0].transitions[3].date: \"31/01/2030\" is not a date; use YYYY-MM-DD (S3 runs the action at midnight UTC)",
                "rules[0].transitions[3].storage_class: GLACIER is the target of another transition of this rule",
            ]
        );
    }

    #[test]
    fn expiration_rules() {
        let mut both = rule_with_expiration(10);
        both.expiration.as_mut().unwrap().date = Some("2030-01-01".into());
        both.expiration
            .as_mut()
            .unwrap()
            .expired_object_delete_marker = true;
        assert_eq!(
            paths(&lifecycle(both).check()),
            ["rules[0].expiration: set only one of days, date and expired_object_delete_marker"]
        );
        let none = LifecycleRule {
            expiration: Some(ExpirationAction::default()),
            ..LifecycleRule::default()
        };
        assert_eq!(
            paths(&lifecycle(none).check()),
            ["rules[0].expiration: set days, date or expired_object_delete_marker"]
        );
        assert_eq!(
            paths(&lifecycle(rule_with_expiration(0)).check()),
            ["rules[0].expiration.days: must be at least 1"]
        );
        let mut late = rule_with_expiration(30);
        late.transitions = vec![LifecycleTransition {
            days: Some(60),
            storage_class: "GLACIER".into(),
            ..LifecycleTransition::default()
        }];
        assert_eq!(
            paths(&lifecycle(late).check()),
            [
                "rules[0].expiration.days: must be greater than the days of every transition (a transition uses 60)"
            ]
        );
        let tagged = LifecycleRule {
            filter: LifecycleFilter {
                tags: vec![Tag {
                    key: "k".into(),
                    value: "v".into(),
                }],
                ..LifecycleFilter::default()
            },
            expiration: Some(ExpirationAction {
                expired_object_delete_marker: true,
                ..ExpirationAction::default()
            }),
            abort_incomplete_multipart_upload_days: Some(3),
            ..LifecycleRule::default()
        };
        assert_eq!(
            paths(&lifecycle(tagged).check()),
            [
                "rules[0].expiration.expired_object_delete_marker: cannot be combined with tags in the filter",
                "rules[0].abort_incomplete_multipart_upload_days: cannot be combined with tags in the filter"
            ]
        );
    }

    #[test]
    fn noncurrent_filter_and_id_rules() {
        let config = LifecycleConfig {
            rules: vec![
                LifecycleRule {
                    id: Some("dup".into()),
                    noncurrent_version_transitions: vec![NoncurrentTransition {
                        noncurrent_days: 10,
                        newer_noncurrent_versions: Some(0),
                        storage_class: "STANDARD_IA".into(),
                    }],
                    noncurrent_version_expiration: Some(NoncurrentExpiration {
                        noncurrent_days: None,
                        newer_noncurrent_versions: Some(0),
                    }),
                    abort_incomplete_multipart_upload_days: Some(0),
                    filter: LifecycleFilter {
                        prefix: Some("p".repeat(2000)),
                        tags: vec![
                            Tag {
                                key: "a".into(),
                                value: "1".into(),
                            },
                            Tag {
                                key: "a".into(),
                                value: "2".into(),
                            },
                        ],
                        object_size_greater_than: Some(10),
                        object_size_less_than: Some(10),
                    },
                    ..LifecycleRule::default()
                },
                LifecycleRule {
                    id: Some("dup".into()),
                    abort_incomplete_multipart_upload_days: Some(1),
                    ..LifecycleRule::default()
                },
                LifecycleRule {
                    id: Some(String::new()),
                    abort_incomplete_multipart_upload_days: Some(1),
                    ..LifecycleRule::default()
                },
            ],
        };
        let found = paths(&config.check());
        for expected in [
            "rules[1].id: the id \"dup\" is used by another rule",
            "rules[2].id: must not be empty (leave it out to let S3 choose one)",
            "rules[0].filter.prefix: is longer than 1024 bytes",
            "rules[0].filter.tags[1].key: the tag key \"a\" is used twice",
            "rules[0].filter.object_size_less_than: must be larger than object_size_greater_than (10)",
            "rules[0].noncurrent_version_transitions[0].noncurrent_days: versions stay at least 30 days in STANDARD before they move to STANDARD_IA",
            "rules[0].noncurrent_version_transitions[0].newer_noncurrent_versions: must be at least 1",
            "rules[0].noncurrent_version_expiration.noncurrent_days: is required",
            "rules[0].noncurrent_version_expiration.newer_noncurrent_versions: must be at least 1",
            "rules[0].abort_incomplete_multipart_upload_days: must be at least 1",
        ] {
            assert!(
                found.contains(&expected.to_owned()),
                "{expected}\n{found:#?}"
            );
        }
        let error = config.validate().unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("invalid lifecycle configuration: ")
        );
    }

    #[test]
    fn lifecycle_json_rejects_unknown_fields() {
        let error =
            serde_json::from_str::<LifecycleConfig>(r#"{"rules":[{"idd":"x"}]}"#).unwrap_err();
        assert!(error.to_string().contains("unknown field `idd`"), "{error}");
        let parsed: LifecycleConfig = serde_json::from_str(
            r#"{"rules":[{"id":"a","expiration":{"days":3},"filter":{"prefix":"x/"}}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.rules[0].status, RuleStatus::Enabled);
        assert_eq!(parsed.rules[0].filter.prefix.as_deref(), Some("x/"));
    }

    #[test]
    fn tiering_checks() {
        let good = TieringConfig {
            id: "archive".into(),
            tierings: vec![
                TieringStep {
                    access_tier: AccessTier::ArchiveAccess,
                    days: 90,
                },
                TieringStep {
                    access_tier: AccessTier::DeepArchiveAccess,
                    days: 730,
                },
            ],
            ..TieringConfig::default()
        };
        assert!(good.check().is_empty());
        let bad = TieringConfig {
            id: "x".repeat(65),
            tierings: vec![
                TieringStep {
                    access_tier: AccessTier::ArchiveAccess,
                    days: 89,
                },
                TieringStep {
                    access_tier: AccessTier::ArchiveAccess,
                    days: 731,
                },
                TieringStep {
                    access_tier: AccessTier::DeepArchiveAccess,
                    days: 179,
                },
            ],
            ..TieringConfig::default()
        };
        assert_eq!(
            paths(&bad.check()),
            [
                "id: must be 1 to 64 characters",
                "tierings[0].days: ARCHIVE_ACCESS takes 90 to 730 days (found 89)",
                "tierings[1].access_tier: ARCHIVE_ACCESS is configured twice",
                "tierings[1].days: ARCHIVE_ACCESS takes 90 to 730 days (found 731)",
                "tierings[2].days: DEEP_ARCHIVE_ACCESS takes 180 to 730 days (found 179)",
            ]
        );
        assert_eq!(
            paths(&TieringConfig::default().check()),
            [
                "id: must be 1 to 64 characters",
                "tierings: add at least one archive tier (ARCHIVE_ACCESS or DEEP_ARCHIVE_ACCESS)"
            ]
        );
        let json = serde_json::to_string(&good.tierings[0]).unwrap();
        assert_eq!(json, r#"{"access_tier":"ARCHIVE_ACCESS","days":90}"#);
    }

    #[test]
    fn cors_checks() {
        let good = CorsConfig {
            rules: vec![CorsRule {
                id: Some("web".into()),
                allowed_origins: vec!["https://*.example.org".into()],
                allowed_methods: vec!["GET".into(), "PUT".into()],
                allowed_headers: vec!["*".into()],
                expose_headers: vec!["ETag".into()],
                max_age_seconds: Some(3000),
            }],
        };
        assert!(good.check().is_empty());
        let bad = CorsConfig {
            rules: vec![
                CorsRule::default(),
                CorsRule {
                    allowed_origins: vec!["*.*.org".into(), " ".into()],
                    allowed_methods: vec!["get".into(), "GET".into(), "GET".into()],
                    allowed_headers: vec!["x-*-*".into()],
                    expose_headers: vec![String::new()],
                    ..CorsRule::default()
                },
            ],
        };
        assert_eq!(
            paths(&bad.check()),
            [
                "rules[0].allowed_origins: allow at least one origin (use * for any)",
                "rules[0].allowed_methods: allow at least one method of GET, PUT, POST, DELETE, HEAD",
                "rules[1].allowed_origins[0]: \"*.*.org\" has more than one * wildcard",
                "rules[1].allowed_origins[1]: must not be empty",
                "rules[1].allowed_methods[0]: \"get\" is not allowed; use one of GET, PUT, POST, DELETE, HEAD (upper case)",
                "rules[1].allowed_methods[2]: GET is listed twice",
                "rules[1].allowed_headers[0]: \"x-*-*\" has more than one * wildcard",
                "rules[1].expose_headers[0]: must not be empty",
            ]
        );
        assert_eq!(
            paths(&CorsConfig::default().check()),
            ["rules: add at least one rule; to remove every rule, delete the configuration"]
        );
    }

    fn replication_rule() -> ReplicationRule {
        ReplicationRule {
            id: Some("all".into()),
            priority: 1,
            destination_bucket: "arn:aws:s3:::backup-bucket".into(),
            ..ReplicationRule::default()
        }
    }

    #[test]
    fn replication_checks() {
        let good = ReplicationConfig {
            role: "arn:aws:iam::123456789012:role/replicate".into(),
            rules: vec![replication_rule()],
        };
        assert!(good.check().is_empty());
        let bad = ReplicationConfig {
            role: "replicate".into(),
            rules: vec![
                ReplicationRule {
                    destination_bucket: "backup-bucket".into(),
                    storage_class: Some("COLD".into()),
                    ..replication_rule()
                },
                ReplicationRule {
                    destination_bucket: "arn:aws:s3:::Bad_Name".into(),
                    ..replication_rule()
                },
            ],
        };
        let found = paths(&bad.check());
        assert_eq!(found.len(), 5, "{found:#?}");
        assert!(found[0].starts_with("role: \"replicate\" is not an IAM role ARN"));
        assert_eq!(
            found[1],
            "rules[0].destination_bucket: \"backup-bucket\" is not a bucket ARN; use arn:aws:s3:::<bucket>"
        );
        assert!(
            found[2].starts_with("rules[0].storage_class: \"COLD\" is not a replica storage class")
        );
        assert_eq!(
            found[3],
            "rules[1].id: the id \"all\" is used by another rule"
        );
        assert_eq!(
            found[4],
            "rules[1].priority: the priority 1 is used by another rule"
        );
        assert_eq!(
            paths(&ReplicationConfig::default().check())[0],
            "role: is required: the ARN of the IAM role S3 uses to replicate (arn:aws:iam::<account>:role/<name>)"
        );
    }

    #[test]
    fn json_documents_are_checked_with_positions() {
        assert_eq!(
            paths(&check_lifecycle_json("  ")),
            ["the document is empty"]
        );
        let found = paths(&check_cors_json(r#"{"rules":[{"allowed_origin":["*"]}]}"#));
        assert_eq!(found.len(), 1);
        assert!(
            found[0].starts_with("not valid: unknown field `allowed_origin`"),
            "{found:?}"
        );
        let found = paths(&check_tiering_json("{"));
        assert!(found[0].contains("line 1 column 1"), "{found:?}");
        assert!(
            paths(&check_replication_json(r#"{"role":"","rules":[]}"#))
                .iter()
                .any(|m| m.starts_with("role: is required"))
        );
        assert!(
            check_lifecycle_json(r#"{"rules":[{"id":"a","expiration":{"days":2}}]}"#).is_empty()
        );
    }

    #[test]
    fn bucket_arns_are_parsed() {
        assert_eq!(bucket_from_arn("arn:aws:s3:::name"), Some("name"));
        assert_eq!(bucket_from_arn("arn:aws-cn:s3:::name"), Some("name"));
        assert_eq!(bucket_from_arn("arn:aws:s3:::name/key"), None);
        assert_eq!(bucket_from_arn("arn:aws:s3:::"), None);
        assert_eq!(bucket_from_arn("name"), None);
    }

    #[test]
    fn dates_are_midnight_utc() {
        assert!(parse_date("2030-02-28").is_some());
        assert!(parse_date("2030-02-30").is_none());
        assert!(parse_date("2030-2-3").is_none());
        let date = parse_date("2030-12-31").unwrap();
        assert_eq!(date_from_sdk(&date).as_deref(), Some("2030-12-31"));
    }
}
