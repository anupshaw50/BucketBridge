//! `bb rm`

use bucketbridge_cli::i18n::{tr, trf};
use bucketbridge_tests::MockResponse;

use super::*;

async fn folder_bucket(env: &Env) -> String {
    let bucket = env.s3.new_bucket().await;
    for key in [
        "dir/a.txt",
        "dir/sub/b.txt",
        "dir2/c.txt",
        "dirx.txt",
        "top.txt",
    ] {
        env.s3.put(&bucket, key, "x").await;
    }
    bucket
}

#[tokio::test]
async fn rm_deletes_one_object() {
    let env = Env::start().await;
    let bucket = folder_bucket(&env).await;
    let out = env.run(&["rm", &format!("s3://{bucket}/top.txt")]);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "deleted s3://{bucket}/{key}",
            &[("bucket", bucket.as_str()), ("key", "top.txt")]
        )
    );
    assert!(!env.s3.exists(&bucket, "top.txt").await);

    let out = env.run(&["rm", "--json", &format!("s3://{bucket}/dirx.txt")]);
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": bucket,
            "key": "dirx.txt",
            "version_id": null,
            "dry_run": false,
            "deleted": true,
            "error": null
        })]
    );
    assert_eq!(
        env.keys(&bucket).await,
        ["dir/a.txt", "dir/sub/b.txt", "dir2/c.txt"]
    );
}

#[tokio::test]
async fn rm_of_a_missing_object_is_not_found() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let out = env.run(&["rm", "--json", &format!("s3://{bucket}/missing.txt")]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn prefixes_need_recursive_and_recursive_needs_a_prefix() {
    let env = Env::start().await;
    let bucket = folder_bucket(&env).await;
    for args in [
        vec!["rm".to_owned(), format!("s3://{bucket}/dir/")],
        vec!["rm".to_owned(), format!("s3://{bucket}")],
        vec![
            "rm".to_owned(),
            "-r".to_owned(),
            format!("s3://{bucket}/dir"),
        ],
        vec![
            "rm".to_owned(),
            "--versions".to_owned(),
            format!("s3://{bucket}/dir/"),
        ],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = env.run(&args);
        out.expect_code(1);
        assert!(
            out.stderr.contains("--recursive"),
            "{args:?}: {}",
            out.stderr
        );
    }
    assert_eq!(env.keys(&bucket).await.len(), 5);
}

#[tokio::test]
async fn recursive_deletes_only_under_the_prefix() {
    let env = Env::start().await;
    let bucket = folder_bucket(&env).await;
    let out = env.run(&["rm", "-r", "--json", &format!("s3://{bucket}/dir/")]);
    out.expect_code(0);
    let records = out.records();
    let keys: Vec<&str> = records.iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["dir/a.txt", "dir/sub/b.txt"]);
    assert!(
        records
            .iter()
            .all(|r| r["deleted"] == true && r["dry_run"] == false)
    );
    assert_eq!(
        env.keys(&bucket).await,
        ["dir2/c.txt", "dirx.txt", "top.txt"]
    );
}

#[tokio::test]
async fn dry_run_deletes_nothing() {
    let env = Env::start().await;
    let bucket = folder_bucket(&env).await;
    let out = env.run(&["rm", "-r", "--dry-run", &format!("s3://{bucket}/dir/")]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            trf(
                "would delete s3://{bucket}/{key}",
                &[("bucket", bucket.as_str()), ("key", "dir/a.txt")]
            ),
            trf(
                "would delete s3://{bucket}/{key}",
                &[("bucket", bucket.as_str()), ("key", "dir/sub/b.txt")]
            ),
        ]
    );
    let out = env.run(&[
        "rm",
        "--dry-run",
        "--json",
        &format!("s3://{bucket}/top.txt"),
    ]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[0]["dry_run"], true);
    assert_eq!(records[0]["deleted"], false);
    assert_eq!(env.keys(&bucket).await.len(), 5);
}

#[tokio::test]
async fn recursive_on_a_bucket_empties_it() {
    let env = Env::start().await;
    let bucket = folder_bucket(&env).await;
    let out = env.run(&["rm", "-r", "--quiet", &format!("s3://{bucket}/")]);
    out.expect_code(0);
    assert!(out.stdout.is_empty());
    assert!(env.keys(&bucket).await.is_empty());
    env.run(&["rb", &bucket]).expect_code(0);
}

#[tokio::test]
async fn per_key_failures_are_reported_and_set_the_exit_code() {
    let ns = xml::NS;
    let mock = MockS3::start(move |request| match request.method.as_str() {
        "GET" if request.query_param("list-type").as_deref() == Some("2") => {
            MockResponse::xml(xml::objects(&["dir/a.txt", "dir/locked.txt"]))
        }
        "POST" if request.query_param("delete").is_some() => MockResponse::xml(format!(
            "<DeleteResult {ns}><Error><Key>dir/locked.txt</Key><Code>AccessDenied</Code><Message>Access Denied</Message></Error></DeleteResult>"
        )),
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    });
    let config = tempfile::tempdir().unwrap();

    let out = run(bb_mock(&mock, config.path()).args(["rm", "-r", "--json", "s3://bucket/dir/"]));
    out.expect_code(3);
    let records = out.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["key"], "dir/a.txt");
    assert_eq!(records[0]["deleted"], true);
    assert_eq!(records[1]["key"], "dir/locked.txt");
    assert_eq!(records[1]["deleted"], false);
    assert_eq!(records[1]["error"]["code"], "auth");
    let error = out.error_record();
    assert_eq!(error["code"], "auth");
    assert_eq!(
        error["message"],
        batch_summary(1, 2, "objects", "could not be deleted")
    );

    let out = run(bb_mock(&mock, config.path()).args(["rm", "-r", "s3://bucket/dir/"]));
    out.expect_code(3);
    assert_eq!(
        out.lines(),
        [trf(
            "deleted s3://{bucket}/{key}",
            &[("bucket", "bucket"), ("key", "dir/a.txt")]
        )]
    );
    assert!(out.stderr.contains("locked.txt"), "stderr: {}", out.stderr);
    // --quiet hides the successes, never the failures.
    let out = run(bb_mock(&mock, config.path()).args(["rm", "-r", "-q", "s3://bucket/dir/"]));
    out.expect_code(3);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("locked.txt"), "{}", out.stderr);
}

#[tokio::test]
async fn versions_of_one_key_are_deleted_one_by_one() {
    let mock = MockS3::start(|request| match request.method.as_str() {
        "GET" if request.query_param("versions").is_some() => {
            match request.query_param("prefix").as_deref() {
                Some("doc.txt") => MockResponse::xml(xml::versions(
                    &[
                        ("doc.txt", "v2", false, false),
                        ("doc.txt", "dm", true, true),
                        ("doc.txt", "v1", false, false),
                        ("doc.txt.bak", "vb", true, false),
                    ],
                    &[],
                    None,
                )),
                _ => MockResponse::xml(xml::versions(&[], &[], None)),
            }
        }
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    });
    let config = tempfile::tempdir().unwrap();

    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--dry-run",
        "s3://bucket/doc.txt",
    ]));
    out.expect_code(0);
    assert_eq!(out.lines().len(), 3, "{}", out.stdout);
    assert!(
        mock.requests().iter().all(|r| r.method != "DELETE"),
        "a dry run sent a DELETE"
    );

    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--yes",
        "--json",
        "s3://bucket/doc.txt",
    ]));
    out.expect_code(0);
    let records = out.records();
    let mut versions: Vec<&str> = records
        .iter()
        .map(|r| r["version_id"].as_str().unwrap())
        .collect();
    versions.sort_unstable();
    assert_eq!(versions, ["dm", "v1", "v2"]);
    assert!(
        records
            .iter()
            .all(|r| r["key"] == "doc.txt" && r["deleted"] == true)
    );
    let mut deleted: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| {
            format!(
                "{}?{}",
                r.path,
                r.query_param("versionId").unwrap_or_default()
            )
        })
        .collect();
    deleted.sort();
    assert_eq!(
        deleted,
        [
            "/bucket/doc.txt?dm",
            "/bucket/doc.txt?v1",
            "/bucket/doc.txt?v2"
        ]
    );

    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--yes",
        "s3://bucket/missing.txt",
    ]));
    out.expect_code(2);
    assert!(
        out.stderr.contains(&tr("has no versions")),
        "{}",
        out.stderr
    );
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn versions_purge_a_versioned_object() {
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
    env.s3.put(&bucket, "dir/v.txt", "one").await;
    env.s3.put(&bucket, "dir/v.txt", "two").await;
    env.run(&["rm", &format!("s3://{bucket}/dir/v.txt")])
        .expect_code(0);
    // The delete marker hides the object, but the bucket is not empty.
    env.run(&["rb", &bucket]).expect_code(2);
    let out = env.run(&[
        "rm",
        "-r",
        "--versions",
        "--yes",
        "--json",
        &format!("s3://{bucket}/dir/"),
    ]);
    out.expect_code(0);
    assert_eq!(out.records().len(), 3);
    env.run(&["rb", &bucket]).expect_code(0);
}
