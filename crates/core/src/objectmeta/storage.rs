//! Storage classes, the archive state of an object, and changing the class.

use std::fmt;
use std::str::FromStr;

use aws_sdk_s3::operation::head_object::HeadObjectOutput;
use aws_sdk_s3::types::{ArchiveStatus, StorageClass as SdkStorageClass};
use aws_smithy_types::date_time::Format;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::inplace::{Edit, Plan};
use super::sse::CustomerKey;
use super::{BatchOptions, ObjectChange, run_batch};
use crate::error::{Error, Result};
use crate::ops::{S3, to_utc};
use crate::s3uri::validate_bucket_name;

/// The storage classes a stored object can be switched to.
///
/// The serde form and [`StorageClass::as_str`] are the names S3 uses
/// (`STANDARD_IA`, `GLACIER_IR`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StorageClass {
    /// `STANDARD`: frequently accessed data.
    Standard,
    /// `STANDARD_IA`: infrequent access.
    StandardIa,
    /// `ONEZONE_IA`: infrequent access, one availability zone.
    OnezoneIa,
    /// `GLACIER_IR`: archive with instant retrieval.
    GlacierIr,
    /// `GLACIER`: archive; objects must be restored before they can be read.
    Glacier,
    /// `DEEP_ARCHIVE`: the coldest archive; objects must be restored first.
    DeepArchive,
    /// `INTELLIGENT_TIERING`: S3 moves the object between tiers.
    IntelligentTiering,
    /// `REDUCED_REDUNDANCY`: legacy class.
    ReducedRedundancy,
}

impl StorageClass {
    /// Every class, in the order the plan lists them.
    pub const ALL: [StorageClass; 8] = [
        StorageClass::Standard,
        StorageClass::StandardIa,
        StorageClass::OnezoneIa,
        StorageClass::GlacierIr,
        StorageClass::Glacier,
        StorageClass::DeepArchive,
        StorageClass::IntelligentTiering,
        StorageClass::ReducedRedundancy,
    ];

    /// The name S3 uses.
    pub fn as_str(self) -> &'static str {
        match self {
            StorageClass::Standard => "STANDARD",
            StorageClass::StandardIa => "STANDARD_IA",
            StorageClass::OnezoneIa => "ONEZONE_IA",
            StorageClass::GlacierIr => "GLACIER_IR",
            StorageClass::Glacier => "GLACIER",
            StorageClass::DeepArchive => "DEEP_ARCHIVE",
            StorageClass::IntelligentTiering => "INTELLIGENT_TIERING",
            StorageClass::ReducedRedundancy => "REDUCED_REDUNDANCY",
        }
    }

    /// The class as the SDK names it.
    pub(crate) fn to_sdk(self) -> SdkStorageClass {
        match self {
            StorageClass::Standard => SdkStorageClass::Standard,
            StorageClass::StandardIa => SdkStorageClass::StandardIa,
            StorageClass::OnezoneIa => SdkStorageClass::OnezoneIa,
            StorageClass::GlacierIr => SdkStorageClass::GlacierIr,
            StorageClass::Glacier => SdkStorageClass::Glacier,
            StorageClass::DeepArchive => SdkStorageClass::DeepArchive,
            StorageClass::IntelligentTiering => SdkStorageClass::IntelligentTiering,
            StorageClass::ReducedRedundancy => SdkStorageClass::ReducedRedundancy,
        }
    }
}

impl fmt::Display for StorageClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StorageClass {
    type Err = Error;

    /// Accepts the S3 names in any letter case, with `-` for `_`.
    fn from_str(text: &str) -> Result<Self> {
        let wanted = text.trim().to_ascii_uppercase().replace('-', "_");
        StorageClass::ALL
            .into_iter()
            .find(|class| class.as_str() == wanted)
            .ok_or_else(|| {
                let valid: Vec<&str> = StorageClass::ALL.iter().map(|c| c.as_str()).collect();
                Error::Usage(format!(
                    "'{}' is not a storage class this tool can set; use one of {}",
                    text.escape_debug(),
                    valid.join(", ")
                ))
            })
    }
}

/// Restore state of an archived object, from the `x-amz-restore` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreStatus {
    /// A restore is still running: the data cannot be read yet.
    pub in_progress: bool,
    /// When the restored copy expires (known once the restore finished).
    pub expires: Option<DateTime<Utc>>,
}

/// Parses `ongoing-request="false", expiry-date="Fri, 21 Dec 2012 00:00:00 GMT"`.
/// `None` when the header does not say whether a restore is running.
pub(crate) fn parse_restore(header: &str) -> Option<RestoreStatus> {
    let in_progress = quoted_value(header, "ongoing-request")? == "true";
    let expires = quoted_value(header, "expiry-date").and_then(|text| {
        aws_smithy_types::DateTime::from_str(text, Format::HttpDate)
            .ok()
            .and_then(|time| to_utc(&time))
    });
    Some(RestoreStatus {
        in_progress,
        expires,
    })
}

/// The value of `name="value"` in a header such as `x-amz-restore`.
fn quoted_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let start = header.find(&format!("{name}=\""))? + name.len() + 2;
    let rest = header.get(start..)?;
    rest.split_once('"').map(|(value, _)| value)
}

/// The name of the archive an object sits in while its data cannot be read:
/// `GLACIER` and `DEEP_ARCHIVE` objects that are not restored (a restore
/// that is still running does not count), and Intelligent-Tiering objects in
/// an archive tier. `None` when the data can be read.
pub(crate) fn unrestored_archive(head: &HeadObjectOutput) -> Option<String> {
    let class = head.storage_class().map(SdkStorageClass::as_str);
    let tier = match head.archive_status() {
        Some(ArchiveStatus::ArchiveAccess) => Some("the Intelligent-Tiering archive tier"),
        Some(ArchiveStatus::DeepArchiveAccess) => Some("the Intelligent-Tiering deep archive tier"),
        _ => None,
    };
    let name = match (class, tier) {
        (Some(class @ ("GLACIER" | "DEEP_ARCHIVE")), _) => class.to_owned(),
        (_, Some(tier)) => tier.to_owned(),
        _ => return None,
    };
    let restored = head
        .restore()
        .and_then(parse_restore)
        .is_some_and(|restore| !restore.in_progress);
    (!restored).then_some(name)
}

/// The class a HEAD response reports; S3 omits it for `STANDARD`.
pub(crate) fn class_name(head: &HeadObjectOutput) -> String {
    head.storage_class()
        .map_or("STANDARD", SdkStorageClass::as_str)
        .to_owned()
}

impl S3 {
    /// Changes the storage class of each object in `keys` with a server-side
    /// copy onto itself that keeps content headers, user metadata, tags and
    /// encryption (see the [module documentation](super)).
    ///
    /// An object already in `class` is reported unchanged and nothing is
    /// sent for it (S3 refuses a copy that changes nothing). An archived
    /// object (`GLACIER`, `DEEP_ARCHIVE`, or an Intelligent-Tiering archive
    /// tier) that has not been restored is a `conflict` for that key: its
    /// data cannot be read, so it cannot be copied. Objects larger than
    /// 5 GiB are copied in parts. For an object encrypted with SSE-C pass its
    /// key as `sse_c`; the copy keeps that key.
    pub async fn set_object_storage_class(
        &self,
        bucket: &str,
        keys: &[String],
        class: StorageClass,
        sse_c: Option<&CustomerKey>,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        run_batch(keys, options, |key| async move {
            self.edit_object(
                bucket,
                &key,
                "storage class",
                sse_c,
                options.dry_run,
                |head| {
                    let current = class_name(head);
                    if current == class.as_str() {
                        return Ok(Plan::unchanged(Some(current)));
                    }
                    Ok(Plan::change(
                        Some(current),
                        Edit {
                            storage_class: Some(class.to_sdk()),
                            ..Edit::default()
                        },
                    ))
                },
            )
            .await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use serde_json::json;

    #[test]
    fn the_plans_classes_round_trip_through_text_and_json() {
        let names = [
            "STANDARD",
            "STANDARD_IA",
            "ONEZONE_IA",
            "GLACIER_IR",
            "GLACIER",
            "DEEP_ARCHIVE",
            "INTELLIGENT_TIERING",
            "REDUCED_REDUNDANCY",
        ];
        assert_eq!(
            StorageClass::ALL.map(StorageClass::as_str),
            names,
            "the order of the plan"
        );
        for (class, name) in StorageClass::ALL.into_iter().zip(names) {
            assert_eq!(name.parse::<StorageClass>().unwrap(), class);
            assert_eq!(class.to_string(), name);
            assert_eq!(serde_json::to_value(class).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<StorageClass>(json!(name)).unwrap(),
                class
            );
            assert_eq!(class.to_sdk().as_str(), name);
        }
        assert_eq!(
            "standard-ia".parse::<StorageClass>().unwrap(),
            StorageClass::StandardIa
        );
        assert_eq!(
            " glacier_ir ".parse::<StorageClass>().unwrap(),
            StorageClass::GlacierIr
        );
    }

    #[test]
    fn classes_outside_the_list_are_usage_errors_naming_the_valid_ones() {
        for bad in [
            "",
            "COLD",
            "OUTPOSTS",
            "EXPRESS_ONEZONE",
            "SNOW",
            "STANDARD IA",
        ] {
            let err = bad.parse::<StorageClass>().unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{bad}");
            assert!(err.to_string().contains("DEEP_ARCHIVE"), "{err}");
        }
        assert!(serde_json::from_value::<StorageClass>(json!("OUTPOSTS")).is_err());
    }

    #[test]
    fn restore_headers_are_parsed() {
        let done = parse_restore(
            r#"ongoing-request="false", expiry-date="Fri, 21 Dec 2012 00:00:00 GMT""#,
        )
        .unwrap();
        assert!(!done.in_progress);
        assert_eq!(done.expires.unwrap().timestamp(), 1_356_048_000);
        let running = parse_restore(r#"ongoing-request="true""#).unwrap();
        assert!(running.in_progress && running.expires.is_none());
        assert_eq!(parse_restore("something else"), None);
        assert_eq!(parse_restore(r#"ongoing-request="#), None);
    }

    #[test]
    fn archived_objects_need_a_finished_restore() {
        let head = |class: SdkStorageClass, restore: Option<&str>| {
            HeadObjectOutput::builder()
                .storage_class(class)
                .set_restore(restore.map(str::to_owned))
                .build()
        };
        assert_eq!(
            unrestored_archive(&head(SdkStorageClass::Glacier, None)).as_deref(),
            Some("GLACIER")
        );
        assert_eq!(
            unrestored_archive(&head(
                SdkStorageClass::DeepArchive,
                Some(r#"ongoing-request="true""#)
            ))
            .as_deref(),
            Some("DEEP_ARCHIVE")
        );
        let restored = r#"ongoing-request="false", expiry-date="Fri, 21 Dec 2012 00:00:00 GMT""#;
        assert_eq!(
            unrestored_archive(&head(SdkStorageClass::Glacier, Some(restored))),
            None
        );
        // Instant retrieval and the other classes can always be read.
        for class in [
            SdkStorageClass::GlacierIr,
            SdkStorageClass::Standard,
            SdkStorageClass::IntelligentTiering,
        ] {
            assert_eq!(unrestored_archive(&head(class, None)), None);
        }
        let tiered = HeadObjectOutput::builder()
            .storage_class(SdkStorageClass::IntelligentTiering)
            .archive_status(ArchiveStatus::DeepArchiveAccess)
            .build();
        assert!(
            unrestored_archive(&tiered)
                .unwrap()
                .contains("Intelligent-Tiering")
        );
        assert_eq!(
            unrestored_archive(&HeadObjectOutput::builder().build()),
            None
        );
    }

    #[test]
    fn standard_is_reported_when_s3_omits_the_class() {
        assert_eq!(class_name(&HeadObjectOutput::builder().build()), "STANDARD");
        let ia = HeadObjectOutput::builder()
            .storage_class(SdkStorageClass::StandardIa)
            .build();
        assert_eq!(class_name(&ia), "STANDARD_IA");
    }
}
