//! IPC tests of the desktop commands: a mock-runtime app with the real
//! handler and state, commands invoked with JSON arguments as the UI sends
//! them, contract shapes and error codes asserted on the JSON that comes
//! back. S3 is the in-process test server of `bucketbridge-tests`; secrets
//! live in an in-memory store, never in the OS keychain.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod accountfile;
mod accounts;
mod app;
mod bookmarks;
mod browse;
mod bucketcfg;
mod bucketrules;
mod cloudfront;
mod credentials;
mod cse;
mod edit;
mod everyday;
mod harness;
mod iam;
mod objectlock;
mod objectmeta;
mod scan;
mod share;
mod sync;
mod syncjobs;
mod tools;
mod transfers;
mod usage;
mod versions;
mod windows;
