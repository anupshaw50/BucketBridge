//! Reading everything the properties of an object show.

use serde::{Deserialize, Serialize};

use super::acl::{ObjectAcl, acl_from_output};
use super::sse::CustomerKey;
use super::storage::{RestoreStatus, parse_restore};
use crate::error::{BbError, Result};
use crate::model::ObjectHead;
use crate::ops::{S3, object_head};

/// Everything known about one object: `HeadObject`, `GetObjectTagging` and
/// `GetObjectAcl` combined.
///
/// The `HeadObject` part is required. Tags and ACL are separate requests that
/// may be refused (a missing permission, a server without the API): each
/// failure is kept in its own `*_error` field and the other parts are still
/// there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectDetails {
    /// Key, size, ETag, modification time, version, checksums and
    /// [`ObjectMeta`](crate::model::ObjectMeta): content headers, user
    /// metadata, encryption and Object Lock state from `HeadObject`, plus
    /// `tags` and `acl` (the canned name or `custom`) when those requests
    /// succeeded. `storage_class` is always set (`STANDARD` when S3 omits it).
    #[serde(flatten)]
    pub head: ObjectHead,
    /// The restore state of an archived object, when a restore was requested.
    pub restore: Option<RestoreStatus>,
    /// `ARCHIVE_ACCESS` or `DEEP_ARCHIVE_ACCESS` for an Intelligent-Tiering
    /// object in an archive tier.
    pub archive_status: Option<String>,
    /// The ACL, when it could be read.
    pub acl: Option<ObjectAcl>,
    /// Why the tags could not be read (`meta.tags` is then empty).
    pub tags_error: Option<BbError>,
    /// Why the ACL could not be read (`acl` is then `None`).
    pub acl_error: Option<BbError>,
}

impl S3 {
    /// Reads the details of an object.
    ///
    /// For an object encrypted with SSE-C, `HeadObject` needs its key: pass it
    /// as `sse_c`. Without it S3 rejects the request (HTTP 400) and a wrong
    /// key is refused (HTTP 403); either is an error that says so. The key is
    /// sent with the request and kept nowhere.
    pub async fn object_details(
        &self,
        bucket: &str,
        key: &str,
        sse_c: Option<&CustomerKey>,
    ) -> Result<ObjectDetails> {
        let head = self.head_with_key(bucket, key, sse_c, true).await?;
        let (tags, acl) = futures::future::join(
            self.get_object_tags(bucket, key),
            self.get_acl_output(bucket, key),
        )
        .await;
        let mut details = object_head(key, &head);
        details
            .storage_class
            .get_or_insert_with(|| "STANDARD".to_owned());
        let (mut tags_error, mut acl_error, mut parsed_acl) = (None, None, None);
        match tags {
            Ok(tags) => details.meta.tags = tags,
            Err(err) => tags_error = Some(err.to_payload()),
        }
        match acl {
            Ok(out) => {
                let acl = acl_from_output(&out);
                details.meta.acl = Some(acl.summary().to_owned());
                parsed_acl = Some(acl);
            }
            Err(err) => acl_error = Some(err.to_payload()),
        }
        Ok(ObjectDetails {
            head: details,
            restore: head.restore().and_then(parse_restore),
            archive_status: head
                .archive_status()
                .map(|status| status.as_str().to_owned()),
            acl: parsed_acl,
            tags_error,
            acl_error,
        })
    }
}
