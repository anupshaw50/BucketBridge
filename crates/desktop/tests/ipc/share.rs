//! URL shortener commands: off by default, token in the secret store, links
//! sent only to a local scripted server (never the network).

use bucketbridge_core::shorten::Endpoints;
use bucketbridge_tests::{MockResponse, MockS3};
use serde_json::json;

use crate::harness::*;

const LONG: &str = "https://bucket.s3.example.com/key?X-Amz-Signature=abc";

fn bitly() -> MockS3 {
    MockS3::start(|request| {
        if request.header("authorization") == Some("Bearer tok") {
            MockResponse {
                status: 200,
                headers: vec![],
                body: r#"{"link":"https://bit.example/q"}"#.into(),
            }
        } else {
            MockResponse::empty(401)
        }
    })
}

fn harness_for(server: &MockS3) -> Harness {
    let endpoint = format!("{}/shorten", server.endpoint());
    Harness::with_options(move |options| {
        options.shortener_endpoints = Some(Endpoints {
            bitly: endpoint.clone(),
            ..Endpoints::default()
        });
    })
}

#[test]
fn shortening_is_off_by_default() {
    let server = bitly();
    let h = harness_for(&server);
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(settings["shortener"]["provider"], "none");
    assert_eq!(h.code("shorten_url", json!({ "url": LONG })), "usage");
    assert!(server.requests().is_empty());
}

#[test]
fn shorten_with_a_stored_token() {
    let server = bitly();
    let h = harness_for(&server);
    h.set_setting("shortener", "provider", json!("bitly"));
    // No token yet: a usage error, nothing sent.
    assert_eq!(h.code("shorten_url", json!({ "url": LONG })), "usage");
    assert!(server.requests().is_empty());
    h.ok("set_shortener_token", json!({ "token": "tok" }));
    let settings = h.ok("get_settings", json!({}));
    assert!(settings["shortener"]["token_ref"].is_string());
    assert!(!settings.to_string().contains("tok\""));
    assert_eq!(
        h.ok("shorten_url", json!({ "url": LONG })),
        "https://bit.example/q"
    );
    assert_eq!(server.requests().len(), 1);
    // A wrong token is an auth error and the link is not a usable URL.
    h.ok("set_shortener_token", json!({ "token": "wrong" }));
    assert_eq!(h.code("shorten_url", json!({ "url": LONG })), "auth");
    assert_eq!(
        h.code("shorten_url", json!({ "url": "ftp://x/y" })),
        "usage"
    );
    h.ok("set_shortener_token", json!({ "token": null }));
    assert_eq!(
        h.ok("get_settings", json!({}))["shortener"]["token_ref"],
        serde_json::Value::Null
    );
}
