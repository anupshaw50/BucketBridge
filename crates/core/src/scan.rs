//! Security scan (T13): reads the access-related configuration of buckets and
//! reports what is risky, with a severity, an explanation and the bucket
//! Properties tab that fixes it.
//!
//! # Checks
//!
//! | Finding ([`FindingId`]) | Source | Severity |
//! |---|---|---|
//! | `public_acl` | `GetBucketAcl`: a grant to `AllUsers` or `AuthenticatedUsers` | critical (write access to everyone), high (read access), medium (any signed-in AWS user) |
//! | `public_policy` | `GetBucketPolicyStatus` (`IsPublic`); when the server does not implement it, a conservative reading of `GetBucketPolicy` | critical |
//! | `public_access_block_off` | `GetPublicAccessBlock`: none configured, or all four toggles off | high |
//! | `public_access_block_partial` | `GetPublicAccessBlock`: some toggles off | medium |
//! | `no_default_encryption` | `GetBucketEncryption`: none configured | medium |
//! | `versioning_off`, `versioning_suspended` | `GetBucketVersioning` | low |
//! | `acls_enabled` | `GetBucketOwnershipControls`: not `BucketOwnerEnforced` | low |
//!
//! The policy text check only flags what is certainly public: an `Allow`
//! statement for the principal `*` (also `{"AWS": "*"}`) without a
//! `Condition`. A policy that restricts the principal by a condition is never
//! reported by it, so it has no false alarms; [`GetBucketPolicyStatus`]
//! (AWS's own analysis) is the authority wherever it exists.
//!
//! # Failures
//!
//! A check that fails (access denied, a server that does not implement the
//! API) is listed in [`BucketScan::skipped`] and never hides the findings of
//! the other checks. A bucket that cannot be reached at all gets
//! [`BucketScan::error`]. Neither stops the scan of the other buckets.
//!
//! # Reports
//!
//! [`write_report`] saves a [`ScanReport`] as CSV or JSON, replacing the
//! file atomically.
//!
//! [`GetBucketPolicyStatus`]: https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetBucketPolicyStatus.html

use std::fmt;
use std::io::Write;
use std::path::Path;

use aws_sdk_s3::types::{ObjectOwnership, Permission};
use chrono::{DateTime, SecondsFormat, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bucketcfg::{
    BucketVersioning, PublicAccessBlock, VersioningStatus, context, is_unsupported, read_optional,
};
use crate::error::{BbError, Error, Result};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// Buckets scanned at the same time.
pub const SCAN_CONCURRENCY: usize = 4;

const ALL_USERS: &str = "http://acs.amazonaws.com/groups/global/AllUsers";
const AUTHENTICATED_USERS: &str = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing, no direct exposure.
    Low,
    /// A missing safeguard.
    Medium,
    /// Data may be readable by anyone, or a safeguard against that is off.
    High,
    /// Anyone on the internet can read or change data.
    Critical,
}

impl Severity {
    /// The name used in reports and by the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// The kind of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingId {
    /// The bucket ACL grants access to everyone or to every AWS account.
    PublicAcl,
    /// The bucket policy makes the bucket public.
    PublicPolicy,
    /// No public access block, or every toggle is off.
    PublicAccessBlockOff,
    /// Some public access block toggles are off.
    PublicAccessBlockPartial,
    /// No default encryption is configured.
    NoDefaultEncryption,
    /// Versioning was never enabled.
    VersioningOff,
    /// Versioning is suspended.
    VersioningSuspended,
    /// Object ownership allows ACLs.
    AclsEnabled,
}

/// The bucket Properties tab that fixes a finding. `public-access`, `policy`
/// and `versioning` exist in the bucket properties dialog; the others name
/// the tab a build that has an editor for the setting uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixTab {
    /// Bucket policy.
    Policy,
    /// Public access block.
    PublicAccess,
    /// Versioning.
    Versioning,
    /// Default encryption.
    Encryption,
    /// Object ownership and ACLs.
    Ownership,
}

impl FixTab {
    /// The tab's id, as the UI and reports spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            FixTab::Policy => "policy",
            FixTab::PublicAccess => "public-access",
            FixTab::Versioning => "versioning",
            FixTab::Encryption => "encryption",
            FixTab::Ownership => "ownership",
        }
    }
}

/// One risk found on a bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The kind of finding.
    pub id: FindingId,
    /// How serious it is.
    pub severity: Severity,
    /// One line.
    pub title: String,
    /// What is wrong and why it matters, with what to do.
    pub explanation: String,
    /// The bucket Properties tab that fixes it.
    pub fix_tab: FixTab,
}

/// The checks of the scan, for naming the ones that were skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// The bucket ACL.
    Acl,
    /// The bucket policy and its public status.
    Policy,
    /// The public access block.
    PublicAccessBlock,
    /// Default encryption.
    Encryption,
    /// Versioning.
    Versioning,
    /// Object ownership.
    Ownership,
}

impl Check {
    /// Every check, in report order.
    pub const ALL: [Check; 6] = [
        Check::Acl,
        Check::Policy,
        Check::PublicAccessBlock,
        Check::Encryption,
        Check::Versioning,
        Check::Ownership,
    ];

    /// What the check looks at, for messages.
    pub fn label(self) -> &'static str {
        match self {
            Check::Acl => "the bucket ACL",
            Check::Policy => "the bucket policy",
            Check::PublicAccessBlock => "the public access block",
            Check::Encryption => "default encryption",
            Check::Versioning => "versioning",
            Check::Ownership => "object ownership",
        }
    }
}

/// A check that could not run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCheck {
    /// Which check.
    pub check: Check,
    /// `true` when the server does not implement the API (nothing to fix on
    /// the user's side), `false` for a failure such as access denied.
    pub unsupported: bool,
    /// Why, for display.
    pub message: String,
}

/// The outcome for one bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketScan {
    /// Bucket name.
    pub bucket: String,
    /// The risks found, most severe first.
    pub findings: Vec<Finding>,
    /// Checks that could not run.
    pub skipped: Vec<SkippedCheck>,
    /// Set when the bucket could not be scanned at all.
    pub error: Option<BbError>,
}

/// The outcome of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// When the scan finished, RFC 3339 in UTC.
    pub generated_at: String,
    /// One entry per scanned bucket, in the order requested.
    pub buckets: Vec<BucketScan>,
}

impl ScanReport {
    /// Every finding with its bucket, most severe first (then by bucket).
    pub fn rows(&self) -> Vec<(&str, &Finding)> {
        let mut rows: Vec<(&str, &Finding)> = self
            .buckets
            .iter()
            .flat_map(|scan| scan.findings.iter().map(|f| (scan.bucket.as_str(), f)))
            .collect();
        rows.sort_by(|a, b| b.1.severity.cmp(&a.1.severity).then(a.0.cmp(b.0)));
        rows
    }

    /// The number of findings of `severity`.
    pub fn count(&self, severity: Severity) -> usize {
        self.buckets
            .iter()
            .flat_map(|scan| &scan.findings)
            .filter(|f| f.severity == severity)
            .count()
    }

    /// The highest severity found, if any.
    pub fn worst(&self) -> Option<Severity> {
        self.buckets
            .iter()
            .flat_map(|scan| &scan.findings)
            .map(|f| f.severity)
            .max()
    }

    /// Buckets that could not be scanned at all.
    pub fn failed_buckets(&self) -> usize {
        self.buckets.iter().filter(|b| b.error.is_some()).count()
    }
}

// ---- what the checks read ----

/// How a grant's permission reaches the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grantee {
    AllUsers,
    AuthenticatedUsers,
}

/// A grant of the bucket ACL to a public group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PublicGrant {
    grantee: Grantee,
    /// The grantee can write objects or change the ACL.
    can_change: bool,
}

/// What `GetBucketOwnershipControls` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    /// `BucketOwnerEnforced`: ACLs are disabled.
    Enforced,
    /// ACLs are honoured (`BucketOwnerPreferred`, `ObjectWriter`, or the
    /// legacy default of a bucket without ownership controls).
    AclsEnabled,
}

/// Everything the checks read; `None` where a check was skipped.
#[derive(Debug, Default)]
struct Observed {
    public_grants: Option<Vec<PublicGrant>>,
    /// `Some(true)` when the policy is public.
    public_policy: Option<bool>,
    public_access_block: Option<Option<PublicAccessBlock>>,
    encrypted: Option<bool>,
    versioning: Option<BucketVersioning>,
    ownership: Option<Ownership>,
}

/// The findings for what was observed, most severe first.
fn evaluate(observed: &Observed) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(finding) = observed.public_grants.as_deref().and_then(acl_finding) {
        findings.push(finding);
    }
    if observed.public_policy == Some(true) {
        findings.push(Finding {
            id: FindingId::PublicPolicy,
            severity: Severity::Critical,
            title: "The bucket policy makes the bucket public".to_owned(),
            explanation: "A statement of the bucket policy allows everyone (the principal \"*\") \
                          without a condition. Restrict the principal to the accounts or roles \
                          that need access, or add a condition, and turn on Block public policy \
                          in the public access block."
                .to_owned(),
            fix_tab: FixTab::Policy,
        });
    }
    if let Some(block) = &observed.public_access_block {
        findings.extend(block_finding(block.as_ref()));
    }
    if observed.encrypted == Some(false) {
        findings.push(Finding {
            id: FindingId::NoDefaultEncryption,
            severity: Severity::Medium,
            title: "No default encryption".to_owned(),
            explanation: "Objects written without an encryption header are stored unencrypted. \
                          Configure default encryption (SSE-S3 or SSE-KMS) so that every new \
                          object is encrypted at rest."
                .to_owned(),
            fix_tab: FixTab::Encryption,
        });
    }
    if let Some(versioning) = &observed.versioning {
        match versioning.status {
            VersioningStatus::Enabled => {}
            VersioningStatus::Unversioned => findings.push(Finding {
                id: FindingId::VersioningOff,
                severity: Severity::Low,
                title: "Versioning is not enabled".to_owned(),
                explanation: "An overwritten or deleted object cannot be recovered. Enable \
                              versioning to keep earlier versions (and consider a lifecycle rule \
                              that expires old ones)."
                    .to_owned(),
                fix_tab: FixTab::Versioning,
            }),
            VersioningStatus::Suspended => findings.push(Finding {
                id: FindingId::VersioningSuspended,
                severity: Severity::Low,
                title: "Versioning is suspended".to_owned(),
                explanation: "New objects get no version history, so an overwrite or delete \
                              cannot be undone. Enable versioning again to keep earlier versions."
                    .to_owned(),
                fix_tab: FixTab::Versioning,
            }),
        }
    }
    if observed.ownership == Some(Ownership::AclsEnabled) {
        findings.push(Finding {
            id: FindingId::AclsEnabled,
            severity: Severity::Low,
            title: "Object ownership allows ACLs".to_owned(),
            explanation: "ACLs on the bucket and its objects are honoured, which makes access \
                          harder to audit and lets a public ACL be added by mistake. Set object \
                          ownership to Bucket owner enforced and manage access with policies."
                .to_owned(),
            fix_tab: FixTab::Ownership,
        });
    }
    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    findings
}

fn acl_finding(grants: &[PublicGrant]) -> Option<Finding> {
    let everyone = grants.iter().filter(|g| g.grantee == Grantee::AllUsers);
    let signed_in = grants
        .iter()
        .filter(|g| g.grantee == Grantee::AuthenticatedUsers);
    let (severity, who, what) = if everyone.clone().any(|g| g.can_change) {
        (
            Severity::Critical,
            "everyone",
            "write objects or change the ACL",
        )
    } else if everyone.clone().next().is_some() {
        (Severity::High, "everyone", "list the objects")
    } else if signed_in.clone().any(|g| g.can_change) {
        (
            Severity::High,
            "any AWS account",
            "write objects or change the ACL",
        )
    } else if signed_in.clone().next().is_some() {
        (Severity::Medium, "any AWS account", "list the objects")
    } else {
        return None;
    };
    Some(Finding {
        id: FindingId::PublicAcl,
        severity,
        title: "The bucket ACL grants public access".to_owned(),
        explanation: format!(
            "The ACL lets {who} {what}. Remove the grant and turn on Block public ACLs and \
             Ignore public ACLs in the public access block."
        ),
        fix_tab: FixTab::PublicAccess,
    })
}

fn block_finding(block: Option<&PublicAccessBlock>) -> Option<Finding> {
    let toggles = |b: &PublicAccessBlock| {
        [
            b.block_public_acls,
            b.ignore_public_acls,
            b.block_public_policy,
            b.restrict_public_buckets,
        ]
    };
    match block {
        Some(block) if toggles(block).iter().all(|on| *on) => None,
        Some(block) if toggles(block).iter().any(|on| *on) => {
            let missing: Vec<&str> = [
                (block.block_public_acls, "Block public ACLs"),
                (block.ignore_public_acls, "Ignore public ACLs"),
                (block.block_public_policy, "Block public policy"),
                (block.restrict_public_buckets, "Restrict public buckets"),
            ]
            .into_iter()
            .filter(|(on, _)| !on)
            .map(|(_, name)| name)
            .collect();
            Some(Finding {
                id: FindingId::PublicAccessBlockPartial,
                severity: Severity::Medium,
                title: "The public access block is only partly on".to_owned(),
                explanation: format!(
                    "These toggles are off: {}. Turn all four on unless the bucket is meant to \
                     be public.",
                    missing.join(", ")
                ),
                fix_tab: FixTab::PublicAccess,
            })
        }
        _ => Some(Finding {
            id: FindingId::PublicAccessBlockOff,
            severity: Severity::High,
            title: "The public access block is off".to_owned(),
            explanation: "Nothing stops a public ACL or policy from being added to this bucket. \
                          Turn on all four public access block toggles unless the bucket is meant \
                          to be public."
                .to_owned(),
            fix_tab: FixTab::PublicAccess,
        }),
    }
}

/// Whether a policy document certainly makes the bucket public: an `Allow`
/// statement for the principal `*` (or `{"AWS": "*"}`) without a
/// `Condition`. Text that is not a JSON policy is not reported.
pub fn policy_allows_everyone(text: &str) -> bool {
    let Ok(document) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let statements = match document.get("Statement") {
        Some(Value::Array(items)) => items.iter().collect::<Vec<_>>(),
        Some(statement @ Value::Object(_)) => vec![statement],
        _ => return false,
    };
    statements.into_iter().any(|statement| {
        let allow = statement.get("Effect").and_then(Value::as_str) == Some("Allow");
        let no_condition = match statement.get("Condition") {
            None | Some(Value::Null) => true,
            Some(Value::Object(map)) => map.is_empty(),
            Some(_) => false,
        };
        allow && no_condition && statement.get("Principal").is_some_and(names_everyone)
    })
}

fn names_everyone(principal: &Value) -> bool {
    match principal {
        Value::String(text) => text == "*",
        Value::Object(map) => map.get("AWS").is_some_and(|aws| match aws {
            Value::String(text) => text == "*",
            Value::Array(items) => items.iter().any(|item| item.as_str() == Some("*")),
            _ => false,
        }),
        _ => false,
    }
}

// ---- reading ----

fn skipped(check: Check, error: &Error) -> SkippedCheck {
    let unsupported = is_unsupported(error);
    let message = if unsupported {
        format!("This server does not support reading {}.", check.label())
    } else {
        format!("Could not read {}: {error}", check.label())
    };
    SkippedCheck {
        check,
        unsupported,
        message,
    }
}

impl S3 {
    /// Scans one bucket. Never fails: a check that cannot run is listed in
    /// [`BucketScan::skipped`], a bucket that cannot be reached in
    /// [`BucketScan::error`].
    pub async fn scan_bucket(&self, bucket: &str) -> BucketScan {
        let mut scan = BucketScan {
            bucket: bucket.to_owned(),
            findings: Vec::new(),
            skipped: Vec::new(),
            error: None,
        };
        if let Err(error) = validate_bucket_name(bucket) {
            scan.error = Some(error.to_payload());
            return scan;
        }
        // Resolves the region once, so an unreachable bucket is reported once.
        if let Err(error) = self.client_for(bucket).await {
            scan.error = Some(
                error
                    .with_context(format!("bucket '{bucket}'"))
                    .to_payload(),
            );
            return scan;
        }
        let (acl, policy, block, encryption, versioning, ownership) = tokio::join!(
            self.read_public_grants(bucket),
            self.read_policy_public(bucket),
            self.get_public_access_block(bucket),
            self.read_encrypted(bucket),
            self.get_bucket_versioning(bucket),
            self.read_ownership(bucket),
        );
        let mut observed = Observed::default();
        keep(
            Check::Acl,
            acl,
            &mut observed.public_grants,
            &mut scan.skipped,
        );
        keep(
            Check::Policy,
            policy,
            &mut observed.public_policy,
            &mut scan.skipped,
        );
        keep(
            Check::PublicAccessBlock,
            block,
            &mut observed.public_access_block,
            &mut scan.skipped,
        );
        keep(
            Check::Encryption,
            encryption,
            &mut observed.encrypted,
            &mut scan.skipped,
        );
        keep(
            Check::Versioning,
            versioning,
            &mut observed.versioning,
            &mut scan.skipped,
        );
        keep(
            Check::Ownership,
            ownership,
            &mut observed.ownership,
            &mut scan.skipped,
        );
        scan.skipped.sort_by_key(|s| {
            Check::ALL
                .iter()
                .position(|c| *c == s.check)
                .unwrap_or(usize::MAX)
        });
        scan.findings = evaluate(&observed);
        scan
    }

    async fn read_public_grants(&self, bucket: &str) -> Result<Vec<PublicGrant>> {
        let out = self
            .client_for(bucket)
            .await?
            .get_bucket_acl()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| Error::from(error).with_context(context("reading the ACL", bucket)))?;
        let mut found = Vec::new();
        for grant in out.grants() {
            let grantee = match grant.grantee().and_then(|g| g.uri()) {
                Some(ALL_USERS) => Grantee::AllUsers,
                Some(AUTHENTICATED_USERS) => Grantee::AuthenticatedUsers,
                _ => continue,
            };
            let can_change = matches!(
                grant.permission(),
                Some(Permission::Write | Permission::WriteAcp | Permission::FullControl)
            );
            found.push(PublicGrant {
                grantee,
                can_change,
            });
        }
        Ok(found)
    }

    /// Whether the bucket policy is public: AWS's own answer where the
    /// server has one, otherwise a reading of the policy text.
    async fn read_policy_public(&self, bucket: &str) -> Result<bool> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_policy_status()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        match reply {
            Ok(out) => Ok(out
                .policy_status()
                .and_then(|status| status.is_public())
                .unwrap_or(false)),
            Err(error) => {
                let error = Error::from(error);
                if error.service().and_then(|s| s.code()) == Some("NoSuchBucketPolicy") {
                    return Ok(false);
                }
                if !is_unsupported(&error) {
                    return Err(error.with_context(context("reading the policy status", bucket)));
                }
                let text = self.get_bucket_policy(bucket).await?;
                Ok(text.is_some_and(|text| policy_allows_everyone(&text)))
            }
        }
    }

    async fn read_encrypted(&self, bucket: &str) -> Result<bool> {
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
            context("reading the encryption", bucket),
        )?;
        Ok(out.is_some_and(|out| {
            out.server_side_encryption_configuration()
                .is_some_and(|config| !config.rules().is_empty())
        }))
    }

    async fn read_ownership(&self, bucket: &str) -> Result<Ownership> {
        let client = self.client_for(bucket).await?;
        let reply = client
            .get_bucket_ownership_controls()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await;
        let out = read_optional(
            reply,
            "OwnershipControlsNotFoundError",
            context("reading the object ownership", bucket),
        )?;
        let enforced = out.is_some_and(|out| {
            out.ownership_controls().is_some_and(|controls| {
                controls
                    .rules()
                    .iter()
                    .any(|rule| *rule.object_ownership() == ObjectOwnership::BucketOwnerEnforced)
            })
        });
        Ok(if enforced {
            Ownership::Enforced
        } else {
            Ownership::AclsEnabled
        })
    }
}

fn keep<T>(
    check: Check,
    result: Result<T>,
    slot: &mut Option<T>,
    skipped_checks: &mut Vec<SkippedCheck>,
) {
    match result {
        Ok(value) => *slot = Some(value),
        Err(error) => skipped_checks.push(skipped(check, &error)),
    }
}

/// Scans `buckets` (four at a time) and returns one [`BucketScan`] per
/// bucket in the same order, duplicates removed.
pub async fn scan_buckets(s3: &S3, buckets: &[String]) -> ScanReport {
    let mut names: Vec<String> = Vec::with_capacity(buckets.len());
    for bucket in buckets {
        if !names.contains(bucket) {
            names.push(bucket.clone());
        }
    }
    let scans = futures::stream::iter(names)
        .map(|bucket| {
            let s3 = s3.clone();
            async move { s3.scan_bucket(&bucket).await }
        })
        .buffered(SCAN_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    ScanReport {
        generated_at: timestamp(Utc::now()),
        buckets: scans,
    }
}

/// Scans every bucket of the account. Fails only when the bucket list
/// cannot be read.
pub async fn scan_account(s3: &S3) -> Result<ScanReport> {
    let names: Vec<String> = s3
        .list_buckets()
        .await?
        .into_iter()
        .map(|bucket| bucket.name)
        .collect();
    Ok(scan_buckets(s3, &names).await)
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

// ---- reports ----

/// File formats of [`write_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    /// One row per finding, then one row per skipped check or failed bucket.
    Csv,
    /// The [`ScanReport`] as JSON.
    Json,
}

impl ReportFormat {
    /// The file extension without a dot.
    pub fn extension(self) -> &'static str {
        match self {
            ReportFormat::Csv => "csv",
            ReportFormat::Json => "json",
        }
    }
}

impl fmt::Display for ReportFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

/// The report as text in `format`.
pub fn render_report(report: &ScanReport, format: ReportFormat) -> Result<Vec<u8>> {
    match format {
        ReportFormat::Json => serde_json::to_vec_pretty(report)
            .map_err(|error| Error::Internal(format!("rendering the scan report: {error}"))),
        ReportFormat::Csv => {
            let mut writer = csv::Writer::from_writer(Vec::new());
            let fail =
                |error: csv::Error| Error::Internal(format!("rendering the scan report: {error}"));
            writer
                .write_record([
                    "bucket",
                    "severity",
                    "finding",
                    "title",
                    "fix_tab",
                    "explanation",
                ])
                .map_err(fail)?;
            for (bucket, finding) in report.rows() {
                let id = serde_json::to_value(finding.id)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                writer
                    .write_record([
                        bucket,
                        finding.severity.as_str(),
                        &id,
                        &finding.title,
                        finding.fix_tab.as_str(),
                        &finding.explanation,
                    ])
                    .map_err(fail)?;
            }
            for scan in &report.buckets {
                for skip in &scan.skipped {
                    writer
                        .write_record([&scan.bucket, "", "skipped", &skip.message, "", ""])
                        .map_err(fail)?;
                }
                if let Some(error) = &scan.error {
                    writer
                        .write_record([&scan.bucket, "", "error", &error.message, "", ""])
                        .map_err(fail)?;
                }
            }
            writer
                .into_inner()
                .map_err(|error| Error::Internal(format!("rendering the scan report: {error}")))
        }
    }
}

/// Saves the report to `path` (replaced atomically). Blocking.
pub fn write_report(report: &ScanReport, format: ReportFormat, path: &Path) -> Result<()> {
    use crate::error::IoContext;
    let bytes = render_report(report, format)?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut file = tempfile::NamedTempFile::new_in(dir)
        .io_context(|| format!("creating a file in {}", dir.display()))?;
    file.write_all(&bytes)
        .io_context(|| format!("writing {}", path.display()))?;
    file.persist(path)
        .map_err(|error| error.error)
        .io_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_good() -> Observed {
        Observed {
            public_grants: Some(vec![]),
            public_policy: Some(false),
            public_access_block: Some(Some(PublicAccessBlock {
                block_public_acls: true,
                ignore_public_acls: true,
                block_public_policy: true,
                restrict_public_buckets: true,
            })),
            encrypted: Some(true),
            versioning: Some(BucketVersioning {
                status: VersioningStatus::Enabled,
                mfa_delete: false,
            }),
            ownership: Some(Ownership::Enforced),
        }
    }

    fn ids(observed: &Observed) -> Vec<FindingId> {
        evaluate(observed).into_iter().map(|f| f.id).collect()
    }

    #[test]
    fn a_clean_bucket_has_no_findings() {
        assert!(evaluate(&all_good()).is_empty());
    }

    #[test]
    fn nothing_observed_means_nothing_reported() {
        assert!(evaluate(&Observed::default()).is_empty());
    }

    #[test]
    fn acl_severity_follows_grantee_and_permission() {
        let level = |grants: Vec<PublicGrant>| {
            let mut o = all_good();
            o.public_grants = Some(grants);
            evaluate(&o).first().map(|f| f.severity)
        };
        let grant = |grantee, can_change| PublicGrant {
            grantee,
            can_change,
        };
        assert_eq!(
            level(vec![grant(Grantee::AllUsers, true)]),
            Some(Severity::Critical)
        );
        assert_eq!(
            level(vec![grant(Grantee::AllUsers, false)]),
            Some(Severity::High)
        );
        assert_eq!(
            level(vec![grant(Grantee::AuthenticatedUsers, true)]),
            Some(Severity::High)
        );
        assert_eq!(
            level(vec![grant(Grantee::AuthenticatedUsers, false)]),
            Some(Severity::Medium)
        );
        assert_eq!(level(vec![]), None);
        // The worst grant decides.
        assert_eq!(
            level(vec![
                grant(Grantee::AuthenticatedUsers, false),
                grant(Grantee::AllUsers, true)
            ]),
            Some(Severity::Critical)
        );
    }

    #[test]
    fn each_finding_names_its_tab() {
        let mut o = all_good();
        o.public_grants = Some(vec![PublicGrant {
            grantee: Grantee::AllUsers,
            can_change: false,
        }]);
        o.public_policy = Some(true);
        o.public_access_block = Some(None);
        o.encrypted = Some(false);
        o.versioning = Some(BucketVersioning::default());
        o.ownership = Some(Ownership::AclsEnabled);
        let found = evaluate(&o);
        let tab = |id| found.iter().find(|f| f.id == id).map(|f| f.fix_tab);
        assert_eq!(tab(FindingId::PublicAcl), Some(FixTab::PublicAccess));
        assert_eq!(tab(FindingId::PublicPolicy), Some(FixTab::Policy));
        assert_eq!(
            tab(FindingId::PublicAccessBlockOff),
            Some(FixTab::PublicAccess)
        );
        assert_eq!(
            tab(FindingId::NoDefaultEncryption),
            Some(FixTab::Encryption)
        );
        assert_eq!(tab(FindingId::VersioningOff), Some(FixTab::Versioning));
        assert_eq!(tab(FindingId::AclsEnabled), Some(FixTab::Ownership));
        // Most severe first.
        assert!(found.windows(2).all(|w| w[0].severity >= w[1].severity));
        assert_eq!(found[0].id, FindingId::PublicPolicy);
    }

    #[test]
    fn public_access_block_states() {
        let on = PublicAccessBlock {
            block_public_acls: true,
            ignore_public_acls: true,
            block_public_policy: true,
            restrict_public_buckets: true,
        };
        let with = |block: Option<PublicAccessBlock>| {
            let mut o = all_good();
            o.public_access_block = Some(block);
            ids(&o)
        };
        assert!(with(Some(on)).is_empty());
        assert_eq!(with(None), [FindingId::PublicAccessBlockOff]);
        assert_eq!(
            with(Some(PublicAccessBlock::default())),
            [FindingId::PublicAccessBlockOff]
        );
        let partial = PublicAccessBlock {
            restrict_public_buckets: false,
            ..on
        };
        assert_eq!(with(Some(partial)), [FindingId::PublicAccessBlockPartial]);
        let found = {
            let mut o = all_good();
            o.public_access_block = Some(Some(partial));
            evaluate(&o)
        };
        assert!(found[0].explanation.contains("Restrict public buckets"));
        assert!(!found[0].explanation.contains("Block public ACLs"));
    }

    #[test]
    fn versioning_states() {
        let with = |status| {
            let mut o = all_good();
            o.versioning = Some(BucketVersioning {
                status,
                mfa_delete: false,
            });
            ids(&o)
        };
        assert!(with(VersioningStatus::Enabled).is_empty());
        assert_eq!(
            with(VersioningStatus::Unversioned),
            [FindingId::VersioningOff]
        );
        assert_eq!(
            with(VersioningStatus::Suspended),
            [FindingId::VersioningSuspended]
        );
    }

    #[test]
    fn policy_text_check_is_conservative() {
        let public = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::b/*"}]}"#;
        assert!(policy_allows_everyone(public));
        let aws_star = public.replace(r#""Principal":"*""#, r#""Principal":{"AWS":"*"}"#);
        assert!(policy_allows_everyone(&aws_star));
        let in_list = public.replace(r#""Principal":"*""#, r#""Principal":{"AWS":["x","*"]}"#);
        assert!(policy_allows_everyone(&in_list));
        let single = r#"{"Statement":{"Effect":"Allow","Principal":"*","Action":"s3:*"}}"#;
        assert!(policy_allows_everyone(single));

        let conditional = public.replace(
            r#""Resource""#,
            r#""Condition":{"IpAddress":{"aws:SourceIp":"10.0.0.0/8"}},"Resource""#,
        );
        assert!(!policy_allows_everyone(&conditional));
        let deny = public.replace("Allow", "Deny");
        assert!(!policy_allows_everyone(&deny));
        let named = public.replace(
            r#""Principal":"*""#,
            r#""Principal":{"AWS":"arn:aws:iam::1:root"}"#,
        );
        assert!(!policy_allows_everyone(&named));
        assert!(!policy_allows_everyone("not json"));
        assert!(!policy_allows_everyone(r#"{"Statement":"x"}"#));
    }

    #[test]
    fn reports_render() {
        let report = ScanReport {
            generated_at: "2026-01-01T00:00:00Z".to_owned(),
            buckets: vec![
                BucketScan {
                    bucket: "alpha".to_owned(),
                    findings: evaluate(&{
                        let mut o = all_good();
                        o.encrypted = Some(false);
                        o
                    }),
                    skipped: vec![SkippedCheck {
                        check: Check::Acl,
                        unsupported: false,
                        message: "Could not read the bucket ACL: access denied".to_owned(),
                    }],
                    error: None,
                },
                BucketScan {
                    bucket: "beta".to_owned(),
                    findings: vec![],
                    skipped: vec![],
                    error: Some(BbError {
                        code: crate::ErrorKind::Remote,
                        message: "no answer".to_owned(),
                    }),
                },
            ],
        };
        let csv = String::from_utf8(render_report(&report, ReportFormat::Csv).unwrap()).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "bucket,severity,finding,title,fix_tab,explanation"
        );
        assert!(
            lines[1].starts_with(
                "alpha,medium,no_default_encryption,No default encryption,encryption,"
            )
        );
        assert!(lines[2].starts_with("alpha,,skipped,"));
        assert!(lines[3].starts_with("beta,,error,no answer"));
        let json = render_report(&report, ReportFormat::Json).unwrap();
        let back: ScanReport = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, report);
        assert_eq!(report.count(Severity::Medium), 1);
        assert_eq!(report.worst(), Some(Severity::Medium));
        assert_eq!(report.failed_buckets(), 1);
    }

    #[test]
    fn report_files_are_replaced_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, "old").unwrap();
        let report = ScanReport {
            generated_at: "2026-01-01T00:00:00Z".to_owned(),
            buckets: vec![],
        };
        write_report(&report, ReportFormat::Json, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("generated_at"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
