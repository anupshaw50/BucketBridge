//! Event log entries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Severity of an event-log entry.
///
/// Ordered by severity: `Information < Warning < Error`, so "record entries
/// at or above the configured level" is `entry.level >= settings.level`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Informational message.
    #[default]
    Information,
    /// Something unexpected that did not fail an operation.
    Warning,
    /// A failed operation.
    Error,
}

/// One event-log entry.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LogEntry {
    /// Monotonic sequence number.
    pub id: u64,
    /// When it happened.
    pub ts: DateTime<Utc>,
    /// Severity.
    pub level: LogLevel,
    /// Component that produced it, such as `transfer` or `sync`.
    pub source: String,
    /// Message text.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn level_spellings_and_order() {
        assert_eq!(
            serde_json::to_value(LogLevel::Error).unwrap(),
            json!("error")
        );
        assert_eq!(
            serde_json::to_value(LogLevel::Warning).unwrap(),
            json!("warning")
        );
        assert_eq!(
            serde_json::to_value(LogLevel::Information).unwrap(),
            json!("information")
        );
        assert!(LogLevel::Information < LogLevel::Warning);
        assert!(LogLevel::Warning < LogLevel::Error);
    }

    #[test]
    fn entry_shape() {
        let entry = LogEntry {
            id: 7,
            ts: Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap(),
            level: LogLevel::Warning,
            source: "transfer".into(),
            message: "retrying part 3".into(),
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            value,
            json!({
                "id": 7,
                "ts": "2026-10-29T08:15:00Z",
                "level": "warning",
                "source": "transfer",
                "message": "retrying part 3"
            })
        );
        assert_eq!(serde_json::from_value::<LogEntry>(value).unwrap(), entry);
    }
}
