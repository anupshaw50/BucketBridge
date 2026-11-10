//! `bb rm`: one object, everything under a prefix, or every version.

use std::collections::HashMap;
use std::future::ready;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bucketbridge_core::model::{DeleteFailure, ObjectVersion};
use bucketbridge_core::ops::MAX_DELETE_BATCH;
use bucketbridge_core::{Error, S3Uri};
use futures::TryStreamExt;

use super::versions::all_versions;
use crate::cli::RmArgs;
use crate::confirm::{confirm, require_answer};
use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::output::{Batch, Failure, Output, delete_failure_error};
use crate::pattern::{S3Pattern, list_matches, s3_pattern};
use crate::records::DeleteRecord;
use crate::versionfilter::VersionFilter;

/// Version deletions in flight at once.
const VERSION_DELETE_CONCURRENCY: usize = 8;

/// Runs `bb rm`.
pub async fn run(ctx: &Context, args: &RmArgs, out: &mut Output) -> Result<(), Failure> {
    let uri = &args.uri;
    if let Some(pattern) = s3_pattern(uri)? {
        return delete_pattern(ctx, args, &pattern, out).await;
    }
    if let Some(version_id) = &args.version_id {
        if uri.is_prefix() {
            return Err(Error::Usage(trf(
                "--version-id needs an object key; {uri} is a prefix",
                &[("uri", &uri.to_string())],
            ))
            .into());
        }
        return delete_version(ctx, uri, version_id, args.dry_run, out).await;
    }
    if uri.is_prefix() && !args.recursive {
        return Err(Error::Usage(trf(
            "{uri} is a prefix; add --recursive to delete everything under it",
            &[("uri", &uri.to_string())],
        ))
        .into());
    }
    if !uri.is_prefix() && args.recursive {
        return Err(Error::Usage(trf(
            "--recursive needs a prefix ending in '/', for example {uri}/",
            &[("uri", uri.to_string().as_str())],
        ))
        .into());
    }
    if args.versions {
        delete_versions(ctx, args, None, out).await
    } else if args.recursive {
        delete_prefix(ctx, uri, args.dry_run, out).await
    } else {
        delete_one(ctx, uri, args.dry_run, out).await
    }
}

fn line(bucket: &str, key: &str, version_id: Option<&str>, dry_run: bool) -> String {
    match version_id {
        Some(version_id) => trf(
            if dry_run {
                "would delete s3://{bucket}/{key} (version {version})"
            } else {
                "deleted s3://{bucket}/{key} (version {version})"
            },
            &[("bucket", bucket), ("key", key), ("version", version_id)],
        ),
        None => trf(
            if dry_run {
                "would delete s3://{bucket}/{key}"
            } else {
                "deleted s3://{bucket}/{key}"
            },
            &[("bucket", bucket), ("key", key)],
        ),
    }
}

/// `bb rm 's3://bucket/logs/*.gz'`: the objects directly in the folder whose
/// names match. Folders are never matched; a pattern that matches nothing
/// is `not_found`.
async fn delete_pattern(
    ctx: &Context,
    args: &RmArgs,
    pattern: &S3Pattern,
    out: &mut Output,
) -> Result<(), Failure> {
    let uri = &args.uri;
    if args.version_id.is_some() {
        return Err(Error::Usage(trf(
            "--version-id needs an object key; {uri} is a pattern",
            &[("uri", &uri.to_string())],
        ))
        .into());
    }
    if args.recursive {
        return Err(Error::Usage(trf(
            "--recursive does not combine with the wildcard in {uri}; a wildcard matches only the objects directly in its folder",
            &[("uri", &uri.to_string())],
        ))
        .into());
    }
    if args.versions {
        return delete_versions(ctx, args, Some(pattern), out).await;
    }
    let matches = list_matches(&ctx.s3, pattern, false).await?;
    let mut batch = Batch::default();
    for keys in matches.chunks(MAX_DELETE_BATCH) {
        let keys: Vec<String> = keys.iter().map(|entry| entry.key.clone()).collect();
        delete_keys(ctx, uri.bucket(), &keys, args.dry_run, out, &mut batch).await?;
    }
    batch.finish("objects", "could not be deleted")
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
                let message = trf(
                    "failed to delete s3://{bucket}/{key}: {message}",
                    &[
                        ("bucket", bucket),
                        ("key", key),
                        ("message", &error.message),
                    ],
                );
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

/// What a version cleanup finds or does, for the questions and the summary.
#[derive(Debug, Default)]
struct Tally {
    versions: u64,
    markers: u64,
    current: u64,
    bytes: u64,
    examples: Vec<String>,
}

impl Tally {
    /// Examples shown before the question.
    const EXAMPLES: usize = 5;

    fn add(&mut self, version: &ObjectVersion) {
        self.versions += 1;
        self.markers += u64::from(version.is_delete_marker);
        self.current += u64::from(version.is_latest);
        self.bytes += version.size;
        if self.examples.len() < Self::EXAMPLES {
            self.examples
                .push(format!("{} ({})", version.key, version.version_id));
        }
    }

    /// "12 versions (3 delete markers, 2 current), 4.0 KiB".
    fn describe(&self) -> String {
        trf(
            "{versions} versions ({markers} delete markers, {current} current), {bytes} bytes",
            &[
                ("versions", &self.versions.to_string()),
                ("markers", &self.markers.to_string()),
                ("current", &self.current.to_string()),
                ("bytes", &self.bytes.to_string()),
            ],
        )
    }
}

/// Deletes every version and delete marker of one key, or of every key
/// under a prefix, that passes the filters. Without `--yes` the matches are
/// counted first and the terminal is asked; a summary ends the run.
async fn delete_versions(
    ctx: &Context,
    args: &RmArgs,
    pattern: Option<&S3Pattern>,
    out: &mut Output,
) -> Result<(), Failure> {
    // A wildcard lists the folder in front of it and keeps the names that
    // match; the `-r` rules do not apply.
    let listed = match pattern {
        Some(pattern) => pattern.dir_uri()?,
        None => args.uri.clone(),
    };
    let uri = &listed;
    let (recursive, dry_run) = (args.recursive || pattern.is_some(), args.dry_run);
    let (bucket, key) = (uri.bucket(), uri.key());
    let filter = VersionFilter::from_args(&args.filter, uri)?;
    if !dry_run {
        require_answer(ctx, args.yes, &tr("deleting versions"))?;
    }
    let exact = (!recursive).then(|| key.to_owned());
    // Whether any key matched the wildcard, whatever the other filters say.
    let hit = Arc::new(AtomicBool::new(false));
    let matching = || {
        let exact = exact.clone();
        let filter = filter.clone();
        let pattern = pattern.cloned();
        let hit = Arc::clone(&hit);
        all_versions(ctx.s3.clone(), bucket.to_owned(), key.to_owned()).try_filter(move |version| {
            let in_pattern = pattern
                .as_ref()
                .is_none_or(|pattern| pattern.matches_key(&version.key));
            if in_pattern {
                hit.store(true, Ordering::Relaxed);
            }
            ready(
                in_pattern
                    && exact.as_deref().is_none_or(|key| version.key == key)
                    && filter.matches(version),
            )
        })
    };
    if !dry_run && !args.yes {
        let mut tally = Tally::default();
        let mut scan = pin!(matching());
        while let Some(version) = scan.try_next().await? {
            tally.add(&version);
        }
        if tally.versions == 0 {
            let missed = pattern.filter(|_| !hit.load(Ordering::Relaxed));
            return finish_versions(
                args,
                &Tally::default(),
                Batch::default(),
                &filter,
                missed,
                out,
            );
        }
        let mut details = vec![trf(
            "{what} under {uri} would be deleted for good:",
            &[("what", &tally.describe()), ("uri", &uri.to_string())],
        )];
        details.extend(tally.examples.iter().map(|example| format!("  {example}")));
        if tally.versions > Tally::EXAMPLES as u64 {
            details.push(trf(
                "  and {more} more",
                &[(
                    "more",
                    &(tally.versions - Tally::EXAMPLES as u64).to_string(),
                )],
            ));
        }
        confirm(
            ctx,
            out,
            false,
            &details,
            &trf(
                "Delete {count} versions for good?",
                &[("count", &tally.versions.to_string())],
            ),
        )?;
    }
    let mut deletions = pin!(
        matching()
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
    let mut tally = Tally::default();
    while let Some((version, result)) = deletions.try_next().await? {
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
                tally.add(&version);
                batch.succeeded();
                record.deleted = !dry_run;
                out.change(&record, || {
                    line(bucket, &version.key, Some(&version.version_id), dry_run)
                })?;
            }
            Err(err) => {
                batch.failed(err.kind(), err.to_string());
                let message = trf(
                    "failed to delete s3://{bucket}/{key} (version {version}): {error}",
                    &[
                        ("bucket", bucket),
                        ("key", version.key.as_str()),
                        ("version", &version.version_id),
                        ("error", &err.to_string()),
                    ],
                );
                record.error = Some(err.to_payload());
                out.item_failed(&record, || message)?;
            }
        }
    }
    let missed = pattern.filter(|_| !hit.load(Ordering::Relaxed));
    finish_versions(args, &tally, batch, &filter, missed, out)
}

/// The summary line, and the error for versions that could not be deleted.
fn finish_versions(
    args: &RmArgs,
    tally: &Tally,
    batch: Batch,
    filter: &VersionFilter,
    missed: Option<&S3Pattern>,
    out: &mut Output,
) -> Result<(), Failure> {
    if let Some(pattern) = missed {
        return Err(Error::NotFound(trf(
            "no objects match {pattern}",
            &[("pattern", &pattern.display())],
        ))
        .into());
    }
    if tally.versions == 0 && !args.recursive && !filter.is_active() {
        return Err(Error::NotFound(trf(
            "{uri} has no versions",
            &[("uri", &args.uri.to_string())],
        ))
        .into());
    }
    out.notice(&trf(
        if args.dry_run {
            "would delete {what} under {uri}"
        } else {
            "deleted {what} under {uri}"
        },
        &[("what", &tally.describe()), ("uri", &args.uri.to_string())],
    ));
    batch.finish("versions", "could not be deleted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::trf;

    #[test]
    fn lines() {
        assert_eq!(
            line("b", "k", None, false),
            trf(
                "deleted s3://{bucket}/{key}",
                &[("bucket", "b"), ("key", "k")]
            )
        );
        assert_eq!(
            line("b", "k", Some("v1"), true),
            trf(
                "would delete s3://{bucket}/{key} (version {version})",
                &[("bucket", "b"), ("key", "k"), ("version", "v1")]
            )
        );
    }
}
