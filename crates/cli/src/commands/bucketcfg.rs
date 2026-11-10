//! `bb bucket`: default encryption and tags of a bucket.

use std::collections::BTreeMap;

use bucketbridge_core::bucketprops::{BucketEncryption, BucketSse};
use bucketbridge_core::objectlock::{self, DefaultRetentionRule};
use bucketbridge_core::objectmeta::{AclGrant, AclPermission, BucketAcl, GranteeKind};
use serde::Serialize;

use crate::cli::{
    AclChangeArgs, BucketAclChangeArgs, BucketAclCommand, BucketAclSetArgs, BucketArg,
    BucketCommand, BucketLockCommand, BucketLockEnableArgs, BucketSseMode, BucketTagsCommand,
    BucketTagsDeleteArgs, BucketTagsSetArgs, EncryptionCommand, EncryptionSetArgs, Grantee,
};
use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};

/// Runs one `bb bucket` command.
pub async fn run(ctx: &Context, command: &BucketCommand, out: &mut Output) -> Result<(), Failure> {
    match command {
        BucketCommand::Encryption(EncryptionCommand::Get(args)) => {
            encryption_get(ctx, args, out).await
        }
        BucketCommand::Encryption(EncryptionCommand::Set(args)) => {
            encryption_set(ctx, args, out).await
        }
        BucketCommand::Encryption(EncryptionCommand::Delete(args)) => {
            encryption_delete(ctx, args, out).await
        }
        BucketCommand::Tags(BucketTagsCommand::Get(args)) => tags_get(ctx, args, out).await,
        BucketCommand::Tags(BucketTagsCommand::Set(args)) => tags_set(ctx, args, out).await,
        BucketCommand::Tags(BucketTagsCommand::Delete(args)) => tags_delete(ctx, args, out).await,
        BucketCommand::Acl(BucketAclCommand::Get(args)) => bucket_acl_get(ctx, args, out).await,
        BucketCommand::Acl(BucketAclCommand::Set(args)) => bucket_acl_set(ctx, args, out).await,
        BucketCommand::Acl(BucketAclCommand::Grant(args)) => {
            bucket_acl_change(ctx, args, true, out).await
        }
        BucketCommand::Acl(BucketAclCommand::Revoke(args)) => {
            bucket_acl_change(ctx, args, false, out).await
        }
        BucketCommand::Lock(BucketLockCommand::Get(args)) => lock_get(ctx, args, out).await,
        BucketCommand::Lock(BucketLockCommand::Enable(args)) => lock_enable(ctx, args, out).await,
    }
}

// ---- default encryption ----

/// `bb bucket encryption get`: `null` for no setting.
#[derive(Debug, Serialize)]
struct EncryptionRecord<'a> {
    bucket: &'a str,
    encryption: Option<&'a BucketEncryption>,
}

/// The name of an algorithm in the human output.
fn algorithm_name(algorithm: BucketSse) -> &'static str {
    match algorithm {
        BucketSse::Aes256 => "SSE-S3",
        BucketSse::Kms => "SSE-KMS",
        BucketSse::KmsDsse => "DSSE-KMS",
    }
}

fn describe(encryption: &BucketEncryption) -> String {
    let mut text = algorithm_name(encryption.algorithm).to_owned();
    if let Some(key) = &encryption.kms_key_id {
        text = trf("{base}, key {key}", &[("base", &text), ("key", key)]);
    }
    if encryption.bucket_key_enabled {
        text = trf("{base}, bucket key", &[("base", &text)]);
    }
    text
}

async fn encryption_get(ctx: &Context, args: &BucketArg, out: &mut Output) -> Result<(), Failure> {
    let encryption = ctx.s3.get_bucket_encryption(&args.bucket).await?;
    let record = EncryptionRecord {
        bucket: &args.bucket,
        encryption: encryption.as_ref(),
    };
    out.row(&record, || {
        let what = encryption.as_ref().map_or_else(|| tr("none"), describe);
        format!("s3://{}\t{what}", args.bucket)
    })
}

async fn encryption_set(
    ctx: &Context,
    args: &EncryptionSetArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let wanted = BucketEncryption {
        algorithm: match args.sse {
            BucketSseMode::S3 => BucketSse::Aes256,
            BucketSseMode::Kms => BucketSse::Kms,
        },
        kms_key_id: args.kms_key.clone(),
        bucket_key_enabled: args.bucket_key,
    };
    ctx.s3.put_bucket_encryption(&args.bucket, &wanted).await?;
    let record = EncryptionRecord {
        bucket: &args.bucket,
        encryption: Some(&wanted),
    };
    out.change(&record, || {
        trf(
            "set the default encryption of {bucket} to {what}",
            &[("bucket", &args.bucket), ("what", &describe(&wanted))],
        )
    })
}

async fn encryption_delete(
    ctx: &Context,
    args: &BucketArg,
    out: &mut Output,
) -> Result<(), Failure> {
    ctx.s3.delete_bucket_encryption(&args.bucket).await?;
    let record = EncryptionRecord {
        bucket: &args.bucket,
        encryption: None,
    };
    out.change(&record, || {
        trf(
            "removed the default encryption of {bucket}",
            &[("bucket", &args.bucket)],
        )
    })
}

// ---- tags ----

/// One tag of a bucket (`bb bucket tags get`).
#[derive(Debug, Serialize)]
struct TagRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    value: &'a str,
}

/// The tag set after `bb bucket tags set` or `delete`.
#[derive(Debug, Serialize)]
struct TagsChangedRecord<'a> {
    bucket: &'a str,
    tags: &'a BTreeMap<String, String>,
}

async fn tags_get(ctx: &Context, args: &BucketArg, out: &mut Output) -> Result<(), Failure> {
    let tags = ctx.s3.get_bucket_tags(&args.bucket).await?;
    for (key, value) in &tags {
        let record = TagRecord {
            bucket: &args.bucket,
            key,
            value,
        };
        out.row(&record, || format!("{key}={value}"))?;
    }
    Ok(())
}

async fn tags_set(
    ctx: &Context,
    args: &BucketTagsSetArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let mut wanted = if args.replace {
        BTreeMap::new()
    } else {
        ctx.s3.get_bucket_tags(&args.bucket).await?
    };
    wanted.extend(args.tags.iter().cloned());
    ctx.s3.put_bucket_tags(&args.bucket, &wanted).await?;
    let record = TagsChangedRecord {
        bucket: &args.bucket,
        tags: &wanted,
    };
    out.change(&record, || {
        trf(
            "set {count} tags on {bucket}",
            &[
                ("count", &wanted.len().to_string()),
                ("bucket", &args.bucket),
            ],
        )
    })
}

async fn tags_delete(
    ctx: &Context,
    args: &BucketTagsDeleteArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let mut wanted = if args.keys.is_empty() {
        BTreeMap::new()
    } else {
        ctx.s3.get_bucket_tags(&args.bucket).await?
    };
    for key in &args.keys {
        wanted.remove(key);
    }
    ctx.s3.put_bucket_tags(&args.bucket, &wanted).await?;
    let record = TagsChangedRecord {
        bucket: &args.bucket,
        tags: &wanted,
    };
    out.change(&record, || {
        if args.keys.is_empty() {
            trf("removed every tag of {bucket}", &[("bucket", &args.bucket)])
        } else {
            trf(
                "removed {count} tags from {bucket}",
                &[
                    ("count", &args.keys.len().to_string()),
                    ("bucket", &args.bucket),
                ],
            )
        }
    })
}

// ---- ACL grants ----

/// `bb bucket acl get`
#[derive(Debug, Serialize)]
struct BucketAclRecord<'a> {
    bucket: &'a str,
    /// The canned ACL the grants equal, or `custom`.
    acl: &'a str,
    owner: Option<&'a str>,
    ownership: Option<&'a str>,
    acls_disabled: bool,
    grants: &'a [AclGrant],
}

fn permission_name(permission: AclPermission) -> &'static str {
    match permission {
        AclPermission::FullControl => "FULL_CONTROL",
        AclPermission::Read => "READ",
        AclPermission::Write => "WRITE",
        AclPermission::ReadAcp => "READ_ACP",
        AclPermission::WriteAcp => "WRITE_ACP",
    }
}

/// A grant as one word for the terminal: `READ:group:AllUsers`.
fn grant_text(grant: &AclGrant) -> String {
    let (kind, who) = match grant.grantee_type {
        GranteeKind::Group => (
            "group",
            grant.grantee.rsplit('/').next().unwrap_or(&grant.grantee),
        ),
        GranteeKind::CanonicalUser => ("id", grant.grantee.as_str()),
        GranteeKind::Email => ("email", grant.grantee.as_str()),
    };
    format!("{}:{kind}:{who}", permission_name(grant.permission))
}

async fn bucket_acl_get(ctx: &Context, args: &BucketArg, out: &mut Output) -> Result<(), Failure> {
    let info: BucketAcl = ctx.s3.get_bucket_acl(&args.bucket).await?;
    let record = BucketAclRecord {
        bucket: &args.bucket,
        acl: info.acl.summary(),
        owner: info.acl.owner.as_deref(),
        ownership: info.ownership.map(|o| o.as_str()),
        acls_disabled: info.acls_disabled,
        grants: &info.acl.grants,
    };
    out.row(&record, || {
        let mut line = format!("s3://{}\t{}", args.bucket, info.acl.summary());
        if info.acl.canned.is_none() {
            let grants: Vec<String> = info.acl.grants.iter().map(grant_text).collect();
            line.push('\t');
            line.push_str(&grants.join(","));
        }
        if info.acls_disabled {
            line.push('\t');
            line.push_str(&tr("ACLs are disabled (BucketOwnerEnforced)"));
        }
        line
    })
}

/// `bb bucket acl set`
#[derive(Debug, Serialize)]
struct BucketAclSetRecord<'a> {
    bucket: &'a str,
    acl: &'a str,
}

async fn bucket_acl_set(
    ctx: &Context,
    args: &BucketAclSetArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    ctx.s3.put_bucket_acl(&args.bucket, args.acl).await?;
    let record = BucketAclSetRecord {
        bucket: &args.bucket,
        acl: args.acl.as_str(),
    };
    out.change(&record, || {
        trf(
            "set the ACL of {target} to {acl}",
            &[
                ("target", &format!("s3://{}", args.bucket)),
                ("acl", args.acl.as_str()),
            ],
        )
    })
}

/// `bb bucket acl grant|revoke` and `bb acl grant|revoke`
#[derive(Debug, Serialize)]
struct AclChangeRecord<'a> {
    bucket: &'a str,
    key: Option<&'a str>,
    action: &'a str,
    permission: AclPermission,
    grantee_type: GranteeKind,
    grantee: &'a str,
    changed: bool,
}

/// The grant list after adding or removing one grant; `None` when it already
/// is as wanted.
pub(crate) fn changed_grants(
    current: &[AclGrant],
    grant: &AclGrant,
    add: bool,
) -> Option<Vec<AclGrant>> {
    let present = current.contains(grant);
    match (add, present) {
        (true, false) => {
            let mut grants = current.to_vec();
            grants.push(grant.clone());
            Some(grants)
        }
        (false, true) => Some(current.iter().filter(|g| *g != grant).cloned().collect()),
        _ => None,
    }
}

fn grant_of(permission: AclPermission, grantee: &Grantee) -> AclGrant {
    AclGrant {
        grantee_type: grantee.kind,
        grantee: grantee.who.clone(),
        permission,
    }
}

fn change_line(target: &str, add: bool, changed: bool, grant: &AclGrant) -> String {
    let what = grant_text(grant);
    let source = match (add, changed) {
        (true, true) => "granted {what} on {target}",
        (true, false) => "{target} already has the grant {what}",
        (false, true) => "revoked {what} on {target}",
        (false, false) => "{target} has no grant {what}",
    };
    trf(source, &[("what", &what), ("target", target)])
}

async fn bucket_acl_change(
    ctx: &Context,
    args: &BucketAclChangeArgs,
    add: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let grant = grant_of(args.permission, &args.grantee);
    let current = ctx.s3.get_bucket_acl(&args.bucket).await?;
    let wanted = changed_grants(&current.acl.grants, &grant, add);
    let changed = wanted.is_some();
    if let Some(grants) = wanted {
        ctx.s3.put_bucket_acl_grants(&args.bucket, &grants).await?;
    }
    let record = AclChangeRecord {
        bucket: &args.bucket,
        key: None,
        action: if add { "grant" } else { "revoke" },
        permission: grant.permission,
        grantee_type: grant.grantee_type,
        grantee: &grant.grantee,
        changed,
    };
    out.change(&record, || {
        change_line(&format!("s3://{}", args.bucket), add, changed, &grant)
    })
}

/// `bb acl grant` and `bb acl revoke`: one object.
pub async fn object_acl_change(
    ctx: &Context,
    args: &AclChangeArgs,
    add: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    if args.uri.is_prefix() {
        return Err(bucketbridge_core::Error::Usage(trf(
            "{uri} is not an object; give s3://bucket/key",
            &[("uri", &args.uri.to_string())],
        ))
        .into());
    }
    let (bucket, key) = (args.uri.bucket(), args.uri.key());
    let grant = grant_of(args.permission, &args.grantee);
    let current = ctx.s3.get_object_acl(bucket, key).await?;
    let wanted = changed_grants(&current.grants, &grant, add);
    let changed = wanted.is_some();
    if let Some(grants) = wanted {
        ctx.s3.put_object_acl_grants(bucket, key, &grants).await?;
    }
    let record = AclChangeRecord {
        bucket,
        key: Some(key),
        action: if add { "grant" } else { "revoke" },
        permission: grant.permission,
        grantee_type: grant.grantee_type,
        grantee: &grant.grantee,
        changed,
    };
    out.change(&record, || {
        change_line(&args.uri.to_string(), add, changed, &grant)
    })
}

// ---- Object Lock ----

/// `bb bucket lock get`
#[derive(Debug, Serialize)]
struct BucketLockRecord<'a> {
    bucket: &'a str,
    enabled: bool,
    default_retention: Option<&'a DefaultRetentionRule>,
}

async fn lock_get(ctx: &Context, args: &BucketArg, out: &mut Output) -> Result<(), Failure> {
    let lock = ctx.s3.get_bucket_object_lock(&args.bucket).await?;
    let record = BucketLockRecord {
        bucket: &args.bucket,
        enabled: lock.enabled,
        default_retention: lock.default_retention.as_ref(),
    };
    out.row(&record, || {
        let state = if lock.enabled { tr("on") } else { tr("off") };
        let mut line = format!("s3://{}\t{state}", args.bucket);
        if let Some(rule) = &lock.default_retention {
            let (amount, unit) = match (rule.days, rule.years) {
                (Some(days), _) => (days, "days"),
                (None, Some(years)) => (years, "years"),
                (None, None) => (0, "days"),
            };
            line.push('\t');
            line.push_str(&trf(
                "default retention {mode} {amount} {unit}",
                &[
                    ("mode", objectlock::mode_name(rule.mode)),
                    ("amount", &amount.to_string()),
                    ("unit", &tr(unit)),
                ],
            ));
        }
        line
    })
}

/// `bb bucket lock enable`
#[derive(Debug, Serialize)]
struct BucketLockEnableRecord<'a> {
    bucket: &'a str,
    enabled: bool,
    changed: bool,
}

async fn lock_enable(
    ctx: &Context,
    args: &BucketLockEnableArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    if !args.yes {
        return Err(bucketbridge_core::Error::Usage(trf(
            "Object Lock cannot be turned off again once it is on for {bucket}; run the command again with --yes to turn it on",
            &[("bucket", &args.bucket)],
        ))
        .into());
    }
    let changed = ctx.s3.enable_bucket_object_lock(&args.bucket).await?;
    let record = BucketLockEnableRecord {
        bucket: &args.bucket,
        enabled: true,
        changed,
    };
    out.change(&record, || {
        if changed {
            trf(
                "turned on Object Lock for {bucket}",
                &[("bucket", &args.bucket)],
            )
        } else {
            trf(
                "Object Lock is already on for {bucket}",
                &[("bucket", &args.bucket)],
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(who: &str, permission: AclPermission) -> AclGrant {
        AclGrant {
            grantee_type: GranteeKind::CanonicalUser,
            grantee: who.to_owned(),
            permission,
        }
    }

    #[test]
    fn a_grant_is_added_once_and_removed_when_present() {
        let owner = grant("owner", AclPermission::FullControl);
        let read = grant("other", AclPermission::Read);
        let current = vec![owner.clone()];
        assert_eq!(
            changed_grants(&current, &read, true),
            Some(vec![owner.clone(), read.clone()])
        );
        assert_eq!(changed_grants(&current, &owner, true), None);
        let both = vec![owner.clone(), read.clone()];
        assert_eq!(changed_grants(&both, &read, false), Some(vec![owner]));
        assert_eq!(changed_grants(&current, &read, false), None);
    }
}
