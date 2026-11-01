//! Data model shared by the desktop host, the UI contract and the CLI.
//!
//! Every type here serializes to exactly the JSON documented in the "Types"
//! section of `docs/desktop-api.md`: `snake_case` fields, unit enums as
//! `snake_case` strings, tagged unions with a `type` field, RFC 3339 UTC
//! timestamps and integer byte counts. Types not in the contract (such as
//! [`ObjectHead`] or [`DeleteOutcome`]) follow the same conventions.
//!
//! One file per area, so that feature work on one area does not touch the
//! others: accounts, buckets, objects, transfers, sync, the event log and
//! filters.

mod account;
mod bucket;
mod filter;
mod log;
mod object;
mod sync;
mod transfer;

pub use account::{
    Account, AccountInput, AccountList, AccountUpdate, DEFAULT_REGION, SecurityStatus,
};
pub use bucket::{Bucket, BucketKind};
pub use filter::{FilterSpec, SavedFilter};
pub use log::{LogEntry, LogLevel};
pub use object::{
    Checksums, CopyOutcome, DeleteFailure, DeleteOutcome, LockMode, MultipartUpload, ObjectEntry,
    ObjectHead, ObjectLock, ObjectMeta, ObjectPage, ObjectVersion, Sse, VersionPage,
};
pub use sync::{FileMeta, Side, SyncAction, SyncDirection, SyncPlan, SyncRow};
pub use transfer::{Location, TransferKind, TransferState, TransferTask};
