//! Creating a bucket with advanced options
//! (`bucketbridge_core::S3::create_bucket_with`): Object Lock and Object
//! Ownership travel with the CreateBucket request, the public access block
//! follows as its own request. Checked against the recorded [`MockS3`]
//! (s3s-fs ignores these options).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ErrorKind;
use bucketbridge_core::bucketcfg::PublicAccessBlock;
use bucketbridge_core::bucketcreate::CreateBucketOptions;
use bucketbridge_core::objectlock::ObjectOwnership;
use bucketbridge_tests::{MockResponse, MockS3};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn all_on() -> PublicAccessBlock {
    PublicAccessBlock {
        block_public_acls: true,
        ignore_public_acls: true,
        block_public_policy: true,
        restrict_public_buckets: true,
    }
}

#[tokio::test]
async fn the_default_options_send_a_plain_create_request() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let result = mock
        .ops()
        .create_bucket_with("plain-one", None, &CreateBucketOptions::default())
        .await
        .unwrap();
    assert!(result.warnings.is_empty());
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].path, "/plain-one/");
    assert_eq!(requests[0].query, "");
    assert!(
        requests[0]
            .header("x-amz-bucket-object-lock-enabled")
            .is_none()
    );
    assert!(requests[0].header("x-amz-object-ownership").is_none());

    // The old entry point is the same request.
    let again = MockS3::start(|_| MockResponse::empty(200));
    again.ops().create_bucket("plain-one", None).await.unwrap();
    assert_eq!(again.requests().len(), 1);
}

#[tokio::test]
async fn lock_ownership_region_and_block_are_sent_in_order() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let options = CreateBucketOptions {
        object_lock: true,
        ownership: Some(ObjectOwnership::BucketOwnerPreferred),
        public_access_block: Some(all_on()),
    };
    let result = mock
        .ops()
        .create_bucket_with("locked-one", Some("eu-west-1"), &options)
        .await
        .unwrap();
    assert!(result.warnings.is_empty());
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");

    let create = &requests[0];
    assert_eq!((create.method.as_str(), create.query.as_str()), ("PUT", ""));
    assert_eq!(
        create.header("x-amz-bucket-object-lock-enabled"),
        Some("true")
    );
    assert_eq!(
        create.header("x-amz-object-ownership"),
        Some("BucketOwnerPreferred")
    );
    assert!(
        create
            .body_text()
            .contains("<LocationConstraint>eu-west-1</LocationConstraint>"),
        "{}",
        create.body_text()
    );

    let block = &requests[1];
    assert_eq!(
        (block.method.as_str(), block.query.as_str()),
        ("PUT", "publicAccessBlock")
    );
    assert_eq!(
        block.body_text(),
        format!(
            "<PublicAccessBlockConfiguration {NS}><BlockPublicAcls>true</BlockPublicAcls><IgnorePublicAcls>true</IgnorePublicAcls><BlockPublicPolicy>true</BlockPublicPolicy><RestrictPublicBuckets>true</RestrictPublicBuckets></PublicAccessBlockConfiguration>"
        )
    );
}

#[tokio::test]
async fn a_server_without_the_public_access_block_leaves_a_warning() {
    let mock = MockS3::start(|request| {
        if request.query == "publicAccessBlock" {
            MockResponse::error(501, "NotImplemented", "not implemented")
        } else {
            MockResponse::empty(200)
        }
    });
    let options = CreateBucketOptions {
        public_access_block: Some(all_on()),
        ..CreateBucketOptions::default()
    };
    let result = mock
        .ops()
        .create_bucket_with("custom-srv", None, &options)
        .await
        .unwrap();
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].contains("public access block"),
        "{:?}",
        result.warnings
    );
}

#[tokio::test]
async fn another_failure_of_the_block_says_the_bucket_was_created() {
    let mock = MockS3::start(|request| {
        if request.query == "publicAccessBlock" {
            MockResponse::error(403, "AccessDenied", "no")
        } else {
            MockResponse::empty(200)
        }
    });
    let options = CreateBucketOptions {
        public_access_block: Some(all_on()),
        ..CreateBucketOptions::default()
    };
    let err = mock
        .ops()
        .create_bucket_with("denied-blk", None, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
    assert!(err.to_string().contains("was created"), "{err}");
}

#[tokio::test]
async fn a_failed_create_does_not_try_the_block() {
    let mock = MockS3::start(|_| MockResponse::error(409, "BucketAlreadyExists", "taken"));
    let options = CreateBucketOptions {
        object_lock: true,
        public_access_block: Some(all_on()),
        ..CreateBucketOptions::default()
    };
    let err = mock
        .ops()
        .create_bucket_with("taken-one", None, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn an_invalid_name_or_region_is_refused_before_any_request() {
    let mock = MockS3::start(|_| MockResponse::empty(200));
    let options = CreateBucketOptions::default();
    for (name, region) in [("Bad_Name", None), ("fine-name", Some("not a region"))] {
        let err = mock
            .ops()
            .create_bucket_with(name, region, &options)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{name}");
    }
    assert!(mock.requests().is_empty());
}
