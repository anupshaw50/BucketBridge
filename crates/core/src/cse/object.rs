//! Client-side encryption of S3 objects: probing, password checks, in-place
//! rewrites, and the encrypting upload and decrypting download the transfer
//! queue and the CLI use.
//!
//! Every function streams through temporary files (a temporary folder, removed
//! on every outcome): memory use is one chunk, disk use is one extra copy of
//! the object.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_sdk_s3::config::ResponseChecksumValidation;
use serde::Serialize;

use super::format::{self, Compression, EncryptOptions, HEADER_LEN, HeaderInfo};
use super::{META_CONTENT_ENCODING_KEY, META_KEY, is_marked, marker_metadata};
use crate::engine::{
    DownloadOutcome, Hooks, Outcome, TransferEngine, UploadHeaders, UploadOutcome, WriteCondition,
    guess_content_type,
};
use crate::error::{Error, IoContext, Result};
use crate::model::ObjectHead;
use crate::ops::S3;
use crate::s3uri::S3Uri;
use crate::secrets::SecretString;

/// What `probe_object` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObjectCse {
    /// The data starts with an envelope header.
    pub encrypted: bool,
    /// The object carries the `bb-cse` metadata marker.
    pub marked: bool,
    /// Format version of the envelope.
    pub version: Option<u8>,
    /// Compression inside the envelope.
    pub compression: Option<Compression>,
    /// Plaintext chunk size of the envelope.
    pub chunk_size: Option<u32>,
}

impl ObjectCse {
    fn of(head: &ObjectHead, header: Option<HeaderInfo>) -> Self {
        ObjectCse {
            encrypted: header.is_some(),
            marked: is_marked(head.meta.user_meta.iter()),
            version: header.map(|info| info.version),
            compression: header.map(|info| info.compression),
            chunk_size: header.map(|info| info.chunk_size),
        }
    }
}

/// The first bytes of an object (at most [`HEADER_LEN`]), or nothing when
/// the object is too short to hold an envelope.
async fn read_prefix(s3: &S3, bucket: &str, key: &str, size: u64) -> Result<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let last = size.min(HEADER_LEN as u64) - 1;
    let out = s3
        .client_for(bucket)
        .await?
        .get_object()
        .bucket(bucket)
        .key(key)
        .range(format!("bytes=0-{last}"))
        .set_request_payer(s3.request_payer())
        .customize()
        .config_override(
            s3.metadata_timeout()
                .response_checksum_validation(ResponseChecksumValidation::WhenRequired),
        )
        .send()
        .await
        .map_err(|err| Error::from(err).with_context(format!("s3://{bucket}/{key}")))?;
    let mut body = out.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body
        .try_next()
        .await
        .map_err(|err| Error::Remote(format!("reading the start of s3://{bucket}/{key}: {err}")))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() >= HEADER_LEN {
            break;
        }
    }
    bytes.truncate(HEADER_LEN);
    Ok(bytes)
}

async fn head_and_prefix(s3: &S3, bucket: &str, key: &str) -> Result<(ObjectHead, Vec<u8>)> {
    if key.ends_with('/') {
        return Err(Error::Usage(format!(
            "s3://{bucket}/{key} is a folder; client-side encryption works on objects"
        )));
    }
    let head = s3.head_object(bucket, key, None).await?;
    let prefix = read_prefix(s3, bucket, key, head.size).await?;
    Ok((head, prefix))
}

/// Reads the start of an object and its metadata and reports whether it is
/// client-side encrypted. No password is needed.
pub async fn probe_object(s3: &S3, bucket: &str, key: &str) -> Result<ObjectCse> {
    let (head, prefix) = head_and_prefix(s3, bucket, key).await?;
    let header =
        format::inspect(&prefix).map_err(|err| err.with_context(format!("s3://{bucket}/{key}")))?;
    Ok(ObjectCse::of(&head, header))
}

/// Checks `password` against the header of an encrypted object (one key
/// derivation, no download of the body).
///
/// Fails with *password not found* for `None` or an empty password and
/// *password not valid* for a wrong one; with [`Error::Usage`] when the
/// object is not encrypted.
pub async fn check_object_password(
    s3: &S3,
    bucket: &str,
    key: &str,
    password: Option<&SecretString>,
) -> Result<HeaderInfo> {
    let (_, prefix) = head_and_prefix(s3, bucket, key).await?;
    let password = password.cloned();
    blocking(move || format::check_password(&prefix, password.as_ref().map(SecretString::expose)))
        .await?
        .map_err(|err| err.with_context(format!("s3://{bucket}/{key}")))
}

async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))
}

/// What to do to an object in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Compress (as the options say) and encrypt.
    Encrypt(EncryptOptions),
    /// Decrypt and decompress.
    Decrypt,
}

/// An in-place rewrite of one object.
#[derive(Debug, Clone)]
pub struct TransformRequest {
    /// Bucket.
    pub bucket: String,
    /// Object key (not a folder).
    pub key: String,
    /// Encrypt or decrypt.
    pub action: Action,
    /// The password.
    pub password: SecretString,
}

/// Result of [`transform_object`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransformOutcome {
    /// Size of the object before.
    pub size_before: u64,
    /// Size of the object now.
    pub size_after: u64,
    /// ETag of the new object, without quotes.
    pub etag: Option<String>,
}

/// Encrypts or decrypts an object in place: downloads it, transforms the file,
/// and uploads it under the same key with its content type, `Cache-Control`,
/// `Content-Disposition`, `Content-Encoding`, user metadata and storage class
/// (tags, ACL, `Content-Language` and `Expires` are not kept; on a versioned
/// bucket the old version stays as the previous version).
///
/// The upload is conditional on the ETag of the downloaded object
/// (`If-Match`), so an object that changed in the meantime is never
/// overwritten. A password problem is reported before the object is
/// downloaded. Stopping through `hooks` fails with [`Error::Cancelled`].
pub async fn transform_object(
    engine: &TransferEngine,
    request: &TransformRequest,
    hooks: &Hooks,
) -> Result<TransformOutcome> {
    let (bucket, key) = (request.bucket.as_str(), request.key.as_str());
    let uri = S3Uri::new(bucket, key)?;
    let (head, prefix) = head_and_prefix(engine.s3(), bucket, key).await?;
    let header = format::inspect(&prefix).map_err(|err| err.with_context(&uri))?;
    match request.action {
        Action::Encrypt(_) => {
            if header.is_some() {
                return Err(Error::Conflict(format!(
                    "{uri} is already encrypted; decrypt it first"
                )));
            }
        }
        Action::Decrypt => {
            if header.is_none() {
                return Err(Error::Usage(format!("{uri} is not client-side encrypted")));
            }
            let password = request.password.clone();
            blocking(move || format::check_password(&prefix, Some(password.expose())))
                .await?
                .map_err(|err| err.with_context(&uri))?;
        }
    }

    let work = tempfile::Builder::new()
        .prefix("bb-cse-")
        .tempdir()
        .io_context(|| "creating a temporary folder".to_owned())?;
    let input = work.path().join("in");
    let output = work.path().join("out");
    let downloaded = engine
        .download_file(&uri, &input, hooks, None, &|_| {})
        .await?;
    let downloaded = completed(downloaded, hooks)?;
    let transform = {
        let (input, output, password, stop) = (
            input.clone(),
            output.clone(),
            request.password.clone(),
            hooks.stop.clone(),
        );
        let action = request.action;
        move || -> Result<()> {
            let reader = BufReader::new(
                File::open(&input).io_context(|| "reading the downloaded object".to_owned())?,
            );
            let writer = BufWriter::new(
                File::create(&output).io_context(|| "creating the temporary file".to_owned())?,
            );
            let mut go = |_: u64| !stop.is_stopped();
            match action {
                Action::Encrypt(options) => {
                    format::encrypt(reader, writer, password.expose(), &options, &mut go)?;
                }
                Action::Decrypt => {
                    format::decrypt(reader, writer, Some(password.expose()), &mut go)?;
                }
            }
            Ok(())
        }
    };
    blocking(transform).await??;
    // The input is no longer needed; free the disk before uploading.
    let _ = std::fs::remove_file(&input);

    let headers = rewrite_headers(&head, request.action);
    let content_type = head
        .meta
        .content_type
        .clone()
        .unwrap_or_else(|| guess_content_type(Path::new(key)).to_owned());
    let mut upload_engine = engine.with_upload_headers(headers);
    if let Some(etag) = downloaded.etag.clone().or_else(|| head.etag.clone()) {
        upload_engine = upload_engine.with_write_condition(WriteCondition::ETag(etag));
    }
    let source = {
        let output = output.clone();
        blocking(move || crate::engine::FileSource::open(&output)).await??
    };
    let uploaded = upload_engine
        .upload(
            Arc::new(source),
            &uri,
            Some(&content_type),
            hooks,
            None,
            &|_| {},
        )
        .await?;
    let uploaded = completed(uploaded, hooks)?;
    Ok(TransformOutcome {
        size_before: head.size,
        size_after: uploaded.size,
        etag: uploaded.etag,
    })
}

/// The headers an object keeps through a rewrite, with the marker added or
/// removed.
fn rewrite_headers(head: &ObjectHead, action: Action) -> UploadHeaders {
    let meta = &head.meta;
    let mut metadata: HashMap<String, String> = meta
        .user_meta
        .iter()
        .filter(|(key, _)| {
            !key.eq_ignore_ascii_case(META_KEY)
                && !key.eq_ignore_ascii_case(META_CONTENT_ENCODING_KEY)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let content_encoding = match action {
        Action::Encrypt(options) => {
            metadata.extend(marker_metadata(options.compression));
            if let Some(encoding) = &meta.content_encoding {
                // The ciphertext is not in that encoding; remember it.
                metadata.insert(META_CONTENT_ENCODING_KEY.to_owned(), encoding.clone());
            }
            None
        }
        Action::Decrypt => meta
            .user_meta
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(META_CONTENT_ENCODING_KEY))
            .map(|(_, value)| value.clone()),
    };
    UploadHeaders {
        metadata,
        cache_control: meta.cache_control.clone(),
        content_disposition: meta.content_disposition.clone(),
        content_encoding,
        content_language: None,
        expires: None,
        storage_class: head
            .storage_class
            .clone()
            .filter(|class| class != "STANDARD"),
    }
}

/// The value of a finished engine run; a stop becomes [`Error::Cancelled`].
fn completed<T>(outcome: Outcome<T>, hooks: &Hooks) -> Result<T> {
    match outcome {
        Outcome::Completed(value) => Ok(value),
        Outcome::Paused | Outcome::Cancelled => {
            let _ = hooks;
            Err(Error::Cancelled)
        }
    }
}

/// Encrypts the local file `path` into a temporary file and uploads that to
/// `dst`, marking the object with the `bb-cse` metadata.
///
/// The resume state of an earlier run is never used: the ciphertext of a
/// second run differs (fresh salt and nonces), so its parts could not be
/// combined with the first run's. A multipart upload that state names is
/// aborted. `checkpoint` receives the state of the new upload, so a pause
/// never leaves an upload that nothing names.
#[allow(clippy::too_many_arguments)]
pub async fn upload_encrypted(
    engine: &TransferEngine,
    path: &Path,
    dst: &S3Uri,
    hooks: &Hooks,
    password: &SecretString,
    compression: Compression,
    resume: Option<crate::engine::UploadState>,
    checkpoint: &(dyn Fn(&crate::engine::UploadState) + Send + Sync),
) -> Result<Outcome<UploadOutcome>> {
    if let Some(saved) = resume {
        let _ = engine.discard_upload(&saved).await;
    }
    if dst.key().ends_with('/') {
        return engine.upload_file(path, dst, hooks, None, checkpoint).await;
    }
    let work = tempfile::Builder::new()
        .prefix("bb-cse-")
        .tempdir()
        .io_context(|| "creating a temporary folder".to_owned())?;
    let encrypted = work.path().join("out");
    let encrypt = {
        let (path, encrypted, password, stop) = (
            path.to_path_buf(),
            encrypted.clone(),
            password.clone(),
            hooks.stop.clone(),
        );
        move || -> Result<()> {
            let reader = BufReader::new(
                File::open(&path)
                    .io_context(|| format!("opening '{}' for encryption", path.display()))?,
            );
            let writer = BufWriter::new(
                File::create(&encrypted).io_context(|| "creating the temporary file".to_owned())?,
            );
            let options = EncryptOptions {
                compression,
                ..EncryptOptions::default()
            };
            format::encrypt(reader, writer, password.expose(), &options, &mut |_| {
                !stop.is_stopped()
            })?;
            Ok(())
        }
    };
    match blocking(encrypt).await? {
        Ok(()) => {}
        Err(Error::Cancelled) if hooks.stop.is_stopped() => {
            return Ok(Outcome::stopped(hooks.stop.reason()));
        }
        Err(err) => return Err(err),
    }
    let source = {
        let encrypted = encrypted.clone();
        blocking(move || crate::engine::FileSource::open(&encrypted)).await??
    };
    let headers = UploadHeaders {
        metadata: marker_metadata(compression),
        ..UploadHeaders::default()
    };
    engine
        .with_upload_headers(headers)
        .upload(
            Arc::new(source),
            dst,
            Some(guess_content_type(path)),
            hooks,
            None,
            checkpoint,
        )
        .await
}

/// Downloads `src` to `dest`; when the object is encrypted, decrypts the
/// file (replacing `dest`, with its modification time kept). Objects that are
/// not encrypted are saved as they are.
///
/// A password problem is reported before the download. If decryption fails
/// after it (modified data), `dest` is removed rather than left holding
/// ciphertext under the plain name.
pub async fn download_decrypted(
    engine: &TransferEngine,
    src: &S3Uri,
    dest: &Path,
    hooks: &Hooks,
    password: Option<&SecretString>,
    resume: Option<crate::engine::DownloadState>,
    checkpoint: &(dyn Fn(&crate::engine::DownloadState) + Send + Sync),
) -> Result<Outcome<DownloadOutcome>> {
    if src.key().ends_with('/') {
        return engine
            .download_file(src, dest, hooks, resume, checkpoint)
            .await;
    }
    let (_, prefix) = head_and_prefix(engine.s3(), src.bucket(), src.key()).await?;
    if format::inspect(&prefix)
        .map_err(|err| err.with_context(src))?
        .is_some()
    {
        let owned = password.cloned();
        blocking(move || format::check_password(&prefix, owned.as_ref().map(SecretString::expose)))
            .await?
            .map_err(|err| err.with_context(src))?;
    }
    let outcome = engine
        .download_file(src, dest, hooks, resume, checkpoint)
        .await?;
    if !outcome.is_completed() {
        return Ok(outcome);
    }
    let (path, password, stop) = (dest.to_path_buf(), password.cloned(), hooks.stop.clone());
    match blocking(move || decrypt_file_in_place(&path, password.as_ref(), &stop)).await? {
        Ok(_) => Ok(outcome),
        Err(Error::Cancelled) if hooks.stop.is_stopped() => {
            Ok(Outcome::stopped(hooks.stop.reason()))
        }
        Err(err) => Err(err.with_context(src)),
    }
}

/// Decrypts the file at `path` if it holds an envelope; `Ok(false)` when it
/// does not. Blocking.
fn decrypt_file_in_place(
    path: &Path,
    password: Option<&SecretString>,
    stop: &crate::engine::StopSignal,
) -> Result<bool> {
    let mut file =
        File::open(path).io_context(|| format!("opening '{}' for decryption", path.display()))?;
    let mut head = Vec::with_capacity(HEADER_LEN);
    (&mut file)
        .take(HEADER_LEN as u64)
        .read_to_end(&mut head)
        .io_context(|| format!("reading '{}'", path.display()))?;
    if format::inspect(&head)?.is_none() {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(0))
        .io_context(|| format!("reading '{}'", path.display()))?;
    let modified = file.metadata().and_then(|meta| meta.modified()).ok();
    let temp = decrypting_path(path);
    let result = (|| -> Result<()> {
        let out = File::create(&temp).io_context(|| format!("creating '{}'", temp.display()))?;
        let mut writer = BufWriter::new(out);
        format::decrypt(
            BufReader::new(&file),
            &mut writer,
            password.map(SecretString::expose),
            &mut |_| !stop.is_stopped(),
        )?;
        let out = writer
            .into_inner()
            .map_err(|err| Error::io(format!("writing '{}'", temp.display()), err.into_error()))?;
        if let Some(modified) = modified {
            let _ = out.set_modified(modified);
        }
        out.sync_all()
            .io_context(|| format!("writing '{}'", temp.display()))?;
        Ok(())
    })();
    drop(file);
    match result {
        Ok(()) => {
            std::fs::rename(&temp, path)
                .io_context(|| format!("replacing '{}'", path.display()))?;
            Ok(true)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            if !matches!(err, Error::Cancelled) {
                let _ = std::fs::remove_file(path);
            }
            Err(err)
        }
    }
}

/// Decrypts the file `input` into `output` (blocking); the file-to-file form
/// of [`format::decrypt`] with the same password states.
pub fn decrypt_file(
    input: &Path,
    output: &Path,
    password: Option<&SecretString>,
) -> Result<format::Summary> {
    let reader =
        BufReader::new(File::open(input).io_context(|| format!("opening '{}'", input.display()))?);
    let writer = BufWriter::new(
        File::create(output).io_context(|| format!("creating '{}'", output.display()))?,
    );
    format::decrypt(
        reader,
        writer,
        password.map(SecretString::expose),
        &mut |_| true,
    )
}

/// Encrypts the file `input` into `output` (blocking).
pub fn encrypt_file(
    input: &Path,
    output: &Path,
    password: &SecretString,
    options: &EncryptOptions,
) -> Result<format::Summary> {
    let reader =
        BufReader::new(File::open(input).io_context(|| format!("opening '{}'", input.display()))?);
    let writer = BufWriter::new(
        File::create(output).io_context(|| format!("creating '{}'", output.display()))?,
    );
    format::encrypt(reader, writer, password.expose(), options, &mut |_| true)
}

/// The temporary name a decryption writes to next to its destination.
fn decrypting_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}.bb-decrypting"))
}
