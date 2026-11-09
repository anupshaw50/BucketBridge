//! `bb`, the BucketBridge command-line S3 client, as a library: the binary
//! is a thin wrapper around [`run`], and tests run commands in-process
//! with captured streams and an injected secret store. See `docs/cli.md`.
//!
//! Commands: `ls`, `mb`, `rb`, `rm`, `cp` and `mv` (uploads, downloads and
//! copies between S3 locations), `sync` (a local folder and an S3 prefix),
//! `presign`, `mpu list|abort`, `export` (a listing as CSV, Parquet or XLSX)
//! and `account add|list|remove|default`. Commands
//! `presign`, `mpu list|abort`, `tag get|set|delete`, `storage-class`, `sse`,
//! `acl get|set` and `account add|list|remove|default`. Commands
//! run as the account named with `--account`, else the default stored
//! account, else the ephemeral `environment` account built from the `BB_*`
//! variables.
//!
//! Exit codes are stable: 0 success, 1 usage error, 2 remote or local
//! failure (including not found and conflicts), 3 authentication failure
//! or locked secrets, 130 interrupted. Data goes to standard output, errors
//! and progress to standard error.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod ask;
mod cli;
mod commands;
mod context;
mod duration;
pub mod i18n;
mod io;
mod location;
mod output;
mod paging;
mod records;
mod transfer;

pub use app::{Exit, run};
pub use context::{Secrets, SessionOptionsHook};
pub use io::{Console, Env, Io, NoConsole, ProcessConsole};
