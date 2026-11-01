//! MFA delete: the `x-amz-mfa` header for changing the MFA-delete state of a
//! bucket and for deleting object versions for good.

use std::fmt;

use aws_sdk_s3::types::{BucketVersioningStatus, MfaDelete, VersioningConfiguration};

use crate::bucketcfg::VersioningStatus;
use crate::error::{Error, Result};
use crate::ops::S3;
use crate::s3uri::validate_bucket_name;

/// The serial of an MFA device and a current code from it. The code is used
/// for one request and is never stored; `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct MfaDeleteAuth {
    serial: String,
    code: String,
}

impl MfaDeleteAuth {
    /// Checks the serial (the device's ARN or serial number, no spaces) and
    /// the code (six digits).
    pub fn new(serial: &str, code: &str) -> Result<Self> {
        let serial = serial.trim();
        let code = code.trim();
        if serial.is_empty() || serial.chars().any(char::is_whitespace) {
            return Err(Error::Usage(
                "the MFA serial is empty or contains spaces".into(),
            ));
        }
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Usage("an MFA code is six digits".into()));
        }
        Ok(MfaDeleteAuth {
            serial: serial.to_owned(),
            code: code.to_owned(),
        })
    }

    /// The `x-amz-mfa` value: the serial, a space and the code.
    pub(crate) fn header(&self) -> String {
        format!("{} {}", self.serial, self.code)
    }
}

impl fmt::Debug for MfaDeleteAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MfaDeleteAuth")
            .field("serial", &self.serial)
            .field("code", &"<redacted>")
            .finish()
    }
}

impl S3 {
    /// Changes versioning and MFA delete in one request: `status` is the
    /// versioning state to send (`Unversioned` is refused, S3 cannot switch
    /// versioning off) and `mfa_delete` turns MFA delete on or off. S3
    /// requires the device in `auth` for any MFA-delete change, and only the
    /// bucket owner's root account may make it; a refusal is reported as the
    /// server sent it.
    pub async fn put_bucket_versioning_mfa(
        &self,
        bucket: &str,
        status: VersioningStatus,
        mfa_delete: bool,
        auth: &MfaDeleteAuth,
    ) -> Result<()> {
        validate_bucket_name(bucket)?;
        let status = match status {
            VersioningStatus::Enabled => BucketVersioningStatus::Enabled,
            VersioningStatus::Suspended => BucketVersioningStatus::Suspended,
            VersioningStatus::Unversioned => {
                return Err(Error::Usage(
                    "versioning cannot be switched off once it was enabled; choose Enabled or Suspended"
                        .into(),
                ));
            }
        };
        let delete = if mfa_delete {
            MfaDelete::Enabled
        } else {
            MfaDelete::Disabled
        };
        self.client_for(bucket)
            .await?
            .put_bucket_versioning()
            .bucket(bucket)
            .mfa(auth.header())
            .versioning_configuration(
                VersioningConfiguration::builder()
                    .status(status)
                    .mfa_delete(delete)
                    .build(),
            )
            .customize()
            .config_override(self.metadata_timeout())
            .send()
            .await
            .map_err(|error| {
                Error::from(error).with_context(format!(
                    "saving the versioning and MFA delete state of bucket '{bucket}'"
                ))
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn the_header_joins_serial_and_code_and_debug_hides_the_code() {
        let auth = MfaDeleteAuth::new(" arn:aws:iam::123456789012:mfa/device ", "123456").unwrap();
        assert_eq!(auth.header(), "arn:aws:iam::123456789012:mfa/device 123456");
        assert!(!format!("{auth:?}").contains("123456 "));
        assert!(!format!("{auth:?}").contains("\"123456\""));
    }

    #[test]
    fn bad_serials_and_codes_are_usage_errors() {
        for (serial, code) in [
            ("", "123456"),
            ("a b", "123456"),
            ("s", "12345"),
            ("s", "1234567"),
            ("s", "12a456"),
        ] {
            assert_eq!(
                MfaDeleteAuth::new(serial, code).unwrap_err().kind(),
                ErrorKind::Usage,
                "{serial:?} {code:?}"
            );
        }
    }
}
