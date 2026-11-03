//! `list_aws_profiles`, `import_aws_profiles`, `probe_instance_role` and the
//! `sso_*` commands, with the AWS files in a temporary folder and IAM
//! Identity Center and the metadata service mocked.

use std::sync::Arc;

use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use bucketbridge_core::aws_sdk_sso::operation::list_account_roles::ListAccountRolesOutput;
use bucketbridge_core::aws_sdk_sso::operation::list_accounts::ListAccountsOutput;
use bucketbridge_core::aws_sdk_sso::types::{AccountInfo, RoleInfo};
use bucketbridge_core::aws_sdk_ssooidc::operation::create_token::{
    CreateTokenError, CreateTokenOutput,
};
use bucketbridge_core::aws_sdk_ssooidc::operation::register_client::RegisterClientOutput;
use bucketbridge_core::aws_sdk_ssooidc::operation::start_device_authorization::StartDeviceAuthorizationOutput;
use bucketbridge_core::creds::{AwsFiles, CredsHooks, SsoConfigHook, SsoOidcConfigHook};
use bucketbridge_tests::FakeImds;
use serde_json::json;

use super::harness::Harness;

const START_URL: &str = "https://example.awsapps.com/start";

fn aws_files(dir: &tempfile::TempDir) -> AwsFiles {
    std::fs::write(
        dir.path().join("credentials"),
        "[dev]\naws_access_key_id = AKIADESKTOPDEV\naws_secret_access_key = desktop-dev-secret\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("config"),
        "[profile dev]\nregion = eu-west-1\n\n[profile broken]\nregion = us-east-1\n",
    )
    .unwrap();
    AwsFiles::in_dir(dir.path())
}

#[test]
fn aws_profiles_are_listed_and_imported_without_their_secrets() {
    let files_dir = tempfile::tempdir().unwrap();
    let files = aws_files(&files_dir);
    let h = Harness::with_options(move |options| options.creds_hooks.aws_files = Some(files));
    let listed = h.ok("list_aws_profiles", json!({}));
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["broken", "dev"]);
    assert_eq!(listed[1]["kind"], "static_keys");
    assert_eq!(listed[1]["region"], "eu-west-1");
    assert_eq!(listed[1]["importable"], true);
    assert_eq!(listed[1]["account_id"], serde_json::Value::Null);
    assert_eq!(listed[0]["importable"], false);
    assert!(!listed.to_string().contains("desktop-dev-secret"));

    let outcome = h.ok(
        "import_aws_profiles",
        json!({ "names": ["dev", "broken", "nope"] }),
    );
    let imported = outcome["imported"].as_array().unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0]["label"], "dev");
    assert_eq!(imported[0]["region"], "eu-west-1");
    assert_eq!(imported[0]["secret_ref"], "");
    assert_eq!(imported[0]["access_key_id"], "");
    assert_eq!(
        imported[0]["source"],
        json!({ "type": "aws_profile", "profile": "dev" })
    );
    assert_eq!(outcome["skipped"].as_array().unwrap().len(), 2);
    // No secret was copied anywhere.
    assert!(h.secrets.is_empty());
    let config = std::fs::read_to_string(h.dir.path().join("app").join("config.toml")).unwrap();
    assert!(!config.contains("desktop-dev-secret") && !config.contains("AKIADESKTOPDEV"));
    // The picker now shows the link.
    let listed = h.ok("list_aws_profiles", json!({}));
    assert_eq!(listed[1]["account_id"], imported[0]["id"]);
    let again = h.ok("import_aws_profiles", json!({ "names": ["dev"] }));
    assert!(again["imported"].as_array().unwrap().is_empty());
}

#[test]
fn accounts_of_every_source_go_through_add_and_update_account() {
    let h = Harness::new();
    // Static keys without a source still work, and get one in the answer.
    let imds = FakeImds::start("AKIAINSTANCEDEV", "instance-secret");
    let box_account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Box",
            "source": { "type": "instance_role" },
            "region": "eu-west-1"
        } }),
    );
    assert_eq!(box_account["source"], json!({ "type": "instance_role" }));
    assert_eq!(box_account["secret_ref"], "");
    assert!(h.secrets.is_empty());
    drop(imds);

    let base = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Base",
            "access_key_id": "AKIABASEEXAMPLE",
            "secret_access_key": "base-secret-value-123"
        } }),
    );
    assert_eq!(base["source"], json!({ "type": "static_keys" }));
    let role = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Role",
            "source": {
                "type": "assume_role",
                "source_account_id": base["id"],
                "role_arn": "arn:aws:iam::123456789012:role/Writer",
                "session_name": "desk",
                "duration_secs": 7200,
                "external_id": "ext-1",
                "mfa_serial": "arn:aws:iam::123456789012:mfa/me"
            }
        } }),
    );
    assert_eq!(role["mfa_serial"], "arn:aws:iam::123456789012:mfa/me");
    // Edit the role's session length.
    let updated = h.ok(
        "update_account",
        json!({ "id": role["id"], "update": { "source": {
            "type": "assume_role",
            "source_account_id": base["id"],
            "role_arn": "arn:aws:iam::123456789012:role/Writer",
            "session_name": "desk",
            "duration_secs": 900,
            "mfa_serial": "arn:aws:iam::123456789012:mfa/me"
        } } }),
    );
    assert_eq!(updated["source"]["duration_secs"], 900);
    assert_eq!(updated["source"]["external_id"], serde_json::Value::Null);
    // A malformed code never reaches STS.
    assert_eq!(
        h.code(
            "mfa_login",
            json!({ "account_id": role["id"], "code": "12" })
        ),
        "usage"
    );
    // Invalid settings are usage errors; the kind cannot change.
    assert_eq!(
        h.code(
            "add_account",
            json!({ "input": {
                "label": "Bad",
                "source": {
                    "type": "assume_role",
                    "source_account_id": base["id"],
                    "role_arn": "arn:aws:iam::123456789012:role/Writer",
                    "duration_secs": 100
                }
            } })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "update_account",
            json!({ "id": base["id"], "update": { "source": { "type": "instance_role" } } })
        ),
        "usage"
    );
    // The source cannot be removed while the role uses it.
    assert_eq!(
        h.code("remove_account", json!({ "id": base["id"] })),
        "conflict"
    );
}

#[test]
fn the_instance_role_probe_reports_the_role_or_why_there_is_none() {
    let imds = FakeImds::start("AKIAPROBEKEY", "probe-secret");
    let url = imds.url().to_owned();
    let h = Harness::with_options(move |options| options.creds_hooks.imds_endpoint = Some(url));
    let found = h.ok("probe_instance_role", json!({}));
    assert_eq!(found["kind"], "instance");
    assert_eq!(found["access_key_id"], "AKIAPROBEKEY");
    assert_eq!(found["expires_at"], "2099-01-01T00:00:00Z");
    assert!(!found.to_string().contains("probe-secret"));

    let empty = FakeImds::without_role();
    let url = empty.url().to_owned();
    let h = Harness::with_options(move |options| options.creds_hooks.imds_endpoint = Some(url));
    let (code, message) = h.error("probe_instance_role", json!({}));
    assert_eq!(code, "auth");
    assert!(message.contains("not an EC2 instance"), "{message}");
}

fn sso_hooks(files: AwsFiles) -> CredsHooks {
    let register =
        mock!(bucketbridge_core::aws_sdk_ssooidc::Client::register_client).then_output(|| {
            RegisterClientOutput::builder()
                .client_id("client-1")
                .client_secret("client-secret-1")
                .client_secret_expires_at(4_102_444_800)
                .build()
        });
    let start = mock!(bucketbridge_core::aws_sdk_ssooidc::Client::start_device_authorization)
        .then_output(|| {
            StartDeviceAuthorizationOutput::builder()
                .device_code("device-code-1")
                .user_code("QRST-5678")
                .verification_uri("https://device.example.com/")
                .verification_uri_complete("https://device.example.com/?user_code=QRST-5678")
                .expires_in(600)
                .interval(5)
                .build()
        });
    let token = mock!(bucketbridge_core::aws_sdk_ssooidc::Client::create_token)
        .sequence()
        .error(|| {
            CreateTokenError::generic(
                bucketbridge_core::aws_sdk_ssooidc::error::ErrorMetadata::builder()
                    .code("AuthorizationPendingException")
                    .build(),
            )
        })
        .error(|| {
            CreateTokenError::generic(
                bucketbridge_core::aws_sdk_ssooidc::error::ErrorMetadata::builder()
                    .code("SlowDownException")
                    .build(),
            )
        })
        .output(|| {
            CreateTokenOutput::builder()
                .access_token("desktop-access-token")
                .refresh_token("desktop-refresh-token")
                .expires_in(28_800)
                .build()
        })
        .build();
    let accounts = mock!(bucketbridge_core::aws_sdk_sso::Client::list_accounts)
        .match_requests(|input| input.access_token() == Some("desktop-access-token"))
        .then_output(|| {
            ListAccountsOutput::builder()
                .account_list(
                    AccountInfo::builder()
                        .account_id("123456789012")
                        .account_name("Production")
                        .email_address("prod@example.com")
                        .build(),
                )
                .build()
        });
    let roles = mock!(bucketbridge_core::aws_sdk_sso::Client::list_account_roles)
        .match_requests(|input| input.account_id() == Some("123456789012"))
        .then_output(|| {
            ListAccountRolesOutput::builder()
                .role_list(
                    RoleInfo::builder()
                        .role_name("Admin")
                        .account_id("123456789012")
                        .build(),
                )
                .build()
        });
    let oidc_rules: Vec<Rule> = vec![register, start, token];
    let sso_rules: Vec<Rule> = vec![accounts, roles];
    let oidc: SsoOidcConfigHook = Arc::new(move |builder| {
        let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
        for rule in &oidc_rules {
            interceptor = interceptor.with_rule(rule);
        }
        builder
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
    });
    let sso: SsoConfigHook = Arc::new(move |builder| {
        let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
        for rule in &sso_rules {
            interceptor = interceptor.with_rule(rule);
        }
        builder
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
    });
    CredsHooks {
        aws_files: Some(files),
        sso_oidc: Some(oidc),
        sso: Some(sso),
        ..CredsHooks::default()
    }
}

#[test]
fn the_sso_sign_in_flow_ends_with_a_picked_account_and_role() {
    let files_dir = tempfile::tempdir().unwrap();
    let files = AwsFiles::in_dir(files_dir.path());
    let hooks = sso_hooks(files.clone());
    let h = Harness::with_options(move |options| options.creds_hooks = hooks);
    let portal =
        json!({ "start_url": START_URL, "sso_region": "eu-west-1", "sso_session": "corp" });

    let status = h.ok("sso_status", portal.clone());
    assert_eq!(status["signed_in"], false);
    assert_eq!(h.code("sso_list_accounts", portal.clone()), "auth");

    let started = h.ok("sso_start_login", portal.clone());
    assert_eq!(started["user_code"], "QRST-5678");
    assert_eq!(started["verification_uri"], "https://device.example.com/");
    assert_eq!(started["interval_secs"], 5);
    assert_eq!(started["browser_opened"], true);
    assert!(!started.to_string().contains("device-code-1"));
    // The page with the code filled in was opened, through the opener seam.
    assert_eq!(
        *h.urls.lock().unwrap(),
        ["https://device.example.com/?user_code=QRST-5678"]
    );
    let id = started["login_id"].clone();
    let first = h.ok("sso_poll_login", json!({ "login_id": id }));
    assert_eq!(first, json!({ "state": "pending", "interval_secs": 5 }));
    // slow_down lengthens the interval.
    let second = h.ok("sso_poll_login", json!({ "login_id": id }));
    assert_eq!(second, json!({ "state": "pending", "interval_secs": 10 }));
    let done = h.ok("sso_poll_login", json!({ "login_id": id }));
    assert_eq!(done["state"], "complete");
    assert!(done["expires_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        h.code("sso_poll_login", json!({ "login_id": id })),
        "not_found"
    );

    // The token is in the AWS CLI cache only.
    let key = bucketbridge_core::creds::sso::CacheKey::new(START_URL, Some("corp"));
    let cache = std::fs::read_to_string(key.path(files.sso_cache())).unwrap();
    assert!(cache.contains("desktop-access-token"));
    assert!(h.secrets.is_empty());
    let status = h.ok("sso_status", portal.clone());
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["can_refresh"], true);

    let accounts = h.ok("sso_list_accounts", portal.clone());
    assert_eq!(accounts[0]["account_id"], "123456789012");
    assert_eq!(accounts[0]["account_name"], "Production");
    let mut with_account = portal.clone();
    with_account["account_id"] = json!("123456789012");
    assert_eq!(h.ok("sso_list_roles", with_account), json!(["Admin"]));

    // The account is stored with the choice; no token reaches the config.
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Prod",
            "source": {
                "type": "sso",
                "start_url": START_URL,
                "sso_region": "eu-west-1",
                "account_id": "123456789012",
                "role_name": "Admin",
                "sso_session": "corp"
            }
        } }),
    );
    assert_eq!(account["source"]["role_name"], "Admin");
    let config = std::fs::read_to_string(h.dir.path().join("app").join("config.toml")).unwrap();
    assert!(!config.contains("desktop-access-token"));

    h.ok("sso_sign_out", portal.clone());
    assert!(!key.path(files.sso_cache()).exists());
    assert_eq!(h.ok("sso_status", portal)["signed_in"], false);
}

#[test]
fn a_sign_in_can_be_cancelled_and_the_browser_left_closed() {
    let files_dir = tempfile::tempdir().unwrap();
    let hooks = sso_hooks(AwsFiles::in_dir(files_dir.path()));
    let h = Harness::with_options(move |options| options.creds_hooks = hooks);
    let started = h.ok(
        "sso_start_login",
        json!({ "start_url": START_URL, "sso_region": "eu-west-1", "open_browser": false }),
    );
    assert_eq!(started["browser_opened"], false);
    assert!(h.urls.lock().unwrap().is_empty());
    h.ok(
        "sso_cancel_login",
        json!({ "login_id": started["login_id"] }),
    );
    assert_eq!(
        h.code("sso_poll_login", json!({ "login_id": started["login_id"] })),
        "not_found"
    );
    assert_eq!(
        h.code(
            "sso_start_login",
            json!({ "start_url": "http://insecure.example.com", "sso_region": "eu-west-1" })
        ),
        "usage"
    );
}
