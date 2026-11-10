//! `bb mb` and `bb rb`.

use bucketbridge_core::bucketcfg::PublicAccessBlock;
use bucketbridge_core::bucketcreate::CreateBucketOptions;

use crate::cli::{HoldState, MbArgs, RbArgs};
use crate::context::Context;
use crate::i18n::trf;
use crate::output::{Failure, Output};
use crate::records::BucketRecord;

/// Creates a bucket (`--region` defaults to the account region), with the
/// advanced options when they are given.
pub async fn make(ctx: &Context, args: &MbArgs, out: &mut Output) -> Result<(), Failure> {
    let options = CreateBucketOptions {
        object_lock: args.object_lock,
        ownership: args.ownership,
        public_access_block: args.public_access_block.map(|state| {
            let on = state == HoldState::On;
            PublicAccessBlock {
                block_public_acls: on,
                ignore_public_acls: on,
                block_public_policy: on,
                restrict_public_buckets: on,
            }
        }),
    };
    let result = ctx
        .s3
        .create_bucket_with(&args.bucket, args.region.as_deref(), &options)
        .await?;
    let mut record = BucketRecord::new(&args.bucket, args.region.clone());
    record.object_lock = args.object_lock.then_some(true);
    record.ownership = args.ownership.map(|o| o.as_str().to_owned());
    record.public_access_block = args.public_access_block.map(|state| state == HoldState::On);
    record.warnings.clone_from(&result.warnings);
    out.change(&record, || match &args.region {
        Some(region) => trf(
            "created bucket {bucket} in {region}",
            &[("bucket", &args.bucket), ("region", region)],
        ),
        None => trf("created bucket {bucket}", &[("bucket", &args.bucket)]),
    })?;
    for warning in &result.warnings {
        out.notice(&trf("bb: warning: {warning}", &[("warning", warning)]));
    }
    Ok(())
}

/// Removes an empty bucket; a bucket that still holds objects (or versions)
/// is a conflict.
pub async fn remove(ctx: &Context, args: &RbArgs, out: &mut Output) -> Result<(), Failure> {
    ctx.s3.delete_bucket(&args.bucket).await?;
    let record = BucketRecord::new(&args.bucket, None);
    out.change(&record, || {
        trf("removed bucket {bucket}", &[("bucket", &args.bucket)])
    })
}
