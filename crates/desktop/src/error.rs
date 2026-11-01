//! The error every command rejects with, and the argument type for secrets.

use std::fmt;

use bucketbridge_core::{BbError, Error, ErrorKind};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// What a failed command rejects with: the `BbError` object of
/// docs/desktop-api.md, `{ "code": ..., "message": ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CommandError(BbError);

impl CommandError {
    /// An error with an explicit code.
    pub fn new(code: ErrorKind, message: impl Into<String>) -> Self {
        CommandError(BbError {
            code,
            message: message.into(),
        })
    }

    /// The error code.
    pub fn code(&self) -> ErrorKind {
        self.0.code
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.0.message
    }
}

impl From<Error> for CommandError {
    fn from(error: Error) -> Self {
        CommandError(error.to_payload())
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.0.code, self.0.message)
    }
}

impl std::error::Error for CommandError {}

/// A secret command argument (a password): zeroized when dropped and redacted
/// in `Debug`, so it never ends up in a log line.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// The secret value; keep the borrow short.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
