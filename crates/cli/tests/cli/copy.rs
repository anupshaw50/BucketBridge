//! `bb cp` and `bb mv` between S3 locations (server-side copies).

use bucketbridge_cli::i18n::{tr, trf};

use super::*;

/// A source bucket with a small photo tree and an empty target bucket.
async fn photos(env: &Env) -> (String, String) {
    let src = env.s3.new_bucket().await;
    for key in [
        "photos/2024/a.jpg",
        "photos/2024/c.png",
        "photos/2024/x/b.jpg",
        "photos/2024/x/d.png",
        "photos/2025/e.jpg",
    ] {
        env.s3.put(&src, key, key.as_bytes().to_vec()).await;
    }
    let dst = env.s3.new_bucket().await;
    (src, dst)
}

#[tokio::test]
async fn cp_copies_one_object_to_an_exact_key_or_into_a_prefix() {
    let env = Env::start().await;
    let (src, dst) = photos(&env).await;
    let out = env.run(&[
        "cp",
        &format!("s3://{src}/photos/2024/a.jpg"),
        &format!("s3://{dst}/renamed.jpg"),
    ]);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf("copied {source} to {destination}", &[
            ("source", &format!("s3://{src}/photos/2024/a.jpg")),
            ("destination", &format!("s3://{dst}/renamed.jpg")),
        ])
    );
    assert_eq!(env.s3.get(&dst, "renamed.jpg").await, b"photos/2024/a.jpg");

    env.run(&[
        "cp",
        &format!("s3://{src}/photos/2024/a.jpg"),
        &format!("s3://{dst}/backup/"),
    ])
    .expect_code(0);
    env.run(&[
        "cp",
        &format!("s3://{src}/photos/2025/e.jpg"),
        &format!("s3://{dst}"),
    ])
    .expect_code(0);
    assert_eq!(
        env.keys(&dst).await,
        ["backup/a.jpg", "e.jpg", "renamed.jpg"]
    );
    // The source is untouched.
    assert_eq!(env.keys(&src).await.len(), 5);
}

#[tokio::test]
async fn recursive_copy_puts_the_selected_folder_under_the_destination() {
    let env = Env::start().await;
    let (src, dst) = photos(&env).await;
    let out = env.run(&[
        "cp",
        "-r",
        "--json",
        &format!("s3://{src}/photos/2024/"),
        &format!("s3://{dst}/backup/"),
    ]);
    out.expect_code(0);
    assert_eq!(
        env.keys(&dst).await,
        [
            "backup/2024/a.jpg",
            "backup/2024/c.png",
            "backup/2024/x/b.jpg",
            "backup/2024/x/d.png"
        ]
    );
    let records = out.records();
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[0]["source"],
        format!("s3://{src}/photos/2024/a.jpg")
    );
    assert_eq!(
        records[0]["destination"],
        format!("s3://{dst}/backup/2024/a.jpg")
    );
    assert_eq!(records[0]["size"], "photos/2024/a.jpg".len());
    assert!(
        records
            .iter()
            .all(|r| r["copied"] == true && r["source_deleted"] == false && r["error"].is_null())
    );
    assert_eq!(
        env.s3.get(&dst, "backup/2024/x/b.jpg").await,
        b"photos/2024/x/b.jpg"
    );
}

#[tokio::test]
async fn include_and_exclude_apply_in_order_and_the_last_match_wins() {
    let env = Env::start().await;
    let (src, _) = photos(&env).await;
    let source = format!("s3://{src}/photos/2024/");
    let cases: [(&[&str], &[&str]); 5] = [
        // `*` stays within one folder level, `**` crosses levels.
        (
            &["--exclude", "*", "--include", "*.jpg"],
            &["2024/a.jpg", "2024/x/b.jpg", "2024/x/d.png"],
        ),
        (&["--exclude", "**", "--include", "*.jpg"], &["2024/a.jpg"]),
        (
            &["--exclude", "**", "--include", "**/*.jpg"],
            &["2024/a.jpg", "2024/x/b.jpg"],
        ),
        // The later rule wins.
        (&["--include", "*.jpg", "--exclude", "**"], &[]),
        (&["--exclude", "**/*.png"], &["2024/a.jpg", "2024/x/b.jpg"]),
    ];
    for (index, (rules, expected)) in cases.iter().enumerate() {
        let dst = format!("s3://{src}/out-{index}/");
        let mut args = vec!["cp", "-r", source.as_str(), dst.as_str()];
        args.extend_from_slice(rules);
        env.run(&args).expect_code(0);
        let copied: Vec<String> = env
            .s3
            .ops()
            .list_all(&src, &format!("out-{index}/"))
            .await
            .unwrap()
            .into_iter()
            .map(|e| {
                e.key
                    .trim_start_matches(&format!("out-{index}/"))
                    .to_owned()
            })
            .collect();
        assert_eq!(copied, *expected, "rules {rules:?}");
    }
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let env = Env::start().await;
    let (src, dst) = photos(&env).await;
    let out = env.run(&[
        "cp",
        "-r",
        "--dry-run",
        "--json",
        &format!("s3://{src}/photos/"),
        &format!("s3://{dst}/"),
    ]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 5);
    assert!(
        records
            .iter()
            .all(|r| r["dry_run"] == true && r["copied"] == false)
    );
    assert_eq!(
        records[0]["destination"],
        format!("s3://{dst}/photos/2024/a.jpg")
    );
    let out = env.run(&[
        "mv",
        "--dry-run",
        &format!("s3://{src}/photos/2025/e.jpg"),
        &format!("s3://{dst}/e.jpg"),
    ]);
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf("would move {source} to {destination}", &[
            ("source", &format!("s3://{src}/photos/2025/e.jpg")),
            ("destination", &format!("s3://{dst}/e.jpg")),
        ])
    );
    assert!(env.keys(&dst).await.is_empty());
    assert_eq!(env.keys(&src).await.len(), 5);
    // A dry run still reports a missing source.
    env.run(&[
        "cp",
        "--dry-run",
        &format!("s3://{src}/nope"),
        &format!("s3://{dst}/nope"),
    ])
    .expect_code(2);
}

#[tokio::test]
async fn mv_deletes_each_source_after_copying_it() {
    let env = Env::start().await;
    let (src, dst) = photos(&env).await;
    let out = env.run(&[
        "mv",
        "--json",
        &format!("s3://{src}/photos/2025/e.jpg"),
        &format!("s3://{dst}/"),
    ]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records[0]["copied"], true);
    assert_eq!(records[0]["source_deleted"], true);
    assert_eq!(env.keys(&dst).await, ["e.jpg"]);

    env.run(&[
        "mv",
        "-r",
        &format!("s3://{src}/photos/2024/x/"),
        &format!("s3://{dst}/moved/"),
    ])
    .expect_code(0);
    assert_eq!(
        env.keys(&dst).await,
        ["e.jpg", "moved/x/b.jpg", "moved/x/d.png"]
    );
    assert_eq!(
        env.keys(&src).await,
        ["photos/2024/a.jpg", "photos/2024/c.png"]
    );
}

#[tokio::test]
async fn mv_keeps_the_source_when_the_copy_fails() {
    let env = Env::start().await;
    let (src, _) = photos(&env).await;
    let out = env.run(&[
        "mv",
        &format!("s3://{src}/photos/2025/e.jpg"),
        "s3://no-such-bucket/e.jpg",
    ]);
    out.expect_code(2);
    assert!(env.s3.exists(&src, "photos/2025/e.jpg").await);

    let out = env.run(&[
        "mv",
        "-r",
        "--json",
        &format!("s3://{src}/photos/2024/"),
        "s3://no-such-bucket/",
    ]);
    out.expect_code(2);
    let records = out.records();
    assert_eq!(records.len(), 4);
    assert!(
        records
            .iter()
            .all(|r| r["copied"] == false && r["error"]["code"] == "not_found")
    );
    assert_eq!(
        out.error_record()["message"],
        batch_summary(4, 4, "objects", "could not be moved")
    );
    assert_eq!(env.keys(&src).await.len(), 5);
}

#[tokio::test]
async fn invalid_copies_are_usage_errors() {
    let env = Env::start().await;
    let (src, dst) = photos(&env).await;
    // The expectations are precomputed through the same templates as the
    // code, so they hold in either language.
    let cases: Vec<(Vec<String>, String)> = vec![
        (
            vec!["cp".into(), "photo.jpg".into(), "./e.jpg".into()],
            tr("both locations are local"),
        ),
        (
            vec![
                "cp".into(),
                format!("s3://{src}/photos/"),
                format!("s3://{dst}/"),
            ],
            "--recursive".to_owned(),
        ),
        (
            vec![
                "cp".into(),
                "-r".into(),
                format!("s3://{src}/photos"),
                format!("s3://{dst}/"),
            ],
            "--recursive".to_owned(),
        ),
        (
            vec![
                "cp".into(),
                "-r".into(),
                format!("s3://{src}/photos/"),
                format!("s3://{src}/photos/again/"),
            ],
            trf(
                "the destination {destination} is inside the source {source}",
                &[
                    ("destination", &format!("s3://{src}/photos/again/")),
                    ("source", &format!("s3://{src}/photos/")),
                ],
            ),
        ),
        (
            vec![
                "mv".into(),
                "-r".into(),
                format!("s3://{src}/photos/2024/"),
                format!("s3://{src}/photos/"),
            ],
            trf(
                "copying {source} to {destination} would put every object onto itself",
                &[
                    ("source", &format!("s3://{src}/photos/2024/")),
                    ("destination", &format!("s3://{src}/photos/")),
                ],
            ),
        ),
        (
            vec![
                "mv".into(),
                format!("s3://{src}/photos/2025/e.jpg"),
                format!("s3://{src}/photos/2025/"),
            ],
            trf(
                "cannot move {source} onto itself",
                &[("source", &format!("s3://{src}/photos/2025/e.jpg"))],
            ),
        ),
        (
            vec![
                "cp".into(),
                format!("s3://{src}/photos/2025/e.jpg"),
                format!("s3://{dst}/"),
                "--exclude".into(),
                "*".into(),
            ],
            "--recursive".to_owned(),
        ),
    ];
    for (args, expected) in cases {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = env.run(&args);
        out.expect_code(1);
        assert!(out.stderr.contains(expected.as_str()), "{args:?}: {}", out.stderr);
    }
    assert!(env.keys(&dst).await.is_empty());
    assert_eq!(env.keys(&src).await.len(), 5);
}
