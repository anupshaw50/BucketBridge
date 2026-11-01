//! Checksums in the form S3 uses: the base64 encoding of the big-endian
//! digest, as in the `x-amz-checksum-*` headers.
//!
//! The implementations are the AWS SDK's own (`aws-smithy-checksums`), so a
//! value computed here matches what the SDK and S3 compute.

use std::fmt;

use aws_smithy_checksums::ChecksumAlgorithm as Implementation;
use aws_smithy_checksums::http::HttpChecksum;
use serde::{Deserialize, Serialize};

use crate::model::Checksums;
use crate::settings::ChecksumAlgorithm;

/// Incremental checksum computation for one algorithm.
pub struct Checksummer {
    algorithm: ChecksumAlgorithm,
    inner: Box<dyn HttpChecksum>,
}

impl fmt::Debug for Checksummer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checksummer")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

impl Checksummer {
    /// A checksummer for `algorithm`; `None` for [`ChecksumAlgorithm::None`].
    pub fn new(algorithm: ChecksumAlgorithm) -> Option<Self> {
        let implementation = match algorithm {
            ChecksumAlgorithm::Crc32 => Implementation::Crc32,
            ChecksumAlgorithm::Crc32c => Implementation::Crc32c,
            ChecksumAlgorithm::Crc64nvme => Implementation::Crc64Nvme,
            ChecksumAlgorithm::Sha1 => Implementation::Sha1,
            ChecksumAlgorithm::Sha256 => Implementation::Sha256,
            ChecksumAlgorithm::None => return None,
        };
        Some(Checksummer {
            algorithm,
            inner: implementation.into_impl(),
        })
    }

    /// The algorithm.
    pub fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    /// Feeds more data.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// The base64 value of everything fed so far.
    pub fn finish(self) -> String {
        aws_smithy_types::base64::encode(&self.inner.finalize()[..])
    }
}

/// The checksum of `data`; `None` for [`ChecksumAlgorithm::None`].
pub fn checksum_of(algorithm: ChecksumAlgorithm, data: &[u8]) -> Option<String> {
    let mut checksummer = Checksummer::new(algorithm)?;
    checksummer.update(data);
    Some(checksummer.finish())
}

/// The value for `algorithm` in a set of checksums.
pub fn value_of(checksums: &Checksums, algorithm: ChecksumAlgorithm) -> Option<&str> {
    match algorithm {
        ChecksumAlgorithm::Crc32 => checksums.crc32.as_deref(),
        ChecksumAlgorithm::Crc32c => checksums.crc32c.as_deref(),
        ChecksumAlgorithm::Crc64nvme => checksums.crc64nvme.as_deref(),
        ChecksumAlgorithm::Sha1 => checksums.sha1.as_deref(),
        ChecksumAlgorithm::Sha256 => checksums.sha256.as_deref(),
        ChecksumAlgorithm::None => None,
    }
}

/// True when a checksum value covers the object as a whole. S3 reports the
/// composite checksum of a multipart object (a checksum of the part
/// checksums) with a `-N` suffix, where N is the number of parts; such a
/// value cannot be compared with a checksum of the content.
pub fn is_full_object(value: &str) -> bool {
    match value.rsplit_once('-') {
        Some((_, parts)) => parts.is_empty() || !parts.bytes().all(|b| b.is_ascii_digit()),
        None => true,
    }
}

/// A full-object checksum value of a stored object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectChecksum {
    /// The algorithm.
    pub algorithm: ChecksumAlgorithm,
    /// Base64 value.
    pub value: String,
}

/// The settings spelling of an algorithm (`crc32`, `crc32c`, `crc64nvme`,
/// `sha1`, `sha256`, `none`).
pub fn name(algorithm: ChecksumAlgorithm) -> &'static str {
    match algorithm {
        ChecksumAlgorithm::Crc32 => "crc32",
        ChecksumAlgorithm::Crc32c => "crc32c",
        ChecksumAlgorithm::Crc64nvme => "crc64nvme",
        ChecksumAlgorithm::Sha1 => "sha1",
        ChecksumAlgorithm::Sha256 => "sha256",
        ChecksumAlgorithm::None => "none",
    }
}

/// The SDK's name for an algorithm; `None` for [`ChecksumAlgorithm::None`].
pub(crate) fn sdk_algorithm(
    algorithm: ChecksumAlgorithm,
) -> Option<aws_sdk_s3::types::ChecksumAlgorithm> {
    use aws_sdk_s3::types::ChecksumAlgorithm as Sdk;
    match algorithm {
        ChecksumAlgorithm::Crc32 => Some(Sdk::Crc32),
        ChecksumAlgorithm::Crc32c => Some(Sdk::Crc32C),
        ChecksumAlgorithm::Crc64nvme => Some(Sdk::Crc64Nvme),
        ChecksumAlgorithm::Sha1 => Some(Sdk::Sha1),
        ChecksumAlgorithm::Sha256 => Some(Sdk::Sha256),
        ChecksumAlgorithm::None => None,
    }
}

/// Every algorithm that produces a value, fastest first.
pub(crate) const ALGORITHMS: [ChecksumAlgorithm; 5] = [
    ChecksumAlgorithm::Crc64nvme,
    ChecksumAlgorithm::Crc32c,
    ChecksumAlgorithm::Crc32,
    ChecksumAlgorithm::Sha1,
    ChecksumAlgorithm::Sha256,
];

/// Sets the checksum value field for an algorithm on an SDK builder (the
/// builders share method names but no trait).
macro_rules! set_checksum {
    ($builder:expr, $algorithm:expr, $value:expr) => {{
        let value: Option<String> = $value;
        match $algorithm {
            $crate::settings::ChecksumAlgorithm::Crc32 => $builder.set_checksum_crc32(value),
            $crate::settings::ChecksumAlgorithm::Crc32c => $builder.set_checksum_crc32_c(value),
            $crate::settings::ChecksumAlgorithm::Crc64nvme => {
                $builder.set_checksum_crc64_nvme(value)
            }
            $crate::settings::ChecksumAlgorithm::Sha1 => $builder.set_checksum_sha1(value),
            $crate::settings::ChecksumAlgorithm::Sha256 => $builder.set_checksum_sha256(value),
            $crate::settings::ChecksumAlgorithm::None => $builder,
        }
    }};
}

/// Reads the checksum value field for an algorithm from an SDK output type.
macro_rules! checksum_field {
    ($output:expr, $algorithm:expr) => {
        match $algorithm {
            $crate::settings::ChecksumAlgorithm::Crc32 => $output.checksum_crc32(),
            $crate::settings::ChecksumAlgorithm::Crc32c => $output.checksum_crc32_c(),
            $crate::settings::ChecksumAlgorithm::Crc64nvme => $output.checksum_crc64_nvme(),
            $crate::settings::ChecksumAlgorithm::Sha1 => $output.checksum_sha1(),
            $crate::settings::ChecksumAlgorithm::Sha256 => $output.checksum_sha256(),
            $crate::settings::ChecksumAlgorithm::None => None,
        }
    };
}

pub(crate) use {checksum_field, set_checksum};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        let data = b"hello world";
        // The value the server stores for the SDK's default CRC32 of this text.
        assert_eq!(
            checksum_of(ChecksumAlgorithm::Crc32, data).as_deref(),
            Some("DUoRhQ==")
        );
        assert_eq!(
            checksum_of(ChecksumAlgorithm::Sha256, b"").as_deref(),
            Some("47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=")
        );
        assert_eq!(
            checksum_of(ChecksumAlgorithm::Sha1, b"").as_deref(),
            Some("2jmj7l5rSw0yVb/vlWAYkK/YBwk=")
        );
        assert_eq!(
            checksum_of(ChecksumAlgorithm::Crc32, b"").as_deref(),
            Some("AAAAAA==")
        );
        assert_eq!(checksum_of(ChecksumAlgorithm::None, data), None);
        for algorithm in ALGORITHMS {
            let value = checksum_of(algorithm, data).unwrap();
            assert!(is_full_object(&value), "{algorithm:?}: {value}");
            assert_eq!(Checksummer::new(algorithm).unwrap().algorithm(), algorithm);
        }
    }

    #[test]
    fn incremental_equals_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        for algorithm in ALGORITHMS {
            let mut checksummer = Checksummer::new(algorithm).unwrap();
            for chunk in data.chunks(777) {
                checksummer.update(chunk);
            }
            assert_eq!(
                Some(checksummer.finish()),
                checksum_of(algorithm, &data),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn composite_values_are_recognised() {
        assert!(is_full_object("DUoRhQ=="));
        assert!(is_full_object("ab+/cd=="));
        assert!(!is_full_object("DUoRhQ==-3"));
        assert!(!is_full_object("x-10000"));
        assert!(is_full_object("x-"));
        assert!(is_full_object("x-3a"));
    }

    #[test]
    fn values_are_picked_by_algorithm() {
        let checksums = Checksums {
            crc32: Some("a".into()),
            crc32c: Some("b".into()),
            crc64nvme: Some("c".into()),
            sha1: Some("d".into()),
            sha256: Some("e".into()),
        };
        let picked: Vec<_> = ALGORITHMS
            .iter()
            .map(|a| value_of(&checksums, *a).unwrap())
            .collect();
        assert_eq!(picked, ["c", "b", "a", "d", "e"]);
        assert_eq!(value_of(&checksums, ChecksumAlgorithm::None), None);
        assert_eq!(sdk_algorithm(ChecksumAlgorithm::None), None);
        for algorithm in ALGORITHMS.into_iter().chain([ChecksumAlgorithm::None]) {
            assert_eq!(
                serde_json::to_value(algorithm).unwrap(),
                serde_json::json!(name(algorithm))
            );
        }
        assert_eq!(
            sdk_algorithm(ChecksumAlgorithm::Crc32c),
            Some(aws_sdk_s3::types::ChecksumAlgorithm::Crc32C)
        );
    }
}
