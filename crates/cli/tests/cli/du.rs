//! `bb du`

use super::*;

#[tokio::test]
async fn totals_of_a_bucket_and_a_prefix() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3.put(&bucket, "a.txt", "12345").await;
    env.s3.put(&bucket, "dir/b.txt", "1234567890").await;
    env.s3.put(&bucket, "dir/sub/c.txt", "abc").await;

    let out = env.run(&["du", &format!("s3://{bucket}")]);
    out.expect_code(0);
    assert_eq!(out.lines().len(), 1, "{}", out.stdout);
    assert!(out.stdout.contains("18 B (18 bytes)"), "{}", out.stdout);

    let out = env.run(&["du", &format!("s3://{bucket}/dir/"), "--json"]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["bucket"], bucket.as_str());
    assert_eq!(records[0]["prefix"], "dir/");
    assert_eq!(records[0]["objects"], 2);
    assert_eq!(records[0]["bytes"], 13);
    for field in [
        "noncurrent_versions",
        "noncurrent_bytes",
        "delete_markers",
        "multipart_uploads",
        "multipart_parts",
        "multipart_bytes",
    ] {
        assert!(records[0][field].is_null(), "{field} was not asked for");
    }
}

#[tokio::test]
async fn an_invalid_uri_is_a_usage_error() {
    let env = Env::start().await;
    let out = env.run(&["du", "not-a-uri"]);
    out.expect_code(1);
}
