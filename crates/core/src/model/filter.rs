//! Saved object filters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Criteria for filtering an object listing. Unset fields match everything.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterSpec {
    /// Key prefix.
    pub prefix: Option<String>,
    /// Glob matched against the key relative to the listing prefix.
    pub glob: Option<String>,
    /// Minimum size in bytes (inclusive).
    pub min_size: Option<u64>,
    /// Maximum size in bytes (inclusive).
    pub max_size: Option<u64>,
    /// Only objects modified after this time.
    pub modified_after: Option<DateTime<Utc>>,
    /// Only objects modified before this time.
    pub modified_before: Option<DateTime<Utc>>,
    /// Allowed storage classes; empty means any.
    pub storage_classes: Vec<String>,
}

/// A named filter stored in the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SavedFilter {
    /// Name shown in the UI.
    pub name: String,
    /// The criteria.
    pub filter: FilterSpec,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn shape_matches_contract() {
        let saved = SavedFilter {
            name: "big logs".into(),
            filter: FilterSpec {
                prefix: Some("logs/".into()),
                glob: Some("*.gz".into()),
                min_size: Some(1024),
                max_size: None,
                modified_after: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
                modified_before: None,
                storage_classes: vec!["STANDARD_IA".into()],
            },
        };
        let value = serde_json::to_value(&saved).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "big logs",
                "filter": {
                    "prefix": "logs/",
                    "glob": "*.gz",
                    "min_size": 1024,
                    "max_size": null,
                    "modified_after": "2026-01-01T00:00:00Z",
                    "modified_before": null,
                    "storage_classes": ["STANDARD_IA"]
                }
            })
        );
        assert_eq!(serde_json::from_value::<SavedFilter>(value).unwrap(), saved);
    }

    #[test]
    fn partial_filter_defaults() {
        let spec: FilterSpec = serde_json::from_value(json!({ "glob": "*.jpg" })).unwrap();
        assert_eq!(spec.glob.as_deref(), Some("*.jpg"));
        assert!(spec.storage_classes.is_empty());
    }
}
