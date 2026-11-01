//! Object previews: one ranged `GetObject` per page, content sniffing and
//! paging. Scripted answers (`serve_object`) check the requests; the
//! in-process server checks real ranged reads and real binary bytes. A
//! preview of an old version of an object needs MinIO.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
use bucketbridge_core::preview::{DEFAULT_PAGE_BYTES, ObjectPreview, PreviewKind, PreviewOptions};
use bucketbridge_core::{ClientOptions, ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest, TestS3};

const LAST_MODIFIED: &str = "Wed, 21 Oct 2026 07:28:00 GMT";

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

/// The `first-last` of a `Range: bytes=first-last` header.
fn requested_range(request: &RecordedRequest) -> Option<(usize, usize)> {
    let (first, last) = request
        .header("range")?
        .strip_prefix("bytes=")?
        .split_once('-')?;
    Some((first.parse().ok()?, last.parse().ok()?))
}

/// One ASCII object, served like S3: ranged `GET`s with `206` and
/// `Content-Range`, `416` for a range past the end (or any range of an empty
/// object), and `HEAD`.
fn serve_object(
    body: String,
    content_type: &'static str,
    version: Option<&'static str>,
) -> impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static {
    move |request| {
        let total = body.len();
        let mut headers = vec![
            ("content-type".to_owned(), content_type.to_owned()),
            ("etag".to_owned(), "\"etag-1\"".to_owned()),
            ("last-modified".to_owned(), LAST_MODIFIED.to_owned()),
        ];
        if let Some(version) = version {
            headers.push(("x-amz-version-id".to_owned(), version.to_owned()));
        }
        match request.method.as_str() {
            "HEAD" => {
                headers.push(("content-length".to_owned(), total.to_string()));
                MockResponse {
                    status: 200,
                    headers,
                    body: String::new(),
                }
            }
            "GET" => {
                let Some((first, last)) = requested_range(request) else {
                    return unexpected(request);
                };
                if total == 0 || first >= total {
                    return MockResponse::error(
                        416,
                        "InvalidRange",
                        "The requested range is not satisfiable",
                    );
                }
                let end = last.min(total - 1);
                headers.push((
                    "content-range".to_owned(),
                    format!("bytes {first}-{end}/{total}"),
                ));
                MockResponse {
                    status: 206,
                    headers,
                    body: body[first..=end].to_owned(),
                }
            }
            _ => unexpected(request),
        }
    }
}

fn options(page_bytes: u64) -> PreviewOptions {
    PreviewOptions {
        page_bytes,
        ..PreviewOptions::default()
    }
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

async fn preview(
    ops: &S3,
    key: &str,
    version: Option<&str>,
    page: u64,
    options: &PreviewOptions,
) -> Result<ObjectPreview, bucketbridge_core::Error> {
    ops.preview_object("bucket", key, version, page, options)
        .await
}

// ---- scripted answers: the requests ----

#[tokio::test]
async fn the_first_page_of_text_is_one_ranged_request() {
    let body = "line of text\n".repeat(40);
    let mock = MockS3::start(serve_object(body.clone(), "text/plain", Some("v7")));
    let preview = preview(
        &mock.ops(),
        "notes/a.txt",
        None,
        0,
        &PreviewOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(preview.kind, PreviewKind::Text);
    assert_eq!(preview.key, "notes/a.txt");
    assert_eq!(preview.text.as_deref(), Some(body.as_str()));
    assert_eq!(
        (
            preview.size,
            preview.page,
            preview.page_count,
            preview.page_size
        ),
        (body.len() as u64, 0, 1, DEFAULT_PAGE_BYTES)
    );
    assert_eq!((preview.offset, preview.length), (0, body.len() as u64));
    assert!(!preview.formatted && preview.image_base64.is_none());
    assert_eq!(preview.version_id.as_deref(), Some("v7"));
    assert_eq!(preview.content_type.as_deref(), Some("text/plain"));
    assert_eq!(preview.media_type.as_deref(), Some("text/plain"));
    assert_eq!(preview.etag.as_deref(), Some("etag-1"));
    assert_eq!(
        serde_json::to_value(preview.last_modified).unwrap(),
        serde_json::json!("2026-10-21T07:28:00Z")
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/bucket/notes/a.txt");
    // The page plus three bytes to finish its last character.
    assert_eq!(
        requests[0].header("range"),
        Some(format!("bytes=0-{}", DEFAULT_PAGE_BYTES + 2).as_str())
    );
    assert_eq!(requests[0].query_param("versionId"), None);
}

#[tokio::test]
async fn pages_of_a_long_text_are_one_request_each_and_tile_the_object() {
    let body: String = (0..50).map(|index| format!("row {index:04}\n")).collect();
    assert_eq!(body.len(), 450);
    let mock = MockS3::start(serve_object(body.clone(), "text/plain", None));
    let ops = mock.ops();
    let options = options(100);
    let mut collected = String::new();
    for page in 0..5u64 {
        let answer = preview(&ops, "log.txt", None, page, &options)
            .await
            .unwrap();
        assert_eq!(answer.kind, PreviewKind::Text);
        assert_eq!(
            (answer.page, answer.page_count, answer.size),
            (page, 5, 450)
        );
        assert_eq!(answer.offset, page * 100);
        assert_eq!(answer.length, if page == 4 { 50 } else { 100 });
        collected.push_str(answer.text.as_deref().unwrap());
    }
    assert_eq!(collected, body);
    let ranges: Vec<String> = mock
        .requests()
        .iter()
        .map(|r| r.header("range").unwrap().to_owned())
        .collect();
    assert_eq!(
        ranges,
        [
            "bytes=0-102",
            "bytes=100-202",
            "bytes=200-302",
            "bytes=300-402",
            "bytes=400-502"
        ]
    );
    // Past the last page.
    let err = preview(&ops, "log.txt", None, 5, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
    assert!(err.to_string().contains("past the end"), "{err}");
}

#[tokio::test]
async fn a_version_is_requested_by_id_and_a_delete_marker_has_no_content() {
    let body = "old text".to_owned();
    let mock = MockS3::start(
        move |request| match request.query_param("versionId").as_deref() {
            Some("v1") => serve_object(body.clone(), "text/plain", Some("v1"))(request),
            Some("dm") => MockResponse::empty(405)
                .with_header("x-amz-delete-marker", "true")
                .with_header("allow", "DELETE"),
            _ => unexpected(request),
        },
    );
    let ops = mock.ops();
    let answer = preview(&ops, "k", Some("v1"), 0, &PreviewOptions::default())
        .await
        .unwrap();
    assert_eq!(answer.text.as_deref(), Some("old text"));
    assert_eq!(answer.version_id.as_deref(), Some("v1"));
    assert_eq!(
        mock.requests()[0].query_param("versionId").as_deref(),
        Some("v1")
    );
    let err = preview(&ops, "k", Some("dm"), 0, &PreviewOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(err.to_string().contains("delete marker"), "{err}");
}

#[tokio::test]
async fn json_that_fits_the_first_page_is_validated_and_pretty_printed() {
    let compact =
        r#"{"name":"example","tags":["a","b"],"count":2.50,"nested":{"ok":true,"none":null}}"#;
    let mock = MockS3::start(serve_object(compact.to_owned(), "application/json", None));
    let answer = preview(&mock.ops(), "data", None, 0, &PreviewOptions::default())
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::Json);
    assert!(answer.formatted);
    assert_eq!(answer.media_type.as_deref(), Some("application/json"));
    let text = answer.text.unwrap();
    // Key order and number spellings are the document's own.
    assert_eq!(
        text,
        "{\n  \"name\": \"example\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ],\n  \"count\": 2.50,\n  \"nested\": {\n    \"ok\": true,\n    \"none\": null\n  }\n}"
    );
    assert_eq!(
        answer.length,
        compact.len() as u64,
        "the bytes of the object, not of the text"
    );

    // Declared JSON that is not JSON is shown as it is, as text.
    let broken = MockS3::start(serve_object(
        "{\"a\": ".to_owned(),
        "application/json",
        None,
    ));
    let answer = preview(&broken.ops(), "data", None, 0, &PreviewOptions::default())
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::Text);
    assert!(!answer.formatted);
    assert_eq!(answer.text.as_deref(), Some("{\"a\": "));

    // JSON by key alone.
    let by_key = MockS3::start(serve_object(
        "[1,2]".to_owned(),
        "application/octet-stream",
        None,
    ));
    let answer = preview(
        &by_key.ops(),
        "list.JSON",
        None,
        0,
        &PreviewOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(answer.kind, PreviewKind::Json);
    assert_eq!(answer.text.as_deref(), Some("[\n  1,\n  2\n]"));
}

#[tokio::test]
async fn long_json_is_paged_as_it_is() {
    let body = format!(
        "[{}]",
        (0..40)
            .map(|index| format!("{{\"id\":{index}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mock = MockS3::start(serve_object(body.clone(), "application/json", None));
    let ops = mock.ops();
    let first = preview(&ops, "big.json", None, 0, &options(64))
        .await
        .unwrap();
    // A document that does not fit one page cannot be validated or
    // re-indented, so it stays raw text of the kind JSON.
    assert_eq!(first.kind, PreviewKind::Json);
    assert!(!first.formatted);
    assert!(first.text.as_deref().unwrap().starts_with("[{\"id\":0}"));
    let pages = first.page_count;
    assert!(pages > 3);
    let mut all = first.text.unwrap();
    for page in 1..pages {
        let answer = preview(&ops, "big.json", None, page, &options(64))
            .await
            .unwrap();
        assert_eq!(answer.kind, PreviewKind::Json);
        all.push_str(&answer.text.unwrap());
    }
    assert_eq!(all, body);
}

#[tokio::test]
async fn an_image_longer_than_the_first_page_is_read_in_two_requests() {
    // A GIF header that the preview recognizes, then plain bytes.
    let body = format!("GIF89a{}", "x".repeat(500));
    let expected = base64(body.as_bytes());
    let options = PreviewOptions {
        page_bytes: 16,
        max_image_bytes: 1000,
    };

    // Unversioned: the rest is read with If-Match on the ETag.
    let mock = MockS3::start(serve_object(body.clone(), "image/gif", None));
    let answer = preview(&mock.ops(), "pic.gif", None, 0, &options)
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::Image);
    assert_eq!(answer.media_type.as_deref(), Some("image/gif"));
    assert_eq!(answer.image_base64.as_deref(), Some(expected.as_str()));
    assert_eq!(
        (answer.size, answer.length, answer.page_count),
        (506, 506, 1)
    );
    assert!(answer.text.is_none());
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("range"), Some("bytes=0-18"));
    assert_eq!(requests[1].header("range"), Some("bytes=19-505"));
    assert_eq!(requests[1].header("if-match"), Some("\"etag-1\""));

    // Versioned: the version pins it.
    let mock = MockS3::start(serve_object(body.clone(), "image/gif", Some("v9")));
    let answer = preview(&mock.ops(), "pic.gif", None, 0, &options)
        .await
        .unwrap();
    assert_eq!(answer.image_base64.as_deref(), Some(expected.as_str()));
    assert_eq!(answer.version_id.as_deref(), Some("v9"));
    let requests = mock.requests();
    assert_eq!(requests[0].query_param("versionId"), None);
    assert_eq!(requests[1].query_param("versionId").as_deref(), Some("v9"));
    assert_eq!(requests[1].header("if-match"), None);

    // An image that fits the first page needs one request.
    let small = MockS3::start(serve_object("GIF89a".to_owned(), "image/gif", None));
    let answer = preview(
        &small.ops(),
        "tiny.gif",
        None,
        0,
        &PreviewOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(answer.kind, PreviewKind::Image);
    assert_eq!(small.requests().len(), 1);

    // An image that changed between the two reads is an error.
    let changing = MockS3::start(move |request| {
        if request.header("if-match").is_some() {
            MockResponse::error(
                412,
                "PreconditionFailed",
                "At least one precondition failed",
            )
        } else {
            serve_object(body.clone(), "image/gif", None)(request)
        }
    });
    let err = preview(&changing.ops(), "pic.gif", None, 0, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
}

#[tokio::test]
async fn an_image_above_the_budget_is_too_large_and_is_not_downloaded() {
    let body = format!("GIF89a{}", "x".repeat(2000));
    let mock = MockS3::start(serve_object(body, "image/gif", None));
    let options = PreviewOptions {
        page_bytes: 64,
        max_image_bytes: 1000,
    };
    let answer = preview(&mock.ops(), "big.gif", None, 0, &options)
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::TooLarge);
    assert_eq!(answer.size, 2006);
    assert_eq!(answer.media_type.as_deref(), Some("image/gif"));
    assert!(answer.image_base64.is_none() && answer.text.is_none());
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn svg_is_an_image_only_when_the_content_type_or_the_key_says_so() {
    let svg = "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_owned();
    for (key, content_type, kind) in [
        ("logo", "image/svg+xml", PreviewKind::Image),
        ("logo.svg", "application/octet-stream", PreviewKind::Image),
        ("logo.txt", "text/plain", PreviewKind::Text),
    ] {
        let mock = MockS3::start(serve_object(svg.clone(), content_type, None));
        let answer = preview(&mock.ops(), key, None, 0, &PreviewOptions::default())
            .await
            .unwrap();
        assert_eq!(answer.kind, kind, "{key} {content_type}");
        if kind == PreviewKind::Image {
            assert_eq!(answer.media_type.as_deref(), Some("image/svg+xml"));
        }
    }
}

#[tokio::test]
async fn data_that_is_not_text_or_an_image_is_binary() {
    let mock = MockS3::start(serve_object(
        "header\0then bytes".to_owned(),
        "text/plain",
        None,
    ));
    let answer = preview(&mock.ops(), "odd.txt", None, 0, &PreviewOptions::default())
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::Binary);
    assert_eq!(answer.size, 17);
    assert_eq!(answer.content_type.as_deref(), Some("text/plain"));
    assert!(answer.text.is_none() && answer.image_base64.is_none());
    assert_eq!((answer.page_count, answer.length), (1, 0));
}

#[tokio::test]
async fn an_empty_object_is_found_by_the_failed_range_and_a_head() {
    let mock = MockS3::start(serve_object(String::new(), "text/plain", Some("v2")));
    let answer = preview(
        &mock.ops(),
        "empty.txt",
        None,
        0,
        &PreviewOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(answer.kind, PreviewKind::Empty);
    assert_eq!((answer.size, answer.page_count, answer.length), (0, 1, 0));
    assert_eq!(answer.version_id.as_deref(), Some("v2"));
    assert_eq!(answer.etag.as_deref(), Some("etag-1"));
    let methods: Vec<String> = mock.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["GET", "HEAD"]);

    // A range that cannot be satisfied for another reason is an error.
    let odd = MockS3::start(|request| match request.method.as_str() {
        "GET" => MockResponse::error(
            416,
            "InvalidRange",
            "The requested range is not satisfiable",
        ),
        _ => MockResponse::empty(200).with_header("content-length", "5"),
    });
    let err = preview(&odd.ops(), "k", None, 0, &PreviewOptions::default())
        .await
        .unwrap_err();
    assert_ne!(err.kind(), ErrorKind::Usage);
    assert!(err.to_string().contains("InvalidRange"), "{err}");
}

#[tokio::test]
async fn a_server_that_ignores_ranges_still_gives_the_first_page() {
    let body = "abcdefghij".repeat(30);
    let total = body.len();
    let mock = MockS3::start(move |request| match request.method.as_str() {
        "GET" => MockResponse {
            status: 200,
            headers: vec![("etag".into(), "\"e\"".into())],
            body: body.clone(),
        },
        _ => unexpected(request),
    });
    let ops = mock.ops();
    let options = options(100);
    let first = preview(&ops, "k", None, 0, &options).await.unwrap();
    assert_eq!(first.kind, PreviewKind::Text);
    assert_eq!(first.size, total as u64);
    assert_eq!(first.page_count, 3);
    assert_eq!(
        first.text.as_deref(),
        Some("abcdefghij".repeat(10).as_str())
    );
    // A later page cannot be had from such a server.
    let err = preview(&ops, "k", None, 1, &options).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
    assert!(err.to_string().contains("instead of byte 100"), "{err}");
}

#[tokio::test]
async fn server_errors_keep_their_kind() {
    for (status, code, kind) in [
        (404, "NoSuchKey", ErrorKind::NotFound),
        (404, "NoSuchVersion", ErrorKind::NotFound),
        (403, "AccessDenied", ErrorKind::Auth),
        (403, "InvalidObjectState", ErrorKind::Conflict),
        (500, "InternalError", ErrorKind::Remote),
    ] {
        let mock = MockS3::start(move |_| MockResponse::error(status, code, "server says no"));
        let err = preview(
            &mock.ops(),
            "dir/file.txt",
            None,
            0,
            &PreviewOptions::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), kind, "{code}: {err}");
        assert!(
            err.to_string().contains("s3://bucket/dir/file.txt"),
            "{err}"
        );
    }
}

#[tokio::test]
async fn invalid_requests_never_reach_the_server() {
    let mock = MockS3::start(unexpected);
    let ops = mock.ops();
    let default = PreviewOptions::default();
    for (key, version, page_options) in [
        ("folder/", None, default),
        ("", None, default),
        ("k", Some(""), default),
        ("k", None, options(15)),
        ("k", None, options(1024 * 1024 + 1)),
        (
            "k",
            None,
            PreviewOptions {
                max_image_bytes: 0,
                ..default
            },
        ),
    ] {
        let err = preview(&ops, key, version, 0, &page_options)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{key:?} {version:?}: {err}");
    }
    let err = ops
        .preview_object("bad bucket", "k", None, 0, &default)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    // An offset S3 cannot address.
    let err = preview(&ops, "k", None, u64::MAX / 2, &default)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn requester_pays_reaches_the_ranged_reads() {
    let body = format!("GIF89a{}", "x".repeat(100));
    let mock = MockS3::start(serve_object(body, "image/gif", Some("v1")));
    let client = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    let ops = mock.ops_with(&client, true);
    let options = PreviewOptions {
        page_bytes: 16,
        ..PreviewOptions::default()
    };
    preview(&ops, "k", None, 0, &options).await.unwrap();
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.header("x-amz-request-payer") == Some("requester"))
    );
}

// ---- the in-process server: real bytes ----

#[tokio::test]
async fn pages_tile_an_object_of_several_scripts_exactly() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let text = "caf\u{e9} cr\u{e8}me \u{65e5}\u{672c}\u{8a9e} \u{1f4f7} fin. ".repeat(30);
    s3.put(&bucket, "unicode.txt", text.clone()).await;
    for page_bytes in [16u64, 17, 18, 19, 25, 64] {
        let options = options(page_bytes);
        let first = s3
            .ops()
            .preview_object(&bucket, "unicode.txt", None, 0, &options)
            .await
            .unwrap();
        assert_eq!(first.kind, PreviewKind::Text);
        assert_eq!(first.size, text.len() as u64);
        assert_eq!(first.page_count, (text.len() as u64).div_ceil(page_bytes));
        let mut collected = first.text.clone().unwrap();
        let mut end = first.offset + first.length;
        for page in 1..first.page_count {
            let answer = s3
                .ops()
                .preview_object(&bucket, "unicode.txt", None, page, &options)
                .await
                .unwrap();
            // Consecutive: each page starts where the one before it ended.
            assert_eq!(answer.offset, end, "page {page} at {page_bytes} bytes");
            end = answer.offset + answer.length;
            let page_text = answer.text.unwrap();
            assert_eq!(page_text.len() as u64, answer.length);
            assert!(!page_text.contains('\u{fffd}'), "{page_text:?}");
            collected.push_str(&page_text);
        }
        assert_eq!(end, text.len() as u64);
        assert_eq!(collected, text, "page size {page_bytes}");
    }
}

#[tokio::test]
async fn any_page_of_a_large_object_is_reached_without_reading_the_others() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    // 3 MiB of numbered lines.
    let mut text = String::new();
    let mut index = 0u32;
    while text.len() < 3 * 1024 * 1024 {
        text.push_str(&format!("line {index:08}\n"));
        index += 1;
    }
    s3.put(&bucket, "big.log", text.clone()).await;
    let pages = (text.len() as u64).div_ceil(DEFAULT_PAGE_BYTES);
    let last = s3
        .ops()
        .preview_object(
            &bucket,
            "big.log",
            None,
            pages - 1,
            &PreviewOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        (last.kind, last.page_count, last.page),
        (PreviewKind::Text, pages, pages - 1)
    );
    assert_eq!(last.offset, (pages - 1) * DEFAULT_PAGE_BYTES);
    assert!(text.ends_with(last.text.as_deref().unwrap()));
    let middle = s3
        .ops()
        .preview_object(&bucket, "big.log", None, 7, &PreviewOptions::default())
        .await
        .unwrap();
    assert_eq!(middle.offset, 7 * DEFAULT_PAGE_BYTES);
    let at = middle.offset as usize;
    assert_eq!(
        middle.text.as_deref(),
        Some(&text[at..at + middle.length as usize])
    );
    let beyond = s3
        .ops()
        .preview_object(&bucket, "big.log", None, pages, &PreviewOptions::default())
        .await
        .unwrap_err();
    assert_eq!(beyond.kind(), ErrorKind::Usage, "{beyond}");
}

#[tokio::test]
async fn real_image_bytes_come_back_whole_and_unchanged() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    // Bytes that are not text, behind real image signatures.
    let padding: Vec<u8> = (0..3000u32).map(|n| (n * 7 % 251) as u8 | 0x80).collect();
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend_from_slice(&padding);
    let mut webp = b"RIFF\xd0\x07\x00\x00WEBPVP8L".to_vec();
    webp.extend_from_slice(&padding);
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    jpeg.extend_from_slice(&padding);
    for (key, bytes, media_type) in [
        ("a.png", &png, "image/png"),
        ("b.webp", &webp, "image/webp"),
        ("c.jpg", &jpeg, "image/jpeg"),
    ] {
        s3.put(&bucket, key, bytes.clone()).await;
        for page_bytes in [16u64, 128 * 1024] {
            let answer = s3
                .ops()
                .preview_object(&bucket, key, None, 0, &options(page_bytes))
                .await
                .unwrap();
            assert_eq!(answer.kind, PreviewKind::Image, "{key} {page_bytes}");
            assert_eq!(answer.media_type.as_deref(), Some(media_type));
            assert_eq!(answer.size, bytes.len() as u64);
            assert_eq!(answer.length, bytes.len() as u64);
            assert_eq!(
                answer.image_base64.as_deref(),
                Some(base64(bytes).as_str()),
                "{key} {page_bytes}"
            );
        }
    }
    // The budget turns the same image into "too large".
    let limited = PreviewOptions {
        max_image_bytes: 1000,
        ..PreviewOptions::default()
    };
    let answer = s3
        .ops()
        .preview_object(&bucket, "b.webp", None, 0, &limited)
        .await
        .unwrap();
    assert_eq!(answer.kind, PreviewKind::TooLarge);
    assert_eq!(answer.media_type.as_deref(), Some("image/webp"));
}

#[tokio::test]
async fn real_objects_are_text_json_binary_or_empty() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "empty", Vec::new()).await;
    s3.put(&bucket, "doc.json", r#"{"b":[1,2],"a":{}}"#).await;
    s3.put(&bucket, "bom.txt", b"\xEF\xBB\xBFhello".to_vec())
        .await;
    s3.put(&bucket, "latin1.txt", b"caf\xe9 au lait".to_vec())
        .await;
    s3.put(&bucket, "data.bin", vec![0u8, 1, 2, 3, 0xFF, 0xFE])
        .await;
    let default = PreviewOptions::default();

    let empty = s3
        .ops()
        .preview_object(&bucket, "empty", None, 0, &default)
        .await
        .unwrap();
    assert_eq!(empty.kind, PreviewKind::Empty);
    assert_eq!((empty.size, empty.page_count), (0, 1));

    let json = s3
        .ops()
        .preview_object(&bucket, "doc.json", None, 0, &default)
        .await
        .unwrap();
    assert_eq!(json.kind, PreviewKind::Json);
    assert!(json.formatted);
    assert_eq!(
        json.text.as_deref(),
        Some("{\n  \"b\": [\n    1,\n    2\n  ],\n  \"a\": {}\n}")
    );

    // The byte order mark is dropped from the text, not from the length.
    let bom = s3
        .ops()
        .preview_object(&bucket, "bom.txt", None, 0, &default)
        .await
        .unwrap();
    assert_eq!(bom.text.as_deref(), Some("hello"));
    assert_eq!((bom.offset, bom.length, bom.size), (0, 8, 8));

    for key in ["latin1.txt", "data.bin"] {
        let answer = s3
            .ops()
            .preview_object(&bucket, key, None, 0, &default)
            .await
            .unwrap();
        assert_eq!(answer.kind, PreviewKind::Binary, "{key}");
        assert!(answer.text.is_none());
    }
    let err = s3
        .ops()
        .preview_object(&bucket, "missing.txt", None, 0, &default)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

// ---- real versioning (MinIO) ----

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement versioning"
)]
async fn minio_previews_an_old_version_and_refuses_a_delete_marker() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    s3.client()
        .put_bucket_versioning()
        .bucket(&bucket)
        .versioning_configuration(
            VersioningConfiguration::builder()
                .status(BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap();
    s3.put(&bucket, "doc.txt", "first version").await;
    s3.put(&bucket, "doc.txt", "second version").await;
    s3.ops()
        .delete_object(&bucket, "doc.txt", None)
        .await
        .unwrap();
    let page = s3
        .ops()
        .list_version_groups(
            &bucket,
            &bucketbridge_core::versions::VersionQuery::new("doc.txt"),
        )
        .await
        .unwrap();
    let versions = &page.groups[0].versions;
    let (marker, newest, oldest) = (&versions[0], &versions[1], &versions[2]);
    let options = PreviewOptions::default();
    let old = s3
        .ops()
        .preview_object(&bucket, "doc.txt", Some(&oldest.version_id), 0, &options)
        .await
        .unwrap();
    assert_eq!(old.text.as_deref(), Some("first version"));
    assert_eq!(old.version_id.as_deref(), Some(oldest.version_id.as_str()));
    let new = s3
        .ops()
        .preview_object(&bucket, "doc.txt", Some(&newest.version_id), 0, &options)
        .await
        .unwrap();
    assert_eq!(new.text.as_deref(), Some("second version"));
    let err = s3
        .ops()
        .preview_object(&bucket, "doc.txt", Some(&marker.version_id), 0, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
    // The latest entry is a delete marker: the key has no current content.
    let err = s3
        .ops()
        .preview_object(&bucket, "doc.txt", None, 0, &options)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}
