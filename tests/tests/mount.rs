//! The filesystem-independent layer of the read-only mount: inodes, listings,
//! ranged reads and the block cache, against the in-process server and a
//! scripted one. No kernel and no FUSE involved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ErrorKind;
use bucketbridge_core::mount::{MountCore, MountOptions, NodeKind, ROOT_INODE};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};

fn small_blocks() -> MountOptions {
    MountOptions {
        block_size: 4,
        cache_blocks: 8,
        ..MountOptions::default()
    }
}

#[tokio::test]
async fn directories_files_and_nested_lookups() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    server.put(&bucket, "top.txt", "hello world").await;
    server.put(&bucket, "docs/a.txt", "aaa").await;
    server.put(&bucket, "docs/deep/b.txt", "bbbb").await;
    let fs = MountCore::new(server.ops().clone(), &bucket, "", small_blocks());

    let root = fs.readdir(ROOT_INODE).await.unwrap();
    let names: Vec<(&str, NodeKind)> = root.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        names,
        [("docs", NodeKind::Dir), ("top.txt", NodeKind::File)]
    );

    let top = fs.lookup(ROOT_INODE, "top.txt").await.unwrap();
    assert_eq!((top.kind, top.size), (NodeKind::File, 11));
    let docs = fs.lookup(ROOT_INODE, "docs").await.unwrap();
    assert_eq!(docs.kind, NodeKind::Dir);
    let deep = fs.lookup(docs.ino, "deep").await.unwrap();
    assert_eq!(fs.parent(deep.ino).unwrap(), docs.ino);
    let b = fs.lookup(deep.ino, "b.txt").await.unwrap();
    assert_eq!(b.size, 4);
    // The same name keeps its inode across listings.
    assert_eq!(fs.lookup(ROOT_INODE, "docs").await.unwrap().ino, docs.ino);

    let missing = fs.lookup(ROOT_INODE, "nope").await.unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::NotFound);
    let not_dir = fs.readdir(top.ino).await.unwrap_err();
    assert_eq!(not_dir.kind(), ErrorKind::Usage);
    assert_eq!(fs.getattr(9999).unwrap_err().kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn reads_cross_block_boundaries_and_stop_at_the_end() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    let body: Vec<u8> = (0u8..=250).collect();
    server.put(&bucket, "data.bin", body.clone()).await;
    let fs = MountCore::new(server.ops().clone(), &bucket, "", small_blocks());
    let file = fs.lookup(ROOT_INODE, "data.bin").await.unwrap();

    assert_eq!(fs.read(file.ino, 0, 251).await.unwrap(), body);
    assert_eq!(fs.read(file.ino, 3, 6).await.unwrap(), body[3..9]);
    assert_eq!(fs.read(file.ino, 248, 100).await.unwrap(), body[248..]);
    assert!(fs.read(file.ino, 251, 10).await.unwrap().is_empty());
    assert!(fs.read(file.ino, 1000, 10).await.unwrap().is_empty());
    assert!(fs.read(file.ino, 0, 0).await.unwrap().is_empty());
    let dir_read = fs.read(ROOT_INODE, 0, 1).await.unwrap_err();
    assert_eq!(dir_read.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn a_prefix_mount_shows_only_that_folder() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    server.put(&bucket, "outside.txt", "x").await;
    server.put(&bucket, "photos/2026/a.jpg", "jpg").await;
    let fs = MountCore::new(server.ops().clone(), &bucket, "photos", small_blocks());
    let root = fs.readdir(ROOT_INODE).await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name, "2026");
    let year = fs.lookup(ROOT_INODE, "2026").await.unwrap();
    let file = fs.lookup(year.ino, "a.jpg").await.unwrap();
    assert_eq!(fs.read(file.ino, 0, 10).await.unwrap(), b"jpg");
}

#[tokio::test]
async fn an_empty_bucket_and_odd_names() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    let fs = MountCore::new(server.ops().clone(), &bucket, "", small_blocks());
    assert!(fs.readdir(ROOT_INODE).await.unwrap().is_empty());
    server.put(&bucket, "with space & more.txt", "ok").await;
    let fs = MountCore::new(server.ops().clone(), &bucket, "", small_blocks());
    let file = fs
        .lookup(ROOT_INODE, "with space & more.txt")
        .await
        .unwrap();
    assert_eq!(fs.read(file.ino, 0, 5).await.unwrap(), b"ok");
}

#[tokio::test]
async fn a_missing_bucket_is_an_error_not_an_empty_folder() {
    let server = TestS3::start().await;
    let fs = MountCore::new(
        server.ops().clone(),
        "no-such-bucket-xyz",
        "",
        small_blocks(),
    );
    let err = fs.readdir(ROOT_INODE).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const CONTENT: &str = "0123456789abcdefghij";

fn scripted(request: &RecordedRequest) -> MockResponse {
    if request.query_param("list-type").is_some() {
        return MockResponse::xml(format!(
            "<ListBucketResult {NS}><Name>b</Name><IsTruncated>false</IsTruncated>\
             <Contents><Key>f.txt</Key><Size>{}</Size><ETag>\"e1\"</ETag></Contents></ListBucketResult>",
            CONTENT.len()
        ));
    }
    let range = request
        .header("range")
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
    match range {
        Some((first, last)) if request.header("if-match") == Some("\"e1\"") => {
            let end = last.min(CONTENT.len() - 1);
            MockResponse {
                status: 206,
                headers: vec![
                    ("etag".into(), "\"e1\"".into()),
                    (
                        "content-range".into(),
                        format!("bytes {first}-{end}/{}", CONTENT.len()),
                    ),
                ],
                body: CONTENT[first..=end].to_owned(),
            }
        }
        Some(_) => MockResponse::error(412, "PreconditionFailed", "changed"),
        None => MockResponse::error(400, "Unexpected", "no range"),
    }
}

fn ranged_gets(server: &MockS3) -> Vec<String> {
    server
        .requests()
        .iter()
        .filter(|r| r.method == "GET" && r.query_param("list-type").is_none())
        .map(|r| r.header("range").unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn blocks_are_fetched_once_with_if_match_and_then_served_from_the_cache() {
    let server = MockS3::start(scripted);
    let options = MountOptions {
        block_size: 8,
        cache_blocks: 2,
        ..MountOptions::default()
    };
    let fs = MountCore::new(server.ops(), "b", "", options);
    let file = fs.lookup(ROOT_INODE, "f.txt").await.unwrap();

    assert_eq!(fs.read(file.ino, 2, 4).await.unwrap(), b"2345");
    assert_eq!(fs.read(file.ino, 0, 8).await.unwrap(), b"01234567");
    assert_eq!(ranged_gets(&server), ["bytes=0-7"]);

    assert_eq!(fs.read(file.ino, 6, 4).await.unwrap(), b"6789");
    assert_eq!(ranged_gets(&server), ["bytes=0-7", "bytes=8-15"]);
    // The last block is cut at the end of the file.
    assert_eq!(fs.read(file.ino, 16, 100).await.unwrap(), b"ghij");
    assert_eq!(
        ranged_gets(&server),
        ["bytes=0-7", "bytes=8-15", "bytes=16-19"]
    );
    // Only two blocks fit: block 0 was evicted and is fetched again.
    assert_eq!(fs.read(file.ino, 0, 1).await.unwrap(), b"0");
    assert_eq!(ranged_gets(&server).len(), 4);

    // The listing was fetched once for all the lookups and readdirs.
    let lists = server
        .requests()
        .iter()
        .filter(|r| r.query_param("list-type").is_some())
        .count();
    assert_eq!(lists, 1);
}

#[tokio::test]
async fn a_changed_object_fails_the_read_and_relists_next_time() {
    let server = MockS3::start(|request| {
        if request.query_param("list-type").is_some() {
            // The listing reports an ETag that the GET handler rejects.
            return MockResponse::xml(format!(
                "<ListBucketResult {NS}><Name>b</Name><IsTruncated>false</IsTruncated>\
                 <Contents><Key>f.txt</Key><Size>20</Size><ETag>\"stale\"</ETag></Contents></ListBucketResult>"
            ));
        }
        scripted(request)
    });
    let fs = MountCore::new(server.ops(), "b", "", MountOptions::default());
    let file = fs.lookup(ROOT_INODE, "f.txt").await.unwrap();
    let err = fs.read(file.ino, 0, 5).await.unwrap_err();
    assert_ne!(err.kind(), ErrorKind::Internal);
    // The directory cache was dropped: the next lookup lists again.
    fs.lookup(ROOT_INODE, "f.txt").await.unwrap();
    let lists = server
        .requests()
        .iter()
        .filter(|r| r.query_param("list-type").is_some())
        .count();
    assert_eq!(lists, 2);
}
