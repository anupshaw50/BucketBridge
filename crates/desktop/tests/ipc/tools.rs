//! Tools and export commands: saved filters, diagnostics, uncompleted
//! multipart uploads and the export of a listing.

use std::fs;

use bucketbridge_tests::{MockResponse, MockS3};
use serde_json::{Value, json};

use crate::harness::*;

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn filter(name: &str, glob: &str) -> Value {
    json!({
        "name": name,
        "filter": {
            "prefix": null, "glob": glob, "min_size": null, "max_size": null,
            "modified_after": null, "modified_before": null, "storage_classes": []
        }
    })
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect()
}

#[test]
fn saved_filters_are_stored_in_the_configuration_file() {
    let h = Harness::new();
    assert_eq!(h.ok("list_filters", json!({})), json!([]));

    let list = h.ok(
        "save_filter",
        json!({ "filter": filter("Logs", "*.log"), "replaces": null }),
    );
    assert_eq!(keys(&list[0]), ["filter", "name"]);
    assert_eq!(
        keys(&list[0]["filter"]),
        [
            "glob",
            "max_size",
            "min_size",
            "modified_after",
            "modified_before",
            "prefix",
            "storage_classes"
        ]
    );
    h.ok(
        "save_filter",
        json!({ "filter": filter("Images", "*.{jpg,png}"), "replaces": null }),
    );

    // Names are unique ignoring case; a rename keeps the place.
    assert_eq!(
        h.code(
            "save_filter",
            json!({ "filter": filter("logs", "*"), "replaces": null })
        ),
        "conflict"
    );
    let list = h.ok(
        "save_filter",
        json!({ "filter": filter("Pictures", "*.jpg"), "replaces": "images" }),
    );
    assert_eq!(names(&list), ["Logs", "Pictures"]);
    assert_eq!(
        h.code(
            "save_filter",
            json!({ "filter": filter("x", "*"), "replaces": "nope" })
        ),
        "not_found"
    );
    // The engine's verdict on a spec is a usage error, and nothing is stored.
    let (code, message) = h.error(
        "save_filter",
        json!({ "filter": filter("Broken", "a[b"), "replaces": null }),
    );
    assert_eq!(code, "usage", "{message}");
    assert_eq!(
        h.code(
            "save_filter",
            json!({ "filter": filter("   ", "*"), "replaces": null })
        ),
        "usage"
    );
    assert_eq!(
        names(&h.ok("list_filters", json!({}))),
        ["Logs", "Pictures"]
    );

    // The file holds them; a restarted app reads them back.
    let text = fs::read_to_string(h.core.dirs().config_dir.join("config.toml")).unwrap();
    assert!(text.contains("name = \"Pictures\""), "{text}");
    let h = h.restart();
    assert_eq!(
        names(&h.ok("list_filters", json!({}))),
        ["Logs", "Pictures"]
    );
    let list = h.ok("delete_filter", json!({ "name": "LOGS" }));
    assert_eq!(names(&list), ["Pictures"]);
    assert_eq!(
        h.code("delete_filter", json!({ "name": "Logs" })),
        "not_found"
    );
}

#[test]
fn diagnostics_probe_a_stored_account_and_an_unsaved_one() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());

    let report = h.ok(
        "run_diagnostics",
        json!({ "account_id": id, "input": null, "bucket": bucket }),
    );
    assert_eq!(
        keys(&report),
        [
            "bucket",
            "elapsed_ms",
            "ok",
            "region",
            "started_at",
            "steps",
            "target"
        ]
    );
    assert_eq!(report["target"], server.endpoint());
    assert_eq!(report["bucket"], bucket.as_str());
    assert_eq!(report["ok"], true, "{report}");
    let steps = report["steps"].as_array().unwrap();
    let outline: Vec<(&str, &str)> = steps
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        outline,
        [
            ("dns", "skip"),
            ("tcp", "pass"),
            ("proxy", "skip"),
            ("tls", "skip"),
            ("credentials", "pass"),
            ("region", "pass")
        ]
    );
    assert_eq!(
        keys(&steps[0]),
        ["code", "detail", "elapsed_ms", "id", "status", "summary"]
    );
    assert_eq!(steps[4]["code"], "valid");

    // An unsaved account: wrong secret, and an unknown access key.
    let mut input = account_input(&server, "Draft");
    input["secret_access_key"] = json!("a-wrong-secret-0123456789");
    let report = h.ok(
        "run_diagnostics",
        json!({ "account_id": null, "input": input, "bucket": null }),
    );
    assert_eq!(report["ok"], false);
    assert_eq!(report["steps"][4]["status"], "fail");
    assert_eq!(report["steps"][4]["code"], "bad_signature");
    assert_eq!(report["steps"][5]["code"], "no_bucket");
    assert!(
        !report.to_string().contains("a-wrong-secret-0123456789"),
        "{report}"
    );
    input["access_key_id"] = json!("AKIAUNKNOWNKEY000000");
    let report = h.ok(
        "run_diagnostics",
        json!({ "account_id": null, "input": input, "bucket": null }),
    );
    // The in-process server answers an unknown key with NotSignedUp, which
    // is reported with the server's own words (S3 itself and MinIO say
    // InvalidAccessKeyId, covered by the scripted tests).
    assert_eq!(report["steps"][4]["status"], "fail");
    assert_eq!(report["steps"][4]["code"], "failed");
    assert!(
        report["steps"][4]["summary"]
            .as_str()
            .unwrap()
            .contains("not signed up"),
        "{report}"
    );
    // Nothing was stored.
    assert_eq!(
        h.ok("list_accounts", json!({}))["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Which account is probed is not ambiguous.
    let draft = account_input(&server, "Draft");
    for args in [
        json!({ "account_id": null, "input": null, "bucket": null }),
        json!({ "account_id": id, "input": draft, "bucket": null }),
        json!({ "account_id": id, "input": null, "bucket": "bad bucket" }),
    ] {
        assert_eq!(h.code("run_diagnostics", args), "usage");
    }
    let mut empty_secret = account_input(&server, "Draft");
    empty_secret["secret_access_key"] = json!("");
    assert_eq!(
        h.code(
            "run_diagnostics",
            json!({ "account_id": null, "input": empty_secret, "bucket": null })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "run_diagnostics",
            json!({ "account_id": "missing", "input": null, "bucket": null })
        ),
        "not_found"
    );

    // Locked secrets reject like every account-bound command.
    h.ok(
        "set_master_password",
        json!({ "current": null, "new_password": "correct horse" }),
    );
    h.ok("lock", json!({}));
    assert_eq!(
        h.code(
            "run_diagnostics",
            json!({ "account_id": id, "input": null, "bucket": null })
        ),
        "locked"
    );
}

fn mock_account(h: &Harness, mock: &MockS3) -> String {
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "Mock", "access_key_id": "AKIDMOCK", "secret_access_key": "mock-secret",
            "session_token": null, "region": "us-east-1", "endpoint": mock.endpoint(),
            "path_style": true, "request_payer": false, "mfa_serial": null
        } }),
    );
    account["id"].as_str().unwrap().to_owned()
}

fn uploads_xml(bucket: &str, uploads: &[(&str, &str, &str)]) -> String {
    let body: String = uploads
        .iter()
        .map(|(key, id, initiated)| {
            format!("<Upload><Key>{key}</Key><UploadId>{id}</UploadId><StorageClass>STANDARD</StorageClass><Initiated>{initiated}</Initiated></Upload>")
        })
        .collect();
    format!(
        "<ListMultipartUploadsResult {NS}><Bucket>{bucket}</Bucket><IsTruncated>false</IsTruncated>{body}</ListMultipartUploadsResult>"
    )
}

#[test]
fn uploads_are_listed_per_bucket_and_aborted_one_by_one() {
    let mock = MockS3::start(|request| {
        let bucket = request
            .path
            .trim_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        match (
            request.method.as_str(),
            request.query_param("uploads").is_some(),
        ) {
            ("GET", true) => match bucket {
                "alpha" => MockResponse::xml(uploads_xml(
                    "alpha",
                    &[
                        ("z.bin", "u2", "2026-10-01T08:00:00.000Z"),
                        ("a.bin", "u1", "2026-09-01T08:00:00.000Z"),
                    ],
                )),
                "gamma" => MockResponse::xml(uploads_xml(
                    "gamma",
                    &[("g.bin", "u9", "2026-10-02T08:00:00.000Z")],
                )),
                _ => MockResponse::error(403, "AccessDenied", "Access Denied"),
            },
            ("DELETE", _) => match request.query_param("uploadId").as_deref() {
                Some("u2") => {
                    MockResponse::error(404, "NoSuchUpload", "The specified upload does not exist")
                }
                _ => MockResponse::empty(204),
            },
            _ => MockResponse::error(400, "Unexpected", "unexpected request"),
        }
    });
    let h = Harness::new();
    let id = mock_account(&h, &mock);

    // Sorted by bucket and key; a bucket that fails is reported, the rest listed;
    // a bucket named twice is listed once.
    let listing = h.ok(
        "list_multipart_uploads",
        json!({ "account_id": id, "buckets": ["gamma", "beta", "alpha", "gamma"] }),
    );
    assert_eq!(keys(&listing), ["failures", "uploads"]);
    let rows: Vec<(&str, &str, &str)> = listing["uploads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["bucket"].as_str().unwrap(),
                u["key"].as_str().unwrap(),
                u["upload_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("alpha", "a.bin", "u1"),
            ("alpha", "z.bin", "u2"),
            ("gamma", "g.bin", "u9")
        ]
    );
    assert_eq!(
        keys(&listing["uploads"][0]),
        [
            "bucket",
            "initiated",
            "initiator",
            "key",
            "storage_class",
            "upload_id"
        ]
    );
    assert_eq!(listing["uploads"][0]["initiated"], "2026-09-01T08:00:00Z");
    assert_eq!(listing["failures"].as_array().unwrap().len(), 1);
    assert_eq!(listing["failures"][0]["bucket"], "beta");
    assert_eq!(listing["failures"][0]["error"]["code"], "auth");
    let listed: usize = mock
        .requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path.trim_matches('/') == "gamma")
        .count();
    assert_eq!(listed, 1, "a bucket named twice is listed once");

    for args in [
        json!({ "account_id": id, "buckets": [] }),
        json!({ "account_id": id, "buckets": ["bad bucket"] }),
    ] {
        assert_eq!(h.code("list_multipart_uploads", args), "usage");
    }

    // Aborting reports every upload and never stops at a failure.
    let reference = |bucket: &str, key: &str, upload_id: &str| json!({ "bucket": bucket, "key": key, "upload_id": upload_id });
    let results = h.ok(
        "abort_multipart_uploads",
        json!({ "account_id": id, "uploads": [
            reference("alpha", "a.bin", "u1"),
            reference("alpha", "z.bin", "u2"),
            reference("gamma", "g.bin", "u9"),
        ] }),
    );
    let outcome: Vec<(&str, bool, Option<&str>)> = results
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["upload_id"].as_str().unwrap(),
                r["aborted"].as_bool().unwrap(),
                r["error"]["code"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        outcome,
        [
            ("u1", true, None),
            ("u2", false, Some("not_found")),
            ("u9", true, None)
        ]
    );
    assert_eq!(
        keys(&results[0]),
        ["aborted", "bucket", "error", "key", "upload_id"]
    );
    let deleted: Vec<String> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.query_param("uploadId").unwrap())
        .collect();
    assert_eq!(deleted, ["u1", "u2", "u9"]);

    // Invalid requests abort nothing.
    for uploads in [
        json!([]),
        json!([
            reference("alpha", "a.bin", "u1"),
            reference("bad bucket", "k", "u")
        ]),
        json!([reference("alpha", "a.bin", "")]),
        json!([reference("alpha", "", "u1")]),
    ] {
        assert_eq!(
            h.code(
                "abort_multipart_uploads",
                json!({ "account_id": id, "uploads": uploads })
            ),
            "usage"
        );
    }
    let deletes = mock
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .count();
    assert_eq!(deletes, 3);
}

#[test]
fn an_upload_is_aborted_on_a_real_server() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let created = block_on(
        server
            .client()
            .create_multipart_upload()
            .bucket(&bucket)
            .key("big.bin")
            .send(),
    )
    .unwrap();
    let upload_id = created.upload_id().unwrap().to_owned();
    assert_eq!(block_on(server.pending_uploads(&bucket, "big.bin")), 1);

    let results = h.ok(
        "abort_multipart_uploads",
        json!({ "account_id": id, "uploads": [
            { "bucket": bucket, "key": "big.bin", "upload_id": upload_id }
        ] }),
    );
    assert_eq!(results[0]["aborted"], true, "{results}");
    assert_eq!(block_on(server.pending_uploads(&bucket, "big.bin")), 0);

    // This server cannot list uploads (s3s-fs): the bucket is a failure, not
    // a rejection of the call.
    let listing = h.ok(
        "list_multipart_uploads",
        json!({ "account_id": id, "buckets": [bucket] }),
    );
    assert_eq!(listing["uploads"], json!([]));
    assert_eq!(listing["failures"][0]["error"]["code"], "remote");
}

#[test]
fn listings_are_exported_in_every_format() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    for key in ["a.txt", "dir/b.txt", "dir/sub/c.bin"] {
        block_on(server.put(&bucket, key, key.as_bytes()));
    }
    let dir = tempfile::tempdir().unwrap();
    let export = |format: &str, recursive: bool, filter: Value, name: &str| {
        let path = dir.path().join(name);
        let outcome = h.ok(
            "export_listing",
            json!({
                "account_id": id, "bucket": bucket, "prefix": "", "recursive": recursive,
                "format": format, "path": path.to_str().unwrap(), "filter": filter
            }),
        );
        (path, outcome)
    };

    let (path, outcome) = export("csv", true, Value::Null, "all.csv");
    assert_eq!(keys(&outcome), ["bytes", "path", "rows"]);
    assert_eq!(outcome["rows"], 3);
    assert_eq!(outcome["path"], path.to_str().unwrap());
    assert_eq!(outcome["bytes"], fs::metadata(&path).unwrap().len());
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "key,size,last_modified,etag,storage_class");
    let first_keys: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l.split(',').next().unwrap())
        .collect();
    assert_eq!(first_keys, ["a.txt", "dir/b.txt", "dir/sub/c.bin"]);

    // One level: the folder is a row without a size.
    let (path, outcome) = export("csv", false, Value::Null, "level.csv");
    assert_eq!(outcome["rows"], 2);
    assert!(fs::read_to_string(&path).unwrap().contains("\ndir/,,,,\n"));

    // A filter (paths relative to the prefix).
    let spec = json!({
        "prefix": null, "glob": "**/*.txt", "min_size": null, "max_size": null,
        "modified_after": null, "modified_before": null, "storage_classes": []
    });
    let (path, outcome) = export("csv", true, spec, "txt.csv");
    assert_eq!(outcome["rows"], 2);
    assert!(!fs::read_to_string(&path).unwrap().contains("c.bin"));

    let (path, _) = export("xlsx", true, Value::Null, "all.xlsx");
    assert_eq!(&fs::read(&path).unwrap()[..2], b"PK");
    let (path, outcome) = export("parquet", true, Value::Null, "all.parquet");
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        (&bytes[..4], &bytes[bytes.len() - 4..]),
        (&b"PAR1"[..], &b"PAR1"[..])
    );
    assert_eq!(outcome["bytes"], bytes.len());
}

#[test]
fn a_failed_export_changes_nothing() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a.txt", "1"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keep.csv");
    fs::write(&path, "old contents").unwrap();
    let args = |bucket: &str, path: &str, format: &str, filter: Value| {
        json!({
            "account_id": id, "bucket": bucket, "prefix": "", "recursive": true,
            "format": format, "path": path, "filter": filter
        })
    };

    assert_eq!(
        h.code(
            "export_listing",
            args(
                &server.unique_bucket(),
                path.to_str().unwrap(),
                "csv",
                Value::Null
            )
        ),
        "not_found"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "old contents");
    assert_eq!(
        h.code(
            "export_listing",
            args(&bucket, "relative/listing.csv", "csv", Value::Null)
        ),
        "usage"
    );
    let missing_folder = dir.path().join("nope").join("x.csv");
    assert_eq!(
        h.code(
            "export_listing",
            args(
                &bucket,
                missing_folder.to_str().unwrap(),
                "csv",
                Value::Null
            )
        ),
        "io"
    );
    let bad_filter = json!({
        "prefix": null, "glob": "a[b", "min_size": null, "max_size": null,
        "modified_after": null, "modified_before": null, "storage_classes": []
    });
    assert_eq!(
        h.code(
            "export_listing",
            args(&bucket, path.to_str().unwrap(), "csv", bad_filter)
        ),
        "usage"
    );
    let mut folder: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    folder.sort();
    assert_eq!(folder, ["keep.csv"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "old contents");
}
