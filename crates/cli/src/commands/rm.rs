//! `bb rm`: one object, everything under a prefix, or every version.

use std::collections::HashMap;
use std::future::ready;
use std::pin::pin;

use bucketbridge_core::model::{DeleteFailure, ObjectVersion};
use bucketbridge_core::ops::MAX_DELETE_BATCH;
use bucketbridge_core::{Error, S3Uri};
use futures::TryStreamExt;

use super::versions::all_versions;
use crate::cli::RmArgs;
use crate::context::Context;
use crate::output::{Batch, Failure, Output, delete_failure_error};
use crate::records::DeleteRecord;

/// Version deletions in flight at once.
const VERSION_DELETE_CONCURRENCY: usize = 8;

/// Runs `bb rm`.
pub async fn run(ctx: &Context, args: &RmArgs, out: &mut Output) -> Result<(), Failure> {
    let uri = &args.uri;
    if let Some(version_id) = &args.version_id {
        if uri.is_prefix() {
            return Err(Error::Usage(format!(
                "--version-id needs an object key; {uri} is a prefix"
            ))
            .into());
        }
        return delete_version(ctx, uri, version_id, args.dry_run, out).await;
    }
    if uri.is_prefix() && !args.recursive {
        return Err(Error::Usage(format!(
            "{uri} is a prefix; add --recursive to delete everything under it"
        ))
        .into());
    }
    if !uri.is_prefix() && args.recursive {
        return Err(Error::Usage(format!(
            "--recursive needs a prefix ending in '/', for example {uri}/"
        ))
        .into());
    }
    if args.versions {
        delete_versions(ctx, uri, args.recursive, args.dry_run, out).await
    } else if args.recursive {
        delete_prefix(ctx, uri, args.dry_run, out).await
    } else {
        delete_one(ctx, uri, args.dry_run, out).await
    }
}

fn line(bucket: &str, key: &str, version_id: Option<&str>, dry_run: bool) -> String {
    let verb = if dry_run { "would delete" } else { "deleted" };
    match version_id {
        Some(version_id) => format!("{verb} s3://{bucket}/{key} (version {version_id})"),
        None => format!("{verb} s3://{bucket}/{key}"),
    }
}

/// Deletes one object; a missing object is `not_found`.
async fn delete_one(
    ctx: &Context,
    uri: &S3Uri,
    dry_run: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let (bucket, key) = (uri.bucket(), uri.key());
    ctx.s3.head_object(bucket, key, None).await?;
    if !dry_run {
        ctx.s3.delete_object(bucket, key, None).await?;
    }
    let record = DeleteRecord {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        version_id: None,
        dry_run,
        deleted: !dry_run,
        error: None,
    };
    out.change(&record, || line(bucket, key, None, dry_run))
}

/// Deletes one version or delete marker of one object for good; a missing
/// version is `not_found`.
async fn delete_version(
    ctx: &Context,
    uri: &S3Uri,
    version_id: &str,
    dry_run: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let (bucket, key) = (uri.bucket(), uri.key());
    ctx.s3.head_version(bucket, key, version_id).await?;
    if !dry_run {
        ctx.s3.delete_object(bucket, key, Some(version_id)).await?;
    }
    let record = DeleteRecord {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        version_id: Some(version_id.to_owned()),
        dry_run,
        deleted: !dry_run,
        error: None,
    };
    out.change(&record, || line(bucket, key, Some(version_id), dry_run))
}

/// Deletes everything under a prefix (or a whole bucket), 1000 keys per request.
async fn delete_prefix(
    ctx: &Context,
    uri: &S3Uri,
    dry_run: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let bucket = uri.bucket();
    let mut batch = Batch::default();
    let mut keys: Vec<String> = Vec::with_capacity(MAX_DELETE_BATCH);
    let mut entries = pin!(ctx.s3.list_all_stream(bucket, uri.key()));
    while let Some(entry) = entries.try_next().await? {
        keys.push(entry.key);
        if keys.len() == MAX_DELETE_BATCH {
            delete_keys(ctx, bucket, &keys, dry_run, out, &mut batch).await?;
            keys.clear();
        }
    }
    if !keys.is_empty() {
        delete_keys(ctx, bucket, &keys, dry_run, out, &mut batch).await?;
    }
    batch.finish("objects", "could not be deleted")
}

async fn delete_keys(
    ctx: &Context,
    bucket: &str,
    keys: &[String],
    dry_run: bool,
    out: &mut Output,
    batch: &mut Batch,
) -> Result<(), Failure> {
    let outcome = if dry_run {
        None
    } else {
        Some(ctx.s3.delete_objects(bucket, keys).await?)
    };
    let failures: HashMap<&str, &DeleteFailure> = outcome
        .iter()
        .flat_map(|outcome| outcome.failures.iter())
        .map(|failure| (failure.key.as_str(), failure))
        .collect();
    for key in keys {
        let mut record = DeleteRecord {
            bucket: bucket.to_owned(),
            key: key.clone(),
            version_id: None,
            dry_run,
            deleted: false,
            error: None,
        };
        match failures.get(key.as_str()) {
            Some(failure) => {
                let error = delete_failure_error(failure);
                batch.failed(error.code, error.message.clone());
                let message = format!("failed to delete s3://{bucket}/{key}: {}", error.message);
                record.error = Some(error);
                out.item_failed(&record, || message)?;
            }
            None => {
                batch.succeeded();
                record.deleted = !dry_run;
                out.change(&record, || line(bucket, key, None, dry_run))?;
            }
        }
    }
    Ok(())
}

/// Deletes every version and delete marker of one key, or of every key
/// under a prefix.
async fn delete_versions(
    ctx: &Context,
    uri: &S3Uri,
    recursive: bool,
    dry_run: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let (bucket, key) = (uri.bucket(), uri.key());
    let exact = (!recursive).then(|| key.to_owned());
    let versions = all_versions(ctx.s3.clone(), bucket.to_owned(), key.to_owned())
        .try_filter(move |version| ready(exact.as_deref().is_none_or(|key| version.key == key)));
    let mut deletions = pin!(
        versions
            .map_ok(|version: ObjectVersion| async move {
                let result = if dry_run {
                    Ok(())
                } else {
                    ctx.s3
                        .delete_object(bucket, &version.key, Some(&version.version_id))
                        .await
                };
                Ok::<_, Error>((version, result))
            })
            .try_buffered(VERSION_DELETE_CONCURRENCY)
    );
    let mut batch = Batch::default();
    let mut found = false;
    while let Some((version, result)) = deletions.try_next().await? {
        found = true;
        let mut record = DeleteRecord {
            bucket: bucket.to_owned(),
            key: version.key.clone(),
            version_id: Some(version.version_id.clone()),
            dry_run,
            deleted: false,
            error: None,
        };
        match result {
            Ok(()) => {
                batch.succeeded();
                record.deleted = !dry_run;
                out.change(&record, || {
                    line(bucket, &version.key, Some(&version.version_id), dry_run)
                })?;
            }
            Err(err) => {
                batch.failed(err.kind(), err.to_string());
                let message = format!(
                    "failed to delete s3://{bucket}/{} (version {}): {err}",
                    version.key, version.version_id
                );
                record.error = Some(err.to_payload());
                out.item_failed(&record, || message)?;
            }
        }
    }
    if !found && !recursive {
        return Err(Error::NotFound(format!("{uri} has no versions")).into());
    }
    batch.finish("versions", "could not be deleted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(line("b", "k", None, false), "deleted s3://b/k");
        assert_eq!(
            line("b", "k", Some("v1"), true),
            "would delete s3://b/k (version v1)"
        );
    }
}
