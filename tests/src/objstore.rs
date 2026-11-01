//! [`ObjectStore`]: a small in-memory S3 for the object-metadata tests.
//!
//! The default backend (s3s-fs) implements neither tagging, ACLs nor
//! encryption (see `docs/testing.md`), and a scripted [`MockS3`] answers
//! fixed replies. This store sits in between: it keeps objects with their
//! headers, user metadata, tags, storage class, encryption and ACL, and
//! answers the requests of `bucketbridge_core::objectmeta` the way S3 does,
//! including the details that make in-place edits tricky:
//!
//! * `CopyObject` resets the storage class, the encryption and the ACL of the
//!   new object unless the request sends them again, never copies the website
//!   redirect, and refuses to copy an object onto itself without a change;
//! * `MetadataDirective: REPLACE` drops every header the request leaves out
//!   (`Content-Type` falls back to `binary/octet-stream`);
//! * objects encrypted with SSE-C can only be read or copied with their key:
//!   no key is HTTP 400, a wrong key HTTP 403, and a key for an object that
//!   has none HTTP 400;
//! * `x-amz-copy-source-if-match` fails with HTTP 412 when the ETag differs.
//!
//! Start it with [`ObjectStore::start`]; every request is recorded by the
//! returned [`MockS3`] for header assertions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use md5::{Digest, Md5};

use crate::{MockResponse, MockS3, RecordedRequest, percent_decode};

const ALL_USERS: &str = "http://acs.amazonaws.com/groups/global/AllUsers";
const AUTHENTICATED_USERS: &str = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";
const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const SSE_C: &str = "x-amz-server-side-encryption-customer-";
const SSE_C_SOURCE: &str = "x-amz-copy-source-server-side-encryption-customer-";
const LAST_MODIFIED: &str = "Thu, 29 Oct 2026 08:15:00 GMT";

/// The account that owns every object.
pub const OWNER_ID: &str = "owner-canonical-id";

/// The owner of the buckets, for the `bucket-owner-*` canned ACLs.
pub const BUCKET_OWNER_ID: &str = "bucket-owner-canonical-id";

/// How a stored object is encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StoredEncryption {
    /// Not encrypted (and nothing reported).
    #[default]
    None,
    /// SSE-S3.
    S3,
    /// SSE-KMS.
    Kms {
        /// The key ID or ARN.
        key_id: String,
        /// Whether a bucket key is used.
        bucket_key: bool,
    },
    /// SSE-C, identified by the base64 MD5 of its key.
    Customer {
        /// The base64 MD5 digest of the key.
        key_md5: String,
    },
}

/// One grant of a stored ACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredGrantee {
    /// A canonical user.
    User(String),
    /// A predefined group, by URI.
    Group(String),
}

/// An object of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    /// Size in bytes (the store keeps no content).
    pub size: u64,
    /// ETag, with quotes.
    pub etag: String,
    /// `x-amz-storage-class`; `None` is `STANDARD`.
    pub storage_class: Option<String>,
    /// `Content-Type`; `None` reads as `binary/octet-stream`.
    pub content_type: Option<String>,
    /// `Cache-Control`.
    pub cache_control: Option<String>,
    /// `Content-Disposition`.
    pub content_disposition: Option<String>,
    /// `Content-Encoding`.
    pub content_encoding: Option<String>,
    /// `Content-Language`.
    pub content_language: Option<String>,
    /// `Expires`, as an HTTP date.
    pub expires: Option<String>,
    /// `x-amz-website-redirect-location`.
    pub website_redirect: Option<String>,
    /// User metadata, keys in lowercase.
    pub user_meta: BTreeMap<String, String>,
    /// Tags.
    pub tags: BTreeMap<String, String>,
    /// Encryption.
    pub encryption: StoredEncryption,
    /// ACL grants.
    pub acl: Vec<(StoredGrantee, String)>,
    /// `x-amz-restore`.
    pub restore: Option<String>,
}

impl StoredObject {
    /// A private, unencrypted `STANDARD` object of `size` bytes.
    pub fn new(size: u64) -> Self {
        StoredObject {
            size,
            etag: "\"00000000000000000000000000000000\"".to_owned(),
            storage_class: None,
            content_type: None,
            cache_control: None,
            content_disposition: None,
            content_encoding: None,
            content_language: None,
            expires: None,
            website_redirect: None,
            user_meta: BTreeMap::new(),
            tags: BTreeMap::new(),
            encryption: StoredEncryption::None,
            acl: private_acl(),
            restore: None,
        }
    }

    /// Sets the storage class (`STANDARD` is stored as no class: S3 never
    /// reports it).
    #[must_use]
    pub fn class(mut self, class: &str) -> Self {
        self.storage_class = stored_class(Some(class));
        self
    }

    /// Sets the content type.
    #[must_use]
    pub fn content_type(mut self, value: &str) -> Self {
        self.content_type = Some(value.to_owned());
        self
    }

    /// Adds user metadata.
    #[must_use]
    pub fn meta(mut self, key: &str, value: &str) -> Self {
        self.user_meta.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Adds a tag.
    #[must_use]
    pub fn tag(mut self, key: &str, value: &str) -> Self {
        self.tags.insert(key.to_owned(), value.to_owned());
        self
    }

    /// Sets the encryption.
    #[must_use]
    pub fn encrypted(mut self, encryption: StoredEncryption) -> Self {
        self.encryption = encryption;
        self
    }

    /// Encrypts the object with the SSE-C key `key` (32 raw bytes).
    #[must_use]
    pub fn encrypted_with_key(self, key: &[u8; 32]) -> Self {
        self.encrypted(StoredEncryption::Customer {
            key_md5: BASE64.encode(Md5::digest(key)),
        })
    }

    /// Sets the website redirect location.
    #[must_use]
    pub fn redirect(mut self, location: &str) -> Self {
        self.website_redirect = Some(location.to_owned());
        self
    }

    /// Sets the ACL to public-read.
    #[must_use]
    pub fn public_read(mut self) -> Self {
        self.acl = canned_acl("public-read");
        self
    }

    /// Sets `x-amz-restore`.
    #[must_use]
    pub fn restore_header(mut self, value: &str) -> Self {
        self.restore = Some(value.to_owned());
        self
    }
}

type Intercept = dyn Fn(&RecordedRequest) -> Option<MockResponse> + Send + Sync;

struct Upload {
    bucket: String,
    key: String,
    /// The object the upload will become, without its size.
    template: StoredObject,
    customer_md5: Option<String>,
    bytes: u64,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<(String, String), StoredObject>,
    acls_disabled: HashSet<String>,
    uploads: HashMap<String, Upload>,
    intercepts: Vec<Arc<Intercept>>,
    counter: u64,
}

/// The in-memory S3; see the [module documentation](self).
#[derive(Default)]
pub struct ObjectStore {
    state: Mutex<State>,
}

impl ObjectStore {
    /// An empty store.
    pub fn new() -> Arc<Self> {
        Arc::new(ObjectStore::default())
    }

    /// Starts a [`MockS3`] endpoint served by this store.
    pub fn start(self: &Arc<Self>) -> MockS3 {
        let store = Arc::clone(self);
        MockS3::start(move |request| store.handle(request))
    }

    /// Stores an object (with a new ETag), replacing any object with that key.
    pub fn put(&self, bucket: &str, key: &str, mut object: StoredObject) {
        let mut state = self.lock();
        state.counter += 1;
        object.etag = format!("\"{:032x}\"", state.counter);
        state
            .objects
            .insert((bucket.to_owned(), key.to_owned()), object);
    }

    /// The object with that key.
    pub fn object(&self, bucket: &str, key: &str) -> Option<StoredObject> {
        self.lock()
            .objects
            .get(&(bucket.to_owned(), key.to_owned()))
            .cloned()
    }

    /// Makes the bucket reject every ACL change, as a bucket with Object
    /// Ownership BucketOwnerEnforced does.
    pub fn disable_acls(&self, bucket: &str) {
        self.lock().acls_disabled.insert(bucket.to_owned());
    }

    /// Answers requests for which `answer` returns a reply instead of
    /// handling them (fault injection). The first interceptor that answers
    /// wins.
    pub fn intercept<F>(&self, answer: F)
    where
        F: Fn(&RecordedRequest) -> Option<MockResponse> + Send + Sync + 'static,
    {
        self.lock().intercepts.push(Arc::new(answer));
    }

    /// Fails every `method` request for `key` (`"*"` matches every method)
    /// with an S3 error.
    pub fn fail(&self, method: &'static str, key: &'static str, status: u16, code: &'static str) {
        self.intercept(move |request| {
            let (_, target) = split_path(request);
            ((method == "*" || request.method == method) && target == key)
                .then(|| MockResponse::error(status, code, "injected failure"))
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers one request.
    pub fn handle(&self, request: &RecordedRequest) -> MockResponse {
        let intercepts: Vec<Arc<Intercept>> = self.lock().intercepts.clone();
        for intercept in intercepts {
            if let Some(reply) = intercept(request) {
                return reply;
            }
        }
        let (bucket, key) = split_path(request);
        let has = |name: &str| request.query_param(name).is_some();
        let method = request.method.as_str();
        let copy_source = request.header("x-amz-copy-source").is_some();
        match method {
            "GET" if key.is_empty() && has("list-type") => self.list(&bucket, request),
            "HEAD" if !key.is_empty() => self.head(&bucket, &key, request),
            "GET" if has("tagging") => self.get_tags(&bucket, &key),
            "PUT" if has("tagging") => self.put_tags(&bucket, &key, request),
            "DELETE" if has("tagging") => self.delete_tags(&bucket, &key),
            "GET" if has("acl") => self.get_acl(&bucket, &key),
            "PUT" if has("acl") => self.put_acl(&bucket, &key, request),
            "POST" if has("uploads") => self.create_upload(&bucket, &key, request),
            "PUT" if has("partNumber") && copy_source => self.copy_part(&bucket, &key, request),
            "POST" if has("uploadId") => self.complete_upload(&bucket, &key, request),
            "DELETE" if has("uploadId") => self.abort_upload(request),
            "PUT" if copy_source => self.copy(&bucket, &key, request),
            _ => error(
                400,
                "UnexpectedRequest",
                &format!("{method} {}", request.path),
            ),
        }
    }

    fn list(&self, bucket: &str, request: &RecordedRequest) -> MockResponse {
        let prefix = request.query_param("prefix").unwrap_or_default();
        let state = self.lock();
        let contents: String = state
            .objects
            .iter()
            .filter(|((b, k), _)| b == bucket && k.starts_with(&prefix))
            .map(|((_, k), o)| {
                format!(
                    "<Contents><Key>{}</Key><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>{}</StorageClass></Contents>",
                    escape(k),
                    escape(&o.etag),
                    o.size,
                    o.storage_class.as_deref().unwrap_or("STANDARD")
                )
            })
            .collect();
        MockResponse::xml(format!(
            "<ListBucketResult {NS}><Name>{bucket}</Name><IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
        ))
    }

    fn head(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        let state = self.lock();
        let Some(object) = state.objects.get(&(bucket.to_owned(), key.to_owned())) else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        if let Err(reply) = check_customer_key(request, SSE_C, &object.encryption) {
            return reply;
        }
        let mut reply = MockResponse::empty(200)
            .with_header("content-length", &object.size.to_string())
            .with_header("etag", &object.etag)
            .with_header("last-modified", LAST_MODIFIED)
            .with_header(
                "content-type",
                object
                    .content_type
                    .as_deref()
                    .unwrap_or("binary/octet-stream"),
            );
        for (name, value) in [
            ("x-amz-storage-class", &object.storage_class),
            ("cache-control", &object.cache_control),
            ("content-disposition", &object.content_disposition),
            ("content-encoding", &object.content_encoding),
            ("content-language", &object.content_language),
            ("expires", &object.expires),
            ("x-amz-website-redirect-location", &object.website_redirect),
            ("x-amz-restore", &object.restore),
        ] {
            if let Some(value) = value {
                reply = reply.with_header(name, value);
            }
        }
        for (name, value) in &object.user_meta {
            reply = reply.with_header(&format!("x-amz-meta-{name}"), value);
        }
        match &object.encryption {
            StoredEncryption::None => {}
            StoredEncryption::S3 => {
                reply = reply.with_header("x-amz-server-side-encryption", "AES256");
            }
            StoredEncryption::Kms { key_id, bucket_key } => {
                reply = reply
                    .with_header("x-amz-server-side-encryption", "aws:kms")
                    .with_header("x-amz-server-side-encryption-aws-kms-key-id", key_id)
                    .with_header(
                        "x-amz-server-side-encryption-bucket-key-enabled",
                        &bucket_key.to_string(),
                    );
            }
            StoredEncryption::Customer { key_md5 } => {
                reply = reply
                    .with_header(&format!("{SSE_C}algorithm"), "AES256")
                    .with_header(&format!("{SSE_C}key-MD5"), key_md5);
            }
        }
        reply
    }

    fn get_tags(&self, bucket: &str, key: &str) -> MockResponse {
        let state = self.lock();
        let Some(object) = state.objects.get(&(bucket.to_owned(), key.to_owned())) else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        let tags: String = object
            .tags
            .iter()
            .map(|(k, v)| {
                format!(
                    "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                    escape(k),
                    escape(v)
                )
            })
            .collect();
        MockResponse::xml(format!("<Tagging {NS}><TagSet>{tags}</TagSet></Tagging>"))
    }

    fn put_tags(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        let mut state = self.lock();
        let Some(object) = state.objects.get_mut(&(bucket.to_owned(), key.to_owned())) else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        object.tags = parse_tag_body(&request.body_text());
        MockResponse::empty(200)
    }

    fn delete_tags(&self, bucket: &str, key: &str) -> MockResponse {
        let mut state = self.lock();
        match state.objects.get_mut(&(bucket.to_owned(), key.to_owned())) {
            Some(object) => {
                object.tags.clear();
                MockResponse::empty(204)
            }
            None => error(404, "NoSuchKey", "The specified key does not exist."),
        }
    }

    fn get_acl(&self, bucket: &str, key: &str) -> MockResponse {
        let state = self.lock();
        let Some(object) = state.objects.get(&(bucket.to_owned(), key.to_owned())) else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        // A bucket with ACLs disabled reports the default ACL.
        let acl = if state.acls_disabled.contains(bucket) {
            private_acl()
        } else {
            object.acl.clone()
        };
        let grants: String = acl
            .iter()
            .map(|(grantee, permission)| match grantee {
                StoredGrantee::User(id) => format!(
                    "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>{id}</ID><DisplayName>{id}</DisplayName></Grantee><Permission>{permission}</Permission></Grant>"
                ),
                StoredGrantee::Group(uri) => format!(
                    "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"Group\"><URI>{uri}</URI></Grantee><Permission>{permission}</Permission></Grant>"
                ),
            })
            .collect();
        MockResponse::xml(format!(
            "<AccessControlPolicy {NS}><Owner><ID>{OWNER_ID}</ID><DisplayName>{OWNER_ID}</DisplayName></Owner><AccessControlList>{grants}</AccessControlList></AccessControlPolicy>"
        ))
    }

    fn put_acl(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        let mut state = self.lock();
        if state.acls_disabled.contains(bucket) {
            return acl_disabled();
        }
        let Some(object) = state.objects.get_mut(&(bucket.to_owned(), key.to_owned())) else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        object.acl = match request.header("x-amz-acl") {
            Some(canned) => canned_acl(canned),
            None => parse_acl_body(&request.body_text()),
        };
        MockResponse::empty(200)
    }

    fn copy(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        let mut state = self.lock();
        let source = request.header("x-amz-copy-source").unwrap_or_default();
        let Some((source_bucket, source_key)) = parse_copy_source(source) else {
            return error(400, "InvalidArgument", "invalid copy source");
        };
        let Some(old) = state
            .objects
            .get(&(source_bucket.clone(), source_key.clone()))
            .cloned()
        else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        if let Err(reply) = check_customer_key(request, SSE_C_SOURCE, &old.encryption)
            .and_then(|()| customer_key_md5(request, SSE_C).map(drop))
        {
            return reply;
        }
        if let Some(wanted) = request.header("x-amz-copy-source-if-match")
            && wanted.trim_matches('"') != old.etag.trim_matches('"')
        {
            return error(
                412,
                "PreconditionFailed",
                "At least one of the pre-conditions you specified did not hold",
            );
        }
        let replace = request.header("x-amz-metadata-directive") == Some("REPLACE");
        let same = (source_bucket.as_str(), source_key.as_str()) == (bucket, key);
        let changes_something = replace
            || request.header("x-amz-storage-class").is_some()
            || request.header("x-amz-website-redirect-location").is_some()
            || request.header("x-amz-server-side-encryption").is_some()
            || request.header(&format!("{SSE_C}algorithm")).is_some();
        if same && !changes_something {
            return error(
                400,
                "InvalidRequest",
                "This copy request is illegal because it is trying to copy an object to itself without changing the object's metadata, storage class, website redirect location or encryption attributes.",
            );
        }
        let wants_acl = request.header("x-amz-acl").is_some();
        if wants_acl && state.acls_disabled.contains(bucket) {
            return acl_disabled();
        }
        let mut new = StoredObject::new(old.size);
        if replace {
            apply_request_headers(&mut new, request);
        } else {
            new.content_type = old.content_type.clone();
            new.cache_control = old.cache_control.clone();
            new.content_disposition = old.content_disposition.clone();
            new.content_encoding = old.content_encoding.clone();
            new.content_language = old.content_language.clone();
            new.expires = old.expires.clone();
            new.user_meta = old.user_meta.clone();
        }
        // The redirect, the storage class and the encryption come from the
        // request only.
        new.website_redirect = request
            .header("x-amz-website-redirect-location")
            .map(str::to_owned);
        new.storage_class = stored_class(request.header("x-amz-storage-class"));
        new.encryption = request_encryption(request);
        new.tags = if request.header("x-amz-tagging-directive") == Some("REPLACE") {
            parse_tagging_header(request.header("x-amz-tagging").unwrap_or_default())
        } else {
            old.tags.clone()
        };
        state.counter += 1;
        new.etag = format!("\"{:032x}\"", state.counter);
        let etag = new.etag.clone();
        state
            .objects
            .insert((bucket.to_owned(), key.to_owned()), new);
        MockResponse::xml(format!(
            "<CopyObjectResult {NS}><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>{}</ETag></CopyObjectResult>",
            escape(&etag)
        ))
    }

    fn create_upload(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        if let Err(reply) = customer_key_md5(request, SSE_C) {
            return reply;
        }
        let mut state = self.lock();
        let mut template = StoredObject::new(0);
        apply_request_headers(&mut template, request);
        template.website_redirect = request
            .header("x-amz-website-redirect-location")
            .map(str::to_owned);
        template.storage_class = stored_class(request.header("x-amz-storage-class"));
        template.encryption = request_encryption(request);
        template.tags = parse_tagging_header(request.header("x-amz-tagging").unwrap_or_default());
        let customer_md5 = match &template.encryption {
            StoredEncryption::Customer { key_md5 } => Some(key_md5.clone()),
            _ => None,
        };
        state.counter += 1;
        let id = format!("UPLOAD-{}", state.counter);
        state.uploads.insert(
            id.clone(),
            Upload {
                bucket: bucket.to_owned(),
                key: key.to_owned(),
                template,
                customer_md5,
                bytes: 0,
            },
        );
        MockResponse::xml(format!(
            "<InitiateMultipartUploadResult {NS}><Bucket>{bucket}</Bucket><Key>{}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>",
            escape(key)
        ))
    }

    fn copy_part(&self, _bucket: &str, _key: &str, request: &RecordedRequest) -> MockResponse {
        let mut state = self.lock();
        let id = request.query_param("uploadId").unwrap_or_default();
        let source = request.header("x-amz-copy-source").unwrap_or_default();
        let Some((source_bucket, source_key)) = parse_copy_source(source) else {
            return error(400, "InvalidArgument", "invalid copy source");
        };
        let Some(old) = state.objects.get(&(source_bucket, source_key)).cloned() else {
            return error(404, "NoSuchKey", "The specified key does not exist.");
        };
        if let Err(reply) = check_customer_key(request, SSE_C_SOURCE, &old.encryption) {
            return reply;
        }
        if let Some(wanted) = request.header("x-amz-copy-source-if-match")
            && wanted.trim_matches('"') != old.etag.trim_matches('"')
        {
            return error(412, "PreconditionFailed", "the source changed");
        }
        let Some(upload) = state.uploads.get_mut(&id) else {
            return error(404, "NoSuchUpload", "The specified upload does not exist.");
        };
        // Every part of an SSE-C upload carries the key of the upload.
        if let Some(md5) = &upload.customer_md5 {
            let sent = request.header(&format!("{SSE_C}key-MD5"));
            if sent != Some(md5.as_str()) {
                return error(
                    400,
                    "InvalidRequest",
                    "the SSE-C headers of the part do not match the upload",
                );
            }
        }
        let range = request
            .header("x-amz-copy-source-range")
            .and_then(|range| range.strip_prefix("bytes="))
            .and_then(|range| range.split_once('-'))
            .and_then(|(start, end)| Some((start.parse::<u64>().ok()?, end.parse::<u64>().ok()?)));
        let Some((start, end)) = range else {
            return error(400, "InvalidArgument", "a part needs a byte range");
        };
        upload.bytes += end - start + 1;
        let number = request.query_param("partNumber").unwrap_or_default();
        MockResponse::xml(format!(
            "<CopyPartResult {NS}><ETag>\"part-{number}\"</ETag></CopyPartResult>"
        ))
    }

    fn complete_upload(&self, bucket: &str, key: &str, request: &RecordedRequest) -> MockResponse {
        let mut state = self.lock();
        let id = request.query_param("uploadId").unwrap_or_default();
        let Some(upload) = state.uploads.remove(&id) else {
            return error(404, "NoSuchUpload", "The specified upload does not exist.");
        };
        let parts = request.body_text().matches("<Part>").count();
        let mut object = upload.template;
        object.size = upload.bytes;
        state.counter += 1;
        object.etag = format!("\"{:032x}-{parts}\"", state.counter);
        let etag = object.etag.clone();
        state.objects.insert((upload.bucket, upload.key), object);
        MockResponse::xml(format!(
            "<CompleteMultipartUploadResult {NS}><Bucket>{bucket}</Bucket><Key>{}</Key><ETag>{}</ETag></CompleteMultipartUploadResult>",
            escape(key),
            escape(&etag)
        ))
    }

    fn abort_upload(&self, request: &RecordedRequest) -> MockResponse {
        let id = request.query_param("uploadId").unwrap_or_default();
        self.lock().uploads.remove(&id);
        MockResponse::empty(204)
    }
}

/// `(bucket, key)` of a path-style request path; the key is percent-decoded.
fn split_path(request: &RecordedRequest) -> (String, String) {
    let path = request.path.trim_start_matches('/');
    let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
    (bucket.to_owned(), percent_decode(key))
}

fn parse_copy_source(source: &str) -> Option<(String, String)> {
    let source = source.split('?').next().unwrap_or_default();
    let source = percent_decode(source.trim_start_matches('/'));
    let (bucket, key) = source.split_once('/')?;
    Some((bucket.to_owned(), key.to_owned()))
}

fn error(status: u16, code: &str, message: &str) -> MockResponse {
    MockResponse::error(status, code, message)
}

fn acl_disabled() -> MockResponse {
    error(
        400,
        "AccessControlListNotSupported",
        "The bucket does not allow ACLs",
    )
}

/// The MD5 of the SSE-C key in the headers that start with `prefix`, after
/// checking that they are well formed: the algorithm is `AES256`, the key is
/// 32 bytes of base64, and the MD5 header is the MD5 of that key. `None`
/// when the request has no such headers.
fn customer_key_md5(
    request: &RecordedRequest,
    prefix: &str,
) -> Result<Option<String>, MockResponse> {
    let algorithm = request.header(&format!("{prefix}algorithm"));
    let key = request.header(&format!("{prefix}key"));
    let md5 = request.header(&format!("{prefix}key-MD5"));
    if algorithm.is_none() && key.is_none() && md5.is_none() {
        return Ok(None);
    }
    let decoded = key.and_then(|key| BASE64.decode(key).ok());
    let well_formed = algorithm == Some("AES256")
        && decoded.as_ref().is_some_and(|bytes| bytes.len() == 32)
        && decoded
            .as_ref()
            .map(|bytes| BASE64.encode(Md5::digest(bytes)))
            .as_deref()
            == md5;
    if well_formed {
        Ok(md5.map(str::to_owned))
    } else {
        Err(error(
            400,
            "InvalidArgument",
            "The calculated MD5 hash of the key did not match the hash that was provided.",
        ))
    }
}

/// The reply of S3 when a request about an SSE-C object has the wrong key
/// headers; `Ok` when it has the right ones, or the object has none.
fn check_customer_key(
    request: &RecordedRequest,
    prefix: &str,
    stored: &StoredEncryption,
) -> Result<(), MockResponse> {
    let supplied = customer_key_md5(request, prefix)?;
    match (stored, supplied) {
        (StoredEncryption::Customer { .. }, None) => Err(error(
            400,
            "InvalidRequest",
            "The object was stored using a form of Server Side Encryption. The correct parameters must be provided to retrieve the object.",
        )),
        (StoredEncryption::Customer { key_md5 }, Some(md5)) if *key_md5 != md5 => {
            Err(error(403, "AccessDenied", "Access Denied"))
        }
        (StoredEncryption::Customer { .. }, Some(_)) => Ok(()),
        (_, Some(_)) => Err(error(
            400,
            "InvalidRequest",
            "The encryption parameters are not applicable to this object.",
        )),
        (_, None) => Ok(()),
    }
}

/// The encryption a write request asks for.
fn request_encryption(request: &RecordedRequest) -> StoredEncryption {
    if let Some(md5) = request.header(&format!("{SSE_C}key-MD5")) {
        return StoredEncryption::Customer {
            key_md5: md5.to_owned(),
        };
    }
    match request.header("x-amz-server-side-encryption") {
        Some("AES256") => StoredEncryption::S3,
        Some(_) => StoredEncryption::Kms {
            key_id: request
                .header("x-amz-server-side-encryption-aws-kms-key-id")
                .unwrap_or("aws/s3")
                .to_owned(),
            bucket_key: request.header("x-amz-server-side-encryption-bucket-key-enabled")
                == Some("true"),
        },
        None => StoredEncryption::None,
    }
}

/// Takes the content headers and user metadata of a request.
fn apply_request_headers(object: &mut StoredObject, request: &RecordedRequest) {
    let get = |name: &str| request.header(name).map(str::to_owned);
    object.content_type = get("content-type");
    object.cache_control = get("cache-control");
    object.content_disposition = get("content-disposition");
    object.content_encoding = get("content-encoding");
    object.content_language = get("content-language");
    object.expires = get("expires");
    object.user_meta = request
        .headers
        .iter()
        .filter_map(|(name, value)| {
            name.strip_prefix("x-amz-meta-")
                .map(|key| (key.to_owned(), value.clone()))
        })
        .collect();
}

/// S3 omits the class of `STANDARD` objects from its answers.
fn stored_class(class: Option<&str>) -> Option<String> {
    class
        .filter(|class| *class != "STANDARD")
        .map(str::to_owned)
}

fn private_acl() -> Vec<(StoredGrantee, String)> {
    vec![(
        StoredGrantee::User(OWNER_ID.to_owned()),
        "FULL_CONTROL".to_owned(),
    )]
}

/// The grants of a canned ACL.
pub fn canned_acl(canned: &str) -> Vec<(StoredGrantee, String)> {
    let mut acl = private_acl();
    let group =
        |uri: &str, permission: &str| (StoredGrantee::Group(uri.to_owned()), permission.to_owned());
    match canned {
        "public-read" => acl.push(group(ALL_USERS, "READ")),
        "public-read-write" => {
            acl.push(group(ALL_USERS, "READ"));
            acl.push(group(ALL_USERS, "WRITE"));
        }
        "authenticated-read" => acl.push(group(AUTHENTICATED_USERS, "READ")),
        "bucket-owner-read" => acl.push((
            StoredGrantee::User(BUCKET_OWNER_ID.to_owned()),
            "READ".to_owned(),
        )),
        "bucket-owner-full-control" => acl.push((
            StoredGrantee::User(BUCKET_OWNER_ID.to_owned()),
            "FULL_CONTROL".to_owned(),
        )),
        _ => {}
    }
    acl
}

fn parse_acl_body(body: &str) -> Vec<(StoredGrantee, String)> {
    body.split("<Grant>")
        .skip(1)
        .filter_map(|grant| {
            let permission = between(grant, "<Permission>", "</Permission>")?;
            let grantee = if let Some(uri) = between(grant, "<URI>", "</URI>") {
                StoredGrantee::Group(unescape(uri))
            } else {
                StoredGrantee::User(unescape(between(grant, "<ID>", "</ID>")?))
            };
            Some((grantee, permission.to_owned()))
        })
        .collect()
}

fn parse_tag_body(body: &str) -> BTreeMap<String, String> {
    body.split("<Tag>")
        .skip(1)
        .filter_map(|tag| {
            let key = between(tag, "<Key>", "</Key>")?;
            let value = between(tag, "<Value>", "</Value>").unwrap_or_default();
            Some((unescape(key), unescape(value)))
        })
        .collect()
}

/// `a=b&c=d` as sent in `x-amz-tagging`.
fn parse_tagging_header(header: &str) -> BTreeMap<String, String> {
    header
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let rest = &text[from..];
    Some(&rest[..rest.find(end)?])
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
