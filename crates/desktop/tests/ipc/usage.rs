//! IPC tests of the bucket usage commands.

use serde_json::json;

use crate::harness::{Harness, block_on, keys, next, start_s3};

#[test]
fn usage_is_computed_with_progress_and_validated() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a.txt", "12345"));
    block_on(server.put(&bucket, "dir/b.txt", "1234567890"));

    let progress = h.events("usage://progress");
    let usage = h.ok(
        "compute_bucket_usage",
        json!({
            "account_id": id, "bucket": bucket, "prefix": "",
            "versions": false, "multipart": false, "job_id": "job-1"
        }),
    );
    assert_eq!(
        keys(&usage),
        ["bytes", "multipart", "noncurrent", "objects"]
    );
    assert_eq!(usage["objects"], 2);
    assert_eq!(usage["bytes"], 15);
    assert!(usage["noncurrent"].is_null() && usage["multipart"].is_null());
    let event = next(&progress);
    assert_eq!(event["job_id"], "job-1");
    assert_eq!(event["phase"], "objects");
    assert_eq!(event["usage"]["objects"], 2);

    let under = h.ok(
        "compute_bucket_usage",
        json!({
            "account_id": id, "bucket": bucket, "prefix": "dir/",
            "versions": false, "multipart": false, "job_id": "job-1"
        }),
    );
    assert_eq!(under["objects"], 1);

    // Cancelling an unknown job is fine; bad input is a usage error.
    h.ok("cancel_bucket_usage", json!({ "job_id": "nope" }));
    assert_eq!(
        h.code(
            "compute_bucket_usage",
            json!({
                "account_id": id, "bucket": "bad name", "prefix": "",
                "versions": false, "multipart": false, "job_id": "job-2"
            })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "compute_bucket_usage",
            json!({
                "account_id": id, "bucket": bucket, "prefix": "",
                "versions": false, "multipart": false, "job_id": ""
            })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "compute_bucket_usage",
            json!({
                "account_id": "missing", "bucket": bucket, "prefix": "",
                "versions": false, "multipart": false, "job_id": "job-3"
            })
        ),
        "not_found"
    );
}
