//! The client-side encryption commands (docs/desktop-api.md, "Client-side
//! encryption (T07)") against the in-process S3 server.

use std::fs;

use serde_json::json;

use crate::harness::*;

#[test]
fn encrypt_decrypt_in_place_and_the_password_states() {
    let server = start_s3();
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a.txt", b"hello client-side".to_vec()));
    let h = Harness::new();
    let id = h.add_account(&server, "Local");
    let base = json!({ "account_id": id, "bucket": bucket, "key": "a.txt" });
    let with = |extra: serde_json::Value| {
        let mut args = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        args
    };

    let probe = h.ok("cse_probe_object", base.clone());
    assert_eq!(probe["encrypted"], false);
    assert_eq!(
        h.code("cse_decrypt_object", with(json!({ "password": "pw" }))),
        "usage"
    );

    let done = h.ok(
        "cse_encrypt_object",
        with(json!({ "password": "pw", "compression": "zstd" })),
    );
    assert_eq!(done["size_before"], 17);
    assert!(block_on(server.get(&bucket, "a.txt")).starts_with(b"BBCSE"));
    let probe = h.ok("cse_probe_object", base.clone());
    assert_eq!(probe["encrypted"], true);
    assert_eq!(probe["marked"], true);
    assert_eq!(probe["compression"], "zstd");
    assert_eq!(
        h.code(
            "cse_encrypt_object",
            with(json!({ "password": "pw", "compression": null }))
        ),
        "conflict"
    );

    let (code, message) = h.error("cse_check_password", with(json!({ "password": null })));
    assert_eq!(code, "auth");
    assert!(message.contains("password not found"), "{message}");
    let (code, message) = h.error("cse_check_password", with(json!({ "password": "bad" })));
    assert_eq!(code, "auth");
    assert!(message.contains("password not valid"), "{message}");
    h.ok("cse_check_password", with(json!({ "password": "pw" })));

    h.ok("cse_decrypt_object", with(json!({ "password": "pw" })));
    assert_eq!(block_on(server.get(&bucket, "a.txt")), b"hello client-side");
}

#[test]
fn encrypted_upload_and_decrypted_download_through_the_queue() {
    let server = start_s3();
    let bucket = block_on(server.new_bucket());
    let h = Harness::new();
    let id = h.add_account(&server, "Local");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    fs::write(&file, b"queue payload").unwrap();

    let tasks = h.ok(
        "enqueue_upload_encrypted",
        json!({
            "account_id": id, "local_paths": [file.to_str().unwrap()],
            "bucket": bucket, "prefix": "", "password": "pw", "compression": "gzip",
        }),
    );
    for task in tasks.as_array().unwrap() {
        assert!(
            task["group_id"]
                .as_str()
                .unwrap()
                .starts_with("cse-enc-gzip-")
        );
        h.wait_state(task["id"].as_str().unwrap(), "completed");
    }
    assert!(block_on(server.get(&bucket, "note.txt")).starts_with(b"BBCSE"));

    let out = tempfile::tempdir().unwrap();
    let tasks = h.ok(
        "enqueue_download_decrypted",
        json!({
            "account_id": id, "bucket": bucket, "keys": ["note.txt"],
            "dest_dir": out.path().to_str().unwrap(), "password": "pw",
        }),
    );
    for task in tasks.as_array().unwrap() {
        h.wait_state(task["id"].as_str().unwrap(), "completed");
    }
    assert_eq!(
        fs::read(out.path().join("note.txt")).unwrap(),
        b"queue payload"
    );

    let other = tempfile::tempdir().unwrap();
    let tasks = h.ok(
        "enqueue_download_decrypted",
        json!({
            "account_id": id, "bucket": bucket, "keys": ["note.txt"],
            "dest_dir": other.path().to_str().unwrap(), "password": "wrong",
        }),
    );
    let failed = h.wait_state(tasks[0]["id"].as_str().unwrap(), "failed");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("password not valid")
    );
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);

    assert_eq!(
        h.code(
            "enqueue_upload_encrypted",
            json!({
                "account_id": id, "local_paths": ["relative.txt"], "bucket": bucket,
                "prefix": "", "password": "pw", "compression": null,
            }),
        ),
        "usage"
    );
}
