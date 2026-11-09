//! `bb`, the BucketBridge command-line S3 client. See `docs/cli.md` and the
//! `bucketbridge_cli` library, which does all the work.

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bucketbridge_cli::i18n::trf;
use bucketbridge_cli::{Io, Secrets};
use bucketbridge_core::ErrorKind;

/// After a cancelled transfer, how long background clean-up (such as the
/// abort of an interrupted multipart copy) may run before the process
/// exits.
const CLEANUP_GRACE: Duration = Duration::from_secs(5);

/// Stack of the thread that runs the command, as large as the main thread's
/// stack on Linux and macOS. The command's future is polled by the thread that
/// calls `block_on`, and in a debug build it needs most of 1 MiB, which is all
/// the main thread gets on Windows.
const COMMAND_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    let command = std::thread::Builder::new()
        .name("bb".to_owned())
        .stack_size(COMMAND_STACK_BYTES)
        .spawn(run_command);
    match command {
        // A panic has already been reported by the panic hook; 101 is the
        // exit code of a panic in the main thread.
        Ok(thread) => thread.join().unwrap_or(ExitCode::from(101)),
        Err(err) => {
            let _ = writeln!(
                std::io::stderr(),
                "{}",
                trf(
                    "bb: error: starting the command: {error}",
                    &[("error", &err.to_string())]
                )
            );
            ExitCode::from(2)
        }
    }
}

fn run_command() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr(),
                "{}",
                trf(
                    "bb: error: starting the async runtime: {error}",
                    &[("error", &err.to_string())]
                )
            );
            return ExitCode::from(2);
        }
    };
    let exit = runtime.block_on(bucketbridge_cli::run(
        std::env::args_os(),
        Io::process(),
        Secrets::Keychain,
        interrupted,
    ));
    if exit.cleanup_pending {
        finish_cleanup(&runtime);
    }
    if exit.code == ErrorKind::Cancelled.exit_code() {
        // Ctrl-C ends `bb` now: dropping the runtime would wait for blocking
        // work that cannot be interrupted, such as the scan of a huge folder
        // that `sync` abandoned.
        runtime.shutdown_background();
    }
    ExitCode::from(exit.code)
}

/// Completes on Ctrl-C. Without a signal handler the command simply cannot
/// be interrupted, so this then never completes.
async fn interrupted() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Lets tasks spawned during the cancelled command (such as the abort of an
/// interrupted multipart copy) finish, for at most [`CLEANUP_GRACE`].
fn finish_cleanup(runtime: &tokio::runtime::Runtime) {
    let metrics = runtime.metrics();
    let deadline = Instant::now() + CLEANUP_GRACE;
    runtime.block_on(async {
        while metrics.num_alive_tasks() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}
