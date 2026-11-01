//! The crash reporter's panic hook is global to the process, so it is tested in this
//! separate test binary, in a single test, instead of next to the other unit tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::panic;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use bucketbridge_core::crash;

static PREVIOUS_HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn panic_hook_writes_local_reports_and_chains_the_previous_hook() {
    let dir = tempfile::tempdir().unwrap();
    let crash_dir = dir.path().join("crash");
    // Stands in for the default hook (and keeps the test output quiet).
    panic::set_hook(Box::new(|_| {
        PREVIOUS_HOOK_CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(crash::install(&crash_dir, "9.8.7-test"));
    assert!(!crash::install(dir.path().join("elsewhere"), "0.0.0"));

    // A panic with a static message on the test thread.
    let result = panic::catch_unwind(|| panic!("boom"));
    assert!(result.is_err());
    assert_eq!(PREVIOUS_HOOK_CALLS.load(Ordering::SeqCst), 1);
    let report = crash::last_report(&crash_dir)
        .unwrap()
        .expect("a crash report");
    let text = &report.contents;
    for expected in [
        "BucketBridge crash report",
        "has not been sent anywhere",
        "Version:  9.8.7-test",
        &format!("OS:       {}", std::env::consts::OS),
        &format!("Arch:     {}", std::env::consts::ARCH),
        &format!("Process:  {}", std::process::id()),
        "Location: ",
        "crash_hook.rs:",
        "Message:\nboom\n",
        "Backtrace:\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    let name = Path::new(&report.path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(name.starts_with("crash-") && name.ends_with(&format!("-{}.txt", std::process::id())));
    assert!((chrono::Utc::now() - report.created_at).num_seconds().abs() < 600);
    crash::dismiss(&crash_dir, &report.path).unwrap();
    assert!(crash::last_report(&crash_dir).unwrap().is_none());
    assert!(crash_dir.join(crash::DISMISSED_DIR).join(&name).is_file());

    // A formatted message in a named worker thread.
    let worker = thread::Builder::new()
        .name("bb-test-worker".to_owned())
        .spawn(|| {
            let reason = "worker failed";
            panic!("{reason} with code {}", 7);
        })
        .unwrap();
    assert!(worker.join().is_err());
    assert_eq!(PREVIOUS_HOOK_CALLS.load(Ordering::SeqCst), 2);
    let report = crash::last_report(&crash_dir).unwrap().unwrap();
    assert!(
        report.contents.contains("Thread:   bb-test-worker ("),
        "{}",
        report.contents
    );
    assert!(report.contents.contains("worker failed with code 7"));
    crash::dismiss(&crash_dir, &report.path).unwrap();

    // A payload that is not text.
    let result = panic::catch_unwind(|| panic::panic_any(7_u32));
    assert!(result.is_err());
    let report = crash::last_report(&crash_dir).unwrap().unwrap();
    assert!(report.contents.contains("(the panic payload is not text)"));

    // A panic loop stops producing reports at the cap; the previous hook still runs.
    let panics = 3 + 2 * crash::MAX_REPORTS_PER_PROCESS as usize;
    for _ in 3..panics {
        assert!(panic::catch_unwind(|| panic!("again")).is_err());
    }
    assert_eq!(PREVIOUS_HOOK_CALLS.load(Ordering::SeqCst), panics);
    let count = |folder: &Path| {
        fs::read_dir(folder)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_file())
            .count()
    };
    assert_eq!(
        count(&crash_dir) + count(&crash_dir.join(crash::DISMISSED_DIR)),
        crash::MAX_REPORTS_PER_PROCESS as usize
    );

    // Everything stayed inside the crash folder.
    let outside: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(outside, vec![std::ffi::OsString::from("crash")]);
}
