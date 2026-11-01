//! Browsing commands: buckets, paginated listings, folders, presigned URLs
//! and `open_object`.

use std::fs;

use bucketbridge_desktop::OPEN_DIR_NAME;
use serde_json::{Value, json};

use crate::harness::*;

/// Every page of a listing, following `next_token`.
fn all_pages(h: &Harness, id: &str, bucket: &str, prefix: &str, flat: bool) -> Vec<Value> {
    let mut pages = Vec::new();
    let mut token = Value::Null;
    loop {
        let page = h.ok(
            "list_objects",
            json!({
                "account_id": id, "bucket": bucket, "prefix": prefix,
                "flat": flat, "token": token
            }),
        );
        assert_eq!(keys(&page), ["entries", "next_token"]);
        token = page["next_token"].clone();
        pages.push(page);
        if token.is_null() {
            return pages;
        }
    }
}

fn entry_keys(pages: &[Value]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| page["entries"].as_array().unwrap().iter())
        .map(|entry| entry["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn buckets_folders_and_paginated_listings() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = server.unique_bucket();

    assert_eq!(
        h.ok(
            "create_bucket",
            json!({ "account_id": id, "name": bucket, "region": null })
        ),
        Value::Null
    );
    let buckets = h.ok("list_buckets", json!({ "account_id": id }));
    let listed = buckets.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        keys(&listed[0]),
        ["creation_date", "kind", "name", "owner", "region"]
    );
    assert_eq!(listed[0]["name"], bucket.as_str());
    // A custom endpoint makes it an external bucket.
    assert_eq!(listed[0]["kind"], "external");

    assert_eq!(
        h.ok(
            "create_folder",
            json!({ "account_id": id, "bucket": bucket, "key": "docs/" })
        ),
        Value::Null
    );
    // The default test server keeps folder markers as directories and never
    // lists them (docs/testing.md), so the marker is checked on disk.
    if let Some(root) = server.root() {
        assert!(root.join(&bucket).join("docs").is_dir());
    }
    assert_eq!(
        h.code(
            "create_folder",
            json!({ "account_id": id, "bucket": bucket, "key": "not-a-folder" })
        ),
        "usage"
    );
    for key in ["docs/a.txt", "docs/b.txt", "docs/c.txt", "docs/d.txt"] {
        block_on(server.put(&bucket, key, key.as_bytes()));
    }
    block_on(server.put(&bucket, "photos/x.jpg", b"jpg".to_vec()));
    block_on(server.put(&bucket, "top.txt", b"top".to_vec()));

    // Pages hold `advanced.list_page_size` entries.
    h.set_setting("advanced", "list_page_size", json!(2));
    let pages = all_pages(&h, &id, &bucket, "docs/", false);
    assert_eq!(pages.len(), 2);
    assert!(pages[0]["next_token"].is_string());
    assert_eq!(
        entry_keys(&pages),
        ["docs/a.txt", "docs/b.txt", "docs/c.txt", "docs/d.txt"]
    );
    let entry = &pages[0]["entries"][0];
    assert_eq!(
        keys(entry),
        [
            "checksums",
            "etag",
            "is_prefix",
            "key",
            "last_modified",
            "size",
            "storage_class"
        ]
    );
    assert_eq!(
        keys(&entry["checksums"]),
        ["crc32", "crc32c", "crc64nvme", "sha1", "sha256"]
    );
    assert_eq!(entry["size"], 10);
    assert_eq!(entry["is_prefix"], false);

    // Folders come first, then the objects; the folder marker itself is
    // never listed under its own prefix.
    let root = all_pages(&h, &id, &bucket, "", false);
    assert_eq!(entry_keys(&root), ["docs/", "photos/", "top.txt"]);
    let first = &root[0]["entries"][0];
    assert_eq!(first["is_prefix"], true);
    assert_eq!(first["size"], 0);

    // Flat: every object, no prefixes (the marker is not listed by this
    // server, see above).
    let flat = all_pages(&h, &id, &bucket, "", true);
    assert_eq!(
        entry_keys(&flat),
        [
            "docs/a.txt",
            "docs/b.txt",
            "docs/c.txt",
            "docs/d.txt",
            "photos/x.jpg",
            "top.txt"
        ]
    );

    assert_eq!(
        h.code(
            "list_objects",
            json!({
                "account_id": id, "bucket": server.unique_bucket(), "prefix": "",
                "flat": false, "token": null
            })
        ),
        "not_found"
    );
    assert_eq!(
        h.code("delete_bucket", json!({ "account_id": id, "name": bucket })),
        "conflict"
    );

    let empty = server.unique_bucket();
    h.ok(
        "create_bucket",
        json!({ "account_id": id, "name": empty, "region": null }),
    );
    assert_eq!(
        h.ok("delete_bucket", json!({ "account_id": id, "name": empty })),
        Value::Null
    );
    assert_eq!(
        h.code(
            "create_bucket",
            json!({ "account_id": id, "name": "Not A Valid Name", "region": null })
        ),
        "usage"
    );
}

#[test]
fn presigned_urls_download_and_upload() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "shared/report.txt", b"quarterly numbers".to_vec()));

    let presign = |key: &str, method: &str, expires_secs: u64| {
        json!({
            "account_id": id, "bucket": bucket, "key": key,
            "method": method, "expires_secs": expires_secs
        })
    };
    let get_url = h.ok("presign", presign("shared/report.txt", "get", 600));
    let get_url = get_url.as_str().unwrap().to_owned();
    assert!(get_url.contains("X-Amz-Signature="), "{get_url}");
    assert!(get_url.contains("X-Amz-Expires=600"), "{get_url}");
    let body = block_on(async {
        reqwest::get(&get_url)
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap()
    });
    assert_eq!(&body[..], b"quarterly numbers");

    let put_url = h.ok("presign", presign("shared/upload.txt", "put", 60));
    let put_url = put_url.as_str().unwrap().to_owned();
    block_on(async {
        reqwest::Client::new()
            .put(&put_url)
            .body("uploaded through a presigned URL")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    assert_eq!(
        block_on(server.get(&bucket, "shared/upload.txt")),
        b"uploaded through a presigned URL"
    );

    // 1 second to 7 days.
    assert_eq!(
        h.code("presign", presign("shared/report.txt", "get", 0)),
        "usage"
    );
    assert_eq!(
        h.code(
            "presign",
            presign("shared/report.txt", "get", 7 * 24 * 3600 + 1)
        ),
        "usage"
    );
}

#[test]
fn open_object_downloads_to_a_private_folder_and_opens_it() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "reports/q3: draft?.pdf", b"%PDF draft".to_vec()));

    let open = |key: &str| json!({ "account_id": id, "bucket": bucket, "key": key });
    assert_eq!(
        h.ok("open_object", open("reports/q3: draft?.pdf")),
        Value::Null
    );

    let opened = h.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    let path = &opened[0];
    let root = h.core.dirs().data_dir.join(OPEN_DIR_NAME);
    assert!(path.starts_with(&root), "{}", path.display());
    // The name is the key's last segment, valid on every OS.
    assert_eq!(path.file_name().unwrap(), "q3_ draft_.pdf");
    assert_eq!(fs::read(path).unwrap(), b"%PDF draft");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let folder = path.parent().unwrap();
        let mode = fs::metadata(folder).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    // Folders cannot be opened; a missing object leaves nothing behind.
    assert_eq!(h.code("open_object", open("reports/")), "usage");
    assert_eq!(
        h.code("open_object", open("reports/missing.pdf")),
        "not_found"
    );
    assert_eq!(h.opened.lock().unwrap().len(), 1);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn open_object_with_opens_the_copy_with_the_application_and_remembers_it() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let bucket = block_on(server.new_bucket());
    block_on(server.put(&bucket, "a/note.txt", b"hello".to_vec()));
    let args = |key: &str, app: &str| json!({ "account_id": id, "bucket": bucket, "key": key, "app": app });

    assert_eq!(
        h.ok("open_object_with", args("a/note.txt", "/Apps/Editor.app")),
        Value::Null
    );
    h.ok("open_object_with", args("a/note.txt", "/Apps/Other.app"));
    h.ok("open_object_with", args("a/note.txt", "/Apps/Editor.app"));
    let opened = h.opened_with.lock().unwrap().clone();
    assert_eq!(opened.len(), 3);
    assert_eq!(opened[0].1, "/Apps/Editor.app");
    assert_eq!(fs::read(&opened[0].0).unwrap(), b"hello");
    assert!(h.opened.lock().unwrap().is_empty());
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(
        settings["general"]["open_with_recent"],
        json!(["/Apps/Editor.app", "/Apps/Other.app"])
    );

    // Folders, blank applications and applications that fail to start are rejected
    // and do not enter the recent list.
    assert_eq!(
        h.code("open_object_with", args("a/", "/Apps/Editor.app")),
        "usage"
    );
    assert_eq!(h.code("open_object_with", args("a/note.txt", " ")), "usage");
    assert_eq!(
        h.code("open_object_with", args("a/note.txt", "/broken.app")),
        "io"
    );
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(
        settings["general"]["open_with_recent"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn open_bucket_in_browser_opens_the_bucket_url() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Main");
    let url = h.ok(
        "open_bucket_in_browser",
        json!({ "account_id": id, "bucket": "photos", "region": null }),
    );
    let urls = h.urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 1);
    assert_eq!(url, json!(urls[0]));
    // The test account has a custom endpoint, so the bucket is part of it.
    assert!(urls[0].contains("photos"), "{}", urls[0]);
    assert!(urls[0].starts_with("http"), "{}", urls[0]);
    assert_eq!(
        h.code(
            "open_bucket_in_browser",
            json!({ "account_id": "missing", "bucket": "b", "region": null })
        ),
        "not_found"
    );
}
