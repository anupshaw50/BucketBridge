//! Core library of BucketBridge, an open-source S3 client.
//!
//! Shared by the desktop host and the `bb` CLI so that no logic is
//! duplicated. Module map:
//!
//! | Module | Contents |
//! |---|---|
//! | [`error`] | [`Error`], [`ErrorKind`] (the IPC `BbError` codes and CLI exit codes) |
//! | [`model`] | Data types of the IPC contract (accounts, buckets, objects, transfers, sync, log, filters) |
//! | [`settings`] | [`Settings`] with documented defaults and validation |
//! | [`config`] | `config.toml` ([`AppConfig`], [`ConfigStore`]) and application directories ([`Dirs`]) |
//! | [`s3uri`] | `s3://bucket/key` parsing and S3 naming rules |
//! | [`path`] | Local path and key mapping, including the download path safety check |
//! | [`client`] | Building an `aws_sdk_s3::Client` for an account ([`Credentials`], [`ClientOptions`]) |
//! | [`ops`] | The S3 transport ([`S3`]): buckets, listings, metadata, deletes, copies, presigned URLs, multipart uploads |
//! | [`bucketcfg`] | Bucket configuration on [`S3`]: policy, public access block, versioning, logging, website, acceleration; external buckets |
//! | [`bucketrules`] | Bucket rules (T04/T07): lifecycle, Intelligent-Tiering, CORS and replication, with validation before send |
//! | [`objectmeta`] | Object metadata (T06): tags, storage class, server-side encryption, HTTP headers, user metadata, canned ACLs, with batch editing |
//! | [`filter`] | Object filters ([`Filter`]) and the CLI's ordered include/exclude rules ([`IncludeExclude`]) |
//! | [`sync`] | Folder sync: dry-run analysis, the planner, the resumable plan store ([`SyncStore`]) and plan execution ([`SyncExecutor`]) |
//! | [`eventlog`] | The event log ([`EventLog`]) with file rotation, export and a `tracing` layer |
//! | [`crash`] | Local crash reports ([`CrashReport`]): panic hook, newest report, dismiss |
//! | [`secrets`] | Secret storage: the OS keychain ([`secrets::SecretStore`]) and the master-password [`secrets::Vault`] |
//! | [`mfa`] | MFA sessions from STS `GetSessionToken` ([`mfa::MfaSessions`]) |
//! | [`proxy`] | The proxy-aware HTTP client shared by every SDK client |
//! | [`accounts`] | [`AccountManager`]: accounts, their secrets and the master password, crash-safe |
//! | [`session`] | [`Session`]: the process-wide entry point (accounts, unlock, MFA, cached S3 handles) |
//! | [`diagnostics`] | Step-by-step probe of an account: DNS, TCP, proxy, TLS, credentials and bucket region |
//! | [`export`] | Export of a listing to CSV, XLSX or Parquet |
//! | [`saved_filters`] | The saved filters kept in `config.toml` |
//! | [`cloudfront`] | CloudFront distributions, invalidations and saved path sets ([`cloudfront::CloudFront`]) |
//! | [`objectlock`] | Object Lock, object ownership, Glacier restore and MFA delete |
//! | [`versions`] | Version lists grouped by key with filters, restore, purge, download of one version |
//! | [`cse`] | Client-side compression and encryption: streaming AES-256-GCM envelope, password states, in-place rewrite of objects |
//! | [`preview`] | Object previews: ranged reads, image, text and JSON detection, paging of text |
//! | [`shorten`] | Optional URL shorteners (Bitly, TinyURL) for share links |
//! | [`edit`] | Edit in place: a local copy of an object whose saves are uploaded, with a conflict check ([`edit::EditManager`]) |
//! | [`scan`] | Security scan (T13): public ACLs and policies, public access block, default encryption, versioning, object ownership |
//!
//! All fallible functions return [`Result`]. Blocking functions (the
//! configuration store, the sync analysis and plan store, log export, crash
//! reports) are documented as such; everything that talks to S3 is `async`
//! and runs on Tokio.

#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod accounts;
pub mod bookmarks;
pub mod bucketcfg;
pub mod bucketrules;
pub mod client;
pub mod cloudfront;
pub mod config;
pub mod crash;
pub mod creds;
pub mod cse;
pub mod diagnostics;
pub mod edit;
pub mod engine;
pub mod error;
pub mod eventlog;
pub mod export;
pub mod filter;
pub mod iam;
pub mod ignore;
pub mod mfa;
pub mod model;
pub mod mount;
pub mod objectlock;
pub mod objectmeta;
pub mod ops;
pub mod path;
pub mod preview;
pub mod proxy;
pub mod s3uri;
pub mod saved_filters;
pub mod scan;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod shorten;
pub mod sync;
pub mod syncjobs;
pub mod transfer;
pub mod versions;

pub use accounts::AccountManager;
pub use client::{ClientOptions, Credentials, build_client, build_client_with_provider};
pub use config::{AppConfig, ConfigStore, Dirs};
pub use crash::CrashReport;
pub use error::{BbError, Error, ErrorKind, Result};
pub use eventlog::{EventLog, ExportFormat};
pub use filter::{Filter, IncludeExclude};
pub use ops::{CopyOptions, S3};
pub use s3uri::S3Uri;
pub use session::Session;
pub use settings::Settings;
pub use sync::{SyncExecutor, SyncStore};

/// Re-export of the AWS SDK for CloudFront, so dependents use the same version.
pub use aws_sdk_cloudfront;
/// Re-export of the AWS SDK for S3, so dependents use the same version.
pub use aws_sdk_s3;
/// Re-export of the AWS SDK for IAM Identity Center (portal), for tests of dependents.
pub use aws_sdk_sso;
/// Re-export of the AWS SDK for IAM Identity Center (OIDC), for tests of dependents.
pub use aws_sdk_ssooidc;
/// Re-export of the AWS SDK for STS, for tests of dependents.
pub use aws_sdk_sts;
