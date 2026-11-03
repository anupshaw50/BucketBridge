//! Integration tests of edit in place (`bucketbridge_core::edit`): saves are
//! uploaded with the object's headers, conflicts block overwrites, and the
//! copies are cleaned up. Real server (s3s-fs by default); the scripted
//! `MockS3` covers servers without conditional writes and the race where the
//! object changes between the check and the upload.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bucketbridge_core::aws_sdk_s3::primitives::ByteStream;
use bucketbridge_core::edit::{
    EditManager, EditOptions, EditResolution, EditSession, EditState, StartEdit,
};
use bucketbridge_core::engine::{S3Provider, TransferOptions};
use bucketbridge_core::{ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, TestS3};

const DEBOUNCE: Duration = Duration::from_millis(150);
const WAIT: Duration = Duration::from_secs(30);

fn manager(s3: &S3, root: &Path, debounce: Duration) -> EditManager {
    let s3 = s3.clone();
    let provider: S3Provider = Arc::new(move |_account: String| {
        let s3 = s3.clone();
        Box::pin(async move { Ok(s3) })
    });
    EditManager::new(root.join("edit"), provider, EditOptions { debounce })
}

fn request(bucket: &str, key: &str) -> StartEdit {
    StartEdit {
        account_id: "acct".to_owned(),
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        transfer: TransferOptions::default(),
    }
}

/// Polls until `done` holds for the session.
async fn wait_for(
    manager: &EditManager,
    id: &str,
    what: &str,
    done: impl Fn(&EditSession) -> bool,
) -> EditSession {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(session) = manager.get(id)
            && done(&session)
        {
            return session;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            manager.get(id)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_uploads(manager: &EditManager, id: &str, uploads: u32) -> EditSession {
    wait_for(manager, id, "an upload", |s| {
        s.uploads >= uploads && s.state == EditState::Uploaded
    })
    .await
}

async fn wait_state(manager: &EditManager, id: &str, state: EditState) -> EditSession {
    wait_for(manager, id, "a state", |s| s.state == state).await
}

/// Puts an object with headers and user metadata.
async fn put_with_headers(server: &TestS3, bucket: &str, key: &str, body: &[u8]) {
    server
        .client()
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(body.to_vec()))
        .content_type("text/x-notes")
        .cache_control("max-age=60")
        .metadata("owner", "team-a")
        .send()
        .await
        .unwrap();
}

async fn started(
    server: &TestS3,
    dir: &Path,
    key: &str,
    body: &[u8],
) -> (EditManager, String, String, EditSession) {
    let bucket = server.new_bucket().await;
    put_with_headers(server, &bucket, key, body).await;
    let manager = manager(server.ops(), dir, DEBOUNCE);
    let started = manager.start(request(&bucket, key)).await.unwrap();
    assert!(!started.reused);
    let session = started.session;
    (manager, bucket, session.id.clone(), session)
}

#[tokio::test]
async fn a_save_is_uploaded_with_the_objects_headers() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) =
        started(&server, dir.path(), "docs/notes.txt", b"v1").await;

    let local = Path::new(&session.local_path);
    assert_eq!(fs::read(local).unwrap(), b"v1");
    assert_eq!(local.file_name().unwrap(), "notes.txt");
    assert!(local.starts_with(dir.path().join("edit")));
    assert_eq!(session.state, EditState::Watching);
    let opened_etag = session.etag.clone().unwrap();

    fs::write(local, b"version two").unwrap();
    let uploaded = wait_uploads(&manager, &id, 1).await;
    assert_eq!(server.get(&bucket, "docs/notes.txt").await, b"version two");
    assert_ne!(uploaded.etag.as_deref(), Some(opened_etag.as_str()));
    assert!(uploaded.last_upload_at.is_some());

    // Content headers and user metadata survive the rewrite.
    let head = server
        .ops()
        .head_object(&bucket, "docs/notes.txt", None)
        .await
        .unwrap();
    assert_eq!(head.etag, uploaded.etag);
    assert_eq!(head.meta.content_type.as_deref(), Some("text/x-notes"));
    assert_eq!(head.meta.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(
        head.meta.user_meta.get("owner").map(String::as_str),
        Some("team-a")
    );

    // A second save is a second upload against the new ETag.
    fs::write(local, b"third").unwrap();
    wait_uploads(&manager, &id, 2).await;
    assert_eq!(server.get(&bucket, "docs/notes.txt").await, b"third");
}

#[tokio::test]
async fn saves_are_debounced_and_identical_content_is_not_uploaded() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = server.new_bucket().await;
    server.put(&bucket, "a.txt", b"start".to_vec()).await;
    let manager = manager(server.ops(), dir.path(), Duration::from_millis(500));
    let session = manager
        .start(request(&bucket, "a.txt"))
        .await
        .unwrap()
        .session;
    let local = Path::new(&session.local_path);

    for round in 0..5 {
        fs::write(local, format!("burst {round}")).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    // Still inside the quiet period: nothing was uploaded.
    assert_eq!(manager.get(&session.id).unwrap().uploads, 0);
    assert_eq!(server.get(&bucket, "a.txt").await, b"start");

    wait_uploads(&manager, &session.id, 1).await;
    assert_eq!(server.get(&bucket, "a.txt").await, b"burst 4");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(manager.get(&session.id).unwrap().uploads, 1);

    // The same bytes saved again are not a change.
    fs::write(local, b"burst 4").unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(manager.get(&session.id).unwrap().uploads, 1);
}

#[tokio::test]
async fn a_save_that_replaces_the_file_by_rename_is_uploaded() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) = started(&server, dir.path(), "a/b.txt", b"old").await;
    let local = Path::new(&session.local_path);
    let staged = local.with_file_name(".b.txt.swp");

    fs::write(&staged, b"renamed in").unwrap();
    fs::rename(&staged, local).unwrap();
    wait_uploads(&manager, &id, 1).await;
    assert_eq!(server.get(&bucket, "a/b.txt").await, b"renamed in");

    // Remove, then recreate (editors that delete the file first).
    fs::remove_file(local).unwrap();
    tokio::time::sleep(DEBOUNCE * 2).await;
    assert_eq!(manager.get(&id).unwrap().state, EditState::Uploaded);
    fs::write(local, b"recreated").unwrap();
    wait_uploads(&manager, &id, 2).await;
    assert_eq!(server.get(&bucket, "a/b.txt").await, b"recreated");
}

#[tokio::test]
async fn a_changed_object_blocks_the_upload() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) = started(&server, dir.path(), "c.txt", b"mine").await;
    server.put(&bucket, "c.txt", b"theirs".to_vec()).await;

    fs::write(&session.local_path, b"my edit").unwrap();
    let conflicted = wait_state(&manager, &id, EditState::Conflict).await;
    let conflict = conflicted.conflict.unwrap();
    assert!(!conflict.remote_deleted);
    assert!(conflict.remote_etag.is_some());
    assert_eq!(server.get(&bucket, "c.txt").await, b"theirs");
    assert_eq!(conflicted.uploads, 0);

    // More saves are held while the conflict stands.
    fs::write(&session.local_path, b"my edit 2").unwrap();
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(manager.get(&id).unwrap().state, EditState::Conflict);
    assert_eq!(server.get(&bucket, "c.txt").await, b"theirs");

    // Stopping does not overwrite either.
    let error = manager.stop(&id, false).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(manager.get(&id).is_some());
    assert_eq!(server.get(&bucket, "c.txt").await, b"theirs");
}

#[tokio::test]
async fn overwrite_uploads_the_local_copy_over_the_changed_object() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) = started(&server, dir.path(), "c.txt", b"mine").await;
    server.put(&bucket, "c.txt", b"theirs".to_vec()).await;
    fs::write(&session.local_path, b"my edit").unwrap();
    wait_state(&manager, &id, EditState::Conflict).await;

    let resolved = manager
        .resolve(&id, EditResolution::Overwrite)
        .await
        .unwrap();
    let after = resolved.session.unwrap();
    assert_eq!(after.state, EditState::Uploaded);
    assert_eq!(after.uploads, 1);
    assert!(after.conflict.is_none());
    assert_eq!(server.get(&bucket, "c.txt").await, b"my edit");

    // Editing goes on against the new ETag.
    fs::write(&session.local_path, b"again").unwrap();
    wait_uploads(&manager, &id, 2).await;
    assert_eq!(server.get(&bucket, "c.txt").await, b"again");

    // Overwrite is only for conflicts.
    let error = manager
        .resolve(&id, EditResolution::Overwrite)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn save_as_a_copy_uses_a_new_key_and_ends_the_session() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) =
        started(&server, dir.path(), "docs/report.txt", b"mine").await;
    server
        .put(&bucket, "docs/report.txt", b"theirs".to_vec())
        .await;
    // A copy by that name exists already: the next name is used.
    server
        .put(&bucket, "docs/report (edited).txt", b"older copy".to_vec())
        .await;
    fs::write(&session.local_path, b"my edit").unwrap();
    wait_state(&manager, &id, EditState::Conflict).await;

    let resolved = manager
        .resolve(&id, EditResolution::SaveCopy)
        .await
        .unwrap();
    assert!(resolved.session.is_none());
    assert_eq!(
        resolved.copy_key.as_deref(),
        Some("docs/report (edited 2).txt")
    );
    assert_eq!(
        server.get(&bucket, "docs/report (edited 2).txt").await,
        b"my edit"
    );
    assert_eq!(
        server.get(&bucket, "docs/report (edited).txt").await,
        b"older copy"
    );
    assert_eq!(server.get(&bucket, "docs/report.txt").await, b"theirs");
    assert!(manager.get(&id).is_none());
    assert!(!Path::new(&session.local_path).parent().unwrap().exists());
}

#[tokio::test]
async fn discard_drops_the_changes_and_cleans_up() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) = started(&server, dir.path(), "c.txt", b"mine").await;
    server.put(&bucket, "c.txt", b"theirs".to_vec()).await;
    fs::write(&session.local_path, b"my edit").unwrap();
    wait_state(&manager, &id, EditState::Conflict).await;

    let resolved = manager.resolve(&id, EditResolution::Discard).await.unwrap();
    assert!(resolved.session.is_none() && resolved.copy_key.is_none());
    assert_eq!(server.get(&bucket, "c.txt").await, b"theirs");
    assert!(manager.get(&id).is_none());
    assert!(!Path::new(&session.local_path).exists());
    // The session is gone.
    let error = manager.stop(&id, false).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn a_deleted_object_is_a_conflict_and_overwrite_recreates_it() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, session) = started(&server, dir.path(), "gone.txt", b"mine").await;
    server
        .ops()
        .delete_object(&bucket, "gone.txt", None)
        .await
        .unwrap();
    fs::write(&session.local_path, b"still here").unwrap();
    let conflicted = wait_state(&manager, &id, EditState::Conflict).await;
    assert!(conflicted.conflict.unwrap().remote_deleted);
    assert!(!server.exists(&bucket, "gone.txt").await);

    manager
        .resolve(&id, EditResolution::Overwrite)
        .await
        .unwrap();
    assert_eq!(server.get(&bucket, "gone.txt").await, b"still here");
    // The headers of the opened object are kept for the recreated one.
    let head = server
        .ops()
        .head_object(&bucket, "gone.txt", None)
        .await
        .unwrap();
    assert_eq!(head.meta.content_type.as_deref(), Some("text/x-notes"));
}

#[tokio::test]
async fn stop_uploads_a_pending_save_and_removes_the_copy() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = server.new_bucket().await;
    server.put(&bucket, "p.txt", b"start".to_vec()).await;
    // A long quiet period: the save is still pending when the session stops.
    let manager = manager(server.ops(), dir.path(), Duration::from_secs(60));
    let session = manager
        .start(request(&bucket, "p.txt"))
        .await
        .unwrap()
        .session;
    fs::write(&session.local_path, b"pending").unwrap();
    let mut events = manager.subscribe();

    manager.stop(&session.id, false).await.unwrap();
    assert_eq!(server.get(&bucket, "p.txt").await, b"pending");
    assert!(manager.list().is_empty());
    assert!(!Path::new(&session.local_path).parent().unwrap().exists());

    let mut states = Vec::new();
    while let Ok(event) = events.try_recv() {
        states.push(event.state);
    }
    assert_eq!(
        states,
        [
            EditState::Uploading,
            EditState::Uploaded,
            EditState::Stopped
        ]
    );
}

#[tokio::test]
async fn stop_with_discard_uploads_nothing() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = server.new_bucket().await;
    server.put(&bucket, "d.txt", b"start".to_vec()).await;
    let manager = manager(server.ops(), dir.path(), Duration::from_secs(60));
    let session = manager
        .start(request(&bucket, "d.txt"))
        .await
        .unwrap()
        .session;
    fs::write(&session.local_path, b"unwanted").unwrap();

    manager.stop(&session.id, true).await.unwrap();
    assert_eq!(server.get(&bucket, "d.txt").await, b"start");
    assert!(!Path::new(&session.local_path).exists());
}

#[tokio::test]
async fn shutdown_flushes_saves_and_keeps_what_it_could_not_upload() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = server.new_bucket().await;
    server.put(&bucket, "ok.txt", b"start".to_vec()).await;
    server.put(&bucket, "clash.txt", b"start".to_vec()).await;
    let manager = manager(server.ops(), dir.path(), Duration::from_secs(60));
    let ok = manager
        .start(request(&bucket, "ok.txt"))
        .await
        .unwrap()
        .session;
    let clash = manager
        .start(request(&bucket, "clash.txt"))
        .await
        .unwrap()
        .session;
    fs::write(&ok.local_path, b"flushed").unwrap();
    server.put(&bucket, "clash.txt", b"theirs".to_vec()).await;
    fs::write(&clash.local_path, b"mine").unwrap();

    let left = manager.shutdown(Duration::from_secs(20)).await;
    assert_eq!(server.get(&bucket, "ok.txt").await, b"flushed");
    assert_eq!(server.get(&bucket, "clash.txt").await, b"theirs");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].key, "clash.txt");
    // The unsaved work stays on disk.
    assert_eq!(fs::read(&clash.local_path).unwrap(), b"mine");
    assert!(!Path::new(&ok.local_path).exists());
}

#[tokio::test]
async fn starting_twice_reuses_the_session_and_folders_are_refused() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, bucket, id, _) = started(&server, dir.path(), "r.txt", b"x").await;
    let again = manager.start(request(&bucket, "r.txt")).await.unwrap();
    assert!(again.reused);
    assert_eq!(again.session.id, id);
    assert_eq!(manager.list().len(), 1);

    for key in ["", "folder/"] {
        let error = manager.start(request(&bucket, key)).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{key:?}");
    }
    let error = manager
        .start(request(&bucket, "missing.txt"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    // A failed start leaves no folder behind.
    assert_eq!(fs::read_dir(dir.path().join("edit")).unwrap().count(), 1);
}

#[tokio::test]
async fn tags_survive_an_upload() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let bucket = server.new_bucket().await;
    server.put(&bucket, "t.txt", b"start".to_vec()).await;
    let tags = [("team".to_owned(), "a".to_owned())].into_iter().collect();
    if server
        .ops()
        .put_object_tags(&bucket, "t.txt", &tags)
        .await
        .is_err()
    {
        // s3s-fs has no tagging (docs/testing.md); MinIO does.
        return;
    }
    let manager = manager(server.ops(), dir.path(), DEBOUNCE);
    let session = manager
        .start(request(&bucket, "t.txt"))
        .await
        .unwrap()
        .session;
    fs::write(&session.local_path, b"edited").unwrap();
    wait_uploads(&manager, &session.id, 1).await;
    assert_eq!(
        server
            .ops()
            .get_object_tags(&bucket, "t.txt")
            .await
            .unwrap(),
        tags
    );
}

// ---- scripted server ----

/// A server whose `PUT` answers come from `put`, with a fixed object.
fn scripted(
    put: impl Fn(usize, &bucketbridge_tests::RecordedRequest) -> MockResponse + Send + Sync + 'static,
) -> MockS3 {
    let puts = std::sync::atomic::AtomicUsize::new(0);
    MockS3::start(move |request| match request.method.as_str() {
        "HEAD" if request.path == "/b/o.txt" => object_headers(MockResponse::empty(200)),
        "GET" if request.path == "/b/o.txt" && request.query == "x-id=GetObject" => {
            let mut reply = object_headers(MockResponse::empty(200));
            reply.body = "hello".to_owned();
            reply
        }
        "PUT" if request.path == "/b/o.txt" && request.query == "x-id=PutObject" => {
            let n = puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            put(n, request)
        }
        _ => MockResponse::error(501, "NotImplemented", "not scripted"),
    })
}

fn object_headers(reply: MockResponse) -> MockResponse {
    reply
        .with_header("etag", "\"5d41402abc4b2a76b9719d911017c592\"")
        .with_header("content-length", "5")
        .with_header("last-modified", "Thu, 29 Oct 2026 08:15:00 GMT")
        .with_header("content-type", "text/plain")
}

fn puts(mock: &MockS3) -> Vec<bucketbridge_tests::RecordedRequest> {
    mock.requests()
        .into_iter()
        .filter(|request| request.method == "PUT")
        .collect()
}

#[tokio::test]
async fn upload_carries_if_match_and_falls_back_when_the_server_lacks_it() {
    let mock = scripted(|n, request| {
        if n == 0 {
            assert!(request.header("if-match").is_some());
            MockResponse::error(501, "NotImplemented", "conditional writes")
        } else {
            MockResponse::empty(200).with_header("etag", "\"new\"")
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&mock.ops(), dir.path(), DEBOUNCE);
    let session = manager.start(request("b", "o.txt")).await.unwrap().session;
    assert_eq!(fs::read(&session.local_path).unwrap(), b"hello");

    fs::write(&session.local_path, b"first").unwrap();
    wait_uploads(&manager, &session.id, 1).await;
    let sent = puts(&mock);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].header("if-match"),
        Some("\"5d41402abc4b2a76b9719d911017c592\"")
    );
    assert!(sent[1].header("if-match").is_none());
    assert_eq!(sent[1].header("content-type"), Some("text/plain"));
}

#[tokio::test]
async fn a_precondition_failure_is_a_conflict() {
    let mock = scripted(|_, _| {
        MockResponse::error(
            412,
            "PreconditionFailed",
            "At least one precondition failed",
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&mock.ops(), dir.path(), DEBOUNCE);
    let session = manager.start(request("b", "o.txt")).await.unwrap().session;
    fs::write(&session.local_path, b"edit").unwrap();
    let conflicted = wait_state(&manager, &session.id, EditState::Conflict).await;
    assert_eq!(conflicted.uploads, 0);
    assert_eq!(puts(&mock).len(), 1);
}

#[tokio::test]
async fn a_failed_upload_is_an_error_that_retry_repeats() {
    let mock = scripted(|n, _| {
        if n == 0 {
            MockResponse::error(403, "AccessDenied", "Access Denied")
        } else {
            MockResponse::empty(200).with_header("etag", "\"new\"")
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&mock.ops(), dir.path(), DEBOUNCE);
    let session = manager.start(request("b", "o.txt")).await.unwrap().session;
    fs::write(&session.local_path, b"edit").unwrap();
    let failed = wait_state(&manager, &session.id, EditState::Error).await;
    assert!(failed.error.is_some());

    let resolved = manager
        .resolve(&session.id, EditResolution::Retry)
        .await
        .unwrap();
    assert_eq!(resolved.session.unwrap().state, EditState::Uploaded);
}

#[tokio::test]
async fn stop_reports_a_save_that_cannot_be_uploaded_and_keeps_the_copy() {
    let mock = scripted(|_, _| MockResponse::error(403, "AccessDenied", "Access Denied"));
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&mock.ops(), dir.path(), Duration::from_secs(60));
    let session = manager.start(request("b", "o.txt")).await.unwrap().session;
    fs::write(&session.local_path, b"edit").unwrap();
    let error = manager.stop(&session.id, false).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    assert!(Path::new(&session.local_path).exists());
    assert_eq!(manager.get(&session.id).unwrap().state, EditState::Error);
}

#[tokio::test]
async fn leftover_copies_are_purged_when_old() {
    let server = TestS3::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (manager, _, id, session) = started(&server, dir.path(), "k.txt", b"x").await;
    let stale = dir.path().join("edit").join("leftover");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("a.txt"), b"a").unwrap();
    manager.purge_stale(Duration::ZERO);
    assert!(!stale.exists());
    // The folder of a live session is never purged.
    assert!(Path::new(&session.local_path).exists());
    manager.stop(&id, true).await.unwrap();
}
