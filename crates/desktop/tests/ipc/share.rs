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

#[test]
fn a_link_can_use_another_service_than_the_settings_one() {
    let server = MockS3::start(|request| {
        if request.path.ends_with("/plain") {
            MockResponse {
                status: 200,
                headers: vec![],
                body: "https://tiny.example/z".into(),
            }
        } else if request.header("authorization") == Some("Bearer tok") {
            MockResponse {
                status: 200,
                headers: vec![],
                body: r#"{"link":"https://bit.example/q"}"#.into(),
            }
        } else {
            MockResponse::empty(401)
        }
    });
    let endpoint = server.endpoint().to_owned();
    let h = Harness::with_options(move |options| {
        options.shortener_endpoints = Some(Endpoints {
            bitly: format!("{endpoint}/shorten"),
            tinyurl: format!("{endpoint}/create"),
            tinyurl_plain: format!("{endpoint}/plain"),
        });
    });
    // Shortening stays off while the settings choose no service.
    assert_eq!(
        h.code("shorten_url", json!({ "url": LONG, "provider": "tinyurl" })),
        "usage"
    );
    assert!(server.requests().is_empty());

    h.set_setting("shortener", "provider", json!("bitly"));
    h.ok("set_shortener_token", json!({ "token": "tok" }));
    // The service of the settings, with or without the choice spelled out.
    assert_eq!(
        h.ok("shorten_url", json!({ "url": LONG, "provider": null })),
        "https://bit.example/q"
    );
    assert_eq!(
        h.ok("shorten_url", json!({ "url": LONG, "provider": "bitly" })),
        "https://bit.example/q"
    );
    // Another service: the Bitly token is not sent to it.
    assert_eq!(
        h.ok("shorten_url", json!({ "url": LONG, "provider": "tinyurl" })),
        "https://tiny.example/z"
    );
    let last = server.requests().last().unwrap().clone();
    assert!(last.header("authorization").is_none());
    assert!(last.path.ends_with("/plain"));
    // "none" as a choice means the service of the settings.
    assert_eq!(
        h.ok("shorten_url", json!({ "url": LONG, "provider": "none" })),
        "https://bit.example/q"
    );
}
