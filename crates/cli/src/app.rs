//! One invocation: parse the arguments, run the command, report how it
//! ended. The `bb` binary and the in-process tests both come through
//! [`run`].

use std::ffi::OsString;
use std::io::Write;
use std::pin::pin;
use std::sync::Mutex;
use std::time::Duration;

use bucketbridge_core::filter::IncludeExclude;
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::{Error, ErrorKind};
use clap::{CommandFactory, FromArgMatches};

use crate::cli::{
    self, AclCommand, Cli, Command, LockCommand, MpuCommand, SyncCommand, TagCommand,
};
use crate::commands;
use crate::commands::copy::Mode;
use crate::context::{Context, Inputs, Secrets};
use crate::io::Io;
use crate::output::{Failure, Output, Stderr, json_line};

/// After Ctrl-C, how long a transfer may take to clean up (abort its
/// multipart uploads, remove its partial downloads) before `bb` stops
/// waiting for it. A second Ctrl-C stops waiting at once.
const CLEANUP_GRACE: Duration = Duration::from_secs(10);

/// How an invocation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The exit status: 0 success, 1 usage error, 2 remote or local
    /// failure, 3 authentication failure or locked secrets, 130
    /// interrupted.
    pub code: u8,
    /// A transfer was cancelled and background clean-up it started (such
    /// as aborting a multipart copy) may still be running on the runtime;
    /// the binary gives it a moment before exiting.
    pub cleanup_pending: bool,
}

impl Exit {
    fn code(code: u8) -> Self {
        Exit {
            code,
            cleanup_pending: false,
        }
    }
}

/// Runs `bb` with `args` (the first one is the program name).
///
/// `interrupt` returns a future that completes on the next Ctrl-C; it is
/// called only once the command runs (after any prompt), and again after
/// the first interrupt of a transfer, whose clean-up a second interrupt
/// cuts short.
pub async fn run<A, T, I, F>(args: A, io: Io, secrets: Secrets, interrupt: I) -> Exit
where
    A: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    I: FnMut() -> F,
    F: Future<Output = ()>,
{
    let Io {
        env,
        stdin,
        mut stdout,
        stderr,
        console,
    } = io;
    let stderr = Stderr::new(stderr);
    let parsed = Cli::command()
        .try_get_matches_from(args)
        .and_then(|matches| Cli::from_arg_matches(&matches).map(|cli| (cli, matches)));
    let (cli, matches) = match parsed {
        Ok(parsed) => parsed,
        Err(err) => return clap_exit(&err, &mut *stdout, &stderr),
    };
    let json = cli.global.json;
    let inputs = Inputs {
        env,
        stdin: Mutex::new(stdin),
        console,
        secrets,
    };
    let mut out = Output::new(json, cli.global.quiet, stdout, stderr.clone());
    let (result, cleanup_pending) = match cli::copy_rules(&matches) {
        Ok(rules) => execute(&cli, &rules, &inputs, &mut out, interrupt).await,
        Err(err) => (Err(err.into()), false),
    };
    let flushed = out.flush();
    match result.and(flushed) {
        Ok(()) | Err(Failure::BrokenPipe) => Exit::code(0),
        Err(Failure::Error(err)) => {
            report(&err, json, &stderr);
            Exit {
                code: err.exit_code(),
                cleanup_pending: cleanup_pending && err.kind() == ErrorKind::Cancelled,
            }
        }
    }
}

/// Help and version go to stdout with status 0; usage errors to stderr
/// with status 1 (clap's own default would be 2).
fn clap_exit(err: &clap::Error, stdout: &mut dyn Write, stderr: &Stderr) -> Exit {
    let text = err.render().to_string();
    if err.use_stderr() {
        stderr.raw(&text);
        Exit::code(ErrorKind::Usage.exit_code())
    } else {
        // A closed stdout is not an error for help either.
        let _ = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush());
        Exit::code(0)
    }
}

/// The error on stderr: `bb: error: ...`, or the `{code, message}` record
/// with `--json`.
fn report(err: &Error, json: bool, stderr: &Stderr) {
    let line = if json {
        json_line(&err.to_payload()).unwrap_or_else(|_| format!("bb: error: {err}"))
    } else {
        format!("bb: error: {err}")
    };
    stderr.line(&line);
}

/// Runs the command; the flag is set when a cancelled transfer may have
/// left background clean-up running.
async fn execute<I, F>(
    cli: &Cli,
    rules: &IncludeExclude,
    inputs: &Inputs,
    out: &mut Output,
    mut interrupt: I,
) -> (Result<(), Failure>, bool)
where
    I: FnMut() -> F,
    F: Future<Output = ()>,
{
    // Usage errors come before credentials are needed.
    let checked = match &cli.command {
        // Prompts and short local changes: Ctrl-C ends the process as usual.
        Command::Account(command) => {
            return (
                commands::account::run(&cli.global, command, inputs, out).await,
                false,
            );
        }
        // IAM needs no S3 transport, and some of its commands no account.
        Command::Iam(command) => {
            let run = commands::iam::run(&cli.global, command, inputs, out);
            return tokio::select! {
                result = run => (result, false),
                () = interrupt() => (Err(Error::Cancelled.into()), false),
            };
        }
        // Saved sync jobs live in the configuration: no account is needed.
        Command::Sync(args) if args.command.is_some() => {
            let Some(SyncCommand::Jobs(command)) = &args.command else {
                return (Ok(()), false);
            };
            return (
                commands::syncjobs::run(&cli.global, command, inputs, out),
                false,
            );
        }
        Command::Cp(args) => commands::copy::check(args, Mode::Copy),
        Command::Mv(args) => commands::copy::check(args, Mode::Move),
        Command::Sync(args) => commands::sync::check(args),
        Command::Sse(args) => commands::objectmeta::check_sse(args),
        Command::Lock(LockCommand::Set(args)) => commands::objectlock::check_set(args),
        _ => Ok(()),
    };
    // The encryption password is asked for before credentials are needed.
    let checked = checked.and_then(|()| commands::cse::resolve(&cli.command, inputs));
    // A saved sync job runs with its own account.
    let job = match (&cli.command, &checked) {
        (Command::Sync(args), Ok(_)) => commands::sync::resolve_job(&cli.global, args, inputs),
        _ => Ok(None),
    };
    let job = match job {
        Ok(job) => job,
        Err(err) => return (Err(err.into()), false),
    };
    let global = job.as_ref().map_or(&cli.global, |(_, global)| global);
    let (ctx, cse) = match checked {
        Ok(cse) => (Context::new(global, inputs).await, cse),
        Err(err) => (Err(err), None),
    };
    let ctx = match ctx {
        Ok(ctx) => ctx,
        Err(err) => return (Err(err.into()), false),
    };
    let accounts = match commands::copy::accounts(&cli.command, &cli.global, inputs).await {
        Ok(accounts) => accounts,
        Err(err) => return (Err(err.into()), false),
    };
    let job = job.map(|(job, _)| job);
    let mut command = pin!(dispatch(
        &ctx,
        &cli.command,
        rules,
        cse,
        &accounts,
        job,
        out
    ));
    tokio::select! {
        result = &mut command => (result, false),
        () = interrupt() => {
            if matches!(cli.command, Command::Edit(_)) {
                // Ctrl-C is how an edit session ends: it uploads the last
                // save and reports how that went; a second Ctrl-C gives up.
                ctx.stop.cancel();
                return tokio::select! {
                    result = &mut command => (result, false),
                    () = interrupt() => (Err(Error::Cancelled.into()), false),
                };
            }
            if !cli.command.is_transfer() {
                // Nothing to clean up: drop the request in flight.
                return (Err(Error::Cancelled.into()), false);
            }
            // The engines abort multipart uploads and remove partial
            // downloads when they see the stop; give them the time.
            ctx.stop.cancel();
            tokio::select! {
                _ = command => {}
                () = interrupt() => {}
                () = tokio::time::sleep(CLEANUP_GRACE) => {}
            }
            (Err(Error::Cancelled.into()), true)
        }
    }
}

async fn dispatch(
    ctx: &Context,
    command: &Command,
    rules: &IncludeExclude,
    cse: Option<SecretString>,
    accounts: &commands::copy::CopyAccounts,
    job: Option<bucketbridge_core::syncjobs::SyncJob>,
    out: &mut Output,
) -> Result<(), Failure> {
    match command {
        Command::Ls(args) => commands::ls::run(ctx, args, out).await,
        Command::Mb(args) => commands::bucket::make(ctx, args, out).await,
        Command::Rb(args) => commands::bucket::remove(ctx, args, out).await,
        Command::Rm(args) => commands::rm::run(ctx, args, out).await,
        Command::RestoreVersion(args) => commands::restore::run(ctx, args, out).await,
        Command::Cp(args) => {
            commands::copy::run(ctx, args, rules, Mode::Copy, cse, accounts, out).await
        }
        Command::Mv(args) => {
            commands::copy::run(ctx, args, rules, Mode::Move, cse, accounts, out).await
        }
        Command::Sync(args) => commands::sync::run(ctx, args, job, out).await,
        Command::Encrypt(args) => commands::cse::encrypt(ctx, args, cse, out).await,
        Command::Decrypt(args) => commands::cse::decrypt(ctx, args, cse, out).await,
        Command::Presign(args) => commands::presign::run(ctx, args, out).await,
        Command::Edit(args) => commands::edit::run(ctx, args, out).await,
        #[cfg(feature = "mount")]
        Command::Mount(args) => commands::mount::run(ctx, args, out).await,
        Command::Scan(args) => commands::scan::run(ctx, args, out).await,
        Command::Export(args) => commands::export::run(ctx, args, out).await,
        Command::Mpu(MpuCommand::List(args)) => commands::mpu::list(ctx, args, out).await,
        Command::Mpu(MpuCommand::Abort(args)) => commands::mpu::abort(ctx, args, out).await,
        Command::Tag(TagCommand::Get(args)) => commands::objectmeta::tag_get(ctx, args, out).await,
        Command::Tag(TagCommand::Set(args)) => commands::objectmeta::tag_set(ctx, args, out).await,
        Command::Tag(TagCommand::Delete(args)) => {
            commands::objectmeta::tag_delete(ctx, args, out).await
        }
        Command::StorageClass(args) => commands::objectmeta::storage_class(ctx, args, out).await,
        Command::Sse(args) => commands::objectmeta::sse(ctx, args, out).await,
        Command::Acl(AclCommand::Get(args)) => commands::objectmeta::acl_get(ctx, args, out).await,
        Command::Acl(AclCommand::Set(args)) => commands::objectmeta::acl_set(ctx, args, out).await,
        Command::Lock(LockCommand::Get(args)) => {
            commands::objectlock::lock_get(ctx, args, out).await
        }
        Command::Lock(LockCommand::Set(args)) => {
            commands::objectlock::lock_set(ctx, args, out).await
        }
        Command::Restore(args) => commands::objectlock::restore(ctx, args, out).await,
        Command::Cloudfront(command) => commands::cloudfront::run(ctx, command, out).await,
        Command::Iam(_) => {
            Err(Error::Internal("iam commands run without an S3 context".into()).into())
        }
        Command::Account(_) => {
            Err(Error::Internal("account commands run without an account context".into()).into())
        }
    }
}
