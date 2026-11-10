//! Wildcards in the last segment of an S3 URI or a local path for `ls`,
//! `rm`, `cp` and `mv`. The patterns reach `bb` quoted, exactly as a shell
//! passes them when the pattern is in quotes.

use bucketbridge_tests::MockResponse;

use super::*;

async fn logs(env: &Env) -> String {
    let bucket = env.s3.new_bucket().await;
    for key in [
        "logs/a.gz",
        "logs/b.gz",
        "logs/c.txt",
        "logs/sub/d.gz",
        "logs/x[1].txt",
        "top.gz",
    ] {
        env.s3.put(&bucket, key, key.as_bytes().to_vec()).await;
    }
    bucket
}

fn keys_of(out: &Output) -> Vec<String> {
    out.records()
        .iter()
        .map(|r| r["key"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn ls_lists_the_names_that_match() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/*.gz")]);
    out.expect_code(0);
    // Not logs/sub/d.gz: `*` stops at a folder.
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/b.gz"]);

    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/?.gz")]);
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/b.gz"]);
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/[ab].gz")]);
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/b.gz"]);
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/{{a,c}}.*")]);
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/c.txt"]);
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/*.gz")]);
    assert_eq!(keys_of(&out), ["top.gz"]);

    // Folders match by name too.
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/s*")]);
    out.expect_code(0);
    assert_eq!(keys_of(&out), ["logs/sub/"]);
    assert_eq!(out.records()[0]["is_prefix"], true);

    // A backslash makes a wildcard character literal.
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/x\\[1].txt")]);
    out.expect_code(0);
    assert_eq!(keys_of(&out), ["logs/x[1].txt"]);
}

#[tokio::test]
async fn a_pattern_without_a_match_is_not_found() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    let out = env.run(&["--json", "ls", &format!("s3://{bucket}/logs/*.zip")]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
    assert!(out.stdout.is_empty());
    let out = env.run(&["rm", &format!("s3://{bucket}/logs/*.zip")]);
    out.expect_code(2);
    assert!(out.stderr.contains("*.zip"), "{}", out.stderr);
    let dst = env.s3.new_bucket().await;
    let out = env.run(&[
        "cp",
        &format!("s3://{bucket}/logs/*.zip"),
        &format!("s3://{dst}/"),
    ]);
    out.expect_code(2);
    assert!(env.keys(&dst).await.is_empty());
    // Nothing was deleted by the failed rm.
    assert_eq!(env.keys(&bucket).await.len(), 6);
}

#[tokio::test]
async fn wildcards_only_work_in_the_last_segment_and_not_with_recursive() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    for args in [
        vec!["ls", "s3://B/*/a.gz"],
        vec!["rm", "s3://B/*/a.gz"],
        vec!["ls", "-r", "s3://B/logs/*.gz"],
        vec!["rm", "-r", "s3://B/logs/*.gz"],
        vec!["rm", "--version-id", "v1", "s3://B/logs/*.gz"],
        vec!["cp", "-r", "s3://B/logs/*.gz", "s3://B/out/"],
        vec!["cp", "s3://B/*/a.gz", "s3://B/out/"],
    ] {
        let args: Vec<String> = args.iter().map(|a| a.replace('B', &bucket)).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = env.run(&args);
        out.expect_code(1);
    }
    assert_eq!(env.keys(&bucket).await.len(), 6);
}

#[tokio::test]
async fn rm_deletes_the_matching_objects_only() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    let uri = format!("s3://{bucket}/logs/*.gz");
    let out = env.run(&["rm", "--dry-run", "--json", &uri]);
    out.expect_code(0);
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/b.gz"]);
    assert!(out.records().iter().all(|r| r["dry_run"] == true));
    assert_eq!(env.keys(&bucket).await.len(), 6);

    let out = env.run(&["rm", "--json", &uri]);
    out.expect_code(0);
    assert_eq!(keys_of(&out), ["logs/a.gz", "logs/b.gz"]);
    assert_eq!(
        env.keys(&bucket).await,
        ["logs/c.txt", "logs/sub/d.gz", "logs/x[1].txt", "top.gz"]
    );
}

#[tokio::test]
async fn cp_downloads_matching_objects_into_a_folder() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    let target = tempfile::tempdir().unwrap();
    let dir = target.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let out = env.run(&[
        "cp",
        &format!("s3://{bucket}/logs/*.gz"),
        dir.to_str().unwrap(),
    ]);
    out.expect_code(0);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["a.gz", "b.gz"]);
    assert_eq!(std::fs::read(dir.join("a.gz")).unwrap(), b"logs/a.gz");

    // Two matches need a folder as the destination.
    let file = target.path().join("one.gz");
    let out = env.run(&[
        "cp",
        &format!("s3://{bucket}/logs/*.gz"),
        file.to_str().unwrap(),
    ]);
    out.expect_code(1);
    assert!(!file.exists());
    // One match may go to an exact file name.
    let out = env.run(&[
        "cp",
        &format!("s3://{bucket}/logs/c*"),
        file.to_str().unwrap(),
    ]);
    out.expect_code(0);
    assert_eq!(std::fs::read(&file).unwrap(), b"logs/c.txt");
    // A folder that ends with a separator is created like for one object.
    let fresh = format!("{}/", target.path().join("fresh").display());
    env.run(&["cp", &format!("s3://{bucket}/logs/*.gz"), &fresh])
        .expect_code(0);
    assert!(target.path().join("fresh/b.gz").exists());
}

#[tokio::test]
async fn cp_and_mv_copy_matching_objects_between_prefixes() {
    let env = Env::start().await;
    let bucket = logs(&env).await;
    let dst = env.s3.new_bucket().await;
    let src = format!("s3://{bucket}/logs/*.gz");
    // Two matches need a prefix.
    env.run(&["cp", &src, &format!("s3://{dst}/one.gz")])
        .expect_code(1);
    assert!(env.keys(&dst).await.is_empty());
    let out = env.run(&["cp", "--json", &src, &format!("s3://{dst}/archive/")]);
    out.expect_code(0);
    assert_eq!(env.keys(&dst).await, ["archive/a.gz", "archive/b.gz"]);
    assert_eq!(env.keys(&bucket).await.len(), 6);

    let out = env.run(&["mv", &src, &format!("s3://{dst}/moved/")]);
    out.expect_code(0);
    assert_eq!(
        env.keys(&dst).await,
        ["archive/a.gz", "archive/b.gz", "moved/a.gz", "moved/b.gz"]
    );
    assert_eq!(
        env.keys(&bucket).await,
        ["logs/c.txt", "logs/sub/d.gz", "logs/x[1].txt", "top.gz"]
    );

    // A dry run lists the matches and copies nothing.
    let out = env.run(&[
        "cp",
        "--dry-run",
        "--json",
        &format!("s3://{bucket}/logs/*.txt"),
        &format!("s3://{dst}/dry/"),
    ]);
    out.expect_code(0);
    assert_eq!(out.records().len(), 2);
    assert_eq!(env.keys(&dst).await.len(), 4);
}

#[tokio::test]
async fn cp_uploads_the_files_a_local_pattern_matches() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.jpg", "b.jpg", "c.png", "a[1].txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    std::fs::create_dir(dir.path().join("d.jpg")).unwrap();
    let pattern = dir.path().join("*.jpg");
    let out = env.run(&[
        "cp",
        pattern.to_str().unwrap(),
        &format!("s3://{bucket}/photos/"),
    ]);
    out.expect_code(0);
    // The folder `d.jpg` is not a file and is left out.
    assert_eq!(env.keys(&bucket).await, ["photos/a.jpg", "photos/b.jpg"]);

    // Two files need a prefix.
    env.run(&[
        "cp",
        pattern.to_str().unwrap(),
        &format!("s3://{bucket}/one.jpg"),
    ])
    .expect_code(1);
    // No file: not_found, nothing uploaded.
    let none = dir.path().join("*.zip");
    let out = env.run(&["cp", none.to_str().unwrap(), &format!("s3://{bucket}/")]);
    out.expect_code(2);
    assert_eq!(env.keys(&bucket).await.len(), 2);
    // A file whose name looks like a pattern is that file.
    let literal = dir.path().join("a[1].txt");
    env.run(&[
        "cp",
        literal.to_str().unwrap(),
        &format!("s3://{bucket}/literal/"),
    ])
    .expect_code(0);
    assert!(
        env.keys(&bucket)
            .await
            .contains(&"literal/a[1].txt".to_owned())
    );
    // `-r` takes a folder, not a pattern.
    env.run(&[
        "cp",
        "-r",
        pattern.to_str().unwrap(),
        &format!("s3://{bucket}/r/"),
    ])
    .expect_code(1);
}

#[tokio::test]
async fn mv_uploads_and_removes_the_matching_files() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.log", "b.log", "keep.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    let pattern = dir.path().join("*.log");
    env.run(&[
        "mv",
        pattern.to_str().unwrap(),
        &format!("s3://{bucket}/logs/"),
    ])
    .expect_code(0);
    assert_eq!(env.keys(&bucket).await, ["logs/a.log", "logs/b.log"]);
    assert!(!dir.path().join("a.log").exists());
    assert!(dir.path().join("keep.txt").exists());
}

fn version_mock() -> MockS3 {
    MockS3::start(|request| match request.method.as_str() {
        "GET" if request.query_param("versions").is_some() => {
            MockResponse::xml(xml::dated_versions(&[
                ("logs/a.gz", "v1", true, false, "2020-01-01T00:00:00.000Z"),
                ("logs/a.gz", "v0", false, false, "2020-01-01T00:00:00.000Z"),
                ("logs/b.txt", "w1", true, false, "2020-01-01T00:00:00.000Z"),
                (
                    "logs/sub/c.gz",
                    "u1",
                    true,
                    false,
                    "2020-01-01T00:00:00.000Z",
                ),
            ]))
        }
        "DELETE" => MockResponse::empty(204),
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    })
}

fn deleted(mock: &MockS3) -> Vec<String> {
    let mut ids: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.query_param("versionId").unwrap_or_default())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn versions_of_the_matching_keys() {
    let mock = version_mock();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "--json",
        "ls",
        "--versions",
        "s3://bucket/logs/*.gz",
    ]));
    out.expect_code(0);
    let ids: Vec<String> = out
        .records()
        .iter()
        .map(|r| r["version_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["v1", "v0"]);

    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--noncurrent-only",
        "--yes",
        "s3://bucket/logs/*.gz",
    ]));
    out.expect_code(0);
    assert_eq!(deleted(&mock), ["v0"]);

    // The wildcard matches no key at all: not_found, whatever the filters.
    let none = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--yes",
        "s3://bucket/logs/*.zip",
    ]));
    none.expect_code(2);
    assert!(none.stderr.contains("*.zip"), "{}", none.stderr);
    // A wildcard that matches keys, with a filter that keeps nothing, is
    // not an error.
    let before = deleted(&mock).len();
    let quiet = run(bb_mock(&mock, config.path()).args([
        "rm",
        "--versions",
        "--yes",
        "--older-than",
        "2019-01-01",
        "s3://bucket/logs/*.gz",
    ]));
    quiet.expect_code(0);
    assert_eq!(deleted(&mock).len(), before);
}
