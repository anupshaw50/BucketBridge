//! The in-place copy behind storage class, encryption and header changes.
//!
//! S3 has no call that changes the storage class, the encryption or the
//! headers of a stored object; the only way is to copy the object onto
//! itself, which S3 only accepts when the copy changes at least one of them.
//! A copy creates a new object (a new version on a versioned bucket), so
//! everything the new object should keep has to be sent again:
//!
//! * content headers and user metadata: `MetadataDirective: COPY` (or `REPLACE`
//!   with the complete new set for header edits);
//! * tags: `TaggingDirective: COPY`;
//! * the storage class, which S3 would otherwise reset to `STANDARD`;
//! * the encryption, which S3 would otherwise reset to the bucket default
//!   (an SSE-C object needs its key on the source and on the copy);
//! * the website redirect, which `COPY` does not carry over;
//! * the ACL, which no copy carries over: it is read first and applied again
//!   afterwards when it is not the default one.
//!
//! The source is pinned by version ID (when the object has one) and by
//! `If-Match` ETag, so an object that is overwritten meanwhile is never
//! replaced by an edit of its old state.

use std::collections::HashMap;

use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_sdk_s3::types::{MetadataDirective, StorageClass as SdkStorageClass, TaggingDirective};

use super::headers::HeaderSet;
use super::sse::{CustomerKey, DestEncryption, SSE_C_ALGORITHM};
use super::storage::unrestored_archive;
use super::{ObjectChange, validate_object_key};
use crate::error::{Error, Result};
use crate::model::CopyOutcome;
use crate::ops::{CopyOptions, S3, copy_source, to_size, unquote};
use crate::s3uri::{S3Uri, validate_bucket_name, validate_key};

/// What a change asks of the copy; whatever it leaves out stays as it is.
#[derive(Debug, Clone, Default)]
pub(crate) struct Edit {
    /// The new storage class.
    pub storage_class: Option<SdkStorageClass>,
    /// The new encryption.
    pub encryption: Option<DestEncryption>,
    /// The complete new headers and user metadata.
    pub replace: Option<HeaderSet>,
}

/// What a change decided after looking at the object.
pub(crate) struct Plan {
    /// The old value, for the report.
    pub previous: Option<String>,
    /// What to change; `None` when the object is already as wanted and
    /// nothing is sent.
    pub edit: Option<Edit>,
}

impl Plan {
    /// The object is already as wanted.
    pub(crate) fn unchanged(previous: Option<String>) -> Self {
        Plan {
            previous,
            edit: None,
        }
    }

    /// The object must be copied with `edit`.
    pub(crate) fn change(previous: Option<String>, edit: Edit) -> Self {
        Plan {
            previous,
            edit: Some(edit),
        }
    }
}

/// Everything the new object gets, resolved against the old one. Used by the
/// single-request copy here and by the multipart copy of [`crate::ops`].
#[derive(Debug, Clone, Default)]
pub(crate) struct CopyChanges {
    /// `x-amz-storage-class`; `None` sends nothing (the class `STANDARD`).
    pub storage_class: Option<SdkStorageClass>,
    /// The encryption headers of the new object.
    pub encryption: DestEncryption,
    /// The key of an SSE-C source.
    pub source_key: Option<CustomerKey>,
    /// `Some`: replace the headers and user metadata with this set; `None`:
    /// carry the source's over.
    pub replace: Option<HeaderSet>,
    /// `x-amz-website-redirect-location` of the new object.
    pub website_redirect: Option<String>,
}

impl Edit {
    /// The changes for `head`'s object: this edit, and everything else kept.
    fn resolve(self, head: &HeadObjectOutput, sse_c: Option<&CustomerKey>) -> Result<CopyChanges> {
        let encryption = match self.encryption {
            Some(encryption) => encryption,
            None => preserved_encryption(head, sse_c)?,
        };
        let storage_class = self.storage_class.or_else(|| {
            head.storage_class()
                .filter(|class| !matches!(class, SdkStorageClass::Standard))
                .cloned()
        });
        let source_key = if head.sse_customer_algorithm().is_some() {
            sse_c.cloned()
        } else {
            None
        };
        Ok(CopyChanges {
            storage_class,
            encryption,
            source_key,
            website_redirect: match &self.replace {
                Some(headers) => headers.website_redirect.clone(),
                None => head.website_redirect_location().map(str::to_owned),
            },
            replace: self.replace,
        })
    }
}

/// The encryption an existing object has, to be sent again with its copy.
fn preserved_encryption(
    head: &HeadObjectOutput,
    sse_c: Option<&CustomerKey>,
) -> Result<DestEncryption> {
    if head.sse_customer_algorithm().is_some() {
        let key = sse_c.ok_or_else(|| {
            Error::Usage(
                "the object is encrypted with a customer-provided key (SSE-C); its key is needed"
                    .into(),
            )
        })?;
        return Ok(DestEncryption {
            customer_key: Some(key.clone()),
            ..DestEncryption::default()
        });
    }
    let sse = head.server_side_encryption().cloned();
    let kms = matches!(
        sse,
        Some(
            aws_sdk_s3::types::ServerSideEncryption::AwsKms
                | aws_sdk_s3::types::ServerSideEncryption::AwsKmsDsse
        )
    );
    Ok(DestEncryption {
        kms_key_id: if kms {
            head.ssekms_key_id().map(str::to_owned)
        } else {
            None
        },
        bucket_key: if kms { head.bucket_key_enabled() } else { None },
        sse,
        customer_key: None,
    })
}

/// Explains a request that S3 refused or rejected while it carried (or
/// should have carried) an SSE-C key.
pub(crate) fn sse_c_context(err: Error, key_supplied: bool) -> Error {
    let hint = match (key_supplied, err.service().and_then(|s| s.status())) {
        (true, Some(403)) => {
            "S3 refused the customer-provided key (SSE-C): it is not the key this object was encrypted with, or access is denied"
        }
        (true, Some(400)) => {
            "S3 rejected the customer-provided key (SSE-C): the object may not be encrypted with a customer key"
        }
        (false, Some(400)) => {
            "S3 rejected the request; if the object is encrypted with a customer-provided key (SSE-C), its key is needed"
        }
        _ => return err,
    };
    err.with_context(hint)
}

impl S3 {
    /// `HeadObject`, with the SSE-C headers when the object is encrypted with
    /// a customer key, and the checksum values when `checksums` is set.
    pub(crate) async fn head_with_key(
        &self,
        bucket: &str,
        key: &str,
        sse_c: Option<&CustomerKey>,
        checksums: bool,
    ) -> Result<HeadObjectOutput> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        let mut request = self
            .client_for(bucket)
            .await?
            .head_object()
            .bucket(bucket)
            .key(key)
            .set_request_payer(self.request_payer());
        if checksums {
            request = request.checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled);
        }
        if let Some(sse_c) = sse_c {
            request = request
                .sse_customer_algorithm(SSE_C_ALGORITHM)
                .sse_customer_key(sse_c.to_base64().as_str())
                .sse_customer_key_md5(sse_c.md5_base64());
        }
        request
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|err| {
                sse_c_context(
                    Error::from(err).with_context(format!("s3://{bucket}/{key}")),
                    sse_c.is_some(),
                )
            })
    }

    /// Changes one object with an in-place copy as `plan` decides after
    /// looking at the object; see the [module documentation](self).
    ///
    /// `what` names the change in the message about an archived object.
    pub(crate) async fn edit_object<F>(
        &self,
        bucket: &str,
        key: &str,
        what: &str,
        sse_c: Option<&CustomerKey>,
        dry_run: bool,
        plan: F,
    ) -> Result<ObjectChange>
    where
        F: FnOnce(&HeadObjectOutput) -> Result<Plan>,
    {
        validate_object_key(key)?;
        let head = self.head_with_key(bucket, key, sse_c, false).await?;
        let Plan { previous, edit } = plan(&head)?;
        let Some(edit) = edit else {
            return Ok(ObjectChange::unchanged(key, previous));
        };
        if let Some(archive) = unrestored_archive(&head) {
            return Err(Error::Conflict(format!(
                "s3://{bucket}/{key} is archived ({archive}) and has not been restored, so its {what} cannot be changed; restore it first"
            )));
        }
        let changes = edit.resolve(&head, sse_c)?;
        let mut change = ObjectChange::changed(key, previous);
        if dry_run {
            return Ok(change);
        }
        let snapshot = match self.acl_to_preserve(bucket, key).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                change.warnings.push(format!(
                    "the object's ACL could not be read, so it was not kept: {err}"
                ));
                None
            }
        };
        self.copy_in_place(bucket, key, &head, &changes).await?;
        if let Some(snapshot) = snapshot
            && let Err(err) = self.restore_acl(bucket, key, snapshot).await
        {
            change.warnings.push(format!(
                "the object was changed, but its ACL could not be applied again and is now the default one: {err}"
            ));
        }
        Ok(change)
    }

    /// Copies the object onto itself with `changes`: one `CopyObject` up to
    /// 5 GiB, a multipart copy above.
    async fn copy_in_place(
        &self,
        bucket: &str,
        key: &str,
        head: &HeadObjectOutput,
        changes: &CopyChanges,
    ) -> Result<CopyOutcome> {
        let uri = S3Uri::new(bucket, key)?;
        let size = to_size(head.content_length());
        let options = CopyOptions::default();
        let copied = if size <= options.multipart_threshold {
            self.single_copy(bucket, key, head, changes).await
        } else {
            self.multipart_copy(&uri, &uri, head, &options, changes, &|_| {})
                .await
        };
        copied.map_err(|err| match err.service() {
            Some(service)
                if service.code() == Some("PreconditionFailed")
                    || service.status() == Some(412) =>
            {
                Error::Conflict(format!(
                    "{uri} changed while it was being edited; nothing was changed, try again"
                ))
            }
            _ => sse_c_context(err, changes.source_key.is_some())
                .with_context(format!("changing {uri}")),
        })
    }

    async fn single_copy(
        &self,
        bucket: &str,
        key: &str,
        head: &HeadObjectOutput,
        changes: &CopyChanges,
    ) -> Result<CopyOutcome> {
        let version = head.version_id().filter(|version| *version != "null");
        let encryption = &changes.encryption;
        let mut request = self
            .client_for(bucket)
            .await?
            .copy_object()
            .bucket(bucket)
            .key(key)
            .copy_source(copy_source(bucket, key, version))
            .set_copy_source_if_match(head.e_tag().map(str::to_owned))
            .tagging_directive(TaggingDirective::Copy)
            .set_storage_class(changes.storage_class.clone())
            .set_website_redirect_location(changes.website_redirect.clone())
            .set_server_side_encryption(encryption.sse.clone())
            .set_ssekms_key_id(encryption.kms_key_id.clone())
            .set_bucket_key_enabled(encryption.bucket_key)
            .set_request_payer(self.request_payer());
        if let Some(key) = &encryption.customer_key {
            request = request
                .sse_customer_algorithm(SSE_C_ALGORITHM)
                .sse_customer_key(key.to_base64().as_str())
                .sse_customer_key_md5(key.md5_base64());
        }
        if let Some(key) = &changes.source_key {
            request = request
                .copy_source_sse_customer_algorithm(SSE_C_ALGORITHM)
                .copy_source_sse_customer_key(key.to_base64().as_str())
                .copy_source_sse_customer_key_md5(key.md5_base64());
        }
        request = match &changes.replace {
            Some(headers) => request
                .metadata_directive(MetadataDirective::Replace)
                .set_content_type(headers.content_type.clone())
                .set_cache_control(headers.cache_control.clone())
                .set_content_disposition(headers.content_disposition.clone())
                .set_content_encoding(headers.content_encoding.clone())
                .set_content_language(headers.content_language.clone())
                .set_expires(headers.expires)
                .set_metadata(Some(
                    headers
                        .user_meta
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<HashMap<_, _>>(),
                )),
            None => request.metadata_directive(MetadataDirective::Copy),
        };
        // No read timeout: a server may answer only when the copy is done.
        let out = request.send().await.map_err(Error::from)?;
        Ok(CopyOutcome {
            size: to_size(head.content_length()),
            etag: out
                .copy_object_result()
                .and_then(|result| result.e_tag())
                .map(unquote),
            parts: 0,
            renamed_to: None,
        })
    }
}
