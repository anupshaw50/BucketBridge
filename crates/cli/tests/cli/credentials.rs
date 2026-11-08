//! Credential sources other than static keys, run in-process: AWS profiles
//! (`--profile`, `account import-aws`), assumed roles, the instance role and
//! IAM Identity Center. The S3 server verifies the signatures, so a listing
//! that works proves the right credentials were used. STS, IAM Identity
//! Center and the instance metadata service are mocked.

use std::sync::Arc;

use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use bucketbridge_cli::SessionOptionsHook;
use bucketbridge_core::aws_sdk_sso::operation::get_role_credentials::GetRoleCredentialsOutput;
use bucketbridge_core::aws_sdk_sso::types::RoleCredentials;
use bucketbridge_core::aws_sdk_ssooidc::operation::create_token::{
    CreateTokenError, CreateTokenOutput,
};
use bucketbridge_core::aws_sdk_ssooidc::operation::register_client::RegisterClientOutput;
use bucketbridge_core::aws_sdk_ssooidc::operation::start_device_authorization::StartDeviceAuthorizationOutput;
use bucketbridge_core::aws_sdk_sts::operation::assume_role::{AssumeRoleError, AssumeRoleOutput};
use bucketbridge_core::aws_sdk_sts::types::Credentials as StsCredentials;
use bucketbridge_core::creds::{AwsFiles, CredsHooks, SsoConfigHook, SsoOidcConfigHook};
use bucketbridge_core::session::{SessionOptions, StsConfigHook};
use bucketbridge_tests::{FakeImds, TestS3};

use bucketbridge_cli::i18n::{tr, trf};

use super::*;

const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/Writer";
const MFA_SERIAL: &str = "arn:aws:iam::123456789012:mfa/device";
const START_URL: &str = "https://example.awsapps.com/start";

fn hook(change: impl Fn(&mut SessionOptions) + Send + Sync + 'static) -> SessionOptionsHook {
    Arc::new(move |mut options| {
        change(&mut options);
        options
    })
}

fn coded(code: &str) -> bucketbridge_core::aws_sdk_sts::error::ErrorMetadata {
    bucketbridge_core::aws_sdk_sts::error::ErrorMetadata::builder()
        .code(code)
        .build()
}

/// `~/.aws` of a test: a profile `dev` with the server's keys.
fn aws_files(dir: &tempfile::TempDir, s3: &TestS3) -> AwsFiles {
    std::fs::write(
        dir.path().join("credentials"),
        format!(
            "[dev]\naws_access_key_id = {}\naws_secret_access_key = {}\n\n[unused]\naws_access_key_id = AKIAUNUSED\naws_secret_access_key = unused-secret\n",
            s3.access_key(),
            s3.secret_key()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("config"),
        "[profile dev]\nregion = eu-west-3\n\n[profile orphan]\nrole_arn = arn:aws:iam::123456789012:role/R\n",
    )
    .unwrap();
    AwsFiles::in_dir(dir.path())
}

fn with_files(files: AwsFiles) -> SessionOptionsHook {
    hook(move |options| options.creds_hooks.aws_files = Some(files.clone()))
}

fn endpoint_vars(s3: &TestS3) -> [(&str, &str); 3] {
    [
        ("BB_REGION", s3.region()),
        ("BB_ENDPOINT", s3.endpoint()),
        ("BB_PATH_STYLE", "true"),
    ]
}

#[tokio::test]
async fn the_profile_option_uses_a_profile_without_storing_it() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let bb = InProcess {
        options_hook: Some(with_files(aws_files(&dir, &s3))),
        ..InProcess::default()
    };
    let out = bb
        .run_with(
            &["--profile", "dev", "ls"],
            &endpoint_vars(&s3),
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    // Nothing was stored, in the keychain or in the configuration.
    assert!(
        bb.session()
            .list_accounts()
            .await
            .unwrap()
            .accounts
            .is_empty()
    );
    assert!(bb.store.is_empty());

    let out = bb
        .run_with(
            &["--profile", "nope", "ls"],
            &endpoint_vars(&s3),
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(2);
    assert!(out.stderr.contains("nope"), "{}", out.stderr);

    let out = bb.run(&["--profile", "dev", "--account", "x", "ls"]).await;
    out.expect_code(1);
    assert!(out.stderr.contains("--profile"), "{}", out.stderr);
    let out = bb
        .run(&["--profile", "dev", "--profile", "unused", "ls"])
        .await;
    out.expect_code(1);
}

#[tokio::test]
async fn import_aws_links_profiles_and_copies_no_secret() {
    let s3 = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bb = InProcess {
        options_hook: Some(with_files(aws_files(&dir, &s3))),
        ..InProcess::default()
    };

    let out = bb
        .run(&["account", "import-aws", "--profile", "dev", "--json"])
        .await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["label"], "dev");
    assert_eq!(records[0]["region"], "eu-west-3");
    assert_eq!(
        records[0]["source"],
        serde_json::json!({ "type": "aws_profile", "profile": "dev" })
    );
    assert_eq!(records[0]["secret_ref"], "");
    assert!(bb.store.is_empty());
    let config = std::fs::read_to_string(bb.config.path().join("config.toml")).unwrap();
    assert!(
        !config.contains(s3.secret_key()) && !config.contains(s3.access_key()),
        "{config}"
    );

    // Without names, every usable profile that is not linked yet.
    let out = bb.run(&["account", "import-aws"]).await;
    out.expect_code(0);
    assert!(
        out.stdout.contains(&tr("imported profile as account")),
        "{}",
        out.stdout
    );
    let out = bb.run(&["account", "import-aws"]).await;
    out.expect_code(0);
    assert!(
        out.stderr.contains(&tr("no AWS profile to import")),
        "{}",
        out.stderr
    );

    // Asking for a profile that cannot be imported fails.
    let out = bb
        .run(&["account", "import-aws", "--profile", "orphan"])
        .await;
    out.expect_code(1);
    assert!(out.stdout.contains("orphan"), "{}", out.stdout);
    let out = bb.run(&["account", "list"]).await;
    assert!(out.stdout.contains("aws-profile"), "{}", out.stdout);
    // --profile belongs to import-aws only.
    let out = bb.run(&["account", "list", "--profile", "dev"]).await;
    out.expect_code(1);
}

fn assumed(access_key: &str, secret_key: &str) -> AssumeRoleOutput {
    AssumeRoleOutput::builder()
        .credentials(
            StsCredentials::builder()
                .access_key_id(access_key)
                .secret_access_key(secret_key)
                .session_token("role-session-token")
                .expiration(
                    bucketbridge_core::aws_sdk_s3::primitives::DateTime::from_secs(4_102_444_800),
                )
                .build()
                .unwrap(),
        )
        .build()
}

fn sts_hook(rules: Vec<Rule>) -> StsConfigHook {
    Arc::new(move |builder| {
        let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
        for rule in &rules {
            interceptor = interceptor.with_rule(rule);
        }
        builder
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
    })
}

/// A stored account for the server, and a role account that assumes
/// `ROLE_ARN` through it.
async fn add_role(bb: &InProcess, s3: &TestS3, extra: &[&str]) -> Output {
    add_from_stdin(bb, s3, "Base", &[]).await.expect_code(0);
    let mut args = vec![
        "account",
        "add",
        "--label",
        "Role",
        "--assume-role",
        ROLE_ARN,
        "--source-account",
        "base",
        "--region",
        s3.region(),
        "--endpoint",
        s3.endpoint(),
        "--path-style",
    ];
    args.extend_from_slice(extra);
    bb.run(&args).await
}

async fn add_from_stdin(bb: &InProcess, s3: &TestS3, label: &str, vars: &[(&str, &str)]) -> Output {
    let args = [
        "account",
        "add",
        "--label",
        label,
        "--access-key-id",
        s3.access_key(),
        "--region",
        s3.region(),
        "--endpoint",
        s3.endpoint(),
        "--path-style",
        "--secret-stdin",
    ];
    let stdin = format!("{}\n", s3.secret_key());
    bb.run_with(&args, vars, stdin.as_bytes(), &Terminal::default())
        .await
}

#[tokio::test]
async fn an_assumed_role_signs_with_the_role_session() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let (access_key, secret_key) = (s3.access_key().to_owned(), s3.secret_key().to_owned());
    let rule = mock!(bucketbridge_core::aws_sdk_sts::Client::assume_role)
        .match_requests(|input| {
            input.role_arn() == Some(ROLE_ARN)
                && input.role_session_name() == Some("my-session")
                && input.duration_seconds() == Some(1800)
                && input.external_id() == Some("ext-id-1")
                && input.serial_number().is_none()
        })
        .then_output(move || assumed(&access_key, &secret_key));
    let mut bb = InProcess::default();
    let rules = vec![rule.clone()];
    bb.options_hook = Some(hook(move |options| {
        options.sts_config_hook = Some(sts_hook(rules.clone()));
    }));
    let out = add_role(
        &bb,
        &s3,
        &[
            "--session-name",
            "my-session",
            "--duration",
            "30m",
            "--external-id",
            "ext-id-1",
        ],
    )
    .await;
    out.expect_code(0);
    assert!(
        out.stdout.starts_with(&trf(
            "added account {label}",
            &[("label", "Role")],
        )),
        "{}",
        out.stdout
    );
    let out = bb.run(&["--account", "Role", "ls"]).await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    assert_eq!(rule.num_calls(), 1);
    // The role holds no secret of its own.
    let out = bb.run(&["account", "list", "--json"]).await;
    let records = out.records();
    let role = records.iter().find(|r| r["label"] == "Role").unwrap();
    assert_eq!(role["source"]["type"], "assume_role");
    assert_eq!(role["secret_ref"], "");
    // The source cannot be removed while the role uses it.
    let out = bb.run(&["account", "remove", "Base"]).await;
    out.expect_code(2);
    assert!(out.stderr.contains("Role"), "{}", out.stderr);
}

#[tokio::test]
async fn a_role_that_needs_a_device_takes_the_mfa_code() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let (access_key, secret_key) = (s3.access_key().to_owned(), s3.secret_key().to_owned());
    let good = mock!(bucketbridge_core::aws_sdk_sts::Client::assume_role)
        .match_requests(|input| {
            input.serial_number() == Some(MFA_SERIAL) && input.token_code() == Some("123456")
        })
        .then_output(move || assumed(&access_key, &secret_key));
    let wrong = mock!(bucketbridge_core::aws_sdk_sts::Client::assume_role)
        .match_requests(|input| input.token_code() == Some("000000"))
        .then_error(|| AssumeRoleError::generic(coded("AccessDenied")));
    let mut bb = InProcess::default();
    let rules = vec![good, wrong];
    bb.options_hook = Some(hook(move |options| {
        options.sts_config_hook = Some(sts_hook(rules.clone()));
    }));
    add_role(&bb, &s3, &["--mfa-serial", MFA_SERIAL])
        .await
        .expect_code(0);
    // Without a code the account asks for one.
    let out = bb.run(&["--account", "Role", "ls"]).await;
    out.expect_code(3);
    assert!(out.stderr.contains("MFA"), "{}", out.stderr);
    let out = bb
        .run(&["--account", "Role", "--mfa-code", "000000", "ls"])
        .await;
    out.expect_code(3);
    let out = bb
        .run(&["--account", "Role", "--mfa-code", "123456", "ls"])
        .await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
}

#[tokio::test]
async fn role_options_are_validated() {
    let s3 = TestS3::start().await;
    let bb = InProcess::default();
    add_from_stdin(&bb, &s3, "Base", &[]).await.expect_code(0);
    // No source account.
    let out = bb
        .run(&["account", "add", "--label", "R", "--assume-role", ROLE_ARN])
        .await;
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("source-account"), "{}", out.stderr);
    // A duration outside 15 minutes to 12 hours.
    for duration in ["10m", "13h"] {
        let out = bb
            .run(&[
                "account",
                "add",
                "--label",
                "R",
                "--assume-role",
                ROLE_ARN,
                "--source-account",
                "Base",
                "--duration",
                duration,
            ])
            .await;
        out.expect_code(1);
        assert!(out.stderr.contains("session length"), "{}", out.stderr);
    }
    // An unknown source.
    let out = bb
        .run(&[
            "account",
            "add",
            "--label",
            "R",
            "--assume-role",
            ROLE_ARN,
            "--source-account",
            "nobody",
        ])
        .await;
    out.expect_code(2);
    let out = bb
        .run(&[
            "account",
            "add",
            "--label",
            "R",
            "--assume-role",
            "arn:bad",
            "--source-account",
            "Base",
        ])
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("role ARN"), "{}", out.stderr);
    // Keys and a role cannot be combined.
    let out = bb
        .run(&[
            "account",
            "add",
            "--label",
            "R",
            "--assume-role",
            ROLE_ARN,
            "--source-account",
            "Base",
            "--access-key-id",
            "AKIAX",
        ])
        .await;
    assert_ne!(out.code, 0);
}

#[tokio::test]
async fn the_instance_role_is_probed_and_signs_requests() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let imds = FakeImds::start(s3.access_key(), s3.secret_key());
    let url = imds.url().to_owned();
    let bb = InProcess {
        options_hook: Some(hook(move |options| {
            options.creds_hooks.imds_endpoint = Some(url.clone());
        })),
        ..InProcess::default()
    };
    let out = bb
        .run(&[
            "account",
            "add",
            "--label",
            "Box",
            "--instance-role",
            "--region",
            s3.region(),
            "--endpoint",
            s3.endpoint(),
            "--path-style",
        ])
        .await;
    out.expect_code(0);
    assert!(
        out.stderr.contains(&tr("found the role")),
        "{}",
        out.stderr
    );
    let out = bb.run(&["--account", "Box", "ls"]).await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
    assert!(bb.store.is_empty());
}

#[tokio::test]
async fn adding_the_instance_role_off_ec2_fails_clearly() {
    let s3 = TestS3::start().await;
    let imds = FakeImds::without_role();
    let url = imds.url().to_owned();
    let bb = InProcess {
        options_hook: Some(hook(move |options| {
            options.creds_hooks.imds_endpoint = Some(url.clone());
        })),
        ..InProcess::default()
    };
    let args = [
        "account",
        "add",
        "--label",
        "Box",
        "--instance-role",
        "--endpoint",
        s3.endpoint(),
    ];
    let out = bb.run(&args).await;
    out.expect_code(3);
    assert!(out.stderr.contains("not an EC2 instance"), "{}", out.stderr);
    assert!(
        bb.session()
            .list_accounts()
            .await
            .unwrap()
            .accounts
            .is_empty()
    );
    // The probe can be skipped.
    let mut skipping = args.to_vec();
    skipping.push("--no-probe");
    bb.run(&skipping).await.expect_code(0);
    let out = bb.run(&["account", "list"]).await;
    assert!(out.stdout.contains("instance-role"), "{}", out.stdout);
}

/// OIDC and portal mocks of one sign-in; role credentials are the server's.
fn sso_options(files: AwsFiles, access_key: &str, secret_key: &str) -> SessionOptionsHook {
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
                .user_code("ABCD-EFGH")
                .verification_uri("https://device.example.com/")
                .verification_uri_complete("https://device.example.com/?user_code=ABCD-EFGH")
                .expires_in(600)
                .interval(1)
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
        .output(|| {
            CreateTokenOutput::builder()
                .access_token("sso-access-token-cli")
                .refresh_token("sso-refresh-token-cli")
                .expires_in(28_800)
                .build()
        })
        .build();
    let (access_key, secret_key) = (access_key.to_owned(), secret_key.to_owned());
    let credentials = mock!(bucketbridge_core::aws_sdk_sso::Client::get_role_credentials)
        .match_requests(|input| {
            input.access_token() == Some("sso-access-token-cli")
                && input.role_name() == Some("Admin")
        })
        .then_output(move || {
            GetRoleCredentialsOutput::builder()
                .role_credentials(
                    RoleCredentials::builder()
                        .access_key_id(&access_key)
                        .secret_access_key(&secret_key)
                        .session_token("sso-role-token")
                        .expiration(4_102_444_800_000)
                        .build(),
                )
                .build()
        });
    let oidc_rules = vec![register, start, token];
    let sso_rules = vec![credentials];
    hook(move |options| {
        let oidc_rules = oidc_rules.clone();
        let sso_rules = sso_rules.clone();
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
        options.creds_hooks = CredsHooks {
            aws_files: Some(files.clone()),
            sso_oidc: Some(oidc),
            sso: Some(sso),
            ..CredsHooks::default()
        };
    })
}

#[tokio::test]
async fn sso_sign_in_then_use_an_account() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    let files = AwsFiles::in_dir(dir.path());
    let bb = InProcess {
        options_hook: Some(sso_options(files.clone(), s3.access_key(), s3.secret_key())),
        ..InProcess::default()
    };

    // Not signed in: using an SSO account asks to sign in.
    let out = bb
        .run(&[
            "account",
            "add",
            "--label",
            "Prod",
            "--sso",
            "--sso-start-url",
            START_URL,
            "--sso-region",
            "eu-west-1",
            "--sso-account-id",
            "123456789012",
            "--sso-role-name",
            "Admin",
            "--sso-session",
            "corp",
            "--region",
            s3.region(),
            "--endpoint",
            s3.endpoint(),
            "--path-style",
        ])
        .await;
    out.expect_code(0);
    let out = bb.run(&["--account", "Prod", "ls"]).await;
    out.expect_code(3);
    assert!(out.stderr.contains("sign in"), "{}", out.stderr);

    // Sign in: the code is printed and the token lands in the AWS cache.
    let out = bb
        .run(&["account", "sso-login", "Prod", "--no-browser"])
        .await;
    out.expect_code(0);
    assert!(out.stdout.contains("ABCD-EFGH"), "{}", out.stdout);
    assert!(
        out.stdout.contains(&tr("signed in to")),
        "{}",
        out.stdout
    );
    let cache = bucketbridge_core::creds::sso::CacheKey::new(START_URL, Some("corp"))
        .path(files.sso_cache());
    let cached = std::fs::read_to_string(&cache).unwrap();
    assert!(cached.contains("sso-access-token-cli"));
    let config = std::fs::read_to_string(bb.config.path().join("config.toml")).unwrap();
    assert!(!config.contains("sso-access-token-cli"), "{config}");
    assert!(bb.store.is_empty());

    let out = bb.run(&["--account", "Prod", "ls"]).await;
    out.expect_code(0);
    assert_eq!(out.lines(), [bucket.as_str()]);
}

#[tokio::test]
async fn sso_login_by_portal_options_and_json() {
    let s3 = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let files = AwsFiles::in_dir(dir.path());
    let bb = InProcess {
        options_hook: Some(sso_options(files.clone(), s3.access_key(), s3.secret_key())),
        ..InProcess::default()
    };
    let out = bb
        .run(&[
            "account",
            "sso-login",
            "--start-url",
            START_URL,
            "--sso-region",
            "eu-west-1",
            "--no-browser",
            "--json",
        ])
        .await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[0]["user_code"], "ABCD-EFGH");
    assert_eq!(
        records[0]["verification_uri_complete"],
        "https://device.example.com/?user_code=ABCD-EFGH"
    );
    assert_eq!(records[1]["start_url"], START_URL);
    // Without options and without a stored SSO account the portal is unknown.
    let out = bb.run(&["account", "sso-login", "--no-browser"]).await;
    out.expect_code(1);
    let out = bb
        .run(&["account", "sso-login", "missing", "--no-browser"])
        .await;
    out.expect_code(2);
}
