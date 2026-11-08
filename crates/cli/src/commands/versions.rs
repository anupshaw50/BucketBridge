//! Every version and delete marker whose key starts with a prefix, used by
//! `bb rm --versions` and by `bb ls --versions` for an exact key.
//!
//! The core's `list_versions` is a browsing API: it never returns the entry
//! whose key equals the prefix, which is exactly the object that `bb rm
//! --versions s3://bucket/key` must purge (and the folder marker of a
//! recursive purge), or whose versions `bb ls --versions s3://bucket/key`
//! should show. This listing has no such filter.

use bucketbridge_core::model::ObjectVersion;
use bucketbridge_core::{Error, Result, S3};
use futures::{Stream, TryStreamExt};

use super::{size_of, unquote, utc};
use crate::paging::SeenTokens;

/// Position of the listing: the markers of the next page and every marker
/// followed so far.
struct Cursor {
    markers: Option<(String, Option<String>)>,
    seen: SeenTokens<(String, Option<String>)>,
}

/// Streams every version and delete marker whose key starts with `prefix`,
/// one page (up to 1000 entries) in memory at a time, in S3 order: by key,
/// the latest version first, then newest first.
pub fn all_versions(
    s3: S3,
    bucket: String,
    prefix: String,
) -> impl Stream<Item = Result<ObjectVersion>> {
    let start = Cursor {
        markers: None,
        seen: SeenTokens::default(),
    };
    // `None` = finished.
    let pages = futures::stream::try_unfold(Some(start), move |state| {
        let s3 = s3.clone();
        let bucket = bucket.clone();
        let prefix = prefix.clone();
        async move {
            let Some(Cursor { markers, mut seen }) = state else {
                return Ok::<_, Error>(None);
            };
            let out = s3
                .client()
                .list_object_versions()
                .bucket(&bucket)
                .prefix(&prefix)
                .set_key_marker(markers.as_ref().map(|m| m.0.clone()))
                .set_version_id_marker(markers.as_ref().and_then(|m| m.1.clone()))
                .set_request_payer(s3.request_payer())
                .send()
                .await
                .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{prefix}")))?;
            let mut page: Vec<ObjectVersion> =
                Vec::with_capacity(out.versions().len() + out.delete_markers().len());
            for version in out.versions() {
                if let (Some(key), Some(version_id)) = (version.key(), version.version_id()) {
                    page.push(ObjectVersion {
                        key: key.to_owned(),
                        version_id: version_id.to_owned(),
                        is_latest: version.is_latest().unwrap_or(false),
                        is_delete_marker: false,
                        size: size_of(version.size()),
                        etag: version.e_tag().map(unquote),
                        last_modified: version.last_modified().and_then(utc),
                        storage_class: version.storage_class().map(|c| c.as_str().to_owned()),
                    });
                }
            }
            for marker in out.delete_markers() {
                if let (Some(key), Some(version_id)) = (marker.key(), marker.version_id()) {
                    page.push(ObjectVersion {
                        key: key.to_owned(),
                        version_id: version_id.to_owned(),
                        is_latest: marker.is_latest().unwrap_or(false),
                        is_delete_marker: true,
                        size: 0,
                        etag: None,
                        last_modified: marker.last_modified().and_then(utc),
                        storage_class: None,
                    });
                }
            }
            // The SDK returns versions and delete markers as two lists.
            page.sort_by(|a, b| {
                a.key
                    .cmp(&b.key)
                    .then(b.is_latest.cmp(&a.is_latest))
                    .then(b.last_modified.cmp(&a.last_modified))
            });
            let base = format!("s3://{bucket}/{prefix}");
            let next = if out.is_truncated() == Some(true) {
                let key = out.next_key_marker().filter(|k| !k.is_empty()).ok_or_else(|| {
                    Error::Remote(crate::i18n::trf(
                        "listing versions of {uri}: the server reported more results but sent no marker",
                        &[("uri", &base)],
                    ))
                })?;
                let next = (
                    key.to_owned(),
                    out.next_version_id_marker().map(str::to_owned),
                );
                let what = crate::i18n::trf("listing versions of {uri}", &[("uri", &base)]);
                let next = seen.follow(next, &what)?;
                Some(Cursor {
                    markers: Some(next),
                    seen,
                })
            } else {
                None
            };
            Ok(Some((page, next)))
        }
    });
    pages
        .map_ok(|page| futures::stream::iter(page.into_iter().map(Ok)))
        .try_flatten()
}
