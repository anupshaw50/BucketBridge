//! `bb mb` and `bb rb`

use super::*;

async fn bucket_names(env: &Env) -> Vec<String> {
    env.s3
        .ops()
        .list_buckets()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect()
}

#[tokio::test]
async fn mb_creates_and_rb_removes_a_bucket() {
    let env = Env::start().await;
    let name = env.s3.unique_bucket();
    let out = env.run(&["mb", &name]);
    out.expect_code(0);
    assert_eq!(out.stdout.trim(), format!("created bucket {name}"));
    assert!(bucket_names(&env).await.contains(&name));

    let out = env.run(&["rb", &format!("s3://{name}"), "--json"]);
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({ "bucket": name, "region": null })]
    );
    assert!(!bucket_names(&env).await.contains(&name));
}

#[tokio::test]
async fn mb_reports_conflicts_and_invalid_names() {
    let env = Env::start().await;
    let name = env.s3.new_bucket().await;
    let out = env.run(&["mb", &name, "--json"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "conflict");
    assert!(out.stdout.is_empty());

    for bad in ["Bad_Name", "ab", "-start"] {
        env.run(&["mb", bad]).expect_code(1);
    }
    assert_eq!(bucket_names(&env).await, [name]);
}

#[tokio::test]
#[cfg_attr(
    feature = "minio",
    ignore = "MinIO rejects location constraints other than its own region"
)]
async fn mb_in_a_region() {
    let env = Env::start().await;
    let name = env.s3.unique_bucket();
    let out = env.run(&[
        "mb",
        &format!("s3://{name}/"),
        "--region",
        "eu-west-1",
        "--json",
    ]);
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({ "bucket": name, "region": "eu-west-1" })]
    );
    assert!(bucket_names(&env).await.contains(&name));
}

#[tokio::test]
async fn rb_refuses_a_bucket_that_is_not_empty() {
    let env = Env::start().await;
    let name = env.s3.new_bucket().await;
    env.s3.put(&name, "keep.txt", "data").await;
    let out = env.run(&["rb", &name]);
    out.expect_code(2);
    assert!(out.stderr.contains("not empty"), "{}", out.stderr);
    assert_eq!(env.s3.get(&name, "keep.txt").await, b"data");
}

#[tokio::test]
async fn rb_of_a_missing_bucket_is_not_found() {
    let env = Env::start().await;
    let out = env.run(&["rb", "no-such-bucket", "--json"]);
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
}

#[tokio::test]
async fn quiet_hides_success_output_only() {
    let env = Env::start().await;
    let name = env.s3.unique_bucket();
    let out = env.run(&["mb", &name, "--quiet"]);
    out.expect_code(0);
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    let out = env.run(&["rb", &name, "-q", "--json"]);
    out.expect_code(0);
    assert!(out.stdout.is_empty());
    let out = env.run(&["rb", &name, "--quiet"]);
    out.expect_code(2);
    assert!(out.stderr.contains("bb: error:"), "{}", out.stderr);
}
