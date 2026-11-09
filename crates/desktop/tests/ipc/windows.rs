//! `open_new_window`: extra main windows share the app, get their own label
//! and can start at a location.

use serde_json::json;

use crate::harness::*;

#[test]
fn new_windows_get_their_own_labels() {
    let h = Harness::new();
    assert_eq!(h.window_labels(), ["main"]);
    let first = h.ok("open_new_window", json!({ "location": null }));
    let second = h.ok(
        "open_new_window",
        json!({ "location": { "account_id": "a", "bucket": "b", "prefix": "dir/x y/" } }),
    );
    let (first, second) = (first.as_str().unwrap(), second.as_str().unwrap());
    assert!(first.starts_with("main-") && second.starts_with("main-"));
    assert_ne!(first, second);
    let mut expected = vec!["main".to_owned(), first.to_owned(), second.to_owned()];
    expected.sort();
    assert_eq!(h.window_labels(), expected);
    // The commands of the shared core work the same from any window.
    assert_eq!(h.ok("list_bookmarks", json!({})), json!([]));
}

#[test]
fn a_location_needs_an_account_and_a_bucket() {
    let h = Harness::new();
    assert!(
        h.invoke("open_new_window", json!({ "location": { "bucket": "b" } }))
            .is_err()
    );
    assert_eq!(h.window_labels(), ["main"]);
}

#[test]
fn a_relayed_drag_is_checked_and_reaches_no_window_when_none_is_under_the_cursor() {
    let h = Harness::new();
    h.ok("open_new_window", json!({ "location": null }));
    let items = json!({ "account_id": "a", "bucket": "b", "keys": ["x.txt", "dir/"] });
    let none = json!({ "account_id": "a", "bucket": "b", "keys": [] });
    assert_eq!(
        h.code(
            "relay_drag",
            json!({ "phase": "spin", "items": items, "move_items": false })
        ),
        "usage"
    );
    for phase in ["over", "drop"] {
        for bad in [json!(null), none.clone()] {
            assert_eq!(
                h.code(
                    "relay_drag",
                    json!({ "phase": phase, "items": bad, "move_items": false })
                ),
                "usage"
            );
        }
    }
    // The test runtime's windows have no screen position, so nothing is under the cursor.
    for phase in ["over", "drop", "leave"] {
        let reached = h.ok(
            "relay_drag",
            json!({ "phase": phase, "items": items, "move_items": true }),
        );
        assert_eq!(reached, json!(false), "{phase}");
    }
}
