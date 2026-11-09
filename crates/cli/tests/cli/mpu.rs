//! `bb mpu list` and `bb mpu abort`

use bucketbridge_tests::MockResponse;

use bucketbridge_cli::i18n::trf;

use super::*;

#[tokio::test]
async fn abort_one_upload() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let created = env
        .s3
        .client()
        .create_multipart_upload()
        .bucket(&bucket)
        .key("big.bin")
        .send()
        .await
        .unwrap();
    let upload_id = created.upload_id().unwrap().to_owned();
    let target = [
        "mpu",
        "abort",
        "--bucket",
        &bucket,
        "--key",
        "big.bin",
        "--upload-id",
        &upload_id,
    ];

    let mut dry_run = target.to_vec();
    dry_run.push("--dry-run");
    let out = env.run(&dry_run);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "would abort upload {id} of s3://{bucket}/{key}",
            &[
                ("id", upload_id.as_str()),
                ("bucket", bucket.as_str()),
                ("key", "big.bin"),
            ],
        )
    );
    assert_eq!(env.s3.pending_uploads(&bucket, "big.bin").await, 1);

    let mut json = target.to_vec();
    json.push("--json");
    let out = env.run(&json);
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "bucket": bucket,
            "key": "big.bin",
            "upload_id": upload_id,
            "dry_run": false,
            "aborted": true,
            "error": null
        })]
    );
    assert_eq!(env.s3.pending_uploads(&bucket, "big.bin").await, 0);
}

fn uploads_mock() -> MockS3 {
    MockS3::start(|request| match request.method.as_str() {
        "GET" if request.path == "/" => {
            MockResponse::xml(xml::buckets(&[("b1", "us-east-1"), ("b2", "eu-west-1")]))
        }
        "GET" if request.query_param("uploads").is_some() => {
            if request.path.starts_with("/b2") {
                MockResponse::xml(xml::uploads("b2", &[("logs/z.gz", "u3")]))
            } else {
                MockResponse::xml(xml::uploads(
                    "b1",
                    &[("big.bin", "u1"), ("tmp/part.bin", "u2")],
                ))
            }
        }
        "DELETE" if request.query_param("uploadId").as_deref() == Some("u2") => {
            MockResponse::error(404, "NoSuchUpload", "The specified upload does not exist")
        }
        "DELETE" if request.query_param("uploadId").is_some() => MockResponse::empty(204),
        _ => MockResponse::error(400, "Unexpected", "unexpected request"),
    })
}

#[tokio::test]
async fn list_uploads_of_one_bucket_or_of_every_bucket() {
    let mock = uploads_mock();
    let config = tempfile::tempdir().unwrap();

    let out =
        run(bb_mock(&mock, config.path())
            .args(["mpu", "list", "--bucket", "b1", "--prefix", "big"]));
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            "2026-10-29T08:15:00Z  s3://b1/big.bin  u1",
            "2026-10-29T08:15:00Z  s3://b1/tmp/part.bin  u2"
        ]
    );
    assert_eq!(
        mock.requests()[0].query_param("prefix").as_deref(),
        Some("big")
    );

    let before = mock.requests().len();
    let out = run(bb_mock(&mock, config.path()).args(["mpu", "list", "--json"]));
    out.expect_code(0);
    let records = out.records();
    let ids: Vec<(&str, &str, &str)> = records
        .iter()
        .map(|r| {
            (
                r["bucket"].as_str().unwrap(),
                r["key"].as_str().unwrap(),
                r["upload_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ids,
        [
            ("b1", "big.bin", "u1"),
            ("b1", "tmp/part.bin", "u2"),
            ("b2", "logs/z.gz", "u3")
        ]
    );
    for field in ["initiated", "storage_class", "initiator"] {
        assert!(records[0].get(field).is_some(), "missing {field}");
    }
    // A custom endpoint serves every bucket in the account region, whatever
    // region ListBuckets reports (bucket regions are resolved for AWS only).
    let requests = &mock.requests()[before..];
    let region_of = |path: &str| {
        let request = requests
            .iter()
            .find(|r| r.path.starts_with(path) && r.query_param("uploads").is_some())
            .unwrap();
        request
            .header("authorization")
            .unwrap_or_default()
            .to_owned()
    };
    assert!(region_of("/b1").contains("/us-east-1/s3/aws4_request"));
    assert!(region_of("/b2").contains("/us-east-1/s3/aws4_request"));
}

#[tokio::test]
async fn abort_all_reports_each_upload() {
    let mock = uploads_mock();
    let config = tempfile::tempdir().unwrap();

    let out = run(bb_mock(&mock, config.path()).args([
        "mpu",
        "abort",
        "--bucket",
        "b1",
        "--all",
        "--dry-run",
    ]));
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            trf(
                "would abort upload {id} of s3://{bucket}/{key}",
                &[("id", "u1"), ("bucket", "b1"), ("key", "big.bin")],
            ),
            trf(
                "would abort upload {id} of s3://{bucket}/{key}",
                &[("id", "u2"), ("bucket", "b1"), ("key", "tmp/part.bin")],
            ),
        ]
    );
    assert!(
        mock.requests().iter().all(|r| r.method != "DELETE"),
        "a dry run sent a DELETE"
    );

    let out =
        run(bb_mock(&mock, config.path())
            .args(["mpu", "abort", "--bucket", "b1", "--all", "--json"]));
    out.expect_code(2);
    let records = out.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["aborted"], true);
    assert_eq!(records[1]["aborted"], false);
    assert_eq!(records[1]["error"]["code"], "not_found");
    assert_eq!(
        out.error_record()["message"],
        super::batch_summary(1, 2, "uploads", "could not be aborted")
    );
    let aborted: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.query_param("uploadId").unwrap_or_default())
        .collect();
    assert_eq!(aborted, ["u1", "u2"]);

    let out = run(bb_mock(&mock, config.path()).args([
        "mpu", "abort", "--bucket", "b1", "--all", "--prefix", "tmp/", "--quiet",
    ]));
    out.expect_code(2);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("u2"), "{}", out.stderr);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement ListMultipartUploads"
)]
async fn list_and_abort_on_minio() {
    let env = Env::start_minio().await;
    let bucket = env.s3.new_bucket().await;
    for _ in 0..2 {
        env.s3
            .client()
            .create_multipart_upload()
            .bucket(&bucket)
            .key("big.bin")
            .send()
            .await
            .unwrap();
    }
    // MinIO lists uploads per object key, so the prefix is the exact key.
    let out = env.run(&[
        "mpu", "list", "--bucket", &bucket, "--prefix", "big.bin", "--json",
    ]);
    out.expect_code(0);
    assert_eq!(out.records().len(), 2);
    env.run(&[
        "mpu", "abort", "--bucket", &bucket, "--all", "--prefix", "big.bin",
    ])
    .expect_code(0);
    assert_eq!(env.s3.pending_uploads(&bucket, "big.bin").await, 0);
}
