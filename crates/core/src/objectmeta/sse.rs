//! Server-side encryption: SSE-S3, SSE-KMS and customer-provided keys (SSE-C).
//!
//! An SSE-C key is supplied with every request that needs it and is never
//! stored, logged or serialized: [`CustomerKey`] zeroes its memory when it is
//! dropped, prints as `<redacted>` in `Debug` output and has no `Serialize`
//! implementation. The copies of the key that the AWS SDK makes for the
//! request headers are outside this crate's control and cannot be zeroed.

use std::fmt;

use aws_sdk_s3::types::ServerSideEncryption;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use md5::{Digest, Md5};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::inplace::{Edit, Plan};
use super::{BatchOptions, ObjectChange, run_batch};
use crate::error::{Error, Result};
use crate::model::Sse;
use crate::ops::{S3, object_head};
use crate::s3uri::validate_bucket_name;

/// Length of an SSE-C key in bytes (AES-256).
pub const CUSTOMER_KEY_LEN: usize = 32;

/// The only algorithm SSE-C supports; the value of
/// `x-amz-server-side-encryption-customer-algorithm`.
pub(crate) const SSE_C_ALGORITHM: &str = "AES256";

/// Longest KMS key identifier accepted (an ARN is at most 2048 characters).
const MAX_KMS_KEY_ID_LEN: usize = 2048;

/// A customer-provided encryption key for SSE-C: 32 random bytes.
///
/// Build it with [`CustomerKey::parse`] (text) or
/// [`CustomerKey::from_file_contents`] (the contents of a key file).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct CustomerKey([u8; CUSTOMER_KEY_LEN]);

impl CustomerKey {
    /// A key from exactly [`CUSTOMER_KEY_LEN`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let key: [u8; CUSTOMER_KEY_LEN] = bytes.try_into().map_err(|_| bad_key())?;
        Ok(CustomerKey(key))
    }

    /// Parses a key written as 64 hexadecimal digits or as the base64 of
    /// 32 bytes (44 characters, the padding is optional). Surrounding white
    /// space is ignored. Errors never repeat the input.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.len() == CUSTOMER_KEY_LEN * 2 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let mut bytes = Zeroizing::new([0u8; CUSTOMER_KEY_LEN]);
            let (pairs, _) = text.as_bytes().as_chunks::<2>();
            for (target, [high, low]) in bytes.iter_mut().zip(pairs) {
                *target = (hex_value(*high) << 4) | hex_value(*low);
            }
            return Self::from_bytes(bytes.as_slice());
        }
        let decoded = STANDARD
            .decode(text)
            .or_else(|_| STANDARD_NO_PAD.decode(text))
            .map(Zeroizing::new)
            .map_err(|_| bad_key())?;
        Self::from_bytes(decoded.as_slice())
    }

    /// A key from the contents of a key file: exactly 32 raw bytes (as made
    /// by `openssl rand 32`), otherwise text in one of the formats of
    /// [`CustomerKey::parse`].
    pub fn from_file_contents(contents: &[u8]) -> Result<Self> {
        if contents.len() == CUSTOMER_KEY_LEN {
            return Self::from_bytes(contents);
        }
        let text = std::str::from_utf8(contents).map_err(|_| bad_key())?;
        Self::parse(text)
    }

    /// The value of the `x-amz-server-side-encryption-customer-key` header:
    /// the key in base64. It is the key itself, so it is zeroed on drop.
    pub(crate) fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(STANDARD.encode(self.0))
    }

    /// The value of the `x-amz-server-side-encryption-customer-key-MD5`
    /// header: the base64 of the MD5 digest of the key. S3 reports the same
    /// value for objects stored with this key, so it identifies a key without
    /// revealing it.
    pub fn md5_base64(&self) -> String {
        STANDARD.encode(Md5::digest(self.0))
    }
}

impl fmt::Debug for CustomerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CustomerKey(<redacted>)")
    }
}

fn bad_key() -> Error {
    Error::Usage(format!(
        "the SSE-C key must be {CUSTOMER_KEY_LEN} bytes, written as {} hexadecimal digits or as base64 ({} characters), or stored as {CUSTOMER_KEY_LEN} raw bytes in a file",
        CUSTOMER_KEY_LEN * 2,
        CUSTOMER_KEY_LEN.div_ceil(3) * 4
    ))
}

/// Value of an ASCII hexadecimal digit (the caller checked it is one).
fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => digit.wrapping_sub(b'A').wrapping_add(10),
    }
}

/// The server-side encryption an object is changed to.
///
/// S3 cannot remove encryption, so there is no "none": every object is at
/// least SSE-S3 encrypted at rest. There is deliberately no `Serialize`
/// implementation: the SSE-C key must never be written anywhere. Use
/// [`EncryptionChange::to_sse`] for a printable description.
#[derive(Debug, Clone)]
pub enum EncryptionChange {
    /// SSE-S3: keys managed by S3 (`x-amz-server-side-encryption: AES256`).
    S3,
    /// SSE-KMS (`x-amz-server-side-encryption: aws:kms`).
    Kms {
        /// KMS key ID, key ARN, alias name or alias ARN; `None` uses the
        /// AWS managed key `aws/s3`.
        key_id: Option<String>,
        /// Whether an S3 Bucket Key reduces the calls to KMS.
        bucket_key: bool,
    },
    /// SSE-C: a key supplied with this request.
    Customer(CustomerKey),
}

impl EncryptionChange {
    /// Checks the parts that can be checked without asking S3.
    pub fn validate(&self) -> Result<()> {
        if let EncryptionChange::Kms {
            key_id: Some(key_id),
            ..
        } = self
        {
            if key_id.trim().is_empty() {
                return Err(Error::Usage(
                    "the KMS key is empty; give a key ID, ARN or alias, or leave it out to use the AWS managed key".into(),
                ));
            }
            if key_id.len() > MAX_KMS_KEY_ID_LEN
                || key_id.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(Error::Usage(
                    "the KMS key must be a key ID, key ARN, alias name or alias ARN without spaces"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// The encryption as [`Sse`] describes it (an SSE-C key becomes its MD5).
    pub fn to_sse(&self) -> Sse {
        match self {
            EncryptionChange::S3 => Sse::S3,
            EncryptionChange::Kms { key_id, bucket_key } => Sse::Kms {
                key_id: key_id.clone(),
                bucket_key: *bucket_key,
            },
            EncryptionChange::Customer(key) => Sse::Customer {
                key_md5: key.md5_base64(),
            },
        }
    }

    /// Whether `current` (what HEAD reported) is already this encryption.
    pub(crate) fn matches(&self, current: &Sse) -> bool {
        match (self, current) {
            (EncryptionChange::S3, Sse::S3) => true,
            (
                EncryptionChange::Kms { key_id, bucket_key },
                Sse::Kms {
                    key_id: current_id,
                    bucket_key: current_bucket_key,
                },
            ) => bucket_key == current_bucket_key && key_id.is_some() && key_id == current_id,
            (EncryptionChange::Customer(key), Sse::Customer { key_md5 }) => {
                key.md5_base64() == *key_md5
            }
            _ => false,
        }
    }
}

/// A short name of an encryption for messages and the `previous` field of
/// results: `none`, `s3`, `kms` or `c`.
pub(crate) fn sse_name(sse: &Sse) -> &'static str {
    match sse {
        Sse::None => "none",
        Sse::S3 => "s3",
        Sse::Kms { .. } => "kms",
        Sse::Customer { .. } => "c",
    }
}

/// The encryption headers of the object a request creates.
///
/// All `None` sends nothing, so that the bucket's default encryption applies.
#[derive(Debug, Clone, Default)]
pub(crate) struct DestEncryption {
    /// `x-amz-server-side-encryption`.
    pub sse: Option<ServerSideEncryption>,
    /// `x-amz-server-side-encryption-aws-kms-key-id`.
    pub kms_key_id: Option<String>,
    /// `x-amz-server-side-encryption-bucket-key-enabled`.
    pub bucket_key: Option<bool>,
    /// The three SSE-C headers.
    pub customer_key: Option<CustomerKey>,
}

impl DestEncryption {
    /// The encryption headers for a change.
    pub(crate) fn from_change(change: &EncryptionChange) -> Self {
        match change {
            EncryptionChange::S3 => DestEncryption {
                sse: Some(ServerSideEncryption::Aes256),
                ..DestEncryption::default()
            },
            EncryptionChange::Kms { key_id, bucket_key } => DestEncryption {
                sse: Some(ServerSideEncryption::AwsKms),
                kms_key_id: key_id.clone(),
                bucket_key: Some(*bucket_key),
                customer_key: None,
            },
            EncryptionChange::Customer(key) => DestEncryption {
                customer_key: Some(key.clone()),
                ..DestEncryption::default()
            },
        }
    }
}

impl S3 {
    /// Changes the server-side encryption of each object in `keys` with a
    /// server-side copy onto itself that keeps content headers, user
    /// metadata, tags and the storage class (see the [module
    /// documentation](super)).
    ///
    /// For objects that are encrypted with SSE-C now, pass their key as
    /// `source_key`; every object of one call must then be encrypted with
    /// that key (mix nothing: S3 rejects a key for an object that is not
    /// SSE-C). To move an object from one customer key to another, give the old
    /// key as `source_key` and [`EncryptionChange::Customer`] with the new one.
    /// An object that already has the wanted encryption (for SSE-KMS: the same
    /// key ID and bucket key setting) is reported unchanged. A wrong
    /// `source_key` is refused by S3 and reported as that object's error.
    pub async fn set_object_encryption(
        &self,
        bucket: &str,
        keys: &[String],
        target: &EncryptionChange,
        source_key: Option<&CustomerKey>,
        options: BatchOptions,
    ) -> Result<Vec<ObjectChange>> {
        validate_bucket_name(bucket)?;
        target.validate()?;
        run_batch(keys, options, |key| async move {
            self.edit_object(
                bucket,
                &key,
                "encryption",
                source_key,
                options.dry_run,
                |head| {
                    let current = object_head(&key, head).meta.sse;
                    let previous = sse_name(&current).to_owned();
                    if target.matches(&current) {
                        return Ok(Plan::unchanged(Some(previous)));
                    }
                    Ok(Plan::change(
                        Some(previous),
                        Edit {
                            encryption: Some(DestEncryption::from_change(target)),
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

    /// The bytes 0x00 to 0x1f.
    fn sample() -> [u8; CUSTOMER_KEY_LEN] {
        std::array::from_fn(|index| u8::try_from(index).unwrap())
    }

    const SAMPLE_BASE64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const SAMPLE_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const SAMPLE_MD5: &str = "tP/LI3N87DFaSk0aoqYgzg==";

    #[test]
    fn keys_parse_from_hex_base64_and_raw_bytes() {
        let expected = CustomerKey::from_bytes(&sample()).unwrap();
        for text in [
            SAMPLE_HEX,
            &SAMPLE_HEX.to_uppercase(),
            SAMPLE_BASE64,
            SAMPLE_BASE64.trim_end_matches('='),
            &format!("  {SAMPLE_BASE64}\n"),
        ] {
            let key = CustomerKey::parse(text).unwrap();
            assert_eq!(key.0, expected.0, "{text}");
        }
        assert_eq!(
            CustomerKey::from_file_contents(&sample()).unwrap().0,
            expected.0
        );
        assert_eq!(
            CustomerKey::from_file_contents(format!("{SAMPLE_HEX}\n").as_bytes())
                .unwrap()
                .0,
            expected.0
        );
        assert_eq!(
            CustomerKey::from_file_contents(SAMPLE_BASE64.as_bytes())
                .unwrap()
                .0,
            expected.0
        );
    }

    #[test]
    fn keys_of_the_wrong_size_or_format_are_rejected_without_echoing_them() {
        for bad in [
            "",
            "secret",
            "0123456789abcdef",
            // 31 bytes of base64.
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg==",
            // 63 hex digits and a stray letter.
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1g",
        ] {
            let err = CustomerKey::parse(bad).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{bad}");
            assert!(bad.is_empty() || !err.to_string().contains(bad), "{err}");
        }
        assert!(CustomerKey::from_bytes(&[0; 31]).is_err());
        assert!(CustomerKey::from_bytes(&[0; 33]).is_err());
        // 33 raw bytes that are not text.
        assert!(CustomerKey::from_file_contents(&[0xff; 33]).is_err());
    }

    #[test]
    fn header_values_match_the_s3_specification() {
        let key = CustomerKey::from_bytes(&sample()).unwrap();
        assert_eq!(key.to_base64().as_str(), SAMPLE_BASE64);
        assert_eq!(key.md5_base64(), SAMPLE_MD5);
        let other = CustomerKey::from_bytes(&[0xab; CUSTOMER_KEY_LEN]).unwrap();
        assert_eq!(other.md5_base64(), "6Z+00jTp9DFT5j+l/q0WFA==");
        assert_ne!(key.md5_base64(), other.md5_base64());
    }

    #[test]
    fn the_key_never_shows_in_debug_output_and_is_zeroed() {
        let mut key = CustomerKey::parse(SAMPLE_BASE64).unwrap();
        let change = EncryptionChange::Customer(key.clone());
        for text in [format!("{key:?}"), format!("{change:?}")] {
            assert!(text.contains("<redacted>"), "{text}");
            assert!(
                !text.contains(SAMPLE_BASE64) && !text.contains(SAMPLE_HEX),
                "{text}"
            );
            assert!(!text.contains("AAECAw"), "{text}");
        }
        key.zeroize();
        assert_eq!(key.0, [0; CUSTOMER_KEY_LEN]);
    }

    #[test]
    fn encryption_changes_describe_themselves_without_the_key() {
        let key = CustomerKey::from_bytes(&sample()).unwrap();
        assert_eq!(EncryptionChange::S3.to_sse(), Sse::S3);
        assert_eq!(
            EncryptionChange::Kms {
                key_id: Some("alias/data".into()),
                bucket_key: true
            }
            .to_sse(),
            Sse::Kms {
                key_id: Some("alias/data".into()),
                bucket_key: true
            }
        );
        assert_eq!(
            EncryptionChange::Customer(key).to_sse(),
            Sse::Customer {
                key_md5: SAMPLE_MD5.into()
            }
        );
    }

    #[test]
    fn an_object_already_in_the_wanted_state_is_recognised() {
        let kms = |id: Option<&str>, bucket_key| EncryptionChange::Kms {
            key_id: id.map(str::to_owned),
            bucket_key,
        };
        let current = Sse::Kms {
            key_id: Some("arn:aws:kms:eu-west-1:000000000000:key/abc".into()),
            bucket_key: true,
        };
        assert!(kms(Some("arn:aws:kms:eu-west-1:000000000000:key/abc"), true).matches(&current));
        assert!(!kms(Some("arn:aws:kms:eu-west-1:000000000000:key/abc"), false).matches(&current));
        assert!(!kms(Some("alias/other"), true).matches(&current));
        // Without a key ID the AWS managed key is meant, which the reported
        // key ARN cannot confirm: re-encrypt rather than guess.
        assert!(!kms(None, true).matches(&current));
        assert!(EncryptionChange::S3.matches(&Sse::S3));
        assert!(!EncryptionChange::S3.matches(&Sse::None));
        assert!(!EncryptionChange::S3.matches(&current));
        let key = CustomerKey::from_bytes(&sample()).unwrap();
        let same = Sse::Customer {
            key_md5: SAMPLE_MD5.into(),
        };
        assert!(EncryptionChange::Customer(key.clone()).matches(&same));
        let other = Sse::Customer {
            key_md5: "other==".into(),
        };
        assert!(!EncryptionChange::Customer(key).matches(&other));
    }

    #[test]
    fn kms_keys_are_validated() {
        let kms = |id: &str| EncryptionChange::Kms {
            key_id: Some(id.to_owned()),
            bucket_key: false,
        };
        assert!(kms("alias/data").validate().is_ok());
        assert!(
            kms("1234abcd-12ab-34cd-56ef-1234567890ab")
                .validate()
                .is_ok()
        );
        assert!(
            EncryptionChange::Kms {
                key_id: None,
                bucket_key: true
            }
            .validate()
            .is_ok()
        );
        for bad in ["", "  ", "two words", "tab\tkey", &"k".repeat(2049)] {
            assert_eq!(kms(bad).validate().unwrap_err().kind(), ErrorKind::Usage);
        }
        assert!(EncryptionChange::S3.validate().is_ok());
    }

    #[test]
    fn encryption_names() {
        assert_eq!(sse_name(&Sse::None), "none");
        assert_eq!(sse_name(&Sse::S3), "s3");
        assert_eq!(
            sse_name(&Sse::Kms {
                key_id: None,
                bucket_key: false
            }),
            "kms"
        );
        assert_eq!(
            sse_name(&Sse::Customer {
                key_md5: "x".into()
            }),
            "c"
        );
    }

    #[test]
    fn destination_headers_follow_the_change() {
        let s3 = DestEncryption::from_change(&EncryptionChange::S3);
        assert_eq!(s3.sse, Some(ServerSideEncryption::Aes256));
        assert!(s3.kms_key_id.is_none() && s3.bucket_key.is_none() && s3.customer_key.is_none());
        let kms = DestEncryption::from_change(&EncryptionChange::Kms {
            key_id: Some("alias/k".into()),
            bucket_key: true,
        });
        assert_eq!(kms.sse, Some(ServerSideEncryption::AwsKms));
        assert_eq!(kms.kms_key_id.as_deref(), Some("alias/k"));
        assert_eq!(kms.bucket_key, Some(true));
        let key = CustomerKey::from_bytes(&sample()).unwrap();
        let customer = DestEncryption::from_change(&EncryptionChange::Customer(key));
        assert!(customer.sse.is_none() && customer.customer_key.is_some());
    }
}
