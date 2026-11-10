//! Cloning a bucket (`bucketbridge_core::bucketclone`) against a scripted
//! endpoint that keeps the configuration of its buckets in memory, and the
//! copy of the objects through the transfer queue against s3s-fs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bucketbridge_core::ErrorKind;
use bucketbridge_core::bucketclone::{
    ApplyState, ClonePreviewWarning, CloneRequest, CloneSetting, SettingState,
};
use bucketbridge_tests::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// The subresource a request is about, and the error code S3 answers a read
/// with when nothing is configured.
fn subresource(request: &RecordedRequest) -> Option<(&'static str, &'static str)> {
    [
        ("policy", "NoSuchBucketPolicy"),
        ("cors", "NoSuchCORSConfiguration"),
        ("lifecycle", "NoSuchLifecycleConfiguration"),
        ("versioning", ""),
        ("tagging", "NoSuchTagSet"),
        (
            "encryption",
            "ServerSideEncryptionConfigurationNotFoundError",
        ),
        ("website", "NoSuchWebsiteConfiguration"),
        ("publicAccessBlock", "NoSuchPublicAccessBlockConfiguration"),
    ]
    .into_iter()
    .find(|(name, _)| request.query_param(name).is_some())
}

struct Server {
    mock: MockS3,
    store: Arc<Mutex<HashMap<(String, String), String>>>,
}

impl Server {
    /// Buckets `src` (everything configured except the public access block)
    /// and whatever is created later. `deny` names a subresource whose
    /// writes are refused.
    fn start(existing: &[&str], deny: Option<&'static str>) -> Server {
        let store: Arc<Mutex<HashMap<(String, String), String>>> = Arc::default();
        let buckets: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(
            existing.iter().map(|b| (*b).to_owned()).collect(),
        ));
        {
            let mut s = store.lock().unwrap();
            let mut put = |sub: &str, body: &str| {
                s.insert(("src".to_owned(), sub.to_owned()), body.to_owned());
            };
            put(
                "policy",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::src/*"}]}"#,
            );
            put(
                "cors",
                &format!(
                    "<CORSConfiguration {NS}><CORSRule><AllowedMethod>GET</AllowedMethod><AllowedOrigin>*</AllowedOrigin></CORSRule></CORSConfiguration>"
                ),
            );
            put(
                "lifecycle",
                &format!(
                    "<LifecycleConfiguration {NS}><Rule><ID>expire</ID><Status>Enabled</Status><Filter><Prefix>tmp/</Prefix></Filter><Expiration><Days>30</Days></Expiration></Rule></LifecycleConfiguration>"
                ),
            );
            put(
                "versioning",
                &format!(
                    "<VersioningConfiguration {NS}><Status>Enabled</Status></VersioningConfiguration>"
                ),
            );
            put(
                "tagging",
                &format!(
                    "<Tagging {NS}><TagSet><Tag><Key>team</Key><Value>web</Value></Tag><Tag><Key>cost</Key><Value>42</Value></Tag></TagSet></Tagging>"
                ),
            );
            put(
                "encryption",
                &format!(
                    "<ServerSideEncryptionConfiguration {NS}><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>aws:kms</SSEAlgorithm><KMSMasterKeyID>alias/web</KMSMasterKeyID></ApplyServerSideEncryptionByDefault><BucketKeyEnabled>true</BucketKeyEnabled></Rule></ServerSideEncryptionConfiguration>"
                ),
            );
            put(
                "website",
                &format!(
                    "<WebsiteConfiguration {NS}><IndexDocument><Suffix>index.html</Suffix></IndexDocument><ErrorDocument><Key>404.html</Key></ErrorDocument></WebsiteConfiguration>"
                ),
            );
        }
        let state = store.clone();
        let mock = MockS3::start(move |request| {
            let bucket = request
                .path
                .trim_matches('/')
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            let Some((sub, absent)) = subresource(request) else {
                // CreateBucket
                if request.method == "PUT" && request.path.trim_matches('/') == bucket {
                    let mut known = buckets.lock().unwrap();
                    if known.contains(&bucket) {
                        return MockResponse::error(
                            409,
                            "BucketAlreadyOwnedByYou",
                            "already yours",
                        );
                    }
                    known.push(bucket);
                    return MockResponse::empty(200);
                }
                return MockResponse::error(400, "Unexpected", "unexpected request");
            };
            let key = (bucket.clone(), sub.to_owned());
            match request.method.as_str() {
                "GET" => match state.lock().unwrap().get(&key) {
                    Some(body) => MockResponse::xml(body.clone()),
                    None if sub == "versioning" => {
                        MockResponse::xml(format!("<VersioningConfiguration {NS}/>"))
                    }
                    None => MockResponse::error(404, absent, "not configured"),
                },
                "PUT" => {
                    if deny == Some(sub) {
                        return MockResponse::error(403, "AccessDenied", "Access Denied");
                    }
                    state.lock().unwrap().insert(key, request.body_text());
                    MockResponse::empty(200)
                }
                "DELETE" => {
                    state.lock().unwrap().remove(&key);
                    MockResponse::empty(204)
                }
                _ => MockResponse::error(400, "Unexpected", "unexpected request"),
            }
        });
        Server { mock, store }
    }

    fn stored(&self, bucket: &str, sub: &str) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .get(&(bucket.to_owned(), sub.to_owned()))
            .cloned()
    }
}

fn request(settings: &[CloneSetting]) -> CloneRequest {
    CloneRequest {
        source_bucket: "src".into(),
        new_bucket: "dst".into(),
        region: None,
        settings: settings.to_vec(),
    }
}

#[tokio::test]
async fn the_preview_lists_every_setting_of_the_source() {
    let server = Server::start(&["src"], None);
    let preview = server.mock.ops().preview_bucket_clone("src").await.unwrap();
    assert_eq!(preview.source_bucket, "src");
    assert_eq!(preview.settings.len(), 8);
    let find = |setting| {
        preview
            .settings
            .iter()
            .find(|s| s.setting == setting)
            .unwrap()
    };
    assert_eq!(find(CloneSetting::Policy).state, SettingState::Configured);
    assert_eq!(find(CloneSetting::Policy).count, Some(1));
    assert_eq!(find(CloneSetting::Cors).count, Some(1));
    assert_eq!(find(CloneSetting::Lifecycle).count, Some(1));
    assert_eq!(find(CloneSetting::Tags).count, Some(2));
    assert_eq!(
        find(CloneSetting::Versioning).detail.as_deref(),
        Some("enabled")
    );
    assert_eq!(
        find(CloneSetting::Encryption).detail.as_deref(),
        Some("aws:kms")
    );
    assert_eq!(
        find(CloneSetting::Website).detail.as_deref(),
        Some("index.html")
    );
    assert_eq!(
        find(CloneSetting::PublicAccessBlock).state,
        SettingState::NotConfigured
    );
    assert!(preview.warnings.contains(&ClonePreviewWarning::KmsKey));
    assert!(
        preview
            .warnings
            .contains(&ClonePreviewWarning::VersionsNotCopied)
    );
}

#[tokio::test]
async fn a_clone_copies_the_chosen_settings_and_renames_the_bucket_in_the_policy() {
    let server = Server::start(&["src"], None);
    let outcome = server
        .mock
        .ops()
        .create_bucket_clone(&request(&CloneSetting::ALL))
        .await
        .unwrap();
    assert_eq!(outcome.bucket, "dst");
    assert_eq!(outcome.results.len(), 8);
    let state = |setting| {
        outcome
            .results
            .iter()
            .find(|r| r.setting == setting)
            .unwrap()
            .state
    };
    for setting in [
        CloneSetting::Policy,
        CloneSetting::Cors,
        CloneSetting::Lifecycle,
        CloneSetting::Versioning,
        CloneSetting::Tags,
        CloneSetting::Encryption,
        CloneSetting::Website,
    ] {
        assert_eq!(state(setting), ApplyState::Applied, "{setting:?}");
    }
    // The source has no public access block: the clone clears the one a new
    // bucket starts with, so that a public policy can be set.
    assert_eq!(state(CloneSetting::PublicAccessBlock), ApplyState::Applied);

    let policy = server.stored("dst", "policy").unwrap();
    assert!(policy.contains("arn:aws:s3:::dst/*"), "{policy}");
    assert!(!policy.contains("arn:aws:s3:::src"), "{policy}");
    assert!(
        server
            .stored("dst", "versioning")
            .unwrap()
            .contains("Enabled")
    );
    let tags = server.stored("dst", "tagging").unwrap();
    assert!(tags.contains("<Key>team</Key>") && tags.contains("<Value>42</Value>"));
    let encryption = server.stored("dst", "encryption").unwrap();
    assert!(encryption.contains("aws:kms") && encryption.contains("alias/web"));
    assert!(encryption.contains("<BucketKeyEnabled>true</BucketKeyEnabled>"));
    assert!(
        server
            .stored("dst", "website")
            .unwrap()
            .contains("index.html")
    );
    assert!(
        server
            .stored("dst", "cors")
            .unwrap()
            .contains("<AllowedOrigin>*</AllowedOrigin>")
    );
    assert!(
        server
            .stored("dst", "lifecycle")
            .unwrap()
            .contains("<Days>30</Days>")
    );

    // The block is removed before the policy is set.
    let requests = server.mock.requests();
    let position = |method: &str, sub: &str| {
        requests
            .iter()
            .position(|r| r.method == method && r.query_param(sub).is_some())
            .unwrap()
    };
    assert!(position("DELETE", "publicAccessBlock") < position("PUT", "policy"));
    // The source is only read.
    assert!(
        requests
            .iter()
            .filter(|r| r.path.starts_with("/src"))
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn only_the_chosen_settings_are_copied() {
    let server = Server::start(&["src"], None);
    let outcome = server
        .mock
        .ops()
        .create_bucket_clone(&request(&[CloneSetting::Tags, CloneSetting::Cors]))
        .await
        .unwrap();
    assert_eq!(outcome.results.len(), 2);
    assert!(server.stored("dst", "tagging").is_some());
    assert!(server.stored("dst", "cors").is_some());
    assert!(server.stored("dst", "policy").is_none());
    assert!(server.stored("dst", "versioning").is_none());
    assert!(
        server
            .mock
            .requests()
            .iter()
            .all(|r| r.query_param("policy").is_none() && r.query_param("website").is_none())
    );
}

#[tokio::test]
async fn a_refused_setting_is_reported_and_the_others_still_apply() {
    let server = Server::start(&["src"], Some("policy"));
    let outcome = server
        .mock
        .ops()
        .create_bucket_clone(&request(&[
            CloneSetting::Policy,
            CloneSetting::Tags,
            CloneSetting::Cors,
        ]))
        .await
        .unwrap();
    let policy = outcome
        .results
        .iter()
        .find(|r| r.setting == CloneSetting::Policy)
        .unwrap();
    assert_eq!(policy.state, ApplyState::Failed);
    assert_eq!(policy.error.as_ref().unwrap().code, ErrorKind::Auth);
    assert!(server.stored("dst", "tagging").is_some());
    assert!(server.stored("dst", "cors").is_some());
}

#[tokio::test]
async fn an_existing_bucket_is_a_conflict_and_nothing_is_copied() {
    let server = Server::start(&["src", "dst"], None);
    let err = server
        .mock
        .ops()
        .create_bucket_clone(&request(&[CloneSetting::Tags]))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    assert!(server.stored("dst", "tagging").is_none());

    let same = CloneRequest {
        new_bucket: "src".into(),
        ..request(&[CloneSetting::Tags])
    };
    assert_eq!(
        server
            .mock
            .ops()
            .create_bucket_clone(&same)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Usage
    );
}

// ---- the copy of the objects ----

mod copy {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use bucketbridge_core::engine::{S3Provider, S3Runner, enqueue_clone_copy};
    use bucketbridge_core::model::TransferState;
    use bucketbridge_core::settings::{DataIntegritySettings, TransferSettings};
    use bucketbridge_core::transfer::{QueueOptions, TransferQueue};
    use bucketbridge_core::{Error, ErrorKind, S3};
    use bucketbridge_tests::TestS3;

    fn provider(s3: S3) -> S3Provider {
        Arc::new(move |_account: String| {
            let s3 = s3.clone();
            Box::pin(async move { Ok::<S3, Error>(s3) })
        })
    }

    async fn queue(s3: &TestS3) -> TransferQueue {
        let runner = S3Runner::new(provider(s3.ops().clone()), DataIntegritySettings::default());
        TransferQueue::open(
            TransferSettings {
                retry_attempts: 0,
                ..TransferSettings::default()
            },
            runner.runners(),
            QueueOptions::default(),
        )
        .await
        .unwrap()
    }

    async fn finished(queue: &TransferQueue, ids: &[String]) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let states: Vec<_> = ids.iter().map(|id| queue.get(id).unwrap().state).collect();
            if states.iter().all(|s| *s == TransferState::Completed) {
                return;
            }
            assert!(
                !states.contains(&TransferState::Failed),
                "a copy failed: {states:?}"
            );
            assert!(Instant::now() < deadline, "timed out: {states:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn filled(s3: &TestS3) -> String {
        let source = s3.new_bucket().await;
        for (key, body) in [
            ("top.txt", "top"),
            ("a/one.txt", "one"),
            ("a/b/two.txt", "two"),
            ("c/three.txt", "three"),
        ] {
            s3.put(&source, key, body).await;
        }
        source
    }

    #[tokio::test]
    async fn the_whole_bucket_is_copied_with_its_keys() {
        let s3 = TestS3::start().await;
        let source = filled(&s3).await;
        let new = s3.new_bucket().await;
        let queue = queue(&s3).await;
        let tasks = enqueue_clone_copy(&queue, s3.ops(), "acct", &source, &new, "")
            .await
            .unwrap();
        assert_eq!(tasks.len(), 4);
        let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        finished(&queue, &ids).await;
        for (key, body) in [
            ("top.txt", "top"),
            ("a/one.txt", "one"),
            ("a/b/two.txt", "two"),
            ("c/three.txt", "three"),
        ] {
            assert_eq!(s3.get(&new, key).await, body.as_bytes(), "{key}");
        }
    }

    #[tokio::test]
    async fn a_prefix_is_copied_with_its_full_keys() {
        let s3 = TestS3::start().await;
        let source = filled(&s3).await;
        let new = s3.new_bucket().await;
        let queue = queue(&s3).await;
        let tasks = enqueue_clone_copy(&queue, s3.ops(), "acct", &source, &new, "a/b/")
            .await
            .unwrap();
        assert_eq!(tasks.len(), 1);
        finished(&queue, &[tasks[0].id.clone()]).await;
        assert_eq!(s3.get(&new, "a/b/two.txt").await, b"two");
        assert!(!s3.exists(&new, "a/one.txt").await);
        assert!(!s3.exists(&new, "top.txt").await);

        let tasks = enqueue_clone_copy(&queue, s3.ops(), "acct", &source, &new, "c/")
            .await
            .unwrap();
        finished(&queue, &[tasks[0].id.clone()]).await;
        assert_eq!(s3.get(&new, "c/three.txt").await, b"three");
    }

    #[tokio::test]
    async fn an_empty_bucket_has_nothing_to_copy_and_bad_input_is_refused() {
        let s3 = TestS3::start().await;
        let source = s3.new_bucket().await;
        let new = s3.new_bucket().await;
        let queue = queue(&s3).await;
        assert!(
            enqueue_clone_copy(&queue, s3.ops(), "acct", &source, &new, "")
                .await
                .unwrap()
                .is_empty()
        );
        for (from, to, prefix) in [
            (source.as_str(), source.as_str(), ""),
            (source.as_str(), new.as_str(), "no-slash"),
        ] {
            assert_eq!(
                enqueue_clone_copy(&queue, s3.ops(), "acct", from, to, prefix)
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::Usage
            );
        }
    }
}
