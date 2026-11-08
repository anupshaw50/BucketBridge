//! `bb tag`, `bb storage-class`, `bb sse` and `bb acl`: object metadata.
//!
//! Each command works on one object, or with `--recursive` on every object
//! under a prefix (folder markers are skipped), 200 keys at a time with up to
//! 8 objects in flight. Every object gets a record; a failed object never
//! stops the others, and the exit code is that of the first failure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::pin::Pin;

use bucketbridge_core::model::{ObjectEntry, Sse};
use bucketbridge_core::objectmeta::{
    AclGrant, BatchOptions, CustomerKey, EncryptionChange, ObjectChange, TagEdit,
};
use bucketbridge_core::{BbError, Error, S3Uri};
use futures::{Stream, StreamExt, TryStreamExt};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::cli::{
    AclGetArgs, AclSetArgs, SseArgs, SseMode, StorageClassArgs, TagDeleteArgs, TagGetArgs,
    TagSetArgs,
};
use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::output::{Batch, Failure, Output};

/// Keys handled per round.
const CHUNK: usize = 200;

/// The objects a command works on, handed out in chunks.
pub(super) enum KeyChunks {
    Single(Option<String>),
    Listing(Pin<Box<dyn Stream<Item = bucketbridge_core::Result<ObjectEntry>> + Send>>),
    Done,
}

impl KeyChunks {
    /// An object, or with `recursive` every object under a prefix.
    pub(super) fn new(ctx: &Context, uri: &S3Uri, recursive: bool) -> Result<Self, Error> {
        match (recursive, uri.is_prefix()) {
            (false, true) => Err(Error::Usage(trf(
                "{uri} is a prefix; add --recursive to change every object under it",
                &[("uri", &uri.to_string())],
            ))),
            (true, false) => Err(Error::Usage(trf(
                "--recursive needs a prefix ending in '/', for example {prefixish}/",
                &[("prefixish", &uri.to_string())],
            ))),
            (false, false) => Ok(KeyChunks::Single(Some(uri.key().to_owned()))),
            (true, true) => Ok(KeyChunks::Listing(Box::pin(
                ctx.s3.list_all_stream(uri.bucket(), uri.key()),
            ))),
        }
    }

    pub(super) async fn next(&mut self) -> Result<Option<Vec<String>>, Error> {
        match self {
            KeyChunks::Single(key) => {
                let key = key.take();
                *self = KeyChunks::Done;
                Ok(key.map(|key| vec![key]))
            }
            KeyChunks::Listing(stream) => {
                let mut chunk = Vec::with_capacity(CHUNK);
                let mut finished = false;
                while chunk.len() < CHUNK {
                    match stream.try_next().await? {
                        Some(entry) if entry.key.ends_with('/') => {}
                        Some(entry) => chunk.push(entry.key),
                        None => {
                            finished = true;
                            break;
                        }
                    }
                }
                if finished {
                    *self = KeyChunks::Done;
                }
                Ok((!chunk.is_empty()).then_some(chunk))
            }
            KeyChunks::Done => Ok(None),
        }
    }
}

/// Prints one result: a record (and line) for an object that went well, a
/// failure line (or record with `--json`) for one that did not.
pub(super) fn emit<R: Serialize>(
    out: &mut Output,
    batch: &mut Batch,
    uri: &str,
    change: &ObjectChange,
    action: &str,
    record: &R,
    line: String,
) -> Result<(), Failure> {
    match &change.error {
        None => {
            batch.succeeded();
            out.change(record, || line)?;
            for warning in &change.warnings {
                out.notice(&trf(
                    "bb: warning: {uri}: {warning}",
                    &[("uri", uri), ("warning", warning)],
                ));
            }
        }
        Some(error) => {
            batch.failed(error.code, error.message.clone());
            let message = trf(
                "failed to {action} {uri}: {message}",
                &[
                    ("action", tr(action).as_str()),
                    ("uri", uri),
                    ("message", error.message.as_str()),
                ],
            );
            out.item_failed(record, || message)?;
        }
    }
    Ok(())
}

pub(super) fn object_uri(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
}

/// Reads an SSE-C key from a file (32 raw bytes, 64 hex digits or base64).
fn read_key(path: &Path, what: &str) -> Result<CustomerKey, Error> {
    let bytes = Zeroizing::new(
        std::fs::read(path).map_err(|err| {
            Error::io(
                trf(
                    "reading the {what} {path}",
                    &[
                        ("what", tr(what).as_str()),
                        ("path", path.display().to_string().as_str()),
                    ],
                ),
                err,
            )
        })?,
    );
    CustomerKey::from_file_contents(&bytes).map_err(|err| {
        err.with_context(trf(
            "the {what} {path}",
            &[
                ("what", tr(what).as_str()),
                ("path", path.display().to_string().as_str()),
            ],
        ))
    })
}

// ---- tags ----

/// `bb tag get`
#[derive(Serialize)]
struct TagsRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    tags: Option<&'a BTreeMap<String, String>>,
    error: Option<BbError>,
}

/// Prints the tags of the objects: `s3://bucket/key<TAB>KEY=VALUE`, one line
/// per tag, nothing for an object without tags; with `--json` one record per
/// object.
pub async fn tag_get(ctx: &Context, args: &TagGetArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results: Vec<(String, bucketbridge_core::Result<BTreeMap<String, String>>)> =
            futures::stream::iter(chunk)
                .map(|key| async {
                    let tags = ctx.s3.get_object_tags(bucket, &key).await;
                    (key, tags)
                })
                .buffered(BatchOptions::default().concurrency)
                .collect()
                .await;
        for (key, tags) in results {
            let uri = object_uri(bucket, &key);
            match tags {
                Ok(tags) => {
                    batch.succeeded();
                    let record = TagsRecord {
                        bucket,
                        key: &key,
                        tags: Some(&tags),
                        error: None,
                    };
                    if out.json() {
                        out.row(&record, String::new)?;
                    } else {
                        for (name, value) in &tags {
                            out.row(&record, || format!("{uri}\t{name}={value}"))?;
                        }
                    }
                }
                Err(err) => {
                    batch.failed(err.kind(), err.to_string());
                    let record = TagsRecord {
                        bucket,
                        key: &key,
                        tags: None,
                        error: Some(err.to_payload()),
                    };
                    out.item_failed(&record, || {
                        trf(
                            "failed to read the tags of {uri}: {error}",
                            &[("uri", &uri), ("error", &err.to_string())],
                        )
                    })?;
                }
            }
        }
    }
    batch.finish("objects", "could not be read")
}

/// `bb tag set`
#[derive(Serialize)]
struct TagSetRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    tags: &'a BTreeMap<String, String>,
    replace: bool,
    dry_run: bool,
    changed: bool,
    warnings: &'a [String],
    error: Option<BbError>,
}

/// Adds tags to the objects, or with `--replace` makes them the whole set.
pub async fn tag_set(ctx: &Context, args: &TagSetArgs, out: &mut Output) -> Result<(), Failure> {
    let tags: BTreeMap<String, String> = args.tags.iter().cloned().collect();
    let edit = if args.replace {
        TagEdit::Replace { tags: tags.clone() }
    } else {
        TagEdit::Merge {
            set: tags.clone(),
            remove: BTreeSet::new(),
        }
    };
    let bucket = args.uri.bucket();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .update_object_tags(bucket, &chunk, &edit, options)
            .await?;
        for change in &results {
            let uri = object_uri(bucket, &change.key);
            let record = TagSetRecord {
                bucket,
                key: &change.key,
                tags: &tags,
                replace: args.replace,
                dry_run: args.dry_run,
                changed: change.changed,
                warnings: &change.warnings,
                error: change.error.clone(),
            };
            let line = match (change.changed, args.dry_run) {
                (true, true) => trf("would tag {uri}", &[("uri", &uri)]),
                (true, false) => trf("tagged {uri}", &[("uri", &uri)]),
                (false, _) => trf("unchanged {uri}", &[("uri", &uri)]),
            };
            emit(out, &mut batch, &uri, change, "tag", &record, line)?;
        }
    }
    batch.finish("objects", "could not be tagged")
}

/// `bb tag delete`
#[derive(Serialize)]
struct TagDeleteRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    /// The keys removed; `null` means every tag.
    removed: Option<&'a [String]>,
    dry_run: bool,
    changed: bool,
    warnings: &'a [String],
    error: Option<BbError>,
}

/// Removes the named tags from the objects, or every tag.
pub async fn tag_delete(
    ctx: &Context,
    args: &TagDeleteArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let edit = if args.keys.is_empty() {
        TagEdit::Replace {
            tags: BTreeMap::new(),
        }
    } else {
        TagEdit::Merge {
            set: BTreeMap::new(),
            remove: args.keys.iter().cloned().collect(),
        }
    };
    let bucket = args.uri.bucket();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .update_object_tags(bucket, &chunk, &edit, options)
            .await?;
        for change in &results {
            let uri = object_uri(bucket, &change.key);
            let record = TagDeleteRecord {
                bucket,
                key: &change.key,
                removed: (!args.keys.is_empty()).then_some(args.keys.as_slice()),
                dry_run: args.dry_run,
                changed: change.changed,
                warnings: &change.warnings,
                error: change.error.clone(),
            };
            let line = match (change.changed, args.dry_run) {
                (true, true) => trf("would remove tags from {uri}", &[("uri", &uri)]),
                (true, false) => trf("removed tags from {uri}", &[("uri", &uri)]),
                (false, _) => trf("unchanged {uri}", &[("uri", &uri)]),
            };
            emit(
                out,
                &mut batch,
                &uri,
                change,
                "remove tags from",
                &record,
                line,
            )?;
        }
    }
    batch.finish("objects", "could not be changed")
}

pub(super) fn batch_options(dry_run: bool) -> BatchOptions {
    BatchOptions {
        dry_run,
        ..BatchOptions::default()
    }
}

// ---- storage class ----

/// `bb storage-class`
#[derive(Serialize)]
struct ClassRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    storage_class: &'a str,
    previous: Option<&'a str>,
    dry_run: bool,
    changed: bool,
    warnings: &'a [String],
    error: Option<BbError>,
}

/// Changes the storage class of the objects.
pub async fn storage_class(
    ctx: &Context,
    args: &StorageClassArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let key = args
        .key_file
        .as_deref()
        .map(|path| read_key(path, "key file"))
        .transpose()?;
    let bucket = args.uri.bucket();
    let class = args.class.as_str();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .set_object_storage_class(bucket, &chunk, args.class, key.as_ref(), options)
            .await?;
        for change in &results {
            let uri = object_uri(bucket, &change.key);
            let previous = change.previous.as_deref();
            let record = ClassRecord {
                bucket,
                key: &change.key,
                storage_class: class,
                previous,
                dry_run: args.dry_run,
                changed: change.changed,
                warnings: &change.warnings,
                error: change.error.clone(),
            };
            let line = match (change.changed, args.dry_run) {
                (true, true) => trf(
                    "would change {uri} from {previous} to {class}",
                    &[
                        ("uri", &uri),
                        ("previous", previous.unwrap_or("?")),
                        ("class", class),
                    ],
                ),
                (true, false) => trf(
                    "changed {uri} from {previous} to {class}",
                    &[
                        ("uri", &uri),
                        ("previous", previous.unwrap_or("?")),
                        ("class", class),
                    ],
                ),
                (false, _) => trf(
                    "unchanged {uri} (already {what})",
                    &[("uri", &uri), ("what", class)],
                ),
            };
            emit(
                out,
                &mut batch,
                &uri,
                change,
                "change the storage class of",
                &record,
                line,
            )?;
        }
    }
    batch.finish("objects", "could not be changed")
}

// ---- encryption ----

/// `bb sse`
#[derive(Serialize)]
struct SseRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    /// The wanted encryption; an SSE-C key shows as its MD5 only.
    encryption: &'a Sse,
    previous: Option<&'a str>,
    dry_run: bool,
    changed: bool,
    warnings: &'a [String],
    error: Option<BbError>,
}

/// Checks the arguments of `bb sse` before any account is needed.
pub fn check_sse(args: &SseArgs) -> Result<(), Error> {
    match args.mode {
        SseMode::Customer if args.key_file.is_none() => Err(Error::Usage(tr(
            "SSE-C (c) needs the new key: pass --key-file FILE",
        ))),
        SseMode::S3 | SseMode::Kms(_) if args.key_file.is_some() => Err(Error::Usage(tr(
            "--key-file is the key of SSE-C (c); use --source-key-file for the key the objects have now",
        ))),
        SseMode::Kms(_) => Ok(()),
        _ if args.bucket_key => Err(Error::Usage(tr("--bucket-key only applies to kms"))),
        _ => Ok(()),
    }
}

/// Changes the server-side encryption of the objects.
pub async fn sse(ctx: &Context, args: &SseArgs, out: &mut Output) -> Result<(), Failure> {
    check_sse(args)?;
    let target = match (&args.mode, &args.key_file) {
        (SseMode::S3, _) => EncryptionChange::S3,
        (SseMode::Kms(key_id), _) => EncryptionChange::Kms {
            key_id: key_id.clone(),
            bucket_key: args.bucket_key,
        },
        (SseMode::Customer, Some(path)) => EncryptionChange::Customer(read_key(path, "key file")?),
        (SseMode::Customer, None) => {
            return Err(Error::Usage(tr("SSE-C (c) needs --key-file")).into());
        }
    };
    let source = args
        .source_key_file
        .as_deref()
        .map(|path| read_key(path, "source key file"))
        .transpose()?;
    let described = target.to_sse();
    let bucket = args.uri.bucket();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .set_object_encryption(bucket, &chunk, &target, source.as_ref(), options)
            .await?;
        for change in &results {
            let uri = object_uri(bucket, &change.key);
            let previous = change.previous.as_deref();
            let record = SseRecord {
                bucket,
                key: &change.key,
                encryption: &described,
                previous,
                dry_run: args.dry_run,
                changed: change.changed,
                warnings: &change.warnings,
                error: change.error.clone(),
            };
            let name = describe(&described);
            let line = match (change.changed, args.dry_run) {
                (true, true) => trf(
                    "would change the encryption of {uri} from {previous} to {name}",
                    &[
                        ("uri", &uri),
                        ("previous", previous.unwrap_or("?")),
                        ("name", &name),
                    ],
                ),
                (true, false) => trf(
                    "changed the encryption of {uri} from {previous} to {name}",
                    &[
                        ("uri", &uri),
                        ("previous", previous.unwrap_or("?")),
                        ("name", &name),
                    ],
                ),
                (false, _) => trf(
                    "unchanged {uri} (already {what})",
                    &[("uri", &uri), ("what", &name)],
                ),
            };
            emit(
                out,
                &mut batch,
                &uri,
                change,
                "change the encryption of",
                &record,
                line,
            )?;
        }
    }
    batch.finish("objects", "could not be changed")
}

/// `s3`, `kms`, `kms:KEY` or `c`.
fn describe(sse: &Sse) -> String {
    match sse {
        Sse::None => "none".to_owned(),
        Sse::S3 => "s3".to_owned(),
        Sse::Kms {
            key_id: Some(key_id),
            ..
        } => format!("kms:{key_id}"),
        Sse::Kms { key_id: None, .. } => "kms".to_owned(),
        Sse::Customer { .. } => "c".to_owned(),
    }
}

// ---- ACLs ----

/// `bb acl get`
#[derive(Serialize)]
struct AclGetRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    /// The canned ACL the grants equal, or `custom`.
    acl: Option<&'a str>,
    owner: Option<&'a str>,
    grants: &'a [AclGrant],
    error: Option<BbError>,
}

/// Prints the ACL of the objects: `s3://bucket/key<TAB>private`, or
/// `custom` with the grants.
pub async fn acl_get(ctx: &Context, args: &AclGetArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results: Vec<(String, bucketbridge_core::Result<_>)> = futures::stream::iter(chunk)
            .map(|key| async {
                let acl = ctx.s3.get_object_acl(bucket, &key).await;
                (key, acl)
            })
            .buffered(BatchOptions::default().concurrency)
            .collect()
            .await;
        for (key, acl) in results {
            let uri = object_uri(bucket, &key);
            match acl {
                Ok(acl) => {
                    batch.succeeded();
                    let record = AclGetRecord {
                        bucket,
                        key: &key,
                        acl: Some(acl.summary()),
                        owner: acl.owner.as_deref(),
                        grants: &acl.grants,
                        error: None,
                    };
                    out.row(&record, || {
                        if acl.canned.is_some() {
                            format!("{uri}\t{}", acl.summary())
                        } else {
                            let grants: Vec<String> = acl
                                .grants
                                .iter()
                                .map(|g| format!("{}:{:?}", g.grantee, g.permission))
                                .collect();
                            format!("{uri}\tcustom\t{}", grants.join(","))
                        }
                    })?;
                }
                Err(err) => {
                    batch.failed(err.kind(), err.to_string());
                    let record = AclGetRecord {
                        bucket,
                        key: &key,
                        acl: None,
                        owner: None,
                        grants: &[],
                        error: Some(err.to_payload()),
                    };
                    out.item_failed(&record, || {
                        trf(
                            "failed to read the ACL of {uri}: {error}",
                            &[("uri", &uri), ("error", &err.to_string())],
                        )
                    })?;
                }
            }
        }
    }
    batch.finish("objects", "could not be read")
}

/// `bb acl set`
#[derive(Serialize)]
struct AclSetRecord<'a> {
    bucket: &'a str,
    key: &'a str,
    acl: &'a str,
    previous: Option<&'a str>,
    dry_run: bool,
    changed: bool,
    error: Option<BbError>,
}

/// Applies a canned ACL to the objects.
pub async fn acl_set(ctx: &Context, args: &AclSetArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.uri.bucket();
    let acl = args.acl.as_str();
    let options = batch_options(args.dry_run);
    let mut keys = KeyChunks::new(ctx, &args.uri, args.recursive)?;
    let mut batch = Batch::default();
    while let Some(chunk) = keys.next().await? {
        let results = ctx
            .s3
            .set_object_acl(bucket, &chunk, args.acl, options)
            .await?;
        for change in &results {
            let uri = object_uri(bucket, &change.key);
            let previous = change.previous.as_deref();
            let record = AclSetRecord {
                bucket,
                key: &change.key,
                acl,
                previous,
                dry_run: args.dry_run,
                changed: change.changed,
                error: change.error.clone(),
            };
            let was = previous.map_or_else(String::new, |p| trf(" (was {previous})", &[("previous", p)]));
            let line = match (change.changed, args.dry_run) {
                (true, true) => trf(
                    "would set the ACL of {uri} to {acl}{was}",
                    &[("uri", &uri), ("acl", acl), ("was", was.as_str())],
                ),
                (true, false) => trf(
                    "set the ACL of {uri} to {acl}{was}",
                    &[("uri", &uri), ("acl", acl), ("was", was.as_str())],
                ),
                (false, _) => trf(
                    "unchanged {uri} (already {what})",
                    &[("uri", &uri), ("what", acl)],
                ),
            };
            emit(
                out,
                &mut batch,
                &uri,
                change,
                "set the ACL of",
                &record,
                line,
            )?;
        }
    }
    batch.finish("objects", "could not be changed")
}
