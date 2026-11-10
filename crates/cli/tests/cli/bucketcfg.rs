//! `bb bucket encryption|tags`: default encryption and tags of a bucket,
//! against a scripted `MockS3` (s3s-fs implements neither API).

use bucketbridge_cli::i18n::{tr, trf};
use bucketbridge_tests::MockResponse;

use super::*;

const NS: &str = xml::NS;

const KMS_KEY: &str = "arn:aws:kms:eu-west-1:000000000000:key/abc";

fn encryption_body(algorithm: &str, key: Option<&str>, bucket_key: bool) -> String {
    let key = key.map_or_else(String::new, |k| {
        format!("<KMSMasterKeyID>{k}</KMSMasterKeyID>")
    });
    format!(
        "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>{algorithm}</SSEAlgorithm>{key}</ApplyServerSideEncryptionByDefault><BucketKeyEnabled>{bucket_key}</BucketKeyEnabled></Rule></ServerSideEncryptionConfiguration>"
    )
}

fn tags_body(tags: &[(&str, &str)]) -> String {
    let tags: String = tags
        .iter()
        .map(|(k, v)| format!("<Tag><Key>{k}</Key><Value>{v}</Value></Tag>"))
        .collect();
    format!("<Tagging {NS}><TagSet>{tags}</TagSet></Tagging>")
}

#[test]
fn encryption_get_prints_the_setting_or_none() {
    let mock =
        MockS3::start(|_| MockResponse::xml(encryption_body("aws:kms", Some(KMS_KEY), true)));
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "encryption", "get", "s3://b"]));
    out.expect_code(0);
    let described = trf(
        "{base}, bucket key",
        &[(
            "base",
            trf(
                "{base}, key {key}",
                &[("base", "SSE-KMS"), ("key", KMS_KEY)],
            )
            .as_str(),
        )],
    );
    assert_eq!(out.stdout.trim(), format!("s3://b\t{described}"));
    let out =
        run(bb_mock(&mock, config.path()).args(["--json", "bucket", "encryption", "get", "b"]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b",
            "encryption": { "algorithm": "aws:kms", "kms_key_id": KMS_KEY, "bucket_key_enabled": true }
        })]
    );

    let none = MockS3::start(|_| {
        MockResponse::error(
            404,
            "ServerSideEncryptionConfigurationNotFoundError",
            "not found",
        )
    });
    let out = run(bb_mock(&none, config.path()).args(["bucket", "encryption", "get", "b"]));
    out.expect_code(0);
    assert_eq!(out.stdout.trim(), format!("s3://b\t{}", tr("none")));
    let out =
        run(bb_mock(&none, config.path()).args(["--json", "bucket", "encryption", "get", "b"]));
    assert_eq!(
        out.records(),
        [serde_json::json!({ "bucket": "b", "encryption": null })]
    );
}

#[test]
fn encryption_set_sends_the_setting() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "bucket",
        "encryption",
        "set",
        "b",
        "--sse",
        "kms",
        "--kms-key",
        "alias/team",
        "--bucket-key",
    ]));
    out.expect_code(0);
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].query, "encryption");
    let body = requests[0].body_text();
    assert!(
        body.contains("<SSEAlgorithm>aws:kms</SSEAlgorithm>"),
        "{body}"
    );
    assert!(
        body.contains("<KMSMasterKeyID>alias/team</KMSMasterKeyID>"),
        "{body}"
    );
    assert!(
        body.contains("<BucketKeyEnabled>true</BucketKeyEnabled>"),
        "{body}"
    );
    assert_eq!(
        out.stdout.trim(),
        trf(
            "set the default encryption of {bucket} to {what}",
            &[
                ("bucket", "b"),
                (
                    "what",
                    trf(
                        "{base}, bucket key",
                        &[(
                            "base",
                            trf(
                                "{base}, key {key}",
                                &[("base", "SSE-KMS"), ("key", "alias/team")]
                            )
                            .as_str()
                        )]
                    )
                    .as_str()
                )
            ]
        )
    );

    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "bucket",
        "encryption",
        "set",
        "b",
        "--sse",
        "s3",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b",
            "encryption": { "algorithm": "AES256", "kms_key_id": null, "bucket_key_enabled": false }
        })]
    );
    assert!(
        mock.requests()[1]
            .body_text()
            .contains("<SSEAlgorithm>AES256</SSEAlgorithm>")
    );
}

#[test]
fn encryption_set_checks_the_options_before_sending() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let config = tempfile::tempdir().unwrap();
    for extra in [
        ["--kms-key", "alias/x"].as_slice(),
        ["--bucket-key"].as_slice(),
    ] {
        let mut args = vec!["bucket", "encryption", "set", "b", "--sse", "s3"];
        args.extend_from_slice(extra);
        run(bb_mock(&mock, config.path()).args(args)).expect_code(1);
    }
    run(bb_mock(&mock, config.path()).args(["bucket", "encryption", "set", "b"])).expect_code(1);
    run(bb_mock(&mock, config.path()).args([
        "bucket",
        "encryption",
        "set",
        "b",
        "--sse",
        "kms",
        "--kms-key",
        "a b",
    ]))
    .expect_code(1);
    assert!(mock.requests().is_empty());
}

#[test]
fn encryption_delete_removes_the_setting() {
    let mock = MockS3::start(|_| MockResponse::empty(204));
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "encryption", "delete", "b"]));
    out.expect_code(0);
    let requests = mock.requests();
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].query, "encryption");
    assert_eq!(
        out.stdout.trim(),
        trf(
            "removed the default encryption of {bucket}",
            &[("bucket", "b")]
        )
    );
}

#[test]
fn tags_get_prints_one_line_per_tag() {
    let mock =
        MockS3::start(|_| MockResponse::xml(tags_body(&[("team", "data"), ("env", "prod")])));
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "tags", "get", "b"]));
    out.expect_code(0);
    assert_eq!(out.lines(), ["env=prod", "team=data"]);
    let out = run(bb_mock(&mock, config.path()).args(["--json", "bucket", "tags", "get", "b"]));
    assert_eq!(
        out.records(),
        [
            serde_json::json!({ "bucket": "b", "key": "env", "value": "prod" }),
            serde_json::json!({ "bucket": "b", "key": "team", "value": "data" }),
        ]
    );
    let none = MockS3::start(|_| MockResponse::error(404, "NoSuchTagSet", "none"));
    let out = run(bb_mock(&none, config.path()).args(["bucket", "tags", "get", "b"]));
    out.expect_code(0);
    assert!(out.stdout.is_empty());
}

#[test]
fn tags_set_adds_to_the_existing_tags_unless_replacing() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" {
            MockResponse::xml(tags_body(&[("env", "prod"), ("team", "old")]))
        } else {
            MockResponse::empty(200)
        }
    });
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "bucket",
        "tags",
        "set",
        "b",
        "team=data",
        "cost-center=42",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b",
            "tags": { "cost-center": "42", "env": "prod", "team": "data" }
        })]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        requests[1].body_text(),
        format!(
            "<Tagging {NS}><TagSet><Tag><Key>cost-center</Key><Value>42</Value></Tag><Tag><Key>env</Key><Value>prod</Value></Tag><Tag><Key>team</Key><Value>data</Value></Tag></TagSet></Tagging>"
        )
    );

    let replace = MockS3::start(|_| MockResponse::empty(200));
    let out = run(bb_mock(&replace, config.path()).args([
        "bucket",
        "tags",
        "set",
        "b",
        "--replace",
        "only=this",
    ]));
    out.expect_code(0);
    let requests = replace.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].body_text().contains("<Key>only</Key>"));
    assert_eq!(
        out.stdout.trim(),
        trf(
            "set {count} tags on {bucket}",
            &[("count", "1"), ("bucket", "b")]
        )
    );
}

#[test]
fn tags_set_rejects_invalid_tags_without_a_request() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let config = tempfile::tempdir().unwrap();
    for bad in ["aws:x=1", "novalue", "k=bad#value"] {
        run(bb_mock(&mock, config.path()).args(["bucket", "tags", "set", "b", bad])).expect_code(1);
    }
    assert!(mock.requests().is_empty());
}

#[test]
fn tags_delete_removes_named_tags_or_all() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" => MockResponse::xml(tags_body(&[("a", "1"), ("b", "2")])),
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::empty(200),
    });
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "tags", "delete", "b", "a"]));
    out.expect_code(0);
    let requests = mock.requests();
    assert_eq!(requests[1].method, "PUT");
    let body = requests[1].body_text();
    assert!(
        body.contains("<Key>b</Key>") && !body.contains("<Key>a</Key>"),
        "{body}"
    );

    let all = MockS3::start(|_| MockResponse::empty(204));
    let out = run(bb_mock(&all, config.path()).args(["bucket", "tags", "delete", "b"]));
    out.expect_code(0);
    let requests = all.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].query, "tagging");
    assert_eq!(
        out.stdout.trim(),
        trf("removed every tag of {bucket}", &[("bucket", "b")])
    );
}

// ---- ACL grants ----

const OWNER: &str = "79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be";
const ALL_USERS: &str = "http://acs.amazonaws.com/groups/global/AllUsers";

fn acl_body(extra: &str) -> String {
    format!(
        "<AccessControlPolicy {NS}><Owner><ID>{OWNER}</ID><DisplayName>me</DisplayName></Owner><AccessControlList><Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>{OWNER}</ID></Grantee><Permission>FULL_CONTROL</Permission></Grant>{extra}</AccessControlList></AccessControlPolicy>"
    )
}

const PUBLIC_READ: &str = "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"Group\"><URI>http://acs.amazonaws.com/groups/global/AllUsers</URI></Grantee><Permission>READ</Permission></Grant>";

/// Serves the ACL (with `extra` grants) and no ownership setting.
fn acl_server(extra: &'static str) -> MockS3 {
    MockS3::start(
        move |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "acl") => MockResponse::xml(acl_body(extra)),
            ("GET", "ownershipControls") => {
                MockResponse::error(404, "OwnershipControlsNotFoundError", "none")
            }
            _ => MockResponse::empty(200),
        },
    )
}

#[test]
fn bucket_acl_get_prints_the_canned_name_or_the_grants() {
    let config = tempfile::tempdir().unwrap();
    let mock = acl_server("");
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "acl", "get", "b"]));
    out.expect_code(0);
    assert_eq!(out.stdout.trim(), "s3://b\tprivate");

    let public = acl_server(PUBLIC_READ);
    let out = run(bb_mock(&public, config.path()).args(["--json", "bucket", "acl", "get", "b"]));
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["acl"], "public-read");
    assert_eq!(record["acls_disabled"], false);
    assert_eq!(record["ownership"], serde_json::Value::Null);
    assert_eq!(record["grants"][1]["grantee"], ALL_USERS);

    let custom = acl_server(
        "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"AmazonCustomerByEmail\"><EmailAddress>a@example.com</EmailAddress></Grantee><Permission>WRITE</Permission></Grant>",
    );
    let out = run(bb_mock(&custom, config.path()).args(["bucket", "acl", "get", "b"]));
    assert_eq!(
        out.stdout.trim(),
        format!("s3://b\tcustom\tFULL_CONTROL:id:{OWNER},WRITE:email:a@example.com")
    );
}

#[test]
fn bucket_acl_get_says_when_acls_are_disabled() {
    let config = tempfile::tempdir().unwrap();
    let mock = MockS3::start(
        |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "acl") => MockResponse::xml(acl_body("")),
            _ => MockResponse::xml(format!(
                "<OwnershipControls {NS}><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>"
            )),
        },
    );
    let out = run(bb_mock(&mock, config.path()).args(["bucket", "acl", "get", "b"]));
    out.expect_code(0);
    assert!(
        out.stdout
            .contains(&tr("ACLs are disabled (BucketOwnerEnforced)")),
        "{}",
        out.stdout
    );
}

#[test]
fn bucket_acl_set_sends_the_canned_acl_and_refuses_object_only_ones() {
    let config = tempfile::tempdir().unwrap();
    let mock = acl_server("");
    let out = run(bb_mock(&mock, config.path()).args([
        "bucket",
        "acl",
        "set",
        "b",
        "authenticated-read",
    ]));
    out.expect_code(0);
    let put = mock.requests().last().unwrap().clone();
    assert_eq!(put.method, "PUT");
    assert_eq!(put.header("x-amz-acl"), Some("authenticated-read"));
    let before = mock.requests().len();
    run(bb_mock(&mock, config.path()).args(["bucket", "acl", "set", "b", "bucket-owner-read"]))
        .expect_code(1);
    run(bb_mock(&mock, config.path()).args(["bucket", "acl", "set", "b", "nonsense"]))
        .expect_code(1);
    assert_eq!(mock.requests().len(), before);
}

#[test]
fn bucket_acl_grant_and_revoke_change_one_grant() {
    let config = tempfile::tempdir().unwrap();
    let mock = acl_server("");
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "bucket",
        "acl",
        "grant",
        "b",
        "read",
        "group=all-users",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b", "key": null, "action": "grant", "permission": "READ",
            "grantee_type": "group", "grantee": ALL_USERS, "changed": true
        })]
    );
    let put = mock.requests().last().unwrap().clone();
    assert_eq!(put.method, "PUT");
    let body = put.body_text();
    assert!(
        body.contains(ALL_USERS) && body.contains(&format!("<ID>{OWNER}</ID>")),
        "{body}"
    );

    // Granting what is there changes nothing.
    let public = acl_server(PUBLIC_READ);
    let out = run(bb_mock(&public, config.path()).args([
        "--json",
        "bucket",
        "acl",
        "grant",
        "b",
        "READ",
        "group=all-users",
    ]));
    assert_eq!(out.records()[0]["changed"], false);
    assert!(public.requests().iter().all(|r| r.method == "GET"));

    // Revoking removes it and keeps the owner.
    let out = run(bb_mock(&public, config.path()).args([
        "bucket",
        "acl",
        "revoke",
        "b",
        "READ",
        "group=all-users",
    ]));
    out.expect_code(0);
    let put = public.requests().last().unwrap().clone();
    assert_eq!(put.method, "PUT");
    assert!(!put.body_text().contains(ALL_USERS));
    assert!(put.body_text().contains("FULL_CONTROL"));
}

#[test]
fn acl_grant_and_revoke_work_on_one_object() {
    let config = tempfile::tempdir().unwrap();
    let mock = acl_server("");
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "acl",
        "grant",
        "s3://b/docs/a.txt",
        "READ_ACP",
        "email=dev@example.com",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b", "key": "docs/a.txt", "action": "grant", "permission": "READ_ACP",
            "grantee_type": "email", "grantee": "dev@example.com", "changed": true
        })]
    );
    let put = mock.requests().last().unwrap().clone();
    assert!(put.path.ends_with("/docs/a.txt"), "{}", put.path);
    assert!(
        put.body_text()
            .contains("<EmailAddress>dev@example.com</EmailAddress>")
    );

    let none = acl_server("");
    let out =
        run(bb_mock(&none, config.path()).args(["acl", "revoke", "s3://b/k", "READ", "id=nobody"]));
    out.expect_code(0);
    assert!(none.requests().iter().all(|r| r.method == "GET"));
    assert!(out.stdout.contains(&trf(
        "{target} has no grant {what}",
        &[("target", "s3://b/k"), ("what", "READ:id:nobody")]
    )));
}

#[test]
fn acl_grants_check_their_arguments_and_the_target() {
    let config = tempfile::tempdir().unwrap();
    let mock = acl_server("");
    for args in [
        vec!["acl", "grant", "s3://b/", "READ", "id=x"],
        vec!["acl", "grant", "s3://b/k", "EXECUTE", "id=x"],
        vec!["acl", "grant", "s3://b/k", "READ", "someone"],
        vec!["acl", "grant", "s3://b/k", "READ", "group=everybody"],
        vec!["bucket", "acl", "grant", "b", "READ", "email=nobody"],
    ] {
        run(bb_mock(&mock, config.path()).args(args.clone())).expect_code(1);
    }
    // Disabled ACLs are a conflict that says so.
    let disabled =
        MockS3::start(
            |request| match (request.method.as_str(), request.query.as_str()) {
                ("GET", "acl") => MockResponse::xml(acl_body("")),
                ("GET", _) => MockResponse::error(404, "OwnershipControlsNotFoundError", "none"),
                _ => MockResponse::error(400, "AccessControlListNotSupported", "ACLs off"),
            },
        );
    let out = run(bb_mock(&disabled, config.path()).args([
        "bucket",
        "acl",
        "grant",
        "b",
        "READ",
        "group=all-users",
    ]));
    out.expect_code(2);
    assert!(out.stderr.contains("ACLs are disabled"), "{}", out.stderr);
}

// ---- bb mb options ----

#[test]
fn mb_sends_object_lock_ownership_and_the_public_access_block() {
    let config = tempfile::tempdir().unwrap();
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "mb",
        "locked-bucket",
        "--object-lock",
        "--ownership",
        "bucket-owner-preferred",
        "--public-access-block",
        "on",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "locked-bucket", "region": null, "object_lock": true,
            "ownership": "BucketOwnerPreferred", "public_access_block": true
        })]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[0].header("x-amz-bucket-object-lock-enabled"),
        Some("true")
    );
    assert_eq!(
        requests[0].header("x-amz-object-ownership"),
        Some("BucketOwnerPreferred")
    );
    assert_eq!(requests[1].query, "publicAccessBlock");
    let body = requests[1].body_text();
    assert_eq!(body.matches(">true<").count(), 4, "{body}");

    // `off` sends four false switches.
    let off = MockS3::start(|_| MockResponse::empty(200));
    run(bb_mock(&off, config.path()).args(["mb", "open-bucket", "--public-access-block", "off"]))
        .expect_code(0);
    assert_eq!(off.requests()[1].body_text().matches(">false<").count(), 4);
}

#[test]
fn mb_without_options_sends_one_request_and_an_unsupported_block_is_a_warning() {
    let config = tempfile::tempdir().unwrap();
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let out = run(bb_mock(&mock, config.path()).args(["mb", "plain-bucket"]));
    out.expect_code(0);
    assert_eq!(mock.requests().len(), 1);

    let unsupported = MockS3::start(|request| {
        if request.query == "publicAccessBlock" {
            MockResponse::error(501, "NotImplemented", "no")
        } else {
            MockResponse::empty(200)
        }
    });
    let out = run(bb_mock(&unsupported, config.path()).args([
        "--json",
        "mb",
        "custom-bucket",
        "--public-access-block",
        "on",
    ]));
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["warnings"].as_array().unwrap().len(), 1);
    assert!(
        out.stderr.is_empty(),
        "json mode keeps stderr for errors: {}",
        out.stderr
    );
    let out = run(bb_mock(&unsupported, config.path()).args([
        "mb",
        "custom-bucket-2",
        "--public-access-block",
        "on",
    ]));
    out.expect_code(0);
    assert!(out.stderr.contains("public access block"), "{}", out.stderr);
}

#[test]
fn mb_rejects_bad_option_values() {
    let config = tempfile::tempdir().unwrap();
    let mock = MockS3::start(|_| MockResponse::empty(200));
    for args in [
        vec!["mb", "some-bucket", "--ownership", "everyone"],
        vec!["mb", "some-bucket", "--public-access-block", "maybe"],
    ] {
        run(bb_mock(&mock, config.path()).args(args)).expect_code(1);
    }
    assert!(mock.requests().is_empty());
}

// ---- bb bucket lock ----

fn lock_bucket(versioning: &'static str, lock_on: bool) -> MockS3 {
    MockS3::start(
        move |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "object-lock") if lock_on => MockResponse::xml(format!(
                "<ObjectLockConfiguration {NS}><ObjectLockEnabled>Enabled</ObjectLockEnabled><Rule><DefaultRetention><Mode>COMPLIANCE</Mode><Years>2</Years></DefaultRetention></Rule></ObjectLockConfiguration>"
            )),
            ("GET", "object-lock") => {
                MockResponse::error(404, "ObjectLockConfigurationNotFoundError", "none")
            }
            ("GET", "versioning") => MockResponse::xml(format!(
                "<VersioningConfiguration {NS}><Status>{versioning}</Status></VersioningConfiguration>"
            )),
            _ => MockResponse::empty(200),
        },
    )
}

#[test]
fn bucket_lock_get_prints_the_state() {
    let config = tempfile::tempdir().unwrap();
    let on = lock_bucket("Enabled", true);
    let out = run(bb_mock(&on, config.path()).args(["bucket", "lock", "get", "b"]));
    out.expect_code(0);
    let retention = trf(
        "default retention {mode} {amount} {unit}",
        &[
            ("mode", "COMPLIANCE"),
            ("amount", "2"),
            ("unit", &tr("years")),
        ],
    );
    assert_eq!(
        out.stdout.trim(),
        format!("s3://b\t{}\t{retention}", tr("on"))
    );
    let out = run(bb_mock(&on, config.path()).args(["--json", "bucket", "lock", "get", "b"]));
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "b", "enabled": true,
            "default_retention": { "mode": "compliance", "days": null, "years": 2 }
        })]
    );
    let off = lock_bucket("Enabled", false);
    let out = run(bb_mock(&off, config.path()).args(["bucket", "lock", "get", "b"]));
    assert_eq!(out.stdout.trim(), format!("s3://b\t{}", tr("off")));
}

#[test]
fn bucket_lock_enable_needs_yes_and_versioning() {
    let config = tempfile::tempdir().unwrap();
    // Without --yes nothing is sent.
    let mock = lock_bucket("Enabled", false);
    run(bb_mock(&mock, config.path()).args(["bucket", "lock", "enable", "b"])).expect_code(1);
    assert!(mock.requests().is_empty());

    let out =
        run(bb_mock(&mock, config.path())
            .args(["--json", "bucket", "lock", "enable", "b", "--yes"]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({ "bucket": "b", "enabled": true, "changed": true })]
    );
    let put = mock.requests().last().unwrap().clone();
    assert_eq!(
        (put.method.as_str(), put.query.as_str()),
        ("PUT", "object-lock")
    );

    // Without versioning it is a conflict that says so, and nothing is written.
    let unversioned = lock_bucket("Suspended", false);
    let out =
        run(bb_mock(&unversioned, config.path()).args(["bucket", "lock", "enable", "b", "--yes"]));
    out.expect_code(2);
    assert!(out.stderr.contains("needs versioning"), "{}", out.stderr);
    assert!(unversioned.requests().iter().all(|r| r.method == "GET"));

    // Already on: reported, nothing written.
    let on = lock_bucket("Enabled", true);
    let out = run(bb_mock(&on, config.path()).args(["bucket", "lock", "enable", "b", "--yes"]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf("Object Lock is already on for {bucket}", &[("bucket", "b")])
    );
    assert!(on.requests().iter().all(|r| r.method == "GET"));
}
