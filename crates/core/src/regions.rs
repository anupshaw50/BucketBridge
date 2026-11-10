//! The single table of AWS regions and partitions.
//!
//! Everything that needs to know which regions exist, or which partition a
//! region belongs to, asks this module: the account form and the bucket
//! creation dialog (through the `list_regions` desktop command), ARN
//! construction ([`Partition::bucket_arn`]) and endpoint names
//! ([`Partition::dns_suffix`], [`s3_endpoint`]).
//!
//! A partition decides the ARN prefix and the DNS suffix of the endpoints:
//! `aws` (`amazonaws.com`), `aws-cn` (`amazonaws.com.cn`, the China
//! regions), `aws-us-gov` (`amazonaws.com`, GovCloud) and `aws-eusc` (the
//! European Sovereign Cloud, `amazonaws.eu`). The SDK resolves its own
//! endpoints from the region name; this table is for what BucketBridge
//! shows and builds itself.
//!
//! Regions that are not in the table (a new AWS region, or the made-up names
//! S3-compatible providers use) are classified by their name with
//! [`partition_of`], the same way the SDK's partition rules do.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An AWS partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Partition {
    /// Standard commercial regions.
    #[serde(rename = "aws")]
    Aws,
    /// The China regions (Beijing and Ningxia).
    #[serde(rename = "aws-cn")]
    AwsCn,
    /// AWS GovCloud (US).
    #[serde(rename = "aws-us-gov")]
    AwsUsGov,
    /// The AWS European Sovereign Cloud.
    #[serde(rename = "aws-eusc")]
    AwsEusc,
}

impl Partition {
    /// The partition name as used in ARNs.
    pub fn id(self) -> &'static str {
        match self {
            Partition::Aws => "aws",
            Partition::AwsCn => "aws-cn",
            Partition::AwsUsGov => "aws-us-gov",
            Partition::AwsEusc => "aws-eusc",
        }
    }

    /// The DNS suffix of the partition's service endpoints.
    pub fn dns_suffix(self) -> &'static str {
        match self {
            Partition::Aws | Partition::AwsUsGov => "amazonaws.com",
            Partition::AwsCn => "amazonaws.com.cn",
            Partition::AwsEusc => "amazonaws.eu",
        }
    }

    /// ARN of a bucket, `arn:<partition>:s3:::<bucket>`.
    pub fn bucket_arn(self, bucket: &str) -> String {
        format!("arn:{}:s3:::{bucket}", self.id())
    }

    /// ARN of an object or of every object below a prefix.
    pub fn object_arn(self, bucket: &str, key: &str) -> String {
        format!("arn:{}:s3:::{bucket}/{key}", self.id())
    }
}

impl fmt::Display for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// One region of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RegionInfo {
    /// Region code, for example `eu-west-1`.
    pub id: &'static str,
    /// Display name, for example `Europe (Ireland)`.
    pub name: &'static str,
    /// The partition the region belongs to.
    pub partition: Partition,
    /// `true` for a region that is off until it is enabled for the AWS
    /// account (opt-in).
    pub opt_in: bool,
}

const fn region(
    id: &'static str,
    name: &'static str,
    partition: Partition,
    opt_in: bool,
) -> RegionInfo {
    RegionInfo {
        id,
        name,
        partition,
        opt_in,
    }
}

const A: Partition = Partition::Aws;

/// Every region BucketBridge knows, commercial regions first.
pub const REGIONS: &[RegionInfo] = &[
    region("us-east-1", "US East (N. Virginia)", A, false),
    region("us-east-2", "US East (Ohio)", A, false),
    region("us-west-1", "US West (N. California)", A, false),
    region("us-west-2", "US West (Oregon)", A, false),
    region("af-south-1", "Africa (Cape Town)", A, true),
    region("ap-east-1", "Asia Pacific (Hong Kong)", A, true),
    region("ap-east-2", "Asia Pacific (Taipei)", A, true),
    region("ap-south-1", "Asia Pacific (Mumbai)", A, false),
    region("ap-south-2", "Asia Pacific (Hyderabad)", A, true),
    region("ap-northeast-1", "Asia Pacific (Tokyo)", A, false),
    region("ap-northeast-2", "Asia Pacific (Seoul)", A, false),
    region("ap-northeast-3", "Asia Pacific (Osaka)", A, false),
    region("ap-southeast-1", "Asia Pacific (Singapore)", A, false),
    region("ap-southeast-2", "Asia Pacific (Sydney)", A, false),
    region("ap-southeast-3", "Asia Pacific (Jakarta)", A, true),
    region("ap-southeast-4", "Asia Pacific (Melbourne)", A, true),
    region("ap-southeast-5", "Asia Pacific (Malaysia)", A, true),
    region("ap-southeast-6", "Asia Pacific (New Zealand)", A, true),
    region("ap-southeast-7", "Asia Pacific (Thailand)", A, true),
    region("ca-central-1", "Canada (Central)", A, false),
    region("ca-west-1", "Canada West (Calgary)", A, true),
    region("eu-central-1", "Europe (Frankfurt)", A, false),
    region("eu-central-2", "Europe (Zurich)", A, true),
    region("eu-west-1", "Europe (Ireland)", A, false),
    region("eu-west-2", "Europe (London)", A, false),
    region("eu-west-3", "Europe (Paris)", A, false),
    region("eu-north-1", "Europe (Stockholm)", A, false),
    region("eu-south-1", "Europe (Milan)", A, true),
    region("eu-south-2", "Europe (Spain)", A, true),
    region("il-central-1", "Israel (Tel Aviv)", A, true),
    region("me-south-1", "Middle East (Bahrain)", A, true),
    region("me-central-1", "Middle East (UAE)", A, true),
    region("mx-central-1", "Mexico (Central)", A, true),
    region("sa-east-1", "South America (Sao Paulo)", A, false),
    region("cn-north-1", "China (Beijing)", Partition::AwsCn, false),
    region("cn-northwest-1", "China (Ningxia)", Partition::AwsCn, false),
    region(
        "us-gov-east-1",
        "AWS GovCloud (US-East)",
        Partition::AwsUsGov,
        false,
    ),
    region(
        "us-gov-west-1",
        "AWS GovCloud (US-West)",
        Partition::AwsUsGov,
        false,
    ),
    region(
        "eusc-de-east-1",
        "AWS European Sovereign Cloud (Germany)",
        Partition::AwsEusc,
        false,
    ),
];

/// The table entry of a region code.
pub fn find(region: &str) -> Option<&'static RegionInfo> {
    REGIONS.iter().find(|info| info.id == region)
}

/// `true` for `<prefix><word>-<digits>` such as `cn-north-1` (the prefix
/// includes its trailing dash).
fn shaped_like_region(region: &str, prefix: &str) -> bool {
    let Some(rest) = region.strip_prefix(prefix) else {
        return false;
    };
    let Some((word, number)) = rest.rsplit_once('-') else {
        return false;
    };
    !word.is_empty()
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// The partition of a region: from the table when the region is listed,
/// otherwise from its name (`cn-*`, `us-gov-*`, `eusc-*`; everything else
/// is `aws`, including the names of S3-compatible providers, where the
/// partition has no meaning).
pub fn partition_of(region: &str) -> Partition {
    if let Some(info) = find(region) {
        return info.partition;
    }
    if shaped_like_region(region, "cn-") {
        Partition::AwsCn
    } else if shaped_like_region(region, "us-gov-") {
        Partition::AwsUsGov
    } else if shaped_like_region(region, "eusc-") {
        Partition::AwsEusc
    } else {
        Partition::Aws
    }
}

/// The regional S3 endpoint URL of an AWS region (virtual-hosted style
/// buckets add the bucket name in front of the host).
pub fn s3_endpoint(region: &str) -> String {
    format!("https://s3.{region}.{}", partition_of(region).dns_suffix())
}

/// `true` when `host` is an AWS service host name (any partition): the host
/// is, or ends in `.` plus, one of the partition DNS suffixes. Used to
/// refuse settings that must never be applied to AWS itself.
pub fn is_aws_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    [
        "amazonaws.com",
        "amazonaws.com.cn",
        "amazonaws.eu",
        "api.aws",
        "on.aws",
    ]
    .iter()
    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// The table as sent by the `list_regions` command.
#[derive(Debug, Clone, Serialize)]
pub struct RegionEntry {
    /// Region code.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Partition id (`aws`, `aws-cn`, `aws-us-gov`, `aws-eusc`).
    pub partition: &'static str,
    /// DNS suffix of the partition's endpoints.
    pub dns_suffix: &'static str,
    /// `true` for an opt-in region.
    pub opt_in: bool,
}

/// Every known region, for display.
pub fn entries() -> Vec<RegionEntry> {
    REGIONS
        .iter()
        .map(|info| RegionEntry {
            id: info.id,
            name: info.name,
            partition: info.partition.id(),
            dns_suffix: info.partition.dns_suffix(),
            opt_in: info.opt_in,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_valid_region_names() {
        let mut seen = HashSet::new();
        for info in REGIONS {
            assert!(seen.insert(info.id), "duplicate {}", info.id);
            crate::client::validate_region(info.id).unwrap();
            assert_eq!(partition_of(info.id), info.partition);
        }
    }

    #[test]
    fn every_partition_has_regions() {
        for partition in [
            Partition::Aws,
            Partition::AwsCn,
            Partition::AwsUsGov,
            Partition::AwsEusc,
        ] {
            assert!(REGIONS.iter().any(|r| r.partition == partition));
        }
    }

    #[test]
    fn partitions_name_arns_and_suffixes() {
        assert_eq!(
            partition_of("cn-north-1").bucket_arn("b"),
            "arn:aws-cn:s3:::b"
        );
        assert_eq!(
            partition_of("us-gov-west-1").object_arn("b", "k/*"),
            "arn:aws-us-gov:s3:::b/k/*"
        );
        assert_eq!(partition_of("eu-west-1").bucket_arn("b"), "arn:aws:s3:::b");
        assert_eq!(
            partition_of("cn-northwest-1").dns_suffix(),
            "amazonaws.com.cn"
        );
        assert_eq!(partition_of("us-gov-east-1").dns_suffix(), "amazonaws.com");
        assert_eq!(partition_of("eusc-de-east-1").dns_suffix(), "amazonaws.eu");
    }

    #[test]
    fn unlisted_regions_are_classified_by_name() {
        assert_eq!(partition_of("cn-southwest-9"), Partition::AwsCn);
        assert_eq!(partition_of("us-gov-central-1"), Partition::AwsUsGov);
        assert_eq!(partition_of("eusc-fr-east-1"), Partition::AwsEusc);
        assert_eq!(partition_of("xx-new-1"), Partition::Aws);
        assert_eq!(partition_of("auto"), Partition::Aws);
        assert_eq!(partition_of("cn-"), Partition::Aws);
        assert_eq!(partition_of("garage"), Partition::Aws);
    }

    #[test]
    fn s3_endpoints_use_the_partition_suffix() {
        assert_eq!(
            s3_endpoint("us-east-1"),
            "https://s3.us-east-1.amazonaws.com"
        );
        assert_eq!(
            s3_endpoint("cn-north-1"),
            "https://s3.cn-north-1.amazonaws.com.cn"
        );
        assert_eq!(
            s3_endpoint("us-gov-west-1"),
            "https://s3.us-gov-west-1.amazonaws.com"
        );
    }

    #[test]
    fn aws_hosts_are_recognised_in_every_partition() {
        assert!(is_aws_host("s3.eu-west-1.amazonaws.com"));
        assert!(is_aws_host("S3.cn-north-1.amazonaws.com.cn"));
        assert!(is_aws_host("bucket.s3.amazonaws.eu."));
        assert!(is_aws_host("s3.dualstack.us-east-1.api.aws"));
        assert!(!is_aws_host("minio.example.com"));
        assert!(!is_aws_host("notamazonaws.com"));
        assert!(!is_aws_host("amazonaws.com.evil.example"));
    }

    /// The fake backend of the UI keeps a copy of the table; it must list the
    /// same regions with the same partitions.
    #[test]
    fn the_ui_mock_matches_the_table() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ui/src/mock/regionsMock.ts"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            return; // a packaged crate has no UI sources
        };
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("{ id: ")).collect();
        assert_eq!(lines.len(), REGIONS.len());
        for info in REGIONS {
            let line = lines
                .iter()
                .find(|l| l.contains(&format!("id: \"{}\"", info.id)))
                .unwrap_or_else(|| panic!("{} missing from the mock", info.id));
            assert!(
                line.contains(&format!("partition: \"{}\"", info.partition.id())),
                "{line}"
            );
            assert!(
                line.contains(&format!("dns_suffix: \"{}\"", info.partition.dns_suffix())),
                "{line}"
            );
            assert!(line.contains(&format!("opt_in: {}", info.opt_in)), "{line}");
        }
    }

    #[test]
    fn entries_mirror_the_table() {
        let entries = entries();
        assert_eq!(entries.len(), REGIONS.len());
        let cn = entries.iter().find(|e| e.id == "cn-north-1").unwrap();
        assert_eq!(cn.partition, "aws-cn");
        assert_eq!(cn.dns_suffix, "amazonaws.com.cn");
    }
}
