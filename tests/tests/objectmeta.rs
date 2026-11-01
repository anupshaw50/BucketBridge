//! Object metadata (task T06): tags, storage class, server-side encryption,
//! HTTP headers, user metadata and canned ACLs.
//!
//! Most tests run against [`ObjectStore`], an in-memory S3 that answers the
//! way S3 does (a copy resets the storage class, the encryption and the ACL;
//! an SSE-C object needs its key) and records every request, so the tests
//! assert the exact headers that are sent and what is stored afterwards. A
//! few run against s3s-fs, and the ones that need a real server are gated
//! like the other MinIO tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use bucketbridge_core::ErrorKind;
use bucketbridge_core::aws_sdk_s3::primitives::ByteStream;
use bucketbridge_core::model::Sse;
use bucketbridge_core::objectmeta::{
    AclPermission, BatchOptions, CannedAcl, CustomerKey, EncryptionChange, GranteeKind,
    HeaderChange, HeaderChanges, StorageClass, TagEdit,
};
use bucketbridge_tests::{
    MockResponse, MockS3, ObjectStore, RecordedRequest, StoredEncryption, StoredObject, TestS3,
};

const BUCKET: &str = "bucket";
const KEY_A: [u8; 32] = [0x11; 32];
const KEY_B: [u8; 32] = [0x22; 32];
const GIB: u64 = 1024 * 1024 * 1024;

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn key_a() -> CustomerKey {
    CustomerKey::from_bytes(&KEY_A).unwrap()
}

fn key_b() -> CustomerKey {
    CustomerKey::from_bytes(&KEY_B).unwrap()
}

fn store() -> (std::sync::Arc<ObjectStore>, MockS3) {
    let store = ObjectStore::new();
    let mock = store.start();
    (store, mock)
}

/// The `CopyObject` requests the mock received.
fn copies(mock: &MockS3) -> Vec<RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|r| {
            r.method == "PUT"
                && r.header("x-amz-copy-source").is_some()
                && r.query_param("partNumber").is_none()
        })
        .collect()
}

/// The requests with `method` whose query has the parameter `name`.
fn with_query(mock: &MockS3, method: &str, name: &str) -> Vec<RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|r| r.method == method && r.query_param(name).is_some())
        .collect()
}

fn count(mock: &MockS3, method: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|r| r.method == method)
        .count()
}

fn md5_of(key: &[u8; 32]) -> String {
    CustomerKey::from_bytes(key).unwrap().md5_base64()
}

/// A rich object: headers, user metadata, tags, a class, encryption and a
/// redirect, everything an in-place edit must keep.
fn rich() -> StoredObject {
    let mut object = StoredObject::new(1234)
        .content_type("text/csv")
        .class("STANDARD_IA")
        .meta("owner", "team")
        .meta("stage", "raw")
        .tag("env", "prod")
        .tag("team", "data")
        .encrypted(StoredEncryption::S3)
        .redirect("/elsewhere");
    object.cache_control = Some("max-age=60".into());
    object.content_disposition = Some("attachment".into());
    object.content_encoding = Some("gzip".into());
    object.content_language = Some("en".into());
    object.expires = Some("Fri, 21 Dec 2012 00:00:00 GMT".into());
    object
}

// ---- reading ----

#[tokio::test]
async fn details_combine_the_head_tags_and_acl_requests() {
    let (store, mock) = store();
    let mut object = rich()
        .encrypted(StoredEncryption::Kms {
            key_id: "arn:aws:kms:eu-west-1:000000000000:key/abc".into(),
            bucket_key: true,
        })
        .public_read();
    object.restore = None;
    store.put(BUCKET, "dir/report.csv", object);

    let details = mock
        .ops()
        .object_details(BUCKET, "dir/report.csv", None)
        .await
        .unwrap();
    assert_eq!(details.head.key, "dir/report.csv");
    assert_eq!(details.head.size, 1234);
    assert_eq!(details.head.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(details.head.meta.content_type.as_deref(), Some("text/csv"));
    assert_eq!(
        details.head.meta.cache_control.as_deref(),
        Some("max-age=60")
    );
    assert_eq!(
        details.head.meta.content_disposition.as_deref(),
        Some("attachment")
    );
    assert_eq!(details.head.meta.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(
        details.head.meta.user_meta,
        map(&[("owner", "team"), ("stage", "raw")])
    );
    assert_eq!(
        details.head.meta.tags,
        map(&[("env", "prod"), ("team", "data")])
    );
    assert_eq!(
        details.head.meta.sse,
        Sse::Kms {
            key_id: Some("arn:aws:kms:eu-west-1:000000000000:key/abc".into()),
            bucket_key: true
        }
    );
    assert_eq!(details.head.meta.acl.as_deref(), Some("public-read"));
    let acl = details.acl.unwrap();
    assert_eq!(acl.canned, Some(CannedAcl::PublicRead));
    assert_eq!(acl.owner.as_deref(), Some(bucketbridge_tests::OWNER_ID));
    assert!(
        acl.grants
            .iter()
            .any(|g| g.grantee_type == GranteeKind::Group && g.permission == AclPermission::Read)
    );
    assert!(details.tags_error.is_none() && details.acl_error.is_none());
    assert!(details.restore.is_none() && details.archive_status.is_none());

    let methods: Vec<(String, String)> = mock
        .requests()
        .iter()
        .map(|r| (r.method.clone(), r.query.clone()))
        .collect();
    assert_eq!(methods.len(), 3, "{methods:?}");
    assert!(methods.contains(&("HEAD".into(), String::new())));
    assert!(
        methods
            .iter()
            .any(|(m, q)| m == "GET" && q.starts_with("tagging"))
    );
    assert!(
        methods
            .iter()
            .any(|(m, q)| m == "GET" && q.starts_with("acl"))
    );
    let head = &mock.requests()[methods.iter().position(|(m, _)| m == "HEAD").unwrap()];
    assert_eq!(head.header("x-amz-checksum-mode"), Some("ENABLED"));
}

#[tokio::test]
async fn s3_omits_the_class_of_standard_objects_and_the_details_say_standard() {
    let (store, mock) = store();
    store.put(BUCKET, "plain", StoredObject::new(1));
    let details = mock
        .ops()
        .object_details(BUCKET, "plain", None)
        .await
        .unwrap();
    assert_eq!(details.head.storage_class.as_deref(), Some("STANDARD"));
    assert_eq!(details.head.meta.sse, Sse::None);
    assert_eq!(details.head.meta.acl.as_deref(), Some("private"));
    assert!(details.head.meta.tags.is_empty());
}

#[tokio::test]
async fn a_failing_tag_or_acl_request_does_not_hide_the_rest() {
    let (store, mock) = store();
    store.put(BUCKET, "k", rich());
    store.intercept(|request| {
        (request.method == "GET" && request.query_param("tagging").is_some())
            .then(|| MockResponse::error(403, "AccessDenied", "Access Denied"))
    });
    let details = mock.ops().object_details(BUCKET, "k", None).await.unwrap();
    let tags_error = details.tags_error.expect("the tags request failed");
    assert_eq!(tags_error.code, ErrorKind::Auth);
    assert!(details.head.meta.tags.is_empty());
    assert_eq!(details.head.meta.content_type.as_deref(), Some("text/csv"));
    assert_eq!(details.acl.unwrap().canned, Some(CannedAcl::Private));
    assert!(details.acl_error.is_none());

    let (store, mock) = self::store();
    store.put(BUCKET, "k", rich());
    store.intercept(|request| {
        (request.method == "GET" && request.query_param("acl").is_some())
            .then(|| MockResponse::error(501, "NotImplemented", "ACLs are not supported"))
    });
    let details = mock.ops().object_details(BUCKET, "k", None).await.unwrap();
    assert_eq!(
        details.acl_error.expect("the ACL request failed").code,
        ErrorKind::Remote
    );
    assert!(details.acl.is_none());
    assert_eq!(details.head.meta.acl, None);
    assert_eq!(
        details.head.meta.tags,
        map(&[("env", "prod"), ("team", "data")])
    );

    // The HEAD part is required: without it there is nothing to show.
    let err = mock
        .ops()
        .object_details(BUCKET, "missing", None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

#[tokio::test]
async fn details_of_an_archived_object_show_the_restore_state() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "cold",
        StoredObject::new(9).class("GLACIER").restore_header(
            r#"ongoing-request="false", expiry-date="Fri, 21 Dec 2012 00:00:00 GMT""#,
        ),
    );
    let details = mock
        .ops()
        .object_details(BUCKET, "cold", None)
        .await
        .unwrap();
    assert_eq!(details.head.storage_class.as_deref(), Some("GLACIER"));
    let restore = details.restore.unwrap();
    assert!(!restore.in_progress);
    assert_eq!(restore.expires.unwrap().timestamp(), 1_356_048_000);
}

// ---- tags ----

#[tokio::test]
async fn tags_round_trip_through_put_get_and_delete() {
    let (store, mock) = store();
    store.put(BUCKET, "a b/ü.txt", StoredObject::new(1));
    let s3 = mock.ops();
    assert!(
        s3.get_object_tags(BUCKET, "a b/ü.txt")
            .await
            .unwrap()
            .is_empty()
    );

    let tags = map(&[("env", "prod"), ("team name", "data eng"), ("k", "a+b=c.d")]);
    s3.put_object_tags(BUCKET, "a b/ü.txt", &tags)
        .await
        .unwrap();
    assert_eq!(s3.get_object_tags(BUCKET, "a b/ü.txt").await.unwrap(), tags);
    assert_eq!(store.object(BUCKET, "a b/ü.txt").unwrap().tags, tags);

    let puts = with_query(&mock, "PUT", "tagging");
    assert_eq!(puts.len(), 1);
    assert!(
        puts[0].header("content-md5").is_some(),
        "PutObjectTagging needs an integrity header"
    );
    let body = puts[0].body_text();
    assert!(
        body.contains("<Key>team name</Key><Value>data eng</Value>"),
        "{body}"
    );

    // An empty set deletes the tags with DeleteObjectTagging.
    s3.put_object_tags(BUCKET, "a b/ü.txt", &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(with_query(&mock, "DELETE", "tagging").len(), 1);
    assert_eq!(with_query(&mock, "PUT", "tagging").len(), 1);
    assert!(
        s3.get_object_tags(BUCKET, "a b/ü.txt")
            .await
            .unwrap()
            .is_empty()
    );

    s3.put_object_tags(BUCKET, "a b/ü.txt", &tags)
        .await
        .unwrap();
    s3.delete_object_tags(BUCKET, "a b/ü.txt").await.unwrap();
    assert!(store.object(BUCKET, "a b/ü.txt").unwrap().tags.is_empty());

    // S3's rules are checked before anything is sent.
    let before = mock.requests().len();
    for bad in [map(&[("k", "semi;colon")]), map(&[("aws:x", "v")])] {
        let err = s3
            .put_object_tags(BUCKET, "a b/ü.txt", &bad)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
    assert_eq!(mock.requests().len(), before);

    let err = s3.get_object_tags(BUCKET, "missing").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

#[tokio::test]
async fn a_tag_merge_over_many_objects_reports_each_key() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "one",
        StoredObject::new(1).tag("env", "dev").tag("temp", "x"),
    );
    store.put(BUCKET, "two", StoredObject::new(1).tag("env", "prod"));
    store.put(BUCKET, "three", StoredObject::new(1).tag("keep", "k"));
    let edit = TagEdit::Merge {
        set: map(&[("env", "prod")]),
        remove: BTreeSet::from(["temp".to_owned()]),
    };
    let results = mock
        .ops()
        .update_object_tags(
            BUCKET,
            &keys(&["one", "two", "missing", "three", "one"]),
            &edit,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let summary: Vec<(&str, bool, Option<ErrorKind>)> = results
        .iter()
        .map(|r| (r.key.as_str(), r.changed, r.error.as_ref().map(|e| e.code)))
        .collect();
    assert_eq!(
        summary,
        [
            ("one", true, None),
            // Already has the tag: nothing is written.
            ("two", false, None),
            ("missing", false, Some(ErrorKind::NotFound)),
            ("three", true, None),
        ]
    );
    assert_eq!(
        store.object(BUCKET, "one").unwrap().tags,
        map(&[("env", "prod")])
    );
    assert_eq!(
        store.object(BUCKET, "two").unwrap().tags,
        map(&[("env", "prod")])
    );
    assert_eq!(
        store.object(BUCKET, "three").unwrap().tags,
        map(&[("env", "prod"), ("keep", "k")])
    );
    // Two writes: one for "one", one for "three".
    assert_eq!(with_query(&mock, "PUT", "tagging").len(), 2);
}

#[tokio::test]
async fn replacing_tags_writes_without_reading_and_an_empty_set_deletes() {
    let (store, mock) = store();
    store.put(BUCKET, "a", StoredObject::new(1).tag("old", "1"));
    store.put(BUCKET, "b", StoredObject::new(1).tag("old", "2"));
    let replace = TagEdit::Replace {
        tags: map(&[("new", "1")]),
    };
    let results = mock
        .ops()
        .update_object_tags(
            BUCKET,
            &keys(&["a", "b"]),
            &replace,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results.iter().all(|r| r.changed && r.error.is_none()));
    assert_eq!(
        store.object(BUCKET, "a").unwrap().tags,
        map(&[("new", "1")])
    );
    assert_eq!(count(&mock, "GET"), 0, "a replacement needs no read");

    let clear = TagEdit::Replace {
        tags: BTreeMap::new(),
    };
    mock.ops()
        .update_object_tags(BUCKET, &keys(&["a"]), &clear, BatchOptions::default())
        .await
        .unwrap();
    assert!(store.object(BUCKET, "a").unwrap().tags.is_empty());
    assert_eq!(with_query(&mock, "DELETE", "tagging").len(), 1);
}

#[tokio::test]
async fn a_dry_run_reads_but_writes_nothing() {
    let (store, mock) = store();
    store.put(BUCKET, "a", StoredObject::new(1).tag("env", "dev"));
    let edit = TagEdit::Merge {
        set: map(&[("env", "prod")]),
        remove: BTreeSet::new(),
    };
    let results = mock
        .ops()
        .update_object_tags(BUCKET, &keys(&["a"]), &edit, BatchOptions::dry_run())
        .await
        .unwrap();
    assert!(results[0].changed, "it would change");
    assert_eq!(
        store.object(BUCKET, "a").unwrap().tags,
        map(&[("env", "dev")])
    );
    assert!(mock.requests().iter().all(|r| r.method == "GET"));
}

#[tokio::test]
async fn invalid_edits_fail_before_anything_is_sent() {
    let (store, mock) = store();
    store.put(BUCKET, "a", StoredObject::new(1));
    let s3 = mock.ops();
    let bad_tags = TagEdit::Replace {
        tags: map(&[("k", "no;good")]),
    };
    let err = s3
        .update_object_tags(BUCKET, &keys(&["a"]), &bad_tags, BatchOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let bad_headers = HeaderChanges {
        cache_control: HeaderChange::Set {
            value: "line\nbreak".into(),
        },
        ..HeaderChanges::default()
    };
    let err = s3
        .update_object_headers(
            BUCKET,
            &keys(&["a"]),
            &bad_headers,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    let err = s3
        .set_object_storage_class(
            BUCKET,
            &[],
            StorageClass::Glacier,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "an empty selection");
    let err = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["a"]),
            &EncryptionChange::Kms {
                key_id: Some("two words".into()),
                bucket_key: false,
            },
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());

    // A folder is not an object: that is the folder's own error.
    let results = s3
        .set_object_storage_class(
            BUCKET,
            &keys(&["dir/", "a"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(results[0].error.as_ref().unwrap().code, ErrorKind::Usage);
    assert!(results[1].changed);
}

// ---- storage class ----

#[tokio::test]
async fn changing_the_storage_class_keeps_headers_tags_encryption_and_redirect() {
    let (store, mock) = store();
    store.put(BUCKET, "k", rich().class("STANDARD"));
    let etag = store.object(BUCKET, "k").unwrap().etag;

    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].changed && results[0].error.is_none());
    assert_eq!(results[0].previous.as_deref(), Some("STANDARD"));
    assert!(results[0].warnings.is_empty());

    let copy = copies(&mock);
    assert_eq!(copy.len(), 1);
    let copy = &copy[0];
    assert_eq!(copy.path, "/bucket/k");
    assert_eq!(copy.header("x-amz-copy-source"), Some("bucket/k"));
    assert_eq!(copy.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(copy.header("x-amz-metadata-directive"), Some("COPY"));
    assert_eq!(copy.header("x-amz-tagging-directive"), Some("COPY"));
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("AES256"));
    assert_eq!(
        copy.header("x-amz-website-redirect-location"),
        Some("/elsewhere")
    );
    assert_eq!(
        copy.header("x-amz-copy-source-if-match"),
        Some(etag.as_str())
    );
    assert!(copy.header("x-amz-acl").is_none());
    assert!(
        copy.header("x-amz-server-side-encryption-customer-key")
            .is_none()
    );

    let after = store.object(BUCKET, "k").unwrap();
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(after.content_type.as_deref(), Some("text/csv"));
    assert_eq!(after.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(after.content_disposition.as_deref(), Some("attachment"));
    assert_eq!(after.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(after.content_language.as_deref(), Some("en"));
    assert_eq!(
        after.expires.as_deref(),
        Some("Fri, 21 Dec 2012 00:00:00 GMT")
    );
    assert_eq!(after.user_meta, map(&[("owner", "team"), ("stage", "raw")]));
    assert_eq!(after.tags, map(&[("env", "prod"), ("team", "data")]));
    assert_eq!(after.encryption, StoredEncryption::S3);
    assert_eq!(after.website_redirect.as_deref(), Some("/elsewhere"));
    assert_ne!(after.etag, etag, "a copy creates a new object");
}

#[tokio::test]
async fn the_class_of_an_object_already_in_that_class_is_not_copied() {
    let (store, mock) = store();
    store.put(BUCKET, "ia", StoredObject::new(1).class("STANDARD_IA"));
    store.put(BUCKET, "std", StoredObject::new(1));
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["ia", "std"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(!results[0].changed && results[0].error.is_none());
    assert_eq!(results[0].previous.as_deref(), Some("STANDARD_IA"));
    assert!(results[1].changed);
    assert_eq!(results[1].previous.as_deref(), Some("STANDARD"));
    assert_eq!(
        copies(&mock).len(),
        1,
        "S3 refuses a copy that changes nothing"
    );

    // Back to STANDARD sends the class explicitly.
    mock.ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["ia"]),
            StorageClass::Standard,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        copies(&mock)[1].header("x-amz-storage-class"),
        Some("STANDARD")
    );
    assert_eq!(store.object(BUCKET, "ia").unwrap().storage_class, None);
}

#[tokio::test]
async fn every_class_of_the_plan_is_sent_by_name() {
    let (store, mock) = store();
    for (index, class) in StorageClass::ALL.into_iter().enumerate() {
        let key = format!("k{index}");
        // Start from a class that differs from the target.
        let start = if class == StorageClass::GlacierIr {
            "STANDARD"
        } else {
            "GLACIER_IR"
        };
        store.put(BUCKET, &key, StoredObject::new(1).class(start));
        let results = mock
            .ops()
            .set_object_storage_class(
                BUCKET,
                std::slice::from_ref(&key),
                class,
                None,
                BatchOptions::default(),
            )
            .await
            .unwrap();
        assert!(results[0].changed, "{class}: {:?}", results[0]);
        let sent = copies(&mock)
            .last()
            .unwrap()
            .header("x-amz-storage-class")
            .map(str::to_owned);
        assert_eq!(sent.as_deref(), Some(class.as_str()));
    }
}

#[tokio::test]
async fn an_archived_object_that_is_not_restored_is_a_conflict_and_nothing_is_copied() {
    let (store, mock) = store();
    store.put(BUCKET, "glacier", StoredObject::new(1).class("GLACIER"));
    store.put(BUCKET, "deep", StoredObject::new(1).class("DEEP_ARCHIVE"));
    store.put(
        BUCKET,
        "restoring",
        StoredObject::new(1)
            .class("GLACIER")
            .restore_header(r#"ongoing-request="true""#),
    );
    store.put(
        BUCKET,
        "restored",
        StoredObject::new(1).class("GLACIER").restore_header(
            r#"ongoing-request="false", expiry-date="Fri, 21 Dec 2112 00:00:00 GMT""#,
        ),
    );
    store.put(BUCKET, "instant", StoredObject::new(1).class("GLACIER_IR"));
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["glacier", "deep", "restoring", "restored", "instant"]),
            StorageClass::Standard,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    for (index, class) in [(0, "GLACIER"), (1, "DEEP_ARCHIVE"), (2, "GLACIER")] {
        let error = results[index]
            .error
            .as_ref()
            .unwrap_or_else(|| panic!("{:?}", results[index]));
        assert_eq!(error.code, ErrorKind::Conflict);
        assert!(error.message.contains(class), "{}", error.message);
        assert!(error.message.contains("restore"), "{}", error.message);
        assert!(!results[index].changed);
    }
    assert!(
        results[3].changed && results[3].error.is_none(),
        "{:?}",
        results[3]
    );
    assert!(
        results[4].changed && results[4].error.is_none(),
        "{:?}",
        results[4]
    );
    // The batch runs objects concurrently, so the copies arrive in any order.
    let mut copied: Vec<String> = copies(&mock).iter().map(|r| r.path.clone()).collect();
    copied.sort();
    assert_eq!(copied, ["/bucket/instant", "/bucket/restored"]);

    // Archiving a readable object is fine.
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["restored"]),
            StorageClass::DeepArchive,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed, "{:?}", results[0]);
}

#[tokio::test]
async fn a_class_change_of_an_archived_object_that_s3_refuses_is_a_conflict_too() {
    // S3 reports InvalidObjectState (HTTP 403) when it cannot read the data.
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1));
    store.intercept(|request| {
        (request.method == "PUT" && request.header("x-amz-copy-source").is_some()).then(|| {
            MockResponse::error(
                403,
                "InvalidObjectState",
                "The operation is not valid for the object's storage class",
            )
        })
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Conflict);
    assert!(error.message.contains("archived"), "{}", error.message);
}

#[tokio::test]
async fn a_kms_object_keeps_its_key_and_bucket_key_when_the_class_changes() {
    let (store, mock) = store();
    let kms = StoredEncryption::Kms {
        key_id: "arn:aws:kms:eu-west-1:000000000000:key/abc".into(),
        bucket_key: true,
    };
    store.put(BUCKET, "k", StoredObject::new(1).encrypted(kms.clone()));
    mock.ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::OnezoneIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let copy = &copies(&mock)[0];
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("aws:kms"));
    assert_eq!(
        copy.header("x-amz-server-side-encryption-aws-kms-key-id"),
        Some("arn:aws:kms:eu-west-1:000000000000:key/abc")
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-bucket-key-enabled"),
        Some("true")
    );
    assert_eq!(store.object(BUCKET, "k").unwrap().encryption, kms);
}

#[tokio::test]
async fn a_dry_run_of_a_class_change_copies_nothing() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1));
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::Glacier,
            None,
            BatchOptions::dry_run(),
        )
        .await
        .unwrap();
    assert!(results[0].changed);
    assert!(copies(&mock).is_empty());
    assert!(mock.requests().iter().all(|r| r.method == "HEAD"));
    assert_eq!(store.object(BUCKET, "k").unwrap().storage_class, None);
}

#[tokio::test]
async fn a_concurrent_overwrite_is_a_conflict_and_changes_nothing() {
    // The source is pinned by its ETag: when the object is overwritten between
    // the HEAD and the copy, S3 answers 412 and the stale edit is not applied.
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1).content_type("text/plain"));
    let racing = std::sync::Arc::clone(&store);
    store.intercept(move |request| {
        if request.method == "PUT" && request.header("x-amz-copy-source").is_some() {
            racing.put(BUCKET, "k", StoredObject::new(2).content_type("text/html"));
        }
        None
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Conflict);
    assert!(
        error.message.contains("changed while it was being edited"),
        "{}",
        error.message
    );
    let stored = store.object(BUCKET, "k").unwrap();
    assert_eq!(stored.size, 2);
    assert_eq!(stored.content_type.as_deref(), Some("text/html"));
    assert_eq!(stored.storage_class, None);
}

// ---- ACL preservation ----

#[tokio::test]
async fn the_acl_a_copy_would_drop_is_applied_again() {
    let (store, mock) = store();
    store.put(BUCKET, "public", StoredObject::new(1).public_read());
    store.put(BUCKET, "private", StoredObject::new(1));
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["public", "private"]),
            StorageClass::StandardIa,
            None,
            BatchOptions {
                concurrency: 1,
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert!(
        results.iter().all(|r| r.changed && r.warnings.is_empty()),
        "{results:?}"
    );
    // The copy reset the ACL; the grants were put back.
    let restored = store.object(BUCKET, "public").unwrap();
    assert_eq!(restored.acl, bucketbridge_tests::canned_acl("public-read"));
    let puts = with_query(&mock, "PUT", "acl");
    assert_eq!(
        puts.len(),
        1,
        "only the public object needs its ACL restored"
    );
    assert_eq!(puts[0].path, "/bucket/public");
    let body = puts[0].body_text();
    assert!(
        body.contains("http://acs.amazonaws.com/groups/global/AllUsers"),
        "{body}"
    );
    assert!(body.contains("<Permission>READ</Permission>"), "{body}");
    assert!(
        body.contains(&format!("<ID>{}</ID>", bucketbridge_tests::OWNER_ID)),
        "{body}"
    );
    assert!(puts[0].header("content-md5").is_some());
    // The ACL is read before the copy, never after.
    let order: Vec<&str> = mock
        .requests()
        .iter()
        .filter(|r| r.path == "/bucket/public")
        .map(
            |r| match (r.method.as_str(), r.query_param("acl").is_some()) {
                ("HEAD", _) => "head",
                ("GET", true) => "read-acl",
                ("PUT", true) => "write-acl",
                ("PUT", false) => "copy",
                _ => "other",
            },
        )
        .collect();
    assert_eq!(order, ["head", "read-acl", "copy", "write-acl"]);
}

#[tokio::test]
async fn an_acl_that_cannot_be_applied_again_is_a_warning_not_a_failure() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1).public_read());
    store.intercept(|request| {
        (request.method == "PUT" && request.query_param("acl").is_some())
            .then(|| MockResponse::error(403, "AccessDenied", "Access Denied"))
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed && results[0].error.is_none());
    assert_eq!(results[0].warnings.len(), 1);
    assert!(
        results[0].warnings[0].contains("ACL"),
        "{:?}",
        results[0].warnings
    );
    assert_eq!(
        store.object(BUCKET, "k").unwrap().storage_class.as_deref(),
        Some("STANDARD_IA")
    );
}

#[tokio::test]
async fn servers_without_acls_lose_nothing_and_warn_about_nothing() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1).public_read());
    store.intercept(|request| {
        (request.query_param("acl").is_some())
            .then(|| MockResponse::error(501, "NotImplemented", "ACLs are not supported"))
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].warnings.is_empty(),
        "{results:?}"
    );
    assert!(with_query(&mock, "PUT", "acl").is_empty());
}

// ---- encryption ----

#[tokio::test]
async fn encryption_changes_send_the_encryption_headers_and_keep_the_class() {
    let (store, mock) = store();
    store.put(BUCKET, "plain", rich().encrypted(StoredEncryption::None));
    store.put(BUCKET, "to-kms", rich().encrypted(StoredEncryption::S3));
    store.put(BUCKET, "to-c", rich().encrypted(StoredEncryption::S3));
    let s3 = mock.ops();

    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["plain"]),
            &EncryptionChange::S3,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(results[0].previous.as_deref(), Some("none"));
    let copy = &copies(&mock)[0];
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("AES256"));
    // The new object keeps the class, which a bare copy would reset.
    assert_eq!(copy.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(copy.header("x-amz-metadata-directive"), Some("COPY"));
    assert_eq!(copy.header("x-amz-tagging-directive"), Some("COPY"));
    assert!(
        copy.header("x-amz-server-side-encryption-customer-key")
            .is_none()
    );
    let after = store.object(BUCKET, "plain").unwrap();
    assert_eq!(after.encryption, StoredEncryption::S3);
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(after.tags, map(&[("env", "prod"), ("team", "data")]));
    assert_eq!(after.user_meta, map(&[("owner", "team"), ("stage", "raw")]));

    let kms = EncryptionChange::Kms {
        key_id: Some("alias/data".into()),
        bucket_key: true,
    };
    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["to-kms"]),
            &kms,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed, "{:?}", results[0]);
    assert_eq!(results[0].previous.as_deref(), Some("s3"));
    let copy = &copies(&mock)[1];
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("aws:kms"));
    assert_eq!(
        copy.header("x-amz-server-side-encryption-aws-kms-key-id"),
        Some("alias/data")
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-bucket-key-enabled"),
        Some("true")
    );
    assert_eq!(
        store.object(BUCKET, "to-kms").unwrap().encryption,
        StoredEncryption::Kms {
            key_id: "alias/data".into(),
            bucket_key: true
        }
    );

    // The AWS managed key: no key ID is sent.
    s3.set_object_encryption(
        BUCKET,
        &keys(&["plain"]),
        &EncryptionChange::Kms {
            key_id: None,
            bucket_key: false,
        },
        None,
        BatchOptions::default(),
    )
    .await
    .unwrap();
    let copy = &copies(&mock)[2];
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("aws:kms"));
    assert!(
        copy.header("x-amz-server-side-encryption-aws-kms-key-id")
            .is_none()
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-bucket-key-enabled"),
        Some("false")
    );

    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["to-c"]),
            &EncryptionChange::Customer(key_a()),
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed, "{:?}", results[0]);
    let copy = &copies(&mock)[3];
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-algorithm"),
        Some("AES256")
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-key"),
        Some("ERERERERERERERERERERERERERERERERERERERERERE=")
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_A).as_str())
    );
    assert!(copy.header("x-amz-server-side-encryption").is_none());
    assert!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-key")
            .is_none()
    );
    assert_eq!(
        store.object(BUCKET, "to-c").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_A)
        }
    );
}

#[tokio::test]
async fn an_object_that_already_has_the_encryption_is_not_copied() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "s3",
        StoredObject::new(1).encrypted(StoredEncryption::S3),
    );
    store.put(
        BUCKET,
        "kms",
        StoredObject::new(1).encrypted(StoredEncryption::Kms {
            key_id: "alias/data".into(),
            bucket_key: true,
        }),
    );
    store.put(BUCKET, "c", StoredObject::new(1).encrypted_with_key(&KEY_A));
    let s3 = mock.ops();
    let same_s3 = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["s3"]),
            &EncryptionChange::S3,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let same_kms = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["kms"]),
            &EncryptionChange::Kms {
                key_id: Some("alias/data".into()),
                bucket_key: true,
            },
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let same_key = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["c"]),
            &EncryptionChange::Customer(key_a()),
            Some(&key_a()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    for results in [same_s3, same_kms, same_key] {
        assert!(
            !results[0].changed && results[0].error.is_none(),
            "{:?}",
            results[0]
        );
    }
    assert!(copies(&mock).is_empty());
}

#[tokio::test]
async fn changing_a_customer_key_sends_the_old_key_for_the_source_and_the_new_one() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "k",
        rich().encrypted_with_key(&KEY_A).class("STANDARD_IA"),
    );
    let s3 = mock.ops();
    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["k"]),
            &EncryptionChange::Customer(key_b()),
            Some(&key_a()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(results[0].previous.as_deref(), Some("c"));

    // The HEAD that looks at the object already carries the old key.
    let head = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "HEAD")
        .unwrap();
    assert_eq!(
        head.header("x-amz-server-side-encryption-customer-algorithm"),
        Some("AES256")
    );
    assert_eq!(
        head.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_A).as_str())
    );

    let copy = &copies(&mock)[0];
    // The source is read with the old key (32 bytes of 0x11 in base64) ...
    assert_eq!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-algorithm"),
        Some("AES256")
    );
    assert_eq!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-key"),
        Some("ERERERERERERERERERERERERERERERERERERERERERE=")
    );
    // ... and the new object is written with the new one (0x22).
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-algorithm"),
        Some("AES256")
    );
    assert_eq!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_A).as_str())
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-key"),
        Some("IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=")
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_B).as_str())
    );
    let after = store.object(BUCKET, "k").unwrap();
    assert_eq!(
        after.encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_B)
        }
    );
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(after.tags, map(&[("env", "prod"), ("team", "data")]));

    // And from the customer key to SSE-S3: the source key, no destination key.
    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["k"]),
            &EncryptionChange::S3,
            Some(&key_b()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed, "{:?}", results[0]);
    let copy = &copies(&mock)[1];
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("AES256"));
    assert_eq!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_B).as_str())
    );
    assert!(
        copy.header("x-amz-server-side-encryption-customer-key")
            .is_none()
    );
    assert_eq!(
        store.object(BUCKET, "k").unwrap().encryption,
        StoredEncryption::S3
    );
}

#[tokio::test]
async fn a_class_change_of_a_customer_key_object_keeps_the_key() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1).encrypted_with_key(&KEY_A));
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["k"]),
            StorageClass::StandardIa,
            Some(&key_a()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    let copy = &copies(&mock)[0];
    assert_eq!(
        copy.header("x-amz-copy-source-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_A).as_str())
    );
    assert_eq!(
        copy.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_A).as_str())
    );
    let after = store.object(BUCKET, "k").unwrap();
    assert_eq!(
        after.encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_A)
        }
    );
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
}

#[tokio::test]
async fn a_wrong_customer_key_fails_loudly_and_changes_nothing() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "secret",
        StoredObject::new(1)
            .encrypted_with_key(&KEY_A)
            .tag("k", "v"),
    );
    store.put(BUCKET, "plain", StoredObject::new(1));
    let s3 = mock.ops();

    // Reading the details with another key: HTTP 403, said in plain words.
    let err = s3
        .object_details(BUCKET, "secret", Some(&key_b()))
        .await
        .unwrap_err();
    assert_eq!(err.service().and_then(|s| s.status()), Some(403), "{err}");
    let text = err.to_string();
    assert!(text.contains("SSE-C") && text.contains("refused"), "{text}");
    assert!(text.contains("s3://bucket/secret"), "{text}");
    assert!(
        !text.contains("IiIiIi") && !text.contains("ERERER"),
        "the key is never in a message: {text}"
    );

    // Without any key the object cannot be read either.
    let err = s3.object_details(BUCKET, "secret", None).await.unwrap_err();
    assert_eq!(err.service().and_then(|s| s.status()), Some(400), "{err}");
    assert!(err.to_string().contains("SSE-C"), "{err}");

    // A key for an object that has none is rejected too.
    let err = s3
        .object_details(BUCKET, "plain", Some(&key_a()))
        .await
        .unwrap_err();
    assert_eq!(err.service().and_then(|s| s.status()), Some(400), "{err}");
    assert!(
        err.to_string()
            .contains("may not be encrypted with a customer key"),
        "{err}"
    );

    // Edits fail for that object only, and nothing is copied.
    let results = s3
        .set_object_storage_class(
            BUCKET,
            &keys(&["secret", "plain"]),
            StorageClass::StandardIa,
            Some(&key_b()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().expect("the wrong key must fail");
    assert!(!results[0].changed);
    assert!(error.message.contains("SSE-C"), "{}", error.message);
    // The plain object rejected the key as well (it has none).
    assert!(results[1].error.is_some());
    let results = s3
        .set_object_encryption(
            BUCKET,
            &keys(&["secret"]),
            &EncryptionChange::S3,
            Some(&key_b()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].error.as_ref().unwrap().message.contains("SSE-C"));
    let results = s3
        .update_object_headers(
            BUCKET,
            &keys(&["secret"]),
            &HeaderChanges {
                content_type: HeaderChange::Set {
                    value: "text/plain".into(),
                },
                ..HeaderChanges::default()
            },
            Some(&key_b()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].error.is_some());
    assert!(
        copies(&mock).is_empty(),
        "no copy may be sent with a wrong key"
    );
    let secret = store.object(BUCKET, "secret").unwrap();
    assert_eq!(
        secret.encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_A)
        }
    );
    assert_eq!(secret.storage_class, None);
    assert_eq!(secret.content_type, None);

    // With the right key everything works, and the details show the key's MD5
    // (never the key).
    let details = s3
        .object_details(BUCKET, "secret", Some(&key_a()))
        .await
        .unwrap();
    assert_eq!(
        details.head.meta.sse,
        Sse::Customer {
            key_md5: md5_of(&KEY_A)
        }
    );
    assert_eq!(details.head.meta.tags, map(&[("k", "v")]));
    let results = s3
        .set_object_storage_class(
            BUCKET,
            &keys(&["secret"]),
            StorageClass::StandardIa,
            Some(&key_a()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
}

#[tokio::test]
async fn a_wrong_source_key_on_the_copy_itself_is_refused_with_the_same_message() {
    // The HEAD is answered (a server that does not check it), the copy is not.
    let (store, mock) = store();
    store.put(
        BUCKET,
        "secret",
        StoredObject::new(1).encrypted_with_key(&KEY_A),
    );
    store.intercept(|request| {
        (request.method == "HEAD").then(|| {
            MockResponse::empty(200)
                .with_header("content-length", "1")
                .with_header("etag", "\"e\"")
                .with_header("x-amz-server-side-encryption-customer-algorithm", "AES256")
                .with_header(
                    "x-amz-server-side-encryption-customer-key-MD5",
                    &md5_of(&KEY_B),
                )
        })
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["secret"]),
            StorageClass::StandardIa,
            Some(&key_b()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Auth);
    assert!(error.message.contains("SSE-C"), "{}", error.message);
    assert!(error.message.contains("refused"), "{}", error.message);
    assert_eq!(
        store.object(BUCKET, "secret").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_A)
        }
    );
}

// ---- headers and user metadata ----

#[tokio::test]
async fn editing_headers_replaces_only_what_is_named_and_keeps_everything_else() {
    let (store, mock) = store();
    store.put(BUCKET, "k", rich());
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "application/json".into(),
        },
        cache_control: HeaderChange::Remove,
        user_meta_set: map(&[("Stage", "clean"), ("new", "1")]),
        user_meta_remove: BTreeSet::from(["owner".to_owned()]),
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );

    let copy = &copies(&mock)[0];
    assert_eq!(copy.header("x-amz-metadata-directive"), Some("REPLACE"));
    assert_eq!(copy.header("x-amz-tagging-directive"), Some("COPY"));
    assert_eq!(copy.header("content-type"), Some("application/json"));
    assert!(copy.header("cache-control").is_none(), "removed");
    // Not named: sent again, because REPLACE drops everything it is not given.
    assert_eq!(copy.header("content-disposition"), Some("attachment"));
    assert_eq!(copy.header("content-encoding"), Some("gzip"));
    assert_eq!(copy.header("content-language"), Some("en"));
    assert_eq!(
        copy.header("expires"),
        Some("Fri, 21 Dec 2012 00:00:00 GMT")
    );
    assert_eq!(copy.header("x-amz-meta-stage"), Some("clean"));
    assert_eq!(copy.header("x-amz-meta-new"), Some("1"));
    assert!(copy.header("x-amz-meta-owner").is_none(), "removed");
    // Storage class, encryption and redirect are kept too.
    assert_eq!(copy.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(copy.header("x-amz-server-side-encryption"), Some("AES256"));
    assert_eq!(
        copy.header("x-amz-website-redirect-location"),
        Some("/elsewhere")
    );

    let after = store.object(BUCKET, "k").unwrap();
    assert_eq!(after.content_type.as_deref(), Some("application/json"));
    assert_eq!(after.cache_control, None);
    assert_eq!(after.content_disposition.as_deref(), Some("attachment"));
    assert_eq!(after.user_meta, map(&[("new", "1"), ("stage", "clean")]));
    assert_eq!(after.tags, map(&[("env", "prod"), ("team", "data")]));
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));
    assert_eq!(after.encryption, StoredEncryption::S3);
    assert_eq!(after.website_redirect.as_deref(), Some("/elsewhere"));
}

#[tokio::test]
async fn a_header_patch_applies_to_each_object_on_top_of_its_own_headers() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "a",
        StoredObject::new(1)
            .content_type("text/plain")
            .meta("x", "1"),
    );
    store.put(
        BUCKET,
        "b",
        StoredObject::new(1)
            .content_type("image/png")
            .meta("y", "2"),
    );
    let changes = HeaderChanges {
        cache_control: HeaderChange::Set {
            value: "public, max-age=3600".into(),
        },
        user_meta_set: map(&[("batch", "7")]),
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["a", "b"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results.iter().all(|r| r.changed));
    let a = store.object(BUCKET, "a").unwrap();
    let b = store.object(BUCKET, "b").unwrap();
    assert_eq!(
        (a.content_type.as_deref(), b.content_type.as_deref()),
        (Some("text/plain"), Some("image/png"))
    );
    assert_eq!(a.user_meta, map(&[("batch", "7"), ("x", "1")]));
    assert_eq!(b.user_meta, map(&[("batch", "7"), ("y", "2")]));
    assert_eq!(a.cache_control.as_deref(), Some("public, max-age=3600"));
    assert_eq!(b.cache_control.as_deref(), Some("public, max-age=3600"));
}

#[tokio::test]
async fn headers_that_already_are_as_wanted_are_not_copied() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "k",
        StoredObject::new(1)
            .content_type("text/plain")
            .meta("a", "1"),
    );
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "text/plain".into(),
        },
        user_meta_set: map(&[("A", "1")]),
        user_meta_remove: BTreeSet::from(["absent".to_owned()]),
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(!results[0].changed && results[0].error.is_none());
    assert!(copies(&mock).is_empty());
}

#[tokio::test]
async fn removing_the_content_type_leaves_the_s3_default() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1).content_type("text/plain"));
    let changes = HeaderChanges {
        content_type: HeaderChange::Remove,
        ..HeaderChanges::default()
    };
    mock.ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(copies(&mock)[0].header("content-type").is_none());
    let details = mock.ops().object_details(BUCKET, "k", None).await.unwrap();
    assert_eq!(
        details.head.meta.content_type.as_deref(),
        Some("binary/octet-stream")
    );
}

#[tokio::test]
async fn user_metadata_over_the_limit_fails_for_that_object_only() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "full",
        StoredObject::new(1).meta("big", &"x".repeat(1500)),
    );
    store.put(BUCKET, "empty", StoredObject::new(1));
    let changes = HeaderChanges {
        user_meta_set: map(&[("more", &"y".repeat(600))]),
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["full", "empty"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Usage);
    assert!(error.message.contains("2048"), "{}", error.message);
    assert!(results[1].changed);
    assert_eq!(copies(&mock).len(), 1);
}

#[tokio::test]
async fn metadata_s3_could_not_return_is_never_dropped() {
    // x-amz-missing-meta counts user metadata that is not plain ASCII and
    // therefore not returned: a REPLACE copy would delete it.
    let mock = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => MockResponse::empty(200)
            .with_header("content-length", "5")
            .with_header("etag", "\"e\"")
            .with_header("content-type", "text/plain")
            .with_header("x-amz-missing-meta", "1"),
        _ => MockResponse::error(400, "Unexpected", &request.path),
    });
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "text/html".into(),
        },
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["k"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Conflict);
    assert!(error.message.contains("user metadata"), "{}", error.message);
    assert_eq!(count(&mock, "PUT"), 0);
}

// ---- canned ACLs ----

#[tokio::test]
async fn canned_acls_are_read_and_applied() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1));
    let s3 = mock.ops();
    let acl = s3.get_object_acl(BUCKET, "k").await.unwrap();
    assert_eq!(acl.canned, Some(CannedAcl::Private));
    assert_eq!(acl.grants.len(), 1);
    assert_eq!(acl.grants[0].grantee_type, GranteeKind::CanonicalUser);
    assert_eq!(acl.grants[0].permission, AclPermission::FullControl);

    for canned in CannedAcl::ALL {
        let results = s3
            .set_object_acl(BUCKET, &keys(&["k"]), canned, BatchOptions::default())
            .await
            .unwrap();
        assert!(results[0].error.is_none(), "{canned}: {:?}", results[0]);
        if canned != CannedAcl::Private {
            assert!(results[0].changed, "{canned}");
        }
        let put = with_query(&mock, "PUT", "acl").pop();
        if canned != CannedAcl::Private {
            let put = put.unwrap();
            assert_eq!(put.header("x-amz-acl"), Some(canned.as_str()));
            assert!(put.header("content-md5").is_some());
        }
    }
    // The last one applied is bucket-owner-full-control.
    assert_eq!(
        store.object(BUCKET, "k").unwrap().acl,
        bucketbridge_tests::canned_acl("bucket-owner-full-control")
    );

    // Public ACLs read back by name.
    s3.put_object_acl(BUCKET, "k", CannedAcl::PublicReadWrite)
        .await
        .unwrap();
    assert_eq!(
        s3.get_object_acl(BUCKET, "k").await.unwrap().canned,
        Some(CannedAcl::PublicReadWrite)
    );
    s3.put_object_acl(BUCKET, "k", CannedAcl::AuthenticatedRead)
        .await
        .unwrap();
    assert_eq!(
        s3.get_object_acl(BUCKET, "k").await.unwrap().canned,
        Some(CannedAcl::AuthenticatedRead)
    );
    // The bucket-owner ACLs are grants to another account: custom.
    s3.put_object_acl(BUCKET, "k", CannedAcl::BucketOwnerRead)
        .await
        .unwrap();
    let acl = s3.get_object_acl(BUCKET, "k").await.unwrap();
    assert_eq!(acl.canned, None);
    assert_eq!(acl.summary(), "custom");
    assert_eq!(acl.grants.len(), 2);
}

#[tokio::test]
async fn setting_the_acl_an_object_already_has_sends_nothing_and_dry_runs_write_nothing() {
    let (store, mock) = store();
    store.put(BUCKET, "pub", StoredObject::new(1).public_read());
    store.put(BUCKET, "priv", StoredObject::new(1));
    let s3 = mock.ops();
    let results = s3
        .set_object_acl(
            BUCKET,
            &keys(&["pub", "priv"]),
            CannedAcl::PublicRead,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(!results[0].changed);
    assert_eq!(results[0].previous.as_deref(), Some("public-read"));
    assert!(results[1].changed);
    assert_eq!(results[1].previous.as_deref(), Some("private"));
    assert_eq!(with_query(&mock, "PUT", "acl").len(), 1);

    store.put(BUCKET, "other", StoredObject::new(1));
    let results = s3
        .set_object_acl(
            BUCKET,
            &keys(&["other"]),
            CannedAcl::PublicRead,
            BatchOptions::dry_run(),
        )
        .await
        .unwrap();
    assert!(results[0].changed);
    assert_eq!(
        with_query(&mock, "PUT", "acl").len(),
        1,
        "a dry run writes nothing"
    );
}

#[tokio::test]
async fn a_bucket_that_enforces_owner_ownership_says_that_acls_are_disabled() {
    let (store, mock) = store();
    store.disable_acls(BUCKET);
    store.put(BUCKET, "k", StoredObject::new(1));
    let s3 = mock.ops();

    // Reading still works and shows the default ACL.
    assert_eq!(
        s3.get_object_acl(BUCKET, "k").await.unwrap().canned,
        Some(CannedAcl::Private)
    );

    let err = s3
        .put_object_acl(BUCKET, "k", CannedAcl::PublicRead)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    let text = err.to_string();
    assert!(text.contains("ACLs are disabled"), "{text}");
    assert!(text.contains("BucketOwnerEnforced"), "{text}");
    assert!(text.contains("'bucket'"), "{text}");

    let results = s3
        .set_object_acl(
            BUCKET,
            &keys(&["k"]),
            CannedAcl::PublicRead,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Conflict);
    assert!(
        error.message.contains("ACLs are disabled"),
        "{}",
        error.message
    );

    // The default ACL is the private one: nothing to do, nothing to fail.
    let results = s3
        .set_object_acl(
            BUCKET,
            &keys(&["k"]),
            CannedAcl::Private,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(!results[0].changed && results[0].error.is_none());

    // An in-place copy on such a bucket never tries to restore an ACL.
    store.put(BUCKET, "j", StoredObject::new(1).public_read());
    let results = s3
        .set_object_storage_class(
            BUCKET,
            &keys(&["j"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].warnings.is_empty(),
        "{results:?}"
    );
    assert!(
        with_query(&mock, "PUT", "acl")
            .iter()
            .all(|r| r.path != "/bucket/j")
    );
}

#[tokio::test]
async fn a_blocked_public_acl_hints_at_block_public_access() {
    let (store, mock) = store();
    store.put(BUCKET, "k", StoredObject::new(1));
    store.intercept(|request| {
        (request.method == "PUT" && request.query_param("acl").is_some())
            .then(|| MockResponse::error(403, "AccessDenied", "Access Denied"))
    });
    let err = mock
        .ops()
        .put_object_acl(BUCKET, "k", CannedAcl::PublicRead)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Block Public Access"), "{err}");
    let err = mock
        .ops()
        .put_object_acl(BUCKET, "k", CannedAcl::BucketOwnerFullControl)
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("Block Public Access"), "{err}");
}

// ---- objects above 5 GiB ----

#[tokio::test]
async fn an_object_above_5_gib_is_changed_with_a_multipart_copy_that_keeps_everything() {
    let (store, mock) = store();
    let mut big = rich().encrypted(StoredEncryption::Kms {
        key_id: "alias/data".into(),
        bucket_key: true,
    });
    big.size = 6 * GIB;
    store.put(BUCKET, "big.bin", big);
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["big.bin"]),
            StorageClass::GlacierIr,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    assert!(copies(&mock).is_empty(), "no single CopyObject above 5 GiB");

    let creates = with_query(&mock, "POST", "uploads");
    assert_eq!(creates.len(), 1);
    let create = &creates[0];
    assert_eq!(create.header("x-amz-storage-class"), Some("GLACIER_IR"));
    assert_eq!(create.header("content-type"), Some("text/csv"));
    assert_eq!(create.header("cache-control"), Some("max-age=60"));
    assert_eq!(create.header("content-language"), Some("en"));
    assert_eq!(create.header("x-amz-meta-owner"), Some("team"));
    assert_eq!(
        create.header("x-amz-website-redirect-location"),
        Some("/elsewhere")
    );
    assert_eq!(
        create.header("x-amz-server-side-encryption"),
        Some("aws:kms")
    );
    assert_eq!(
        create.header("x-amz-server-side-encryption-aws-kms-key-id"),
        Some("alias/data")
    );
    assert_eq!(
        create.header("x-amz-server-side-encryption-bucket-key-enabled"),
        Some("true")
    );
    assert!(create.header("x-amz-tagging").unwrap().contains("env=prod"));

    let parts: Vec<RecordedRequest> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT" && r.query_param("partNumber").is_some())
        .collect();
    assert_eq!(parts.len(), 24, "6 GiB in 256 MiB parts");
    assert_eq!(with_query(&mock, "POST", "uploadId").len(), 1);

    let after = store.object(BUCKET, "big.bin").unwrap();
    assert_eq!(after.size, 6 * GIB);
    assert_eq!(after.storage_class.as_deref(), Some("GLACIER_IR"));
    assert_eq!(after.content_type.as_deref(), Some("text/csv"));
    assert_eq!(after.tags, map(&[("env", "prod"), ("team", "data")]));
    assert_eq!(after.user_meta, map(&[("owner", "team"), ("stage", "raw")]));
    assert_eq!(after.website_redirect.as_deref(), Some("/elsewhere"));
    assert_eq!(
        after.encryption,
        StoredEncryption::Kms {
            key_id: "alias/data".into(),
            bucket_key: true
        }
    );
}

#[tokio::test]
async fn a_multipart_header_edit_replaces_the_headers_of_the_new_upload() {
    let (store, mock) = store();
    let mut big = rich();
    big.size = 6 * GIB;
    store.put(BUCKET, "big.bin", big);
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "application/zip".into(),
        },
        cache_control: HeaderChange::Remove,
        user_meta_set: map(&[("stage", "done")]),
        user_meta_remove: BTreeSet::from(["owner".to_owned()]),
        ..HeaderChanges::default()
    };
    let results = mock
        .ops()
        .update_object_headers(
            BUCKET,
            &keys(&["big.bin"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    let create = &with_query(&mock, "POST", "uploads")[0];
    assert_eq!(create.header("content-type"), Some("application/zip"));
    assert!(create.header("cache-control").is_none());
    assert_eq!(create.header("x-amz-meta-stage"), Some("done"));
    assert!(create.header("x-amz-meta-owner").is_none());
    assert_eq!(create.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(create.header("content-disposition"), Some("attachment"));
    let after = store.object(BUCKET, "big.bin").unwrap();
    assert_eq!(after.content_type.as_deref(), Some("application/zip"));
    assert_eq!(after.user_meta, map(&[("stage", "done")]));
    assert_eq!(after.cache_control, None);
}

#[tokio::test]
async fn a_multipart_copy_of_a_customer_key_object_carries_both_keys_on_every_part() {
    let (store, mock) = store();
    let mut big = StoredObject::new(1).encrypted_with_key(&KEY_A);
    big.size = 6 * GIB;
    store.put(BUCKET, "big.bin", big);
    let results = mock
        .ops()
        .set_object_encryption(
            BUCKET,
            &keys(&["big.bin"]),
            &EncryptionChange::Customer(key_b()),
            Some(&key_a()),
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    let create = &with_query(&mock, "POST", "uploads")[0];
    assert_eq!(
        create.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_B).as_str())
    );
    let parts: Vec<RecordedRequest> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT" && r.query_param("partNumber").is_some())
        .collect();
    assert_eq!(parts.len(), 24);
    for part in &parts {
        assert_eq!(
            part.header("x-amz-copy-source-server-side-encryption-customer-key-md5"),
            Some(md5_of(&KEY_A).as_str())
        );
        assert_eq!(
            part.header("x-amz-server-side-encryption-customer-key-md5"),
            Some(md5_of(&KEY_B).as_str())
        );
        assert!(part.header("x-amz-copy-source-if-match").is_some());
    }
    let complete = &with_query(&mock, "POST", "uploadId")[0];
    assert_eq!(
        complete.header("x-amz-server-side-encryption-customer-key-md5"),
        Some(md5_of(&KEY_B).as_str())
    );
    assert_eq!(
        store.object(BUCKET, "big.bin").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: md5_of(&KEY_B)
        }
    );
}

#[tokio::test]
async fn a_failed_multipart_edit_aborts_its_upload_and_leaves_the_object_alone() {
    let (store, mock) = store();
    let mut big = StoredObject::new(1).content_type("text/plain");
    big.size = 6 * GIB;
    store.put(BUCKET, "big.bin", big);
    store.intercept(|request| {
        (request.method == "PUT" && request.query_param("partNumber").as_deref() == Some("3"))
            .then(|| MockResponse::error(500, "InternalError", "part three failed"))
    });
    let results = mock
        .ops()
        .set_object_storage_class(
            BUCKET,
            &keys(&["big.bin"]),
            StorageClass::StandardIa,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    let error = results[0].error.as_ref().unwrap();
    assert!(
        error.message.contains("part three failed"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("copying part 3"),
        "{}",
        error.message
    );
    assert_eq!(
        with_query(&mock, "DELETE", "uploadId").len(),
        1,
        "the upload is aborted"
    );
    assert!(
        with_query(&mock, "POST", "uploadId").is_empty(),
        "never completed"
    );
    let stored = store.object(BUCKET, "big.bin").unwrap();
    assert_eq!(stored.storage_class, None);
    assert_eq!(stored.size, 6 * GIB);
}

// ---- requester pays ----

#[tokio::test]
async fn requester_pays_is_sent_where_the_api_has_it() {
    let (store, mock) = store();
    store.put(
        BUCKET,
        "k",
        StoredObject::new(1).public_read().tag("a", "1"),
    );
    let s3 = mock.ops_with(
        &bucketbridge_core::ClientOptions {
            max_attempts: 1,
            ..bucketbridge_core::ClientOptions::default()
        },
        true,
    );
    s3.object_details(BUCKET, "k", None).await.unwrap();
    s3.put_object_tags(BUCKET, "k", &map(&[("b", "2")]))
        .await
        .unwrap();
    s3.put_object_acl(BUCKET, "k", CannedAcl::Private)
        .await
        .unwrap();
    s3.set_object_storage_class(
        BUCKET,
        &keys(&["k"]),
        StorageClass::StandardIa,
        None,
        BatchOptions::default(),
    )
    .await
    .unwrap();
    s3.delete_object_tags(BUCKET, "k").await.unwrap();
    for request in mock.requests() {
        let is_delete_tagging =
            request.method == "DELETE" && request.query_param("tagging").is_some();
        let expected = (!is_delete_tagging).then_some("requester");
        assert_eq!(
            request.header("x-amz-request-payer"),
            expected,
            "{} {}?{}",
            request.method,
            request.path,
            request.query
        );
    }
}

// ---- the real servers ----

#[tokio::test]
async fn s3s_fs_serves_the_head_and_reports_what_it_lacks_per_part() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("a.txt")
        .body(ByteStream::from_static(b"hello"))
        .content_type("text/plain")
        .metadata("owner", "team")
        .send()
        .await
        .unwrap();
    let details = s3
        .ops()
        .object_details(&bucket, "a.txt", None)
        .await
        .unwrap();
    assert_eq!(details.head.size, 5);
    assert_eq!(
        details.head.meta.content_type.as_deref(),
        Some("text/plain")
    );
    assert_eq!(details.head.meta.user_meta, map(&[("owner", "team")]));
    assert_eq!(details.head.storage_class.as_deref(), Some("STANDARD"));
    // s3s-fs implements neither tagging nor ACLs (docs/testing.md); the two
    // failures are kept apart and do not hide the head.
    assert_eq!(
        details.tags_error.expect("tagging is not implemented").code,
        ErrorKind::Remote
    );
    assert_eq!(
        details.acl_error.expect("ACLs are not implemented").code,
        ErrorKind::Remote
    );
    assert!(details.head.meta.tags.is_empty() && details.acl.is_none());
}

#[tokio::test]
async fn headers_are_edited_in_place_on_a_real_server() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("dir/page.html")
        .body(ByteStream::from_static(b"<p>hi</p>"))
        .content_type("text/plain")
        .cache_control("no-store")
        .metadata("owner", "team")
        .metadata("stage", "raw")
        .send()
        .await
        .unwrap();
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "text/html".into(),
        },
        cache_control: HeaderChange::Remove,
        user_meta_set: map(&[("stage", "done")]),
        user_meta_remove: BTreeSet::from(["owner".to_owned()]),
        ..HeaderChanges::default()
    };
    let results = s3
        .ops()
        .update_object_headers(
            &bucket,
            &keys(&["dir/page.html"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    let head = s3
        .ops()
        .head_object(&bucket, "dir/page.html", None)
        .await
        .unwrap();
    assert_eq!(head.meta.content_type.as_deref(), Some("text/html"));
    assert_eq!(head.meta.cache_control, None);
    assert_eq!(head.meta.user_meta, map(&[("stage", "done")]));
    assert_eq!(s3.get(&bucket, "dir/page.html").await, b"<p>hi</p>");

    // Missing objects and wrong keys are per-key results on a real server too.
    let results = s3
        .ops()
        .update_object_headers(
            &bucket,
            &keys(&["missing.txt"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(results[0].error.as_ref().unwrap().code, ErrorKind::NotFound);
}

// ---- MinIO ----

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement object tagging"
)]
async fn tags_and_headers_round_trip_on_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("a.txt")
        .body(ByteStream::from_static(b"data"))
        .content_type("text/plain")
        .metadata("owner", "team")
        .send()
        .await
        .unwrap();
    s3.client()
        .put_object()
        .bucket(&bucket)
        .key("b.txt")
        .body(ByteStream::from_static(b"data"))
        .send()
        .await
        .unwrap();
    let ops = s3.ops();
    ops.put_object_tags(&bucket, "a.txt", &map(&[("env", "dev"), ("keep", "k")]))
        .await
        .unwrap();
    assert_eq!(
        ops.get_object_tags(&bucket, "a.txt").await.unwrap(),
        map(&[("env", "dev"), ("keep", "k")])
    );

    // A batch merge: one object has tags, the other none.
    let edit = TagEdit::Merge {
        set: map(&[("env", "prod")]),
        remove: BTreeSet::from(["keep".to_owned()]),
    };
    let results = ops
        .update_object_tags(
            &bucket,
            &keys(&["a.txt", "b.txt", "gone.txt"]),
            &edit,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(results[0].changed && results[1].changed);
    assert_eq!(results[2].error.as_ref().unwrap().code, ErrorKind::NotFound);
    assert_eq!(
        ops.get_object_tags(&bucket, "a.txt").await.unwrap(),
        map(&[("env", "prod")])
    );
    assert_eq!(
        ops.get_object_tags(&bucket, "b.txt").await.unwrap(),
        map(&[("env", "prod")])
    );

    // Editing the headers keeps the tags (TaggingDirective COPY).
    let changes = HeaderChanges {
        content_type: HeaderChange::Set {
            value: "text/markdown".into(),
        },
        user_meta_set: map(&[("stage", "done")]),
        ..HeaderChanges::default()
    };
    let results = ops
        .update_object_headers(
            &bucket,
            &keys(&["a.txt"]),
            &changes,
            None,
            BatchOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        results[0].changed && results[0].error.is_none(),
        "{:?}",
        results[0]
    );
    let details = ops.object_details(&bucket, "a.txt", None).await.unwrap();
    assert_eq!(
        details.head.meta.content_type.as_deref(),
        Some("text/markdown")
    );
    assert_eq!(
        details.head.meta.user_meta,
        map(&[("owner", "team"), ("stage", "done")])
    );
    assert_eq!(details.head.meta.tags, map(&[("env", "prod")]));
    assert_eq!(s3.get(&bucket, "a.txt").await, b"data");

    ops.delete_object_tags(&bucket, "a.txt").await.unwrap();
    assert!(
        ops.get_object_tags(&bucket, "a.txt")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement ACLs"
)]
async fn the_default_acl_reads_as_private_on_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a.txt", "data").await;
    let acl = s3.ops().get_object_acl(&bucket, "a.txt").await.unwrap();
    assert_eq!(acl.canned, Some(CannedAcl::Private));
}
