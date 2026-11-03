//! Transfer engine: the transport-agnostic transfer queue and the building
//! blocks that the S3 runners (upload, download, copy, move, delete) use.
//!
//! * [`TransferQueue`] owns every [`TransferTask`](crate::model::TransferTask),
//!   schedules them under a concurrency cap, retries retryable failures with
//!   backoff, persists the queue, and publishes [`QueueEvent`]s.
//! * [`TaskRunner`] is the seam to the transport: the queue hands a runner a
//!   [`TaskContext`] (task snapshot, settings snapshot, [`Progress`] reporter,
//!   the shared [`BandwidthLimiter`], a [`StopSignal`] and the persisted
//!   [`ResumeState`]) and calls [`TaskRunner::cleanup`] when a task is
//!   cancelled.
//! * [`BandwidthLimiter`], [`ThrottledStream`] and [`ThrottledRead`] throttle
//!   all transfers together.
//! * [`plan_transfer`] decides between a single request and a multipart
//!   transfer and yields the part ranges.
//! * [`RetryPolicy`] and [`Backoff`] implement capped exponential backoff with
//!   full jitter; [`is_retryable`] classifies errors.
//! * [`RateEstimator`] and [`eta_secs`] compute the smoothed speed and ETA.
//!
//! The design, the task state machine, the persistence format and the runner
//! contract are described in `docs/transfers.md`.

mod limiter;
mod parts;
mod queue;
mod rate;
mod retry;
mod runner;
mod store;

pub use limiter::{Acquire, BandwidthLimiter, MAX_CHUNK, ThrottledRead, ThrottledStream};
pub use parts::{
    MAX_MULTIPART_SIZE, MAX_PART_COUNT, MAX_PART_SIZE, MIB, MIN_PART_SIZE, MultipartPlan, Part,
    Parts, TransferPlan, plan_for_settings, plan_transfer,
};
pub use queue::{FailedTask, NewTask, QueueEvent, QueueOptions, TransferQueue};
pub use rate::{RateEstimator, eta_secs};
pub use retry::{Backoff, RandomSource, RetryPolicy, SplitMix64, is_retryable};
pub use runner::{
    Progress, ResumeState, Runners, SkipSlot, StopController, StopReason, StopSignal, TaskContext,
    TaskRunner,
};

use crate::error::{Error, ErrorKind};

// The only two places where this module constructs errors, so that adapting
// to the constructors of `crate::error::Error` touches these functions only.

/// An error of `kind` with `message`. File-system failures use [`io_error`].
/// `Locked` and `Cancelled` carry no message in [`Error`].
fn new_error(kind: ErrorKind, message: impl Into<String>) -> Error {
    let message = message.into();
    match kind {
        ErrorKind::Usage => Error::Usage(message),
        ErrorKind::Auth => Error::Auth(message),
        ErrorKind::Locked => Error::Locked,
        ErrorKind::MfaRequired => Error::MfaRequired(message),
        ErrorKind::NotFound => Error::NotFound(message),
        ErrorKind::Conflict => Error::Conflict(message),
        ErrorKind::Remote => Error::Remote(message),
        ErrorKind::Io => Error::io(message, std::io::Error::other("I/O failure")),
        ErrorKind::Cancelled => Error::Cancelled,
        ErrorKind::Internal => Error::Internal(message),
    }
}

/// An `io` error: what was being done, and the underlying failure.
fn io_error(context: impl Into<String>, source: std::io::Error) -> Error {
    Error::io(context, source)
}
