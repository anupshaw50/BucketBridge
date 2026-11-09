//! `bb account move|folder|export|import|set-secret`: folders, order and the
//! account file, run in-process with an in-memory secret store.

use super::*;

const SECRET: &str = "example-secret-access-key";

async fn add(bb: &InProcess, label: &str, key: &str) {
    let out = bb
        .run_with(
            &[
                "account",
                "add",
                "--label",
                label,
                "--access-key-id",
                key,
                "--region",
                "eu-west-1",
                "--secret-stdin",
            ],
            &[],
            format!("{SECRET}\nexample-session-token\n").as_bytes(),
            &Terminal::default(),
        )
        .await;
    out.expect_code(0);
}

fn labels(out: &Output) -> Vec<String> {
    out.records()
        .iter()
        .map(|record| record["label"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn folders_and_order() {
    let bb = InProcess::default();
    add(&bb, "A", "AKIDAAAA").await;
    add(&bb, "B", "AKIDBBBB").await;
    add(&bb, "C", "AKIDCCCC").await;

    // Put C in a new folder; folders are listed first.
    bb.run(&["account", "move", "C", "--folder", "Work"])
        .await
        .expect_code(0);
    let out = bb.run(&["account", "list", "--json"]).await;
    out.expect_code(0);
    assert_eq!(labels(&out), ["C", "A", "B"]);
    assert_eq!(out.records()[0]["folder"], "Work");
    assert!(out.records()[1]["folder"].is_null());

    // Reorder the accounts in no folder.
    bb.run(&["account", "move", "B", "--before", "A"])
        .await
        .expect_code(0);
    let out = bb.run(&["account", "list", "--json"]).await;
    assert_eq!(labels(&out), ["C", "B", "A"]);

    // Folder commands.
    let out = bb.run(&["account", "folder", "list", "--json"]).await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["name"], "Work");
    assert_eq!(out.records()[0]["accounts"][0], "C");
    bb.run(&["account", "folder", "add", "Home"])
        .await
        .expect_code(0);
    bb.run(&["account", "folder", "add", "work"])
        .await
        .expect_code(2);
    bb.run(&["account", "folder", "rename", "Home", "House"])
        .await
        .expect_code(0);
    bb.run(&["account", "folder", "remove", "Work"])
        .await
        .expect_code(0);
    let out = bb.run(&["account", "list", "--json"]).await;
    assert!(out.records().iter().all(|r| r["folder"].is_null()));
    let out = bb.run(&["account", "folder", "list", "--json"]).await;
    assert_eq!(labels_of_names(&out), ["House"]);

    // Bad input.
    bb.run(&["account", "move", "nobody", "--first"])
        .await
        .expect_code(2);
    bb.run(&["account", "move", "A"]).await.expect_code(1);
}

fn labels_of_names(out: &Output) -> Vec<String> {
    out.records()
        .iter()
        .map(|record| record["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn export_has_no_secret_and_import_reports_duplicates() {
    let bb = InProcess::default();
    add(&bb, "A", "AKIDAAAA").await;
    add(&bb, "B", "AKIDBBBB").await;
    bb.run(&["account", "move", "B", "--folder", "Work"])
        .await
        .expect_code(0);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("accounts.json");
    let file_arg = file.to_str().unwrap();
    bb.run(&["account", "export", file_arg])
        .await
        .expect_code(0);
    let text = std::fs::read_to_string(&file).unwrap();
    for forbidden in [
        SECRET,
        "example-session-token",
        "secret_ref",
        "session_token",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} is in the file");
    }
    assert!(text.contains("AKIDAAAA") && text.contains("\"Work\""));

    // A fresh installation imports both: they need a secret, and B is in its folder.
    let other = InProcess::default();
    let out = other.run(&["account", "import", file_arg, "--json"]).await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[0]["kind"], "imported");
    assert_eq!(records[0]["needs_secret"], true);
    assert_eq!(records.last().unwrap()["imported"], 2);
    let out = other.run(&["account", "list", "--json"]).await;
    assert_eq!(labels(&out), ["B", "A"]);
    assert!(out.records().iter().all(|r| r["needs_secret"] == true));
    assert_eq!(out.records()[0]["folder"], "Work");

    // An imported account cannot be used until it has a secret.
    let out = other.run(&["--account", "A", "ls"]).await;
    out.expect_code(1);
    let stored = other
        .run_with(
            &["account", "set-secret", "A", "--secret-stdin"],
            &[],
            format!("{SECRET}\n").as_bytes(),
            &Terminal::default(),
        )
        .await;
    stored.expect_code(0);
    let out = other.run(&["account", "list", "--json"]).await;
    let a = out
        .records()
        .into_iter()
        .find(|r| r["label"] == "A")
        .unwrap();
    assert_eq!(a["needs_secret"], false);

    // The same file again: duplicates are reported, nothing changes.
    let out = other.run(&["account", "import", file_arg, "--json"]).await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "duplicate").count(),
        2
    );
    assert_eq!(records.last().unwrap()["imported"], 0);
    assert_eq!(
        other
            .run(&["account", "list", "--json"])
            .await
            .records()
            .len(),
        2
    );

    // A label taken by another account: skipped, or renamed on request.
    let third = InProcess::default();
    add(&third, "A", "AKIDOTHER").await;
    let out = third.run(&["account", "import", file_arg, "--json"]).await;
    let records = out.records();
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "duplicate" && r["reason"] == "label")
    );
    let out = third
        .run(&[
            "account",
            "import",
            file_arg,
            "--rename-duplicates",
            "--json",
        ])
        .await;
    out.expect_code(0);
    let out = third.run(&["account", "list", "--json"]).await;
    assert!(labels(&out).contains(&"A (2)".to_owned()));

    // Not an account file.
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{\"hello\": 1}").unwrap();
    third
        .run(&["account", "import", bad.to_str().unwrap()])
        .await
        .expect_code(1);
}
