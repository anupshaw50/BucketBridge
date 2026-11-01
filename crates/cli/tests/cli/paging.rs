//! Pagination: the configured page size reaches the server, and a server
//! that repeats or cycles its continuation tokens ends the command with a
//! remote error (exit 2) instead of an endless listing.

use bucketbridge_tests::{MockResponse, RecordedRequest};

use super::*;

fn is_list_v2(request: &RecordedRequest) -> bool {
    request.method == "GET" && request.query_param("list-type").as_deref() == Some("2")
}

/// Three pages behind the tokens `t1` and `t2`.
fn three_pages() -> MockS3 {
    MockS3::start(|request| {
        if !is_list_v2(request) {
            return MockResponse::error(400, "Unexpected", "unexpected request");
        }
        match request.query_param("continuation-token").as_deref() {
            None => MockResponse::xml(xml::objects_page(&["item-0", "item-1"], Some("t1"))),
            Some("t1") => MockResponse::xml(xml::objects_page(&["item-2", "item-3"], Some("t2"))),
            Some("t2") => MockResponse::xml(xml::objects_page(&["item-4"], None)),
            Some(_) => MockResponse::error(400, "Unexpected", "unknown token"),
        }
    })
}

fn max_keys(requests: &[RecordedRequest]) -> Vec<String> {
    requests
        .iter()
        .map(|r| r.query_param("max-keys").unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn the_configured_page_size_is_sent_and_every_page_is_followed() {
    let mock = three_pages();
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("config.toml"),
        "[settings.advanced]\nlist_page_size = 2\n",
    )
    .unwrap();

    let out = run(bb_mock(&mock, config.path()).args(["ls", "s3://bucket/"]));
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        ["item-0", "item-1", "item-2", "item-3", "item-4"]
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(max_keys(&requests), ["2", "2", "2"]);
    let tokens: Vec<Option<String>> = requests
        .iter()
        .map(|r| r.query_param("continuation-token"))
        .collect();
    assert_eq!(tokens, [None, Some("t1".to_owned()), Some("t2".to_owned())]);

    // Without a setting the default page size (1000) is used.
    let defaults = tempfile::tempdir().unwrap();
    run(bb_mock(&mock, defaults.path()).args(["ls", "-r", "s3://bucket/"])).expect_code(0);
    assert_eq!(max_keys(&mock.requests()[3..]), ["1000", "1000", "1000"]);
}

#[tokio::test]
async fn config_dir_flag_settings_reach_the_server() {
    let mock = three_pages();
    // BB_CONFIG_DIR points at a corrupt file, --config-dir at a valid one.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("config.toml"), "= = not toml").unwrap();
    let good = tempfile::tempdir().unwrap();
    std::fs::write(
        good.path().join("config.toml"),
        "[settings.advanced]\nlist_page_size = 3\n",
    )
    .unwrap();

    let out = run(bb_mock(&mock, broken.path()).args(["ls", "s3://bucket/"]));
    out.expect_code(2);
    assert!(out.stderr.contains("config.toml"), "{}", out.stderr);
    assert!(mock.requests().is_empty());

    let out = run(bb_mock(&mock, broken.path()).args([
        "ls",
        "--config-dir",
        good.path().to_str().unwrap(),
        "s3://bucket/",
    ]));
    out.expect_code(0);
    assert_eq!(max_keys(&mock.requests()), ["3", "3", "3"]);
}

#[tokio::test]
async fn a_repeated_token_ends_the_listing() {
    let mock = MockS3::start(|request| {
        if is_list_v2(request) {
            MockResponse::xml(xml::objects_page(&["k"], Some("again")))
        } else {
            MockResponse::error(400, "Unexpected", "unexpected request")
        }
    });
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["ls", "--json", "s3://bucket/"]));
    out.expect_code(2);
    let error = out.error_record();
    assert_eq!(error["code"], "remote");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("continuation token"),
        "{error}"
    );
    assert_eq!(mock.requests().len(), 2);
}

#[tokio::test]
async fn a_token_cycle_ends_the_listing() {
    let mock = MockS3::start(|request| {
        if !is_list_v2(request) {
            return MockResponse::error(400, "Unexpected", "unexpected request");
        }
        let next = match request.query_param("continuation-token").as_deref() {
            None | Some("B") => "A",
            Some(_) => "B",
        };
        MockResponse::xml(xml::objects_page(&["k"], Some(next)))
    });
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["ls", "-r", "s3://bucket/"]));
    out.expect_code(2);
    assert!(out.stderr.contains("continuation token"), "{}", out.stderr);
    assert_eq!(mock.requests().len(), 3);
}

/// Version pages that cycle: no marker, then (a, v1), then (b, v1), then (a, v1) again.
fn cycling_versions() -> MockS3 {
    MockS3::start(|request| {
        if request.query_param("versions").is_none() {
            return MockResponse::error(400, "Unexpected", "unexpected request");
        }
        let next = match request.query_param("key-marker").as_deref() {
            None | Some("b") => ("a", "v1"),
            Some(_) => ("b", "v1"),
        };
        MockResponse::xml(xml::versions(&[("a", "v1", true, false)], &[], Some(next)))
    })
}

#[tokio::test]
async fn a_version_marker_cycle_ends_ls() {
    let mock = cycling_versions();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args(["ls", "--versions", "s3://bucket/"]));
    out.expect_code(2);
    assert!(out.stderr.contains("continuation token"), "{}", out.stderr);
    assert_eq!(mock.requests().len(), 3);
}

#[tokio::test]
async fn a_version_marker_cycle_ends_rm_versions() {
    let mock = cycling_versions();
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_mock(&mock, config.path()).args([
        "rm",
        "-r",
        "--versions",
        "--dry-run",
        "s3://bucket/",
    ]));
    out.expect_code(2);
    assert!(out.stderr.contains("continuation token"), "{}", out.stderr);
    assert_eq!(mock.requests().len(), 3);
}
