//! `bb iam ...` against a mocked IAM endpoint (aws-smithy-mocks injected
//! through the session options, like the desktop tests), run in-process
//! with the environment account.

use std::sync::Arc;

use aws_sdk_iam::operation::{
    attach_user_policy::AttachUserPolicyOutput, create_access_key::CreateAccessKeyOutput,
    create_login_profile::CreateLoginProfileOutput, create_user::CreateUserOutput,
    delete_access_key::DeleteAccessKeyOutput, get_login_profile::GetLoginProfileOutput,
    get_user_policy::GetUserPolicyOutput, list_access_keys::ListAccessKeysOutput,
    list_users::ListUsersOutput, put_user_policy::PutUserPolicyOutput,
    update_access_key::UpdateAccessKeyOutput,
};
use aws_sdk_iam::primitives::DateTime;
use aws_sdk_iam::types::{AccessKey, AccessKeyMetadata, LoginProfile, StatusType, User};
use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use bucketbridge_core::session::IamConfigHook;

use bucketbridge_cli::i18n::{tr, trf};

use super::*;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYIPCSECRETKEY";
const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
const VARS: [(&str, &str); 3] = [
    ("BB_ACCESS_KEY_ID", "AKIAADMINEXAMPLE0000"),
    ("BB_SECRET_ACCESS_KEY", "admin-secret"),
    ("BB_REGION", "us-east-1"),
];

fn user(name: &str) -> User {
    User::builder()
        .path("/")
        .user_name(name)
        .user_id("AIDAEXAMPLE")
        .arn(format!("arn:aws:iam::123456789012:user/{name}"))
        .create_date(DateTime::from_secs(1_700_000_000))
        .build()
        .unwrap()
}

fn hook(rules: Vec<Rule>) -> IamConfigHook {
    let rules = Arc::new(rules);
    Arc::new(move |builder| {
        let interceptor = rules.iter().fold(
            MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny),
            |interceptor, rule| interceptor.with_rule(rule),
        );
        builder
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
    })
}

fn bb_with(rules: Vec<Rule>) -> InProcess {
    InProcess {
        iam_hook: Some(hook(rules)),
        options_hook: None,
        ..InProcess::default()
    }
}

async fn run(bb: &InProcess, args: &[&str]) -> Output {
    bb.run_with(args, &VARS, b"", &Terminal::default()).await
}

async fn run_stdin(bb: &InProcess, args: &[&str], stdin: &str) -> Output {
    bb.run_with(args, &VARS, stdin.as_bytes(), &Terminal::default())
        .await
}

fn new_key() -> Rule {
    mock!(aws_sdk_iam::Client::create_access_key).then_output(|| {
        CreateAccessKeyOutput::builder()
            .access_key(
                AccessKey::builder()
                    .user_name("alice")
                    .access_key_id(KEY_ID)
                    .status(StatusType::Active)
                    .secret_access_key(SECRET)
                    .build()
                    .unwrap(),
            )
            .build()
    })
}

#[tokio::test]
async fn lists_and_creates_users() {
    let list = mock!(aws_sdk_iam::Client::list_users).then_output(|| {
        ListUsersOutput::builder()
            .users(user("alice"))
            .users(user("bob"))
            .is_truncated(false)
            .build()
            .unwrap()
    });
    let create = mock!(aws_sdk_iam::Client::create_user)
        .match_requests(|i| i.user_name() == Some("alice") && i.path() == Some("/team/"))
        .then_output(|| CreateUserOutput::builder().user(user("alice")).build());
    let bb = bb_with(vec![list, create.clone()]);

    let out = run(&bb, &["iam", "users"]).await;
    out.expect_code(0);
    assert_eq!(out.lines().len(), 2);
    assert!(out.lines()[0].starts_with("alice\t/\t2023-11-14T22:13:20Z\tarn:aws:iam::"));

    let out = run(&bb, &["--json", "iam", "users"]).await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[1]["name"], "bob");

    let out = run(
        &bb,
        &[
            "--json", "iam", "user", "create", "alice", "--path", "/team/",
        ],
    )
    .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["user_id"], "AIDAEXAMPLE");
    assert_eq!(create.num_calls(), 1);

    // Core validation is a usage error before any request.
    let out = run(&bb, &["iam", "user", "create", "bad name"]).await;
    out.expect_code(1);
    assert_eq!(create.num_calls(), 1);
}

#[tokio::test]
async fn a_new_access_key_secret_is_printed_once_with_a_warning() {
    let create = new_key();
    let bb = bb_with(vec![create.clone()]);

    let out = run(&bb, &["iam", "keys", "create", "alice"]).await;
    out.expect_code(0);
    assert!(
        out.stdout
            .contains(&trf("secret access key: {secret}", &[("secret", SECRET)]))
    );
    assert!(
        out.stdout
            .contains(&trf("access key ID: {key_id}", &[("key_id", KEY_ID)]))
    );
    assert!(out.stderr.contains(&tr("shown only now")), "{}", out.stderr);

    // --quiet does not hide a secret; --json has it as a field and no warning.
    let out = run(
        &bb,
        &["--quiet", "--json", "iam", "keys", "create", "alice"],
    )
    .await;
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["secret_access_key"], SECRET);
    assert_eq!(record["access_key_id"], KEY_ID);
    assert_eq!(out.stderr, "");
    assert_eq!(create.num_calls(), 2);
}

#[tokio::test]
async fn keys_list_and_change() {
    let list = mock!(aws_sdk_iam::Client::list_access_keys).then_output(|| {
        ListAccessKeysOutput::builder()
            .access_key_metadata(
                AccessKeyMetadata::builder()
                    .user_name("alice")
                    .access_key_id(KEY_ID)
                    .status(StatusType::Inactive)
                    .create_date(DateTime::from_secs(1_700_000_000))
                    .build(),
            )
            .is_truncated(false)
            .build()
            .unwrap()
    });
    let update = mock!(aws_sdk_iam::Client::update_access_key)
        .match_requests(|i| i.status() == Some(&StatusType::Active))
        .then_output(|| UpdateAccessKeyOutput::builder().build());
    let delete = mock!(aws_sdk_iam::Client::delete_access_key)
        .then_output(|| DeleteAccessKeyOutput::builder().build());
    let bb = bb_with(vec![list, update.clone(), delete.clone()]);

    let out = run(&bb, &["--json", "iam", "keys", "list", "alice"]).await;
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["user"], "alice");
    assert_eq!(record["status"], "inactive");
    assert!(record.get("secret_access_key").is_none());

    let out = run(&bb, &["iam", "keys", "activate", "alice", KEY_ID]).await;
    out.expect_code(0);
    assert_eq!(update.num_calls(), 1);

    // Deleting needs --yes: no request without it.
    let out = run(&bb, &["iam", "keys", "delete", "alice", KEY_ID]).await;
    out.expect_code(1);
    assert!(out.stderr.contains(&tr("needs --yes")), "{}", out.stderr);
    assert_eq!(delete.num_calls(), 0);
    let out = run(&bb, &["iam", "keys", "delete", "alice", KEY_ID, "--yes"]).await;
    out.expect_code(0);
    assert_eq!(delete.num_calls(), 1);
}

#[tokio::test]
async fn user_delete_login_profile_delete_and_inline_delete_need_yes() {
    let bb = bb_with(Vec::new());
    for args in [
        &["iam", "user", "delete", "alice"][..],
        &["iam", "login-profile", "delete", "alice"][..],
        &["iam", "inline", "delete", "alice", "p"][..],
    ] {
        let out = run(&bb, args).await;
        out.expect_code(1);
        assert!(out.stderr.contains(&tr("needs --yes")), "{}", out.stderr);
    }
}

#[tokio::test]
async fn attaches_a_managed_policy() {
    let attach = mock!(aws_sdk_iam::Client::attach_user_policy)
        .match_requests(|i| {
            i.user_name() == Some("alice")
                && i.policy_arn() == Some("arn:aws:iam::aws:policy/ReadOnlyAccess")
        })
        .then_output(|| AttachUserPolicyOutput::builder().build());
    let bb = bb_with(vec![attach.clone()]);
    let out = run(
        &bb,
        &[
            "--json",
            "iam",
            "policy",
            "attach",
            "alice",
            "arn:aws:iam::aws:policy/ReadOnlyAccess",
        ],
    )
    .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["attached"], true);
    assert_eq!(attach.num_calls(), 1);
}

const POLICY: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]}"#;

#[tokio::test]
async fn inline_policies_from_stdin_and_back() {
    let put = mock!(aws_sdk_iam::Client::put_user_policy)
        .match_requests(|i| i.policy_name() == Some("p") && i.policy_document() == Some(POLICY))
        .then_output(|| PutUserPolicyOutput::builder().build());
    let get = mock!(aws_sdk_iam::Client::get_user_policy).then_output(|| {
        GetUserPolicyOutput::builder()
            .user_name("alice")
            .policy_name("p")
            .policy_document("%7B%22Version%22%3A%222012-10-17%22%2C%22Statement%22%3A%5B%5D%7D")
            .build()
            .unwrap()
    });
    let bb = bb_with(vec![put.clone(), get]);

    let out = run_stdin(
        &bb,
        &["iam", "inline", "put", "alice", "p", "--file", "-"],
        POLICY,
    )
    .await;
    out.expect_code(0);
    assert_eq!(put.num_calls(), 1);

    let out = run(&bb, &["iam", "inline", "get", "alice", "p"]).await;
    out.expect_code(0);
    assert!(
        out.stdout.contains("\"Version\": \"2012-10-17\""),
        "{}",
        out.stdout
    );
    let out = run(&bb, &["--json", "iam", "inline", "get", "alice", "p"]).await;
    assert_eq!(out.records()[0]["name"], "p");
}

#[tokio::test]
async fn login_profiles() {
    let create = mock!(aws_sdk_iam::Client::create_login_profile).then_output(|| {
        CreateLoginProfileOutput::builder()
            .login_profile(
                LoginProfile::builder()
                    .user_name("alice")
                    .create_date(DateTime::from_secs(1_700_000_000))
                    .build()
                    .unwrap(),
            )
            .build()
    });
    let show = mock!(aws_sdk_iam::Client::get_login_profile).then_output(|| {
        GetLoginProfileOutput::builder()
            .login_profile(
                LoginProfile::builder()
                    .user_name("alice")
                    .create_date(DateTime::from_secs(1_700_000_000))
                    .password_reset_required(true)
                    .build()
                    .unwrap(),
            )
            .build()
    });
    let bb = bb_with(vec![create.clone(), show]);

    // Generated: printed once with a warning, and a JSON field.
    let out = run(
        &bb,
        &["iam", "login-profile", "create", "alice", "--generate"],
    )
    .await;
    out.expect_code(0);
    assert!(out.stdout.contains(&tr("password: ")), "{}", out.stdout);
    assert!(out.stderr.contains(&tr("shown only now")), "{}", out.stderr);
    let out = run(
        &bb,
        &[
            "--json",
            "iam",
            "login-profile",
            "create",
            "alice",
            "--generate",
        ],
    )
    .await;
    let password = out.records()[0]["password"].as_str().unwrap().to_owned();
    assert_eq!(password.len(), 24);

    // From stdin the password is never echoed.
    let out = run_stdin(
        &bb,
        &[
            "--json",
            "iam",
            "login-profile",
            "create",
            "alice",
            "--password-stdin",
        ],
        "my-own-password\n",
    )
    .await;
    out.expect_code(0);
    assert!(out.records()[0]["password"].is_null());
    assert!(!out.stdout.contains("my-own-password"));

    // No source and no terminal: usage error.
    let out = run(&bb, &["iam", "login-profile", "create", "alice"]).await;
    out.expect_code(1);
    assert_eq!(create.num_calls(), 3);

    let out = run(&bb, &["iam", "login-profile", "show", "alice"]).await;
    out.expect_code(0);
    assert!(
        out.stdout
            .contains(&tr("must be changed at the next sign-in"))
    );
}

#[tokio::test]
async fn policy_documents_are_checked_without_an_account() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.json");
    let bad = dir.path().join("bad.json");
    std::fs::write(&good, POLICY).unwrap();
    std::fs::write(&bad, r#"{"Statement":[{"Effect":"Maybe"}]}"#).unwrap();
    let bb = InProcess::default();
    let (good, bad) = (good.to_str().unwrap(), bad.to_str().unwrap());

    let out = bb.run(&["iam", "validate-policy", good]).await;
    out.expect_code(0);
    assert!(out.stdout.contains(&tr(": valid")));

    let out = bb.run(&["--json", "iam", "validate-policy", bad]).await;
    out.expect_code(1);
    let record = &out.records()[0];
    assert_eq!(record["valid"], false);
    assert!(!record["issues"].as_array().unwrap().is_empty());

    let out = bb.run(&["iam", "format-policy", good]).await;
    out.expect_code(0);
    assert!(out.stdout.contains("\"Effect\": \"Allow\""));

    let out = bb
        .run(&["iam", "validate-policy", "/nonexistent/x.json"])
        .await;
    out.expect_code(2);
}

#[tokio::test]
async fn custom_endpoints_have_no_iam() {
    let bb = InProcess::default();
    let out = bb
        .run_with(
            &["iam", "users"],
            &[
                ("BB_ACCESS_KEY_ID", "AK"),
                ("BB_SECRET_ACCESS_KEY", "SK"),
                ("BB_ENDPOINT", "http://127.0.0.1:9"),
            ],
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(1);
    assert!(
        out.stderr
            .contains("IAM is not available for this endpoint")
    );
    // No credentials at all: authentication failure.
    bb.run(&["iam", "users"]).await.expect_code(3);
}
