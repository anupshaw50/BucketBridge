//! `bb ls`

use bucketbridge_tests::MockResponse;

use super::*;

async fn sample_bucket(env: &Env) -> String {
    let bucket = env.s3.new_bucket().await;
    for key in ["b.txt", "a.txt", "dir/x.txt", "dir/sub/y.txt"] {
        env.s3.put(&bucket, key, "12345").await;
    }
    bucket
}

#[tokio::test]
async fn buckets_are_listed_sorted_in_every_format() {
    let env = Env::start().await;
    let mut names = vec![
        env.s3.new_bucket().await,
        env.s3.new_bucket().await,
        env.s3.new_bucket().await,
    ];
    names.sort();

    let out = env.run(&["ls"]);
    out.expect_code(0);
    assert_eq!(out.lines(), names);
    assert!(out.stderr.is_empty(), "{}", out.stderr);

    let out = env.run(&["ls", "--long"]);
    out.expect_code(0);
    for (line, name) in out.lines().iter().zip(&names) {
        assert!(line.ends_with(name.as_str()), "{line}");
        assert!(
            line.starts_with("20") && line.contains('Z'),
            "creation date missing: {line}"
        );
    }

    let out = env.run(&["ls", "--json"]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 3);
    for (record, name) in records.iter().zip(&names) {
        assert_eq!(record["name"], name.as_str());
        assert_eq!(record["kind"], "external");
        for field in ["region", "creation_date", "owner"] {
            assert!(record.get(field).is_some(), "missing {field}");
        }
    }
}

#[tokio::test]
async fn folders_come_before_objects() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;

    let out = env.run(&["ls", &format!("s3://{bucket}")]);
    out.expect_code(0);
    assert_eq!(out.lines(), ["dir/", "a.txt", "b.txt"]);

    let out = env.run(&["ls", &format!("s3://{bucket}/dir/")]);
    assert_eq!(out.lines(), ["dir/sub/", "dir/x.txt"]);

    let out = env.run(&["ls", "-l", &format!("s3://{bucket}/")]);
    out.expect_code(0);
    let long = out.lines();
    assert_eq!(long.len(), 3);
    assert!(
        long[0].contains(" PRE ") && long[0].ends_with("dir/"),
        "{}",
        long[0]
    );
    assert!(
        long[1].contains(" 5 ") && long[1].ends_with("a.txt"),
        "{}",
        long[1]
    );

    let out = env.run(&["ls", "--json", &format!("s3://{bucket}/")]);
    let records = out.records();
    let keys: Vec<&str> = records.iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["dir/", "a.txt", "b.txt"]);
    assert_eq!(records[0]["is_prefix"], true);
    assert_eq!(records[1]["is_prefix"], false);
    assert_eq!(records[1]["size"], 5);
    for field in ["etag", "last_modified", "storage_class", "checksums"] {
        assert!(records[1].get(field).is_some(), "missing {field}");
    }
}

#[tokio::test]
async fn recursive_lists_every_key_under_the_prefix() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;
    let out = env.run(&["ls", "-r", &format!("s3://{bucket}/")]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        ["a.txt", "b.txt", "dir/sub/y.txt", "dir/x.txt"]
    );
    let out = env.run(&[
        "ls",
        "--recursive",
        "--json",
        &format!("s3://{bucket}/dir/"),
    ]);
    let keys: Vec<String> = out
        .records()
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys, ["dir/sub/y.txt", "dir/x.txt"]);
}

#[tokio::test]
async fn versions_show_markers_and_folders_across_pages() {
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return MockResponse::error(400, "Unexpected", "unexpected request");
        }
        match request.query_param("key-marker").as_deref() {
            None => MockResponse::xml(xml::versions(
                &[("a.txt", "va2", false, false), ("a.txt", "dm1", true, true)],
                &["dir/"],
                Some(("a.txt", "va2")),
            )),
            Some("a.txt") => {
                MockResponse::xml(xml::versions(&[("b.txt", "vb1", true, false)], &[], None))
            }
            Some(_) => MockResponse::error(400, "Unexpected", "unexpected marker"),
        }
    });
    let config = tempfile::tempdir().unwrap();

    let out = run(bb_mock(&mock, config.path()).args(["ls", "--versions", "s3://bucket/"]));
    out.expect_code(0);
    let lines = out.lines();
    assert_eq!(lines.len(), 4, "{}", out.stdout);
    assert!(
        lines[0].contains(" PRE ") && lines[0].ends_with("dir/"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(" DEL ") && lines[1].contains("latest") && lines[1].contains("dm1"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains(" 5 ") && lines[2].contains("va2"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("vb1") && lines[3].ends_with("b.txt"),
        "{}",
        lines[3]
    );

    let out =
        run(bb_mock(&mock, config.path()).args(["ls", "--versions", "--json", "s3://bucket/"]));
    out.expect_code(0);
    let records = out.records();
    let summary: Vec<(String, serde_json::Value, bool, bool, bool)> = records
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_owned(),
                r["version_id"].clone(),
                r["is_latest"].as_bool().unwrap(),
                r["is_delete_marker"].as_bool().unwrap(),
                r["is_prefix"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "dir/".to_owned(),
                serde_json::Value::Null,
                false,
                false,
                true
            ),
            (
                "a.txt".to_owned(),
                serde_json::json!("dm1"),
                true,
                true,
                false
            ),
            (
                "a.txt".to_owned(),
                serde_json::json!("va2"),
                false,
                false,
                false
            ),
            (
                "b.txt".to_owned(),
                serde_json::json!("vb1"),
                true,
                false,
                false
            ),
        ]
    );
    for field in ["size", "etag", "last_modified", "storage_class"] {
        assert!(records[2].get(field).is_some(), "missing {field}");
    }

    let requests = mock.requests();
    assert_eq!(requests[0].query_param("delimiter").as_deref(), Some("/"));
    run(bb_mock(&mock, config.path()).args(["ls", "--versions", "-r", "s3://bucket/"]))
        .expect_code(0);
    let last = mock.requests();
    let recursive_first_page = &last[last.len() - 2];
    assert_eq!(recursive_first_page.query_param("delimiter"), None);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn versions_of_a_versioned_bucket() {
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
    env.s3.put(&bucket, "v.txt", "one").await;
    env.s3.put(&bucket, "v.txt", "two").await;
    env.s3
        .ops()
        .delete_object(&bucket, "v.txt", None)
        .await
        .unwrap();

    let out = env.run(&["ls", "--versions", "--json", &format!("s3://{bucket}/")]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["is_delete_marker"], true);
    assert_eq!(records[0]["is_latest"], true);
    assert!(records.iter().all(|r| r["key"] == "v.txt"));
}

#[tokio::test]
async fn a_uri_without_a_trailing_slash_includes_the_exact_object() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    for key in ["report", "report-2024.txt", "other.txt"] {
        env.s3.put(&bucket, key, "1234").await;
    }
    let out = env.run(&["ls", &format!("s3://{bucket}/report")]);
    out.expect_code(0);
    assert_eq!(out.lines(), ["report", "report-2024.txt"]);

    let out = env.run(&["ls", "-r", "--json", &format!("s3://{bucket}/report")]);
    out.expect_code(0);
    let records = out.records();
    let keys: Vec<&str> = records.iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["report", "report-2024.txt"]);
    assert_eq!(records[0]["size"], 4);
    assert_eq!(records[0]["is_prefix"], false);

    let out = env.run(&["ls", "-l", &format!("s3://{bucket}/report")]);
    assert!(
        out.lines()[0].contains(" 4 ") && out.lines()[0].ends_with("report"),
        "{}",
        out.stdout
    );

    // A partial name lists the keys that start with it.
    let out = env.run(&["ls", &format!("s3://{bucket}/repo")]);
    assert_eq!(out.lines(), ["report", "report-2024.txt"]);
}

#[tokio::test]
async fn versions_of_an_exact_key_come_first() {
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return MockResponse::error(400, "Unexpected", "unexpected request");
        }
        MockResponse::xml(xml::versions(
            &[
                ("doc.txt", "v2", false, false),
                ("doc.txt", "dm", true, true),
                ("doc.txt", "v1", false, false),
                ("doc.txt.bak", "vb", true, false),
            ],
            &[],
            None,
        ))
    });
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "ls",
        "--versions",
        "--json",
        "s3://bucket/doc.txt",
    ]));
    out.expect_code(0);
    let order: Vec<(String, String)> = out
        .records()
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_owned(),
                r["version_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = [
        ("doc.txt", "dm"),
        ("doc.txt", "v2"),
        ("doc.txt", "v1"),
        ("doc.txt.bak", "vb"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    assert_eq!(order, expected);
}
