//! Version lists grouped by key, restore, purge and the download of one
//! version. A simulated versioned bucket (`serve_versions`) answers
//! `ListObjectVersions` with S3's marker rules, so that every page boundary
//! can be put anywhere; scripted answers check the requests of the other
//! operations. Real versioning needs MinIO (s3s-fs has none): those tests are
//! ignored without the `minio` feature (docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bucketbridge_core::aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
use bucketbridge_core::engine::S3Runner;
use bucketbridge_core::model::TransferState;
use bucketbridge_core::settings::{DataIntegritySettings, TransferSettings};
use bucketbridge_core::transfer::{QueueOptions, TransferQueue};
use bucketbridge_core::versions::{
    DeleteMarkers, GROUP_SCAN_CAP, MAX_GROUP_ENTRIES, SCAN_REQUESTS, VersionFilter, VersionHead,
    VersionQuery, VersionRef, enqueue_download_version,
};
use bucketbridge_core::{ClientOptions, Error, ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};
use serde_json::json;

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const ACCOUNT: &str = "account-1";

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

// ---- a simulated versioned bucket ----

#[derive(Debug, Clone)]
struct Entry {
    key: &'static str,
    id: &'static str,
    latest: bool,
    marker: bool,
    day: u32,
}

fn version(key: &'static str, id: &'static str, latest: bool, day: u32) -> Entry {
    Entry {
        key,
        id,
        latest,
        marker: false,
        day,
    }
}

fn marker(key: &'static str, id: &'static str, latest: bool, day: u32) -> Entry {
    Entry {
        key,
        id,
        latest,
        marker: true,
        day,
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn entry_xml(entry: &Entry) -> String {
    let key = xml_escape(entry.key);
    if entry.marker {
        format!(
            "<DeleteMarker><Key>{key}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest><LastModified>2026-10-{:02}T12:00:00.000Z</LastModified></DeleteMarker>",
            entry.id, entry.latest, entry.day
        )
    } else {
        format!(
            "<Version><Key>{key}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest><LastModified>2026-10-{:02}T12:00:00.000Z</LastModified><ETag>\"etag-{}\"</ETag><Size>10</Size><StorageClass>STANDARD</StorageClass></Version>",
            entry.id, entry.latest, entry.day, entry.id
        )
    }
}

/// Answers `ListObjectVersions` over `entries` (in key order, newest first)
/// with S3's rules: `prefix`, `max-keys`, and the markers; the next markers
/// are the last entry returned. With `inclusive` a key marker without a
/// version marker starts at that key instead of after it, as a server that
/// does not follow S3 would.
fn serve_versions(
    entries: Vec<Entry>,
    inclusive: bool,
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
            .filter(|entry| entry.key.starts_with(prefix.as_str()))
            .collect();
        let start = match (
            request.query_param("key-marker"),
            request.query_param("version-id-marker"),
        ) {
            (Some(key), Some(id)) => matching
                .iter()
                .position(|entry| entry.key == key && entry.id == id)
                .map_or(matching.len(), |at| at + 1),
            (Some(key), None) if inclusive => matching
                .iter()
                .position(|entry| entry.key >= key.as_str())
                .unwrap_or(matching.len()),
            (Some(key), None) => matching
                .iter()
                .position(|entry| entry.key > key.as_str())
                .unwrap_or(matching.len()),
            _ => 0,
        };
        let end = (start + max).min(matching.len());
        let page = &matching[start..end];
        let truncation = match page.last() {
            Some(last) if end < matching.len() => format!(
                "<IsTruncated>true</IsTruncated><NextKeyMarker>{}</NextKeyMarker><NextVersionIdMarker>{}</NextVersionIdMarker>",
                xml_escape(last.key),
                last.id
            ),
            _ => "<IsTruncated>false</IsTruncated>".to_owned(),
        };
        let body: String = page.iter().map(|entry| entry_xml(entry)).collect();
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><Prefix>{}</Prefix><MaxKeys>{max}</MaxKeys>{truncation}{body}</ListVersionsResult>",
            xml_escape(&prefix)
        ))
    }
}

fn dataset() -> Vec<Entry> {
    vec![
        marker("a.txt", "a4", true, 28),
        version("a.txt", "a3", false, 20),
        version("a.txt", "a2", false, 10),
        version("a.txt", "a1", false, 5),
        version("b.txt", "b1", true, 12),
        version("c.txt", "c3", true, 25),
        version("c.txt", "c2", false, 15),
        marker("d.txt", "dm2", true, 27),
        marker("d.txt", "dm1", false, 26),
        version("e.txt", "e2", true, 22),
        version("e.txt", "e1", false, 3),
        version("f/g.txt", "g1", true, 8),
    ]
}

/// A filter written as the JSON the desktop contract uses (dates included).
fn filter(value: serde_json::Value) -> VersionFilter {
    serde_json::from_value(value).unwrap()
}

/// Every page of a listing, as `key: version ids` lines.
async fn list_all(
    ops: &S3,
    mut query: VersionQuery,
    pages: &mut Vec<usize>,
) -> Result<Vec<String>, Error> {
    let mut lines = Vec::new();
    for _ in 0..200 {
        let page = ops.list_version_groups("bucket", &query).await?;
        pages.push(page.groups.len());
        for group in &page.groups {
            let ids: Vec<&str> = group
                .versions
                .iter()
                .map(|version| version.version_id.as_str())
                .collect();
            lines.push(format!("{}: {}", group.key, ids.join(" ")));
        }
        match page.next_token {
            Some(token) => query.token = Some(token),
            None => return Ok(lines),
        }
    }
    panic!("the listing did not end");
}

#[tokio::test]
async fn groups_are_complete_whatever_the_page_boundaries() {
    let filters: Vec<(&str, VersionFilter, Vec<&str>)> = vec![
        (
            "everything",
            VersionFilter::default(),
            vec![
                "a.txt: a4 a3 a2 a1",
                "b.txt: b1",
                "c.txt: c3 c2",
                "d.txt: dm2 dm1",
                "e.txt: e2 e1",
                "f/g.txt: g1",
            ],
        ),
        (
            "multiple versions",
            VersionFilter {
                multiple_versions: true,
                ..VersionFilter::default()
            },
            vec![
                "a.txt: a4 a3 a2 a1",
                "c.txt: c3 c2",
                "d.txt: dm2 dm1",
                "e.txt: e2 e1",
            ],
        ),
        (
            "delete markers only",
            VersionFilter {
                delete_markers: DeleteMarkers::Only,
                ..VersionFilter::default()
            },
            vec!["a.txt: a4", "d.txt: dm2 dm1"],
        ),
        (
            "no delete markers",
            VersionFilter {
                delete_markers: DeleteMarkers::Exclude,
                ..VersionFilter::default()
            },
            vec![
                "a.txt: a3 a2 a1",
                "b.txt: b1",
                "c.txt: c3 c2",
                "e.txt: e2 e1",
                "f/g.txt: g1",
            ],
        ),
        (
            "versions off",
            VersionFilter {
                show_versions: false,
                ..VersionFilter::default()
            },
            vec!["a.txt: a4", "d.txt: dm2 dm1"],
        ),
        (
            "more than two",
            VersionFilter {
                count_greater_than: Some(2),
                ..VersionFilter::default()
            },
            vec!["a.txt: a4 a3 a2 a1"],
        ),
        (
            "more than two without markers",
            VersionFilter {
                count_greater_than: Some(2),
                delete_markers: DeleteMarkers::Exclude,
                ..VersionFilter::default()
            },
            vec!["a.txt: a3 a2 a1"],
        ),
        (
            "fewer than two",
            VersionFilter {
                count_less_than: Some(2),
                ..VersionFilter::default()
            },
            vec!["b.txt: b1", "f/g.txt: g1"],
        ),
        (
            "dates",
            filter(json!({
                "modified_after": "2026-10-10T12:00:00Z",
                "modified_before": "2026-10-26T12:00:00Z"
            })),
            vec!["a.txt: a3 a2", "b.txt: b1", "c.txt: c3 c2", "e.txt: e2"],
        ),
        (
            "dates and multiple versions",
            filter(json!({
                "modified_after": "2026-10-10T12:00:00Z",
                "modified_before": "2026-10-26T12:00:00Z",
                "multiple_versions": true
            })),
            vec!["a.txt: a3 a2", "c.txt: c3 c2"],
        ),
    ];
    for inclusive in [false, true] {
        let mock = MockS3::start(serve_versions(dataset(), inclusive));
        let ops = mock.ops();
        for (name, filter, expected) in &filters {
            for request_keys in [1, 2, 3, 5, 1000] {
                for max_groups in [1, 2, 100] {
                    let mut query = VersionQuery::new("");
                    query.filter = filter.clone();
                    query.request_keys = request_keys;
                    query.max_groups = max_groups;
                    let mut pages = Vec::new();
                    let lines = list_all(&ops, query, &mut pages).await.unwrap();
                    assert_eq!(
                        &lines, expected,
                        "{name}, {request_keys} entries per request, {max_groups} keys per page, inclusive {inclusive}"
                    );
                    assert!(
                        pages.iter().all(|count| *count <= max_groups as usize),
                        "{name}: {pages:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn requests_carry_the_prefix_markers_and_no_delimiter() {
    let mock = MockS3::start(serve_versions(dataset(), false));
    let ops = mock.ops();
    let mut query = VersionQuery::new("c");
    query.request_keys = 1;
    query.max_groups = 1;
    let first = ops.list_version_groups("bucket", &query).await.unwrap();
    assert_eq!(first.groups.len(), 1);
    assert_eq!(first.groups[0].key, "c.txt");
    assert_eq!(first.groups[0].total, 2);
    assert!(first.next_token.is_none(), "c.txt is the only key under c");
    let requests = mock.requests();
    // c3 on the first page, c2 on the second, then the end.
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].query_param("prefix").as_deref(), Some("c"));
    assert_eq!(requests[0].query_param("max-keys").as_deref(), Some("1"));
    assert_eq!(requests[0].query_param("delimiter"), None);
    assert_eq!(requests[0].query_param("key-marker"), None);
    assert_eq!(
        requests[1].query_param("key-marker").as_deref(),
        Some("c.txt")
    );
    assert_eq!(
        requests[1].query_param("version-id-marker").as_deref(),
        Some("c3")
    );

    // A second page starts after the last key that was complete: no version
    // marker.
    let mock = MockS3::start(serve_versions(dataset(), false));
    let ops = mock.ops();
    let mut query = VersionQuery::new("");
    query.max_groups = 2;
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    let keys: Vec<&str> = page.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["a.txt", "b.txt"]);
    let token = page.next_token.expect("more keys follow");
    query.token = Some(token);
    let next = ops.list_version_groups("bucket", &query).await.unwrap();
    let keys: Vec<&str> = next.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["c.txt", "d.txt"]);
    // The first request of the second page names the key before it.
    let resumed = mock
        .requests()
        .into_iter()
        .find(|request| request.query_param("key-marker").as_deref() == Some("b.txt"))
        .expect("a request that resumes after b.txt");
    assert_eq!(resumed.query_param("version-id-marker"), None);
}

#[tokio::test]
async fn an_exact_key_ends_at_the_first_other_key() {
    let mock = MockS3::start(serve_versions(
        vec![
            version("doc.txt", "d2", true, 2),
            version("doc.txt", "d1", false, 1),
            version("doc.txt.bak", "b1", true, 3),
            version("doc.txt.old", "o1", true, 4),
        ],
        false,
    ));
    let ops = mock.ops();
    let mut query = VersionQuery::new("doc.txt");
    query.exact = true;
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    assert_eq!(page.groups.len(), 1);
    assert_eq!(page.groups[0].key, "doc.txt");
    assert_eq!(page.groups[0].versions.len(), 2);
    assert_eq!(page.next_token, None);
    assert_eq!(mock.requests().len(), 1);

    // Without `exact`, the keys that merely start with it are listed too.
    query.exact = false;
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    assert_eq!(page.groups.len(), 3);

    // An exact key that does not exist lists nothing.
    let mut missing = VersionQuery::new("nothing.txt");
    missing.exact = true;
    let page = ops.list_version_groups("bucket", &missing).await.unwrap();
    assert!(page.groups.is_empty() && page.next_token.is_none());

    // The folder marker of a prefix is a key like any other here.
    let mock = MockS3::start(serve_versions(
        vec![
            version("dir/", "m1", true, 1),
            version("dir/a.txt", "a1", true, 2),
        ],
        false,
    ));
    let page = mock
        .ops()
        .list_version_groups("bucket", &VersionQuery::new("dir/"))
        .await
        .unwrap();
    let keys: Vec<&str> = page.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["dir/", "dir/a.txt"]);
}

#[tokio::test]
async fn groups_report_their_state_counts_and_entries() {
    let mock = MockS3::start(serve_versions(dataset(), false));
    let ops = mock.ops();
    let mut query = VersionQuery::new("a.txt");
    query.filter.delete_markers = DeleteMarkers::Exclude;
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    let group = &page.groups[0];
    assert_eq!(group.key, "a.txt");
    assert!(group.is_deleted, "the newest entry is a delete marker");
    assert_eq!((group.total, group.matching), (4, 3));
    assert!(!group.truncated);
    let first = &group.versions[0];
    assert_eq!(first.version_id, "a3");
    assert!(!first.is_latest && !first.is_delete_marker);
    assert_eq!(first.size, 10);
    assert_eq!(first.etag.as_deref(), Some("etag-a3"));
    assert_eq!(first.storage_class.as_deref(), Some("STANDARD"));
    assert_eq!(
        serde_json::to_value(first.last_modified).unwrap(),
        json!("2026-10-20T12:00:00Z")
    );
    assert_eq!(page.scanned, 4, "the four entries of a.txt, listed or not");
}

#[tokio::test]
async fn a_selective_filter_ends_a_call_with_a_token_after_a_bounded_scan() {
    // An endless bucket of keys with one version each: nothing passes
    // "multiple versions", and the call still ends.
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    let mock = MockS3::start(move |request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        let page = counter.fetch_add(1, Ordering::SeqCst);
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><NextKeyMarker>k{page:04}</NextKeyMarker><NextVersionIdMarker>v{page}</NextVersionIdMarker><Version><Key>k{page:04}</Key><VersionId>v{page}</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version></ListVersionsResult>"
        ))
    });
    let ops = mock.ops();
    let mut query = VersionQuery::new("");
    query.filter.multiple_versions = true;
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    assert!(page.groups.is_empty());
    assert_eq!(mock.requests().len(), SCAN_REQUESTS);
    assert_eq!(page.scanned, SCAN_REQUESTS as u64);
    // The token continues after the last key that was judged.
    let token = page.next_token.expect("a token to continue with");
    query.token = Some(token);
    let again = ops.list_version_groups("bucket", &query).await.unwrap();
    assert!(again.next_token.is_some());
    let requests = mock.requests();
    let resumed = &requests[SCAN_REQUESTS];
    assert_eq!(
        resumed.query_param("key-marker").as_deref(),
        Some(format!("k{:04}", SCAN_REQUESTS - 2).as_str())
    );
    assert_eq!(resumed.query_param("version-id-marker"), None);
}

#[tokio::test]
async fn a_huge_key_is_counted_up_to_a_cap_and_skipped_on_the_server() {
    let requests_seen = Arc::new(Mutex::new(Vec::<(Option<String>, Option<String>)>::new()));
    let log = Arc::clone(&requests_seen);
    let mock = MockS3::start(move |request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        let key_marker = request.query_param("key-marker");
        let id_marker = request.query_param("version-id-marker");
        log.lock()
            .unwrap()
            .push((key_marker.clone(), id_marker.clone()));
        match (key_marker.as_deref(), id_marker) {
            // After the huge key: the rest of the bucket.
            (Some("huge"), None) => MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated><Version><Key>zeta</Key><VersionId>z1</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version></ListVersionsResult>"
            )),
            (marker, id) => {
                let first: usize = match (marker, id) {
                    (None, _) => 0,
                    (Some("huge"), Some(id)) => {
                        id.trim_start_matches('v').parse::<usize>().unwrap() + 1
                    }
                    other => panic!("unexpected markers {other:?}"),
                };
                let versions: String = (first..first + 1000)
                    .map(|index| {
                        format!(
                            "<Version><Key>huge</Key><VersionId>v{index}</VersionId><IsLatest>{}</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version>",
                            index == 0
                        )
                    })
                    .collect();
                MockResponse::xml(format!(
                    "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><NextKeyMarker>huge</NextKeyMarker><NextVersionIdMarker>v{}</NextVersionIdMarker>{versions}</ListVersionsResult>",
                    first + 999
                ))
            }
        }
    });
    let ops = mock.ops();
    let mut query = VersionQuery::new("");
    let page = ops.list_version_groups("bucket", &query).await.unwrap();
    // The key took more requests than one call may send, so the call ends at
    // its end, with a token.
    let keys: Vec<&str> = page.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["huge"]);
    let huge = &page.groups[0];
    assert!(huge.truncated);
    assert_eq!(huge.total, GROUP_SCAN_CAP);
    assert_eq!(huge.matching, GROUP_SCAN_CAP);
    assert_eq!(huge.versions.len(), MAX_GROUP_ENTRIES);
    assert_eq!(huge.versions[0].version_id, "v0");
    assert!(huge.versions[0].is_latest);
    query.token = Some(page.next_token.expect("the bucket goes on"));
    let rest = ops.list_version_groups("bucket", &query).await.unwrap();
    let keys: Vec<&str> = rest.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, ["zeta"]);
    assert!(!rest.groups[0].truncated);
    assert_eq!(rest.next_token, None);
    // GROUP_SCAN_CAP entries took 50 requests; the next one skipped the rest
    // of the key with a key marker alone, and so did the one that resumed.
    let seen = requests_seen.lock().unwrap().clone();
    assert_eq!(seen.len() as u64, GROUP_SCAN_CAP / 1000 + 2);
    let skip = (Some("huge".to_owned()), None);
    assert_eq!(&seen[seen.len() - 2..], [skip.clone(), skip]);
}

#[tokio::test]
async fn a_server_that_repeats_a_marker_is_stopped() {
    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><NextKeyMarker>k</NextKeyMarker><NextVersionIdMarker>v1</NextVersionIdMarker><Version><Key>k</Key><VersionId>v1</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version></ListVersionsResult>"
        ))
    });
    let err = mock
        .ops()
        .list_version_groups("bucket", &VersionQuery::new(""))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
    assert!(
        err.to_string().contains("same continuation marker"),
        "{err}"
    );
    assert_eq!(mock.requests().len(), 2);

    let mock = MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return unexpected(request);
        }
        MockResponse::xml(format!(
            "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>true</IsTruncated><Version><Key>k</Key><VersionId>v1</VersionId><IsLatest>true</IsLatest><LastModified>2026-10-01T00:00:00.000Z</LastModified><ETag>\"e\"</ETag><Size>1</Size><StorageClass>STANDARD</StorageClass></Version></ListVersionsResult>"
        ))
    });
    let err = mock
        .ops()
        .list_version_groups("bucket", &VersionQuery::new(""))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
    assert!(err.to_string().contains("no marker"), "{err}");
}

#[tokio::test]
async fn invalid_queries_never_reach_the_server() {
    let mock = MockS3::start(serve_versions(dataset(), false));
    let ops = mock.ops();
    let mut garbage = VersionQuery::new("");
    garbage.token = Some("garbage".into());
    let mut bad_filter = VersionQuery::new("");
    bad_filter.filter.count_greater_than = Some(5);
    bad_filter.filter.count_less_than = Some(3);
    let mut exact_folder = VersionQuery::new("");
    exact_folder.exact = true;
    for query in [garbage, bad_filter, exact_folder] {
        let err = ops.list_version_groups("bucket", &query).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
    }
    let err = ops
        .list_version_groups("bad bucket", &VersionQuery::new(""))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn failures_of_the_server_keep_their_kind_and_name_the_prefix() {
    let mock = MockS3::start(|_| MockResponse::error(403, "AccessDenied", "Access Denied"));
    let err = mock
        .ops()
        .list_version_groups("bucket", &VersionQuery::new("photos/"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
    assert!(err.to_string().contains("s3://bucket/photos/"), "{err}");
    let mock = MockS3::start(|_| MockResponse::error(404, "NoSuchBucket", "no bucket"));
    let err = mock
        .ops()
        .list_version_groups("bucket", &VersionQuery::new(""))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

// ---- head, restore, purge ----

fn head_response(version_id: Option<&str>, size: u64) -> MockResponse {
    let mut response = MockResponse::empty(200)
        .with_header("content-length", &size.to_string())
        .with_header("etag", "\"etag-v1\"")
        .with_header("last-modified", "Wed, 21 Oct 2026 07:28:00 GMT")
        .with_header("content-type", "text/plain");
    if let Some(version_id) = version_id {
        response = response.with_header("x-amz-version-id", version_id);
    }
    response
}

#[tokio::test]
async fn head_version_tells_a_delete_marker_from_a_version() {
    let mock = MockS3::start(
        |request| match request.query_param("versionId").as_deref() {
            Some("v1") => head_response(Some("v1"), 5),
            Some("dm") => MockResponse::empty(405)
                .with_header("x-amz-delete-marker", "true")
                .with_header("x-amz-version-id", "dm"),
            Some("gone") => MockResponse::empty(404),
            _ => unexpected(request),
        },
    );
    let ops = mock.ops();
    match ops.head_version("bucket", "doc.txt", "v1").await.unwrap() {
        VersionHead::Version(head) => {
            assert_eq!(head.size, 5);
            assert_eq!(head.version_id.as_deref(), Some("v1"));
        }
        VersionHead::DeleteMarker => panic!("a version"),
    }
    assert_eq!(
        ops.head_version("bucket", "doc.txt", "dm").await.unwrap(),
        VersionHead::DeleteMarker
    );
    let err = ops
        .head_version("bucket", "doc.txt", "gone")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = ops.head_version("bucket", "doc.txt", "").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

/// A bucket that holds `dir/a b+c.txt` with versions v1 (an old one, with
/// `meta`) and v3 (the current one).
fn restore_mock(old_headers: &'static [(&'static str, &'static str)]) -> MockS3 {
    MockS3::start(move |request| {
        let key_path = "/bucket/dir/a%20b%2Bc.txt";
        match (request.method.as_str(), request.query_param("versionId")) {
            ("HEAD", Some(id)) if request.path == key_path && id == "v1" => {
                let mut response = head_response(Some("v1"), 5);
                for (name, value) in old_headers {
                    response = response.with_header(name, value);
                }
                response
            }
            ("HEAD", Some(id)) if request.path == key_path && id == "dm" => {
                MockResponse::empty(405)
            }
            ("HEAD", None) if request.path == key_path => head_response(Some("v3"), 7),
            ("PUT", None)
                if request.path == key_path && request.header("x-amz-copy-source").is_some() =>
            {
                MockResponse::xml(format!(
                    "<CopyObjectResult {NS}><ETag>\"etag-v1\"</ETag><LastModified>2026-10-29T08:15:00.000Z</LastModified></CopyObjectResult>"
                ))
                .with_header("x-amz-version-id", "v4")
            }
            _ => unexpected(request),
        }
    })
}

#[tokio::test]
async fn restore_copies_the_version_over_the_key_and_preserves_what_it_can() {
    let mock = restore_mock(&[
        ("x-amz-storage-class", "STANDARD_IA"),
        ("x-amz-server-side-encryption", "aws:kms"),
        (
            "x-amz-server-side-encryption-aws-kms-key-id",
            "arn:aws:kms:eu-west-1:000000000000:key/example",
        ),
        ("x-amz-server-side-encryption-bucket-key-enabled", "true"),
    ]);
    let outcome = mock
        .ops()
        .restore_version("bucket", "dir/a b+c.txt", "v1")
        .await
        .unwrap();
    assert_eq!(outcome.key, "dir/a b+c.txt");
    assert_eq!(outcome.restored_version_id, "v1");
    assert_eq!(outcome.version_id.as_deref(), Some("v4"));
    assert_eq!(outcome.etag.as_deref(), Some("etag-v1"));
    assert_eq!(outcome.size, 5);

    let requests = mock.requests();
    let put = requests.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(
        put.header("x-amz-copy-source"),
        Some("bucket/dir/a%20b%2Bc.txt?versionId=v1")
    );
    assert_eq!(put.header("x-amz-metadata-directive"), Some("COPY"));
    assert_eq!(put.header("x-amz-tagging-directive"), Some("COPY"));
    assert_eq!(put.header("x-amz-storage-class"), Some("STANDARD_IA"));
    assert_eq!(put.header("x-amz-server-side-encryption"), Some("aws:kms"));
    assert_eq!(
        put.header("x-amz-server-side-encryption-aws-kms-key-id"),
        Some("arn:aws:kms:eu-west-1:000000000000:key/example")
    );
    assert_eq!(
        put.header("x-amz-server-side-encryption-bucket-key-enabled"),
        Some("true")
    );
    // The version, then the current state, then the copy.
    let order: Vec<(&str, Option<String>)> = requests
        .iter()
        .map(|r| (r.method.as_str(), r.query_param("versionId")))
        .collect();
    assert_eq!(
        order,
        [
            ("HEAD", Some("v1".to_owned())),
            ("HEAD", None),
            ("PUT", None)
        ]
    );
}

#[tokio::test]
async fn restore_of_a_plain_version_sends_no_storage_class_or_encryption() {
    let mock = restore_mock(&[("x-amz-server-side-encryption", "AES256")]);
    mock.ops()
        .restore_version("bucket", "dir/a b+c.txt", "v1")
        .await
        .unwrap();
    let put = mock
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(put.header("x-amz-storage-class"), None);
    assert_eq!(put.header("x-amz-server-side-encryption"), Some("AES256"));
    assert_eq!(
        put.header("x-amz-server-side-encryption-aws-kms-key-id"),
        None
    );
}

#[tokio::test]
async fn restore_refuses_what_it_cannot_do_without_sending_a_copy() {
    let mock = restore_mock(&[]);
    let ops = mock.ops();
    // A delete marker has no content.
    let err = ops
        .restore_version("bucket", "dir/a b+c.txt", "dm")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(err.to_string().contains("delete marker"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method == "HEAD"));

    // The current version is not restored over itself.
    let current = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => head_response(Some("v1"), 5),
        _ => unexpected(request),
    });
    let err = current
        .ops()
        .restore_version("bucket", "k", "v1")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
    assert!(err.to_string().contains("already is the current"), "{err}");
    assert!(current.requests().iter().all(|r| r.method == "HEAD"));

    // Without versioning the one version is called "null" and HEAD sends no
    // version header.
    let unversioned = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => head_response(None, 5),
        _ => unexpected(request),
    });
    let err = unversioned
        .ops()
        .restore_version("bucket", "k", "null")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");

    // Customer-key encryption cannot be copied without the key.
    let sse_c = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => head_response(Some("v1"), 5)
            .with_header("x-amz-server-side-encryption-customer-algorithm", "AES256")
            .with_header(
                "x-amz-server-side-encryption-customer-key-md5",
                "ZXhhbXBsZQ==",
            ),
        _ => unexpected(request),
    });
    let err = sse_c
        .ops()
        .restore_version("bucket", "k", "v1")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(err.to_string().contains("SSE-C"), "{err}");
    assert_eq!(sse_c.requests().len(), 1);

    // One copy request moves at most 5 GiB.
    let huge = MockS3::start(|request| match request.method.as_str() {
        "HEAD" => head_response(Some("v1"), 6 * 1024 * 1024 * 1024),
        _ => unexpected(request),
    });
    let err = huge
        .ops()
        .restore_version("bucket", "k", "v1")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(err.to_string().contains("cannot be restored"), "{err}");

    // Bad input never reaches the server.
    let none = MockS3::start(unexpected);
    for (bucket, key, id) in [
        ("bad bucket", "k", "v1"),
        ("bucket", "", "v1"),
        ("bucket", "k", ""),
    ] {
        let err = none
            .ops()
            .restore_version(bucket, key, id)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{bucket} {key} {id}");
    }
    assert!(none.requests().is_empty());
}

#[tokio::test]
async fn restore_brings_back_a_deleted_key_and_reports_server_errors() {
    // The latest entry is a delete marker: HEAD of the key is a 404.
    let mock = MockS3::start(|request| {
        match (request.method.as_str(), request.query_param("versionId")) {
            ("HEAD", Some(_)) => head_response(Some("v1"), 5),
            ("HEAD", None) => MockResponse::empty(404).with_header("x-amz-delete-marker", "true"),
            ("PUT", None) => MockResponse::xml(format!(
                "<CopyObjectResult {NS}><ETag>\"e\"</ETag></CopyObjectResult>"
            )),
            _ => unexpected(request),
        }
    });
    let outcome = mock
        .ops()
        .restore_version("bucket", "k", "v1")
        .await
        .unwrap();
    assert_eq!(
        outcome.version_id, None,
        "an unversioned answer has no version"
    );

    // An archived object is a conflict with a hint, access denied is auth.
    for (status, code, kind) in [
        (403, "InvalidObjectState", ErrorKind::Conflict),
        (403, "AccessDenied", ErrorKind::Auth),
        (404, "NoSuchVersion", ErrorKind::NotFound),
    ] {
        let mock = MockS3::start(move |request| {
            match (request.method.as_str(), request.query_param("versionId")) {
                ("HEAD", Some(_)) => head_response(Some("v1"), 5),
                ("HEAD", None) => MockResponse::empty(404),
                ("PUT", _) => MockResponse::error(status, code, "no"),
                _ => unexpected(request),
            }
        });
        let err = mock
            .ops()
            .restore_version("bucket", "k", "v1")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), kind, "{code}: {err}");
        assert!(
            err.to_string()
                .contains("restoring version v1 of s3://bucket/k"),
            "{err}"
        );
    }
}

fn refs(items: &[(&str, &str)]) -> Vec<VersionRef> {
    items
        .iter()
        .map(|(key, id)| VersionRef {
            key: (*key).to_owned(),
            version_id: (*id).to_owned(),
        })
        .collect()
}

#[tokio::test]
async fn purge_deletes_the_named_versions_in_one_request() {
    let mock = MockS3::start(|request| {
        if request.method == "POST" && request.query_param("delete").is_some() {
            MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>"))
        } else {
            unexpected(request)
        }
    });
    let outcome = mock
        .ops()
        .purge_versions(
            "bucket",
            &refs(&[
                ("a.txt", "v1"),
                ("dir/b & c.txt", "v 2"),
                ("a.txt", "v1"),
                ("a.txt", "dm"),
            ]),
        )
        .await
        .unwrap();
    assert_eq!((outcome.deleted, outcome.failures.len()), (3, 0));
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "one request for three versions");
    let body = requests[0].body_text();
    assert!(body.contains("<Quiet>true</Quiet>"), "{body}");
    assert!(
        body.contains("<Key>a.txt</Key><VersionId>v1</VersionId>"),
        "{body}"
    );
    assert!(
        body.contains("<Key>dir/b &amp; c.txt</Key><VersionId>v 2</VersionId>"),
        "{body}"
    );
    assert!(
        body.contains("<Key>a.txt</Key><VersionId>dm</VersionId>"),
        "{body}"
    );
    assert_eq!(
        body.matches("<Object>").count(),
        3,
        "the duplicate is sent once"
    );
    assert!(
        requests[0].header("content-md5").is_some(),
        "DeleteObjects needs an integrity header"
    );
}

#[tokio::test]
async fn purge_batches_by_a_thousand_and_reports_each_failure() {
    let mock = MockS3::start(|request| {
        if request.method != "POST" || request.query_param("delete").is_none() {
            return unexpected(request);
        }
        // The version v7 of k7 is refused, as is the one of "locked".
        let body = request.body_text();
        let mut errors = String::new();
        for (key, id) in [("k7", "v7"), ("locked", "v1")] {
            if body.contains(&format!("<Key>{key}</Key><VersionId>{id}</VersionId>")) {
                errors.push_str(&format!(
                    "<Error><Key>{key}</Key><VersionId>{id}</VersionId><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"
                ));
            }
        }
        MockResponse::xml(format!("<DeleteResult {NS}>{errors}</DeleteResult>"))
    });
    let mut versions: Vec<VersionRef> = (0..2500)
        .map(|index| VersionRef {
            key: format!("k{index}"),
            version_id: format!("v{index}"),
        })
        .collect();
    versions.push(VersionRef {
        key: "locked".into(),
        version_id: "v1".into(),
    });
    let outcome = mock
        .ops()
        .purge_versions("bucket", &versions)
        .await
        .unwrap();
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "2501 versions are three batches");
    assert_eq!(outcome.deleted, 2501 - 2);
    let failed: Vec<(&str, Option<&str>, Option<&str>)> = outcome
        .failures
        .iter()
        .map(|f| (f.key.as_str(), f.version_id.as_deref(), f.code.as_deref()))
        .collect();
    assert_eq!(
        failed,
        [
            ("k7", Some("v7"), Some("AccessDenied")),
            ("locked", Some("v1"), Some("AccessDenied"))
        ]
    );

    // Nothing to purge sends nothing; bad input is refused first.
    let none = MockS3::start(unexpected);
    let outcome = none.ops().purge_versions("bucket", &[]).await.unwrap();
    assert_eq!((outcome.deleted, outcome.failures.len()), (0, 0));
    for bad in [refs(&[("", "v1")]), refs(&[("k", "")])] {
        let err = none.ops().purge_versions("bucket", &bad).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
    assert!(none.requests().is_empty());

    // A request that fails as a whole is an error.
    let broken = MockS3::start(|_| MockResponse::error(403, "AccessDenied", "Access Denied"));
    let err = broken
        .ops()
        .purge_versions("bucket", &refs(&[("k", "v1")]))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
}

#[tokio::test]
async fn requester_pays_reaches_every_version_request() {
    let mock = MockS3::start(|request| {
        match (request.method.as_str(), request.query_param("versionId")) {
            ("GET", _) if request.query_param("versions").is_some() => MockResponse::xml(format!(
                "<ListVersionsResult {NS}><Name>bucket</Name><IsTruncated>false</IsTruncated></ListVersionsResult>"
            )),
            ("HEAD", Some(_)) => head_response(Some("v1"), 5),
            ("HEAD", None) => MockResponse::empty(404),
            ("PUT", _) => MockResponse::xml(format!(
                "<CopyObjectResult {NS}><ETag>\"e\"</ETag></CopyObjectResult>"
            )),
            ("POST", _) => MockResponse::xml(format!("<DeleteResult {NS}></DeleteResult>")),
            _ => unexpected(request),
        }
    });
    let options = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    let ops = mock.ops_with(&options, true);
    ops.list_version_groups("bucket", &VersionQuery::new(""))
        .await
        .unwrap();
    ops.restore_version("bucket", "k", "v1").await.unwrap();
    ops.purge_versions("bucket", &refs(&[("k", "v1")]))
        .await
        .unwrap();
    let requests = mock.requests();
    assert_eq!(requests.len(), 5);
    for request in &requests {
        assert_eq!(
            request.header("x-amz-request-payer"),
            Some("requester"),
            "{} {}?{}",
            request.method,
            request.path,
            request.query
        );
    }
}

// ---- the download of one version through the queue ----

fn provider(s3: S3) -> bucketbridge_core::engine::S3Provider {
    Arc::new(move |_account: String| {
        let s3 = s3.clone();
        Box::pin(async move { Ok(s3) })
    })
}

async fn open_queue(s3: S3) -> TransferQueue {
    let runner = S3Runner::new(provider(s3), DataIntegritySettings::default());
    TransferQueue::open(
        TransferSettings {
            retry_attempts: 0,
            ..TransferSettings::default()
        },
        runner.runners(),
        QueueOptions::default(),
    )
    .await
    .unwrap()
}

async fn wait_state(queue: &TransferQueue, id: &str, state: TransferState) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let task = queue.get(id).unwrap();
        if task.state == state {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "task {id} is {:?} ({:?}), not {state:?}",
            task.state,
            task.error
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A bucket with an old and a current version of `doc.txt` and a delete
/// marker; `failures` GETs fail first.
fn download_mock(failures: usize) -> MockS3 {
    let failed = Arc::new(AtomicUsize::new(0));
    MockS3::start(move |request| {
        let version = request.query_param("versionId");
        match (request.method.as_str(), version.as_deref()) {
            ("HEAD", Some("old")) => head_response(Some("old"), 11),
            ("HEAD", Some("dm")) => MockResponse::empty(405),
            ("HEAD", Some("gone")) => MockResponse::empty(404),
            ("GET", Some("old")) => {
                if failed.fetch_add(1, Ordering::SeqCst) < failures {
                    return MockResponse::error(500, "InternalError", "try again");
                }
                MockResponse {
                    status: 200,
                    headers: vec![
                        ("etag".into(), "\"etag-old\"".into()),
                        (
                            "last-modified".into(),
                            "Wed, 21 Oct 2026 07:28:00 GMT".into(),
                        ),
                        ("x-amz-version-id".into(), "old".into()),
                    ],
                    body: "old content".into(),
                }
            }
            // The current version has other content: asking for it would
            // fail the test.
            ("GET", None) => MockResponse {
                status: 200,
                headers: Vec::new(),
                body: "NEW CONTENT".into(),
            },
            _ => unexpected(request),
        }
    })
}

#[tokio::test]
async fn one_version_downloads_through_the_queue() {
    let mock = download_mock(0);
    let queue = open_queue(mock.ops()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("saved").join("doc (old).txt");
    let task = try_enqueue(&queue, &mock.ops(), "doc.txt", "old", &dest)
        .await
        .unwrap();
    assert_eq!(task.bytes_total, Some(11));
    assert_eq!(task.state, TransferState::Queued);
    assert_eq!(task.attempts, 0);
    wait_state(&queue, &task.id, TransferState::Completed).await;
    assert_eq!(fs::read_to_string(&dest).unwrap(), "old content");
    let gets: Vec<RecordedRequest> = mock
        .requests()
        .into_iter()
        .filter(|r| r.method == "GET")
        .collect();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].query_param("versionId").as_deref(), Some("old"));
    assert_eq!(queue.resume_state(&task.id).unwrap(), None);
}

#[tokio::test]
async fn a_failed_version_download_keeps_its_version_for_the_retry() {
    let mock = download_mock(1);
    let queue = open_queue(mock.ops()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("doc.txt");
    let task = try_enqueue(&queue, &mock.ops(), "doc.txt", "old", &dest)
        .await
        .unwrap();
    wait_state(&queue, &task.id, TransferState::Failed).await;
    assert!(!dest.exists());
    // The state that outlives the failure still names the version.
    let state = queue.resume_state(&task.id).unwrap().unwrap();
    assert_eq!(state["type"], "version_download");
    assert_eq!(state["version_id"], "old");
    queue.retry_task(&task.id).unwrap();
    wait_state(&queue, &task.id, TransferState::Completed).await;
    assert_eq!(fs::read_to_string(&dest).unwrap(), "old content");
    let versions: Vec<Option<String>> = mock
        .requests()
        .iter()
        .filter(|r| r.method == "GET")
        .map(|r| r.query_param("versionId"))
        .collect();
    assert_eq!(versions, [Some("old".to_owned()), Some("old".to_owned())]);
}

/// Queues the download of `key` at `id` to `dest`.
async fn try_enqueue(
    queue: &TransferQueue,
    s3: &S3,
    key: &str,
    id: &str,
    dest: &Path,
) -> Result<bucketbridge_core::model::TransferTask, Error> {
    enqueue_download_version(queue, s3, ACCOUNT, "bucket", key, id, dest).await
}

#[tokio::test]
async fn a_version_download_checks_what_it_is_asked_to_download() {
    let mock = download_mock(0);
    let ops = mock.ops();
    let queue = open_queue(ops.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("x.txt");
    let partial = dir.path().join("doc.txt.BBPART");
    let cases = [
        (
            "a delete marker",
            ("doc.txt", "dm", dest.as_path()),
            ErrorKind::Usage,
        ),
        (
            "a missing version",
            ("doc.txt", "gone", dest.as_path()),
            ErrorKind::NotFound,
        ),
        (
            "a folder",
            ("dir/", "old", dest.as_path()),
            ErrorKind::Usage,
        ),
        (
            "an empty version",
            ("doc.txt", "", dest.as_path()),
            ErrorKind::Usage,
        ),
        (
            "a relative path",
            ("doc.txt", "old", Path::new("relative.txt")),
            ErrorKind::Usage,
        ),
        (
            "a partial name",
            ("doc.txt", "old", partial.as_path()),
            ErrorKind::Usage,
        ),
        (
            "no file name",
            ("doc.txt", "old", Path::new("/")),
            ErrorKind::Usage,
        ),
    ];
    for (what, (key, id, path), kind) in cases {
        let err = try_enqueue(&queue, &ops, key, id, path).await.unwrap_err();
        assert_eq!(err.kind(), kind, "{what}: {err}");
    }
    assert!(queue.list().is_empty(), "nothing was queued");
}

// ---- real versioning (MinIO) ----

async fn enable_versioning(s3: &TestS3, bucket: &str) {
    s3.client()
        .put_bucket_versioning()
        .bucket(bucket)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn minio_lists_restores_and_purges_versions() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    enable_versioning(&s3, &bucket).await;
    for body in ["one", "two", "three"] {
        s3.client()
            .put_object()
            .bucket(&bucket)
            .key("doc.txt")
            .content_type("text/x-example")
            .metadata("owner", "team")
            .body(body.as_bytes().to_vec().into())
            .send()
            .await
            .unwrap();
    }
    s3.put(&bucket, "other.txt", "x").await;
    s3.ops()
        .delete_object(&bucket, "doc.txt", None)
        .await
        .unwrap();

    let ops = s3.ops();
    let page = ops
        .list_version_groups(&bucket, &VersionQuery::new(""))
        .await
        .unwrap();
    let doc = page.groups.iter().find(|g| g.key == "doc.txt").unwrap();
    assert!(doc.is_deleted);
    assert_eq!(doc.total, 4);
    assert!(doc.versions[0].is_delete_marker && doc.versions[0].is_latest);
    let other = page.groups.iter().find(|g| g.key == "other.txt").unwrap();
    assert!(!other.is_deleted);

    // Filters against a real server.
    let mut only_markers = VersionQuery::new("");
    only_markers.filter.delete_markers = DeleteMarkers::Only;
    let page = ops
        .list_version_groups(&bucket, &only_markers)
        .await
        .unwrap();
    assert_eq!(page.groups.len(), 1);
    assert_eq!(page.groups[0].key, "doc.txt");
    let mut multiple = VersionQuery::new("");
    multiple.filter.multiple_versions = true;
    multiple.request_keys = 2;
    multiple.max_groups = 1;
    let page = ops.list_version_groups(&bucket, &multiple).await.unwrap();
    assert_eq!(page.groups[0].key, "doc.txt");

    // Restore the oldest version over the deleted key: metadata comes along.
    let oldest = doc.versions.last().unwrap().version_id.clone();
    let outcome = ops
        .restore_version(&bucket, "doc.txt", &oldest)
        .await
        .unwrap();
    assert!(outcome.version_id.is_some());
    assert_eq!(s3.get(&bucket, "doc.txt").await, b"one");
    let head = ops.head_object(&bucket, "doc.txt", None).await.unwrap();
    assert_eq!(head.meta.content_type.as_deref(), Some("text/x-example"));
    assert_eq!(
        head.meta.user_meta.get("owner").map(String::as_str),
        Some("team")
    );
    // Restoring it again is refused: it is the current version's content,
    // but a different version.
    let page = ops
        .list_version_groups(
            &bucket,
            &VersionQuery {
                exact: true,
                ..VersionQuery::new("doc.txt")
            },
        )
        .await
        .unwrap();
    let group = &page.groups[0];
    assert!(!group.is_deleted);
    assert_eq!(group.total, 5);
    let current = group.versions[0].version_id.clone();
    let err = ops
        .restore_version(&bucket, "doc.txt", &current)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);

    // Purge every version of doc.txt: the key is gone for good.
    let all: Vec<VersionRef> = group
        .versions
        .iter()
        .map(|v| VersionRef {
            key: v.key.clone(),
            version_id: v.version_id.clone(),
        })
        .collect();
    let outcome = ops.purge_versions(&bucket, &all).await.unwrap();
    assert_eq!((outcome.deleted, outcome.failures.len()), (5, 0));
    let page = ops
        .list_version_groups(&bucket, &VersionQuery::new("doc"))
        .await
        .unwrap();
    assert!(page.groups.is_empty());
    assert!(!s3.exists(&bucket, "doc.txt").await);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn minio_downloads_an_old_version_and_refuses_a_delete_marker() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    enable_versioning(&s3, &bucket).await;
    s3.put(&bucket, "doc.txt", "first version").await;
    s3.put(&bucket, "doc.txt", "second version").await;
    s3.ops()
        .delete_object(&bucket, "doc.txt", None)
        .await
        .unwrap();
    let page = s3
        .ops()
        .list_version_groups(&bucket, &VersionQuery::new("doc.txt"))
        .await
        .unwrap();
    let versions = &page.groups[0].versions;
    assert_eq!(versions.len(), 3);
    let marker = &versions[0];
    let newest = &versions[1];
    let oldest = &versions[2];
    assert!(marker.is_delete_marker);

    let queue = open_queue(s3.ops().clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.txt");
    let new = dir.path().join("new.txt");
    let tasks = [
        enqueue_download_version(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            "doc.txt",
            &oldest.version_id,
            &old,
        )
        .await
        .unwrap(),
        enqueue_download_version(
            &queue,
            s3.ops(),
            ACCOUNT,
            &bucket,
            "doc.txt",
            &newest.version_id,
            &new,
        )
        .await
        .unwrap(),
    ];
    for task in &tasks {
        wait_state(&queue, &task.id, TransferState::Completed).await;
    }
    assert_eq!(fs::read_to_string(&old).unwrap(), "first version");
    assert_eq!(fs::read_to_string(&new).unwrap(), "second version");
    let err = enqueue_download_version(
        &queue,
        s3.ops(),
        ACCOUNT,
        &bucket,
        "doc.txt",
        &marker.version_id,
        &dir.path().join("marker.txt"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
}
