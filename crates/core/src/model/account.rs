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
    /// MFA device serial number or ARN, for MFA-protected operations.
    #[serde(default)]
    pub mfa_serial: Option<String>,
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
        }
    }
}

impl Account {
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
    /// Access key ID.
    pub access_key_id: String,
    /// Secret access key; it goes to the secret store, never to the
    /// configuration file.
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
                "mfa_serial": null
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
