//! Settings, event log, crash report and app commands, and `log://entry`.

use std::fs;

use bucketbridge_desktop::LOG_ENTRY;
use serde_json::{Value, json};

use crate::harness::*;

const REPORT_NAME: &str = "crash-20261029T081500.123456Z-4242.txt";

#[test]
fn settings_are_validated_and_applied() {
    let h = Harness::new();
    let settings = h.ok("get_settings", json!({}));
    assert_eq!(
        keys(&settings),
        [
            "advanced",
            "data_integrity",
            "general",
            "interface",
            "logging",
            "proxy",
            "shortener",
            "transfer",
            "upload_rules"
        ]
    );
    assert_eq!(settings["advanced"]["list_page_size"], 1000);
    assert_eq!(settings["logging"]["level"], "information");

    // Every problem is reported at once and nothing is saved.
    let mut invalid = settings.clone();
    invalid["advanced"]["list_page_size"] = json!(0);
    invalid["transfer"]["max_concurrent_tasks"] = json!(0);
    let (code, message) = h.error("save_settings", json!({ "settings": invalid }));
    assert_eq!(code, "usage");
    assert!(message.contains("advanced.list_page_size"), "{message}");
    assert!(
        message.contains("transfer.max_concurrent_tasks"),
        "{message}"
    );
    assert_eq!(h.ok("get_settings", json!({})), settings);

    // The log level applies at once.
    assert!(h.core.log().information("test", "recorded").is_some());
    h.set_setting("logging", "level", json!("error"));
    assert_eq!(h.ok("get_settings", json!({}))["logging"]["level"], "error");
    assert!(h.core.log().information("test", "dropped").is_none());
    assert!(h.core.log().warning("test", "dropped").is_none());
    assert!(h.core.log().error("test", "recorded").is_some());

    // The proxy password is stored and removed through the secret store;
    // settings only ever hold its reference.
    assert_eq!(
        h.ok("set_proxy_password", json!({ "password": "proxy secret" })),
        Value::Null
    );
    let with_password = h.ok("get_settings", json!({}));
    let reference = with_password["proxy"]["password_ref"].as_str().unwrap();
    assert!(!reference.contains("proxy secret"));
    assert_eq!(
        h.ok("set_proxy_password", json!({ "password": null })),
        Value::Null
    );
    assert_eq!(
        h.ok("get_settings", json!({}))["proxy"]["password_ref"],
        Value::Null
    );
}

#[test]
fn log_query_export_clear_and_events() {
    let h = Harness::new();
    let entries = h.events(LOG_ENTRY);
    let log = h.core.log();
    log.information("sync", "plan analyzed");
    log.warning("s3", "slow response");
    log.error("s3", "Access Denied");

    let event = next(&entries);
    assert_eq!(keys(&event), ["id", "level", "message", "source", "ts"]);
    assert_eq!(event["message"], "plan analyzed");
    assert_eq!(event["level"], "information");
    assert_eq!(next(&entries)["level"], "warning");
    assert_eq!(next(&entries)["level"], "error");

    let query = |levels: Value, text: Value, limit: Value| {
        h.ok(
            "get_log",
            json!({ "levels": levels, "text": text, "limit": limit }),
        )
    };
    let messages = |entries: Value| -> Vec<String> {
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["message"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        messages(query(Value::Null, Value::Null, Value::Null)),
        ["plan analyzed", "slow response", "Access Denied"]
    );
    assert_eq!(
        messages(query(json!(["error", "warning"]), Value::Null, Value::Null)),
        ["slow response", "Access Denied"]
    );
    assert_eq!(
        messages(query(json!([]), Value::Null, Value::Null)),
        Vec::<String>::new()
    );
    // Case-insensitive on message and source.
    assert_eq!(
        messages(query(Value::Null, json!("S3"), Value::Null)),
        ["slow response", "Access Denied"]
    );
    assert_eq!(
        messages(query(Value::Null, json!("analyzed"), Value::Null)),
        ["plan analyzed"]
    );
    // `limit` keeps the newest.
    assert_eq!(
        messages(query(Value::Null, Value::Null, json!(1))),
        ["Access Denied"]
    );

    let csv = h.dir.path().join("log.csv");
    assert_eq!(
        h.ok(
            "export_log",
            json!({ "path": csv.to_str().unwrap(), "format": "csv" })
        ),
        Value::Null
    );
    let text = fs::read_to_string(&csv).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("id,ts,level,source,message"));
    assert_eq!(lines.count(), 3);

    let json_file = h.dir.path().join("log.json");
    h.ok(
        "export_log",
        json!({ "path": json_file.to_str().unwrap(), "format": "json" }),
    );
    let exported: Value = serde_json::from_str(&fs::read_to_string(&json_file).unwrap()).unwrap();
    assert_eq!(exported.as_array().unwrap().len(), 3);
    assert_eq!(exported[2]["message"], "Access Denied");

    assert_eq!(
        h.code(
            "export_log",
            json!({ "path": "relative/log.csv", "format": "csv" })
        ),
        "usage"
    );

    assert_eq!(h.ok("clear_log", json!({})), Value::Null);
    assert_eq!(query(Value::Null, Value::Null, Value::Null), json!([]));
}

#[test]
fn crash_reports_are_read_and_dismissed() {
    let h = Harness::new();
    let crash_dir = h.core.dirs().crash_dir.clone();
    assert_eq!(h.ok("last_crash_report", json!({})), Value::Null);

    fs::write(crash_dir.join(REPORT_NAME), "thread 'main' panicked").unwrap();
    let report = h.ok("last_crash_report", json!({}));
    assert_eq!(keys(&report), ["contents", "created_at", "path"]);
    assert_eq!(report["contents"], "thread 'main' panicked");
    assert!(
        report["created_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-10-29T08:15:00"),
        "{report}"
    );
    let path = report["path"].as_str().unwrap().to_owned();

    // The path is untrusted: only reports directly in the crash folder.
    let outside = h.dir.path().join(REPORT_NAME);
    fs::write(&outside, "not a report of this app").unwrap();
    for path in [
        outside.to_str().unwrap().to_owned(),
        REPORT_NAME.to_owned(),
        crash_dir.to_str().unwrap().to_owned(),
        crash_dir.join("notes.txt").to_str().unwrap().to_owned(),
    ] {
        assert_eq!(
            h.code("dismiss_crash_report", json!({ "path": path })),
            "usage",
            "{path}"
        );
    }
    assert!(outside.exists());

    assert_eq!(
        h.ok("dismiss_crash_report", json!({ "path": path })),
        Value::Null
    );
    assert_eq!(h.ok("last_crash_report", json!({})), Value::Null);
    assert!(crash_dir.join("dismissed").join(REPORT_NAME).exists());
    assert_eq!(
        h.code("dismiss_crash_report", json!({ "path": path })),
        "not_found"
    );
}

#[test]
fn list_regions_returns_the_region_table_with_partitions() {
    let h = Harness::new();
    let regions = h.ok("list_regions", json!({}));
    let regions = regions.as_array().unwrap();
    assert!(regions.len() > 30);
    let cn = regions.iter().find(|r| r["id"] == "cn-north-1").unwrap();
    assert_eq!(cn["partition"], "aws-cn");
    assert_eq!(cn["dns_suffix"], "amazonaws.com.cn");
    let gov = regions.iter().find(|r| r["id"] == "us-gov-west-1").unwrap();
    assert_eq!(gov["partition"], "aws-us-gov");
}

#[test]
fn app_info_names_the_app_and_its_folders() {
    let h = Harness::new();
    let dirs = h.core.dirs();
    assert_eq!(
        h.ok("app_info", json!({})),
        json!({
            "name": "BucketBridge",
            "version": env!("CARGO_PKG_VERSION"),
            "license": "GPL-3.0-only",
            "repository_url": "https://github.com/anupshaw50/BucketBridge",
            "config_dir": dirs.config_dir.to_str().unwrap(),
            "data_dir": dirs.data_dir.to_str().unwrap(),
            "log_dir": dirs.log_dir.to_str().unwrap(),
        })
    );
}
