//! `bb mb` and `bb rb`.

use crate::cli::{MbArgs, RbArgs};
use crate::context::Context;
use crate::i18n::trf;
use crate::output::{Failure, Output};
use crate::records::BucketRecord;

/// Creates a bucket (`--region` defaults to the account region).
pub async fn make(ctx: &Context, args: &MbArgs, out: &mut Output) -> Result<(), Failure> {
    ctx.s3
        .create_bucket(&args.bucket, args.region.as_deref())
        .await?;
    let record = BucketRecord {
        bucket: args.bucket.clone(),
        region: args.region.clone(),
    };
    out.change(&record, || match &args.region {
        Some(region) => trf(
            "created bucket {bucket} in {region}",
            &[("bucket", &args.bucket), ("region", region)],
        ),
        None => trf("created bucket {bucket}", &[("bucket", &args.bucket)]),
    })
}

/// Removes an empty bucket; a bucket that still holds objects (or versions)
/// is a conflict.
pub async fn remove(ctx: &Context, args: &RbArgs, out: &mut Output) -> Result<(), Failure> {
    ctx.s3.delete_bucket(&args.bucket).await?;
    let record = BucketRecord {
        bucket: args.bucket.clone(),
        region: None,
    };
    out.change(&record, || {
        trf("removed bucket {bucket}", &[("bucket", &args.bucket)])
    })
}
