//! The ACL grant editor of `bucketbridge_core::objectmeta`: bucket ACLs and
//! whole grant lists for buckets and objects, checked against the recorded
//! [`MockS3`] (s3s-fs has no ACL support).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ErrorKind;
use bucketbridge_core::objectlock::ObjectOwnership;
use bucketbridge_core::objectmeta::{
    ALL_USERS_URI, AUTHENTICATED_USERS_URI, AclGrant, AclPermission, CannedAcl, GranteeKind,
    LOG_DELIVERY_URI, validate_grants,
};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const BUCKET: &str = "shared";
const OWNER: &str = "79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn owner_policy(extra: &str) -> String {
    format!(
        "<AccessControlPolicy {NS}><Owner><ID>{OWNER}</ID><DisplayName>me</DisplayName></Owner><AccessControlList><Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>{OWNER}</ID><DisplayName>me</DisplayName></Grantee><Permission>FULL_CONTROL</Permission></Grant>{extra}</AccessControlList></AccessControlPolicy>"
    )
}

/// Answers `GET ?acl` with the owner's ACL and everything else with 200.
fn acl_server() -> MockS3 {
    MockS3::start(|request| {
        if request.method == "GET" && request.query == "acl" {
            MockResponse::xml(owner_policy(""))
        } else {
            MockResponse::empty(200)
        }
    })
}

fn grant(kind: GranteeKind, who: &str, permission: AclPermission) -> AclGrant {
    AclGrant {
        grantee_type: kind,
        grantee: who.to_owned(),
        permission,
    }
}

fn owner_full() -> AclGrant {
    grant(
        GranteeKind::CanonicalUser,
        OWNER,
        AclPermission::FullControl,
    )
}

fn last(mock: &MockS3) -> RecordedRequest {
    mock.requests().last().unwrap().clone()
}

#[tokio::test]
async fn an_object_acl_is_replaced_with_every_kind_of_grantee() {
    let mock = acl_server();
    let grants = [
        owner_full(),
        grant(GranteeKind::Group, ALL_USERS_URI, AclPermission::Read),
        grant(GranteeKind::CanonicalUser, "abc123", AclPermission::ReadAcp),
        grant(GranteeKind::Email, "a@example.com", AclPermission::Write),
        grant(
            GranteeKind::Group,
            LOG_DELIVERY_URI,
            AclPermission::WriteAcp,
        ),
    ];
    mock.ops()
        .put_object_acl_grants(BUCKET, "docs/a.txt", &grants)
        .await
        .unwrap();
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        (requests[0].method.as_str(), requests[0].query.as_str()),
        ("GET", "acl")
    );
    let put = &requests[1];
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query, "acl");
    assert!(put.path.ends_with("/docs/a.txt"), "{}", put.path);
    let body = put.body_text();
    assert!(
        body.contains(&format!(
            "<Owner><DisplayName>me</DisplayName><ID>{OWNER}</ID></Owner>"
        )),
        "{body}"
    );
    assert!(body.contains(&format!("<ID>{OWNER}</ID>")), "{body}");
    assert!(
        body.contains(&format!("<URI>{ALL_USERS_URI}</URI>")),
        "{body}"
    );
    assert!(body.contains("<ID>abc123</ID>"), "{body}");
    assert!(
        body.contains("<EmailAddress>a@example.com</EmailAddress>"),
        "{body}"
    );
    assert!(body.contains("AmazonCustomerByEmail"), "{body}");
    for permission in ["FULL_CONTROL", "READ", "READ_ACP", "WRITE", "WRITE_ACP"] {
        assert!(
            body.contains(&format!("<Permission>{permission}</Permission>")),
            "{permission}: {body}"
        );
    }
    assert!(put.header("content-md5").is_some());
}

#[tokio::test]
async fn a_bucket_acl_is_replaced_and_read_with_its_ownership() {
    let mock = acl_server();
    mock.ops()
        .put_bucket_acl_grants(
            BUCKET,
            &[
                owner_full(),
                grant(
                    GranteeKind::Group,
                    AUTHENTICATED_USERS_URI,
                    AclPermission::Read,
                ),
            ],
        )
        .await
        .unwrap();
    let put = last(&mock);
    assert_eq!((put.method.as_str(), put.query.as_str()), ("PUT", "acl"));
    assert_eq!(put.path, format!("/{BUCKET}/"));
    assert!(put.body_text().contains(AUTHENTICATED_USERS_URI));

    let read = MockS3::start(|request| match request.query.as_str() {
        "acl" => MockResponse::xml(owner_policy(&format!(
            "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"Group\"><URI>{ALL_USERS_URI}</URI></Grantee><Permission>READ</Permission></Grant>"
        ))),
        "ownershipControls" => MockResponse::xml(format!(
            "<OwnershipControls {NS}><Rule><ObjectOwnership>ObjectWriter</ObjectOwnership></Rule></OwnershipControls>"
        )),
        _ => MockResponse::error(400, "Unexpected", "unexpected"),
    });
    let info = read.ops().get_bucket_acl(BUCKET).await.unwrap();
    assert_eq!(info.acl.canned, Some(CannedAcl::PublicRead));
    assert_eq!(info.acl.owner.as_deref(), Some(OWNER));
    assert_eq!(info.acl.grants.len(), 2);
    assert_eq!(info.ownership, Some(ObjectOwnership::ObjectWriter));
    assert!(!info.acls_disabled);
}

#[tokio::test]
async fn acls_disabled_by_ownership_is_reported_and_changes_are_a_clear_conflict() {
    let mock = MockS3::start(
        |request| match (request.method.as_str(), request.query.as_str()) {
            ("GET", "acl") => MockResponse::xml(owner_policy("")),
            ("GET", "ownershipControls") => MockResponse::xml(format!(
                "<OwnershipControls {NS}><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule></OwnershipControls>"
            )),
            _ => MockResponse::error(
                400,
                "AccessControlListNotSupported",
                "The bucket does not allow ACLs",
            ),
        },
    );
    let info = mock.ops().get_bucket_acl(BUCKET).await.unwrap();
    assert!(info.acls_disabled);
    assert_eq!(info.ownership, Some(ObjectOwnership::BucketOwnerEnforced));
    assert_eq!(info.acl.canned, Some(CannedAcl::Private));

    for result in [
        mock.ops()
            .put_bucket_acl_grants(BUCKET, &[owner_full()])
            .await,
        mock.ops()
            .put_object_acl_grants(BUCKET, "k", &[owner_full()])
            .await,
        mock.ops()
            .put_bucket_acl(BUCKET, CannedAcl::PublicRead)
            .await,
    ] {
        let err = result.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
        let text = err.to_string();
        assert!(text.contains("BucketOwnerEnforced"), "{text}");
        assert!(text.contains("ACLs are disabled"), "{text}");
    }
}

#[tokio::test]
async fn an_unreadable_ownership_does_not_hide_the_bucket_acl() {
    let mock = MockS3::start(|request| match request.query.as_str() {
        "acl" => MockResponse::xml(owner_policy("")),
        _ => MockResponse::error(403, "AccessDenied", "no"),
    });
    let info = mock.ops().get_bucket_acl(BUCKET).await.unwrap();
    assert_eq!(info.ownership, None);
    assert!(!info.acls_disabled);
}

#[tokio::test]
async fn a_canned_bucket_acl_is_sent_as_a_header_and_object_only_ones_are_refused() {
    let mock = acl_server();
    mock.ops()
        .put_bucket_acl(BUCKET, CannedAcl::AuthenticatedRead)
        .await
        .unwrap();
    let put = last(&mock);
    assert_eq!((put.method.as_str(), put.query.as_str()), ("PUT", "acl"));
    assert_eq!(put.header("x-amz-acl"), Some("authenticated-read"));

    let before = mock.requests().len();
    for acl in [
        CannedAcl::BucketOwnerRead,
        CannedAcl::BucketOwnerFullControl,
    ] {
        let err = mock.ops().put_bucket_acl(BUCKET, acl).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
    assert_eq!(mock.requests().len(), before);
}

#[tokio::test]
async fn invalid_grant_lists_are_refused_before_any_request() {
    let mock = acl_server();
    let ops = mock.ops();
    let cases: Vec<Vec<AclGrant>> = vec![
        vec![],
        vec![grant(
            GranteeKind::Group,
            "http://example.com/group",
            AclPermission::Read,
        )],
        vec![grant(GranteeKind::Email, "nobody", AclPermission::Read)],
        vec![grant(
            GranteeKind::Email,
            "a b@example.com",
            AclPermission::Read,
        )],
        vec![grant(GranteeKind::CanonicalUser, "", AclPermission::Read)],
        vec![grant(
            GranteeKind::CanonicalUser,
            "a b",
            AclPermission::Read,
        )],
        vec![grant(
            GranteeKind::CanonicalUser,
            " abc",
            AclPermission::Read,
        )],
        vec![owner_full(), owner_full()],
    ];
    for grants in &cases {
        assert_eq!(
            validate_grants(grants).unwrap_err().kind(),
            ErrorKind::Usage,
            "{grants:?}"
        );
        assert_eq!(
            ops.put_bucket_acl_grants(BUCKET, grants)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
        assert_eq!(
            ops.put_object_acl_grants(BUCKET, "k", grants)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
    }
    // A folder key is not an object.
    assert_eq!(
        ops.put_object_acl_grants(BUCKET, "folder/", &[owner_full()])
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Usage
    );
    assert!(mock.requests().is_empty());
    // The same grantee with two permissions is fine.
    validate_grants(&[
        owner_full(),
        grant(GranteeKind::Group, ALL_USERS_URI, AclPermission::Read),
        grant(GranteeKind::Group, ALL_USERS_URI, AclPermission::ReadAcp),
    ])
    .unwrap();
}

#[tokio::test]
async fn an_email_grant_that_s3_refuses_passes_its_message_on() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" {
            MockResponse::xml(owner_policy(""))
        } else {
            MockResponse::error(
                400,
                "UnresolvableGrantByEmailAddress",
                "The email address you provided does not match any account on record.",
            )
        }
    });
    let err = mock
        .ops()
        .put_bucket_acl_grants(
            BUCKET,
            &[
                owner_full(),
                grant(GranteeKind::Email, "x@example.com", AclPermission::Read),
            ],
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("does not match any account"),
        "{err}"
    );
}
