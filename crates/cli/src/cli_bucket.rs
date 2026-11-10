//! The command-line definition of `bb bucket`: settings of one bucket
//! (default encryption, tags, ACLs, Object Lock).

use std::str::FromStr;

use bucketbridge_core::objectmeta::{
    ALL_USERS_URI, AUTHENTICATED_USERS_URI, AclPermission, CannedAcl, GranteeKind,
    LOG_DELIVERY_URI, validate_tag,
};
use bucketbridge_core::{Error, S3Uri};
use clap::{Args, Subcommand, ValueEnum};

use crate::location::{parse_bucket, parse_s3_uri};

/// `bb bucket`
#[derive(Debug, Subcommand)]
pub enum BucketCommand {
    /// Read, set and remove the default encryption of a bucket.
    #[command(subcommand)]
    Encryption(EncryptionCommand),
    /// Read, set and remove the tags of a bucket (cost allocation).
    #[command(subcommand)]
    Tags(BucketTagsCommand),
    /// Read and change the ACL of a bucket, grant by grant.
    #[command(subcommand)]
    Acl(BucketAclCommand),
    /// Read and turn on Object Lock of a bucket.
    #[command(subcommand)]
    Lock(BucketLockCommand),
}

/// `bb bucket lock`
#[derive(Debug, Subcommand)]
pub enum BucketLockCommand {
    /// Print whether Object Lock is on for a bucket and its default retention.
    Get(BucketArg),
    /// Turn Object Lock on for an existing bucket (needs versioning; it can never be turned off).
    Enable(BucketLockEnableArgs),
}

/// `bb bucket lock enable`
#[derive(Debug, Args)]
pub struct BucketLockEnableArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Confirm that Object Lock cannot be turned off again.
    #[arg(long)]
    pub yes: bool,
}

/// The bucket a `bb bucket` command works on.
#[derive(Debug, Args)]
pub struct BucketArg {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
}

/// `bb bucket encryption`
#[derive(Debug, Subcommand)]
pub enum EncryptionCommand {
    /// Print the default encryption of a bucket.
    Get(BucketArg),
    /// Set the default encryption: SSE-S3 or SSE-KMS.
    Set(EncryptionSetArgs),
    /// Remove the default encryption setting.
    Delete(BucketArg),
}

/// The algorithm of `bb bucket encryption set --sse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BucketSseMode {
    /// SSE-S3: keys managed by S3.
    S3,
    /// SSE-KMS: AWS Key Management Service keys.
    Kms,
}

/// `bb bucket encryption set`
#[derive(Debug, Args)]
pub struct EncryptionSetArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// The algorithm: s3 (SSE-S3) or kms (SSE-KMS).
    #[arg(long, value_enum, value_name = "s3|kms")]
    pub sse: BucketSseMode,
    /// With kms: a KMS key ID, key ARN, alias name or alias ARN (default: the AWS managed key).
    #[arg(long, value_name = "KEY")]
    pub kms_key: Option<String>,
    /// With kms: use an S3 Bucket Key to reduce the requests to KMS.
    #[arg(long)]
    pub bucket_key: bool,
}

/// `bb bucket tags`
#[derive(Debug, Subcommand)]
pub enum BucketTagsCommand {
    /// Print the tags of a bucket.
    Get(BucketArg),
    /// Add tags to a bucket; with --replace make them the whole tag set.
    Set(BucketTagsSetArgs),
    /// Remove the named tags from a bucket, or every tag when none is named.
    Delete(BucketTagsDeleteArgs),
}

/// `bb bucket tags set`
#[derive(Debug, Args)]
pub struct BucketTagsSetArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// The tags to set, each `KEY=VALUE`. Existing tags with other keys stay.
    #[arg(value_name = "KEY=VALUE", required = true, value_parser = parse_bucket_tag)]
    pub tags: Vec<(String, String)>,
    /// Make these tags the whole tag set, removing every other tag.
    #[arg(long)]
    pub replace: bool,
}

/// `bb bucket tags delete`
#[derive(Debug, Args)]
pub struct BucketTagsDeleteArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Keys of the tags to remove; without any, every tag is removed.
    #[arg(value_name = "KEY")]
    pub keys: Vec<String>,
}

/// `KEY=VALUE`, checked against the S3 tag rules.
fn parse_bucket_tag(text: &str) -> Result<(String, String), String> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| crate::i18n::trf("'{text}' is not KEY=VALUE", &[("text", text)]))?;
    validate_tag(key, value).map_err(|err| err.to_string())?;
    Ok((key.to_owned(), value.to_owned()))
}

/// `bb bucket acl`
#[derive(Debug, Subcommand)]
pub enum BucketAclCommand {
    /// Print the ACL of a bucket and whether ACLs are disabled on it.
    Get(BucketArg),
    /// Apply a canned ACL: private, public-read, public-read-write or authenticated-read.
    Set(BucketAclSetArgs),
    /// Add one grant to the ACL of a bucket.
    Grant(BucketAclChangeArgs),
    /// Remove one grant from the ACL of a bucket.
    Revoke(BucketAclChangeArgs),
}

/// `bb bucket acl set`
#[derive(Debug, Args)]
pub struct BucketAclSetArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// private, public-read, public-read-write or authenticated-read.
    #[arg(value_name = "CANNED", value_parser = parse_canned)]
    pub acl: CannedAcl,
}

/// `bb bucket acl grant` and `revoke`
#[derive(Debug, Args)]
pub struct BucketAclChangeArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// READ, WRITE, READ_ACP, WRITE_ACP or FULL_CONTROL.
    #[arg(value_name = "PERMISSION", value_parser = parse_permission)]
    pub permission: AclPermission,
    /// id=CANONICAL_USER_ID, email=ADDRESS, or group=all-users|authenticated-users|log-delivery.
    #[arg(value_name = "GRANTEE", value_parser = parse_grantee)]
    pub grantee: Grantee,
}

/// `bb acl grant` and `revoke`
#[derive(Debug, Args)]
pub struct AclChangeArgs {
    /// `s3://bucket/key` of one object.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// READ, READ_ACP, WRITE_ACP or FULL_CONTROL.
    #[arg(value_name = "PERMISSION", value_parser = parse_permission)]
    pub permission: AclPermission,
    /// id=CANONICAL_USER_ID, email=ADDRESS, or group=all-users|authenticated-users|log-delivery.
    #[arg(value_name = "GRANTEE", value_parser = parse_grantee)]
    pub grantee: Grantee,
}

/// Who a grant is for: the kind and the ID, address or group URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grantee {
    /// What `who` is.
    pub kind: GranteeKind,
    /// The canonical user ID, e-mail address or group URI.
    pub who: String,
}

fn parse_canned(text: &str) -> Result<CannedAcl, String> {
    CannedAcl::from_str(text).map_err(|err: Error| err.to_string())
}

/// `READ`, `read`, `read-acp`, `WRITE_ACP` and so on.
pub(crate) fn parse_permission(text: &str) -> Result<AclPermission, String> {
    match text.trim().to_ascii_uppercase().replace('-', "_").as_str() {
        "READ" => Ok(AclPermission::Read),
        "WRITE" => Ok(AclPermission::Write),
        "READ_ACP" => Ok(AclPermission::ReadAcp),
        "WRITE_ACP" => Ok(AclPermission::WriteAcp),
        "FULL_CONTROL" => Ok(AclPermission::FullControl),
        _ => Err(crate::i18n::trf(
            "'{text}' is not a permission; use READ, WRITE, READ_ACP, WRITE_ACP or FULL_CONTROL",
            &[("text", text)],
        )),
    }
}

/// `id=...`, `email=...` or `group=all-users|authenticated-users|log-delivery`.
pub(crate) fn parse_grantee(text: &str) -> Result<Grantee, String> {
    let wrong = || {
        crate::i18n::trf(
            "'{text}' is not a grantee; use id=ID, email=ADDRESS or group=all-users|authenticated-users|log-delivery",
            &[("text", text)],
        )
    };
    let (kind, value) = text.split_once('=').ok_or_else(wrong)?;
    if value.is_empty() {
        return Err(wrong());
    }
    match kind.to_ascii_lowercase().as_str() {
        "id" => Ok(Grantee {
            kind: GranteeKind::CanonicalUser,
            who: value.to_owned(),
        }),
        "email" => Ok(Grantee {
            kind: GranteeKind::Email,
            who: value.to_owned(),
        }),
        "group" => {
            let uri = match value.to_ascii_lowercase().as_str() {
                "all-users" | "allusers" => ALL_USERS_URI,
                "authenticated-users" | "authenticatedusers" => AUTHENTICATED_USERS_URI,
                "log-delivery" | "logdelivery" => LOG_DELIVERY_URI,
                _ => return Err(wrong()),
            };
            Ok(Grantee {
                kind: GranteeKind::Group,
                who: uri.to_owned(),
            })
        }
        _ => Err(wrong()),
    }
}
