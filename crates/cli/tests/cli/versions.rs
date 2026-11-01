//! `bb restore-version` and `bb rm --version-id`

use bucketbridge_tests::MockResponse;

use super::*;

const NS: &str = xml::NS;

fn head(version: Option<&str>) -> MockResponse {
    let mut response = MockResponse::empty(200)
        .with_header("content-length", "5")
        .with_header("etag", "\"etag-1\"")
        .with_header("last-modified", "Wed, 21 Oct 2026 07:28:00 GMT")
        .with_header("content-type", "text/plain");
    if let Some(version) = version {
        response = response.with_header("x-amz-version-id", version);
    }
    response
}

fn unexpected() -> MockResponse {
    MockResponse::error(400, "Unexpected", "unexpected request")
}

/// `doc.txt`: v1 is old, v3 current, `dm` a delete marker, `gone` unknown.
fn bucket_with_versions() -> MockS3 {
    MockS3::start(|request| {
        let version = request.query_param("versionId");
        match (request.method.as_str(), version.as_deref()) {
            ("HEAD", Some("v1")) => head(Some("v1")),
            ("HEAD", Some("v3")) => head(Some("v3")),
            ("HEAD", Some("dm")) => MockResponse::empty(405),
            ("HEAD", Some(_)) => MockResponse::empty(404),
            ("HEAD", None) => head(Some("v3")),
            ("PUT", None) if request.header("x-amz-copy-source").is_some() => MockResponse::xml(
                format!("<CopyObjectResult {NS}><ETag>\"etag-1\"</ETag></CopyObjectResult>"),
            )
            .with_header("x-amz-version-id", "v4"),
            ("DELETE", Some(_)) => MockResponse::empty(204),
            _ => unexpected(),
        }
    })
}

#[tokio::test]
async fn restore_version_makes_an_old_version_current() {
    let mock = bucket_with_versions();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "restore-version",
        "s3://bucket/dir/doc.txt",
        "--version-id",
        "v1",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        "restored version v1 of s3://bucket/dir/doc.txt (new version v4)"
    );
    let copy = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(copy.path, "/bucket/dir/doc.txt");
    assert_eq!(
        copy.header("x-amz-copy-source"),
        Some("bucket/dir/doc.txt?versionId=v1")
    );

    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "restore-version",
        "s3://bucket/dir/doc.txt",
        "--version-id",
        "v1",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "bucket",
            "key": "dir/doc.txt",
            "restored_version_id": "v1",
            "version_id": "v4",
            "etag": "etag-1",
            "size": 5
        })]
    );

    // --quiet hides the success line.
    let out = run(bb_mock(&mock, config.path()).args([
        "-q",
        "restore-version",
        "s3://bucket/dir/doc.txt",
        "--version-id",
        "v1",
    ]));
    out.expect_code(0);
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn restore_version_refuses_what_cannot_be_restored() {
    let mock = bucket_with_versions();
    let config = tempfile::tempdir().unwrap();
    let restore = |args: &[&str]| {
        let mut full = vec!["--json", "restore-version"];
        full.extend_from_slice(args);
        run(bb_mock(&mock, config.path()).args(full))
    };

    // The current version is not restored over itself: a conflict.
    let out = restore(&["s3://bucket/doc.txt", "--version-id", "v3"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "conflict");
    assert!(out.stdout.is_empty());

    // A delete marker has no content: a usage error.
    let out = restore(&["s3://bucket/doc.txt", "--version-id", "dm"]);
    out.expect_code(1);
    assert!(out.stderr.contains("delete marker"), "{}", out.stderr);

    // An unknown version is not found.
    let out = restore(&["s3://bucket/doc.txt", "--version-id", "gone"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");

    // A prefix is not an object, and the version is required.
    restore(&["s3://bucket/dir/", "--version-id", "v1"]).expect_code(1);
    restore(&["s3://bucket", "--version-id", "v1"]).expect_code(1);
    let out = restore(&["s3://bucket/doc.txt"]);
    out.expect_code(1);
    assert!(out.stderr.contains("--version-id"), "{}", out.stderr);
    // Nothing was copied by any of them.
    assert!(mock.requests().iter().all(|r| r.method != "PUT"));
}

#[tokio::test]
async fn rm_version_id_deletes_one_version_for_good() {
    let mock = bucket_with_versions();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "s3://bucket/doc.txt",
        "--version-id",
        "v1",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        "deleted s3://bucket/doc.txt (version v1)"
    );
    let delete = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "DELETE")
        .unwrap();
    assert_eq!(delete.path, "/bucket/doc.txt");
    assert_eq!(delete.query_param("versionId").as_deref(), Some("v1"));

    // A delete marker is removed the same way (its HEAD is a 405).
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--json",
        "s3://bucket/doc.txt",
        "--version-id",
        "dm",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": "bucket",
            "key": "doc.txt",
            "version_id": "dm",
            "dry_run": false,
            "deleted": true,
            "error": null
        })]
    );
}

#[tokio::test]
async fn rm_version_id_dry_run_and_errors_delete_nothing() {
    let mock = bucket_with_versions();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--dry-run",
        "--json",
        "s3://bucket/doc.txt",
        "--version-id",
        "v1",
    ]));
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[0]["dry_run"], true);
    assert_eq!(records[0]["deleted"], false);
    assert_eq!(records[0]["version_id"], "v1");

    // An unknown version is not found; a prefix needs another command.
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--json",
        "s3://bucket/doc.txt",
        "--version-id",
        "gone",
    ]));
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    let out =
        run(bb_mock(&mock, config.path()).args(["rm", "s3://bucket/dir/", "--version-id", "v1"]));
    out.expect_code(1);
    assert!(out.stderr.contains("object key"), "{}", out.stderr);
    for args in [
        vec![
            "rm",
            "s3://bucket/doc.txt",
            "--version-id",
            "v1",
            "--versions",
        ],
        vec!["rm", "-r", "s3://bucket/dir/", "--version-id", "v1"],
    ] {
        run(bb_mock(&mock, config.path()).args(args)).expect_code(1);
    }
    assert!(mock.requests().iter().all(|r| r.method != "DELETE"));
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn minio_restores_and_purges_by_version_id() {
    use bucketbridge_core::aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};

    let env = Env::start_minio().await;
    let bucket = env.s3.new_bucket().await;
    env.s3
        .client()
        .put_bucket_versioning()
        .bucket(&bucket)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap();
    env.s3.put(&bucket, "doc.txt", "one").await;
    env.s3.put(&bucket, "doc.txt", "two").await;
    let uri = format!("s3://{bucket}/doc.txt");
    let listed = env.run(&["ls", "--versions", "--json", &uri]);
    listed.expect_code(0);
    let records = listed.records();
    let oldest = records[1]["version_id"].as_str().unwrap().to_owned();

    env.run(&["restore-version", &uri, "--version-id", &oldest])
        .expect_code(0);
    assert_eq!(env.s3.get(&bucket, "doc.txt").await, b"one");
    let listed = env.run(&["ls", "--versions", "--json", &uri]);
    assert_eq!(listed.records().len(), 3);

    env.run(&["rm", &uri, "--version-id", &oldest])
        .expect_code(0);
    let listed = env.run(&["ls", "--versions", "--json", &uri]);
    assert_eq!(listed.records().len(), 2);
    assert_eq!(env.s3.get(&bucket, "doc.txt").await, b"one");
}
