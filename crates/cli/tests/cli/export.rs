//! `bb export`

use bucketbridge_tests::MockResponse;

use super::*;

async fn sample_bucket(env: &Env) -> String {
    let bucket = env.s3.new_bucket().await;
    for key in ["b.txt", "a.txt", "dir/x.txt", "dir/sub/y.txt"] {
        env.s3.put(&bucket, key, "12345").await;
    }
    bucket
}

/// The data rows of a CSV file: the header must be the documented one.
fn csv_rows(path: &Path) -> Vec<Vec<String>> {
    let mut reader = csv::Reader::from_path(path).unwrap();
    assert_eq!(
        reader.headers().unwrap(),
        vec!["key", "size", "last_modified", "etag", "storage_class"]
    );
    reader
        .records()
        .map(|record| record.unwrap().iter().map(str::to_owned).collect())
        .collect()
}

fn keys(rows: &[Vec<String>]) -> Vec<&str> {
    rows.iter().map(|row| row[0].as_str()).collect()
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn one_folder_level_or_everything_below_the_prefix() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("listing.csv");
    let file_arg = file.to_str().unwrap();

    // Without --recursive the rows are what `bb ls` shows: folders first.
    let out = env.run(&[
        "export",
        &format!("s3://{bucket}"),
        "--format",
        "csv",
        "--out",
        file_arg,
    ]);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        format!(
            "exported 3 rows from s3://{bucket}/ to {file_arg} (csv, {} bytes)",
            std::fs::metadata(&file).unwrap().len()
        )
    );
    let rows = csv_rows(&file);
    assert_eq!(keys(&rows), ["dir/", "a.txt", "b.txt"]);
    // A folder has no size or date; an object has both.
    assert_eq!(&rows[0][1..3], ["", ""]);
    assert_eq!(rows[1][1], "5");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&rows[1][2]).is_ok(),
        "{}",
        rows[1][2]
    );

    let out = env.run(&[
        "export",
        &format!("s3://{bucket}/"),
        "--format",
        "csv",
        "--out",
        file_arg,
        "--recursive",
    ]);
    out.expect_code(0);
    assert_eq!(
        keys(&csv_rows(&file)),
        ["a.txt", "b.txt", "dir/sub/y.txt", "dir/x.txt"]
    );
    assert_eq!(leftovers(dir.path()), ["listing.csv"]);
}

#[tokio::test]
async fn a_prefix_limits_the_listing() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dir.csv");
    let file_arg = file.to_str().unwrap();
    env.run(&[
        "export",
        &format!("s3://{bucket}/dir/"),
        "--format",
        "csv",
        "--out",
        file_arg,
    ])
    .expect_code(0);
    assert_eq!(keys(&csv_rows(&file)), ["dir/sub/", "dir/x.txt"]);
    env.run(&[
        "export",
        &format!("s3://{bucket}/dir/"),
        "--format",
        "csv",
        "--out",
        file_arg,
        "-r",
    ])
    .expect_code(0);
    assert_eq!(keys(&csv_rows(&file)), ["dir/sub/y.txt", "dir/x.txt"]);
}

#[tokio::test]
async fn json_and_quiet_output_and_the_other_formats() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;
    let dir = tempfile::tempdir().unwrap();
    for (format, magic) in [
        ("xlsx", &b"PK"[..]),
        ("parquet", &b"PAR1"[..]),
        ("csv", &b"key,"[..]),
    ] {
        let file = dir.path().join(format!("listing.{format}"));
        let out = env.run(&[
            "export",
            &format!("s3://{bucket}"),
            "--format",
            format,
            "--out",
            file.to_str().unwrap(),
            "--recursive",
            "--json",
        ]);
        out.expect_code(0);
        let records = out.records();
        assert_eq!(records.len(), 1);
        let bytes = std::fs::read(&file).unwrap();
        assert!(bytes.starts_with(magic), "{format}: {:?}", &bytes[..8]);
        assert_eq!(
            records[0],
            serde_json::json!({
                "source": format!("s3://{bucket}/"),
                "format": format,
                "file": file.to_str().unwrap(),
                "recursive": true,
                "rows": 4,
                "bytes": bytes.len(),
            })
        );
    }
    let file = dir.path().join("quiet.csv");
    let out = env.run(&[
        "export",
        &format!("s3://{bucket}"),
        "--format",
        "csv",
        "--out",
        file.to_str().unwrap(),
        "--quiet",
    ]);
    out.expect_code(0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(file.exists());
}

#[tokio::test]
async fn a_relative_file_is_made_absolute_in_the_record() {
    let env = Env::start().await;
    let bucket = sample_bucket(&env).await;
    let dir = tempfile::tempdir().unwrap();
    let out = env.run_in(
        dir.path(),
        &[
            "export",
            &format!("s3://{bucket}"),
            "--format",
            "csv",
            "--out",
            "listing.csv",
            "--json",
        ],
    );
    out.expect_code(0);
    let file = out.records()[0]["file"].as_str().unwrap().to_owned();
    assert!(Path::new(&file).is_absolute(), "{file}");
    assert!(file.ends_with("listing.csv"), "{file}");
    assert!(dir.path().join("listing.csv").exists());
}

#[tokio::test]
async fn failures_leave_the_old_file_and_no_temporary_file() {
    let env = Env::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("keep.csv");
    std::fs::write(&file, "old contents").unwrap();

    // A bucket that does not exist: remote error (exit 2).
    let out = env.run(&[
        "export",
        "s3://no-such-bucket-here",
        "--format",
        "csv",
        "--out",
        file.to_str().unwrap(),
        "--json",
    ]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old contents");

    // A folder that does not exist for the file: local error (exit 2).
    let bucket = sample_bucket(&env).await;
    let missing = dir.path().join("missing").join("x.csv");
    let out = env.run(&[
        "export",
        &format!("s3://{bucket}"),
        "--format",
        "csv",
        "--out",
        missing.to_str().unwrap(),
        "--json",
    ]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "io");
    assert_eq!(leftovers(dir.path()), ["keep.csv"]);
}

#[tokio::test]
async fn a_listing_that_fails_half_way_changes_nothing() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").is_some() {
            return match request.query_param("continuation-token").as_deref() {
                None => MockResponse::xml(xml::objects_page(&["a", "b"], Some("t1"))),
                Some(_) => MockResponse::error(403, "AccessDenied", "Access Denied"),
            };
        }
        MockResponse::error(400, "Unexpected", "unexpected request")
    });
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.csv");
    std::fs::write(&file, "old contents").unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "export",
        "s3://bucket",
        "--format",
        "csv",
        "--out",
        file.to_str().unwrap(),
        "--recursive",
    ]));
    out.expect_code(3);
    assert!(out.stderr.contains("Access Denied"), "{}", out.stderr);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old contents");
    assert_eq!(leftovers(dir.path()), ["out.csv"]);
}

#[tokio::test]
async fn a_server_that_repeats_a_continuation_token_ends_the_export() {
    let mock = MockS3::start(|request| {
        if request.method == "GET" && request.query_param("list-type").is_some() {
            return MockResponse::xml(xml::objects_page(&["k"], Some("again")));
        }
        MockResponse::error(400, "Unexpected", "unexpected request")
    });
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    for recursive in [false, true] {
        let file = dir.path().join("loop.csv");
        let mut args = vec![
            "export",
            "s3://bucket",
            "--format",
            "csv",
            "--out",
            file.to_str().unwrap(),
        ];
        if recursive {
            args.push("--recursive");
        }
        let out = run(bb_mock(&mock, config.path()).args(&args));
        out.expect_code(2);
        assert!(
            out.stderr.contains("same continuation token"),
            "{}",
            out.stderr
        );
        assert!(leftovers(dir.path()).is_empty());
    }
}

#[tokio::test]
async fn usage_errors_exit_with_1() {
    let env = Env::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x");
    let file = file.to_str().unwrap();
    for args in [
        vec!["export", "s3://b", "--out", file],
        vec!["export", "s3://b", "--format", "csv"],
        vec!["export", "s3://b", "--format", "json", "--out", file],
        vec!["export", "not-a-uri", "--format", "csv", "--out", file],
    ] {
        env.run(&args).expect_code(1);
    }
    assert!(leftovers(dir.path()).is_empty());
}
