//! The process-wide `tracing` subscriber that feeds the event log
//! (docs/logging.md, "Forwarding `tracing` events").

use bucketbridge_core::EventLog;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;

/// Installs the global subscriber: information and above from BucketBridge's
/// own targets (`bucketbridge*`), warnings and errors from dependencies.
/// Entries below `settings.logging.level` are dropped by the log itself.
///
/// Nothing logged here carries a secret: passwords arrive as
/// [`crate::Secret`], and no command logs its arguments.
pub(crate) fn install(log: &EventLog) {
    let targets = Targets::new()
        .with_target("bucketbridge", LevelFilter::INFO)
        .with_default(LevelFilter::WARN);
    let subscriber = tracing_subscriber::registry().with(log.layer().with_filter(targets));
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        eprintln!("BucketBridge: a tracing subscriber is already installed; it is kept");
    }
}
