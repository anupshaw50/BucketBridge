//! Listing the remote side of a sync (v0.2.0): the parallel listing with a bounded
//! number of requests in flight, and the local metadata cache with its validity check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::S3Uri;
use bucketbridge_core::sync::{ListOptions, ListingCache, ListingSource, list_folder};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};
use std::collections::BTreeSet;

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

fn keys_of(entries: &[bucketbridge_core::model::ObjectEntry]) -> BTreeSet<String> {
    entries.iter().map(|entry| entry.key.clone()).collect()
}

#[tokio::test]
async fn parallel_listing_returns_what_a_flat_listing_returns() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    for index in 0..6 {
        for file in ["a.txt", "deep/b.txt"] {
            s3.put(&bucket, &format!("root/dir{index}/{file}"), "x")
                .await;
        }
    }
    s3.put(&bucket, "root/top.txt", "top").await;
    s3.put(&bucket, "root/zz/c.txt", "c").await;
    s3.put(&bucket, "elsewhere/ignored.txt", "no").await;
    let folder = S3Uri::new(&bucket, "root/").unwrap();

    let flat = list_folder(s3.ops(), &folder, &ListOptions::default())
        .await
        .unwrap();
    assert_eq!(flat.source, ListingSource::Fresh);
    assert_eq!(flat.entries.len(), 6 * 2 + 2);
    for parallelism in [2, 4, 32] {
        let parallel = list_folder(
            s3.ops(),
            &folder,
            &ListOptions {
                parallelism,
                cache: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(keys_of(&parallel.entries), keys_of(&flat.entries));
        assert_eq!(parallel.entries.len(), flat.entries.len());
    }
    // An empty prefix lists the bucket.
    let whole = S3Uri::new(&bucket, "").unwrap();
    let all = list_folder(
        s3.ops(),
        &whole,
        &ListOptions {
            parallelism: 4,
            cache: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(all.entries.len(), flat.entries.len() + 1);
}

fn list_page(keys: &[String]) -> MockResponse {
    let contents: String = keys
        .iter()
        .map(|key| {
            format!(
                "<Contents><Key>{key}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Contents>"
            )
        })
        .collect();
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>bucket</Name><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>",
        keys.len()
    ))
}

fn delimiter_page(folders: &[String]) -> MockResponse {
    let prefixes: String = folders
        .iter()
        .map(|folder| format!("<CommonPrefixes><Prefix>{folder}</Prefix></CommonPrefixes>"))
        .collect();
    MockResponse::xml(format!(
        "<ListBucketResult {NS}><Name>bucket</Name><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys><Delimiter>/</Delimiter><IsTruncated>false</IsTruncated>{prefixes}</ListBucketResult>",
        folders.len()
    ))
}

/// A server with eight sub-folders, each holding one file.
fn folder_server() -> MockS3 {
    MockS3::start(move |request: &RecordedRequest| {
        if request.method != "GET" || request.query_param("list-type").as_deref() != Some("2") {
            return MockResponse::error(400, "UnexpectedRequest", "unexpected");
        }
        let prefix = request.query_param("prefix").unwrap_or_default();
        if request.query_param("delimiter").as_deref() == Some("/") {
            let folders: Vec<String> = (0..8).map(|index| format!("{prefix}d{index}/")).collect();
            return delimiter_page(&folders);
        }
        list_page(&[format!("{prefix}file")])
    })
}

#[tokio::test]
async fn the_parallel_listing_asks_for_the_first_level_then_for_each_sub_folder() {
    let mock = folder_server();
    let folder = S3Uri::new("bucket", "p/").unwrap();
    let listing = list_folder(
        &mock.ops(),
        &folder,
        &ListOptions {
            parallelism: 3,
            cache: None,
        },
    )
    .await
    .unwrap();
    let keys = keys_of(&listing.entries);
    assert_eq!(keys.len(), 8);
    assert!(keys.contains("p/d0/file") && keys.contains("p/d7/file"));
    let requests = mock.requests();
    let delimited = requests
        .iter()
        .filter(|r| r.query_param("delimiter").as_deref() == Some("/"))
        .count();
    assert_eq!(delimited, 1);
    assert_eq!(requests.len(), 1 + 8);

    // One at a time lists flat: a single request.
    let before = mock.requests().len();
    let serial = list_folder(&mock.ops(), &folder, &ListOptions::default())
        .await
        .unwrap();
    assert!(!serial.entries.is_empty());
    assert_eq!(mock.requests().len(), before + 1);
}

#[tokio::test]
async fn the_cache_answers_a_repeat_listing_after_a_one_page_check_and_notices_changes() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "p/a.txt", "a").await;
    s3.put(&bucket, "p/b.txt", "b").await;
    let dir = tempfile::tempdir().unwrap();
    let cache = ListingCache::at(dir.path().join("sync-cache"));
    let folder = S3Uri::new(&bucket, "p/").unwrap();
    let options = ListOptions {
        parallelism: 4,
        cache: Some((cache.clone(), "account-1".to_owned())),
    };

    let first = list_folder(s3.ops(), &folder, &options).await.unwrap();
    assert_eq!(first.source, ListingSource::Fresh);
    assert_eq!(first.entries.len(), 2);
    assert_eq!(
        cache
            .load("account-1", &bucket, "p/")
            .unwrap()
            .entries
            .len(),
        2
    );

    let second = list_folder(s3.ops(), &folder, &options).await.unwrap();
    assert_eq!(second.source, ListingSource::Cache);
    assert_eq!(keys_of(&second.entries), keys_of(&first.entries));

    // A new object, a removed object and a changed object each make the snapshot invalid.
    s3.put(&bucket, "p/c.txt", "c").await;
    let after_add = list_folder(s3.ops(), &folder, &options).await.unwrap();
    assert_eq!(after_add.source, ListingSource::Fresh);
    assert_eq!(after_add.entries.len(), 3);
    assert_eq!(
        list_folder(s3.ops(), &folder, &options)
            .await
            .unwrap()
            .source,
        ListingSource::Cache
    );
    s3.put(&bucket, "p/a.txt", "longer content").await;
    let after_change = list_folder(s3.ops(), &folder, &options).await.unwrap();
    assert_eq!(after_change.source, ListingSource::Fresh);
    assert!(
        after_change
            .entries
            .iter()
            .any(|e| e.key == "p/a.txt" && e.size == 14)
    );
    s3.ops()
        .delete_object(&bucket, "p/b.txt", None)
        .await
        .unwrap();
    let after_delete = list_folder(s3.ops(), &folder, &options).await.unwrap();
    assert_eq!(after_delete.source, ListingSource::Fresh);
    assert_eq!(after_delete.entries.len(), 2);

    // Another account never sees this account's snapshot.
    let other = ListOptions {
        parallelism: 1,
        cache: Some((cache.clone(), "account-2".to_owned())),
    };
    assert_eq!(
        list_folder(s3.ops(), &folder, &other).await.unwrap().source,
        ListingSource::Fresh
    );
}
