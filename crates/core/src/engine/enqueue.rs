//! The enqueue commands of the desktop contract (`enqueue_upload`,
//! `enqueue_download`, `enqueue_copy`, `enqueue_move`, `enqueue_delete` and
//! `rename`): a selection is expanded ([`super::expand`]) into one task per
//! object and added to the transfer queue.
//!
//! * Every task of one call shares a new `group_id`.
//! * Two selected entries that would land on the same destination reject the
//!   call with `conflict` before anything is added (as does any other error
//!   of the expansion), so nothing is enqueued.
//! * An entry that cannot be added (an unreadable file, a key that would
//!   escape the download folder, a folder that cannot be listed, ...) becomes
//!   a `failed` task with the reason as its error. Its resume state marks it
//!   as rejected: retrying it fails again with the same error, and cancelling
//!   it touches nothing. To try again, select the entry again.

use std::path::{Path, PathBuf};

use crate::cse::{Compression, CseMode, CsePasswords};
use crate::secrets::SecretString;

use super::expand::{ExpandFailure, Expansion, expand_delete, expand_upload};
use super::job::JobOptions;
use super::runner::rejected;
use super::{TransferEngine, TransferOptions};
use crate::error::Result;
use crate::model::{Location, TransferKind, TransferTask};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;
use crate::transfer::{FailedTask, NewTask, TransferQueue};

/// `enqueue_upload`: uploads local files and folders to `prefix` in
/// `bucket` (folders keep their relative paths; empty folders become folder
/// markers).
pub async fn enqueue_upload(
    queue: &TransferQueue,
    account_id: &str,
    local_paths: &[PathBuf],
    bucket: &str,
    prefix: &str,
) -> Result<Vec<TransferTask>> {
    enqueue_upload_with(
        queue,
        account_id,
        local_paths,
        bucket,
        prefix,
        &JobOptions::default(),
    )
    .await
}

/// [`enqueue_upload`] with per-call options (overwrite policy, modification
/// times). `options.dst_account` does not apply to uploads.
pub async fn enqueue_upload_with(
    queue: &TransferQueue,
    account_id: &str,
    local_paths: &[PathBuf],
    bucket: &str,
    prefix: &str,
    options: &JobOptions,
) -> Result<Vec<TransferTask>> {
    options.validate()?;
    let batch = Batch::with_options(account_id, options);
    upload_in_batch(queue, batch, local_paths, bucket, prefix).await
}

/// `enqueue_upload` with client-side encryption: every file is compressed
/// (as `compression` says) and encrypted with `password` before it is
/// uploaded, and the objects carry the `bb-cse` metadata marker. The password
/// is kept in `passwords` (memory only) under the group of this call; the
/// group ID tells the runner to encrypt, also after a restart.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_upload_encrypted(
    queue: &TransferQueue,
    passwords: &CsePasswords,
    password: SecretString,
    compression: Compression,
    account_id: &str,
    local_paths: &[PathBuf],
    bucket: &str,
    prefix: &str,
) -> Result<Vec<TransferTask>> {
    check_password(&password)?;
    let batch = Batch::with_mode(account_id, CseMode::Encrypt(compression));
    passwords.insert(&batch.group_id, password);
    let group = batch.group_id.clone();
    let result = upload_in_batch(queue, batch, local_paths, bucket, prefix).await;
    if result.is_err() {
        passwords.forget(&group);
    }
    result
}

fn check_password(password: &SecretString) -> Result<()> {
    if password.is_empty() {
        return Err(crate::error::Error::Usage(
            "the password must not be empty".to_owned(),
        ));
    }
    Ok(())
}

async fn upload_in_batch(
    queue: &TransferQueue,
    batch: Batch,
    local_paths: &[PathBuf],
    bucket: &str,
    prefix: &str,
) -> Result<Vec<TransferTask>> {
    validate_bucket_name(bucket)?;
    let expansion = expand_upload(local_paths.to_vec(), prefix.to_owned()).await?;
    let s3 = |key: &str| Location::S3 {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    };
    batch.add(
        queue,
        expansion,
        |item| {
            (
                TransferKind::Upload,
                Location::Local {
                    path: item.path.clone(),
                },
                Some(s3(&item.key)),
                Some(item.size),
            )
        },
        |failure| {
            (
                TransferKind::Upload,
                Location::Local {
                    path: PathBuf::from(&failure.entry),
                },
                Some(s3(prefix)),
            )
        },
    )
}

/// `enqueue_download`: downloads keys and prefixes of `bucket` into
/// `dest_dir` (the parent part of every selected key is stripped).
pub async fn enqueue_download(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    bucket: &str,
    keys: &[String],
    dest_dir: &Path,
) -> Result<Vec<TransferTask>> {
    enqueue_download_with(
        queue,
        s3,
        account_id,
        bucket,
        keys,
        dest_dir,
        &JobOptions::default(),
    )
    .await
}

/// [`enqueue_download`] with per-call options (overwrite policy, modification
/// times). `options.dst_account` does not apply to downloads.
pub async fn enqueue_download_with(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    bucket: &str,
    keys: &[String],
    dest_dir: &Path,
    options: &JobOptions,
) -> Result<Vec<TransferTask>> {
    options.validate()?;
    let batch = Batch::with_options(account_id, options);
    download_in_batch(queue, batch, s3, bucket, keys, dest_dir).await
}

/// `enqueue_download` with client-side decryption: objects that start with
/// an encryption header are decrypted with `password` after the download
/// (`None`: encrypted objects fail with *password not found*; other objects
/// are saved as they are).
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_download_decrypted(
    queue: &TransferQueue,
    passwords: &CsePasswords,
    password: Option<SecretString>,
    s3: &S3,
    account_id: &str,
    bucket: &str,
    keys: &[String],
    dest_dir: &Path,
) -> Result<Vec<TransferTask>> {
    let batch = Batch::with_mode(account_id, CseMode::Decrypt);
    if let Some(password) = password.filter(|password| !password.is_empty()) {
        passwords.insert(&batch.group_id, password);
    }
    let group = batch.group_id.clone();
    let result = download_in_batch(queue, batch, s3, bucket, keys, dest_dir).await;
    if result.is_err() {
        passwords.forget(&group);
    }
    result
}

async fn download_in_batch(
    queue: &TransferQueue,
    batch: Batch,
    s3: &S3,
    bucket: &str,
    keys: &[String],
    dest_dir: &Path,
) -> Result<Vec<TransferTask>> {
    let engine = TransferEngine::new(s3.clone(), TransferOptions::default())?;
    let expansion = engine.expand_download(bucket, keys, dest_dir).await?;
    let source = |key: &str| Location::S3 {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    };
    batch.add(
        queue,
        expansion,
        |item| {
            let total = if item.is_folder { Some(0) } else { item.size };
            (
                TransferKind::Download,
                source(&item.key),
                Some(Location::Local {
                    path: item.path.clone(),
                }),
                total,
            )
        },
        |failure| {
            (
                TransferKind::Download,
                source(&failure.entry),
                Some(Location::Local {
                    path: dest_dir.to_path_buf(),
                }),
            )
        },
    )
}

/// `enqueue_copy`: copies keys and prefixes of `src_bucket` to `dst_prefix`
/// in `dst_bucket` on the server.
pub async fn enqueue_copy(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    src_bucket: &str,
    keys: &[String],
    dst_bucket: &str,
    dst_prefix: &str,
) -> Result<Vec<TransferTask>> {
    enqueue_copy_with(
        queue,
        s3,
        account_id,
        src_bucket,
        keys,
        dst_bucket,
        dst_prefix,
        &JobOptions::default(),
    )
    .await
}

/// [`enqueue_copy`] with per-call options: the overwrite policy, and
/// `dst_account`, the account that owns the destination bucket (a copy
/// between accounts; see `docs/engines.md`).
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_copy_with(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    src_bucket: &str,
    keys: &[String],
    dst_bucket: &str,
    dst_prefix: &str,
    options: &JobOptions,
) -> Result<Vec<TransferTask>> {
    options.validate()?;
    let engine = TransferEngine::new(s3.clone(), TransferOptions::default())?;
    let expansion = engine
        .expand_copy(src_bucket, keys, dst_bucket, dst_prefix)
        .await?;
    add_server_side(
        queue,
        Batch::with_options(account_id, options),
        TransferKind::Copy,
        expansion,
        (src_bucket, dst_bucket, dst_prefix),
    )
}

/// `enqueue_move`: like [`enqueue_copy`], deleting each source once its copy
/// exists.
pub async fn enqueue_move(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    src_bucket: &str,
    keys: &[String],
    dst_bucket: &str,
    dst_prefix: &str,
) -> Result<Vec<TransferTask>> {
    enqueue_move_with(
        queue,
        s3,
        account_id,
        src_bucket,
        keys,
        dst_bucket,
        dst_prefix,
        &JobOptions::default(),
    )
    .await
}

/// [`enqueue_move`] with per-call options (see [`enqueue_copy_with`]). A move
/// to another account deletes each source only after its copy was verified.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_move_with(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    src_bucket: &str,
    keys: &[String],
    dst_bucket: &str,
    dst_prefix: &str,
    options: &JobOptions,
) -> Result<Vec<TransferTask>> {
    options.validate()?;
    let engine = TransferEngine::new(s3.clone(), TransferOptions::default())?;
    let expansion = engine
        .expand_move(src_bucket, keys, dst_bucket, dst_prefix)
        .await?;
    add_server_side(
        queue,
        Batch::with_options(account_id, options),
        TransferKind::Move,
        expansion,
        (src_bucket, dst_bucket, dst_prefix),
    )
}

/// `enqueue_delete`: one task per key; a prefix (ending in `/`) is one task
/// that deletes everything below it.
pub async fn enqueue_delete(
    queue: &TransferQueue,
    account_id: &str,
    bucket: &str,
    keys: &[String],
) -> Result<Vec<TransferTask>> {
    let expansion = expand_delete(bucket, keys)?;
    let target = |key: &str| Location::S3 {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    };
    Batch::new(account_id).add(
        queue,
        expansion,
        |item| (TransferKind::Delete, target(&item.key), None, None),
        |failure| (TransferKind::Delete, target(&failure.entry), None),
    )
}

/// `rename`: renames an object, or a prefix with everything below it, as
/// move tasks; `conflict` when the destination exists (see
/// [`TransferEngine::plan_rename`]).
pub async fn enqueue_rename(
    queue: &TransferQueue,
    s3: &S3,
    account_id: &str,
    bucket: &str,
    from_key: &str,
    to_key: &str,
) -> Result<Vec<TransferTask>> {
    let engine = TransferEngine::new(s3.clone(), TransferOptions::default())?;
    let items = engine.plan_rename(bucket, from_key, to_key).await?;
    let expansion = Expansion {
        items,
        failures: Vec::new(),
    };
    add_server_side(
        queue,
        Batch::new(account_id),
        TransferKind::Move,
        expansion,
        (bucket, bucket, ""),
    )
}

/// Adds copy or move tasks: `(source bucket, destination bucket,
/// destination prefix)`.
fn add_server_side(
    queue: &TransferQueue,
    batch: Batch,
    kind: TransferKind,
    expansion: Expansion<super::CopyItem>,
    (src_bucket, dst_bucket, dst_prefix): (&str, &str, &str),
) -> Result<Vec<TransferTask>> {
    let location = |bucket: &str, key: &str| Location::S3 {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    };
    batch.add(
        queue,
        expansion,
        |item| {
            (
                kind,
                location(src_bucket, &item.src_key),
                Some(location(dst_bucket, &item.dst_key)),
                item.size,
            )
        },
        |failure| {
            (
                kind,
                location(src_bucket, &failure.entry),
                Some(location(dst_bucket, dst_prefix)),
            )
        },
    )
}

/// The tasks of one enqueue call: one account, one group.
struct Batch {
    account_id: String,
    group_id: String,
}

/// Kind, source, destination and total of an item's task.
type ItemTask = (TransferKind, Location, Option<Location>, Option<u64>);

/// Kind, source and destination of a failed entry's task.
type FailureTask = (TransferKind, Location, Option<Location>);

impl Batch {
    fn new(account_id: &str) -> Self {
        Batch {
            account_id: account_id.to_owned(),
            group_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// A batch whose group ID carries the per-call `options`.
    fn with_options(account_id: &str, options: &JobOptions) -> Self {
        Batch {
            account_id: account_id.to_owned(),
            group_id: options.group_id(uuid::Uuid::new_v4().to_string()),
        }
    }

    /// A batch whose group ID carries a client-side encryption mode.
    fn with_mode(account_id: &str, mode: CseMode) -> Self {
        Batch {
            account_id: account_id.to_owned(),
            group_id: mode.new_group_id(),
        }
    }

    fn task(
        &self,
        kind: TransferKind,
        src: Location,
        dst: Option<Location>,
        total: Option<u64>,
    ) -> NewTask {
        NewTask {
            account_id: self.account_id.clone(),
            group_id: Some(self.group_id.clone()),
            kind,
            src,
            dst,
            bytes_total: total,
        }
    }

    fn add<T>(
        &self,
        queue: &TransferQueue,
        expansion: Expansion<T>,
        item: impl Fn(&T) -> ItemTask,
        failure: impl Fn(&ExpandFailure) -> FailureTask,
    ) -> Result<Vec<TransferTask>> {
        let tasks = expansion
            .items
            .iter()
            .map(|entry| {
                let (kind, src, dst, total) = item(entry);
                self.task(kind, src, dst, total)
            })
            .collect();
        let failed = expansion
            .failures
            .into_iter()
            .map(|entry| {
                let (kind, src, dst) = failure(&entry);
                FailedTask {
                    task: self.task(kind, src, dst, None),
                    resume: rejected(entry.code, &entry.message),
                    error: entry.message,
                }
            })
            .collect();
        queue.add_tasks_with_failures(tasks, failed)
    }
}
