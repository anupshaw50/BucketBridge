//! Everyday-use commands: pending links, quit and the new settings.

use serde_json::json;

use crate::harness::*;

#[test]
fn no_link_is_pending_by_default() {
    let h = Harness::new();
    assert_eq!(h.ok("take_pending_link", json!({})), json!(null));
}

#[test]
fn quit_app_needs_a_known_mode() {
    let h = Harness::new();
    assert!(h.invoke("quit_app", json!({ "mode": "later" })).is_err());
    assert!(h.invoke("quit_app", json!({})).is_err());
}

#[test]
fn keep_awake_and_quit_settings_have_defaults_and_are_saved() {
    let h = Harness::new();
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(settings["general"]["keep_awake"], true);
    assert_eq!(settings["general"]["quit_with_tasks"], "ask");

    h.set_setting("general", "keep_awake", json!(false));
    h.set_setting("general", "quit_with_tasks", json!("wait"));
    let saved = h.ok("get_settings", json!({}));
    assert_eq!(saved["general"]["keep_awake"], false);
    assert_eq!(saved["general"]["quit_with_tasks"], "wait");

    let mut bad = saved.clone();
    bad["general"]["quit_with_tasks"] = json!("sometimes");
    assert!(
        h.invoke("save_settings", json!({ "settings": bad }))
            .is_err()
    );
    assert_eq!(h.ok("get_settings", json!({})), saved);
}
