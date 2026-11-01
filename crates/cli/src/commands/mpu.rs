//! `bb mpu list` and `bb mpu abort`: uncompleted multipart uploads.

use bucketbridge_core::Error;
use bucketbridge_core::model::MultipartUpload;

use super::timestamp;
use crate::cli::{MpuAbortArgs, MpuListArgs};
use crate::context::Context;
use crate::output::{Batch, Failure, Output};
use crate::records::{AbortRecord, UploadRecord};

/// Lists uploads of one bucket, or of every bucket (each addressed in its
/// own region; the transport resolves bucket regions itself).
pub async fn list(ctx: &Context, args: &MpuListArgs, out: &mut Output) -> Result<(), Failure> {
    let buckets: Vec<String> = match &args.bucket {
        Some(bucket) => vec![bucket.clone()],
        None => ctx
            .s3
            .list_buckets()
            .await?
            .into_iter()
            .map(|bucket| bucket.name)
            .collect(),
    };
    for bucket in buckets {
        for upload in ctx.s3.list_multipart_uploads(&bucket, &args.prefix).await? {
            let record = upload_record(&bucket, upload);
            out.row(&record, || upload_line(&record))?;
        }
    }
    Ok(())
}

/// Aborts one upload (`--key` and `--upload-id`) or every upload of the
/// bucket under `--prefix` (`--all`).
pub async fn abort(ctx: &Context, args: &MpuAbortArgs, out: &mut Output) -> Result<(), Failure> {
    let bucket = args.bucket.as_str();
    if args.all {
        let prefix = args.prefix.as_deref().unwrap_or("");
        let uploads = ctx.s3.list_multipart_uploads(bucket, prefix).await?;
        let mut batch = Batch::default();
        for upload in uploads {
            let (record, error) =
                abort_one(ctx, bucket, &upload.key, &upload.upload_id, args.dry_run).await;
            match error {
                None => {
                    batch.succeeded();
                    out.change(&record, || abort_line(&record))?;
                }
                Some(err) => {
                    batch.failed(err.kind(), err.to_string());
                    let message = format!(
                        "failed to abort upload {} of s3://{bucket}/{}: {err}",
                        record.upload_id, record.key
                    );
                    out.item_failed(&record, || message)?;
                }
            }
        }
        return batch.finish("uploads", "could not be aborted");
    }
    let (Some(key), Some(upload_id)) = (&args.key, &args.upload_id) else {
        return Err(Error::Usage("give --key and --upload-id, or --all".into()).into());
    };
    match abort_one(ctx, bucket, key, upload_id, args.dry_run).await {
        (record, None) => out.change(&record, || abort_line(&record)),
        (_, Some(err)) => Err(err.into()),
    }
}

/// Aborts one upload; returns the record and the error, if any.
async fn abort_one(
    ctx: &Context,
    bucket: &str,
    key: &str,
    upload_id: &str,
    dry_run: bool,
) -> (AbortRecord, Option<Error>) {
    let mut record = AbortRecord {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        upload_id: upload_id.to_owned(),
        dry_run,
        aborted: false,
        error: None,
    };
    if dry_run {
        return (record, None);
    }
    match ctx.s3.abort_multipart_upload(bucket, key, upload_id).await {
        Ok(()) => {
            record.aborted = true;
            (record, None)
        }
        Err(err) => {
            record.error = Some(err.to_payload());
            (record, Some(err))
        }
    }
}

fn upload_record(bucket: &str, upload: MultipartUpload) -> UploadRecord {
    UploadRecord {
        bucket: bucket.to_owned(),
        key: upload.key,
        upload_id: upload.upload_id,
        initiated: upload.initiated,
        storage_class: upload.storage_class,
        initiator: upload.initiator,
    }
}

/// `initiated  s3://bucket/key  upload-id`
fn upload_line(record: &UploadRecord) -> String {
    format!(
        "{:<20}  s3://{}/{}  {}",
        timestamp(record.initiated),
        record.bucket,
        record.key,
        record.upload_id
    )
}

fn abort_line(record: &AbortRecord) -> String {
    let verb = if record.dry_run {
        "would abort"
    } else {
        "aborted"
    };
    format!(
        "{verb} upload {} of s3://{}/{}",
        record.upload_id, record.bucket, record.key
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn lines() {
        let record = upload_record(
            "b",
            MultipartUpload {
                key: "big.bin".into(),
                upload_id: "u1".into(),
                initiated: Some(Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap()),
                storage_class: None,
                initiator: None,
            },
        );
        assert_eq!(
            upload_line(&record),
            "2026-10-29T08:15:00Z  s3://b/big.bin  u1"
        );
        let abort = AbortRecord {
            bucket: "b".into(),
            key: "k".into(),
            upload_id: "u1".into(),
            dry_run: true,
            aborted: false,
            error: None,
        };
        assert_eq!(abort_line(&abort), "would abort upload u1 of s3://b/k");
    }
}
