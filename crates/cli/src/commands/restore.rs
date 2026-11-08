//! `bb restore-version`: an older version becomes the current one.

use bucketbridge_core::Error;

use crate::cli::RestoreVersionArgs;
use crate::context::Context;
use crate::i18n::trf;
use crate::output::{Failure, Output};
use crate::records::RestoreRecord;

/// Runs `bb restore-version`: a server-side copy of the version over its key
/// (see `S3::restore_version`), which leaves every version in place.
pub async fn run(
    ctx: &Context,
    args: &RestoreVersionArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let uri = &args.uri;
    if uri.is_prefix() {
        return Err(Error::Usage(trf(
            "restore-version needs an object key; {uri} is a prefix",
            &[("uri", &uri.to_string())],
        ))
        .into());
    }
    let (bucket, key) = (uri.bucket(), uri.key());
    let outcome = ctx
        .s3
        .restore_version(bucket, key, &args.version_id)
        .await?;
    let record = RestoreRecord {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        restored_version_id: outcome.restored_version_id,
        version_id: outcome.version_id,
        etag: outcome.etag,
        size: outcome.size,
    };
    out.change(&record, || line(&record))
}

fn line(record: &RestoreRecord) -> String {
    let template = if record.version_id.is_some() {
        "restored version {version} of s3://{bucket}/{key} (new version {new_version})"
    } else {
        "restored version {version} of s3://{bucket}/{key}"
    };
    trf(
        template,
        &[
            ("version", record.restored_version_id.as_str()),
            ("bucket", record.bucket.as_str()),
            ("key", record.key.as_str()),
            ("new_version", record.version_id.as_deref().unwrap_or("")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::trf;

    #[test]
    fn lines_name_the_old_and_the_new_version() {
        let mut record = RestoreRecord {
            bucket: "b".into(),
            key: "k".into(),
            restored_version_id: "v1".into(),
            version_id: Some("v9".into()),
            etag: None,
            size: 3,
        };
        assert_eq!(
            line(&record),
            trf(
                "restored version {version} of s3://{bucket}/{key} (new version {new_version})",
                &[("version", "v1"), ("bucket", "b"), ("key", "k"), ("new_version", "v9")]
            )
        );
        record.version_id = None;
        assert_eq!(
            line(&record),
            trf(
                "restored version {version} of s3://{bucket}/{key}",
                &[("version", "v1"), ("bucket", "b"), ("key", "k")]
            )
        );
    }
}
