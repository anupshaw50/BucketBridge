//! Buckets.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Badge shown next to a bucket in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketKind {
    /// A bucket on AWS S3.
    #[default]
    Aws,
    /// A bucket on an S3-compatible provider reached through a custom endpoint.
    External,
    /// A bucket used as a CloudFront origin.
    #[serde(rename = "cloudfront")]
    CloudFront,
}

/// A bucket as listed by `ListBuckets`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Bucket {
    /// Bucket name.
    pub name: String,
    /// Region reported by the server, when it reports one.
    pub region: Option<String>,
    /// Creation time.
    pub creation_date: Option<DateTime<Utc>>,
    /// Owner display name or canonical ID, when reported.
    pub owner: Option<String>,
    /// Badge for the UI.
    pub kind: BucketKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn kind_spellings() {
        assert_eq!(serde_json::to_value(BucketKind::Aws).unwrap(), json!("aws"));
        assert_eq!(
            serde_json::to_value(BucketKind::External).unwrap(),
            json!("external")
        );
        assert_eq!(
            serde_json::to_value(BucketKind::CloudFront).unwrap(),
            json!("cloudfront")
        );
        let kind: BucketKind = serde_json::from_value(json!("cloudfront")).unwrap();
        assert_eq!(kind, BucketKind::CloudFront);
        assert!(serde_json::from_value::<BucketKind>(json!("cloud_front")).is_err());
    }

    #[test]
    fn json_shape_matches_contract() {
        let bucket = Bucket {
            name: "photos".into(),
            region: Some("eu-central-1".into()),
            creation_date: Some(Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap()),
            owner: None,
            kind: BucketKind::External,
        };
        let value = serde_json::to_value(&bucket).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "photos",
                "region": "eu-central-1",
                "creation_date": "2026-10-29T08:15:00Z",
                "owner": null,
                "kind": "external"
            })
        );
        assert_eq!(serde_json::from_value::<Bucket>(value).unwrap(), bucket);
    }
}
