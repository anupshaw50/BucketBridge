//! A stateful emulation of the bucket rule APIs (lifecycle,
//! Intelligent-Tiering, CORS, replication and the versioning read that
//! replication needs) on top of [`MockS3`], for the tests of
//! `bucketbridge_core::bucketrules` and the desktop commands built on it.
//!
//! s3s-fs implements none of these APIs. The mock keeps the XML a client puts
//! and hands it back on the next read, answers "nothing is configured" with
//! the error codes S3 uses, and can be told to refuse or not implement a
//! section. Every request is recorded by the underlying [`MockS3`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use bucketbridge_core::S3;

use crate::{MockResponse, MockS3, RecordedRequest};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

#[derive(Default)]
struct State {
    /// `(bucket, key)` to the stored document; key is the subresource, with
    /// `intelligent-tiering/<id>` for tiering configurations.
    docs: BTreeMap<(String, String), String>,
    versioning: String,
    unimplemented: BTreeSet<&'static str>,
    denied: BTreeSet<&'static str>,
}

/// A mock S3 endpoint that keeps the rule documents it is given. No
/// authentication is checked and any bucket name is accepted.
pub struct RulesMock {
    mock: MockS3,
    state: Arc<Mutex<State>>,
}

const SUBRESOURCES: [&str; 5] = [
    "lifecycle",
    "cors",
    "replication",
    "intelligent-tiering",
    "versioning",
];

fn absent(subresource: &str) -> MockResponse {
    let (code, message) = match subresource {
        "lifecycle" => (
            "NoSuchLifecycleConfiguration",
            "The lifecycle configuration does not exist",
        ),
        "cors" => (
            "NoSuchCORSConfiguration",
            "The CORS configuration does not exist",
        ),
        "replication" => (
            "ReplicationConfigurationNotFoundError",
            "The replication configuration was not found",
        ),
        _ => (
            "NoSuchConfiguration",
            "The specified configuration does not exist",
        ),
    };
    MockResponse::error(404, code, message)
}

fn answer(state: &mut State, request: &RecordedRequest) -> MockResponse {
    let bucket = request.path.trim_matches('/').to_owned();
    let Some(subresource) = SUBRESOURCES
        .into_iter()
        .find(|name| request.query_param(name).is_some())
    else {
        return if request.method == "HEAD" {
            MockResponse::empty(200)
        } else {
            MockResponse::error(400, "UnexpectedRequest", &request.query)
        };
    };
    if state.unimplemented.contains(subresource) {
        return MockResponse::error(501, "NotImplemented", "Not implemented");
    }
    if state.denied.contains(subresource) {
        return MockResponse::error(403, "AccessDenied", "Access Denied");
    }
    if subresource == "versioning" {
        return MockResponse::xml(format!(
            "<VersioningConfiguration {NS}>{}</VersioningConfiguration>",
            state.versioning
        ));
    }
    let id = request.query_param("id");
    let key = match (&id, subresource) {
        (Some(id), "intelligent-tiering") => format!("{subresource}/{id}"),
        _ => subresource.to_owned(),
    };
    match request.method.as_str() {
        "GET" if subresource == "intelligent-tiering" && id.is_none() => {
            let items: String = state
                .docs
                .iter()
                .filter(|((b, k), _)| *b == bucket && k.starts_with("intelligent-tiering/"))
                .map(|(_, doc)| doc.clone())
                .collect();
            MockResponse::xml(format!(
                "<ListBucketIntelligentTieringConfigurationsOutput {NS}><IsTruncated>false</IsTruncated>{items}</ListBucketIntelligentTieringConfigurationsOutput>"
            ))
        }
        "GET" => match state.docs.get(&(bucket, key)) {
            Some(doc) => MockResponse::xml(doc.clone()),
            None => absent(subresource),
        },
        "PUT" => {
            state.docs.insert((bucket, key), request.body_text());
            MockResponse::empty(200)
        }
        "DELETE" => {
            state.docs.remove(&(bucket, key));
            MockResponse::empty(204)
        }
        _ => MockResponse::error(400, "UnexpectedRequest", &request.method),
    }
}

impl RulesMock {
    /// An endpoint with nothing configured and versioning off.
    pub fn start() -> RulesMock {
        let state: Arc<Mutex<State>> = Arc::default();
        let shared = Arc::clone(&state);
        let mock = MockS3::start(move |request| {
            answer(
                &mut shared.lock().unwrap_or_else(PoisonError::into_inner),
                request,
            )
        });
        RulesMock { mock, state }
    }

    fn with<R>(&self, change: impl FnOnce(&mut State) -> R) -> R {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Sets the versioning answer: `""` (never enabled), `"Enabled"` or
    /// `"Suspended"`.
    pub fn set_versioning(&self, status: &str) {
        self.with(|s| {
            s.versioning = if status.is_empty() {
                String::new()
            } else {
                format!("<Status>{status}</Status>")
            };
        });
    }

    /// Makes the server answer `NotImplemented` for a subresource
    /// (`lifecycle`, `cors`, `replication`, `intelligent-tiering`).
    pub fn not_implemented(&self, subresource: &'static str) {
        self.with(|s| s.unimplemented.insert(subresource));
    }

    /// Makes the server refuse a subresource with `AccessDenied`.
    pub fn deny(&self, subresource: &'static str) {
        self.with(|s| s.denied.insert(subresource));
    }

    /// Endpoint URL.
    pub fn endpoint(&self) -> &str {
        self.mock.endpoint()
    }

    /// A transport for an account with a custom endpoint.
    pub fn ops(&self) -> S3 {
        self.mock.ops()
    }

    /// The stored document of a subresource (`intelligent-tiering/<id>` for
    /// tiering configurations).
    pub fn stored(&self, bucket: &str, key: &str) -> Option<String> {
        self.with(|s| s.docs.get(&(bucket.to_owned(), key.to_owned())).cloned())
    }

    /// Stores a document as if a client had put it.
    pub fn store(&self, bucket: &str, key: &str, document: &str) {
        self.with(|s| {
            s.docs
                .insert((bucket.to_owned(), key.to_owned()), document.to_owned())
        });
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.mock.requests()
    }
}
