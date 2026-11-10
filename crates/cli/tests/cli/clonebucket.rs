//! `bb clone-bucket`: the new bucket, the copy of the objects with their keys
//! unchanged, and the checks that need no server. (s3s-fs implements none of the
//! bucket settings, which the integration tests of the core cover with a
//! scripted server.)

use bucketbridge_cli::i18n::trf;

use super::*;

async fn filled(env: &Env) -> String {
    let src = env.s3.new_bucket().await;
    for (key, body) in [
        ("top.txt", "top"),
        ("a/one.txt", "one"),
        ("a/b/two.txt", "two"),
        ("c/three.txt", "three"),
    ] {
        env.s3.put(&src, key, body).await;
    }
    src
}

#[tokio::test]
async fn clone_copies_every_object_with_its_key() {
    let env = Env::start().await;
    let src = filled(&env).await;
    let new = env.s3.unique_bucket();
    let out = env.run(&["clone-bucket", &src, &new, "--settings", "none"]);
    out.expect_code(0);
    assert!(
        out.stdout
            .contains(&trf("created bucket {bucket}", &[("bucket", &new)])),
        "{}",
        out.stdout
    );
    assert_eq!(
        env.keys(&new).await,
        ["a/b/two.txt", "a/one.txt", "c/three.txt", "top.txt"]
    );
    assert_eq!(env.s3.get(&new, "a/b/two.txt").await, b"two");
    // The source is untouched.
    assert_eq!(env.keys(&src).await.len(), 4);
}

#[tokio::test]
async fn clone_with_a_prefix_keeps_the_full_keys_and_can_skip_the_objects() {
    let env = Env::start().await;
    let src = filled(&env).await;
    let only = env.s3.unique_bucket();
    env.run(&[
        "clone-bucket",
        &src,
        &only,
        "--settings",
        "none",
        "--prefix",
        "a/b/",
    ])
    .expect_code(0);
    assert_eq!(env.keys(&only).await, ["a/b/two.txt"]);

    let empty = env.s3.unique_bucket();
    env.run(&[
        "clone-bucket",
        &src,
        &empty,
        "--settings",
        "none",
        "--no-objects",
    ])
    .expect_code(0);
    assert!(env.keys(&empty).await.is_empty());
}

#[tokio::test]
async fn clone_refuses_bad_input_and_an_existing_bucket() {
    let env = Env::start().await;
    let src = filled(&env).await;
    let other = env.s3.new_bucket().await;
    // The same name, a prefix without a slash and an unknown setting are usage errors.
    env.run(&["clone-bucket", &src, &src]).expect_code(1);
    env.run(&["clone-bucket", &src, "new-bucket-x", "--prefix", "a"])
        .expect_code(1);
    env.run(&[
        "clone-bucket",
        &src,
        "new-bucket-x",
        "--settings",
        "nonsense",
    ])
    .expect_code(1);
    // A bucket that exists is a conflict; nothing is copied into it.
    env.run(&["clone-bucket", &src, &other, "--settings", "none"])
        .expect_code(2);
    assert!(env.keys(&other).await.is_empty());
}

#[tokio::test]
async fn a_dry_run_reports_the_settings_and_creates_nothing() {
    let env = Env::start().await;
    let src = filled(&env).await;
    let new = env.s3.unique_bucket();
    let out = env.run(&["clone-bucket", &src, &new, "--dry-run", "--json"]);
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 8);
    assert!(
        records
            .iter()
            .all(|r| r["dry_run"] == true && r["bucket"] == new.as_str())
    );
    let buckets = env.s3.ops().list_buckets().await.unwrap();
    assert!(buckets.iter().all(|bucket| bucket.name != new));
}
