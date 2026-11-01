//! The version and preview commands: shapes, error codes, and what reaches
//! the server. Versioning needs a server s3s-fs is not, so the bucket is a
//! scripted `MockS3` (a small simulation of `ListObjectVersions`); the
//! previews read real objects from the in-process server.

use std::fs;

use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};
use serde_json::{Value, json};

use crate::harness::*;

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const UNKNOWN_ID: &str = "00000000-0000-4000-8000-000000000000";

/// The fields of a `TransferTask`, sorted.
const TASK_KEYS: [&str; 15] = [
    "account_id",
    "attempts",
    "bytes_done",
    "bytes_total",
    "created_at",
    "dst",
    "error",
    "eta",
    "group_id",
    "id",
    "kind",
    "speed",
    "src",
    "state",
    "updated_at",
];

/// The fields of an `ObjectVersion`, sorted.
const VERSION_KEYS: [&str; 8] = [
    "etag",
    "is_delete_marker",
    "is_latest",
    "key",
    "last_modified",
    "size",
    "storage_class",
    "version_id",
];

/// The fields of an `ObjectPreview`, sorted.
const PREVIEW_KEYS: [&str; 16] = [
    "content_type",
    "etag",
    "formatted",
    "image_base64",
    "key",
    "kind",
    "last_modified",
    "length",
    "media_type",
    "offset",
    "page",
    "page_count",
    "page_size",
    "size",
    "text",
    "version_id",
];

fn unexpected(request: &RecordedRequest) -> MockResponse {
    MockResponse::error(
        400,
        "UnexpectedRequest",
        &format!(
            "unexpected {} {}?{}",
            request.method, request.path, request.query
        ),
    )
}

/// An account for `mock` (it accepts any credentials).
fn add_mock_account(h: &Harness, mock: &MockS3) -> String {
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": format!("Mock {}", mock.endpoint()),
            "access_key_id": "AKIDMOCK",
            "secret_access_key": "mock-secret",
            "session_token": null,
            "region": "us-east-1",
            "endpoint": mock.endpoint(),
            "path_style": true,
            "request_payer": false,
            "mfa_serial": null,
        } }),
    );
    account["id"].as_str().unwrap().to_owned()
}

/// `(key, version id, latest, delete marker, day of October 2026)`
type Entry = (&'static str, &'static str, bool, bool, u32);

fn entry_xml((key, id, latest, marker, day): &Entry) -> String {
    if *marker {
        format!(
            "<DeleteMarker><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>{latest}</IsLatest><LastModified>2026-10-{day:02}T12:00:00.000Z</LastModified></DeleteMarker>"
        )
    } else {
        format!(
            "<Version><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>{latest}</IsLatest><LastModified>2026-10-{day:02}T12:00:00.000Z</LastModified><ETag>\"etag-{id}\"</ETag><Size>10</Size><StorageClass>STANDARD</StorageClass></Version>"
        )
    }
}

/// `ListObjectVersions` over `entries` with S3's prefix, `max-keys` and
/// marker rules.
fn serve_versions(
    entries: Vec<Entry>,
) -> impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static {
    move |request| {
        if request.method != "GET" || request.query_param("versions").is_none() {
            return unexpected(request);
        }
        let prefix = request.query_param("prefix").unwrap_or_default();
        let max = request
            .query_param("max-keys")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1000);
        let matching: Vec<&Entry> = entries
            .iter()
            .filter(|entry| entry.0.starts_with(prefix.as_str()))
            .collect();
        let start = match (
            request.query_param("key-marker"),
            request.query_param("version-id-marker"),
        ) {
            (Some(key), Some(id)) => matching
                .iter()
                .position(|entry| entry.0 == key && entry.1 == id)
                .map_or(matching.len(), |at| at + 1),
            (Some(key), None) => matching
                .iter()
                .position(|entry| entry.0 > key.as_str())
                .unwrap_or(matching.len()),
            _ => 0,
        };
        let end = (start + max).min(matching.len());
        let page = &matching[start..end];
        let truncation = match page.last() {
            Some(last) if end < matching.len() => format!(
                "<IsTruncated>true</IsTruncated><NextKeyMarker>{}</NextKeyMarker><NextVersionIdMarker>{}</NextVersionIdMarker>",
                last.0, last.1
            ),
            _ => "<IsTruncated>false</IsTruncated>".to_owned(),
        };
        let body: String = page.iter().map(|entry| entry_xml(entry)).collect();
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><Prefix>{prefix}</Prefix><MaxKeys>{max}</MaxKeys>{truncation}{body}</ListVersionsResult>"
        ))
    }
}

fn dataset() -> Vec<Entry> {
    vec![
        ("a.txt", "a3", true, true, 28),
        ("a.txt", "a2", false, false, 10),
        ("a.txt", "a1", false, false, 5),
        ("b.txt", "b1", true, false, 12),
        ("c.txt", "c2", true, false, 25),
        ("c.txt", "c1", false, false, 15),
    ]
}

/// A `VersionFilter` with every field, changed by `overrides`.
fn filter(overrides: Value) -> Value {
    let mut filter = json!({
        "show_versions": true,
        "delete_markers": "include",
        "multiple_versions": false,
        "count_greater_than": null,
        "count_less_than": null,
        "modified_after": null,
        "modified_before": null,
    });
    for (name, value) in overrides.as_object().unwrap() {
        filter[name] = value.clone();
    }
    filter
}

fn list_args(id: &str, prefix: &str, exact: bool, filter: Value, token: Value) -> Value {
    json!({
        "account_id": id, "bucket": "bucket", "prefix": prefix,
        "exact": exact, "filter": filter, "token": token
    })
}

fn group_lines(page: &Value) -> Vec<String> {
    page["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            let ids: Vec<&str> = group["versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|version| version["version_id"].as_str().unwrap())
                .collect();
            format!("{}: {}", group["key"].as_str().unwrap(), ids.join(" "))
        })
        .collect()
}

#[test]
fn list_versions_groups_keys_pages_and_filters() {
    let mock = MockS3::start(serve_versions(dataset()));
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    // A page holds `list_page_size` keys.
    h.set_setting("advanced", "list_page_size", json!(2));

    let first = h.ok(
        "list_versions",
        list_args(&id, "", false, filter(json!({})), Value::Null),
    );
    assert_eq!(keys(&first), ["groups", "next_token", "scanned"]);
    assert_eq!(group_lines(&first), ["a.txt: a3 a2 a1", "b.txt: b1"]);
    let group = &first["groups"][0];
    assert_eq!(
        keys(group),
        [
            "is_deleted",
            "key",
            "matching",
            "total",
            "truncated",
            "versions"
        ]
    );
    assert_eq!(group["is_deleted"], true);
    assert_eq!(
        (&group["total"], &group["matching"]),
        (&json!(3), &json!(3))
    );
    assert_eq!(group["truncated"], false);
    let marker = &group["versions"][0];
    assert_eq!(keys(marker), VERSION_KEYS);
    assert_eq!(marker["is_delete_marker"], true);
    assert_eq!(marker["is_latest"], true);
    assert_eq!(marker["size"], 0);
    assert_eq!(marker["etag"], Value::Null);
    assert_eq!(marker["last_modified"], "2026-10-28T12:00:00Z");
    let version = &group["versions"][1];
    assert_eq!(keys(version), VERSION_KEYS);
    assert_eq!(version["etag"], "etag-a2");
    assert_eq!(version["storage_class"], "STANDARD");

    let token = first["next_token"].clone();
    assert!(token.is_string());
    let second = h.ok(
        "list_versions",
        list_args(&id, "", false, filter(json!({})), token),
    );
    assert_eq!(group_lines(&second), ["c.txt: c2 c1"]);
    assert_eq!(second["next_token"], Value::Null);

    // Filters: delete markers only, multiple versions, dates, a count.
    h.set_setting("advanced", "list_page_size", json!(1000));
    let lines = |overrides: Value| {
        group_lines(&h.ok(
            "list_versions",
            list_args(&id, "", false, filter(overrides), Value::Null),
        ))
    };
    assert_eq!(lines(json!({ "delete_markers": "only" })), ["a.txt: a3"]);
    assert_eq!(
        lines(json!({ "delete_markers": "exclude" })),
        ["a.txt: a2 a1", "b.txt: b1", "c.txt: c2 c1"]
    );
    assert_eq!(
        lines(json!({ "show_versions": false })),
        ["a.txt: a3"],
        "versions off leaves the delete markers"
    );
    assert_eq!(
        lines(json!({ "multiple_versions": true })),
        ["a.txt: a3 a2 a1", "c.txt: c2 c1"]
    );
    assert_eq!(
        lines(json!({ "count_greater_than": 2 })),
        ["a.txt: a3 a2 a1"]
    );
    assert_eq!(lines(json!({ "count_less_than": 2 })), ["b.txt: b1"]);
    assert_eq!(
        lines(json!({
            "modified_after": "2026-10-10T12:00:00Z",
            "modified_before": "2026-10-26T00:00:00Z"
        })),
        ["a.txt: a2", "b.txt: b1", "c.txt: c2 c1"]
    );

    // One key, exactly.
    let exact = h.ok(
        "list_versions",
        list_args(&id, "b.txt", true, filter(json!({})), Value::Null),
    );
    assert_eq!(group_lines(&exact), ["b.txt: b1"]);
}

#[test]
fn list_versions_rejects_bad_requests_with_the_contract_codes() {
    let mock = MockS3::start(serve_versions(dataset()));
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    let list = |id: &str, filter: Value, token: Value, exact: bool, prefix: &str| {
        h.code("list_versions", list_args(id, prefix, exact, filter, token))
    };
    assert_eq!(
        list(
            &id,
            filter(json!({ "show_versions": false, "delete_markers": "exclude" })),
            Value::Null,
            false,
            ""
        ),
        "usage"
    );
    assert_eq!(
        list(
            &id,
            filter(
                json!({ "modified_after": "2026-10-10T00:00:00Z", "modified_before": "2026-10-10T00:00:00Z" })
            ),
            Value::Null,
            false,
            ""
        ),
        "usage"
    );
    assert_eq!(
        list(
            &id,
            filter(json!({ "count_greater_than": 4, "count_less_than": 5 })),
            Value::Null,
            false,
            ""
        ),
        "usage"
    );
    assert_eq!(
        list(&id, filter(json!({})), json!("not a token"), false, ""),
        "usage"
    );
    assert_eq!(list(&id, filter(json!({})), Value::Null, true, ""), "usage");
    assert_eq!(
        list(UNKNOWN_ID, filter(json!({})), Value::Null, false, ""),
        "not_found"
    );
    // Nothing of that reached the server.
    assert!(mock.requests().is_empty());

    // The server's own answers keep their codes.
    let denied = MockS3::start(|_| MockResponse::error(403, "AccessDenied", "Access Denied"));
    let id = add_mock_account(&h, &denied);
    assert_eq!(list(&id, filter(json!({})), Value::Null, false, ""), "auth");
}

/// HEAD answers for `restore_version` and `enqueue_download_version`.
fn head(version: &str, size: u64) -> MockResponse {
    MockResponse::empty(200)
        .with_header("content-length", &size.to_string())
        .with_header("etag", "\"etag-old\"")
        .with_header("last-modified", "Wed, 21 Oct 2026 07:28:00 GMT")
        .with_header("content-type", "text/plain")
        .with_header("x-amz-version-id", version)
}

#[test]
fn restore_version_makes_a_copy_current_and_keeps_the_contract_shape() {
    let mock = MockS3::start(|request| {
        let version = request.query_param("versionId");
        match (request.method.as_str(), version.as_deref()) {
            ("HEAD", Some("v1")) => head("v1", 5),
            ("HEAD", Some("v3")) => head("v3", 7),
            ("HEAD", Some("dm")) => MockResponse::empty(405),
            ("HEAD", Some(_)) => MockResponse::empty(404),
            ("HEAD", None) => head("v3", 7),
            ("PUT", None) if request.header("x-amz-copy-source").is_some() => MockResponse::xml(
                format!("<CopyObjectResult {NS}><ETag>\"etag-old\"</ETag></CopyObjectResult>"),
            )
            .with_header("x-amz-version-id", "v4"),
            _ => unexpected(request),
        }
    });
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    let args = |version: &str| json!({ "account_id": id, "bucket": "bucket", "key": "dir/doc.txt", "version_id": version });
    let outcome = h.ok("restore_version", args("v1"));
    assert_eq!(
        keys(&outcome),
        ["etag", "key", "restored_version_id", "size", "version_id"]
    );
    assert_eq!(
        outcome,
        json!({
            "key": "dir/doc.txt",
            "restored_version_id": "v1",
            "version_id": "v4",
            "etag": "etag-old",
            "size": 5
        })
    );
    let copy = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(
        copy.header("x-amz-copy-source"),
        Some("bucket/dir/doc.txt?versionId=v1")
    );

    // The current version is a conflict, a delete marker a usage error, a
    // missing version is not found.
    assert_eq!(h.code("restore_version", args("v3")), "conflict");
    let (code, message) = h.error("restore_version", args("dm"));
    assert_eq!(code, "usage");
    assert!(message.contains("delete marker"), "{message}");
    assert_eq!(h.code("restore_version", args("gone")), "not_found");
    assert_eq!(h.code("restore_version", args("")), "usage");
    assert_eq!(
        h.code(
            "restore_version",
            json!({ "account_id": UNKNOWN_ID, "bucket": "bucket", "key": "k", "version_id": "v1" })
        ),
        "not_found"
    );
    assert_eq!(
        mock.requests().iter().filter(|r| r.method == "PUT").count(),
        1
    );
}

#[test]
fn purge_versions_reports_what_was_deleted_and_what_was_refused() {
    let mock = MockS3::start(|request| {
        if request.method == "POST" && request.query_param("delete").is_some() {
            let refused = request
                .body_text()
                .contains("<Key>locked.txt</Key><VersionId>v9</VersionId>");
            let errors = if refused {
                "<Error><Key>locked.txt</Key><VersionId>v9</VersionId><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"
            } else {
                ""
            };
            MockResponse::xml(format!("<DeleteResult {NS}>{errors}</DeleteResult>"))
        } else {
            unexpected(request)
        }
    });
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    let versions = json!([
        { "key": "a.txt", "version_id": "v1" },
        { "key": "a.txt", "version_id": "dm" },
        { "key": "locked.txt", "version_id": "v9" },
    ]);
    let outcome = h.ok(
        "purge_versions",
        json!({ "account_id": id, "bucket": "bucket", "versions": versions }),
    );
    assert_eq!(keys(&outcome), ["deleted", "failures"]);
    assert_eq!(outcome["deleted"], 2);
    let failure = &outcome["failures"][0];
    assert_eq!(keys(failure), ["code", "key", "message", "version_id"]);
    assert_eq!(
        *failure,
        json!({
            "key": "locked.txt",
            "version_id": "v9",
            "code": "AccessDenied",
            "message": "Access Denied"
        })
    );
    let sent = mock.requests()[0].body_text();
    assert!(
        sent.contains("<Key>a.txt</Key><VersionId>dm</VersionId>"),
        "{sent}"
    );

    // Nothing to purge, a missing version ID and an unknown account.
    let none = h.ok(
        "purge_versions",
        json!({ "account_id": id, "bucket": "bucket", "versions": [] }),
    );
    assert_eq!(none, json!({ "deleted": 0, "failures": [] }));
    assert_eq!(
        h.code(
            "purge_versions",
            json!({ "account_id": id, "bucket": "bucket", "versions": [{ "key": "k", "version_id": "" }] })
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "purge_versions",
            json!({ "account_id": UNKNOWN_ID, "bucket": "bucket", "versions": versions })
        ),
        "not_found"
    );
    assert_eq!(
        mock.requests().len(),
        1,
        "only the first call reached the server"
    );
}

#[test]
fn a_version_downloads_as_an_ordinary_queue_task() {
    let mock = MockS3::start(|request| {
        let version = request.query_param("versionId");
        match (request.method.as_str(), version.as_deref()) {
            ("HEAD", Some("old")) => head("old", 11),
            ("HEAD", Some("dm")) => MockResponse::empty(405),
            ("HEAD", Some(_)) => MockResponse::empty(404),
            ("GET", Some("old")) => MockResponse {
                status: 200,
                headers: vec![
                    ("etag".into(), "\"etag-old\"".into()),
                    (
                        "last-modified".into(),
                        "Wed, 21 Oct 2026 07:28:00 GMT".into(),
                    ),
                ],
                body: "old content".into(),
            },
            _ => unexpected(request),
        }
    });
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    let dest = h.dir.path().join("saved").join("doc (old).txt");
    let tasks = h.ok(
        "enqueue_download_version",
        json!({
            "account_id": id, "bucket": "bucket", "key": "dir/doc.txt",
            "version_id": "old", "dest_path": dest.to_str().unwrap()
        }),
    );
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(keys(task), TASK_KEYS);
    assert_eq!(task["kind"], "download");
    assert_eq!(task["account_id"], id.as_str());
    assert_eq!(
        task["src"],
        json!({ "type": "s3", "bucket": "bucket", "key": "dir/doc.txt" })
    );
    assert_eq!(
        task["dst"],
        json!({ "type": "local", "path": dest.to_str().unwrap() })
    );
    assert_eq!(task["bytes_total"], 11);
    let done = h.wait_state(task["id"].as_str().unwrap(), "completed");
    assert_eq!(done["bytes_done"], 11);
    assert_eq!(fs::read_to_string(&dest).unwrap(), "old content");
    let get = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "GET")
        .unwrap();
    assert_eq!(get.path, "/bucket/dir/doc.txt");
    assert_eq!(get.query_param("versionId").as_deref(), Some("old"));

    // What is refused never becomes a task.
    let before = h.tasks().len();
    let attempt = |version: &str, path: &str| {
        h.code(
            "enqueue_download_version",
            json!({
                "account_id": id, "bucket": "bucket", "key": "dir/doc.txt",
                "version_id": version, "dest_path": path
            }),
        )
    };
    assert_eq!(attempt("old", "relative/doc.txt"), "usage");
    assert_eq!(attempt("dm", dest.to_str().unwrap()), "usage");
    assert_eq!(attempt("gone", dest.to_str().unwrap()), "not_found");
    assert_eq!(attempt("", dest.to_str().unwrap()), "usage");
    assert_eq!(
        h.code(
            "enqueue_download_version",
            json!({
                "account_id": UNKNOWN_ID, "bucket": "bucket", "key": "k",
                "version_id": "old", "dest_path": dest.to_str().unwrap()
            })
        ),
        "not_found"
    );
    assert_eq!(h.tasks().len(), before);
}

/// RFC 4648 base64, to compare with the preview's image.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * i))
        });
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn previews_text_json_images_and_the_other_states() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    let preview = |key: &str, version: Value, page: u64, page_size: Value| {
        json!({
            "account_id": id, "bucket": bucket, "key": key,
            "version_id": version, "page": page, "page_size": page_size
        })
    };

    block_on(server.put(&bucket, "notes.txt", "hello\nworld\n"));
    let text = h.ok(
        "preview_object",
        preview("notes.txt", Value::Null, 0, Value::Null),
    );
    assert_eq!(keys(&text), PREVIEW_KEYS);
    assert_eq!(text["kind"], "text");
    assert_eq!(text["text"], "hello\nworld\n");
    assert_eq!(text["key"], "notes.txt");
    assert_eq!(
        (&text["page"], &text["page_count"], &text["page_size"]),
        (&json!(0), &json!(1), &json!(131_072))
    );
    assert_eq!(
        (&text["offset"], &text["length"], &text["size"]),
        (&json!(0), &json!(12), &json!(12))
    );
    assert_eq!(text["formatted"], false);
    assert_eq!(text["image_base64"], Value::Null);
    assert_eq!(text["media_type"], "text/plain");

    // Pages of 1 KiB.
    let long: String = (0..400).map(|n| format!("line {n:04}\n")).collect();
    block_on(server.put(&bucket, "long.log", long.clone()));
    let first = h.ok(
        "preview_object",
        preview("long.log", Value::Null, 0, json!(1024)),
    );
    assert_eq!(first["page_count"], (long.len() as u64).div_ceil(1024));
    let mut collected = String::new();
    for page in 0..first["page_count"].as_u64().unwrap() {
        let answer = h.ok(
            "preview_object",
            preview("long.log", Value::Null, page, json!(1024)),
        );
        assert_eq!(answer["page"], page);
        assert_eq!(answer["offset"].as_u64().unwrap(), collected.len() as u64);
        collected.push_str(answer["text"].as_str().unwrap());
    }
    assert_eq!(collected, long);
    assert_eq!(
        h.code(
            "preview_object",
            preview(
                "long.log",
                Value::Null,
                first["page_count"].as_u64().unwrap(),
                json!(1024)
            )
        ),
        "usage"
    );

    block_on(server.put(&bucket, "doc.json", r#"{"b":[1,2],"a":{}}"#));
    let json_page = h.ok(
        "preview_object",
        preview("doc.json", Value::Null, 0, Value::Null),
    );
    assert_eq!(json_page["kind"], "json");
    assert_eq!(json_page["formatted"], true);
    assert_eq!(
        json_page["text"],
        "{\n  \"b\": [\n    1,\n    2\n  ],\n  \"a\": {}\n}"
    );

    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend((0..200u32).map(|n| (n * 13 % 251) as u8 | 0x80));
    block_on(server.put(&bucket, "pic.png", png.clone()));
    let image = h.ok(
        "preview_object",
        preview("pic.png", Value::Null, 0, Value::Null),
    );
    assert_eq!(image["kind"], "image");
    assert_eq!(image["media_type"], "image/png");
    assert_eq!(image["image_base64"], base64(&png));
    assert_eq!(image["text"], Value::Null);
    assert_eq!(image["size"], png.len());

    block_on(server.put(&bucket, "data.bin", vec![0u8, 1, 2, 0xFF]));
    assert_eq!(
        h.ok(
            "preview_object",
            preview("data.bin", Value::Null, 0, Value::Null)
        )["kind"],
        "binary"
    );
    block_on(server.put(&bucket, "empty.txt", Vec::new()));
    let empty = h.ok(
        "preview_object",
        preview("empty.txt", Value::Null, 0, Value::Null),
    );
    assert_eq!(empty["kind"], "empty");
    assert_eq!(empty["size"], 0);
    assert_eq!(keys(&empty), PREVIEW_KEYS);

    // Errors.
    assert_eq!(
        h.code(
            "preview_object",
            preview("missing.txt", Value::Null, 0, Value::Null)
        ),
        "not_found"
    );
    assert_eq!(
        h.code(
            "preview_object",
            preview("folder/", Value::Null, 0, Value::Null)
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "preview_object",
            preview("notes.txt", Value::Null, 0, json!(15))
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "preview_object",
            preview("notes.txt", Value::Null, 0, json!(1_048_577))
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "preview_object",
            preview("notes.txt", json!(""), 0, Value::Null)
        ),
        "usage"
    );
    assert_eq!(
        h.code(
            "preview_object",
            json!({ "account_id": UNKNOWN_ID, "bucket": bucket, "key": "notes.txt", "version_id": null, "page": 0, "page_size": null })
        ),
        "not_found"
    );
}

#[test]
fn a_preview_of_one_version_names_the_version() {
    let mock = MockS3::start(|request| {
        if request.query_param("versionId").as_deref() != Some("v1") {
            return unexpected(request);
        }
        MockResponse {
            status: 206,
            headers: vec![
                ("content-range".into(), "bytes 0-7/8".into()),
                ("etag".into(), "\"etag-1\"".into()),
                ("x-amz-version-id".into(), "v1".into()),
                ("content-type".into(), "text/plain".into()),
            ],
            body: "old text".into(),
        }
    });
    let h = Harness::new();
    let id = add_mock_account(&h, &mock);
    let answer = h.ok(
        "preview_object",
        json!({ "account_id": id, "bucket": "bucket", "key": "k.txt", "version_id": "v1", "page": 0, "page_size": null }),
    );
    assert_eq!(answer["text"], "old text");
    assert_eq!(answer["version_id"], "v1");
    assert_eq!(answer["etag"], "etag-1");
    assert_eq!(
        mock.requests()[0].query_param("versionId").as_deref(),
        Some("v1")
    );
}
