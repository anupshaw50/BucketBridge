//! Stored S3 accounts, and the account and security types of the IPC
//! contract.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::BucketKind;

/// Region used when an account does not name one.
pub const DEFAULT_REGION: &str = "us-east-1";

fn default_region() -> String {
    DEFAULT_REGION.to_owned()
}

/// Session name used for role sessions when none is given.
pub const DEFAULT_ROLE_SESSION_NAME: &str = "bucketbridge";

/// Shortest role session STS accepts, in seconds.
pub const MIN_ROLE_DURATION_SECS: u32 = 900;

/// Longest role session STS accepts, in seconds (12 hours).
pub const MAX_ROLE_DURATION_SECS: u32 = 43_200;

/// Role session length used when none is given, in seconds.
pub const DEFAULT_ROLE_DURATION_SECS: u32 = 3_600;

fn default_session_name() -> String {
    DEFAULT_ROLE_SESSION_NAME.to_owned()
}

fn default_role_duration() -> u32 {
    DEFAULT_ROLE_DURATION_SECS
}

/// Where an account's credentials come from.
///
/// Only [`CredentialSource::StaticKeys`] stores secrets of its own (in the
/// secret store). Every other source obtains temporary credentials when
/// they are needed and refreshes them before they expire; none of them
/// puts a secret into the configuration file. The JSON form is a tagged
/// union with a `type` field (`static_keys`, `aws_profile`, `assume_role`,
/// `sso`, `instance_role`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialSource {
    /// Access key and secret key held by BucketBridge (the default; the
    /// only kind before version 0.2.0).
    #[default]
    StaticKeys,
    /// A profile of the AWS shared config and credentials files, read when
    /// credentials are needed (the files stay the single source of truth).
    AwsProfile {
        /// Profile name (`default` or the name after `profile ` in the
        /// config file).
        profile: String,
    },
    /// Temporary credentials from STS `AssumeRole`, signed with another
    /// account of BucketBridge.
    AssumeRole {
        /// Id of the account whose credentials call STS.
        source_account_id: String,
        /// ARN of the role to assume.
        role_arn: String,
        /// Role session name.
        #[serde(default = "default_session_name")]
        session_name: String,
        /// Session length in seconds, 900 to 43200.
        #[serde(default = "default_role_duration")]
        duration_secs: u32,
        /// External id the role's trust policy asks for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_id: Option<String>,
        /// MFA device the role's trust policy asks for; a code is asked
        /// for each time the session ends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_serial: Option<String>,
    },
    /// A role of an AWS account reached through IAM Identity Center. The
    /// sign-in token lives in the AWS CLI cache (`~/.aws/sso/cache`).
    Sso {
        /// Start URL of the IAM Identity Center portal.
        start_url: String,
        /// Region of IAM Identity Center (also decides the partition).
        sso_region: String,
        /// AWS account id the role belongs to.
        account_id: String,
        /// Permission set (role) name.
        role_name: String,
        /// Name of the `sso-session` the token is cached under; without it
        /// the cache file is named after the start URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sso_session: Option<String>,
    },
    /// The role of the EC2 instance (IMDSv2) or ECS/EKS container
    /// BucketBridge runs on.
    InstanceRole,
}

impl CredentialSource {
    /// `true` for [`CredentialSource::StaticKeys`].
    pub fn is_static(&self) -> bool {
        matches!(self, CredentialSource::StaticKeys)
    }

    /// The MFA device an assumed role asks for, if any.
    pub fn role_mfa_serial(&self) -> Option<&str> {
        match self {
            CredentialSource::AssumeRole { mfa_serial, .. } => {
                mfa_serial.as_deref().filter(|serial| !serial.is_empty())
            }
            _ => None,
        }
    }
}

/// An S3 account as stored in the configuration file.
///
/// The secret access key and any static session token live in the secret
/// store; the account only holds the keys (`*_ref`) under which they are
/// stored, so this type can be written to disk and sent over IPC safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// UUID v4 identifying the account.
    pub id: String,
    /// Display label, unique across accounts (case-insensitive).
    pub label: String,
    /// Access key ID (not a secret).
    pub access_key_id: String,
    /// Secret-store key of the secret access key.
    pub secret_ref: String,
    /// Secret-store key of a static session token, if the account has one.
    #[serde(default)]
    pub session_token_ref: Option<String>,
    /// Signing region.
    #[serde(default = "default_region")]
    pub region: String,
    /// Custom endpoint URL for S3-compatible providers; `None` means AWS.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Use path-style addressing (`https://host/bucket/key`).
    #[serde(default)]
    pub path_style: bool,
    /// Send `x-amz-request-payer: requester` on every request that supports it.
    #[serde(default)]
    pub request_payer: bool,
    /// MFA device serial number or ARN, for MFA-protected operations. For an
    /// assumed role it is the device the role asks for.
    #[serde(default)]
    pub mfa_serial: Option<String>,
    /// Where the credentials come from; accounts saved before version 0.2.0
    /// have static keys.
    #[serde(default)]
    pub source: CredentialSource,
}

impl Default for Account {
    fn default() -> Self {
        Account {
            id: String::new(),
            label: String::new(),
            access_key_id: String::new(),
            secret_ref: String::new(),
            session_token_ref: None,
            region: default_region(),
            endpoint: None,
            path_style: false,
            request_payer: false,
            mfa_serial: None,
            source: CredentialSource::StaticKeys,
        }
    }
}

impl Account {
    /// `true` when the account's credentials are access keys held by
    /// BucketBridge.
    pub fn has_static_keys(&self) -> bool {
        self.source.is_static()
    }

    /// `true` for an account with static keys that has no secret access key
    /// yet: one imported from an account file (which holds no secrets). It
    /// cannot be used until the secret is entered.
    pub fn needs_secret(&self) -> bool {
        self.has_static_keys() && self.secret_ref.is_empty()
    }

    /// `true` when requests need an MFA session (a code entered with
    /// `mfa_login`): static keys with an MFA device, or an assumed role
    /// whose trust policy asks for one.
    pub fn needs_mfa_session(&self) -> bool {
        match &self.source {
            CredentialSource::StaticKeys => self.mfa_serial.is_some(),
            source => source.role_mfa_serial().is_some(),
        }
    }

    /// The badge reported for this account's buckets: `External` when a
    /// custom endpoint is configured, `Aws` otherwise.
    pub fn bucket_kind(&self) -> BucketKind {
        if self.endpoint.is_some() {
            BucketKind::External
        } else {
            BucketKind::Aws
        }
    }
}

/// Input of `add_account`: a new account together with its secrets.
///
/// Only deserialized (it arrives from the UI or the CLI), never serialized,
/// so its secrets cannot end up in JSON by accident. `Debug` never prints the
/// secret access key or the session token, and every field is zeroed when the
/// value is dropped.
#[derive(Clone, PartialEq, Eq, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct AccountInput {
    /// Display label, unique across accounts (case-insensitive).
    pub label: String,
    /// Access key ID (static keys only).
    #[serde(default)]
    pub access_key_id: String,
    /// Secret access key (static keys only); it goes to the secret store,
    /// never to the configuration file.
    #[serde(default)]
    pub secret_access_key: String,
    /// Static session token for temporary credentials.
    #[serde(default)]
    pub session_token: Option<String>,
    /// Signing region.
    #[serde(default = "default_region")]
    pub region: String,
    /// Custom endpoint URL; `None` means AWS.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Use path-style addressing.
    #[serde(default)]
    pub path_style: bool,
    /// Send `x-amz-request-payer: requester`.
    #[serde(default)]
    pub request_payer: bool,
    /// MFA device serial number or ARN.
    #[serde(default)]
    pub mfa_serial: Option<String>,
    /// Where the credentials come from; `static_keys` when omitted. Any
    /// other source needs no access key or secret.
    #[serde(default)]
    #[zeroize(skip)]
    pub source: CredentialSource,
}

impl Default for AccountInput {
    fn default() -> Self {
        AccountInput {
            label: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            session_token: None,
            region: default_region(),
            endpoint: None,
            path_style: false,
            request_payer: false,
            mfa_serial: None,
            source: CredentialSource::StaticKeys,
        }
    }
}

impl fmt::Debug for AccountInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountInput")
            .field("label", &self.label)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("path_style", &self.path_style)
            .field("request_payer", &self.request_payer)
            .field("mfa_serial", &self.mfa_serial)
            .field("source", &self.source)
            .finish()
    }
}

/// Input of `update_account`: `None` keeps the current value.
///
/// `secret_access_key` rotates the secret and `session_token` replaces the
/// session token; `clear_*` remove an optional value (setting a value and
/// clearing it in one update is a usage error). Like [`AccountInput`] it is
/// only deserialized, redacts its secrets in `Debug` and is zeroed on drop.
#[derive(Clone, Default, PartialEq, Eq, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct AccountUpdate {
    /// New display label.
    #[serde(default)]
    pub label: Option<String>,
    /// New access key ID.
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// New secret access key (key rotation).
    #[serde(default)]
    pub secret_access_key: Option<String>,
    /// New static session token.
    #[serde(default)]
    pub session_token: Option<String>,
    /// Remove the static session token.
    #[serde(default)]
    pub clear_session_token: bool,
    /// New signing region.
    #[serde(default)]
    pub region: Option<String>,
    /// New custom endpoint URL.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Remove the custom endpoint (back to AWS).
    #[serde(default)]
    pub clear_endpoint: bool,
    /// New path-style setting.
    #[serde(default)]
    pub path_style: Option<bool>,
    /// New requester-pays setting.
    #[serde(default)]
    pub request_payer: Option<bool>,
    /// New MFA device serial number or ARN.
    #[serde(default)]
    pub mfa_serial: Option<String>,
    /// Remove the MFA device.
    #[serde(default)]
    pub clear_mfa_serial: bool,
    /// New credential source of an account that already has a source other
    /// than static keys (for example another role duration); the kind of
    /// source cannot change.
    #[serde(default)]
    #[zeroize(skip)]
    pub source: Option<CredentialSource>,
}

impl fmt::Debug for AccountUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |value: &Option<String>| value.as_ref().map(|_| "<redacted>");
        f.debug_struct("AccountUpdate")
            .field("label", &self.label)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &redacted(&self.secret_access_key))
            .field("session_token", &redacted(&self.session_token))
            .field("clear_session_token", &self.clear_session_token)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("clear_endpoint", &self.clear_endpoint)
            .field("path_style", &self.path_style)
            .field("request_payer", &self.request_payer)
            .field("mfa_serial", &self.mfa_serial)
            .field("clear_mfa_serial", &self.clear_mfa_serial)
            .field("source", &self.source)
            .finish()
    }
}

/// Result of `list_accounts`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountList {
    /// Every stored account.
    pub accounts: Vec<Account>,
    /// Account used when none is named.
    pub default_account_id: Option<String>,
    /// The account folders in their order, each with its accounts. Accounts
    /// in no folder are listed after the folders. Empty for a configuration
    /// from before folders existed.
    #[serde(default)]
    pub folders: Vec<crate::accountfile::AccountFolder>,
}

/// Result of `security_status` and payload of `security://changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SecurityStatus {
    /// A master password protects the stored secrets.
    pub master_password_set: bool,
    /// Secrets can be read: no master password is set, or it was unlocked.
    pub unlocked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Account {
        Account {
            id: "6f1c2b8e-3f7a-4c1d-9a55-0d2b7e4f9a10".into(),
            label: "Work".into(),
            access_key_id: "AKIAEXAMPLE".into(),
            secret_ref: "account/6f1c/secret".into(),
            session_token_ref: None,
            region: "eu-west-1".into(),
            endpoint: Some("https://s3.example.com".into()),
            path_style: true,
            request_payer: false,
            mfa_serial: None,
            source: CredentialSource::StaticKeys,
        }
    }

    #[test]
    fn json_shape_matches_contract() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(
            value,
            json!({
                "id": "6f1c2b8e-3f7a-4c1d-9a55-0d2b7e4f9a10",
                "label": "Work",
                "access_key_id": "AKIAEXAMPLE",
                "secret_ref": "account/6f1c/secret",
                "session_token_ref": null,
                "region": "eu-west-1",
                "endpoint": "https://s3.example.com",
                "path_style": true,
                "request_payer": false,
                "mfa_serial": null,
                "source": { "type": "static_keys" }
            })
        );
        let back: Account = serde_json::from_value(value).unwrap();
        assert_eq!(back, sample());
    }

    #[test]
    fn optional_fields_default_when_missing() {
        let account: Account = serde_json::from_value(json!({
            "id": "a",
            "label": "b",
            "access_key_id": "c",
            "secret_ref": "d"
        }))
        .unwrap();
        assert_eq!(account.region, DEFAULT_REGION);
        assert_eq!(account.endpoint, None);
        assert!(!account.path_style);
        assert!(!account.request_payer);
        assert_eq!(account.bucket_kind(), BucketKind::Aws);
    }

    #[test]
    fn credential_sources_round_trip_as_tagged_unions() {
        let sources = [
            CredentialSource::StaticKeys,
            CredentialSource::AwsProfile {
                profile: "dev".into(),
            },
            CredentialSource::AssumeRole {
                source_account_id: "a".into(),
                role_arn: "arn:aws:iam::123456789012:role/R".into(),
                session_name: "s".into(),
                duration_secs: 900,
                external_id: Some("x".into()),
                mfa_serial: None,
            },
            CredentialSource::Sso {
                start_url: "https://example.awsapps.com/start".into(),
                sso_region: "eu-west-1".into(),
                account_id: "123456789012".into(),
                role_name: "Admin".into(),
                sso_session: Some("work".into()),
            },
            CredentialSource::InstanceRole,
        ];
        for source in sources {
            let json = serde_json::to_value(&source).unwrap();
            assert!(json["type"].is_string());
            assert_eq!(
                serde_json::from_value::<CredentialSource>(json).unwrap(),
                source
            );
        }
        let role: CredentialSource = serde_json::from_value(json!({
            "type": "assume_role", "source_account_id": "a", "role_arn": "arn:aws:iam::1:role/r"
        }))
        .unwrap();
        assert_eq!(
            role,
            CredentialSource::AssumeRole {
                source_account_id: "a".into(),
                role_arn: "arn:aws:iam::1:role/r".into(),
                session_name: DEFAULT_ROLE_SESSION_NAME.into(),
                duration_secs: DEFAULT_ROLE_DURATION_SECS,
                external_id: None,
                mfa_serial: None,
            }
        );
    }

    #[test]
    fn accounts_without_a_source_have_static_keys() {
        let account: Account = serde_json::from_value(json!({
            "id": "a", "label": "b", "access_key_id": "c", "secret_ref": "d"
        }))
        .unwrap();
        assert_eq!(account.source, CredentialSource::StaticKeys);
        assert!(account.has_static_keys());
        assert!(!account.needs_mfa_session());
    }

    #[test]
    fn required_fields_are_required() {
        let err =
            serde_json::from_value::<Account>(json!({ "id": "a", "label": "b" })).unwrap_err();
        assert!(err.to_string().contains("access_key_id"), "{err}");
    }

    #[test]
    fn custom_endpoint_means_external_buckets() {
        assert_eq!(sample().bucket_kind(), BucketKind::External);
        assert_eq!(Account::default().region, "us-east-1");
    }

    #[test]
    fn account_input_shape_and_redaction() {
        let input: AccountInput = serde_json::from_value(json!({
            "label": "Work",
            "access_key_id": "AKIAEXAMPLE",
            "secret_access_key": "secret-value-1",
            "session_token": "token-value-1",
            "region": "eu-west-1",
            "endpoint": null,
            "path_style": false,
            "request_payer": true,
            "mfa_serial": null
        }))
        .unwrap();
        assert_eq!(input.secret_access_key, "secret-value-1");
        assert_eq!(input.session_token.as_deref(), Some("token-value-1"));
        assert!(input.request_payer);
        let debug = format!("{input:?}");
        assert!(!debug.contains("secret-value-1") && !debug.contains("token-value-1"));
        assert!(debug.contains("AKIAEXAMPLE"));

        let minimal: AccountInput = serde_json::from_value(json!({
            "label": "a", "access_key_id": "b", "secret_access_key": "c"
        }))
        .unwrap();
        assert_eq!(minimal.region, DEFAULT_REGION);
        assert_eq!(minimal.session_token, None);
    }

    #[test]
    fn account_update_shape_and_redaction() {
        let update: AccountUpdate = serde_json::from_value(json!({
            "label": null,
            "access_key_id": null,
            "secret_access_key": "rotated-secret",
            "session_token": null,
            "clear_session_token": true,
            "region": "us-west-2",
            "endpoint": null,
            "clear_endpoint": false,
            "path_style": null,
            "request_payer": false,
            "mfa_serial": null,
            "clear_mfa_serial": false
        }))
        .unwrap();
        assert_eq!(update.secret_access_key.as_deref(), Some("rotated-secret"));
        assert!(update.clear_session_token);
        assert_eq!(update.request_payer, Some(false));
        assert_eq!(update.path_style, None);
        assert!(!format!("{update:?}").contains("rotated-secret"));
        let empty: AccountUpdate = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, AccountUpdate::default());
    }

    #[test]
    fn list_and_status_shapes() {
        let list = AccountList {
            accounts: vec![sample()],
            default_account_id: Some(sample().id),
            folders: Vec::new(),
        };
        let value = serde_json::to_value(&list).unwrap();
        assert_eq!(value["default_account_id"], json!(sample().id));
        assert_eq!(value["accounts"][0]["label"], json!("Work"));
        let status = SecurityStatus {
            master_password_set: true,
            unlocked: false,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({ "master_password_set": true, "unlocked": false })
        );
    }
}
