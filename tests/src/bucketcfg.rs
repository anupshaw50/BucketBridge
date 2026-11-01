//! A stateful emulation of the bucket configuration APIs on top of
//! [`MockS3`], for the tests of `bucketbridge_core::bucketcfg` and the
//! desktop commands built on it.
//!
//! s3s-fs implements none of these APIs, and MinIO only some of them, so
//! there is no default backend to round-trip a policy or a website
//! configuration against. [`BucketConfigMock`] keeps each document a client
//! puts (the policy text, or the XML of the other sections) and hands it back
//! on the next read, exactly as S3 does for these APIs, answers "nothing is
//! configured" with the error codes S3 uses, and can be told to refuse or not
//! implement a section. Every request is recorded by the underlying
//! [`MockS3`], so tests also assert what was sent.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bucketbridge_core::model::BucketKind;
use bucketbridge_core::{S3, aws_sdk_s3};

use crate::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// One configuration section of a bucket, named by its S3 subresource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigKind {
    /// `?policy`, a JSON document.
    Policy,
    /// `?publicAccessBlock`.
    PublicAccessBlock,
    /// `?versioning`.
    Versioning,
    /// `?logging`.
    Logging,
    /// `?website`.
    Website,
    /// `?accelerate`.
    Acceleration,
}

impl ConfigKind {
    /// Every section.
    pub const ALL: [ConfigKind; 6] = [
        ConfigKind::Policy,
        ConfigKind::PublicAccessBlock,
        ConfigKind::Versioning,
        ConfigKind::Logging,
        ConfigKind::Website,
        ConfigKind::Acceleration,
    ];

    /// The query parameter that addresses the section.
    pub fn subresource(self) -> &'static str {
        match self {
            ConfigKind::Policy => "policy",
            ConfigKind::PublicAccessBlock => "publicAccessBlock",
            ConfigKind::Versioning => "versioning",
            ConfigKind::Logging => "logging",
            ConfigKind::Website => "website",
            ConfigKind::Acceleration => "accelerate",
        }
    }

    fn of(request: &RecordedRequest) -> Option<ConfigKind> {
        ConfigKind::ALL
            .into_iter()
            .find(|kind| request.query_param(kind.subresource()).is_some())
    }

    /// What a read answers while nothing is configured: the error S3 uses, or
    /// an empty document for the sections that always answer.
    fn nothing(self) -> MockResponse {
        match self {
            ConfigKind::Policy => MockResponse::error(
                404,
                "NoSuchBucketPolicy",
                "The bucket policy does not exist",
            ),
            ConfigKind::PublicAccessBlock => MockResponse::error(
                404,
                "NoSuchPublicAccessBlockConfiguration",
                "The public access block configuration was not found",
            ),
            ConfigKind::Website => MockResponse::error(
                404,
                "NoSuchWebsiteConfiguration",
                "The specified bucket does not have a website configuration",
            ),
            ConfigKind::Versioning => MockResponse::xml(format!(
                "<VersioningConfiguration {NS}></VersioningConfiguration>"
            )),
            ConfigKind::Logging => {
                MockResponse::xml(format!("<BucketLoggingStatus {NS}></BucketLoggingStatus>"))
            }
            ConfigKind::Acceleration => MockResponse::xml(format!(
                "<AccelerateConfiguration {NS}></AccelerateConfiguration>"
            )),
        }
    }
}

#[derive(Default)]
struct State {
    /// Bucket name to the document of each section that is configured.
    buckets: BTreeMap<String, BTreeMap<ConfigKind, String>>,
    /// Sections refused with `AccessDenied`, per bucket.
    denied: BTreeSet<(String, ConfigKind)>,
    /// Sections the server does not implement, for every bucket.
    unimplemented: BTreeSet<ConfigKind>,
    /// `ListBuckets` is refused with `AccessDenied`.
    listing_denied: bool,
}

/// A mock S3 endpoint that keeps the bucket configuration it is given.
///
/// It answers `ListBuckets` (`GET /`), `HeadBucket`, and `GET`, `PUT` and
/// `DELETE` of the subresources of [`ConfigKind`] on buckets created with
/// [`BucketConfigMock::add_bucket`]; anything else is a 400
/// `UnexpectedRequest`. No authentication is checked.
pub struct BucketConfigMock {
    mock: MockS3,
    state: Arc<Mutex<State>>,
}

impl BucketConfigMock {
    /// An endpoint without buckets.
    pub fn start() -> BucketConfigMock {
        let state: Arc<Mutex<State>> = Arc::default();
        let shared = Arc::clone(&state);
        let mock = MockS3::start(move |request| answer(&mut lock(&shared), request));
        BucketConfigMock { mock, state }
    }

    /// An endpoint with these buckets.
    pub fn with_buckets(names: &[&str]) -> BucketConfigMock {
        let server = BucketConfigMock::start();
        for name in names {
            server.add_bucket(name);
        }
        server
    }

    /// Creates a bucket with nothing configured.
    pub fn add_bucket(&self, name: &str) {
        lock(&self.state)
            .buckets
            .entry(name.to_owned())
            .or_default();
    }

    /// Makes reading and writing `kind` of `bucket` fail with `AccessDenied`.
    pub fn deny(&self, bucket: &str, kind: ConfigKind) {
        lock(&self.state).denied.insert((bucket.to_owned(), kind));
    }

    /// Makes the server answer `NotImplemented` for `kind` on every bucket.
    pub fn not_implemented(&self, kind: ConfigKind) {
        lock(&self.state).unimplemented.insert(kind);
    }

    /// Makes `ListBuckets` fail with `AccessDenied`, as for a user without
    /// `s3:ListAllMyBuckets`.
    pub fn deny_listing(&self) {
        lock(&self.state).listing_denied = true;
    }

    /// The document stored for a section: the policy text, or the XML.
    pub fn stored(&self, bucket: &str, kind: ConfigKind) -> Option<String> {
        lock(&self.state)
            .buckets
            .get(bucket)
            .and_then(|docs| docs.get(&kind))
            .cloned()
    }

    /// Stores a document as if a client had put it, for the shapes real
    /// servers answer with.
    pub fn store(&self, bucket: &str, kind: ConfigKind, document: &str) {
        lock(&self.state)
            .buckets
            .entry(bucket.to_owned())
            .or_default()
            .insert(kind, document.to_owned());
    }

    /// The underlying scripted endpoint.
    pub fn mock(&self) -> &MockS3 {
        &self.mock
    }

    /// Endpoint URL.
    pub fn endpoint(&self) -> &str {
        self.mock.endpoint()
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.mock.requests()
    }

    /// A transport for an account with a custom endpoint (one attempt per
    /// request).
    pub fn ops(&self) -> S3 {
        self.mock.ops()
    }

    /// A transport that treats the mock as AWS: bucket kind
    /// [`BucketKind::Aws`] (so transfer acceleration is supported) without
    /// bucket-region resolution. `request_payer` is the account's
    /// requester-pays setting.
    pub fn aws_ops(&self, request_payer: bool) -> S3 {
        let client: aws_sdk_s3::Client = self.mock.ops().client().clone();
        S3::from_client(client, request_payer, BucketKind::Aws).with_region_resolution(false)
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unexpected(request: &RecordedRequest) -> MockResponse {
    MockResponse::error(
        400,
        "UnexpectedRequest",
        &format!(
            "unexpected {} {}?{}",
            request.method, request.path, request.query
        ),
    )
}

fn answer(state: &mut State, request: &RecordedRequest) -> MockResponse {
    let name = request.path.trim_matches('/');
    if name.is_empty() {
        return list_buckets(state, request);
    }
    if name.contains('/') {
        return unexpected(request);
    }
    if !state.buckets.contains_key(name) {
        return if request.method == "HEAD" {
            MockResponse::empty(404)
        } else {
            MockResponse::error(404, "NoSuchBucket", "The specified bucket does not exist")
        };
    }
    let Some(kind) = ConfigKind::of(request) else {
        return if request.method == "HEAD" && request.query.is_empty() {
            MockResponse::empty(200)
        } else {
            unexpected(request)
        };
    };
    if state.unimplemented.contains(&kind) {
        return MockResponse::error(
            501,
            "NotImplemented",
            "A header you provided implies functionality that is not implemented",
        );
    }
    if state.denied.contains(&(name.to_owned(), kind)) {
        return MockResponse::error(403, "AccessDenied", "Access Denied");
    }
    let Some(docs) = state.buckets.get_mut(name) else {
        return unexpected(request);
    };
    match request.method.as_str() {
        "GET" => match docs.get(&kind) {
            None => kind.nothing(),
            Some(document) if kind == ConfigKind::Policy => MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: document.clone(),
            },
            Some(document) => MockResponse::xml(document.clone()),
        },
        "PUT" => {
            docs.insert(kind, request.body_text());
            MockResponse::empty(if kind == ConfigKind::Policy { 204 } else { 200 })
        }
        "DELETE" => {
            docs.remove(&kind);
            MockResponse::empty(204)
        }
        _ => unexpected(request),
    }
}

fn list_buckets(state: &State, request: &RecordedRequest) -> MockResponse {
    if request.method != "GET" {
        return unexpected(request);
    }
    if state.listing_denied {
        return MockResponse::error(403, "AccessDenied", "Access Denied");
    }
    let buckets: String = state
        .buckets
        .keys()
        .map(|name| {
            format!(
                "<Bucket><Name>{name}</Name><CreationDate>2026-10-29T08:15:00.000Z</CreationDate></Bucket>"
            )
        })
        .collect();
    MockResponse::xml(format!(
        "<ListAllMyBucketsResult {NS}><Owner><ID>owner-id</ID><DisplayName>owner</DisplayName></Owner><Buckets>{buckets}</Buckets></ListAllMyBucketsResult>"
    ))
}
