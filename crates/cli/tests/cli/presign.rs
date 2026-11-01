//! `bb presign`

use super::*;
use bucketbridge_tests::MockResponse;

#[tokio::test]
async fn get_url_downloads_the_object() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    env.s3
        .put(&bucket, "shared/a b+ü.txt", "shared content")
        .await;
    let out = env.run(&["presign", &format!("s3://{bucket}/shared/a b+ü.txt")]);
    out.expect_code(0);
    let url = out.stdout.trim();
    assert_eq!(out.lines().len(), 1);
    assert!(url.contains("X-Amz-Expires=3600"), "{url}");
    let response = reqwest::get(url).await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"shared content");
}

#[tokio::test]
async fn put_url_uploads_an_object() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let out = env.run(&[
        "presign",
        "--method",
        "put",
        "--expires",
        "15m",
        &format!("s3://{bucket}/in/up.txt"),
    ]);
    out.expect_code(0);
    let url = out.stdout.trim().to_owned();
    assert!(url.contains("X-Amz-Expires=900"), "{url}");
    let response = reqwest::Client::new()
        .put(&url)
        .body("uploaded")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(env.s3.get(&bucket, "in/up.txt").await, b"uploaded");
}

#[tokio::test]
async fn durations_and_the_json_record() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let uri = format!("s3://{bucket}/k.txt");
    for (expires, seconds) in [
        ("30s", 30),
        ("90", 90),
        ("1h30m", 5400),
        ("7d", 604_800),
        ("1s", 1),
    ] {
        let out = env.run(&["presign", "--expires", expires, &uri]);
        out.expect_code(0);
        assert!(
            out.stdout.contains(&format!("X-Amz-Expires={seconds}&")),
            "{expires}: {}",
            out.stdout
        );
    }
    let before = chrono::Utc::now();
    let out = env.run(&["presign", "--json", "--expires", "2h", &uri]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["bucket"], bucket.as_str());
    assert_eq!(record["key"], "k.txt");
    assert_eq!(record["method"], "get");
    assert_eq!(record["expires_secs"], 7200);
    assert!(
        record["url"]
            .as_str()
            .unwrap()
            .contains("X-Amz-Expires=7200")
    );
    let expires_at: chrono::DateTime<chrono::Utc> =
        record["expires_at"].as_str().unwrap().parse().unwrap();
    let lifetime = (expires_at - before).num_seconds();
    assert!(
        (7195..=7260).contains(&lifetime),
        "expires_at {expires_at} is {lifetime}s ahead"
    );
}

#[tokio::test]
async fn invalid_durations_and_targets_are_usage_errors() {
    let env = Env::start().await;
    let bucket = env.s3.new_bucket().await;
    let uri = format!("s3://{bucket}/k.txt");
    for expires in [
        "0s",
        "0",
        "8d",
        "604801",
        "1.5h",
        "-1h",
        "",
        "1w",
        "h",
        "1h30",
        "99999999999999999999d",
    ] {
        // `--expires=VALUE` hands every value, even "-1h", to the duration parser.
        let out = env.run(&["presign", &format!("--expires={expires}"), &uri]);
        out.expect_code(1);
        assert!(
            out.stderr.contains("--expires"),
            "{expires:?}: {}",
            out.stderr
        );
        let reason = if ["0s", "0", "8d", "604801"].contains(&expires) {
            "out of range"
        } else {
            "invalid duration"
        };
        assert!(out.stderr.contains(reason), "{expires:?}: {}", out.stderr);
    }
    env.run(&["presign", "--expires", "-1h", &uri])
        .expect_code(1);
    let out = env.run(&["presign", &format!("s3://{bucket}")]);
    out.expect_code(1);
    assert!(out.stderr.contains("names a bucket"), "{}", out.stderr);
    env.run(&["presign", "--method", "delete", &uri])
        .expect_code(1);
}

/// A local server standing in for the shortener: nothing reaches the network.
fn shortener_server() -> MockS3 {
    MockS3::start(|request| {
        if request.header("authorization") == Some("Bearer good") {
            MockResponse {
                status: 200,
                headers: vec![],
                body: r#"{"link":"https://bit.example/q"}"#.into(),
            }
        } else {
            MockResponse::empty(403)
        }
    })
}

fn presign_vars<'a>(server: &'a MockS3, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut vars = vec![
        ("BB_ACCESS_KEY_ID", "AKIDTEST"),
        ("BB_SECRET_ACCESS_KEY", "test-secret"),
        ("BB_ENDPOINT", "http://127.0.0.1:9"),
        ("BB_PATH_STYLE", "true"),
        ("BB_SHORTENER_ENDPOINT", server.endpoint()),
    ];
    vars.extend_from_slice(extra);
    vars
}

fn choose_provider(bb: &InProcess, provider: &str) {
    std::fs::write(
        bb.config.path().join("config.toml"),
        format!("[settings.shortener]\nprovider = \"{provider}\"\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn shorten_is_off_unless_asked_and_needs_a_provider() {
    let server = shortener_server();
    let bb = InProcess::default();
    // Without --shorten nothing is sent, even with a provider chosen.
    choose_provider(&bb, "bitly");
    let vars = presign_vars(&server, &[("BB_SHORTENER_TOKEN", "good")]);
    let out = bb
        .run_with(&["presign", "s3://b/k"], &vars, b"", &Terminal::default())
        .await;
    out.expect_code(0);
    assert!(out.stdout.contains("X-Amz-Signature="), "{}", out.stdout);
    assert!(server.requests().is_empty());
    // --shorten without a provider is a usage error and sends nothing.
    let bb = InProcess::default();
    let out = bb
        .run_with(
            &["presign", "--shorten", "s3://b/k"],
            &vars,
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(1);
    assert!(out.stderr.contains("no URL shortener"), "{}", out.stderr);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn shorten_prints_the_short_link_and_says_where_it_went() {
    let server = shortener_server();
    let bb = InProcess::default();
    choose_provider(&bb, "bitly");
    let vars = presign_vars(&server, &[("BB_SHORTENER_TOKEN", "good")]);
    let out = bb
        .run_with(
            &["presign", "--shorten", "s3://b/k"],
            &vars,
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    assert_eq!(out.stdout.trim(), "https://bit.example/q");
    assert!(out.stderr.contains("sent to Bitly"), "{}", out.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body_text().contains("X-Amz-Signature"));
    // JSON keeps both links and no notice.
    let out = bb
        .run_with(
            &["presign", "--shorten", "--json", "s3://b/k"],
            &vars,
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["short_url"], "https://bit.example/q");
    assert!(record["url"].as_str().unwrap().contains("X-Amz-Signature="));
    assert!(out.stderr.is_empty(), "{}", out.stderr);
}

#[tokio::test]
async fn a_rejected_token_is_an_auth_error() {
    let server = shortener_server();
    let bb = InProcess::default();
    choose_provider(&bb, "bitly");
    let vars = presign_vars(&server, &[("BB_SHORTENER_TOKEN", "wrong-token")]);
    let out = bb
        .run_with(
            &["presign", "--shorten", "s3://b/k"],
            &vars,
            b"",
            &Terminal::default(),
        )
        .await;
    out.expect_code(3);
    assert!(!out.stderr.contains("wrong-token"), "{}", out.stderr);
}
