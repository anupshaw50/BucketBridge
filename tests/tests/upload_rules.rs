//! Upload rules (`bucketbridge_core::upload_rules`) applied by the upload
//! engine: the request headers of `PutObject` and `CreateMultipartUpload`
//! are checked against the scripted `MockS3`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::engine::{Hooks, TransferEngine, TransferOptions, UploadHeaders};
use bucketbridge_core::settings::MIB;
use bucketbridge_core::upload_rules::{UploadRule, UploadRulesSettings};
use bucketbridge_core::{ClientOptions, S3Uri};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn mock() -> MockS3 {
    MockS3::start(|request| {
        let query = |name: &str| request.query_param(name);
        match request.method.as_str() {
            "POST" if query("uploads").is_some() => MockResponse::xml(format!(
                "<InitiateMultipartUploadResult {NS}><Bucket>b</Bucket><Key>k</Key><UploadId>u1</UploadId></InitiateMultipartUploadResult>"
            )),
            "PUT" if query("partNumber").is_some() => {
                MockResponse::empty(200).with_header("etag", "\"p\"")
            }
            "POST" if query("uploadId").is_some() => MockResponse::xml(format!(
                "<CompleteMultipartUploadResult {NS}><Bucket>b</Bucket><Key>k</Key><ETag>\"w-1\"</ETag></CompleteMultipartUploadResult>"
            )),
            "PUT" => MockResponse::empty(200).with_header("etag", "\"s\""),
            _ => MockResponse::empty(500),
        }
    })
}

fn rules() -> UploadRulesSettings {
    let mut web = UploadRule {
        pattern: "*.html".into(),
        content_type: Some("text/html; charset=utf-8".into()),
        cache_control: Some("max-age=300".into()),
        storage_class: Some("STANDARD_IA".into()),
        sse: Some("aws:kms".into()),
        sse_kms_key_id: Some("alias/web".into()),
        ..UploadRule::default()
    };
    web.headers.insert("x-amz-meta-team".into(), "web".into());
    let any = UploadRule {
        pattern: "*".into(),
        cache_control: Some("no-store".into()),
        ..UploadRule::default()
    };
    UploadRulesSettings {
        enabled: true,
        rules: vec![web, any],
    }
}

async fn upload_one(engine: &TransferEngine, name: &str, size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, vec![7u8; size]).unwrap();
    let dst = S3Uri::new("b", name).unwrap();
    let outcome = engine
        .upload_file(&path, &dst, &Hooks::new(), None, &|_| {})
        .await
        .unwrap();
    assert!(outcome.is_completed(), "{outcome:?}");
}

fn engine(mock: &MockS3, options: TransferOptions) -> TransferEngine {
    let client = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    TransferEngine::new(mock.ops_with(&client, false), options).unwrap()
}

fn first(mock: &MockS3, method: &str) -> RecordedRequest {
    mock.requests()
        .into_iter()
        .find(|r| r.method == method)
        .expect("a request")
}

#[tokio::test]
async fn a_matching_rule_sets_the_headers_of_a_put() {
    let mock = mock();
    let engine = engine(&mock, TransferOptions::default())
        .with_upload_rule_settings(&rules())
        .unwrap();
    upload_one(&engine, "index.HTML", 100).await;
    let put = first(&mock, "PUT");
    assert_eq!(put.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(put.header("cache-control"), Some("max-age=300"));
    assert_eq!(put.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(put.header("x-amz-server-side-encryption"), Some("aws:kms"));
    assert_eq!(
        put.header("x-amz-server-side-encryption-aws-kms-key-id"),
        Some("alias/web")
    );
    assert_eq!(put.header("x-amz-meta-team"), Some("web"));
}

#[tokio::test]
async fn the_first_match_wins_and_unmatched_files_keep_the_guess() {
    let mock = mock();
    let engine = engine(&mock, TransferOptions::default())
        .with_upload_rule_settings(&rules())
        .unwrap();
    upload_one(&engine, "notes.txt", 10).await;
    let put = first(&mock, "PUT");
    // The catch-all rule, not the first: no storage class, no encryption.
    assert_eq!(put.header("cache-control"), Some("no-store"));
    assert_eq!(put.header("content-type"), Some("text/plain"));
    assert!(put.header("x-amz-storage-class").is_none());
    assert!(put.header("x-amz-server-side-encryption").is_none());

    let none = TransferEngine::new(engine.s3().clone(), TransferOptions::default()).unwrap();
    let before = mock.requests().len();
    upload_one(&none, "other.txt", 10).await;
    let put = mock.requests().split_off(before).remove(0);
    assert!(put.header("cache-control").is_none());
}

#[tokio::test]
async fn explicit_headers_win_over_a_rule() {
    let mock = mock();
    let explicit = UploadHeaders {
        cache_control: Some("private".into()),
        storage_class: Some("GLACIER_IR".into()),
        ..UploadHeaders::default()
    };
    let engine = engine(&mock, TransferOptions::default())
        .with_upload_headers(explicit)
        .with_upload_rule_settings(&rules())
        .unwrap();
    upload_one(&engine, "a.html", 10).await;
    let put = first(&mock, "PUT");
    assert_eq!(put.header("cache-control"), Some("private"));
    assert_eq!(put.header("x-amz-storage-class"), Some("GLACIER_IR"));
    // What the caller left open still comes from the rule.
    assert_eq!(put.header("x-amz-meta-team"), Some("web"));
}

#[tokio::test]
async fn a_rule_applies_to_a_multipart_upload_too() {
    let mock = mock();
    let options = TransferOptions {
        multipart_threshold: 5 * MIB,
        part_size: 5 * MIB,
        ..TransferOptions::default()
    };
    let engine = engine(&mock, options)
        .with_upload_rule_settings(&rules())
        .unwrap();
    upload_one(&engine, "big.html", 5 * MIB as usize + 10).await;
    let create = first(&mock, "POST");
    assert_eq!(create.header("cache-control"), Some("max-age=300"));
    assert_eq!(create.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(
        create.header("x-amz-server-side-encryption"),
        Some("aws:kms")
    );
}

#[tokio::test]
async fn a_switched_off_list_changes_nothing() {
    let mock = mock();
    let mut settings = rules();
    settings.enabled = false;
    let engine = engine(&mock, TransferOptions::default())
        .with_upload_rule_settings(&settings)
        .unwrap();
    upload_one(&engine, "a.html", 10).await;
    let put = first(&mock, "PUT");
    assert!(put.header("cache-control").is_none());
    assert!(put.header("x-amz-storage-class").is_none());
}
