//! Integration-test harness for BucketBridge.
//!
//! * [`TestS3::start`] gives a test its own S3 server: by default an
//!   in-process [`s3s`] + [`s3s_fs`] server on `127.0.0.1` with SigV4
//!   authentication over a temporary directory; with the `minio` feature and
//!   `BB_TEST_BACKEND=minio`, a MinIO container (Linux CI). Every instance has
//!   its own port, data directory and keys, so tests run in parallel. The
//!   s3s server runs on a dedicated thread with its own Tokio runtime, so a
//!   test may block (for example while running the `bb` binary) without
//!   stalling the server.
//! * [`MockS3`] is a scripted HTTP endpoint for behaviour no real backend
//!   produces on demand: per-key delete failures, throttling, repeated
//!   continuation tokens, version listings on the default backend.
//! * [`RawServer`] is a raw HTTP/1.1 endpoint that is slow or silent on
//!   purpose, for read-timeout and stalled-stream tests.
//! * [`BucketConfigMock`] is a [`MockS3`] that keeps the bucket configuration
//!   it is given (policy, public access block, versioning, logging, website,
//!   acceleration), for round trips that no default backend supports.
//!
//! See `docs/testing.md` for what s3s-fs does not implement.
//!
//! The harness panics with a descriptive message when the test environment
//! cannot be set up; it is test support code, not library code.

#![warn(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod bucketcfg;
mod bucketrules;
mod imds;
mod objstore;
mod raw;

pub use bucketcfg::{BucketConfigMock, ConfigKind};
pub use bucketrules::RulesMock;
pub use imds::FakeImds;
pub use objstore::{
    BUCKET_OWNER_ID, OWNER_ID, ObjectStore, StoredEncryption, StoredGrantee, StoredObject,
    canned_acl,
};
pub use raw::{RawBehavior, RawServer};

use std::future::Future;
use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use bucketbridge_core::aws_sdk_s3;
use bucketbridge_core::aws_sdk_s3::primitives::ByteStream;
use bucketbridge_core::model::Account;
use bucketbridge_core::{ClientOptions, Credentials, ErrorKind, S3};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::sync::oneshot;

/// Region used by every test server.
pub const REGION: &str = "us-east-1";

/// Environment variable that selects the backend (`minio` or `s3s`).
pub const BACKEND_ENV: &str = "BB_TEST_BACKEND";

/// Environment variable that replaces the MinIO image: `name` or `name:tag`,
/// for example an image in a private registry.
pub const MINIO_IMAGE_ENV: &str = "BB_TEST_MINIO_IMAGE";

/// The MinIO image the container uses. The `minio/minio` images that the
/// `testcontainers-modules` MinIO module names are no longer on Docker Hub or
/// Quay (MinIO stopped publishing them in 2025), so this is the last release
/// that the community fork `pgsty/minio` published under the MinIO name. It
/// has the same entrypoint, port and start-up message as the old image.
#[cfg(feature = "minio")]
const DEFAULT_MINIO_IMAGE: &str = "pgsty/minio:RELEASE.2026-08-04T00-00-00Z";

/// The server behind a [`TestS3`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// In-process s3s + s3s-fs (default).
    S3sFs,
    /// A MinIO container (feature `minio`, `BB_TEST_BACKEND=minio`).
    MinIO,
}

/// An isolated S3 server for one test, with a ready account and clients.
pub struct TestS3 {
    backend: Backend,
    endpoint: String,
    access_key: String,
    secret_key: String,
    account: Account,
    ops: S3,
    names: AtomicU64,
    // Declared last so it is dropped after the clients.
    server: Server,
}

enum Server {
    S3sFs {
        // Held for its Drop. Field order matters: the server thread is
        // stopped before the data directory is removed.
        _thread: ServerThread,
        root: tempfile::TempDir,
    },
    #[cfg(feature = "minio")]
    MinIO {
        // Held for its Drop, which removes the container. Boxed because it is large.
        _container: Box<
            testcontainers_modules::testcontainers::ContainerAsync<
                testcontainers_modules::minio::MinIO,
            >,
        >,
    },
}

impl TestS3 {
    /// Starts the backend selected by `BB_TEST_BACKEND` (s3s-fs by default).
    ///
    /// Panics if `BB_TEST_BACKEND=minio` is set without the `minio` feature,
    /// or if the server cannot be started.
    pub async fn start() -> TestS3 {
        match requested_backend() {
            Backend::S3sFs => Self::start_s3s(),
            Backend::MinIO => Self::start_minio().await,
        }
    }

    /// Starts MinIO. For tests that need features s3s-fs lacks; such tests
    /// are `#[ignore]`d unless the `minio` feature is enabled.
    ///
    /// Panics unless the crate is built with `--features minio` and
    /// `BB_TEST_BACKEND=minio` is set (Docker is required).
    pub async fn start_minio() -> TestS3 {
        #[cfg(feature = "minio")]
        if requested_backend() == Backend::MinIO {
            return Self::launch_minio().await;
        }
        panic!(
            "this test needs MinIO: build with `--features minio` and set {BACKEND_ENV}=minio (Docker is required)"
        );
    }

    fn start_s3s() -> TestS3 {
        let root = tempfile::Builder::new()
            .prefix("bb-test-s3-")
            .tempdir()
            .expect("TestS3: create a temporary data directory");
        let access_key = format!("BBTEST{}", random_hex(14)).to_ascii_uppercase();
        let secret_key = random_hex(40);
        let filesystem =
            s3s_fs::FileSystem::new(root.path()).expect("TestS3: open the s3s-fs data directory");
        let mut builder = s3s::service::S3ServiceBuilder::new(filesystem);
        builder.set_auth(s3s::auth::SimpleAuth::from_single(
            access_key.clone(),
            secret_key.clone(),
        ));
        let service = builder.build();
        let thread = ServerThread::spawn("s3s-fs", move |stream| {
            let service = service.clone();
            async move {
                let mut http = http1::Builder::new();
                http.timer(TokioTimer::new());
                let _ = http.serve_connection(TokioIo::new(stream), service).await;
            }
        });
        let endpoint = format!("http://{}", thread.addr);
        Self::assemble(
            Backend::S3sFs,
            endpoint,
            access_key,
            secret_key,
            Server::S3sFs {
                _thread: thread,
                root,
            },
        )
    }

    #[cfg(feature = "minio")]
    async fn launch_minio() -> TestS3 {
        use testcontainers_modules::minio::MinIO;
        use testcontainers_modules::testcontainers::ImageExt;
        use testcontainers_modules::testcontainers::runners::AsyncRunner;

        let (image, tag) = minio_image();
        let container = MinIO::default()
            .with_name(&image)
            .with_tag(&tag)
            .start()
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "TestS3: start the MinIO container {image}:{tag} (is Docker running, and can \
                     it pull the image? {MINIO_IMAGE_ENV} names another one): {err}"
                )
            });
        let host = container.get_host().await.expect("TestS3: MinIO host");
        let port = container
            .get_host_port_ipv4(9000)
            .await
            .expect("TestS3: MinIO port");
        Self::assemble(
            Backend::MinIO,
            format!("http://{host}:{port}"),
            "minioadmin".to_owned(),
            "minioadmin".to_owned(),
            Server::MinIO {
                _container: Box::new(container),
            },
        )
    }

    fn assemble(
        backend: Backend,
        endpoint: String,
        access_key: String,
        secret_key: String,
        server: Server,
    ) -> TestS3 {
        let account = Account {
            id: format!("test-account-{}", random_hex(8)),
            label: "Test account".to_owned(),
            access_key_id: access_key.clone(),
            secret_ref: "test/secret".to_owned(),
            region: REGION.to_owned(),
            endpoint: Some(endpoint.clone()),
            path_style: true,
            ..Account::default()
        };
        let credentials = Credentials::new(access_key.clone(), secret_key.clone(), None);
        let ops = S3::new(&account, &credentials, &ClientOptions::default())
            .expect("TestS3: build the S3 client");
        TestS3 {
            backend,
            endpoint,
            access_key,
            secret_key,
            account,
            ops,
            names: AtomicU64::new(0),
            server,
        }
    }

    /// Which backend is running.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Endpoint URL, for example `http://127.0.0.1:50123`.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Signing region.
    pub fn region(&self) -> &str {
        REGION
    }

    /// Access key ID accepted by the server.
    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    /// Secret access key accepted by the server.
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    /// An account pointing at this server (custom endpoint, path-style).
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Valid credentials for [`TestS3::account`].
    pub fn credentials(&self) -> Credentials {
        Credentials::new(self.access_key.clone(), self.secret_key.clone(), None)
    }

    /// The core transport for [`TestS3::account`] with default options.
    pub fn ops(&self) -> &S3 {
        &self.ops
    }

    /// The raw SDK client behind [`TestS3::ops`].
    pub fn client(&self) -> &aws_sdk_s3::Client {
        self.ops.client()
    }

    /// A transport for this server with other credentials or options.
    pub fn connect(&self, credentials: &Credentials, options: &ClientOptions) -> S3 {
        S3::new(&self.account, credentials, options).expect("TestS3: build the S3 client")
    }

    /// The s3s-fs data directory (`None` for MinIO).
    pub fn root(&self) -> Option<&Path> {
        match &self.server {
            Server::S3sFs { root, .. } => Some(root.path()),
            #[cfg(feature = "minio")]
            Server::MinIO { .. } => None,
        }
    }

    /// A bucket name not used before on this server.
    pub fn unique_bucket(&self) -> String {
        let n = self.names.fetch_add(1, Ordering::Relaxed);
        format!("bb-test-{n}-{}", random_hex(8))
    }

    /// Creates a bucket with the raw client.
    pub async fn create_bucket(&self, name: &str) {
        self.client()
            .create_bucket()
            .bucket(name)
            .send()
            .await
            .unwrap_or_else(|err| panic!("TestS3: create bucket {name}: {err:?}"));
    }

    /// Creates a bucket with a fresh name and returns the name.
    pub async fn new_bucket(&self) -> String {
        let name = self.unique_bucket();
        self.create_bucket(&name).await;
        name
    }

    /// Uploads an object with the raw client (SDK default checksums).
    pub async fn put(&self, bucket: &str, key: &str, body: impl Into<Vec<u8>>) {
        self.client()
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from(body.into()))
            .send()
            .await
            .unwrap_or_else(|err| panic!("TestS3: put s3://{bucket}/{key}: {err:?}"));
    }

    /// Downloads an object with the raw client.
    pub async fn get(&self, bucket: &str, key: &str) -> Vec<u8> {
        let out = self
            .client()
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .unwrap_or_else(|err| panic!("TestS3: get s3://{bucket}/{key}: {err:?}"));
        out.body
            .collect()
            .await
            .unwrap_or_else(|err| panic!("TestS3: read s3://{bucket}/{key}: {err:?}"))
            .into_bytes()
            .to_vec()
    }

    /// Whether an object exists (`HeadObject`).
    pub async fn exists(&self, bucket: &str, key: &str) -> bool {
        match self.ops.head_object(bucket, key, None).await {
            Ok(_) => true,
            Err(err) if err.kind() == ErrorKind::NotFound => false,
            Err(err) => panic!("TestS3: head s3://{bucket}/{key}: {err}"),
        }
    }

    /// Number of multipart uploads for `bucket`/`key` that were started and
    /// neither completed nor aborted.
    ///
    /// With MinIO this lists the uploads of exactly that key (MinIO lists
    /// uploads per object key, not by arbitrary prefix). With s3s-fs it counts
    /// every open upload on the (isolated) server, so a test should only
    /// start uploads for the key it checks.
    #[cfg_attr(not(feature = "minio"), allow(unused_variables))]
    pub async fn pending_uploads(&self, bucket: &str, key: &str) -> usize {
        match &self.server {
            Server::S3sFs { root, .. } => std::fs::read_dir(root.path())
                .expect("TestS3: read the data directory")
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".upload-") && name.ends_with(".json")
                })
                .count(),
            #[cfg(feature = "minio")]
            Server::MinIO { .. } => self
                .ops
                .list_multipart_uploads(bucket, key)
                .await
                .unwrap_or_else(|err| panic!("TestS3: list uploads of s3://{bucket}/{key}: {err}"))
                .iter()
                .filter(|upload| upload.key == key)
                .count(),
        }
    }
}

/// Name and tag of the MinIO image: [`MINIO_IMAGE_ENV`] or the default.
#[cfg(feature = "minio")]
fn minio_image() -> (String, String) {
    let reference =
        std::env::var(MINIO_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_MINIO_IMAGE.to_owned());
    split_image(&reference)
}

/// Splits `name` or `name:tag` into the name and the tag (`latest` when there
/// is none). A colon followed by a slash belongs to a registry port.
#[cfg(feature = "minio")]
fn split_image(reference: &str) -> (String, String) {
    match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name.to_owned(), tag.to_owned()),
        _ => (reference.to_owned(), "latest".to_owned()),
    }
}

fn requested_backend() -> Backend {
    match std::env::var(BACKEND_ENV)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "s3s" | "s3s-fs" => Backend::S3sFs,
        "minio" if cfg!(feature = "minio") => Backend::MinIO,
        "minio" => panic!(
            "{BACKEND_ENV}=minio needs the `minio` feature: cargo test -p bucketbridge-tests --features minio"
        ),
        other => panic!("unknown {BACKEND_ENV} value {other:?}; use `minio` or `s3s`"),
    }
}

/// Random lowercase hex digits (process-local randomness from the standard library).
fn random_hex(len: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(len + 16);
    while out.len() < len {
        let seed = (SystemTime::now(), COUNTER.fetch_add(1, Ordering::Relaxed));
        out.push_str(&format!("{:016x}", RandomState::new().hash_one(seed)));
    }
    out.truncate(len);
    out
}

/// A server accept loop on its own thread and current-thread runtime.
struct ServerThread {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ServerThread {
    fn spawn<F, Fut>(name: &str, serve: F) -> ServerThread
    where
        F: Fn(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("TestS3: bind 127.0.0.1:0");
        listener
            .set_nonblocking(true)
            .expect("TestS3: non-blocking listener");
        let addr = listener.local_addr().expect("TestS3: listener address");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name(format!("{name}-{}", addr.port()))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("TestS3: server runtime");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("TestS3: register listener");
                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            accepted = listener.accept() => match accepted {
                                Ok((stream, _)) => {
                                    tokio::spawn(serve(stream));
                                }
                                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                            },
                        }
                    }
                });
                runtime.shutdown_timeout(Duration::from_secs(5));
            })
            .expect("TestS3: spawn the server thread");
        ServerThread {
            addr,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }
}

impl Drop for ServerThread {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One request received by a [`MockS3`].
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// HTTP method, such as `GET`.
    pub method: String,
    /// Request path as sent (percent-encoded).
    pub path: String,
    /// Raw query string, without the `?`.
    pub query: String,
    /// Headers with lowercase names.
    pub headers: Vec<(String, String)>,
    /// Request body.
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// First value of a header (name compared case-insensitively).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Percent-decoded value of a query parameter; `Some("")` for a bare flag such as `?delete`.
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key) == name).then(|| percent_decode(value))
        })
    }

    /// The body as UTF-8 text.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A canned reply of a [`MockS3`].
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: String,
}

impl MockResponse {
    /// `200 OK` with an XML body.
    pub fn xml(body: impl Into<String>) -> Self {
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/xml".into())],
            body: body.into(),
        }
    }

    /// An empty response with the given status.
    pub fn empty(status: u16) -> Self {
        MockResponse {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// An S3 error document with the given status, code and message, and an
    /// `x-amz-request-id` header.
    pub fn error(status: u16, code: &str, message: &str) -> Self {
        MockResponse {
            status,
            headers: vec![
                ("content-type".into(), "application/xml".into()),
                ("x-amz-request-id".into(), "MOCKREQUEST1".into()),
            ],
            body: format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{message}</Message><RequestId>MOCKREQUEST1</RequestId></Error>"
            ),
        }
    }

    /// Adds a header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

type Handler = dyn Fn(&RecordedRequest) -> MockResponse + Send + Sync;

/// A scripted S3 endpoint: every request is recorded and answered by a
/// handler. No authentication is checked.
pub struct MockS3 {
    endpoint: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    // Dropped last.
    _thread: ServerThread,
}

impl MockS3 {
    /// Starts the endpoint with `handler` answering every request.
    pub fn start<F>(handler: F) -> MockS3
    where
        F: Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static,
    {
        let handler: Arc<Handler> = Arc::new(handler);
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::default();
        let log = requests.clone();
        let thread = ServerThread::spawn("mock-s3", move |stream| {
            let handler = handler.clone();
            let log = log.clone();
            async move {
                let service =
                    hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
                        let handler = handler.clone();
                        let log = log.clone();
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body
                                .collect()
                                .await
                                .map(|b| b.to_bytes().to_vec())
                                .unwrap_or_default();
                            let recorded = RecordedRequest {
                                method: parts.method.as_str().to_owned(),
                                path: parts.uri.path().to_owned(),
                                query: parts.uri.query().unwrap_or_default().to_owned(),
                                headers: parts
                                    .headers
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.as_str().to_owned(),
                                            String::from_utf8_lossy(v.as_bytes()).into_owned(),
                                        )
                                    })
                                    .collect(),
                                body,
                            };
                            let reply = handler(&recorded);
                            log.lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(recorded);
                            let mut response = hyper::Response::builder().status(reply.status);
                            for (name, value) in &reply.headers {
                                response = response.header(name, value);
                            }
                            let response = response
                                .body(Full::new(Bytes::from(reply.body)))
                                .expect("MockS3: build the response");
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                let mut http = http1::Builder::new();
                http.timer(TokioTimer::new());
                let _ = http.serve_connection(TokioIo::new(stream), service).await;
            }
        });
        MockS3 {
            endpoint: format!("http://{}", thread.addr),
            requests,
            _thread: thread,
        }
    }

    /// Endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// An account pointing at the mock (custom endpoint, path-style).
    pub fn account(&self, request_payer: bool) -> Account {
        Account {
            id: "mock-account".to_owned(),
            label: "Mock".to_owned(),
            access_key_id: "AKIDMOCK".to_owned(),
            secret_ref: "mock/secret".to_owned(),
            region: REGION.to_owned(),
            endpoint: Some(self.endpoint.clone()),
            path_style: true,
            request_payer,
            ..Account::default()
        }
    }

    /// A transport for the mock with one attempt per request.
    pub fn ops(&self) -> S3 {
        self.ops_with(
            &ClientOptions {
                max_attempts: 1,
                ..ClientOptions::default()
            },
            false,
        )
    }

    /// A transport for the mock with explicit options.
    pub fn ops_with(&self, options: &ClientOptions, request_payer: bool) -> S3 {
        let credentials = Credentials::new("AKIDMOCK", "mock-secret", None);
        S3::new(&self.account(request_payer), &credentials, options)
            .expect("MockS3: build the S3 client")
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_are_decoded() {
        let request = RecordedRequest {
            method: "GET".into(),
            path: "/b".into(),
            query: "list-type=2&prefix=a%2Fb%20c&delete&max-keys=10&bad=%zz&tail=%4".into(),
            headers: vec![("x-amz-request-payer".into(), "requester".into())],
            body: Vec::new(),
        };
        assert_eq!(request.query_param("prefix").as_deref(), Some("a/b c"));
        assert_eq!(request.query_param("delete").as_deref(), Some(""));
        assert_eq!(request.query_param("max-keys").as_deref(), Some("10"));
        assert_eq!(request.query_param("bad").as_deref(), Some("%zz"));
        assert_eq!(request.query_param("tail").as_deref(), Some("%4"));
        assert_eq!(request.query_param("missing"), None);
        assert_eq!(request.header("X-Amz-Request-Payer"), Some("requester"));
    }

    #[cfg(feature = "minio")]
    #[test]
    fn image_references_split_into_name_and_tag() {
        let split = |reference: &str| {
            let (name, tag) = split_image(reference);
            format!("{name} | {tag}")
        };
        assert_eq!(split("pgsty/minio:RELEASE.1"), "pgsty/minio | RELEASE.1");
        assert_eq!(split("pgsty/minio"), "pgsty/minio | latest");
        assert_eq!(split("registry:5000/minio"), "registry:5000/minio | latest");
        assert_eq!(split("registry:5000/minio:v1"), "registry:5000/minio | v1");
        assert_eq!(split(DEFAULT_MINIO_IMAGE).split(" | ").count(), 2);
    }

    #[test]
    fn random_hex_has_the_requested_length() {
        for len in [1, 8, 16, 40] {
            let value = random_hex(len);
            assert_eq!(value.len(), len);
            assert!(value.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(random_hex(16), random_hex(16));
    }
}
