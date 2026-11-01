//! Read timeouts and stalled-stream protection, against raw endpoints that
//! are slow or silent on purpose ([`RawServer`]).
//!
//! The clients have no read timeout: the SDK measures it from the start of a
//! request until the response headers arrive, which includes uploading the
//! body. `S3` applies it per request to metadata requests instead
//! (`S3::metadata_timeout`); transfers rely on stalled-stream protection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use bucketbridge_core::aws_sdk_s3::config::StalledStreamProtectionConfig;
use bucketbridge_core::aws_sdk_s3::primitives::ByteStream;
use bucketbridge_core::client::STALL_GRACE_PERIOD;
use bucketbridge_core::{ClientOptions, Error, ErrorKind, S3Uri};
use bucketbridge_tests::{RawBehavior, RawServer};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, TryFutureExt};
use hyper::body::{Body, Frame, SizeHint};

fn options(read_timeout: Duration) -> ClientOptions {
    ClientOptions {
        max_attempts: 1,
        read_timeout,
        ..ClientOptions::default()
    }
}

fn with_grace(read_timeout: Duration, grace: Duration) -> ClientOptions {
    ClientOptions {
        stalled_stream: StalledStreamProtectionConfig::enabled()
            .grace_period(grace)
            .build(),
        ..options(read_timeout)
    }
}

/// The messages of `err` and all its sources.
fn chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut next = err.source();
    while let Some(source) = next {
        text.push_str(": ");
        text.push_str(&source.to_string());
        next = source.source();
    }
    text
}

/// An upload body that yields `chunks` chunks with `pause` between them, as
/// a bandwidth-limited transfer does.
struct PausingBody {
    chunk: Bytes,
    remaining: usize,
    pause: Duration,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl PausingBody {
    fn new(chunk_len: usize, chunks: usize, pause: Duration) -> Self {
        PausingBody {
            chunk: Bytes::from(vec![b'u'; chunk_len]),
            remaining: chunks,
            pause,
            sleep: None,
        }
    }

    fn len(&self) -> u64 {
        (self.chunk.len() * self.remaining) as u64
    }
}

impl Body for PausingBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        if let Some(sleep) = self.sleep.as_mut() {
            ready!(sleep.as_mut().poll(cx));
        }
        self.remaining -= 1;
        self.sleep = (self.remaining > 0).then(|| Box::pin(tokio::time::sleep(self.pause)));
        Poll::Ready(Some(Ok(Frame::data(self.chunk.clone()))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_slower_than_the_read_timeout_succeed() {
    // About 1.3 MB/s: 3 MiB take about 2.4 s, the read timeout is 1 s, and
    // the server answers only once the whole body has arrived.
    let server = RawServer::start(RawBehavior::SlowDrain {
        chunk: 64 * 1024,
        pause: Duration::from_millis(50),
    });
    let s3 = server.ops_with(&options(Duration::from_secs(1)));
    let client = s3.client_for("bucket").await.unwrap();
    let body = vec![7u8; 3 * 1024 * 1024];

    let started = Instant::now();
    let out = client
        .put_object()
        .bucket("bucket")
        .key("in-memory.bin")
        .body(ByteStream::from(body.clone()))
        .send()
        .await
        .unwrap();
    assert!(
        started.elapsed() > Duration::from_secs(1),
        "the upload has to outlast the read timeout to prove anything"
    );
    assert_eq!(out.e_tag(), Some("\"raw-etag\""));

    // A streamed body (read from a file in chunks, with a CRC32 trailer), as
    // the transfer engine sends it.
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &body).unwrap();
    client
        .put_object()
        .bucket("bucket")
        .key("streamed.bin")
        .body(ByteStream::from_path(file.path()).await.unwrap())
        .send()
        .await
        .unwrap();
    assert!(server.body_bytes() >= 2 * body.len());
    assert_eq!(server.requests(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadata_requests_time_out_when_the_server_never_answers() {
    let server = RawServer::start(RawBehavior::Silent);
    let s3 = server.ops_with(&options(Duration::from_secs(1)));
    let resolving = s3.clone().with_region_resolution(true);
    let (src, dst) = (
        S3Uri::new("bucket", "a").unwrap(),
        S3Uri::new("bucket", "b").unwrap(),
    );
    let keys = vec!["k".to_owned()];
    let raw_get = async {
        s3.client()
            .get_object()
            .bucket("bucket")
            .key("k")
            .customize()
            .config_override(s3.metadata_timeout())
            .send()
            .await
            .map(drop)
            .map_err(Error::from)
    };
    // Every request the transport makes without a large body, and a raw
    // GetObject with the per-request override.
    let checks: Vec<(&str, BoxFuture<'_, bucketbridge_core::Result<()>>)> = vec![
        ("list_buckets", s3.list_buckets().map_ok(drop).boxed()),
        (
            "create_bucket",
            s3.create_bucket("new-bucket", None).boxed(),
        ),
        ("delete_bucket", s3.delete_bucket("bucket").boxed()),
        (
            "list_objects",
            s3.list_objects("bucket", "", false, None, 1000)
                .map_ok(drop)
                .boxed(),
        ),
        ("list_all", s3.list_all("bucket", "").map_ok(drop).boxed()),
        (
            "list_versions",
            s3.list_versions("bucket", "", false, None, 1000)
                .map_ok(drop)
                .boxed(),
        ),
        (
            "head_object",
            s3.head_object("bucket", "k", None).map_ok(drop).boxed(),
        ),
        ("create_folder", s3.create_folder("bucket", "dir/").boxed()),
        (
            "delete_object",
            s3.delete_object("bucket", "k", None).boxed(),
        ),
        (
            "delete_objects",
            s3.delete_objects("bucket", &keys).map_ok(drop).boxed(),
        ),
        (
            "delete_prefix",
            s3.delete_prefix("bucket", "dir/").map_ok(drop).boxed(),
        ),
        (
            "copy_object",
            s3.copy_object(&src, &dst).map_ok(drop).boxed(),
        ),
        (
            "list_multipart_uploads",
            s3.list_multipart_uploads("bucket", "").map_ok(drop).boxed(),
        ),
        (
            "abort_multipart_upload",
            s3.abort_multipart_upload("bucket", "k", "upload").boxed(),
        ),
        (
            "client_for (HeadBucket)",
            resolving.client_for("bucket").map_ok(drop).boxed(),
        ),
        ("raw GetObject", raw_get.boxed()),
    ];
    let count = checks.len();
    let started = Instant::now();
    let results = futures::future::join_all(checks.into_iter().map(|(name, check)| async move {
        (
            name,
            tokio::time::timeout(Duration::from_secs(20), check).await,
        )
    }))
    .await;
    for (name, result) in results {
        let result = result.unwrap_or_else(|_| panic!("{name} has no read timeout"));
        let err = result.expect_err(name);
        assert_eq!(err.kind(), ErrorKind::Remote, "{name}: {err}");
        assert!(err.to_string().contains("timeout"), "{name}: {err}");
    }
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(server.requests(), count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_survive_pauses_of_a_few_seconds() {
    // Headers at once, then three chunks 2 s apart. The read timeout (1 s)
    // ends with the headers, and stalled-stream protection tolerates
    // pauses shorter than its grace period.
    let server = RawServer::start(RawBehavior::Trickle {
        chunk: 1024,
        chunks: 3,
        pause: Duration::from_secs(2),
    });
    let s3 = server.ops_with(&options(Duration::from_secs(1)));
    assert!(STALL_GRACE_PERIOD > Duration::from_secs(2));
    let out = s3
        .client_for("bucket")
        .await
        .unwrap()
        .get_object()
        .bucket("bucket")
        .key("k")
        .customize()
        .config_override(s3.metadata_timeout())
        .send()
        .await
        .unwrap();
    let started = Instant::now();
    let body = out.body.collect().await.unwrap().into_bytes();
    assert_eq!(body.len(), 3 * 1024);
    assert!(started.elapsed() >= Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_download_silent_for_longer_than_the_grace_period_fails() {
    let server = RawServer::start(RawBehavior::Trickle {
        chunk: 1024,
        chunks: 2,
        pause: Duration::from_secs(5),
    });
    let s3 = server.ops_with(&with_grace(Duration::from_secs(1), Duration::from_secs(1)));
    let out = s3
        .client()
        .get_object()
        .bucket("bucket")
        .key("k")
        .send()
        .await
        .unwrap();
    let started = Instant::now();
    let err = out.body.collect().await.unwrap_err();
    assert!(
        chain(&err).contains("minimum throughput"),
        "{}",
        chain(&err)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn throttled_upload_bodies_survive_pauses_of_a_few_seconds() {
    let server = RawServer::start(RawBehavior::SlowDrain {
        chunk: 1024 * 1024,
        pause: Duration::ZERO,
    });
    let s3 = server.ops_with(&options(Duration::from_secs(1)));
    let body = PausingBody::new(16 * 1024, 3, Duration::from_secs(2));
    let length = body.len();
    let started = Instant::now();
    s3.client()
        .put_object()
        .bucket("bucket")
        .key("throttled.bin")
        .content_length(i64::try_from(length).unwrap())
        .body(ByteStream::from_body_1_x(body))
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(server.body_bytes() >= 3 * 16 * 1024);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_body_silent_for_longer_than_the_grace_period_fails() {
    let server = RawServer::start(RawBehavior::SlowDrain {
        chunk: 1024 * 1024,
        pause: Duration::ZERO,
    });
    let s3 = server.ops_with(&with_grace(Duration::from_secs(1), Duration::from_secs(1)));
    let body = PausingBody::new(16 * 1024, 2, Duration::from_secs(5));
    let length = body.len();
    let started = Instant::now();
    let err = s3
        .client()
        .put_object()
        .bucket("bucket")
        .key("stalled.bin")
        .content_length(i64::try_from(length).unwrap())
        .body(ByteStream::from_body_1_x(body))
        .send()
        .await
        .unwrap_err();
    assert!(
        chain(&err).contains("minimum throughput"),
        "{}",
        chain(&err)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_per_request_grace_period_overrides_the_client() {
    // A client that fails a body after 1 s of silence, and one request
    // allowed 20 s, as a transfer under a very low bandwidth limit needs.
    let server = RawServer::start(RawBehavior::SlowDrain {
        chunk: 1024 * 1024,
        pause: Duration::ZERO,
    });
    let s3 = server.ops_with(&with_grace(Duration::from_secs(1), Duration::from_secs(1)));
    let body = PausingBody::new(16 * 1024, 2, Duration::from_secs(3));
    let length = body.len();
    let relaxed = bucketbridge_core::aws_sdk_s3::config::Builder::default()
        .stalled_stream_protection(
            StalledStreamProtectionConfig::enabled()
                .grace_period(STALL_GRACE_PERIOD)
                .build(),
        );
    s3.client()
        .put_object()
        .bucket("bucket")
        .key("patient.bin")
        .content_length(i64::try_from(length).unwrap())
        .body(ByteStream::from_body_1_x(body))
        .customize()
        .config_override(relaxed)
        .send()
        .await
        .unwrap();
    assert!(server.body_bytes() >= 2 * 16 * 1024);
}
