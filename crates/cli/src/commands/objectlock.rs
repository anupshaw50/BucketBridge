//! `bb lock get|set` and `bb restore`: Object Lock retention, legal hold and
//! Glacier restore of objects.
//!
//! Like the object metadata commands, each works on one object or, with
//! `--recursive`, on every object under a prefix; a failed object never stops
//! the others and a refusal of the server is printed as the server worded it.

use bucketbridge_core::model::LockMode;
use bucketbridge_core::objectlock::{self, ObjectLockState, RetentionChange};
use bucketbridge_core::{BbError, Error};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Serialize;

use super::objectmeta::{KeyChunks, batch_options, emit, object_uri};
use crate::cli::{HoldState, LockGetArgs, LockSetArgs, RestoreArgs};
use crate::context::Context;
use crate::output::{Batch, Failure, Output};

/// Objects read at the same time by `bb lock get`.
const READ_CONCURRENCY: usize = 8;

/// Usage checks that need no account: `--bypass-governance` only matters
/// with a retention change, and a retention in the past is refused here.
pub fn check_set(args: &LockSetArgs) -> Result<(), Error> {
    if args.bypass_governance && args.mode.is_none() && !args.clear_retention {
        return Err(Error::Usage(
            "--bypass-governance applies to a retention change (--mode and --until, or --clear-retention)"
                .into(),
        ));
    }
    if args.until.is_some_and(|until| until <= Utc::now()) {
        return Err(Error::Usage("--until must be in the future".into()));
    }
    Ok(())
}

fn mode_text(mode: Option<LockMode>) -> Option<&'static str> {
    mode.map(objectlock::mode_name)
}

// ---- lock get ----

/// `bb lock get`
#[derive(Serialize)]
struct LockGetRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    mode: Option<&'static str>,
    retain_until: Option<DateTime<Utc>>,
    legal_hold: Option<bool>,
    error: Option<BbError>,
}

/// Prints the retention and legal hold of the objects.
pub async fn lock_get(ctx: &Context, args: &LockGetArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let version = args.version_id.as_deref();
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let reads: Vec<(String, Result<ObjectLockState, Error>)> = futures::stream::iter(chunk)
            .map(|key| async move {
                let state = ctx.s3.get_object_lock(bucket, &key, version).await;
                (key, state)
            })
            .buffered(READ_CONCURRENCY)
            .collect()
            .await;
        for (key, state) in reads {
            let uri = object_uri(bucket, &key);
            match state {
                Ok(state) => {
                    batch.succeeded();
                    let record = LockGetRecord {
                        bucket,
                        key: &key,
                        mode: mode_text(state.mode),
                        retain_until: state.retain_until,
                        legal_hold: Some(state.legal_hold),
                        error: None,
                    };
                    out.row(&record, || format!("{uri}\t{}", state.summary()))?;
                }
                Err(error) => {
                    batch.failed(error.kind(), error.to_string());
                    let record = LockGetRecord {
                        bucket,
                        key: &key,
                        mode: None,
                        retain_until: None,
                        legal_hold: None,
                        error: Some(error.to_payload()),
                    };
                    out.item_failed(&record, || {
                        format!("failed to read the lock of {uri}: {error}")
                    })?;
                }
            }
        }
    }
    batch.finish("objects", "could not be read")
}

// ---- lock set ----

/// `bb lock set`: one record per object and aspect.
#[derive(Serialize)]
struct LockSetRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    /// `retention` or `legal_hold`.
    aspect: &'static str,
    mode: Option<&'static str>,
    retain_until: Option<DateTime<Utc>>,
    legal_hold: Option<bool>,
    previous: Option<&'a str>,
    dry_run: bool,
    changed: bool,
    error: Option<BbError>,
}

/// Sets or clears the retention and the legal hold of the objects.
pub async fn lock_set(ctx: &Context, args: &LockSetArgs, out: &mut Output) -> Result<(), Failure> {
    check_set(args)?;
    let bucket = args.uri.bucket();
    let version = args.version_id.as_deref();
    let options = batch_options(args.dry_run);
    let retention = match (args.mode, args.until) {
        (Some(mode), Some(retain_until)) => Some(RetentionChange::Set { mode, retain_until }),
        _ if args.clear_retention => Some(RetentionChange::Clear),
        _ => None,
    };
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        if let Some(change) = retention {
            let results = ctx
                .s3
                .set_object_retention(
                    bucket,
                    &chunk,
                    change,
                    version,
                    args.bypass_governance,
                    options,
                )
                .await?;
            for result in &results {
                let uri = object_uri(bucket, &result.key);
                let (mode, until) = match change {
                    RetentionChange::Set { mode, retain_until } => (Some(mode), Some(retain_until)),
                    RetentionChange::Clear => (None, None),
                };
                let record = LockSetRecord {
                    bucket,
                    key: &result.key,
                    aspect: "retention",
                    mode: mode_text(mode),
                    retain_until: until,
                    legal_hold: None,
                    previous: result.previous.as_deref(),
                    dry_run: args.dry_run,
                    changed: result.changed,
                    error: result.error.clone(),
                };
                let what = match (mode, until) {
                    (Some(mode), Some(until)) => format!(
                        "the retention of {uri} to {} until {}",
                        objectlock::mode_name(mode),
                        until.format("%Y-%m-%dT%H:%M:%SZ")
                    ),
                    _ => format!("the retention of {uri} to none"),
                };
                let line = if args.dry_run {
                    format!("would set {what}")
                } else {
                    format!("set {what}")
                };
                emit(
                    out,
                    &mut batch,
                    &uri,
                    result,
                    "change the retention of",
                    &record,
                    line,
                )?;
            }
        }
        if let Some(hold) = args.legal_hold {
            let on = hold == HoldState::On;
            let results = ctx
                .s3
                .set_object_legal_hold(bucket, &chunk, on, version, options)
                .await?;
            let word = if on { "on" } else { "off" };
            for result in &results {
                let uri = object_uri(bucket, &result.key);
                let record = LockSetRecord {
                    bucket,
                    key: &result.key,
                    aspect: "legal_hold",
                    mode: None,
                    retain_until: None,
                    legal_hold: Some(on),
                    previous: result.previous.as_deref(),
                    dry_run: args.dry_run,
                    changed: result.changed,
                    error: result.error.clone(),
                };
                let line = match (result.changed, args.dry_run) {
                    (true, true) => format!("would turn the legal hold of {uri} {word}"),
                    (true, false) => format!("turned the legal hold of {uri} {word}"),
                    (false, _) => format!("unchanged {uri} (legal hold already {word})"),
                };
                emit(
                    out,
                    &mut batch,
                    &uri,
                    result,
                    "change the legal hold of",
                    &record,
                    line,
                )?;
            }
        }
    }
    batch.finish("objects", "could not be changed")
}

// ---- restore ----

/// `bb restore`
#[derive(Serialize)]
struct RestoreRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    days: u32,
    tier: &'static str,
    dry_run: bool,
    requested: bool,
    error: Option<BbError>,
}

/// Requests the restore of the archived objects.
pub async fn restore(ctx: &Context, args: &RestoreArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .restore_objects(
                bucket,
                &chunk,
                args.days,
                args.tier,
                args.version_id.as_deref(),
                options,
            )
            .await?;
        for result in &results {
            let uri = object_uri(bucket, &result.key);
            let record = RestoreRecord {
                bucket,
                key: &result.key,
                days: args.days,
                tier: args.tier.as_str(),
                dry_run: args.dry_run,
                requested: result.changed,
                error: result.error.clone(),
            };
            let line = if args.dry_run {
                format!(
                    "would request the restore of {uri} for {} days ({})",
                    args.days, args.tier
                )
            } else {
                format!(
                    "requested the restore of {uri} for {} days ({})",
                    args.days, args.tier
                )
            };
            emit(out, &mut batch, &uri, result, "restore", &record, line)?;
        }
    }
    batch.finish("objects", "could not be restored")
}
