//! Object ACLs: reading them and applying canned ACLs.

use std::fmt;
use std::str::FromStr;

use aws_sdk_s3::operation::get_bucket_acl::GetBucketAclOutput;
use aws_sdk_s3::operation::get_object_acl::GetObjectAclOutput;
use aws_sdk_s3::types::{
    AccessControlPolicy, BucketCannedAcl, Grant, Grantee, ObjectCannedAcl, Owner, Permission, Type,
};
use serde::{Deserialize, Serialize};

use super::{BatchOptions, ObjectChange, run_batch, validate_object_key};
use crate::error::{Error, Result};
use crate::objectlock::ObjectOwnership;
use crate::ops::{S3, build_error};
use crate::s3uri::{validate_bucket_name, validate_key};

/// The group of everyone, signed in or not.
pub const ALL_USERS_URI: &str = "http://acs.amazonaws.com/groups/global/AllUsers";

/// The group of every AWS account.
pub const AUTHENTICATED_USERS_URI: &str =
    "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";

/// The group that S3 server access logging delivers logs as.
pub const LOG_DELIVERY_URI: &str = "http://acs.amazonaws.com/groups/s3/LogDelivery";

/// Longest canonical user ID, e-mail address or display name accepted.
const MAX_GRANTEE_LEN: usize = 256;

/// The canned ACLs of S3 that apply to objects.
///
/// The serde form and [`CannedAcl::as_str`] are the `x-amz-acl` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CannedAcl {
    /// The owner has full control; nobody else has access.
    Private,
    /// Everyone can read the object.
    PublicRead,
    /// Everyone can read and write the object.
    PublicReadWrite,
    /// Every authenticated AWS account can read the object.
    AuthenticatedRead,
    /// The bucket owner can read the object.
    BucketOwnerRead,
    /// The bucket owner has full control of the object.
    BucketOwnerFullControl,
}

impl CannedAcl {
    /// Every canned ACL, in the order the plan lists them.
    pub const ALL: [CannedAcl; 6] = [
        CannedAcl::Private,
        CannedAcl::PublicRead,
        CannedAcl::PublicReadWrite,
        CannedAcl::AuthenticatedRead,
        CannedAcl::BucketOwnerRead,
        CannedAcl::BucketOwnerFullControl,
    ];

    /// The `x-amz-acl` value.
    pub fn as_str(self) -> &'static str {
        match self {
            CannedAcl::Private => "private",
            CannedAcl::PublicRead => "public-read",
            CannedAcl::PublicReadWrite => "public-read-write",
            CannedAcl::AuthenticatedRead => "authenticated-read",
            CannedAcl::BucketOwnerRead => "bucket-owner-read",
            CannedAcl::BucketOwnerFullControl => "bucket-owner-full-control",
        }
    }

    /// Whether the ACL gives access to people outside the account.
    pub fn is_public(self) -> bool {
        matches!(
            self,
            CannedAcl::PublicRead | CannedAcl::PublicReadWrite | CannedAcl::AuthenticatedRead
        )
    }

    fn to_sdk(self) -> ObjectCannedAcl {
        match self {
            CannedAcl::Private => ObjectCannedAcl::Private,
            CannedAcl::PublicRead => ObjectCannedAcl::PublicRead,
            CannedAcl::PublicReadWrite => ObjectCannedAcl::PublicReadWrite,
            CannedAcl::AuthenticatedRead => ObjectCannedAcl::AuthenticatedRead,
            CannedAcl::BucketOwnerRead => ObjectCannedAcl::BucketOwnerRead,
            CannedAcl::BucketOwnerFullControl => ObjectCannedAcl::BucketOwnerFullControl,
        }
    }
}

impl fmt::Display for CannedAcl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CannedAcl {
    type Err = Error;

    /// Accepts the S3 names in any letter case, with `_` for `-`.
    fn from_str(text: &str) -> Result<Self> {
        let wanted = text.trim().to_ascii_lowercase().replace('_', "-");
        CannedAcl::ALL
            .into_iter()
            .find(|acl| acl.as_str() == wanted)
            .ok_or_else(|| {
                let valid: Vec<&str> = CannedAcl::ALL.iter().map(|a| a.as_str()).collect();
                Error::Usage(format!(
                    "'{}' is not a canned ACL; use one of {}",
                    text.escape_debug(),
                    valid.join(", ")
                ))
            })
    }
}

/// What kind of grantee an ACL grant names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GranteeKind {
    /// An AWS account, by canonical user ID.
    CanonicalUser,
    /// A predefined group, by URI.
    Group,
    /// An AWS account, by e-mail address.
    Email,
}

/// A permission an ACL grant gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AclPermission {
    /// Read, write and manage the ACL.
    FullControl,
    /// Read the object.
    Read,
    /// Write the object.
    Write,
    /// Read the ACL.
    ReadAcp,
    /// Write the ACL.
    WriteAcp,
}

/// One grant of an ACL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclGrant {
    /// What `grantee` is.
    pub grantee_type: GranteeKind,
    /// The canonical user ID (or the display name when S3 sends no ID), the
    /// group URI or the e-mail address.
    pub grantee: String,
    /// What the grant allows.
    pub permission: AclPermission,
}

/// The ACL of an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectAcl {
    /// The owner: the canonical user ID, or the display name when S3 sends no ID.
    pub owner: Option<String>,
    /// Every grant.
    pub grants: Vec<AclGrant>,
    /// The canned ACL the grants are exactly those of (`private`,
    /// `public-read`, `public-read-write` or `authenticated-read`), `None`
    /// for any other combination of grants. The two bucket-owner ACLs cannot
    /// be told apart from custom grants to another account.
    pub canned: Option<CannedAcl>,
}

impl ObjectAcl {
    /// The name shown for the ACL: the canned name, or `custom`.
    pub fn summary(&self) -> &'static str {
        self.canned.map_or("custom", CannedAcl::as_str)
    }
}

/// The ACL of a bucket and whether ACLs can be changed on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketAcl {
    /// The grants of the bucket.
    pub acl: ObjectAcl,
    /// The Object Ownership setting; `None` when none is set or it could not
    /// be read.
    pub ownership: Option<ObjectOwnership>,
    /// Object Ownership is BucketOwnerEnforced: S3 refuses every ACL change
    /// on the bucket and on its objects.
    pub acls_disabled: bool,
}

impl AclPermission {
    fn to_sdk(self) -> Permission {
        match self {
            AclPermission::FullControl => Permission::FullControl,
            AclPermission::Read => Permission::Read,
            AclPermission::Write => Permission::Write,
            AclPermission::ReadAcp => Permission::ReadAcp,
            AclPermission::WriteAcp => Permission::WriteAcp,
        }
    }
}

/// Checks a list of grants without asking S3: not empty, each grantee valid
/// for its kind (a group is one of the three predefined groups, an e-mail
/// address has an `@`, a canonical user ID has no spaces) and no grant twice.
pub fn validate_grants(grants: &[AclGrant]) -> Result<()> {
    if grants.is_empty() {
        return Err(Error::Usage(
            "an ACL needs at least one grant; the owner usually keeps FULL_CONTROL".into(),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for grant in grants {
        let who = grant.grantee.trim();
        if who.is_empty() || who != grant.grantee || who.chars().count() > MAX_GRANTEE_LEN {
            return Err(Error::Usage(format!(
                "the grantee '{}' is empty, has spaces around it or is too long",
                grant.grantee.escape_debug()
            )));
        }
        match grant.grantee_type {
            GranteeKind::Group => {
                if ![ALL_USERS_URI, AUTHENTICATED_USERS_URI, LOG_DELIVERY_URI].contains(&who) {
                    return Err(Error::Usage(format!(
                        "'{}' is not a predefined group; use AllUsers, AuthenticatedUsers or LogDelivery",
                        who.escape_debug()
                    )));
                }
            }
            GranteeKind::Email => {
                let valid = who
                    .split_once('@')
                    .is_some_and(|(name, host)| !name.is_empty() && host.contains('.'))
                    && !who.chars().any(|c| c.is_whitespace() || c.is_control());
                if !valid {
                    return Err(Error::Usage(format!(
                        "'{}' is not an e-mail address",
                        who.escape_debug()
                    )));
                }
            }
            GranteeKind::CanonicalUser => {
                if who.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err(Error::Usage(format!(
                        "'{}' is not a canonical user ID (no spaces)",
                        who.escape_debug()
                    )));
                }
            }
        }
        if !seen.insert((grant.grantee_type, who.to_owned(), grant.permission)) {
            return Err(Error::Usage(format!(
                "the grant {:?} to '{}' is listed twice",
                grant.permission,
                who.escape_debug()
            )));
        }
    }
    Ok(())
}

fn sdk_grant(grant: &AclGrant) -> Result<Grant> {
    let grantee = match grant.grantee_type {
        GranteeKind::CanonicalUser => Grantee::builder()
            .r#type(Type::CanonicalUser)
            .id(&grant.grantee),
        GranteeKind::Group => Grantee::builder().r#type(Type::Group).uri(&grant.grantee),
        GranteeKind::Email => Grantee::builder()
            .r#type(Type::AmazonCustomerByEmail)
            .email_address(&grant.grantee),
    }
    .build()
    .map_err(build_error)?;
    Ok(Grant::builder()
        .grantee(grantee)
        .permission(grant.permission.to_sdk())
        .build())
}

/// The policy that replaces the ACL: `grants` for the owner the S3 answer named.
fn policy_of(owner: Option<Owner>, grants: &[AclGrant]) -> Result<AccessControlPolicy> {
    let owner = owner.ok_or_else(|| {
        Error::Internal("S3 did not say who owns the ACL, so it cannot be replaced".into())
    })?;
    let sdk: Vec<Grant> = grants.iter().map(sdk_grant).collect::<Result<_>>()?;
    Ok(AccessControlPolicy::builder()
        .owner(owner)
        .set_grants(Some(sdk))
        .build())
}

fn permission_of(permission: &Permission) -> Option<AclPermission> {
    match permission {
        Permission::FullControl => Some(AclPermission::FullControl),
        Permission::Read => Some(AclPermission::Read),
        Permission::Write => Some(AclPermission::Write),
        Permission::ReadAcp => Some(AclPermission::ReadAcp),
        Permission::WriteAcp => Some(AclPermission::WriteAcp),
        _ => None,
    }
}

fn grant_of(grant: &Grant) -> Option<AclGrant> {
    let grantee = grant.grantee()?;
    let permission = permission_of(grant.permission()?)?;
    let (grantee_type, name) = match grantee.r#type() {
        Type::Group => (GranteeKind::Group, grantee.uri()),
        Type::AmazonCustomerByEmail => (GranteeKind::Email, grantee.email_address()),
        _ => (
            GranteeKind::CanonicalUser,
            grantee.id().or(grantee.display_name()),
        ),
    };
    Some(AclGrant {
        grantee_type,
        grantee: name?.to_owned(),
        permission,
    })
}

/// The canned ACL whose grants are exactly `grants`, for an object owned by
/// `owner_id`.
fn infer_canned(owner_id: Option<&str>, grants: &[AclGrant]) -> Option<CannedAcl> {
    let owner = owner_id?;
    let mut wanted: Vec<(GranteeKind, &str, AclPermission)> = grants
        .iter()
        .map(|g| (g.grantee_type, g.grantee.as_str(), g.permission))
        .collect();
    wanted.sort();
    let owner_full = (
        GranteeKind::CanonicalUser,
        owner,
        AclPermission::FullControl,
    );
    let group = |uri: &'static str, permission| (GranteeKind::Group, uri, permission);
    let candidates = [
        (CannedAcl::Private, vec![owner_full]),
        (
            CannedAcl::PublicRead,
            vec![owner_full, group(ALL_USERS_URI, AclPermission::Read)],
        ),
        (
            CannedAcl::PublicReadWrite,
            vec![
                owner_full,
                group(ALL_USERS_URI, AclPermission::Read),
                group(ALL_USERS_URI, AclPermission::Write),
            ],
        ),
        (
            CannedAcl::AuthenticatedRead,
            vec![
                owner_full,
                group(AUTHENTICATED_USERS_URI, AclPermission::Read),
            ],
        ),
    ];
    candidates.into_iter().find_map(|(canned, mut expected)| {
        expected.sort();
        (expected == wanted).then_some(canned)
    })
}

/// The ACL a `GetObjectAcl` response describes.
pub(crate) fn acl_from_output(out: &GetObjectAclOutput) -> ObjectAcl {
    let owner_id = out.owner().and_then(Owner::id);
    let grants: Vec<AclGrant> = out.grants().iter().filter_map(grant_of).collect();
    ObjectAcl {
        owner: owner_id
            .or(out.owner().and_then(Owner::display_name))
            .map(str::to_owned),
        canned: infer_canned(owner_id, &grants),
        grants,
    }
}

/// The ACL a `GetBucketAcl` response describes.
fn acl_from_bucket_output(out: &GetBucketAclOutput) -> ObjectAcl {
    let owner_id = out.owner().and_then(Owner::id);
    let grants: Vec<AclGrant> = out.grants().iter().filter_map(grant_of).collect();
    ObjectAcl {
        owner: owner_id
            .or(out.owner().and_then(Owner::display_name))
            .map(str::to_owned),
        canned: infer_canned(owner_id, &grants),
        grants,
    }
}

/// An ACL that an in-place copy would drop, kept to be applied again.
pub(crate) struct AclSnapshot {
    owner: Option<Owner>,
    grants: Vec<Grant>,
}

impl AclSnapshot {
    /// The grants beyond the owner's full control, or `None` when the ACL is
    /// the default one that every new object gets.
    fn of(out: &GetObjectAclOutput) -> Option<AclSnapshot> {
        let owner_id = out.owner().and_then(Owner::id);
        let is_default = out.grants().iter().all(|grant| {
            matches!(grant.permission(), Some(Permission::FullControl))
                && grant
                    .grantee()
                    .is_some_and(|g: &Grantee| g.id().is_some() && g.id() == owner_id)
        });
        (!is_default).then(|| AclSnapshot {
            owner: out.owner().cloned(),
            grants: out.grants().to_vec(),
        })
    }
}

/// A failure that makes a request about ACLs clearer: buckets with
/// BucketOwnerEnforced reject every ACL change.
fn acl_error(err: Error, bucket: &str, setting: Option<CannedAcl>) -> Error {
    let Some(service) = err.service() else {
        return err;
    };
    if service.code() == Some("AccessControlListNotSupported") {
        return Error::Conflict(format!(
            "ACLs are disabled for bucket '{bucket}' (Object Ownership is BucketOwnerEnforced); change the bucket's Object Ownership setting to use ACLs"
        ));
    }
    if setting.is_some_and(CannedAcl::is_public) && service.status() == Some(403) {
        return err
            .with_context("the bucket's Block Public Access settings may be blocking public ACLs");
    }
    err
}

impl S3 {
    /// The ACL of an object (`GetObjectAcl`).
    ///
    /// Buckets with ACLs disabled (Object Ownership BucketOwnerEnforced)
    /// still answer with the default ACL, which reads as `private`.
    pub async fn get_object_acl(&self, bucket: &str, key: &str) -> Result<ObjectAcl> {
        Ok(acl_from_output(&self.get_acl_output(bucket, key).await?))
    }

    pub(crate) async fn get_acl_output(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<GetObjectAclOutput> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        self.client_for(bucket)
            .await?
            .get_object_acl()
            .bucket(bucket)
            .key(key)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("reading the ACL of s3://{bucket}/{key}"))
            })
    }

    /// Applies a canned ACL to an object (`PutObjectAcl`).
    ///
    /// A bucket with ACLs disabled (BucketOwnerEnforced) is a `conflict` that
    /// says so.
    pub async fn put_object_acl(&self, bucket: &str, key: &str, acl: CannedAcl) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        self.client_for(bucket)
            .await?
            .put_object_acl()
            .bucket(bucket)
            .key(key)
            .acl(acl.to_sdk())
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                acl_error(
                    Error::from(err)
                        .with_context(format!("setting the ACL of s3://{bucket}/{key}")),
                    bucket,
                    Some(acl),
                )
            })?;
        Ok(())
    }

    /// Replaces every grant of an object's ACL (`PutObjectAcl`), keeping the
    /// owner S3 reports. Checked first: [`validate_grants`]. A bucket with ACLs
    /// disabled is a `conflict` that says so.
    pub async fn put_object_acl_grants(
        &self,
        bucket: &str,
        key: &str,
        grants: &[AclGrant],
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_object_key(key)?;
        validate_grants(grants)?;
        let current = self.get_acl_output(bucket, key).await?;
        let policy = policy_of(current.owner().cloned(), grants)?;
        self.client_for(bucket)
            .await?
            .put_object_acl()
            .bucket(bucket)
            .key(key)
            .access_control_policy(policy)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                acl_error(
                    Error::from(err)
                        .with_context(format!("setting the ACL of s3://{bucket}/{key}")),
                    bucket,
                    None,
                )
            })?;
        Ok(())
    }

    /// The ACL of a bucket (`GetBucketAcl`) with the Object Ownership that
    /// decides whether ACLs work at all.
    pub async fn get_bucket_acl(&self, bucket: &str) -> Result<BucketAcl> {
        let acl = acl_from_bucket_output(&self.get_bucket_acl_output(bucket).await?);
        // An unreadable ownership setting does not hide the ACL.
        let ownership = self.get_object_ownership(bucket).await.ok().flatten();
        Ok(BucketAcl {
            acl,
            ownership,
            acls_disabled: ownership == Some(ObjectOwnership::BucketOwnerEnforced),
        })
    }

    async fn get_bucket_acl_output(&self, bucket: &str) -> Result<GetBucketAclOutput> {
        validate_bucket_name(bucket)?;
        self.client_for(bucket)
            .await?
            .get_bucket_acl()
            .bucket(bucket)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                Error::from(err).with_context(format!("reading the ACL of bucket '{bucket}'"))
            })
    }

    /// Applies a canned ACL to a bucket (`PutBucketAcl`). Only `private`,
    /// `public-read`, `public-read-write` and `authenticated-read` apply to
    /// buckets (`usage` for the others).
    pub async fn put_bucket_acl(&self, bucket: &str, acl: CannedAcl) -> Result<()> {
        validate_bucket_name(bucket)?;
        if matches!(
            acl,
            CannedAcl::BucketOwnerRead | CannedAcl::BucketOwnerFullControl
        ) {
            return Err(Error::Usage(format!(
                "the canned ACL '{acl}' applies to objects, not to buckets; use private, public-read, public-read-write or authenticated-read"
            )));
        }
        self.client_for(bucket)
            .await?
            .put_bucket_acl()
            .bucket(bucket)
            .acl(BucketCannedAcl::from(acl.as_str()))
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                acl_error(
                    Error::from(err).with_context(format!("setting the ACL of bucket '{bucket}'")),
                    bucket,
                    Some(acl),
                )
            })?;
        Ok(())
    }

    /// Replaces every grant of a bucket's ACL (`PutBucketAcl`), keeping the
    /// owner S3 reports. Checked first: [`validate_grants`].
    pub async fn put_bucket_acl_grants(&self, bucket: &str, grants: &[AclGrant]) -> Result<()> {
        validate_bucket_name(bucket)?;
        validate_grants(grants)?;
        let current = self.get_bucket_acl_output(bucket).await?;
        let policy = policy_of(current.owner().cloned(), grants)?;
        self.client_for(bucket)
            .await?
            .put_bucket_acl()
            .bucket(bucket)
            .access_control_policy(policy)
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                acl_error(
                    Error::from(err).with_context(format!("setting the ACL of bucket '{bucket}'")),
                    bucket,
                    None,
                )
            })?;
        Ok(())
    }

    /// Reads the ACL of an object about to be copied onto itself, which
    /// drops it. `Ok(None)`: nothing to restore.
    pub(crate) async fn acl_to_preserve(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Option<AclSnapshot>> {
        match self.get_acl_output(bucket, key).await {
            Ok(out) => Ok(AclSnapshot::of(&out)),
            // Servers without ACLs have nothing to lose.
            Err(err) if err.service().is_some_and(|s| s.is_not_implemented()) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Applies an ACL read by [`S3::acl_to_preserve`] to the new object.
    pub(crate) async fn restore_acl(
        &self,
        bucket: &str,
        key: &str,
        snapshot: AclSnapshot,
    ) -> Result<()> {
        let policy = AccessControlPolicy::builder()
            .set_owner(snapshot.owner)
            .set_grants(Some(snapshot.grants))
            .build();
        self.client_for(bucket)
            .await?
            .put_object_acl()
            .bucket(bucket)
            .key(key)
            .access_control_policy(policy)
            .set_request_payer(self.request_payer())
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                acl_error(
                    Error::from(err)
                        .with_context(format!("restoring the ACL of s3://{bucket}/{key}")),
                    bucket,
                    None,
                )
            })?;
        Ok(())
    }

    /// Applies a canned ACL to each object in `keys`.
    ///
    /// An object that already has exactly that ACL is reported unchanged. A
    /// bucket with ACLs disabled is a `conflict` for every object whose ACL
    /// would change.
    pub async fn set_object_acl(
        &self,
        bucket: &str,
        keys: &[String],
        acl: CannedAcl,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        run_batch(keys, options, |key| async move {
            validate_object_key(&key)?;
            // An unreadable ACL does not stop the change; the write reports
            // its own error if it cannot be made either.
            let previous = self.get_object_acl(bucket, &key).await.ok();
            let name = previous
                .as_ref()
                .map(|current| current.summary().to_owned());
            if previous.is_some_and(|current| current.canned == Some(acl)) {
                return Ok(ObjectChange::unchanged(&key, name));
            }
            if !options.dry_run {
                self.put_object_acl(bucket, &key, acl).await?;
            }
            Ok(ObjectChange::changed(&key, name))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use serde_json::json;

    const OWNER: &str = "owner-canonical-id";

    fn user(id: &str, permission: Permission) -> Grant {
        Grant::builder()
            .grantee(
                Grantee::builder()
                    .r#type(Type::CanonicalUser)
                    .id(id)
                    .build()
                    .unwrap(),
            )
            .permission(permission)
            .build()
    }

    fn group(uri: &str, permission: Permission) -> Grant {
        Grant::builder()
            .grantee(
                Grantee::builder()
                    .r#type(Type::Group)
                    .uri(uri)
                    .build()
                    .unwrap(),
            )
            .permission(permission)
            .build()
    }

    fn output(grants: Vec<Grant>) -> GetObjectAclOutput {
        GetObjectAclOutput::builder()
            .owner(Owner::builder().id(OWNER).display_name("me").build())
            .set_grants(Some(grants))
            .build()
    }

    #[test]
    fn every_canned_acl_round_trips() {
        let names = [
            "private",
            "public-read",
            "public-read-write",
            "authenticated-read",
            "bucket-owner-read",
            "bucket-owner-full-control",
        ];
        assert_eq!(CannedAcl::ALL.map(CannedAcl::as_str), names);
        for (acl, name) in CannedAcl::ALL.into_iter().zip(names) {
            assert_eq!(name.parse::<CannedAcl>().unwrap(), acl);
            assert_eq!(serde_json::to_value(acl).unwrap(), json!(name));
            assert_eq!(acl.to_string(), name);
            assert_eq!(acl.to_sdk().as_str(), name);
        }
        assert_eq!(
            "Public_Read".parse::<CannedAcl>().unwrap(),
            CannedAcl::PublicRead
        );
        for bad in ["", "aws-exec-read", "public", "log-delivery-write"] {
            let err = bad.parse::<CannedAcl>().unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{bad}");
            assert!(
                err.to_string().contains("bucket-owner-full-control"),
                "{err}"
            );
        }
        assert!(CannedAcl::PublicRead.is_public());
        assert!(CannedAcl::AuthenticatedRead.is_public());
        assert!(!CannedAcl::Private.is_public());
        assert!(!CannedAcl::BucketOwnerFullControl.is_public());
    }

    #[test]
    fn grants_that_match_a_canned_acl_are_recognised() {
        let private = acl_from_output(&output(vec![user(OWNER, Permission::FullControl)]));
        assert_eq!(private.canned, Some(CannedAcl::Private));
        assert_eq!(private.owner.as_deref(), Some(OWNER));
        assert_eq!(private.summary(), "private");

        let public_read = acl_from_output(&output(vec![
            group(ALL_USERS_URI, Permission::Read),
            user(OWNER, Permission::FullControl),
        ]));
        assert_eq!(public_read.canned, Some(CannedAcl::PublicRead));
        assert_eq!(public_read.grants.len(), 2);
        assert_eq!(public_read.grants[0].grantee_type, GranteeKind::Group);
        assert_eq!(public_read.grants[0].grantee, ALL_USERS_URI);
        assert_eq!(public_read.grants[0].permission, AclPermission::Read);

        let read_write = acl_from_output(&output(vec![
            user(OWNER, Permission::FullControl),
            group(ALL_USERS_URI, Permission::Write),
            group(ALL_USERS_URI, Permission::Read),
        ]));
        assert_eq!(read_write.canned, Some(CannedAcl::PublicReadWrite));

        let authenticated = acl_from_output(&output(vec![
            user(OWNER, Permission::FullControl),
            group(AUTHENTICATED_USERS_URI, Permission::Read),
        ]));
        assert_eq!(authenticated.canned, Some(CannedAcl::AuthenticatedRead));
    }

    #[test]
    fn other_combinations_are_custom() {
        let cases = [
            // Another account may read.
            vec![
                user(OWNER, Permission::FullControl),
                user("someone-else", Permission::Read),
            ],
            // A canned ACL plus one more grant.
            vec![
                user(OWNER, Permission::FullControl),
                group(ALL_USERS_URI, Permission::Read),
                group(ALL_USERS_URI, Permission::ReadAcp),
            ],
            // The owner without full control.
            vec![user(OWNER, Permission::Read)],
            // Full control of somebody else.
            vec![user("someone-else", Permission::FullControl)],
            vec![],
        ];
        for grants in cases {
            let acl = acl_from_output(&output(grants.clone()));
            assert_eq!(acl.canned, None, "{grants:?}");
            assert_eq!(acl.summary(), "custom");
        }
    }

    #[test]
    fn only_a_non_default_acl_is_kept_for_restoring() {
        assert!(AclSnapshot::of(&output(vec![user(OWNER, Permission::FullControl)])).is_none());
        let public = output(vec![
            user(OWNER, Permission::FullControl),
            group(ALL_USERS_URI, Permission::Read),
        ]);
        let snapshot = AclSnapshot::of(&public).unwrap();
        assert_eq!(snapshot.grants.len(), 2);
        assert_eq!(
            snapshot
                .owner
                .and_then(|o| o.id().map(str::to_owned))
                .as_deref(),
            Some(OWNER)
        );
        assert!(
            AclSnapshot::of(&output(vec![
                user(OWNER, Permission::FullControl),
                user("someone-else", Permission::Read)
            ]))
            .is_some()
        );
    }

    #[test]
    fn email_and_name_only_grantees_are_listed() {
        let email = Grant::builder()
            .grantee(
                Grantee::builder()
                    .r#type(Type::AmazonCustomerByEmail)
                    .email_address("a@example.com")
                    .build()
                    .unwrap(),
            )
            .permission(Permission::Read)
            .build();
        let named = Grant::builder()
            .grantee(
                Grantee::builder()
                    .r#type(Type::CanonicalUser)
                    .display_name("alice")
                    .build()
                    .unwrap(),
            )
            .permission(Permission::WriteAcp)
            .build();
        let acl = acl_from_output(&output(vec![email, named]));
        assert_eq!(acl.grants[0].grantee_type, GranteeKind::Email);
        assert_eq!(acl.grants[0].grantee, "a@example.com");
        assert_eq!(acl.grants[1].grantee, "alice");
        assert_eq!(acl.grants[1].permission, AclPermission::WriteAcp);
        assert_eq!(
            serde_json::to_value(&acl.grants[1]).unwrap(),
            json!({ "grantee_type": "canonical_user", "grantee": "alice", "permission": "WRITE_ACP" })
        );
    }
}
