//! `bb version` and the opt-in `bb version --check`: the check asks a scripted
//! server (`BB_UPDATE_CHECK_URL`) and plain `bb version` never makes a
//! request.

use bucketbridge_cli::i18n::trf;
use bucketbridge_tests::MockResponse;

use super::*;

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const PAGE: &str = "https://github.com/anupshaw50/BucketBridge/releases/tag/v99.0.0";

fn release(tag: &str) -> MockResponse {
    MockResponse {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: format!(r#"{{"tag_name":"{tag}","html_url":"{PAGE}","name":"x","body":"notes"}}"#),
    }
}

fn answering(response: MockResponse) -> MockS3 {
    MockS3::start(move |_| response.clone())
}

/// `bb` with the release address pointed at `url`, and nothing else set.
fn bb_check(config: &std::path::Path, url: &str) -> assert_cmd::Command {
    let mut cmd = bb_process(config);
    cmd.env("BB_UPDATE_CHECK_URL", url);
    assert_cmd::Command::from_std(cmd)
}

#[tokio::test]
async fn plain_version_makes_no_request() {
    let mock = answering(release("v99.0.0"));
    let config = tempfile::tempdir().unwrap();
    let out = run(bb_check(config.path(), &format!("{}/latest", mock.endpoint())).arg("version"));
    out.expect_code(0);
    assert_eq!(out.stdout.trim(), format!("bb {CURRENT}"));

    let out = run(
        bb_check(config.path(), &format!("{}/latest", mock.endpoint())).args(["--json", "version"]),
    );
    out.expect_code(0);
    assert_eq!(out.records(), [serde_json::json!({ "version": CURRENT })]);

    // The flag of clap is the same text and needs no network either.
    let out = run(bb_check(config.path(), &format!("{}/latest", mock.endpoint())).arg("--version"));
    out.expect_code(0);
    assert!(out.stdout.contains(CURRENT), "{}", out.stdout);

    // No other command asks either: not even one that runs without an account.
    run(bb_check(config.path(), &format!("{}/latest", mock.endpoint())).args(["account", "list"]))
        .expect_code(0);
    assert!(
        mock.requests().is_empty(),
        "a request was made: {:?}",
        mock.requests()
    );
}

#[tokio::test]
async fn check_reports_a_newer_release_with_its_page() {
    let mock = answering(release("v99.0.0"));
    let config = tempfile::tempdir().unwrap();
    let url = format!("{}/repos/x/releases/latest", mock.endpoint());
    let out = run(bb_check(config.path(), &url).args(["version", "--check"]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "update available: {latest} (you have {current}); release page: {url}",
            &[("latest", "99.0.0"), ("current", CURRENT), ("url", PAGE)],
        )
    );

    let out = run(bb_check(config.path(), &url).args(["--json", "version", "--check"]));
    out.expect_code(0);
    assert_eq!(
        out.records(),
        [serde_json::json!({
            "current": CURRENT,
            "latest": "99.0.0",
            "update_available": true,
            "url": PAGE,
        })]
    );

    // One GET per run, with no account, no key and no version number.
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/repos/x/releases/latest");
        assert_eq!(
            request.header("accept"),
            Some("application/vnd.github+json")
        );
        let agent = request.header("user-agent").unwrap();
        assert!(!agent.contains(CURRENT), "{agent}");
        assert!(request.header("authorization").is_none());
        assert!(request.body.is_empty());
    }
}

#[tokio::test]
async fn check_says_when_there_is_nothing_newer() {
    let config = tempfile::tempdir().unwrap();
    let same = answering(release(&format!("v{CURRENT}")));
    let out = run(
        bb_check(config.path(), &format!("{}/l", same.endpoint())).args(["version", "--check"])
    );
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "bb {current} is up to date (the latest release is {latest})",
            &[("current", CURRENT), ("latest", CURRENT)],
        )
    );
    let out = run(bb_check(config.path(), &format!("{}/l", same.endpoint()))
        .args(["--json", "version", "--check"]));
    assert_eq!(out.records()[0]["update_available"], false);

    let older = answering(release("0.0.1"));
    let out =
        run(bb_check(config.path(), &format!("{}/l", older.endpoint()))
            .args(["version", "--check"]));
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "bb {current} is newer than the latest release ({latest})",
            &[("current", CURRENT), ("latest", "0.0.1")],
        )
    );
}

#[tokio::test]
async fn a_failed_check_is_an_error_with_exit_2() {
    let config = tempfile::tempdir().unwrap();
    for response in [
        MockResponse::empty(404),
        MockResponse::empty(500),
        MockResponse::empty(403),
        MockResponse {
            status: 200,
            headers: Vec::new(),
            body: "not json".into(),
        },
        MockResponse {
            status: 200,
            headers: Vec::new(),
            body: r#"{"tag_name":"v1"}"#.into(),
        },
        // A tag that is no version.
        release("nightly"),
    ] {
        let mock = answering(response.clone());
        let out = run(bb_check(config.path(), &format!("{}/l", mock.endpoint()))
            .args(["--json", "version", "--check"]));
        out.expect_code(2);
        assert_eq!(out.error_record()["code"], "remote", "{response:?}");
        assert!(out.stdout.is_empty());
    }
}

#[tokio::test]
async fn an_unreachable_server_is_exit_2_with_a_clear_message() {
    let config = tempfile::tempdir().unwrap();
    // A port nobody listens on.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/latest", free.local_addr().unwrap());
    drop(free);
    let out = run(bb_check(config.path(), &url).args(["version", "--check"]));
    out.expect_code(2);
    assert!(
        out.stderr.contains("could not reach GitHub"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn the_proxy_settings_apply_to_the_check() {
    // The proxy answers for a host that does not exist.
    let proxy = answering(release("v99.0.0"));
    let config = tempfile::tempdir().unwrap();
    let mut cmd = bb_process(config.path());
    cmd.env("BB_UPDATE_CHECK_URL", "http://updates.invalid/latest")
        .env("HTTP_PROXY", proxy.endpoint())
        .env("http_proxy", proxy.endpoint());
    let out = run(assert_cmd::Command::from_std(cmd).args(["version", "--check"]));
    out.expect_code(0);
    assert!(out.stdout.contains("99.0.0"), "{}", out.stdout);
    let requests = proxy.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].path.contains("updates.invalid")
            || requests[0].header("host") == Some("updates.invalid"),
        "{:?}",
        requests[0]
    );
}
