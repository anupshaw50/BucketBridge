//! Client-side encryption: the password of `--encrypt` / `--decrypt`, and
//! `bb encrypt` and `bb decrypt`, which rewrite objects in place.
//!
//! The password is never a command-line argument. It comes from
//! `--password-file` (first line), else `BB_CSE_PASSWORD`, else a prompt on the
//! terminal (asked twice when encrypting). The transfers themselves are in
//! `copy.rs`; the codec is `bucketbridge_core::cse`.

use std::path::Path;
use std::pin::pin;

use bucketbridge_core::cse::{self, Action, Compression, EncryptOptions, TransformRequest};
use bucketbridge_core::engine::{Hooks, TransferEngine, TransferOptions};
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::{BbError, Error, ErrorKind, S3Uri};
use futures::TryStreamExt;
use serde::Serialize;

use crate::cli::{Command, DecryptArgs, EncryptArgs};
use crate::context::{Context, Inputs};
use crate::i18n::{tr, trf};
use crate::output::{Batch, Failure, Output};

/// Environment variable holding the encryption password.
pub const PASSWORD_ENV: &str = "BB_CSE_PASSWORD";

/// The password a command needs, if it needs one: for `cp` and `mv` with
/// `--encrypt` or `--decrypt` (not with `--dry-run`, which transfers
/// nothing), and for `encrypt` and `decrypt`.
pub fn resolve(command: &Command, inputs: &Inputs) -> Result<Option<SecretString>, Error> {
    let (file, confirm) = match command {
        Command::Cp(args) | Command::Mv(args) => {
            let cse = &args.cse;
            if args.dry_run || !(cse.encrypt || cse.decrypt) {
                return Ok(None);
            }
            (cse.password_file.as_deref(), cse.encrypt)
        }
        Command::Encrypt(args) => (args.password_file.as_deref(), true),
        Command::Decrypt(args) => (args.password_file.as_deref(), false),
        _ => return Ok(None),
    };
    password(file, confirm, inputs).map(Some)
}

/// Reads the password: file, else environment, else the terminal (twice
/// when `confirm`).
pub fn password(
    file: Option<&Path>,
    confirm: bool,
    inputs: &Inputs,
) -> Result<SecretString, Error> {
    let text = if let Some(path) = file {
        let content = std::fs::read_to_string(path).map_err(|err| {
            Error::io(
                trf(
                    "reading the password file '{path}'",
                    &[("path", &path.display().to_string())],
                ),
                err,
            )
        })?;
        content.lines().next().unwrap_or_default().to_owned()
    } else if let Some(value) = inputs.env.get(PASSWORD_ENV)? {
        value
    } else {
        let asked = inputs.console.read_secret(&tr("Encryption password: "));
        let Ok(Some(first)) = asked else {
            return Err(Error::Usage(trf(
                "no encryption password: use --password-file, set {env}, or run on a terminal",
                &[("env", PASSWORD_ENV)],
            )));
        };
        if confirm {
            match inputs
                .console
                .read_secret(&tr("Confirm encryption password: "))
            {
                Ok(Some(again)) if again == first => {}
                _ => return Err(Error::Usage(tr("the two passwords differ"))),
            }
        }
        first
    };
    if text.is_empty() {
        return Err(Error::Usage(tr("the encryption password is empty")));
    }
    Ok(SecretString::new(text))
}

/// `bb encrypt` and `bb decrypt`: one object or a prefix.
#[derive(Debug, Serialize)]
struct CseRecord {
    bucket: String,
    key: String,
    action: &'static str,
    size_before: Option<u64>,
    size_after: Option<u64>,
    etag: Option<String>,
    skipped: bool,
    error: Option<BbError>,
}

/// Runs `bb encrypt`.
pub async fn encrypt(
    ctx: &Context,
    args: &EncryptArgs,
    password: Option<SecretString>,
    out: &mut Output,
) -> Result<(), Failure> {
    let options = EncryptOptions {
        compression: args.compress.unwrap_or(Compression::None),
        ..EncryptOptions::default()
    };
    transform(
        ctx,
        &args.uri,
        args.recursive,
        Action::Encrypt(options),
        password,
        out,
    )
    .await
}

/// Runs `bb decrypt`.
pub async fn decrypt(
    ctx: &Context,
    args: &DecryptArgs,
    password: Option<SecretString>,
    out: &mut Output,
) -> Result<(), Failure> {
    transform(
        ctx,
        &args.uri,
        args.recursive,
        Action::Decrypt,
        password,
        out,
    )
    .await
}

fn verb(action: Action) -> &'static str {
    match action {
        Action::Encrypt(_) => "encrypt",
        Action::Decrypt => "decrypt",
    }
}

async fn transform(
    ctx: &Context,
    uri: &S3Uri,
    recursive: bool,
    action: Action,
    password: Option<SecretString>,
    out: &mut Output,
) -> Result<(), Failure> {
    let Some(password) = password else {
        return Err(Error::Internal("the encryption password was not read".into()).into());
    };
    if recursive != uri.is_prefix() {
        let message = if recursive {
            trf(
                "--recursive needs a prefix ending in '/', for example {prefix}/",
                &[("prefix", &format!("{uri}"))],
            )
        } else {
            let template = match action {
                Action::Encrypt(_) => {
                    "{uri} is a prefix; add --recursive to encrypt everything under it"
                }
                Action::Decrypt => {
                    "{uri} is a prefix; add --recursive to decrypt everything under it"
                }
            };
            trf(template, &[("uri", &uri.to_string())])
        };
        return Err(Error::Usage(message).into());
    }
    let engine = TransferEngine::new(
        ctx.s3.clone(),
        TransferOptions::from_settings(&ctx.settings),
    )?
    .with_if_exists(bucketbridge_core::settings::IfExists::Overwrite);
    let hooks = Hooks::new().with_stop(ctx.stop.clone());
    let one = |key: String| {
        let request = TransformRequest {
            bucket: uri.bucket().to_owned(),
            key,
            action,
            password: password.clone(),
        };
        let (engine, hooks) = (&engine, &hooks);
        async move { cse::transform_object(engine, &request, hooks).await }
    };
    if !recursive {
        let outcome = one(uri.key().to_owned()).await?;
        let record = record(uri.bucket(), uri.key(), action, Ok(&outcome));
        return out.change(&record, || line(&record));
    }
    let mut batch = Batch::default();
    let mut entries = pin!(ctx.s3.list_all_stream(uri.bucket(), uri.key()));
    while let Some(entry) = entries.try_next().await? {
        if entry.key.ends_with('/') {
            continue;
        }
        if ctx.stop.is_stopped() {
            return Err(Error::Cancelled.into());
        }
        let key = entry.key;
        match one(key.clone()).await {
            Ok(outcome) => {
                batch.succeeded();
                let record = record(uri.bucket(), &key, action, Ok(&outcome));
                out.change(&record, || line(&record))?;
            }
            Err(err) if skippable(action, &err) => {
                let mut record = record(uri.bucket(), &key, action, Err(&err));
                record.skipped = true;
                record.error = None;
                out.change(&record, || line(&record))?;
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled.into()),
            Err(err) => {
                batch.failed(err.kind(), err.to_string());
                let record = record(uri.bucket(), &key, action, Err(&err));
                let message = trf(
                    "failed to {action} s3://{bucket}/{key}: {error}",
                    &[
                        ("action", verb(action)),
                        ("bucket", uri.bucket()),
                        ("key", &key),
                        ("error", &err.to_string()),
                    ],
                );
                out.item_failed(&record, || message)?;
            }
        }
    }
    let failure = match action {
        Action::Encrypt(_) => "could not be encrypted",
        Action::Decrypt => "could not be decrypted",
    };
    batch.finish("objects", failure)
}

/// Whether `err` only says the object is already in the wanted state.
fn skippable(action: Action, err: &Error) -> bool {
    match action {
        Action::Encrypt(_) => err.kind() == ErrorKind::Conflict,
        Action::Decrypt => err.kind() == ErrorKind::Usage,
    }
}

fn record(
    bucket: &str,
    key: &str,
    action: Action,
    result: Result<&cse::TransformOutcome, &Error>,
) -> CseRecord {
    let (size_before, size_after, etag, error) = match result {
        Ok(done) => (
            Some(done.size_before),
            Some(done.size_after),
            done.etag.clone(),
            None,
        ),
        Err(err) => (None, None, None, Some(err.to_payload())),
    };
    CseRecord {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
        action: verb(action),
        size_before,
        size_after,
        etag,
        skipped: false,
        error,
    }
}

fn line(record: &CseRecord) -> String {
    let (what, skipped) = if record.action == "encrypt" {
        (
            "encrypted s3://{bucket}/{key}",
            "skipped s3://{bucket}/{key} (already encrypted)",
        )
    } else {
        (
            "decrypted s3://{bucket}/{key}",
            "skipped s3://{bucket}/{key} (not encrypted)",
        )
    };
    let template = if record.skipped { skipped } else { what };
    trf(
        template,
        &[("bucket", &record.bucket), ("key", &record.key)],
    )
}
