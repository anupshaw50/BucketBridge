//! Bucket configuration (`bucketbridge_core::bucketcfg`): policy, public
//! access block, versioning, server access logging, website hosting and
//! transfer acceleration, plus the external buckets of an account.
//!
//! The round trips run against [`BucketConfigMock`], a stateful emulation on
//! top of the recorded [`MockS3`], because s3s-fs implements none of these
//! APIs; every test asserts the exact requests that were sent (method, path,
//! subresource, body, integrity header) as well as what comes back. The tests
//! that need a real server for the APIs MinIO supports (policy and
//! versioning) are ignored unless the `minio` feature is enabled (see
//! docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::bucketcfg::{
    AccelerationStatus, BucketVersioning, LoggingTarget, PublicAccessBlock, RedirectProtocol,
    RoutingCondition, RoutingRedirect, RoutingRule, Section, VersioningStatus, WebsiteConfig,
    check_policy, format_policy,
};
use bucketbridge_core::model::{Bucket, BucketKind};
use bucketbridge_core::{BbError, ErrorKind};
use bucketbridge_tests::{
    BucketConfigMock, ConfigKind, MockResponse, MockS3, RecordedRequest, TestS3,
};
use md5::{Digest, Md5};
use serde_json::{Value, json};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

const POLICY: &str = r#"{
    "Version": "2012-10-17",
    "Statement": [
        {
            "Sid": "PublicRead",
            "Effect": "Allow",
            "Principal": "*",
            "Action": ["s3:GetObject"],
            "Resource": "arn:aws:s3:::site/*"
        }
    ]
}"#;

const POLICY_COMPACT: &str = r#"{"Version":"2012-10-17","Statement":[{"Sid":"PublicRead","Effect":"Allow","Principal":"*","Action":["s3:GetObject"],"Resource":"arn:aws:s3:::site/*"}]}"#;

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The request `n` positions from the end, which must address `subresource`
/// of `bucket` with `method`.
fn last_request(
    server: &BucketConfigMock,
    from_end: usize,
    method: &str,
    bucket: &str,
    subresource: &str,
) -> RecordedRequest {
    let requests = server.requests();
    let request = requests[requests.len() - 1 - from_end].clone();
    assert_eq!(request.method, method, "{request:?}");
    assert_eq!(request.path, format!("/{bucket}/"), "{request:?}");
    assert_eq!(request.query, subresource, "{request:?}");
    request
}

/// Asserts the integrity header S3 requires on a write: `Content-MD5` of the
/// body.
fn assert_content_md5(request: &RecordedRequest) {
    assert_eq!(
        request.header("content-md5"),
        Some(base64(&Md5::digest(&request.body)).as_str()),
        "{request:?}"
    );
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// The error of a section that failed.
fn failure<T: std::fmt::Debug>(section: &Section<T>) -> &BbError {
    match section {
        Section::Failed { error } => error,
        other => panic!("expected a failed section, got {other:?}"),
    }
}

// ---- bucket policy ----

#[tokio::test]
async fn the_bucket_policy_round_trips_and_is_sent_without_whitespace() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();

    // Nothing yet: "not configured" is an answer, not an error.
    assert_eq!(s3.get_bucket_policy("site").await.unwrap(), None);
    let get = last_request(&server, 0, "GET", "site", "policy");
    assert_eq!(get.header("x-amz-request-payer"), None);

    s3.put_bucket_policy("site", POLICY).await.unwrap();
    let put = last_request(&server, 0, "PUT", "site", "policy");
    assert_eq!(put.body_text(), POLICY_COMPACT);
    assert_content_md5(&put);
    assert_eq!(
        server.stored("site", ConfigKind::Policy).unwrap(),
        POLICY_COMPACT
    );

    // Read back pretty-printed, with the keys in the order they were written.
    let read = s3.get_bucket_policy("site").await.unwrap().unwrap();
    assert_eq!(read, format_policy(POLICY).unwrap());
    assert!(read.starts_with("{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": ["));
    assert_eq!(json_of(&read), json_of(POLICY));

    // Edit what was read and write it back: the stored policy changes.
    let edited = read.replace("Allow", "Deny");
    s3.put_bucket_policy("site", &edited).await.unwrap();
    let again = s3.get_bucket_policy("site").await.unwrap().unwrap();
    assert_eq!(json_of(&again)["Statement"][0]["Effect"], "Deny");
    assert_eq!(json_of(&again)["Statement"][0]["Sid"], "PublicRead");

    s3.delete_bucket_policy("site").await.unwrap();
    let delete = last_request(&server, 0, "DELETE", "site", "policy");
    assert!(delete.body.is_empty());
    assert_eq!(s3.get_bucket_policy("site").await.unwrap(), None);
    assert_eq!(server.stored("site", ConfigKind::Policy), None);
    // Deleting what is not there is fine.
    s3.delete_bucket_policy("site").await.unwrap();
}

#[tokio::test]
async fn a_policy_that_is_not_valid_is_never_sent() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    for (policy, expected) in [
        ("{", "not valid JSON"),
        ("", "the policy is empty"),
        (
            r#"{"Version":"2012-10-17","Statement":[]}"#,
            "Statement: must contain at least one statement",
        ),
        (
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Maybe","Principal":"*","Action":"s3:*","Resource":"*"}]}"#,
            "Statement[0].Effect: must be \"Allow\" or \"Deny\" (found \"Maybe\")",
        ),
    ] {
        let error = s3.put_bucket_policy("site", policy).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{policy}");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!check_policy(policy).is_empty());
    }
    assert!(server.requests().is_empty(), "{:?}", server.requests());
    assert_eq!(server.stored("site", ConfigKind::Policy), None);
}

#[tokio::test]
async fn a_policy_over_the_size_limit_is_refused_before_it_is_sent() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let policy = json!({"Version": "2012-10-17", "Statement": [{
        "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject",
        "Resource": format!("arn:aws:s3:::site/{}", "k".repeat(21 * 1024))
    }]})
    .to_string();
    let error = server
        .ops()
        .put_bucket_policy("site", &policy)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(
        error
            .to_string()
            .contains("S3 accepts at most 20480 bytes (20 KiB)"),
        "{error}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_missing_bucket_is_not_mistaken_for_a_missing_configuration() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    let error = s3.get_bucket_policy("nope").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
    assert!(
        error
            .to_string()
            .starts_with("reading the policy of bucket 'nope': "),
        "{error}"
    );
    assert!(error.to_string().contains("NoSuchBucket"), "{error}");
    for error in [
        s3.get_public_access_block("nope").await.unwrap_err(),
        s3.get_bucket_website("nope").await.unwrap_err(),
        s3.delete_bucket_policy("nope").await.unwrap_err(),
        s3.delete_public_access_block("nope").await.unwrap_err(),
        s3.delete_bucket_website("nope").await.unwrap_err(),
    ] {
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
    }
}

#[tokio::test]
async fn access_denied_is_an_error_with_context() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.deny("site", ConfigKind::Policy);
    server.deny("site", ConfigKind::Website);
    let s3 = server.ops();
    let error = s3.get_bucket_policy("site").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    let text = error.to_string();
    assert!(
        text.starts_with("reading the policy of bucket 'site': Access Denied"),
        "{text}"
    );
    assert!(
        text.contains("AccessDenied, HTTP 403, request id MOCKREQUEST1"),
        "{text}"
    );
    let error = s3.put_bucket_policy("site", POLICY).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    assert!(
        error
            .to_string()
            .starts_with("saving the policy of bucket 'site': "),
        "{error}"
    );
    assert_eq!(
        s3.delete_bucket_policy("site").await.unwrap_err().kind(),
        ErrorKind::Auth
    );
    assert_eq!(
        s3.get_bucket_website("site").await.unwrap_err().kind(),
        ErrorKind::Auth
    );
}

#[tokio::test]
async fn deleting_a_missing_configuration_succeeds_on_servers_that_answer_404() {
    let mock = MockS3::start(|request| {
        if request.method != "DELETE" {
            return MockResponse::error(400, "UnexpectedRequest", "unexpected");
        }
        let code = if request.query_param("policy").is_some() {
            "NoSuchBucketPolicy"
        } else if request.query_param("publicAccessBlock").is_some() {
            "NoSuchPublicAccessBlockConfiguration"
        } else {
            "NoSuchWebsiteConfiguration"
        };
        MockResponse::error(404, code, "nothing to delete")
    });
    let s3 = mock.ops();
    s3.delete_bucket_policy("site").await.unwrap();
    s3.delete_public_access_block("site").await.unwrap();
    s3.delete_bucket_website("site").await.unwrap();
    assert_eq!(mock.requests().len(), 3);
}

// ---- public access block ----

#[tokio::test]
async fn the_public_access_block_round_trips_with_four_toggles() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    assert_eq!(s3.get_public_access_block("site").await.unwrap(), None);

    let block = PublicAccessBlock {
        block_public_acls: true,
        ignore_public_acls: false,
        block_public_policy: true,
        restrict_public_buckets: false,
    };
    s3.put_public_access_block("site", &block).await.unwrap();
    let put = last_request(&server, 0, "PUT", "site", "publicAccessBlock");
    assert_eq!(
        put.body_text(),
        format!(
            "<PublicAccessBlockConfiguration {NS}><BlockPublicAcls>true</BlockPublicAcls><IgnorePublicAcls>false</IgnorePublicAcls><BlockPublicPolicy>true</BlockPublicPolicy><RestrictPublicBuckets>false</RestrictPublicBuckets></PublicAccessBlockConfiguration>"
        )
    );
    assert_content_md5(&put);
    assert_eq!(
        s3.get_public_access_block("site").await.unwrap(),
        Some(block)
    );

    // All four are sent, so switching everything off is explicit too.
    let none = PublicAccessBlock::default();
    s3.put_public_access_block("site", &none).await.unwrap();
    assert!(
        last_request(&server, 0, "PUT", "site", "publicAccessBlock")
            .body_text()
            .contains("<RestrictPublicBuckets>false</RestrictPublicBuckets>")
    );
    assert_eq!(
        s3.get_public_access_block("site").await.unwrap(),
        Some(none)
    );

    s3.delete_public_access_block("site").await.unwrap();
    last_request(&server, 0, "DELETE", "site", "publicAccessBlock");
    assert_eq!(s3.get_public_access_block("site").await.unwrap(), None);
}

#[tokio::test]
async fn a_public_access_block_with_missing_toggles_reads_them_as_off() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.store(
        "site",
        ConfigKind::PublicAccessBlock,
        &format!(
            "<PublicAccessBlockConfiguration {NS}><RestrictPublicBuckets>true</RestrictPublicBuckets></PublicAccessBlockConfiguration>"
        ),
    );
    assert_eq!(
        server.ops().get_public_access_block("site").await.unwrap(),
        Some(PublicAccessBlock {
            restrict_public_buckets: true,
            ..PublicAccessBlock::default()
        })
    );
}

// ---- versioning ----

#[tokio::test]
async fn versioning_round_trips_and_never_sends_mfa_delete() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    assert_eq!(
        s3.get_bucket_versioning("site").await.unwrap(),
        BucketVersioning {
            status: VersioningStatus::Unversioned,
            mfa_delete: false
        }
    );

    s3.put_bucket_versioning("site", VersioningStatus::Enabled)
        .await
        .unwrap();
    let put = last_request(&server, 0, "PUT", "site", "versioning");
    assert_eq!(
        put.body_text(),
        format!("<VersioningConfiguration {NS}><Status>Enabled</Status></VersioningConfiguration>")
    );
    assert!(!put.body_text().contains("MfaDelete"));
    assert_eq!(put.header("x-amz-mfa"), None);
    assert_content_md5(&put);
    assert_eq!(
        s3.get_bucket_versioning("site").await.unwrap().status,
        VersioningStatus::Enabled
    );

    s3.put_bucket_versioning("site", VersioningStatus::Suspended)
        .await
        .unwrap();
    assert!(
        last_request(&server, 0, "PUT", "site", "versioning")
            .body_text()
            .contains("<Status>Suspended</Status>")
    );
    assert_eq!(
        s3.get_bucket_versioning("site").await.unwrap().status,
        VersioningStatus::Suspended
    );

    // S3 cannot go back to "never versioned": refused before a request.
    let before = server.requests().len();
    let error = s3
        .put_bucket_versioning("site", VersioningStatus::Unversioned)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert_eq!(server.requests().len(), before);
}

#[tokio::test]
async fn mfa_delete_is_read_but_not_changed() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.store(
        "site",
        ConfigKind::Versioning,
        &format!(
            "<VersioningConfiguration {NS}><Status>Enabled</Status><MfaDelete>Enabled</MfaDelete></VersioningConfiguration>"
        ),
    );
    assert_eq!(
        server.ops().get_bucket_versioning("site").await.unwrap(),
        BucketVersioning {
            status: VersioningStatus::Enabled,
            mfa_delete: true
        }
    );
    server.store(
        "site",
        ConfigKind::Versioning,
        &format!(
            "<VersioningConfiguration {NS}><Status>Suspended</Status><MfaDelete>Disabled</MfaDelete></VersioningConfiguration>"
        ),
    );
    assert_eq!(
        server.ops().get_bucket_versioning("site").await.unwrap(),
        BucketVersioning {
            status: VersioningStatus::Suspended,
            mfa_delete: false
        }
    );
}

// ---- server access logging ----

#[tokio::test]
async fn logging_round_trips_and_can_be_turned_off() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    assert_eq!(s3.get_bucket_logging("site").await.unwrap(), None);

    let target = LoggingTarget {
        target_bucket: "logs".into(),
        target_prefix: "site/".into(),
    };
    s3.put_bucket_logging("site", &target).await.unwrap();
    // The current configuration is read first (to keep what is not edited),
    // then the new one is written.
    let put = last_request(&server, 0, "PUT", "site", "logging");
    last_request(&server, 1, "GET", "site", "logging");
    assert_eq!(
        put.body_text(),
        format!(
            "<BucketLoggingStatus {NS}><LoggingEnabled><TargetBucket>logs</TargetBucket><TargetPrefix>site/</TargetPrefix></LoggingEnabled></BucketLoggingStatus>"
        )
    );
    assert_content_md5(&put);
    assert_eq!(s3.get_bucket_logging("site").await.unwrap(), Some(target));

    // An empty prefix is a prefix too.
    let bare = LoggingTarget {
        target_bucket: "logs".into(),
        target_prefix: String::new(),
    };
    s3.put_bucket_logging("site", &bare).await.unwrap();
    assert!(
        last_request(&server, 0, "PUT", "site", "logging")
            .body_text()
            .contains("<TargetPrefix></TargetPrefix>")
    );
    assert_eq!(s3.get_bucket_logging("site").await.unwrap(), Some(bare));

    s3.disable_bucket_logging("site").await.unwrap();
    let put = last_request(&server, 0, "PUT", "site", "logging");
    assert_eq!(
        put.body_text(),
        format!("<BucketLoggingStatus {NS}></BucketLoggingStatus>")
    );
    assert_content_md5(&put);
    assert_eq!(s3.get_bucket_logging("site").await.unwrap(), None);
}

#[tokio::test]
async fn changing_the_log_target_keeps_the_grants_and_the_key_format() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.store(
        "site",
        ConfigKind::Logging,
        &format!(
            "<BucketLoggingStatus {NS}><LoggingEnabled><TargetBucket>old-logs</TargetBucket><TargetGrants><Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be</ID></Grantee><Permission>READ</Permission></Grant></TargetGrants><TargetPrefix>old/</TargetPrefix><TargetObjectKeyFormat><PartitionedPrefix><PartitionDateSource>EventTime</PartitionDateSource></PartitionedPrefix></TargetObjectKeyFormat></LoggingEnabled></BucketLoggingStatus>"
        ),
    );
    let s3 = server.ops();
    assert_eq!(
        s3.get_bucket_logging("site").await.unwrap(),
        Some(LoggingTarget {
            target_bucket: "old-logs".into(),
            target_prefix: "old/".into()
        })
    );
    s3.put_bucket_logging(
        "site",
        &LoggingTarget {
            target_bucket: "new-logs".into(),
            target_prefix: "new/".into(),
        },
    )
    .await
    .unwrap();
    let body = last_request(&server, 0, "PUT", "site", "logging").body_text();
    for expected in [
        "<TargetBucket>new-logs</TargetBucket>",
        "<TargetPrefix>new/</TargetPrefix>",
        "<ID>79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be</ID>",
        "<Permission>READ</Permission>",
        "<PartitionedPrefix><PartitionDateSource>EventTime</PartitionDateSource></PartitionedPrefix>",
    ] {
        assert!(body.contains(expected), "{expected}\n{body}");
    }
    assert!(
        !body.contains("old-logs") && !body.contains("<TargetPrefix>old/"),
        "{body}"
    );
    assert_eq!(
        s3.get_bucket_logging("site")
            .await
            .unwrap()
            .unwrap()
            .target_bucket,
        "new-logs"
    );
}

#[tokio::test]
async fn a_log_target_that_cannot_be_read_is_not_overwritten_blindly() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.deny("site", ConfigKind::Logging);
    let error = server
        .ops()
        .put_bucket_logging(
            "site",
            &LoggingTarget {
                target_bucket: "logs".into(),
                target_prefix: String::new(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    assert!(
        error
            .to_string()
            .starts_with("reading the access logging of bucket 'site': "),
        "{error}"
    );
    // Only the read was attempted.
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.stored("site", ConfigKind::Logging), None);
}

// ---- static website hosting ----

fn docs_rules() -> Vec<RoutingRule> {
    vec![
        RoutingRule {
            condition: Some(RoutingCondition {
                key_prefix_equals: Some("docs/".into()),
                http_error_code_returned_equals: None,
            }),
            redirect: RoutingRedirect {
                replace_key_prefix_with: Some("documents/".into()),
                ..RoutingRedirect::default()
            },
        },
        RoutingRule {
            condition: Some(RoutingCondition {
                key_prefix_equals: None,
                http_error_code_returned_equals: Some("404".into()),
            }),
            redirect: RoutingRedirect {
                protocol: Some(RedirectProtocol::Https),
                host_name: Some("example.com".into()),
                replace_key_with: Some("not-found.html".into()),
                http_redirect_code: Some("307".into()),
                ..RoutingRedirect::default()
            },
        },
    ]
}

#[tokio::test]
async fn website_hosting_round_trips_with_routing_rules() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    assert_eq!(s3.get_bucket_website("site").await.unwrap(), None);

    let website = WebsiteConfig::Hosting {
        index_document: "index.html".into(),
        error_document: Some("error.html".into()),
        routing_rules: docs_rules(),
    };
    s3.put_bucket_website("site", &website).await.unwrap();
    let put = last_request(&server, 0, "PUT", "site", "website");
    assert_eq!(
        put.body_text(),
        format!(
            "<WebsiteConfiguration {NS}><ErrorDocument><Key>error.html</Key></ErrorDocument><IndexDocument><Suffix>index.html</Suffix></IndexDocument><RoutingRules>\
<RoutingRule><Condition><KeyPrefixEquals>docs/</KeyPrefixEquals></Condition><Redirect><ReplaceKeyPrefixWith>documents/</ReplaceKeyPrefixWith></Redirect></RoutingRule>\
<RoutingRule><Condition><HttpErrorCodeReturnedEquals>404</HttpErrorCodeReturnedEquals></Condition><Redirect><HostName>example.com</HostName><HttpRedirectCode>307</HttpRedirectCode><Protocol>https</Protocol><ReplaceKeyWith>not-found.html</ReplaceKeyWith></Redirect></RoutingRule>\
</RoutingRules></WebsiteConfiguration>"
        )
    );
    assert_content_md5(&put);
    assert_eq!(s3.get_bucket_website("site").await.unwrap(), Some(website));

    // Without an error document or rules, and with a rule that has no condition.
    let plain = WebsiteConfig::Hosting {
        index_document: "home.html".into(),
        error_document: None,
        routing_rules: vec![RoutingRule {
            condition: None,
            redirect: RoutingRedirect {
                http_redirect_code: Some("301".into()),
                host_name: Some("example.org".into()),
                ..RoutingRedirect::default()
            },
        }],
    };
    s3.put_bucket_website("site", &plain).await.unwrap();
    let body = last_request(&server, 0, "PUT", "site", "website").body_text();
    assert!(
        !body.contains("ErrorDocument") && !body.contains("<Condition>"),
        "{body}"
    );
    assert_eq!(s3.get_bucket_website("site").await.unwrap(), Some(plain));

    s3.delete_bucket_website("site").await.unwrap();
    last_request(&server, 0, "DELETE", "site", "website");
    assert_eq!(s3.get_bucket_website("site").await.unwrap(), None);
}

#[tokio::test]
async fn website_redirect_of_all_requests_round_trips() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    for protocol in [
        Some(RedirectProtocol::Https),
        Some(RedirectProtocol::Http),
        None,
    ] {
        let website = WebsiteConfig::Redirect {
            host_name: "www.example.com".into(),
            protocol,
        };
        s3.put_bucket_website("site", &website).await.unwrap();
        let body = last_request(&server, 0, "PUT", "site", "website").body_text();
        assert!(
            body.starts_with(&format!(
                "<WebsiteConfiguration {NS}><RedirectAllRequestsTo><HostName>www.example.com</HostName>"
            )),
            "{body}"
        );
        assert!(!body.contains("IndexDocument"), "{body}");
        assert_eq!(s3.get_bucket_website("site").await.unwrap(), Some(website));
    }
}

#[tokio::test]
async fn a_website_that_breaks_the_rules_is_never_sent() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    let error = s3
        .put_bucket_website(
            "site",
            &WebsiteConfig::Hosting {
                index_document: "dir/index.html".into(),
                error_document: None,
                routing_rules: vec![RoutingRule {
                    condition: Some(RoutingCondition::default()),
                    redirect: RoutingRedirect::default(),
                }],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    let text = error.to_string();
    assert!(
        text.starts_with("invalid website configuration: "),
        "{text}"
    );
    assert!(text.contains("must not contain a slash"), "{text}");
    assert!(
        text.contains("routing rule 1: the condition needs"),
        "{text}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_website_answer_in_the_shape_s3_documents_is_understood() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    server.store(
        "site",
        ConfigKind::Website,
        &format!(
            "<WebsiteConfiguration {NS}><IndexDocument><Suffix>index.html</Suffix></IndexDocument><ErrorDocument><Key>404.html</Key></ErrorDocument><RoutingRules><RoutingRule><Condition><HttpErrorCodeReturnedEquals>404</HttpErrorCodeReturnedEquals></Condition><Redirect><HostName>ec2-11-22-333-44.compute-1.amazonaws.com</HostName><ReplaceKeyPrefixWith>report-404/</ReplaceKeyPrefixWith></Redirect></RoutingRule></RoutingRules></WebsiteConfiguration>"
        ),
    );
    assert_eq!(
        server.ops().get_bucket_website("site").await.unwrap(),
        Some(WebsiteConfig::Hosting {
            index_document: "index.html".into(),
            error_document: Some("404.html".into()),
            routing_rules: vec![RoutingRule {
                condition: Some(RoutingCondition {
                    key_prefix_equals: None,
                    http_error_code_returned_equals: Some("404".into()),
                }),
                redirect: RoutingRedirect {
                    host_name: Some("ec2-11-22-333-44.compute-1.amazonaws.com".into()),
                    replace_key_prefix_with: Some("report-404/".into()),
                    ..RoutingRedirect::default()
                },
            }],
        })
    );
    // A server that answers 200 without any website part has no website.
    server.store(
        "site",
        ConfigKind::Website,
        &format!("<WebsiteConfiguration {NS}></WebsiteConfiguration>"),
    );
    assert_eq!(server.ops().get_bucket_website("site").await.unwrap(), None);
}

// ---- transfer acceleration ----

#[tokio::test]
async fn transfer_acceleration_round_trips_on_aws() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.aws_ops(false);
    assert!(s3.supports_acceleration());
    assert_eq!(
        s3.get_bucket_acceleration("site").await.unwrap(),
        AccelerationStatus::Unset
    );
    last_request(&server, 0, "GET", "site", "accelerate");

    s3.put_bucket_acceleration("site", AccelerationStatus::Enabled)
        .await
        .unwrap();
    let put = last_request(&server, 0, "PUT", "site", "accelerate");
    assert_eq!(
        put.body_text(),
        format!("<AccelerateConfiguration {NS}><Status>Enabled</Status></AccelerateConfiguration>")
    );
    assert_eq!(
        s3.get_bucket_acceleration("site").await.unwrap(),
        AccelerationStatus::Enabled
    );
    s3.put_bucket_acceleration("site", AccelerationStatus::Suspended)
        .await
        .unwrap();
    assert!(
        last_request(&server, 0, "PUT", "site", "accelerate")
            .body_text()
            .contains("<Status>Suspended</Status>")
    );
    assert_eq!(
        s3.get_bucket_acceleration("site").await.unwrap(),
        AccelerationStatus::Suspended
    );

    // Refused before any request: unset, and a bucket name with dots.
    let before = server.requests().len();
    for (bucket, status) in [
        ("site", AccelerationStatus::Unset),
        ("my.site", AccelerationStatus::Enabled),
    ] {
        let error = s3
            .put_bucket_acceleration(bucket, status)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{bucket}: {error}");
    }
    assert_eq!(server.requests().len(), before);
}

#[tokio::test]
async fn transfer_acceleration_is_not_supported_by_custom_endpoints() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    assert!(!s3.supports_acceleration());
    let error = s3.get_bucket_acceleration("site").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert_eq!(
        error.to_string(),
        "Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint."
    );
    let error = s3
        .put_bucket_acceleration("site", AccelerationStatus::Enabled)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(
        server.requests().is_empty(),
        "no request for an unsupported call"
    );
}

#[tokio::test]
async fn only_reading_acceleration_carries_the_requester_pays_header() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.aws_ops(true);
    s3.get_bucket_acceleration("site").await.unwrap();
    assert_eq!(
        last_request(&server, 0, "GET", "site", "accelerate").header("x-amz-request-payer"),
        Some("requester")
    );
    // The other bucket configuration calls do not support the header.
    s3.get_bucket_policy("site").await.unwrap();
    s3.get_public_access_block("site").await.unwrap();
    s3.get_bucket_versioning("site").await.unwrap();
    s3.get_bucket_logging("site").await.unwrap();
    s3.get_bucket_website("site").await.unwrap();
    s3.put_bucket_policy("site", POLICY).await.unwrap();
    s3.put_bucket_versioning("site", VersioningStatus::Enabled)
        .await
        .unwrap();
    for request in &server.requests()[1..] {
        assert_eq!(request.header("x-amz-request-payer"), None, "{request:?}");
    }
}

// ---- the combined read ----

#[tokio::test]
async fn the_combined_config_reads_every_section() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.aws_ops(false);
    s3.put_bucket_policy("site", POLICY).await.unwrap();
    s3.put_public_access_block(
        "site",
        &PublicAccessBlock {
            block_public_acls: true,
            ..PublicAccessBlock::default()
        },
    )
    .await
    .unwrap();
    s3.put_bucket_versioning("site", VersioningStatus::Enabled)
        .await
        .unwrap();
    s3.put_bucket_logging(
        "site",
        &LoggingTarget {
            target_bucket: "logs".into(),
            target_prefix: "p/".into(),
        },
    )
    .await
    .unwrap();
    s3.put_bucket_website(
        "site",
        &WebsiteConfig::Redirect {
            host_name: "example.com".into(),
            protocol: None,
        },
    )
    .await
    .unwrap();
    s3.put_bucket_acceleration("site", AccelerationStatus::Enabled)
        .await
        .unwrap();

    let before = server.requests().len();
    let config = s3.get_bucket_config("site").await.unwrap();
    // One read per section, nothing else.
    let mut queries: Vec<String> = server.requests()[before..]
        .iter()
        .map(|r| format!("{} {}", r.method, r.query))
        .collect();
    queries.sort();
    assert_eq!(
        queries,
        [
            "GET accelerate",
            "GET logging",
            "GET policy",
            "GET publicAccessBlock",
            "GET versioning",
            "GET website"
        ]
    );
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(
        value["policy"],
        json!({"type": "loaded", "value": format_policy(POLICY).unwrap()})
    );
    assert_eq!(
        value["public_access_block"],
        json!({"type": "loaded", "value": {
            "block_public_acls": true, "ignore_public_acls": false,
            "block_public_policy": false, "restrict_public_buckets": false
        }})
    );
    assert_eq!(
        value["versioning"],
        json!({"type": "loaded", "value": {"status": "enabled", "mfa_delete": false}})
    );
    assert_eq!(
        value["logging"],
        json!({"type": "loaded", "value": {"target_bucket": "logs", "target_prefix": "p/"}})
    );
    assert_eq!(
        value["website"],
        json!({"type": "loaded", "value": {"type": "redirect", "host_name": "example.com", "protocol": null}})
    );
    assert_eq!(
        value["acceleration"],
        json!({"type": "loaded", "value": "enabled"})
    );
}

#[tokio::test]
async fn nothing_configured_is_reported_as_loaded_nothing() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let config = server
        .aws_ops(false)
        .get_bucket_config("site")
        .await
        .unwrap();
    assert_eq!(config.policy, Section::Loaded { value: None });
    assert_eq!(config.public_access_block, Section::Loaded { value: None });
    assert_eq!(
        config.versioning,
        Section::Loaded {
            value: BucketVersioning::default()
        }
    );
    assert_eq!(config.logging, Section::Loaded { value: None });
    assert_eq!(config.website, Section::Loaded { value: None });
    assert_eq!(
        config.acceleration,
        Section::Loaded {
            value: AccelerationStatus::Unset
        }
    );
}

#[tokio::test]
async fn one_failing_section_never_hides_the_others() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let s3 = server.ops();
    s3.put_bucket_versioning("site", VersioningStatus::Suspended)
        .await
        .unwrap();
    server.deny("site", ConfigKind::Policy);
    server.not_implemented(ConfigKind::PublicAccessBlock);
    server.not_implemented(ConfigKind::Website);
    server.deny("site", ConfigKind::Logging);

    let config = s3.get_bucket_config("site").await.unwrap();

    // Refused: a failure with the code of the error.
    for error in [failure(&config.policy), failure(&config.logging)] {
        assert_eq!(error.code, ErrorKind::Auth);
        assert!(error.message.contains("AccessDenied"), "{}", error.message);
    }
    // Not implemented: reported as unsupported, with what the server said.
    let Section::Unsupported { message } = &config.public_access_block else {
        panic!("{:?}", config.public_access_block);
    };
    assert_eq!(
        message,
        "This server does not support the public access block (NotImplemented, HTTP 501)."
    );
    let Section::Unsupported { message } = &config.website else {
        panic!("{:?}", config.website);
    };
    assert!(
        message.starts_with("This server does not support static website hosting"),
        "{message}"
    );
    // What works is read.
    assert_eq!(
        config.versioning,
        Section::Loaded {
            value: BucketVersioning {
                status: VersioningStatus::Suspended,
                mfa_delete: false
            }
        }
    );
    // A custom endpoint has no transfer acceleration, and no request was sent for it.
    assert_eq!(
        config.acceleration,
        Section::Unsupported {
            message: "Transfer acceleration is available on AWS S3 only; this account uses a custom endpoint."
                .into()
        }
    );
    assert!(
        !server.requests().iter().any(|r| r.query == "accelerate"),
        "{:?}",
        server.requests()
    );
}

#[tokio::test]
async fn the_combined_config_of_a_missing_bucket_fails_every_section_as_not_found() {
    let server = BucketConfigMock::with_buckets(&["site"]);
    let config = server.ops().get_bucket_config("nope").await.unwrap();
    for error in [
        failure(&config.policy),
        failure(&config.public_access_block),
        failure(&config.versioning),
        failure(&config.logging),
        failure(&config.website),
    ] {
        assert_eq!(error.code, ErrorKind::NotFound, "{}", error.message);
    }
}

#[tokio::test]
async fn an_unreachable_bucket_fails_the_combined_read_once() {
    // A bucket name that is not valid fails before anything is sent.
    let server = BucketConfigMock::start();
    let error = server
        .ops()
        .get_bucket_config("not a bucket")
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn configuration_requests_go_to_the_region_of_the_bucket() {
    let mock = MockS3::start(|request| {
        if request.method == "HEAD" && request.path == "/far/" {
            return MockResponse::empty(301).with_header("x-amz-bucket-region", "eu-west-1");
        }
        if request.method == "GET" && request.query_param("policy").is_some() {
            return MockResponse::error(404, "NoSuchBucketPolicy", "no policy");
        }
        MockResponse::error(400, "UnexpectedRequest", "unexpected")
    });
    let s3 = mock.ops().with_region_resolution(true);
    assert_eq!(s3.get_bucket_policy("far").await.unwrap(), None);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "HEAD");
    let authorization = requests[1].header("authorization").unwrap();
    assert!(
        authorization.contains("/eu-west-1/s3/aws4_request"),
        "{authorization}"
    );
}

// ---- external buckets ----

fn named(name: &str) -> Bucket {
    Bucket {
        name: name.into(),
        region: None,
        creation_date: None,
        owner: None,
        kind: BucketKind::External,
    }
}

#[tokio::test]
async fn pinned_buckets_are_merged_into_the_listing_as_external() {
    let server = BucketConfigMock::with_buckets(&["mine-a", "mine-b"]);
    let s3 = server.ops();
    let listed = s3
        .list_buckets_with_pinned(&["shared-x".to_owned(), "mine-a".to_owned()])
        .await
        .unwrap();
    let names: Vec<&str> = listed.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["mine-a", "mine-b", "shared-x"]);
    // The listed bucket keeps its listing data; the pinned one has only a name.
    assert!(listed[0].creation_date.is_some());
    assert_eq!(listed[2], named("shared-x"));
    assert_eq!(listed[2].kind, BucketKind::External);

    // Without pins it is a plain listing.
    assert_eq!(
        s3.list_buckets_with_pinned(&[]).await.unwrap(),
        s3.list_buckets().await.unwrap()
    );
}

#[tokio::test]
async fn pinned_buckets_are_all_there_is_when_listing_is_denied() {
    let server = BucketConfigMock::with_buckets(&["shared-x"]);
    server.deny_listing();
    let s3 = server.ops();
    let listed = s3
        .list_buckets_with_pinned(&["shared-x".to_owned(), "shared-y".to_owned()])
        .await
        .unwrap();
    assert_eq!(listed, [named("shared-x"), named("shared-y")]);
    // Without a pinned bucket the refusal is what the caller needs to hear.
    let error = s3.list_buckets_with_pinned(&[]).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    // Only the denial of listing is covered: other failures stay failures.
    let broken = MockS3::start(|_| MockResponse::error(500, "InternalError", "boom"));
    let error = broken
        .ops()
        .list_buckets_with_pinned(&["shared-x".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Remote);
    let bad_key = MockS3::start(|_| MockResponse::error(403, "InvalidAccessKeyId", "no such key"));
    let error = bad_key
        .ops()
        .list_buckets_with_pinned(&["shared-x".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
}

#[tokio::test]
async fn a_bucket_is_verified_before_it_is_pinned() {
    let server = BucketConfigMock::with_buckets(&["real"]);
    let s3 = server.ops();
    s3.verify_bucket_exists("real").await.unwrap();
    let error = s3.verify_bucket_exists("imaginary").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert_eq!(error.to_string(), "bucket 'imaginary' does not exist");

    // A bucket the caller may not even HEAD, one in another region, and a
    // server without HeadBucket all count as existing; a broken server does not.
    for (status, exists) in [
        (403, true),
        (301, true),
        (307, true),
        (501, true),
        (500, false),
        (400, false),
    ] {
        let mock = MockS3::start(move |_| MockResponse::empty(status));
        let result = mock.ops().verify_bucket_exists("other").await;
        assert_eq!(result.is_ok(), exists, "HTTP {status}: {result:?}");
    }
}

// ---- settings of one bucket against a real server (MinIO) ----

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement bucket policies"
)]
async fn bucket_policy_round_trips_against_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    assert_eq!(s3.ops().get_bucket_policy(&bucket).await.unwrap(), None);

    let policy = json!({"Version": "2012-10-17", "Statement": [{
        "Sid": "PublicRead", "Effect": "Allow", "Principal": "*",
        "Action": ["s3:GetObject"], "Resource": [format!("arn:aws:s3:::{bucket}/*")]
    }]})
    .to_string();
    s3.ops().put_bucket_policy(&bucket, &policy).await.unwrap();
    let read = s3.ops().get_bucket_policy(&bucket).await.unwrap().unwrap();
    // MinIO may normalize the document, so the meaning is compared.
    let document = json_of(&read);
    assert_eq!(document["Version"], "2012-10-17");
    let statement = &document["Statement"][0];
    assert_eq!(statement["Effect"], "Allow");
    let flatten = |value: &Value| -> Vec<String> {
        match value {
            Value::String(text) => vec![text.clone()],
            Value::Array(items) => items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    };
    assert_eq!(flatten(&statement["Action"]), ["s3:GetObject"]);
    assert_eq!(
        flatten(&statement["Resource"]),
        [format!("arn:aws:s3:::{bucket}/*")]
    );

    // Edit and write again.
    let edited = read.replace("s3:GetObject", "s3:ListBucket");
    s3.ops().put_bucket_policy(&bucket, &edited).await.unwrap();
    let again = json_of(&s3.ops().get_bucket_policy(&bucket).await.unwrap().unwrap());
    assert_eq!(flatten(&again["Statement"][0]["Action"]), ["s3:ListBucket"]);

    s3.ops().delete_bucket_policy(&bucket).await.unwrap();
    assert_eq!(s3.ops().get_bucket_policy(&bucket).await.unwrap(), None);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn versioning_round_trips_against_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    assert_eq!(
        s3.ops()
            .get_bucket_versioning(&bucket)
            .await
            .unwrap()
            .status,
        VersioningStatus::Unversioned
    );
    s3.ops()
        .put_bucket_versioning(&bucket, VersioningStatus::Enabled)
        .await
        .unwrap();
    assert_eq!(
        s3.ops()
            .get_bucket_versioning(&bucket)
            .await
            .unwrap()
            .status,
        VersioningStatus::Enabled
    );
    s3.ops()
        .put_bucket_versioning(&bucket, VersioningStatus::Suspended)
        .await
        .unwrap();
    assert_eq!(
        s3.ops()
            .get_bucket_versioning(&bucket)
            .await
            .unwrap()
            .status,
        VersioningStatus::Suspended
    );
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement bucket policies or versioning"
)]
async fn the_combined_config_against_minio_reports_what_it_supports() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    s3.ops()
        .put_bucket_versioning(&bucket, VersioningStatus::Enabled)
        .await
        .unwrap();
    let config = s3.ops().get_bucket_config(&bucket).await.unwrap();
    assert_eq!(config.policy, Section::Loaded { value: None });
    assert_eq!(
        config.versioning,
        Section::Loaded {
            value: BucketVersioning {
                status: VersioningStatus::Enabled,
                mfa_delete: false
            }
        }
    );
    // MinIO implements some of the other sections and not others; each is
    // reported on its own and none of them hides the two above. A custom
    // endpoint never offers transfer acceleration.
    assert!(matches!(config.acceleration, Section::Unsupported { .. }));
}
