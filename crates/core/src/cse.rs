//! Client-side compression and encryption.
//!
//! Objects are encrypted on this machine before they are uploaded, and
//! decrypted after they are downloaded; the server only ever sees ciphertext.
//!
//! | Item | What it is |
//! |---|---|
//! | [`format`] | The streaming envelope: Argon2id key, chunked AES-256-GCM with the chunk index and a final flag bound as associated data, optional gzip or zstd before encryption |
//! | [`encrypt`], [`decrypt`] | Blocking stream codecs over [`std::io::Read`] and [`std::io::Write`] |
//! | [`object`] | Probing objects, checking a password against the first bytes of an object, and rewriting an object in place ([`object::transform_object`]) |
//! | [`jobs`] | Encrypt and decrypt options of transfer tasks, carried by the task's group ID, and the in-memory [`CsePasswords`] |
//!
//! Objects written by BucketBridge carry the user metadata `x-amz-meta-bb-cse`
//! ([`META_KEY`]) with the format name and version, the cipher and the
//! compression, for example `bbcse1;aes-256-gcm;zstd`. It holds no secret.
//! The authoritative test is the envelope header at the start of the data
//! ([`format::inspect`]); the marker lets a listing show the state without a
//! download.
//!
//! Two failures are kept apart, in the error message and as [`Problem`]:
//! *password not found* (the data is encrypted and no password was given) and
//! *password not valid* (the password does not open it). Both are
//! [`crate::ErrorKind::Auth`]. Damaged, truncated or reordered data is
//! [`Problem::Tampered`].
//!
//! Passwords are never logged, never written to the transfer queue and never
//! stored; the desktop host keeps them in memory for the tasks of one
//! enqueue call only ([`CsePasswords`]).

pub mod format;
pub mod jobs;
pub mod object;

use std::collections::HashMap;

use crate::error::Error;

pub use format::{
    Compression, DEFAULT_KDF_COST, EncryptOptions, HEADER_LEN, HeaderInfo, Summary, check_password,
    decrypt, encrypt, inspect, read_header,
};
pub use jobs::{CseMode, CsePasswords};
pub use object::{
    Action, ObjectCse, TransformOutcome, TransformRequest, check_object_password, decrypt_file,
    download_decrypted, encrypt_file, probe_object, transform_object, upload_encrypted,
};

/// The user metadata key (without `x-amz-meta-`) that marks encrypted
/// objects.
pub const META_KEY: &str = "bb-cse";

/// The user metadata key that keeps the `Content-Encoding` an object had
/// before it was encrypted (restored on decrypt).
pub const META_CONTENT_ENCODING_KEY: &str = "bb-cse-content-encoding";

/// The value of [`META_KEY`] for an envelope of `compression`.
pub fn marker_value(compression: Compression) -> String {
    format!("bbcse{};aes-256-gcm;{compression}", format::FORMAT_VERSION)
}

/// Whether `metadata` (user metadata, keys without the prefix) carries the
/// marker of an encrypted object. Keys compare case-insensitively.
pub fn is_marked<'a>(mut metadata: impl Iterator<Item = (&'a String, &'a String)>) -> bool {
    metadata.any(|(key, value)| key.eq_ignore_ascii_case(META_KEY) && value.starts_with("bbcse"))
}

/// The user metadata to set on an encrypted object.
pub fn marker_metadata(compression: Compression) -> HashMap<String, String> {
    HashMap::from([(META_KEY.to_owned(), marker_value(compression))])
}

/// The error of an encrypting task whose password is gone (after a restart).
pub(crate) fn format_password_not_found(target: &crate::s3uri::S3Uri) -> Error {
    Error::Auth(
        "password not found: the password of this encrypted transfer is no longer known (it is kept in memory only); add the files again"
            .to_owned(),
    )
    .with_context(target)
}

/// The states of client-side encryption a caller tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The data is encrypted and no password was given.
    PasswordNotFound,
    /// The password does not open the data.
    PasswordNotValid,
    /// The encrypted body failed authentication: truncated, reordered or
    /// modified.
    Tampered,
}

/// Which [`Problem`] `error` is, if any.
pub fn problem(error: &Error) -> Option<Problem> {
    if format::is_tampered(error) {
        return Some(Problem::Tampered);
    }
    match error {
        Error::Auth(message) if message.contains("password not found") => {
            Some(Problem::PasswordNotFound)
        }
        Error::Auth(message) if message.contains("password not valid") => {
            Some(Problem::PasswordNotValid)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_names_format_cipher_and_compression() {
        assert_eq!(marker_value(Compression::None), "bbcse1;aes-256-gcm;none");
        assert_eq!(marker_value(Compression::Zstd), "bbcse1;aes-256-gcm;zstd");
        let meta = marker_metadata(Compression::Gzip);
        assert_eq!(meta.len(), 1);
        assert!(is_marked(meta.iter()));
        assert!(!is_marked(HashMap::new().iter()));
        let other = HashMap::from([(META_KEY.to_owned(), "something".to_owned())]);
        assert!(!is_marked(other.iter()));
        let upper = HashMap::from([("BB-CSE".to_owned(), marker_value(Compression::None))]);
        assert!(is_marked(upper.iter()));
    }

    #[test]
    fn marker_holds_no_secret_material() {
        for compression in [Compression::None, Compression::Gzip, Compression::Zstd] {
            assert!(marker_value(compression).len() < 40);
        }
    }

    #[test]
    fn problems_are_classified() {
        let missing = Error::Auth(format::PASSWORD_NOT_FOUND.to_owned());
        let invalid = Error::Auth(format::PASSWORD_NOT_VALID.to_owned());
        assert_eq!(problem(&missing), Some(Problem::PasswordNotFound));
        assert_eq!(problem(&invalid), Some(Problem::PasswordNotValid));
        assert_eq!(
            problem(&missing.with_context("s3://b/k")),
            Some(Problem::PasswordNotFound)
        );
        assert_eq!(problem(&Error::Auth("denied".to_owned())), None);
        assert_eq!(problem(&Error::Cancelled), None);
    }
}
