//! `bb ls`: buckets, objects and folders, or object versions.

use std::future::ready;
use std::pin::pin;

use bucketbridge_core::model::{Bucket, Checksums, ObjectEntry};
use bucketbridge_core::{Error, S3};
use futures::TryStreamExt;

use super::versions::all_versions;
use super::{size_of, timestamp, unquote, utc};
use crate::cli::LsArgs;
use crate::context::Context;
use crate::output::{Failure, Output};
use crate::paging::SeenTokens;
use crate::records::VersionRecord;

/// Runs `bb ls`, following pages with the configured page size.
///
/// A URI that does not end in `/` lists every key that starts with it, the
/// object with exactly that key (and its versions) first. The core listing
/// leaves that object out, because it equals the requested prefix.
pub async fn run(ctx: &Context, args: &LsArgs, out: &mut Output) -> Result<(), Failure> {
    let Some(uri) = &args.uri else {
        for bucket in ctx.s3.list_buckets().await? {
            out.row(&bucket, || bucket_line(&bucket, args.long))?;
        }
        return Ok(());
    };
    let key = uri.key();
    if !key.is_empty() && !key.ends_with('/') {
        if args.versions {
            let mut versions = pin!(
                all_versions(ctx.s3.clone(), uri.bucket().to_owned(), key.to_owned())
                    .try_take_while(|version| ready(Ok(version.key == key)))
            );
            while let Some(version) = versions.try_next().await? {
                let record = VersionRecord::from(&version);
                out.row(&record, || version_line(&record))?;
            }
        } else if let Some(entry) = exact_object(&ctx.s3, uri.bucket(), key).await? {
            out.row(&entry, || object_line(&entry, args.long))?;
        }
    }
    let page_size = ctx.settings.advanced.list_page_size;
    let what = format!("listing {uri}");
    let mut seen = SeenTokens::default();
    let mut token: Option<String> = None;
    loop {
        let next = if args.versions {
            let page = ctx
                .s3
                .list_versions(
                    uri.bucket(),
                    uri.key(),
                    args.recursive,
                    token.as_deref(),
                    page_size,
                )
                .await?;
            for prefix in &page.prefixes {
                let record = VersionRecord::prefix(prefix);
                out.row(&record, || version_line(&record))?;
            }
            for version in &page.entries {
                let record = VersionRecord::from(version);
                out.row(&record, || version_line(&record))?;
            }
            page.next_token
        } else {
            let page = ctx
                .s3
                .list_objects(
                    uri.bucket(),
                    uri.key(),
                    args.recursive,
                    token.as_deref(),
                    page_size,
                )
                .await?;
            for entry in &page.entries {
                out.row(entry, || object_line(entry, args.long))?;
            }
            page.next_token
        };
        match next {
            Some(next) => token = Some(seen.follow(next, &what)?),
            None => return Ok(()),
        }
    }
}

/// The object whose key is exactly `key`, if there is one. Asked with a
/// one-key listing, so it needs no permission beyond listing the bucket:
/// the shortest key with a prefix sorts first.
async fn exact_object(s3: &S3, bucket: &str, key: &str) -> Result<Option<ObjectEntry>, Error> {
    let out = s3
        .client()
        .list_objects_v2()
        .bucket(bucket)
        .prefix(key)
        .max_keys(1)
        .set_request_payer(s3.request_payer())
        .send()
        .await
        .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
    Ok(out
        .contents()
        .first()
        .filter(|object| object.key() == Some(key))
        .map(|object| ObjectEntry {
            key: key.to_owned(),
            size: size_of(object.size()),
            etag: object.e_tag().map(unquote),
            last_modified: object.last_modified().and_then(utc),
            storage_class: object
                .storage_class()
                .map(|class| class.as_str().to_owned()),
            checksums: Checksums::default(),
            is_prefix: false,
        }))
}

/// `name`, or `creation-date  region  name`.
fn bucket_line(bucket: &Bucket, long: bool) -> String {
    if long {
        format!(
            "{:<20}  {:<16}  {}",
            timestamp(bucket.creation_date),
            bucket.region.as_deref().unwrap_or("-"),
            bucket.name
        )
    } else {
        bucket.name.clone()
    }
}

/// `key`, or `modified  size  storage-class  key` (`PRE` for folders).
fn object_line(entry: &ObjectEntry, long: bool) -> String {
    if !long {
        return entry.key.clone();
    }
    let size = if entry.is_prefix {
        "PRE".to_owned()
    } else {
        entry.size.to_string()
    };
    format!(
        "{:<20}  {:>14}  {:<19}  {}",
        timestamp(entry.last_modified),
        size,
        entry.storage_class.as_deref().unwrap_or("-"),
        entry.key
    )
}

/// `modified  size  latest  version-id  key`; `DEL` marks delete markers and
/// `PRE` folders.
fn version_line(record: &VersionRecord) -> String {
    let size = if record.is_prefix {
        "PRE".to_owned()
    } else if record.is_delete_marker {
        "DEL".to_owned()
    } else {
        record.size.to_string()
    };
    format!(
        "{:<20}  {:>14}  {:<6}  {:<36}  {}",
        timestamp(record.last_modified),
        size,
        if record.is_latest { "latest" } else { "-" },
        record.version_id.as_deref().unwrap_or("-"),
        record.key
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::model::ObjectVersion;
    use chrono::{TimeZone, Utc};

    fn pad(n: usize) -> String {
        " ".repeat(n)
    }

    #[test]
    fn object_and_bucket_lines() {
        let entry = ObjectEntry {
            key: "dir/a.txt".into(),
            size: 1234,
            last_modified: Some(Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap()),
            storage_class: Some("STANDARD".into()),
            ..ObjectEntry::default()
        };
        assert_eq!(object_line(&entry, false), "dir/a.txt");
        // Columns: time (20 wide), size (14, right-aligned), class (19), key; two spaces apart.
        assert_eq!(
            object_line(&entry, true),
            format!(
                "2026-10-29T08:15:00Z{}1234  STANDARD{}dir/a.txt",
                pad(2 + 10),
                pad(11 + 2)
            )
        );
        assert_eq!(
            object_line(&ObjectEntry::prefix("dir/sub/"), true),
            format!("-{}PRE  -{}dir/sub/", pad(19 + 2 + 11), pad(18 + 2))
        );
        let bucket = Bucket {
            name: "photos".into(),
            region: Some("eu-west-1".into()),
            ..Bucket::default()
        };
        assert_eq!(bucket_line(&bucket, false), "photos");
        assert_eq!(
            bucket_line(&bucket, true),
            format!("-{}eu-west-1{}photos", pad(19 + 2), pad(7 + 2))
        );
    }

    #[test]
    fn version_lines() {
        let version = ObjectVersion {
            key: "a.txt".into(),
            version_id: "v1".into(),
            is_latest: true,
            is_delete_marker: false,
            size: 5,
            ..ObjectVersion::default()
        };
        let line = version_line(&VersionRecord::from(&version));
        assert_eq!(
            line,
            format!("-{}5  latest  v1{}a.txt", pad(19 + 2 + 13), pad(34 + 2))
        );
        let marker = ObjectVersion {
            is_delete_marker: true,
            is_latest: false,
            ..version
        };
        assert!(
            version_line(&VersionRecord::from(&marker))
                .contains(&format!("DEL  -{}v1", pad(5 + 2))),
            "{}",
            version_line(&VersionRecord::from(&marker))
        );
        assert!(
            version_line(&VersionRecord::prefix("dir/")).ends_with(&format!(
                "PRE  -{}-{}dir/",
                pad(5 + 2),
                pad(35 + 2)
            ))
        );
    }
}
