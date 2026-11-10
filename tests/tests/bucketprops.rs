//! Default encryption and tags of a bucket (`bucketbridge_core::bucketprops`),
//! checked against the recorded [`MockS3`]: s3s-fs implements neither API, so
//! the tests assert the exact requests and what comes back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use bucketbridge_core::ErrorKind;
use bucketbridge_core::bucketprops::{BucketEncryption, BucketSse, MAX_BUCKET_TAGS};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const BUCKET: &str = "costs";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn only(mock: &MockS3) -> RecordedRequest {
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    requests[0].clone()
}

fn ok() -> MockS3 {
    MockS3::start(|_| MockResponse::empty(200))
}

fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---- default encryption ----

#[tokio::test]
async fn sse_s3_is_sent_without_a_bucket_key() {
    let mock = ok();
    mock.ops()
        .put_bucket_encryption(
            BUCKET,
            &BucketEncryption {
                algorithm: BucketSse::Aes256,
                kms_key_id: None,
                bucket_key_enabled: false,
            },
        )
        .await
        .unwrap();
    let put = only(&mock);
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query, "encryption");
    assert_eq!(
        put.body_text(),
        format!(
            "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>AES256</SSEAlgorithm></ApplyServerSideEncryptionByDefault></Rule></ServerSideEncryptionConfiguration>"
        )
    );
    assert!(put.header("content-md5").is_some());
}

#[tokio::test]
async fn sse_kms_carries_the_key_and_the_bucket_key() {
    let mock = ok();
    mock.ops()
        .put_bucket_encryption(
            BUCKET,
            &BucketEncryption {
                algorithm: BucketSse::Kms,
                kms_key_id: Some("alias/team-key".into()),
                bucket_key_enabled: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        only(&mock).body_text(),
        format!(
            "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>aws:kms</SSEAlgorithm><KMSMasterKeyID>alias/team-key</KMSMasterKeyID></ApplyServerSideEncryptionByDefault><BucketKeyEnabled>true</BucketKeyEnabled></Rule></ServerSideEncryptionConfiguration>"
        )
    );
}

#[tokio::test]
async fn invalid_encryption_settings_are_refused_before_any_request() {
    let mock = ok();
    let ops = mock.ops();
    let cases = [
        // A key with SSE-S3.
        BucketEncryption {
            algorithm: BucketSse::Aes256,
            kms_key_id: Some("alias/x".into()),
            bucket_key_enabled: false,
        },
        // A bucket key with SSE-S3.
        BucketEncryption {
            algorithm: BucketSse::Aes256,
            kms_key_id: None,
            bucket_key_enabled: true,
        },
        // An empty key.
        BucketEncryption {
            algorithm: BucketSse::Kms,
            kms_key_id: Some("  ".into()),
            bucket_key_enabled: false,
        },
        // A key with a space.
        BucketEncryption {
            algorithm: BucketSse::Kms,
            kms_key_id: Some("alias/a b".into()),
            bucket_key_enabled: false,
        },
        // Dual-layer cannot be set.
        BucketEncryption {
            algorithm: BucketSse::KmsDsse,
            kms_key_id: None,
            bucket_key_enabled: false,
        },
    ];
    for case in cases {
        let err = ops.put_bucket_encryption(BUCKET, &case).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{case:?}");
    }
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn the_encryption_is_read_and_absence_is_none() {
    let body = format!(
        "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>aws:kms</SSEAlgorithm><KMSMasterKeyID>arn:aws:kms:eu-west-1:000000000000:key/abc</KMSMasterKeyID></ApplyServerSideEncryptionByDefault><BucketKeyEnabled>true</BucketKeyEnabled></Rule></ServerSideEncryptionConfiguration>"
    );
    let mock = MockS3::start(move |_| MockResponse::xml(body.clone()));
    assert_eq!(
        mock.ops().get_bucket_encryption(BUCKET).await.unwrap(),
        Some(BucketEncryption {
            algorithm: BucketSse::Kms,
            kms_key_id: Some("arn:aws:kms:eu-west-1:000000000000:key/abc".into()),
            bucket_key_enabled: true,
        })
    );
    assert_eq!(only(&mock).query, "encryption");

    let s3 = MockS3::start(|_| {
        MockResponse::xml(format!(
            "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>AES256</SSEAlgorithm></ApplyServerSideEncryptionByDefault></Rule></ServerSideEncryptionConfiguration>"
        ))
    });
    assert_eq!(
        s3.ops().get_bucket_encryption(BUCKET).await.unwrap(),
        Some(BucketEncryption {
            algorithm: BucketSse::Aes256,
            kms_key_id: None,
            bucket_key_enabled: false,
        })
    );

    let none = MockS3::start(|_| {
        MockResponse::error(
            404,
            "ServerSideEncryptionConfigurationNotFoundError",
            "not found",
        )
    });
    assert_eq!(
        none.ops().get_bucket_encryption(BUCKET).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn removing_the_encryption_tolerates_none_and_reports_other_errors() {
    let mock = MockS3::start(|_| MockResponse::empty(204));
    mock.ops().delete_bucket_encryption(BUCKET).await.unwrap();
    let delete = only(&mock);
    assert_eq!(delete.method, "DELETE");
    assert_eq!(delete.query, "encryption");

    let none = MockS3::start(|_| {
        MockResponse::error(
            404,
            "ServerSideEncryptionConfigurationNotFoundError",
            "not found",
        )
    });
    none.ops().delete_bucket_encryption(BUCKET).await.unwrap();

    let denied = MockS3::start(|_| MockResponse::error(403, "AccessDenied", "no"));
    let err = denied
        .ops()
        .delete_bucket_encryption(BUCKET)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
}

// ---- tags ----

#[tokio::test]
async fn tags_are_sent_sorted_with_exact_xml() {
    let mock = ok();
    mock.ops()
        .put_bucket_tags(BUCKET, &tags(&[("team", "data"), ("cost-center", "42")]))
        .await
        .unwrap();
    let put = only(&mock);
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query, "tagging");
    assert_eq!(
        put.body_text(),
        format!(
            "<Tagging {NS}><TagSet><Tag><Key>cost-center</Key><Value>42</Value></Tag><Tag><Key>team</Key><Value>data</Value></Tag></TagSet></Tagging>"
        )
    );
    assert!(put.header("content-md5").is_some());
}

#[tokio::test]
async fn an_empty_tag_set_deletes_the_tags() {
    let mock = MockS3::start(|_| MockResponse::empty(204));
    mock.ops()
        .put_bucket_tags(BUCKET, &BTreeMap::new())
        .await
        .unwrap();
    let delete = only(&mock);
    assert_eq!(delete.method, "DELETE");
    assert_eq!(delete.query, "tagging");
}

#[tokio::test]
async fn tag_limits_are_checked_before_any_request() {
    let mock = ok();
    let ops = mock.ops();
    let many: BTreeMap<String, String> = (0..=MAX_BUCKET_TAGS)
        .map(|n| (format!("k{n}"), "v".to_owned()))
        .collect();
    assert_eq!(
        ops.put_bucket_tags(BUCKET, &many).await.unwrap_err().kind(),
        ErrorKind::Usage
    );
    let at_limit: BTreeMap<String, String> = (0..MAX_BUCKET_TAGS)
        .map(|n| (format!("k{n}"), "v".to_owned()))
        .collect();
    ops.put_bucket_tags(BUCKET, &at_limit).await.unwrap();
    assert_eq!(mock.requests().len(), 1);

    for bad in [
        tags(&[("aws:reserved", "x")]),
        tags(&[("", "x")]),
        tags(&[(&"k".repeat(129), "x")]),
        tags(&[("k", &"v".repeat(257))]),
        tags(&[("k", "bad#value")]),
    ] {
        let err = ops.put_bucket_tags(BUCKET, &bad).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{bad:?}");
    }
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn tags_are_read_and_a_missing_set_is_empty() {
    let body = format!(
        "<Tagging {NS}><TagSet><Tag><Key>team</Key><Value>data</Value></Tag><Tag><Key>env</Key><Value>prod</Value></Tag></TagSet></Tagging>"
    );
    let mock = MockS3::start(move |_| MockResponse::xml(body.clone()));
    assert_eq!(
        mock.ops().get_bucket_tags(BUCKET).await.unwrap(),
        tags(&[("env", "prod"), ("team", "data")])
    );
    assert_eq!(only(&mock).query, "tagging");

    let none = MockS3::start(|_| MockResponse::error(404, "NoSuchTagSet", "none"));
    assert!(none.ops().get_bucket_tags(BUCKET).await.unwrap().is_empty());
    none.ops().delete_bucket_tags(BUCKET).await.unwrap();
}
