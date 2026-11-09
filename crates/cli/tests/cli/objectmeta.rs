//! `bb tag`, `bb storage-class`, `bb sse` and `bb acl` against the in-memory
//! `ObjectStore`, which answers like S3 does (see docs/testing.md).

use std::sync::Arc;

use bucketbridge_tests::{ObjectStore, StoredEncryption, StoredObject};
use serde_json::json;

use bucketbridge_cli::i18n::trf;

use super::*;

const BUCKET: &str = "bucket";
const KEY_A_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const KEY_B_HEX: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn setup() -> (Arc<ObjectStore>, MockS3, TempDir) {
    let store = ObjectStore::new();
    let mock = store.start();
    (store, mock, tempfile::tempdir().unwrap())
}

fn bb_run(mock: &MockS3, config: &TempDir, args: &[&str]) -> Output {
    run(bb_mock(mock, config.path()).args(args))
}

fn key_file(dir: &TempDir, name: &str, contents: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path.to_str().unwrap().to_owned()
}

#[tokio::test]
async fn tag_get_prints_a_line_per_tag_and_a_record_per_object() {
    let (store, mock, config) = setup();
    store.put(
        BUCKET,
        "a.txt",
        StoredObject::new(1).tag("env", "prod").tag("team", "data"),
    );
    store.put(BUCKET, "plain.txt", StoredObject::new(1));

    let out = bb_run(&mock, &config, &["tag", "get", "s3://bucket/a.txt"]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            "s3://bucket/a.txt\tenv=prod",
            "s3://bucket/a.txt\tteam=data"
        ]
    );
    // No tags, no lines.
    let out = bb_run(&mock, &config, &["tag", "get", "s3://bucket/plain.txt"]);
    out.expect_code(0);
    assert!(out.stdout.is_empty());

    let out = bb_run(
        &mock,
        &config,
        &["tag", "get", "s3://bucket/", "-r", "--json"],
    );
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [
            json!({ "bucket": "bucket", "key": "a.txt", "tags": { "env": "prod", "team": "data" }, "error": null }),
            json!({ "bucket": "bucket", "key": "plain.txt", "tags": {}, "error": null }),
        ]
    );

    let out = bb_run(
        &mock,
        &config,
        &["tag", "get", "s3://bucket/missing", "--json"],
    );
    out.expect_code(2);
    assert_eq!(out.records()[0]["error"]["code"], "not_found");
}

#[tokio::test]
async fn tag_set_merges_or_replaces_and_a_dry_run_writes_nothing() {
    let (store, mock, config) = setup();
    store.put(
        BUCKET,
        "a.txt",
        StoredObject::new(1).tag("keep", "k").tag("env", "dev"),
    );

    let dry = bb_run(
        &mock,
        &config,
        &["tag", "set", "s3://bucket/a.txt", "env=prod", "--dry-run"],
    );
    dry.expect_code(0);
    assert_eq!(
        dry.lines(),
        [trf("would tag {uri}", &[("uri", "s3://bucket/a.txt")])]
    );
    assert_eq!(store.object(BUCKET, "a.txt").unwrap().tags["env"], "dev");
    assert!(
        mock.requests().iter().all(|r| r.method != "PUT"),
        "a dry run must not write"
    );

    let out = bb_run(
        &mock,
        &config,
        &[
            "tag",
            "set",
            "s3://bucket/a.txt",
            "env=prod",
            "new=1",
            "--json",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [json!({
            "bucket": "bucket", "key": "a.txt", "tags": { "env": "prod", "new": "1" },
            "replace": false, "dry_run": false, "changed": true, "warnings": [], "error": null
        })]
    );
    let tags = store.object(BUCKET, "a.txt").unwrap().tags;
    assert_eq!(tags.len(), 3);
    assert_eq!(tags["keep"], "k");

    // The same tags again: nothing to do.
    let out = bb_run(
        &mock,
        &config,
        &["tag", "set", "s3://bucket/a.txt", "env=prod"],
    );
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf("unchanged {uri}", &[("uri", "s3://bucket/a.txt")])]
    );

    let out = bb_run(
        &mock,
        &config,
        &["tag", "set", "s3://bucket/a.txt", "only=this", "--replace"],
    );
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf("tagged {uri}", &[("uri", "s3://bucket/a.txt")])]
    );
    assert_eq!(
        store.object(BUCKET, "a.txt").unwrap().tags,
        [("only".to_owned(), "this".to_owned())]
            .into_iter()
            .collect()
    );

    // Quiet hides the success line, bad tags are usage errors.
    let out = bb_run(
        &mock,
        &config,
        &["tag", "set", "s3://bucket/a.txt", "x=1", "-q"],
    );
    out.expect_code(0);
    assert!(out.stdout.is_empty());
    for bad in ["novalue", "k=semi;colon", "aws:x=1"] {
        let out = bb_run(&mock, &config, &["tag", "set", "s3://bucket/a.txt", bad]);
        out.expect_code(1);
    }
}

#[tokio::test]
async fn tag_delete_removes_the_named_tags_or_every_tag() {
    let (store, mock, config) = setup();
    store.put(
        BUCKET,
        "a.txt",
        StoredObject::new(1)
            .tag("a", "1")
            .tag("b", "2")
            .tag("c", "3"),
    );
    let out = bb_run(
        &mock,
        &config,
        &["tag", "delete", "s3://bucket/a.txt", "a", "c", "--json"],
    );
    out.expect_code(0);
    assert_eq!(out.records()[0]["removed"], json!(["a", "c"]));
    assert_eq!(out.records()[0]["changed"], true);
    assert_eq!(store.object(BUCKET, "a.txt").unwrap().tags.len(), 1);

    let out = bb_run(&mock, &config, &["tag", "delete", "s3://bucket/a.txt"]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf(
            "removed tags from {uri}",
            &[("uri", "s3://bucket/a.txt")]
        )]
    );
    assert!(store.object(BUCKET, "a.txt").unwrap().tags.is_empty());
}

#[tokio::test]
async fn storage_class_changes_objects_and_reports_each_failure_with_the_first_exit_code() {
    let (store, mock, config) = setup();
    store.put(BUCKET, "logs/a.log", StoredObject::new(1).tag("k", "v"));
    store.put(BUCKET, "logs/b.log", StoredObject::new(1).class("GLACIER"));
    store.put(
        BUCKET,
        "logs/c.log",
        StoredObject::new(1).class("STANDARD_IA"),
    );
    store.put(BUCKET, "other.txt", StoredObject::new(1));

    let dry = bb_run(
        &mock,
        &config,
        &[
            "storage-class",
            "s3://bucket/logs/",
            "STANDARD_IA",
            "-r",
            "--dry-run",
        ],
    );
    dry.expect_code(2);
    assert_eq!(
        dry.lines(),
        [
            trf(
                "would change {uri} from {previous} to {class}",
                &[
                    ("uri", "s3://bucket/logs/a.log"),
                    ("previous", "STANDARD"),
                    ("class", "STANDARD_IA"),
                ],
            ),
            trf(
                "unchanged {uri} (already {what})",
                &[("uri", "s3://bucket/logs/c.log"), ("what", "STANDARD_IA")],
            )
        ]
    );
    assert!(dry.stderr.contains("archived (GLACIER)"), "{}", dry.stderr);
    assert!(
        mock.requests()
            .iter()
            .all(|r| r.header("x-amz-copy-source").is_none()),
        "a dry run must not copy"
    );

    let out = bb_run(
        &mock,
        &config,
        &[
            "storage-class",
            "s3://bucket/logs/",
            "standard_ia",
            "-r",
            "--json",
        ],
    );
    out.expect_code(2);
    let records = out.records();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[0],
        json!({
            "bucket": "bucket", "key": "logs/a.log", "storage_class": "STANDARD_IA",
            "previous": "STANDARD", "dry_run": false, "changed": true, "warnings": [], "error": null
        })
    );
    assert_eq!(records[1]["key"], "logs/b.log");
    assert_eq!(records[1]["changed"], false);
    assert_eq!(records[1]["error"]["code"], "conflict");
    assert_eq!(records[2]["changed"], false);
    assert_eq!(
        out.error_record()["message"],
        super::batch_summary(1, 3, "objects", "could not be changed")
    );
    assert_eq!(
        store
            .object(BUCKET, "logs/a.log")
            .unwrap()
            .storage_class
            .as_deref(),
        Some("STANDARD_IA")
    );
    assert_eq!(
        store.object(BUCKET, "logs/a.log").unwrap().tags.len(),
        1,
        "tags are kept"
    );
    assert_eq!(
        store.object(BUCKET, "other.txt").unwrap().storage_class,
        None,
        "outside the prefix"
    );

    // One object, by key.
    let out = bb_run(
        &mock,
        &config,
        &["storage-class", "s3://bucket/other.txt", "GLACIER_IR"],
    );
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf(
            "changed {uri} from {previous} to {class}",
            &[
                ("uri", "s3://bucket/other.txt"),
                ("previous", "STANDARD"),
                ("class", "GLACIER_IR"),
            ],
        )]
    );
}

#[tokio::test]
async fn storage_class_arguments_are_checked() {
    let (store, mock, config) = setup();
    store.put(BUCKET, "a.txt", StoredObject::new(1));
    for args in [
        vec!["storage-class", "s3://bucket/a.txt", "COLD"],
        vec!["storage-class", "s3://bucket/dir/", "STANDARD_IA"],
        vec!["storage-class", "s3://bucket/a.txt", "STANDARD_IA", "-r"],
        vec!["storage-class", "s3://bucket/a.txt"],
    ] {
        bb_run(&mock, &config, &args).expect_code(1);
    }
    let out = bb_run(
        &mock,
        &config,
        &["storage-class", "s3://bucket/a.txt", "COLD"],
    );
    assert!(out.stderr.contains("DEEP_ARCHIVE"), "{}", out.stderr);
    bb_run(
        &mock,
        &config,
        &["storage-class", "s3://bucket/missing", "GLACIER"],
    )
    .expect_code(2);
    assert!(
        mock.requests()
            .iter()
            .all(|r| r.header("x-amz-copy-source").is_none())
    );
}

#[tokio::test]
async fn sse_changes_the_encryption_and_keeps_the_rest() {
    let (store, mock, config) = setup();
    store.put(
        BUCKET,
        "a.txt",
        StoredObject::new(1).class("STANDARD_IA").tag("k", "v"),
    );
    store.put(
        BUCKET,
        "b.txt",
        StoredObject::new(1).encrypted(StoredEncryption::S3),
    );

    let out = bb_run(&mock, &config, &["sse", "s3://bucket/a.txt", "s3"]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf(
            "changed the encryption of {uri} from {previous} to {name}",
            &[
                ("uri", "s3://bucket/a.txt"),
                ("previous", "none"),
                ("name", "s3"),
            ],
        )]
    );
    let after = store.object(BUCKET, "a.txt").unwrap();
    assert_eq!(after.encryption, StoredEncryption::S3);
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));

    let out = bb_run(
        &mock,
        &config,
        &[
            "sse",
            "s3://bucket/a.txt",
            "kms:alias/data",
            "--bucket-key",
            "--json",
        ],
    );
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [json!({
            "bucket": "bucket", "key": "a.txt",
            "encryption": { "type": "kms", "key_id": "alias/data", "bucket_key": true },
            "previous": "s3", "dry_run": false, "changed": true, "warnings": [], "error": null
        })]
    );
    assert_eq!(
        store.object(BUCKET, "a.txt").unwrap().encryption,
        StoredEncryption::Kms {
            key_id: "alias/data".into(),
            bucket_key: true
        }
    );

    // Already as wanted.
    let out = bb_run(&mock, &config, &["sse", "s3://bucket/b.txt", "s3"]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [trf(
            "unchanged {uri} (already {what})",
            &[("uri", "s3://bucket/b.txt"), ("what", "s3")]
        )]
    );

    let dry = bb_run(
        &mock,
        &config,
        &["sse", "s3://bucket/b.txt", "kms", "--dry-run"],
    );
    dry.expect_code(0);
    assert!(
        dry.stdout.starts_with(&trf(
            "would change the encryption of {uri} from {previous} to {name}",
            &[
                ("uri", "s3://bucket/b.txt"),
                ("previous", "s3"),
                ("name", "kms"),
            ],
        )),
        "{}",
        dry.stdout
    );
    assert_eq!(
        store.object(BUCKET, "b.txt").unwrap().encryption,
        StoredEncryption::S3
    );
}

#[tokio::test]
async fn a_customer_key_file_encrypts_and_a_wrong_key_fails_loudly() {
    let (store, mock, config) = setup();
    let files = tempfile::tempdir().unwrap();
    let key_a = key_file(&files, "a.key", &format!("{KEY_A_HEX}\n"));
    let key_b = key_file(&files, "b.key", KEY_B_HEX);
    // The raw form, as `openssl rand 32` writes it.
    let raw = files.path().join("raw.key");
    std::fs::write(&raw, [0x11u8; 32]).unwrap();
    store.put(BUCKET, "a.txt", StoredObject::new(1).class("STANDARD_IA"));

    let out = bb_run(
        &mock,
        &config,
        &[
            "sse",
            "s3://bucket/a.txt",
            "c",
            "--key-file",
            &key_a,
            "--json",
        ],
    );
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["encryption"]["type"], "c");
    assert_eq!(record["encryption"]["key_md5"], "YSbdkQDUu1vJkAgt83fvFg==");
    assert!(
        !out.stdout.contains(KEY_A_HEX) && !out.stderr.contains(KEY_A_HEX),
        "the key is never printed"
    );
    let after = store.object(BUCKET, "a.txt").unwrap();
    assert_eq!(
        after.encryption,
        StoredEncryption::Customer {
            key_md5: "YSbdkQDUu1vJkAgt83fvFg==".into()
        }
    );
    assert_eq!(after.storage_class.as_deref(), Some("STANDARD_IA"));

    // Without the current key S3 rejects the request.
    let out = bb_run(&mock, &config, &["sse", "s3://bucket/a.txt", "s3"]);
    out.expect_code(2);
    assert!(out.stderr.contains("SSE-C"), "{}", out.stderr);

    // A wrong key is refused: exit 3, a message about SSE-C, nothing changed.
    let out = bb_run(
        &mock,
        &config,
        &[
            "sse",
            "s3://bucket/a.txt",
            "s3",
            "--source-key-file",
            &key_b,
        ],
    );
    out.expect_code(3);
    assert!(
        out.stderr.contains("SSE-C") && out.stderr.contains("refused"),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains(KEY_B_HEX), "{}", out.stderr);
    assert_eq!(
        store.object(BUCKET, "a.txt").unwrap().encryption,
        StoredEncryption::Customer {
            key_md5: "YSbdkQDUu1vJkAgt83fvFg==".into()
        }
    );

    // The right key (raw file) moves it to another key, and the class change
    // of an SSE-C object needs the key too.
    let out = bb_run(
        &mock,
        &config,
        &[
            "sse",
            "s3://bucket/a.txt",
            "c",
            "--key-file",
            &key_b,
            "--source-key-file",
            raw.to_str().unwrap(),
        ],
    );
    out.expect_code(0);
    let out = bb_run(
        &mock,
        &config,
        &["storage-class", "s3://bucket/a.txt", "GLACIER_IR"],
    );
    out.expect_code(2);
    let out = bb_run(
        &mock,
        &config,
        &[
            "storage-class",
            "s3://bucket/a.txt",
            "GLACIER_IR",
            "--key-file",
            &key_b,
        ],
    );
    out.expect_code(0);
    assert_eq!(
        store
            .object(BUCKET, "a.txt")
            .unwrap()
            .storage_class
            .as_deref(),
        Some("GLACIER_IR")
    );
}

#[tokio::test]
async fn sse_arguments_are_checked_before_anything_is_sent() {
    let (store, mock, config) = setup();
    store.put(BUCKET, "a.txt", StoredObject::new(1));
    let files = tempfile::tempdir().unwrap();
    let key = key_file(&files, "k", KEY_A_HEX);
    let short = key_file(&files, "short", "not a key");
    for args in [
        vec!["sse", "s3://bucket/a.txt", "c"],
        vec!["sse", "s3://bucket/a.txt", "s3", "--key-file", &key],
        vec!["sse", "s3://bucket/a.txt", "kms", "--key-file", &key],
        vec!["sse", "s3://bucket/a.txt", "s3", "--bucket-key"],
        vec!["sse", "s3://bucket/a.txt", "aes256"],
        vec!["sse", "s3://bucket/a.txt", "kms:"],
        vec!["sse", "s3://bucket/dir/", "s3"],
        vec!["sse", "s3://bucket/a.txt", "c", "--key-file", &short],
    ] {
        let out = bb_run(&mock, &config, &args);
        out.expect_code(1);
    }
    let out = bb_run(
        &mock,
        &config,
        &["sse", "s3://bucket/a.txt", "c", "--key-file", &short],
    );
    assert!(!out.stderr.contains("not a key"), "{}", out.stderr);
    // A key file that does not exist is a local failure.
    bb_run(
        &mock,
        &config,
        &[
            "sse",
            "s3://bucket/a.txt",
            "c",
            "--key-file",
            "/no/such/key",
        ],
    )
    .expect_code(2);
    assert!(mock.requests().is_empty(), "{:?}", mock.requests());
}

#[tokio::test]
async fn acl_get_and_set_work_on_objects_and_prefixes() {
    let (store, mock, config) = setup();
    store.put(BUCKET, "a.txt", StoredObject::new(1));
    store.put(BUCKET, "dir/b.txt", StoredObject::new(1).public_read());
    store.put(BUCKET, "dir/c.txt", StoredObject::new(1));

    let out = bb_run(&mock, &config, &["acl", "get", "s3://bucket/dir/", "-r"]);
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            "s3://bucket/dir/b.txt\tpublic-read",
            "s3://bucket/dir/c.txt\tprivate"
        ]
    );
    let out = bb_run(
        &mock,
        &config,
        &["acl", "get", "s3://bucket/a.txt", "--json"],
    );
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["acl"], "private");
    assert_eq!(record["grants"][0]["permission"], "FULL_CONTROL");
    assert!(record["owner"].is_string() && record["error"].is_null());

    let out = bb_run(
        &mock,
        &config,
        &["acl", "set", "s3://bucket/dir/", "public-read", "-r"],
    );
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            trf(
                "unchanged {uri} (already {what})",
                &[("uri", "s3://bucket/dir/b.txt"), ("what", "public-read")],
            ),
            trf(
                "set the ACL of {uri} to {acl}{was}",
                &[
                    ("uri", "s3://bucket/dir/c.txt"),
                    ("acl", "public-read"),
                    ("was", &trf(" (was {previous})", &[("previous", "private")])),
                ],
            )
        ]
    );
    let dry = bb_run(
        &mock,
        &config,
        &[
            "acl",
            "set",
            "s3://bucket/a.txt",
            "public-read",
            "--dry-run",
            "--json",
        ],
    );
    dry.expect_code(0);
    assert_eq!(dry.records()[0]["changed"], true);
    assert_eq!(dry.records()[0]["dry_run"], true);
    assert_eq!(
        store.object(BUCKET, "a.txt").unwrap().acl.len(),
        1,
        "a dry run changes nothing"
    );
    let custom = bb_run(
        &mock,
        &config,
        &["acl", "set", "s3://bucket/a.txt", "bucket-owner-read"],
    );
    custom.expect_code(0);
    let out = bb_run(&mock, &config, &["acl", "get", "s3://bucket/a.txt"]);
    assert!(
        out.stdout.starts_with("s3://bucket/a.txt\tcustom\t"),
        "{}",
        out.stdout
    );

    bb_run(
        &mock,
        &config,
        &["acl", "set", "s3://bucket/a.txt", "world-readable"],
    )
    .expect_code(1);
}

#[tokio::test]
async fn acls_on_a_bucket_that_disables_them_are_a_conflict_with_a_clear_message() {
    let (store, mock, config) = setup();
    store.disable_acls(BUCKET);
    store.put(BUCKET, "a.txt", StoredObject::new(1));
    let out = bb_run(
        &mock,
        &config,
        &["acl", "set", "s3://bucket/a.txt", "public-read", "--json"],
    );
    out.expect_code(2);
    let record = &out.records()[0];
    assert_eq!(record["changed"], false);
    assert_eq!(record["error"]["code"], "conflict");
    assert!(
        record["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ACLs are disabled"),
        "{record}"
    );
    // Reading works, and the default ACL is private: nothing to change.
    bb_run(&mock, &config, &["acl", "get", "s3://bucket/a.txt"]).expect_code(0);
    bb_run(
        &mock,
        &config,
        &["acl", "set", "s3://bucket/a.txt", "private"],
    )
    .expect_code(0);
}
