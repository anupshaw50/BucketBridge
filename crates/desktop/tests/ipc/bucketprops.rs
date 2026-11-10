//! Default encryption and tags of a bucket (docs/desktop-api.md, "Bucket
//! encryption, tags, ACLs and creation options"), invoked with the JSON the
//! UI sends. S3 is a scripted `MockS3` that records the requests.

use bucketbridge_tests::{MockResponse, MockS3};
use serde_json::{Value, json};

use crate::harness::*;

const BUCKET: &str = "costs";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

struct Fixture {
    h: Harness,
    mock: MockS3,
    account: String,
}

fn fixture(
    handler: impl Fn(&bucketbridge_tests::RecordedRequest) -> MockResponse + Send + Sync + 'static,
) -> Fixture {
    let mock = MockS3::start(handler);
    let h = Harness::new();
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Mock",
            "access_key_id": "AKIDMOCK",
            "secret_access_key": "mock-secret",
            "session_token": null,
            "region": "us-east-1",
            "endpoint": mock.endpoint(),
            "path_style": true,
            "request_payer": false,
            "mfa_serial": null,
        }}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    Fixture { h, mock, account }
}

impl Fixture {
    fn on(&self) -> Value {
        json!({ "account_id": self.account, "bucket": BUCKET })
    }

    fn with(&self, key: &str, value: Value) -> Value {
        let mut args = self.on();
        args[key] = value;
        args
    }
}

#[test]
fn the_default_encryption_round_trips_in_the_contract_shape() {
    let f = fixture(|request| match request.method.as_str() {
        "GET" => MockResponse::xml(format!(
            "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>aws:kms</SSEAlgorithm><KMSMasterKeyID>alias/k</KMSMasterKeyID></ApplyServerSideEncryptionByDefault><BucketKeyEnabled>true</BucketKeyEnabled></Rule></ServerSideEncryptionConfiguration>"
        )),
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::empty(200),
    });
    let read = f.h.ok("get_bucket_encryption", f.on());
    assert_eq!(
        read,
        json!({ "algorithm": "aws:kms", "kms_key_id": "alias/k", "bucket_key_enabled": true })
    );

    let config =
        json!({ "algorithm": "aws:kms", "kms_key_id": "alias/new", "bucket_key_enabled": false });
    assert_eq!(
        f.h.ok("put_bucket_encryption", f.with("config", config)),
        Value::Null
    );
    let put = f.mock.requests().last().unwrap().clone();
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query, "encryption");
    assert!(
        put.body_text()
            .contains("<KMSMasterKeyID>alias/new</KMSMasterKeyID>")
    );

    assert_eq!(f.h.ok("delete_bucket_encryption", f.on()), Value::Null);
    assert_eq!(f.mock.requests().last().unwrap().method, "DELETE");
}

#[test]
fn no_encryption_is_null_and_bad_settings_are_usage_errors_without_a_request() {
    let f = fixture(|_| {
        MockResponse::error(
            404,
            "ServerSideEncryptionConfigurationNotFoundError",
            "not found",
        )
    });
    assert_eq!(f.h.ok("get_bucket_encryption", f.on()), Value::Null);
    let sent = f.mock.requests().len();
    for config in [
        json!({ "algorithm": "AES256", "kms_key_id": "alias/k", "bucket_key_enabled": false }),
        json!({ "algorithm": "AES256", "kms_key_id": null, "bucket_key_enabled": true }),
        json!({ "algorithm": "aws:kms", "kms_key_id": "a b", "bucket_key_enabled": false }),
        json!({ "algorithm": "aws:kms:dsse", "kms_key_id": null, "bucket_key_enabled": false }),
    ] {
        assert_eq!(
            f.h.code("put_bucket_encryption", f.with("config", config)),
            "usage"
        );
    }
    assert_eq!(f.mock.requests().len(), sent);
}

#[test]
fn bucket_tags_round_trip_and_an_empty_set_removes_them() {
    let f = fixture(|request| match request.method.as_str() {
        "GET" => MockResponse::xml(format!(
            "<Tagging {NS}><TagSet><Tag><Key>team</Key><Value>data</Value></Tag></TagSet></Tagging>"
        )),
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::empty(200),
    });
    assert_eq!(f.h.ok("get_bucket_tags", f.on()), json!({ "team": "data" }));
    f.h.ok(
        "put_bucket_tags",
        f.with("tags", json!({ "cost-center": "42" })),
    );
    let put = f.mock.requests().last().unwrap().clone();
    assert_eq!(
        (put.method.as_str(), put.query.as_str()),
        ("PUT", "tagging")
    );
    assert!(put.body_text().contains("<Key>cost-center</Key>"));

    f.h.ok("put_bucket_tags", f.with("tags", json!({})));
    assert_eq!(f.mock.requests().last().unwrap().method, "DELETE");
}

#[test]
fn invalid_bucket_tags_are_usage_errors() {
    let f = fixture(|_| MockResponse::empty(200));
    let too_many: serde_json::Map<String, Value> =
        (0..51).map(|n| (format!("k{n}"), json!("v"))).collect();
    for tags in [
        json!({ "aws:x": "v" }),
        json!({ "": "v" }),
        json!({ "k": "bad#value" }),
        Value::Object(too_many),
    ] {
        assert_eq!(f.h.code("put_bucket_tags", f.with("tags", tags)), "usage");
    }
    assert!(f.mock.requests().is_empty());
}

const OWNER: &str = "79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be";

fn acl_xml() -> String {
    format!(
        "<AccessControlPolicy {NS}><Owner><ID>{OWNER}</ID><DisplayName>me</DisplayName></Owner><AccessControlList><Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>{OWNER}</ID></Grantee><Permission>FULL_CONTROL</Permission></Grant></AccessControlList></AccessControlPolicy>"
    )
}

#[test]
fn the_bucket_acl_is_read_with_its_ownership_and_written_as_grants_or_canned() {
    let f = fixture(
        |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "acl") => MockResponse::xml(acl_xml()),
            ("GET", "ownershipControls") => MockResponse::xml(format!(
                "<OwnershipControls {NS}><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>"
            )),
            _ => MockResponse::empty(200),
        },
    );
    let read = f.h.ok("get_bucket_acl", f.on());
    assert_eq!(keys(&read), ["acl", "acls_disabled", "ownership"]);
    assert_eq!(read["acls_disabled"], true);
    assert_eq!(read["ownership"], "BucketOwnerEnforced");
    assert_eq!(read["acl"]["owner"], OWNER);
    assert_eq!(read["acl"]["canned"], "private");
    assert_eq!(
        read["acl"]["grants"],
        json!([{ "grantee_type": "canonical_user", "grantee": OWNER, "permission": "FULL_CONTROL" }])
    );

    let grants = json!([
        { "grantee_type": "canonical_user", "grantee": OWNER, "permission": "FULL_CONTROL" },
        { "grantee_type": "group", "grantee": "http://acs.amazonaws.com/groups/global/AllUsers", "permission": "READ" },
    ]);
    assert_eq!(
        f.h.ok("put_bucket_acl_grants", f.with("grants", grants.clone())),
        Value::Null
    );
    let put = f.mock.requests().last().unwrap().clone();
    assert_eq!((put.method.as_str(), put.query.as_str()), ("PUT", "acl"));
    assert!(put.body_text().contains("AllUsers"));

    assert_eq!(
        f.h.ok("put_object_acl_grants", {
            let mut args = f.with("grants", grants);
            args["key"] = json!("docs/a.txt");
            args
        }),
        Value::Null
    );
    assert!(
        f.mock
            .requests()
            .last()
            .unwrap()
            .path
            .ends_with("/docs/a.txt")
    );

    f.h.ok("set_bucket_acl", f.with("acl", json!("public-read")));
    assert_eq!(
        f.mock.requests().last().unwrap().header("x-amz-acl"),
        Some("public-read")
    );
}

#[test]
fn bad_acl_input_is_a_usage_error_and_disabled_acls_are_a_conflict() {
    let f = fixture(|request| {
        if request.method == "GET" {
            MockResponse::xml(acl_xml())
        } else {
            MockResponse::error(400, "AccessControlListNotSupported", "ACLs are off")
        }
    });
    assert_eq!(
        f.h.code("set_bucket_acl", f.with("acl", json!("bucket-owner-read"))),
        "usage"
    );
    assert_eq!(
        f.h.code("put_bucket_acl_grants", f.with("grants", json!([]))),
        "usage"
    );
    assert_eq!(
        f.h.code(
            "put_bucket_acl_grants",
            f.with(
                "grants",
                json!([{ "grantee_type": "group", "grantee": "http://example.com/x", "permission": "READ" }])
            )
        ),
        "usage"
    );
    let (code, message) = f.h.error(
        "put_bucket_acl_grants",
        f.with(
            "grants",
            json!([{ "grantee_type": "canonical_user", "grantee": OWNER, "permission": "FULL_CONTROL" }]),
        ),
    );
    assert_eq!(code, "conflict");
    assert!(message.contains("ACLs are disabled"), "{message}");
}

#[test]
fn a_bucket_is_created_with_advanced_options_and_the_warnings_come_back() {
    let f = fixture(|request| {
        if request.query == "publicAccessBlock" {
            MockResponse::error(501, "NotImplemented", "not implemented")
        } else {
            MockResponse::empty(200)
        }
    });
    let result = f.h.ok(
        "create_bucket",
        json!({
            "account_id": f.account,
            "name": "locked-bucket",
            "region": null,
            "options": {
                "object_lock": true,
                "ownership": "BucketOwnerEnforced",
                "public_access_block": {
                    "block_public_acls": true, "ignore_public_acls": true,
                    "block_public_policy": true, "restrict_public_buckets": true
                }
            }
        }),
    );
    assert_eq!(keys(&result), ["warnings"]);
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
    let requests = f.mock.requests();
    assert_eq!(
        requests[0].header("x-amz-bucket-object-lock-enabled"),
        Some("true")
    );
    assert_eq!(
        requests[0].header("x-amz-object-ownership"),
        Some("BucketOwnerEnforced")
    );
    assert_eq!(requests[1].query, "publicAccessBlock");

    // Without options (or with an empty object) only the bucket is created.
    for options in [Value::Null, json!({})] {
        let before = f.mock.requests().len();
        let result = f.h.ok(
            "create_bucket",
            json!({ "account_id": f.account, "name": "plain-bucket", "region": null, "options": options }),
        );
        assert_eq!(result, json!({ "warnings": [] }));
        assert_eq!(f.mock.requests().len(), before + 1);
    }
    assert_eq!(
        f.h.code(
            "create_bucket",
            json!({ "account_id": f.account, "name": "Bad_Name", "region": null,
                    "options": { "object_lock": true } })
        ),
        "usage"
    );
}

#[test]
fn an_unknown_account_is_refused() {
    let f = fixture(|_| MockResponse::empty(200));
    assert_eq!(
        f.h.code(
            "get_bucket_encryption",
            json!({ "account_id": "00000000-0000-4000-8000-000000000000", "bucket": BUCKET })
        ),
        "not_found"
    );
}
