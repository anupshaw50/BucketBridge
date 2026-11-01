//! URL shortening through `Session::shorten_url`, against a local scripted
//! HTTP server standing in for the providers: nothing here reaches the
//! network. Secrets live in an in-memory store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bucketbridge_core::secrets::{KdfCost, MemoryStore};
use bucketbridge_core::session::SessionOptions;
use bucketbridge_core::settings::ShortenerProvider;
use bucketbridge_core::shorten::Endpoints;
use bucketbridge_core::{Dirs, ErrorKind, Session};
use bucketbridge_tests::{MockResponse, MockS3};

const LONG: &str = "https://bucket.s3.example.com/key?X-Amz-Signature=abc";

fn answers(request: &bucketbridge_tests::RecordedRequest) -> MockResponse {
    let json = |body: &str| MockResponse {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_owned(),
    };
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/bitly") => {
            if request.header("authorization") == Some("Bearer good") {
                json(r#"{"link":"https://bit.example/abc"}"#)
            } else {
                MockResponse::empty(403)
            }
        }
        ("POST", "/tiny") => json(r#"{"data":{"tiny_url":"https://tiny.example/xyz"}}"#),
        ("GET", "/plain") => MockResponse {
            status: 200,
            headers: vec![],
            body: "https://tiny.example/plain\n".into(),
        },
        _ => MockResponse::empty(404),
    }
}

fn session(dir: &tempfile::TempDir, server: &MockS3) -> Session {
    let base = server.endpoint();
    let options = SessionOptions {
        kdf_cost: KdfCost {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        },
        environment_proxy: false,
        shortener_endpoints: Some(Endpoints {
            bitly: format!("{base}/bitly"),
            tinyurl: format!("{base}/tiny"),
            tinyurl_plain: format!("{base}/plain"),
        }),
        ..SessionOptions::default()
    };
    Session::open_with(
        Dirs::under(dir.path()),
        Arc::new(MemoryStore::new()),
        options,
    )
    .unwrap()
}

async fn choose(session: &Session, provider: ShortenerProvider) {
    let mut settings = session.settings().await.unwrap();
    settings.shortener.provider = provider;
    session.save_settings(settings).await.unwrap();
}

#[tokio::test]
async fn nothing_is_sent_without_a_provider() {
    let server = MockS3::start(answers);
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir, &server);
    let error = session.shorten_url(LONG).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn bitly_uses_the_stored_token() {
    let server = MockS3::start(answers);
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir, &server);
    choose(&session, ShortenerProvider::Bitly).await;
    session.set_shortener_token(Some("good")).await.unwrap();
    assert_eq!(
        session.shorten_url(LONG).await.unwrap(),
        "https://bit.example/abc"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body_text().contains(LONG));
    // A wrong token is an auth error that does not echo the token.
    session
        .set_shortener_token(Some("bad-secret"))
        .await
        .unwrap();
    let error = session.shorten_url(LONG).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    assert!(!error.to_string().contains("bad-secret"));
}

#[tokio::test]
async fn bitly_without_a_token_is_a_usage_error() {
    let server = MockS3::start(answers);
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir, &server);
    choose(&session, ShortenerProvider::Bitly).await;
    let error = session.shorten_url(LONG).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn tinyurl_with_and_without_a_token() {
    let server = MockS3::start(answers);
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir, &server);
    choose(&session, ShortenerProvider::Tinyurl).await;
    assert_eq!(
        session.shorten_url(LONG).await.unwrap(),
        "https://tiny.example/plain"
    );
    session.set_shortener_token(Some("t")).await.unwrap();
    assert_eq!(
        session.shorten_url(LONG).await.unwrap(),
        "https://tiny.example/xyz"
    );
}

#[tokio::test]
async fn a_token_override_wins_over_the_stored_one() {
    let server = MockS3::start(answers);
    let dir = tempfile::tempdir().unwrap();
    let session = session(&dir, &server);
    choose(&session, ShortenerProvider::Bitly).await;
    session.set_shortener_token(Some("stored")).await.unwrap();
    let endpoints = Endpoints {
        bitly: format!("{}/bitly", server.endpoint()),
        ..Endpoints::default()
    };
    let link = session
        .shorten_url_with(&endpoints, Some("good".into()), LONG)
        .await
        .unwrap();
    assert_eq!(link, "https://bit.example/abc");
}
