//! IAM manager commands against a mocked IAM endpoint: create user, create
//! access key, attach policy, delete (with typed confirmation).

use std::sync::Arc;

use aws_sdk_iam::operation::{
    attach_user_policy::AttachUserPolicyOutput, create_access_key::CreateAccessKeyOutput,
    create_user::CreateUserOutput, delete_user::DeleteUserOutput, get_user::GetUserOutput,
    list_access_keys::ListAccessKeysOutput,
    list_attached_user_policies::ListAttachedUserPoliciesOutput,
    list_groups_for_user::ListGroupsForUserOutput, list_user_policies::ListUserPoliciesOutput,
};
use aws_sdk_iam::types::{AccessKey, StatusType, User};
use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use serde_json::json;

use crate::harness::*;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYIPCSECRETKEY";

fn user() -> User {
    User::builder()
        .path("/")
        .user_name("alice")
        .user_id("AIDAALICE")
        .arn("arn:aws:iam::123456789012:user/alice")
        .create_date(aws_smithy_types_date())
        .build()
        .unwrap()
}

fn aws_smithy_types_date() -> aws_sdk_iam::primitives::DateTime {
    aws_sdk_iam::primitives::DateTime::from_secs(1_700_000_000)
}

#[test]
fn custom_endpoint_accounts_have_no_iam() {
    let h = Harness::new();
    let server = start_s3();
    let id = h.add_account(&server, "local");
    let status = h.ok("iam_status", json!({ "account_id": id }));
    assert_eq!(
        status,
        json!({ "supported": false, "message": "IAM is not available for this endpoint" })
    );
    let (code, message) = h.error("iam_list_users", json!({ "account_id": id }));
    assert_eq!(code, "usage");
    assert_eq!(message, "IAM is not available for this endpoint");
    assert_eq!(
        h.code("iam_status", json!({ "account_id": "nope" })),
        "not_found"
    );
}

#[test]
fn policy_documents_are_validated_without_an_account() {
    let h = Harness::new();
    assert_eq!(
        h.ok(
            "iam_validate_policy",
            json!({ "document": r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"# })
        ),
        json!([])
    );
    let issues = h.ok("iam_validate_policy", json!({ "document": "{" }));
    assert_eq!(issues.as_array().unwrap().len(), 1);
    assert_eq!(
        h.code("iam_format_policy", json!({ "document": "{" })),
        "usage"
    );
}

#[test]
fn create_user_key_attach_policy_and_delete() {
    let create = mock!(aws_sdk_iam::Client::create_user)
        .then_output(|| CreateUserOutput::builder().user(user()).build());
    let key = mock!(aws_sdk_iam::Client::create_access_key).then_output(|| {
        CreateAccessKeyOutput::builder()
            .access_key(
                AccessKey::builder()
                    .user_name("alice")
                    .access_key_id("AKIAIOSFODNN7EXAMPLE")
                    .status(StatusType::Active)
                    .secret_access_key(SECRET)
                    .build()
                    .unwrap(),
            )
            .build()
    });
    let attach = mock!(aws_sdk_iam::Client::attach_user_policy)
        .match_requests(|i| i.policy_arn() == Some("arn:aws:iam::aws:policy/ReadOnlyAccess"))
        .then_output(|| AttachUserPolicyOutput::builder().build());
    let get = mock!(aws_sdk_iam::Client::get_user)
        .then_output(|| GetUserOutput::builder().user(user()).build());
    let keys = mock!(aws_sdk_iam::Client::list_access_keys).then_output(|| {
        ListAccessKeysOutput::builder()
            .set_access_key_metadata(Some(Vec::new()))
            .is_truncated(false)
            .build()
            .unwrap()
    });
    let inline = mock!(aws_sdk_iam::Client::list_user_policies).then_output(|| {
        ListUserPoliciesOutput::builder()
            .set_policy_names(Some(Vec::new()))
            .is_truncated(false)
            .build()
            .unwrap()
    });
    let attached = mock!(aws_sdk_iam::Client::list_attached_user_policies)
        .then_output(|| ListAttachedUserPoliciesOutput::builder().build());
    let profile = mock!(aws_sdk_iam::Client::delete_login_profile)
        .then_http_response(|| aws_smithy_runtime_api_response(404, "NoSuchEntity"));
    let groups = mock!(aws_sdk_iam::Client::list_groups_for_user).then_output(|| {
        ListGroupsForUserOutput::builder()
            .set_groups(Some(Vec::new()))
            .is_truncated(false)
            .build()
            .unwrap()
    });
    let delete =
        mock!(aws_sdk_iam::Client::delete_user).then_output(|| DeleteUserOutput::builder().build());

    let rules: Arc<Vec<Rule>> = Arc::new(vec![
        create,
        key,
        attach.clone(),
        get,
        keys,
        inline,
        attached,
        profile,
        groups,
        delete.clone(),
    ]);
    let h = Harness::with_session_options(move |mut options| {
        let rules = Arc::clone(&rules);
        options.iam_config_hook = Some(Arc::new(move |builder| {
            let interceptor = rules.iter().fold(
                MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny),
                |interceptor, rule| interceptor.with_rule(rule),
            );
            builder
                .interceptor(interceptor)
                .http_client(create_mock_http_client())
        }));
        options
    });
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "aws", "access_key_id": "AKIAADMINEXAMPLE0000",
            "secret_access_key": "admin-secret", "session_token": null,
            "region": "eu-west-1", "endpoint": null, "path_style": false,
            "request_payer": false, "mfa_serial": null
        } }),
    );
    let id = account["id"].as_str().unwrap();

    assert_eq!(
        h.ok("iam_status", json!({ "account_id": id }))["supported"],
        true
    );
    let created = h.ok(
        "iam_create_user",
        json!({ "account_id": id, "name": "alice", "path": null }),
    );
    assert_eq!(created["name"], "alice");
    assert_eq!(
        keys_of(&created),
        [
            "arn",
            "created",
            "name",
            "password_last_used",
            "path",
            "user_id"
        ]
    );

    let new_key = h.ok(
        "iam_create_access_key",
        json!({ "account_id": id, "user": "alice" }),
    );
    assert_eq!(new_key["access_key_id"], "AKIAIOSFODNN7EXAMPLE");
    assert_eq!(new_key["secret_access_key"], SECRET);

    h.ok(
        "iam_attach_policy",
        json!({ "account_id": id, "user": "alice", "policy_arn": "arn:aws:iam::aws:policy/ReadOnlyAccess" }),
    );
    assert_eq!(attach.num_calls(), 1);

    // No typed confirmation, no delete.
    assert_eq!(
        h.code(
            "iam_delete_user",
            json!({ "account_id": id, "name": "alice", "confirm": "" })
        ),
        "usage"
    );
    assert_eq!(delete.num_calls(), 0);
    let outcome = h.ok(
        "iam_delete_user",
        json!({ "account_id": id, "name": "alice", "confirm": "alice" }),
    );
    assert_eq!(outcome["deleted"], true);
    assert_eq!(
        outcome["steps"].as_array().unwrap().last().unwrap()["kind"],
        "user"
    );
    assert_eq!(delete.num_calls(), 1);

    // The secret reached the UI and no log.
    let log = h.ok(
        "get_log",
        json!({ "levels": null, "text": null, "limit": null }),
    );
    assert!(!log.to_string().contains(SECRET), "{log}");
}

fn keys_of(value: &serde_json::Value) -> Vec<String> {
    keys(value)
}

fn aws_smithy_runtime_api_response(
    status: u16,
    code: &str,
) -> aws_sdk_iam::config::http::HttpResponse {
    aws_sdk_iam::config::http::HttpResponse::new(
        status.try_into().unwrap(),
        format!(
            "<ErrorResponse><Error><Type>Sender</Type><Code>{code}</Code>\
             <Message>none</Message></Error><RequestId>r</RequestId></ErrorResponse>"
        )
        .into(),
    )
}
