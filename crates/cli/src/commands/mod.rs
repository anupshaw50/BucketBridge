//! One module per command.

pub mod account;
pub mod accountfile;
pub mod bucket;
pub mod bucketcfg;
pub mod clonebucket;
pub mod cloudfront;
pub mod copy;
pub mod cse;
pub mod du;
pub mod edit;
pub mod export;
pub mod iam;
pub mod ls;
#[cfg(feature = "mount")]
pub mod mount;
pub mod mpu;
pub mod objectlock;
pub mod objectmeta;
pub mod presign;
pub mod restore;
pub mod rm;
pub mod scan;
pub mod sync;
pub mod syncjobs;
mod versions;

use bucketbridge_core::aws_sdk_s3::primitives::DateTime as SdkDateTime;
use chrono::{DateTime, SecondsFormat, Utc};

/// RFC 3339 time with whole seconds, or `-` when unknown.
pub(crate) fn timestamp(time: Option<DateTime<Utc>>) -> String {
    time.map_or_else(
        || "-".to_owned(),
        |t| t.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

/// An SDK timestamp in UTC.
pub(crate) fn utc(time: &SdkDateTime) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos())
}

/// An entity tag without the quotes S3 puts around it.
pub(crate) fn unquote(etag: &str) -> String {
    etag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(etag)
        .to_owned()
}

/// A size reported by the SDK; a missing or negative size counts as 0.
pub(crate) fn size_of(size: Option<i64>) -> u64 {
    size.and_then(|size| u64::try_from(size).ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(None), "-");
        let time = Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap();
        assert_eq!(timestamp(Some(time)), "2026-10-29T08:15:00Z");
        let sdk = SdkDateTime::from_secs_and_nanos(time.timestamp(), 5_000_000);
        assert_eq!(
            utc(&sdk).unwrap().timestamp_millis(),
            time.timestamp_millis() + 5
        );
    }

    #[test]
    fn etags_and_sizes() {
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("abc"), "abc");
        assert_eq!(unquote("\""), "\"");
        assert_eq!(size_of(Some(7)), 7);
        assert_eq!(size_of(Some(-1)), 0);
        assert_eq!(size_of(None), 0);
    }
}
