//! Printing results: human tables by default, one JSON record per item with
//! `--json`; `--quiet` hides the success output of commands that change
//! things. Data goes to stdout; failures go to stderr (or, with `--json`,
//! become item records on stdout plus a JSON error record on stderr).

use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use bucketbridge_core::error::classify;
use bucketbridge_core::model::DeleteFailure;
use bucketbridge_core::{BbError, Error, ErrorKind};
use serde::Serialize;

use crate::i18n::{tr, trf};

/// Why a command stopped early.
pub enum Failure {
    /// An error to report; its kind decides the exit code.
    Error(Error),
    /// The reader of standard output went away (for example `bb ls | head`);
    /// not an error.
    BrokenPipe,
}

impl From<Error> for Failure {
    fn from(err: Error) -> Self {
        Failure::Error(err)
    }
}

/// Standard error, shared by the output, the progress line and the final
/// error report.
#[derive(Clone)]
pub struct Stderr(Arc<Mutex<Box<dyn Write + Send>>>);

impl Stderr {
    /// Wraps a writer.
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        Stderr(Arc::new(Mutex::new(writer)))
    }

    /// Writes one line; a failing standard error is ignored (there is
    /// nowhere left to report it).
    pub fn line(&self, text: &str) {
        let mut writer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(writer, "{text}");
        let _ = writer.flush();
    }

    /// Writes text as is (progress lines redraw with `\r`).
    pub fn raw(&self, text: &str) {
        let mut writer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = write!(writer, "{text}");
        let _ = writer.flush();
    }
}

/// Where results go.
pub struct Output {
    json: bool,
    quiet: bool,
    stdout: Box<dyn Write + Send>,
    stderr: Stderr,
}

impl Output {
    /// Output to the given streams.
    pub fn new(json: bool, quiet: bool, stdout: Box<dyn Write + Send>, stderr: Stderr) -> Self {
        Output {
            json,
            quiet,
            stdout,
            stderr,
        }
    }

    /// Whether records are printed as JSON (`--json`).
    pub fn json(&self) -> bool {
        self.json
    }

    /// Standard error.
    pub fn stderr(&self) -> &Stderr {
        &self.stderr
    }

    /// A row of a listing or query (`ls`, `presign`, `mpu list`, `account
    /// list`): always printed.
    pub fn row<T: Serialize>(
        &mut self,
        record: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), Failure> {
        if self.json {
            let line = json_line(record)?;
            self.write_line(&line)
        } else {
            self.write_line(&human())
        }
    }

    /// The outcome of a change; hidden by `--quiet`.
    pub fn change<T: Serialize>(
        &mut self,
        record: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), Failure> {
        if self.quiet {
            return Ok(());
        }
        self.row(record, human)
    }

    /// An item of a batch that failed: a record on stdout with `--json`,
    /// a `bb: ...` line on stderr otherwise. Never hidden by `--quiet`.
    pub fn item_failed<T: Serialize>(
        &mut self,
        record: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), Failure> {
        if self.json {
            let line = json_line(record)?;
            self.write_line(&line)
        } else {
            self.stderr.line(&format!("bb: {}", human()));
            Ok(())
        }
    }

    /// An item that was skipped on purpose (an existing destination kept by
    /// the overwrite policy): a record on stdout with `--json`, a `bb: ...`
    /// line on stderr otherwise. Never hidden by `--quiet`: nothing is
    /// skipped silently.
    pub fn skipped_item<T: Serialize>(
        &mut self,
        record: &T,
        human: impl FnOnce() -> String,
    ) -> Result<(), Failure> {
        self.item_failed(record, human)
    }

    /// A note for the person at the terminal, on standard error (for example
    /// how to resume an interrupted command). Never hidden by `--quiet`;
    /// left out with `--json`, where standard error carries only the final
    /// error record and the records on standard output carry the facts.
    pub fn notice(&self, text: &str) {
        if !self.json {
            self.stderr.line(text);
        }
    }

    /// Flushes standard output.
    pub fn flush(&mut self) -> Result<(), Failure> {
        self.stdout.flush().map_err(write_failure)
    }

    fn write_line(&mut self, line: &str) -> Result<(), Failure> {
        writeln!(self.stdout, "{line}").map_err(write_failure)
    }
}

/// Serializes one record.
pub fn json_line<T: Serialize>(record: &T) -> Result<String, Error> {
    serde_json::to_string(record)
        .map_err(|err| Error::Internal(format!("encoding JSON output: {err}")))
}

/// Maps a write error: a closed pipe ends the command quietly.
pub fn write_failure(err: io::Error) -> Failure {
    if err.kind() == io::ErrorKind::BrokenPipe {
        Failure::BrokenPipe
    } else {
        Failure::Error(Error::io("writing to standard output", err))
    }
}

/// Counts the items of a batch command and turns failures into the final
/// error: the exit code is that of the first failed item.
#[derive(Debug, Default)]
pub struct Batch {
    total: usize,
    failed: usize,
    /// Kind and message of the first failed item.
    first_failure: Option<(ErrorKind, String)>,
}

impl Batch {
    /// An item succeeded.
    pub fn succeeded(&mut self) {
        self.total += 1;
    }

    /// An item failed with an error of `kind`.
    pub fn failed(&mut self, kind: ErrorKind, message: impl Into<String>) {
        self.total += 1;
        self.failed += 1;
        if self.first_failure.is_none() {
            self.first_failure = Some((kind, message.into()));
        }
    }

    /// `Ok` when nothing failed; otherwise an error such as "2 of 10 objects
    /// could not be deleted". A local I/O failure also names its cause (that
    /// of the first failed item).
    pub fn finish(self, noun: &str, failure: &str) -> Result<(), Failure> {
        let Some((kind, first)) = self.first_failure else {
            return Ok(());
        };
        let summary = trf(
            "{failed} of {total} {noun} {failure}",
            &[
                ("failed", self.failed.to_string().as_str()),
                ("total", self.total.to_string().as_str()),
                ("noun", tr(noun).as_str()),
                ("failure", tr(failure).as_str()),
            ],
        );
        let err = match kind {
            ErrorKind::Io => Error::io(summary, io::Error::other(first)),
            kind => error_of_kind(kind, summary),
        };
        Err(Failure::Error(err))
    }
}

/// An error with the given kind and message.
pub fn error_of_kind(kind: ErrorKind, message: String) -> Error {
    match kind {
        ErrorKind::Usage => Error::Usage(message),
        ErrorKind::Auth => Error::Auth(message),
        ErrorKind::Locked => Error::Locked,
        ErrorKind::MfaRequired => Error::MfaRequired(message),
        ErrorKind::NotFound => Error::NotFound(message),
        ErrorKind::Conflict => Error::Conflict(message),
        ErrorKind::Io => Error::io(message, io::Error::other("one or more items failed")),
        ErrorKind::Cancelled => Error::Cancelled,
        ErrorKind::Internal => Error::Internal(message),
        ErrorKind::Remote => Error::Remote(message),
    }
}

/// The error of a key that a batch delete could not remove.
pub fn delete_failure_error(failure: &DeleteFailure) -> BbError {
    let message = match &failure.code {
        Some(code) => format!("{} ({code})", failure.message),
        None => failure.message.clone(),
    };
    BbError {
        code: classify(failure.code.as_deref(), None),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{tr, trf, with_language};

    #[test]
    fn batch_reports_the_first_failure_kind() {
        with_language(false, || {
            assert!(Batch::default().finish("objects", "failed").is_ok());
            let mut batch = Batch::default();
            batch.succeeded();
            batch.failed(ErrorKind::Auth, "denied");
            batch.failed(ErrorKind::NotFound, "gone");
            match batch.finish("objects", "could not be deleted") {
                Err(Failure::Error(err)) => {
                    assert_eq!(err.kind(), ErrorKind::Auth);
                    assert_eq!(
                        err.to_string(),
                        trf(
                            "{failed} of {total} {noun} {failure}",
                            &[
                                ("failed", "2"),
                                ("total", "3"),
                                ("noun", tr("objects").as_str()),
                                ("failure", tr("could not be deleted").as_str()),
                            ],
                        )
                    );
                }
                _ => panic!("expected an error"),
            }
        });
    }

    #[test]
    fn io_failures_name_the_first_cause() {
        with_language(false, || {
            let mut batch = Batch::default();
            batch.failed(ErrorKind::Io, "reading 'a': denied");
            batch.failed(ErrorKind::Remote, "slow down");
            batch.succeeded();
            match batch.finish("files", "could not be uploaded") {
                Err(Failure::Error(err)) => {
                    assert_eq!(err.kind(), ErrorKind::Io);
                    assert_eq!(
                        err.to_string(),
                        format!(
                            "{}: {}",
                            trf(
                                "{failed} of {total} {noun} {failure}",
                                &[
                                    ("failed", "2"),
                                    ("total", "3"),
                                    ("noun", tr("files").as_str()),
                                    ("failure", tr("could not be uploaded").as_str()),
                                ],
                            ),
                            "reading 'a': denied"
                        )
                    );
                }
                _ => panic!("expected an error"),
            }
        });
    }

    #[test]
    fn every_kind_round_trips() {
        for kind in ErrorKind::ALL {
            assert_eq!(error_of_kind(kind, "m".into()).kind(), kind);
        }
    }

    #[test]
    fn delete_failures_are_classified() {
        let failure = DeleteFailure {
            key: "k".into(),
            version_id: None,
            code: Some("AccessDenied".into()),
            message: "Access Denied".into(),
        };
        let error = delete_failure_error(&failure);
        assert_eq!(error.code, ErrorKind::Auth);
        assert_eq!(error.message, "Access Denied (AccessDenied)");
    }

    #[test]
    fn broken_pipe_is_not_an_error() {
        assert!(matches!(
            write_failure(io::Error::from(io::ErrorKind::BrokenPipe)),
            Failure::BrokenPipe
        ));
        match write_failure(io::Error::other("disk full")) {
            Failure::Error(err) => assert_eq!(err.kind(), ErrorKind::Io),
            Failure::BrokenPipe => panic!("not a broken pipe"),
        }
    }

    #[test]
    fn quiet_hides_changes_but_not_failures() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let errors = Arc::new(Mutex::new(Vec::new()));
        let mut out = Output::new(
            false,
            true,
            Box::new(Shared(buffer.clone())),
            Stderr::new(Box::new(Shared(errors.clone()))),
        );
        out.change(&1, || "changed".into()).ok();
        out.row(&2, || "row".into()).ok();
        out.item_failed(&3, || "failed".into()).ok();
        assert_eq!(
            String::from_utf8(buffer.lock().unwrap().clone()).unwrap(),
            "row\n"
        );
        assert_eq!(
            String::from_utf8(errors.lock().unwrap().clone()).unwrap(),
            "bb: failed\n"
        );
    }
}
