//! `bb edit`

use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;

use bucketbridge_core::edit::{
    EditManager, EditOptions, EditResolution, EditSession, EditState, KEEP_LEFTOVER_COPIES,
    StartEdit,
};
use bucketbridge_core::engine::{S3Provider, StopSignal, TransferOptions};
use bucketbridge_core::{Error, ErrorKind};
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;

use crate::cli::EditArgs;
use crate::context::Context;
use crate::output::{Failure, Output};
use crate::transfer::throttle;

/// An event of an edit session, printed as a line (or with `--json` as a
/// record).
#[derive(Debug, Serialize)]
struct EditRecord<'a> {
    /// `started`, `uploaded`, `conflict`, `error`, `copy_saved`,
    /// `discarded` or `stopped`.
    event: &'static str,
    bucket: &'a str,
    key: &'a str,
    /// The local copy being edited.
    local_path: &'a str,
    /// ETag of the object after an upload, or of the changed object in a
    /// conflict.
    etag: Option<&'a str>,
    /// Key of the copy, for `copy_saved`.
    copy_key: Option<&'a str>,
    /// What went wrong, for `conflict` and `error`.
    message: Option<String>,
}

impl<'a> EditRecord<'a> {
    fn new(event: &'static str, session: &'a EditSession) -> Self {
        EditRecord {
            event,
            bucket: &session.bucket,
            key: &session.key,
            local_path: &session.local_path,
            etag: None,
            copy_key: None,
            message: None,
        }
    }
}

/// Opens the object's local copy and uploads every save until Ctrl-C.
pub async fn run(ctx: &Context, args: &EditArgs, out: &mut Output) -> Result<(), Failure> {
    let uri = &args.uri;
    if uri.key().is_empty() || uri.key().ends_with('/') {
        return Err(Error::Usage(format!(
            "{uri} is not an object; edit needs an object, for example s3://{}/key",
            uri.bucket()
        ))
        .into());
    }
    let s3 = ctx.s3.clone();
    let provider: S3Provider = Arc::new(move |_account: String| {
        let s3 = s3.clone();
        Box::pin(async move { Ok(s3) })
    });
    let root = match &ctx.dirs {
        Some(dirs) => dirs.data_dir.join("edit"),
        None => std::env::temp_dir().join("bucketbridge-edit"),
    };
    let manager = EditManager::new(root, provider, EditOptions::default())
        .with_throttle(throttle(&ctx.settings.transfer));
    manager.purge_stale(KEEP_LEFTOVER_COPIES);

    let mut events = manager.subscribe();
    let started = manager
        .start(StartEdit {
            account_id: ctx.account_id.clone(),
            bucket: uri.bucket().to_owned(),
            key: uri.key().to_owned(),
            transfer: TransferOptions::from_settings(&ctx.settings),
        })
        .await?;
    let session = started.session;
    if let Err(message) = open_file(Path::new(&session.local_path), args.app.as_deref()) {
        // Nothing was edited yet.
        let _ = manager.stop(&session.id, true).await;
        return Err(Error::io(
            format!("opening {}", session.local_path),
            io::Error::other(message),
        )
        .into());
    }
    out.change(&EditRecord::new("started", &session), || {
        format!("editing {uri}: {}", session.local_path)
    })?;
    out.notice("Save the file to upload it. Press Ctrl-C to stop editing.");

    let interactive = !out.json() && io::stdin().is_terminal() && io::stderr().is_terminal();
    let id = session.id.clone();
    loop {
        tokio::select! {
            biased;
            () = ctx.stop.stopped() => break,
            event = events.recv() => match event {
                Ok(change) if change.id == id => {
                    if report(out, &change)? && !settle(ctx, &manager, &change, interactive, out).await? {
                        return Ok(());
                    }
                    if change.state == EditState::Stopped {
                        return Ok(());
                    }
                }
                // Another session of this manager (none) or missed
                // changes: the next one carries the full state.
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
        }
    }
    finish(&manager, &id, interactive, out).await
}

/// Prints a session change. True when the user has to decide (a conflict).
fn report(out: &mut Output, session: &EditSession) -> Result<bool, Failure> {
    let uri = format!("s3://{}/{}", session.bucket, session.key);
    match session.state {
        EditState::Uploaded => {
            let mut record = EditRecord::new("uploaded", session);
            record.etag = session.etag.as_deref();
            out.change(&record, || format!("uploaded {uri}"))?;
            Ok(false)
        }
        EditState::Conflict => {
            let mut record = EditRecord::new("conflict", session);
            let deleted = session
                .conflict
                .as_ref()
                .is_some_and(|conflict| conflict.remote_deleted);
            record.etag = session
                .conflict
                .as_ref()
                .and_then(|conflict| conflict.remote_etag.as_deref());
            let what = if deleted {
                "was deleted"
            } else {
                "changed on the server"
            };
            let message = format!(
                "{uri} {what} since you opened it; your save was not uploaded (the copy stays at {})",
                session.local_path
            );
            record.message = Some(message.clone());
            out.item_failed(&record, || message.clone())?;
            Ok(true)
        }
        EditState::Error => {
            let message = session.error.clone().unwrap_or_default();
            let mut record = EditRecord::new("error", session);
            record.message = Some(message.clone());
            out.item_failed(&record, || {
                format!("{uri} was not uploaded: {message}; save the file again to retry")
            })?;
            Ok(false)
        }
        EditState::Watching | EditState::Uploading | EditState::Stopped => Ok(false),
    }
}

/// Lets the user settle a conflict on a terminal; anywhere else nothing is
/// ever overwritten and the session waits (the stop then reports it).
/// Returns false when the session ended.
async fn settle(
    ctx: &Context,
    manager: &EditManager,
    session: &EditSession,
    interactive: bool,
    out: &mut Output,
) -> Result<bool, Failure> {
    if !interactive {
        return Ok(true);
    }
    let Some(resolution) = ask(Some(&ctx.stop), session, out).await else {
        return Ok(true);
    };
    resolve(manager, session, resolution, out).await
}

async fn resolve(
    manager: &EditManager,
    session: &EditSession,
    resolution: EditResolution,
    out: &mut Output,
) -> Result<bool, Failure> {
    let resolved = manager.resolve(&session.id, resolution).await?;
    match resolution {
        EditResolution::SaveCopy => {
            let mut record = EditRecord::new("copy_saved", session);
            record.copy_key = resolved.copy_key.as_deref();
            out.change(&record, || {
                format!(
                    "saved your version as s3://{}/{}",
                    session.bucket,
                    resolved.copy_key.as_deref().unwrap_or_default()
                )
            })?;
        }
        EditResolution::Discard => {
            out.change(&EditRecord::new("discarded", session), || {
                format!(
                    "discarded your changes to s3://{}/{}",
                    session.bucket, session.key
                )
            })?;
        }
        EditResolution::Overwrite | EditResolution::Retry => {}
    }
    Ok(resolved.session.is_some())
}

/// Asks what to do about a conflict; `None` when there is no answer (end of
/// input, Ctrl-C, or "decide later").
async fn ask(
    stop: Option<&StopSignal>,
    session: &EditSession,
    out: &Output,
) -> Option<EditResolution> {
    out.stderr().raw(&format!(
        "s3://{}/{} changed on the server. Overwrite it with your version (o), save your version as a copy (c), discard your changes (d), or decide later (l)? ",
        session.bucket, session.key
    ));
    let (tx, rx) = oneshot::channel();
    // A plain thread: a read of standard input cannot be cancelled, and a
    // blocked runtime thread would keep the process from exiting.
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line);
        let _ = tx.send(read.ok().map(|_| line));
    });
    let line = match stop {
        Some(stop) => tokio::select! {
            line = rx => line.ok().flatten(),
            () = stop.stopped() => None,
        },
        None => rx.await.ok().flatten(),
    }?;
    match line.trim().to_ascii_lowercase().as_str() {
        "o" | "overwrite" => Some(EditResolution::Overwrite),
        "c" | "copy" => Some(EditResolution::SaveCopy),
        "d" | "discard" => Some(EditResolution::Discard),
        _ => None,
    }
}

/// Ends the session after Ctrl-C: the last save is uploaded first. A
/// conflict or a failed upload keeps the copy and fails the command; the
/// object is never overwritten without a choice.
async fn finish(
    manager: &EditManager,
    id: &str,
    interactive: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    loop {
        let session = manager.get(id);
        match manager.stop(id, false).await {
            Ok(()) => {
                if let Some(session) = &session {
                    out.change(&EditRecord::new("stopped", session), || {
                        format!("stopped editing s3://{}/{}", session.bucket, session.key)
                    })?;
                }
                return Ok(());
            }
            Err(error) if error.kind() == ErrorKind::Conflict && interactive => {
                let Some(session) = session else {
                    return Err(error.into());
                };
                match ask(None, &session, out).await {
                    Some(resolution) => {
                        if !resolve(manager, &session, resolution, out).await? {
                            return Ok(());
                        }
                    }
                    None => return Err(kept(error, &session).into()),
                }
            }
            Err(error) => {
                return Err(match session {
                    Some(session) => kept(error, &session),
                    None => error,
                }
                .into());
            }
        }
    }
}

/// The error that says where the unsaved work is.
fn kept(error: Error, session: &EditSession) -> Error {
    error.with_context(format!(
        "your edited copy is kept at {}",
        session.local_path
    ))
}

/// Opens `path` with the default application, or with `app` (an `.app`
/// bundle on macOS, a program elsewhere), without waiting for it.
fn open_file(path: &Path, app: Option<&str>) -> Result<(), String> {
    let mut command = match app {
        Some(app) if cfg!(target_os = "macos") && app.to_ascii_lowercase().ends_with(".app") => {
            let mut command = Process::new("open");
            command.arg("-a").arg(app).arg(path);
            command
        }
        Some(app) => {
            let mut command = Process::new(app);
            command.arg(path);
            command
        }
        None if cfg!(target_os = "macos") => {
            let mut command = Process::new("open");
            command.arg(path);
            command
        }
        None if cfg!(windows) => {
            let mut command = Process::new("rundll32");
            command.arg("url.dll,FileProtocolHandler").arg(path);
            command
        }
        None => {
            let mut command = Process::new("xdg-open");
            command.arg(path);
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|err| format!("{}: {err}", command.get_program().to_string_lossy()))
}
