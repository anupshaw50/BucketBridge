//! `bb du`: how much a bucket or prefix holds (item 22 of the roadmap).

use bucketbridge_core::usage::{Usage, UsageOptions};
use serde::Serialize;

use crate::cli::DuArgs;
use crate::context::Context;
use crate::i18n::trf;
use crate::output::{Failure, Output};
use crate::transfer::human_bytes;

/// The totals of `bb du`. Fields of parts that were not asked for are `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageRecord {
    /// Bucket name.
    pub bucket: String,
    /// Prefix counted (empty for the whole bucket).
    pub prefix: String,
    /// Number of current objects.
    pub objects: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// Number of non-current versions (`--versions`).
    pub noncurrent_versions: Option<u64>,
    /// Their total size in bytes (`--versions`).
    pub noncurrent_bytes: Option<u64>,
    /// Number of delete markers (`--versions`).
    pub delete_markers: Option<u64>,
    /// Number of unfinished multipart uploads (`--multipart`).
    pub multipart_uploads: Option<u64>,
    /// Number of parts uploaded so far (`--multipart`).
    pub multipart_parts: Option<u64>,
    /// Size of those parts in bytes (`--multipart`).
    pub multipart_bytes: Option<u64>,
}

fn record(bucket: &str, prefix: &str, usage: &Usage) -> UsageRecord {
    UsageRecord {
        bucket: bucket.to_owned(),
        prefix: prefix.to_owned(),
        objects: usage.objects,
        bytes: usage.bytes,
        noncurrent_versions: usage.noncurrent.map(|n| n.versions),
        noncurrent_bytes: usage.noncurrent.map(|n| n.bytes),
        delete_markers: usage.noncurrent.map(|n| n.delete_markers),
        multipart_uploads: usage.multipart.map(|m| m.uploads),
        multipart_parts: usage.multipart.map(|m| m.parts),
        multipart_bytes: usage.multipart.map(|m| m.bytes),
    }
}

fn size(bytes: u64) -> String {
    format!("{} ({bytes} bytes)", human_bytes(bytes))
}

fn lines(record: &UsageRecord) -> String {
    let mut text = vec![trf(
        "objects: {count}, {size}",
        &[
            ("count", &record.objects.to_string()),
            ("size", &size(record.bytes)),
        ],
    )];
    if let (Some(count), Some(bytes), Some(markers)) = (
        record.noncurrent_versions,
        record.noncurrent_bytes,
        record.delete_markers,
    ) {
        text.push(trf(
            "non-current versions: {count}, {size}",
            &[("count", &count.to_string()), ("size", &size(bytes))],
        ));
        text.push(trf(
            "delete markers: {count}",
            &[("count", &markers.to_string())],
        ));
    }
    if let (Some(uploads), Some(parts), Some(bytes)) = (
        record.multipart_uploads,
        record.multipart_parts,
        record.multipart_bytes,
    ) {
        text.push(trf(
            "unfinished multipart uploads: {count} ({parts} parts), {size}",
            &[
                ("count", &uploads.to_string()),
                ("parts", &parts.to_string()),
                ("size", &size(bytes)),
            ],
        ));
    }
    text.join("\n")
}

/// Adds up the objects under the URI by listing them and prints the totals.
pub async fn run(ctx: &Context, args: &DuArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let prefix = args.uri.key();
    let usage = ctx
        .s3
        .compute_usage(
            bucket,
            prefix,
            UsageOptions {
                versions: args.versions,
                multipart: args.multipart,
            },
            None,
            |_| {},
        )
        .await?;
    let record = record(bucket, prefix, &usage);
    out.row(&record, || lines(&record))
}
