//! The filters of `bb ls --versions` and `bb rm --versions`: age, current or
//! not, delete markers, and key globs. Pure functions over listed versions,
//! so they are tested without a server.

use std::time::Duration;

use bucketbridge_core::filter::Glob;
use bucketbridge_core::model::ObjectVersion;
use bucketbridge_core::path::parent_prefix;
use bucketbridge_core::{Result, S3Uri};
use chrono::{DateTime, NaiveDate, Utc};

use crate::cli::VersionFilterArgs;
use crate::duration::parse_duration;

/// What `--older-than` was given: an age, or a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cutoff {
    /// Versions last modified more than this long ago.
    Age(Duration),
    /// Versions last modified before this time.
    At(DateTime<Utc>),
}

impl Cutoff {
    /// The moment before which a version counts as old.
    pub fn resolve(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Cutoff::Age(age) => {
                let age = chrono::Duration::from_std(*age).unwrap_or(chrono::Duration::MAX);
                now.checked_sub_signed(age)
                    .unwrap_or(DateTime::<Utc>::MIN_UTC)
            }
            Cutoff::At(time) => *time,
        }
    }
}

/// Parses `--older-than`: a duration (`30d`, `12h`, `1h30m`, `90`), a date
/// (`2026-01-31`, midnight UTC) or an RFC 3339 time (`2026-01-31T08:00:00Z`).
pub fn parse_cutoff(text: &str) -> std::result::Result<Cutoff, String> {
    if text.contains(['-', ':']) {
        if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d")
            && let Some(midnight) = date.and_hms_opt(0, 0, 0)
        {
            return Ok(Cutoff::At(midnight.and_utc()));
        }
        if let Ok(time) = DateTime::parse_from_rfc3339(text) {
            return Ok(Cutoff::At(time.with_timezone(&Utc)));
        }
        return Err(crate::i18n::trf(
            "invalid age or date '{text}': use a duration such as 30d or 12h, a date such as 2026-01-31, or an RFC 3339 time",
            &[("text", text)],
        ));
    }
    match parse_duration(text) {
        Ok(age) => Ok(Cutoff::Age(age)),
        Err(_) => Err(crate::i18n::trf(
            "invalid age or date '{text}': use a duration such as 30d or 12h, a date such as 2026-01-31, or an RFC 3339 time",
            &[("text", text)],
        )),
    }
}

/// A compiled set of version filters. A version must pass every one.
#[derive(Debug, Clone, Default)]
pub struct VersionFilter {
    older_than: Option<DateTime<Utc>>,
    noncurrent_only: bool,
    delete_markers_only: bool,
    globs: Vec<Glob>,
    /// What is removed from a key before the globs see it: the listed
    /// folder, so that `*.gz` means "directly in this folder".
    base: String,
}

impl VersionFilter {
    /// Compiles the options. `base` is the key prefix the globs are relative
    /// to. A malformed glob is a usage error.
    pub fn new(
        older_than: Option<&Cutoff>,
        now: DateTime<Utc>,
        noncurrent_only: bool,
        delete_markers_only: bool,
        globs: &[String],
        base: &str,
    ) -> Result<Self> {
        Ok(VersionFilter {
            older_than: older_than.map(|cutoff| cutoff.resolve(now)),
            noncurrent_only,
            delete_markers_only,
            globs: globs
                .iter()
                .map(|pattern| Glob::new(pattern))
                .collect::<Result<Vec<_>>>()?,
            base: base.to_owned(),
        })
    }

    /// The filters given on the command line for the listing of `uri`; the
    /// globs are relative to the folder the URI names (or contains).
    pub fn from_args(args: &VersionFilterArgs, uri: &S3Uri) -> Result<Self> {
        let base = if uri.is_prefix() {
            uri.key()
        } else {
            parent_prefix(uri.key())
        };
        Self::new(
            args.older_than.as_ref(),
            Utc::now(),
            args.noncurrent_only,
            args.delete_markers_only,
            &args.key_glob,
            base,
        )
    }

    /// Whether any filter is set.
    pub fn is_active(&self) -> bool {
        self.older_than.is_some()
            || self.noncurrent_only
            || self.delete_markers_only
            || !self.globs.is_empty()
    }

    /// Whether `version` passes every filter. A version without a
    /// modification time is never "older than" anything.
    pub fn matches(&self, version: &ObjectVersion) -> bool {
        if self.noncurrent_only && version.is_latest {
            return false;
        }
        if self.delete_markers_only && !version.is_delete_marker {
            return false;
        }
        if let Some(cutoff) = self.older_than {
            match version.last_modified {
                Some(modified) if modified < cutoff => {}
                _ => return false,
            }
        }
        if !self.globs.is_empty() {
            let relative = version
                .key
                .strip_prefix(self.base.as_str())
                .unwrap_or(&version.key);
            if !self.globs.iter().all(|glob| glob.is_match(relative)) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap()
    }

    fn version(key: &str, days_old: i64, latest: bool, marker: bool) -> ObjectVersion {
        ObjectVersion {
            key: key.into(),
            version_id: "v".into(),
            is_latest: latest,
            is_delete_marker: marker,
            last_modified: Some(now() - chrono::Duration::days(days_old)),
            ..ObjectVersion::default()
        }
    }

    fn filter(
        older: Option<&str>,
        noncurrent: bool,
        markers: bool,
        globs: &[&str],
    ) -> VersionFilter {
        let cutoff = older.map(|text| parse_cutoff(text).unwrap());
        let globs: Vec<String> = globs.iter().map(|g| (*g).to_owned()).collect();
        VersionFilter::new(cutoff.as_ref(), now(), noncurrent, markers, &globs, "logs/").unwrap()
    }

    #[test]
    fn cutoffs_parse_ages_dates_and_times() {
        assert_eq!(
            parse_cutoff("30d").unwrap(),
            Cutoff::Age(Duration::from_secs(30 * 86_400))
        );
        assert_eq!(
            parse_cutoff("2026-01-31").unwrap(),
            Cutoff::At(Utc.with_ymd_and_hms(2026, 1, 31, 0, 0, 0).unwrap())
        );
        assert_eq!(
            parse_cutoff("2026-01-31T08:00:00+02:00").unwrap(),
            Cutoff::At(Utc.with_ymd_and_hms(2026, 1, 31, 6, 0, 0).unwrap())
        );
        for bad in ["", "soon", "2026-13-40", "-5d", "1 week"] {
            assert!(parse_cutoff(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Cutoff::Age(Duration::from_secs(86_400)).resolve(now()),
            now() - chrono::Duration::days(1)
        );
    }

    #[test]
    fn no_options_means_no_filter() {
        let none = filter(None, false, false, &[]);
        assert!(!none.is_active());
        assert!(none.matches(&version("logs/a", 0, true, false)));
    }

    #[test]
    fn age_is_strictly_older() {
        let old = filter(Some("30d"), false, false, &[]);
        assert!(old.matches(&version("logs/a", 31, false, false)));
        assert!(!old.matches(&version("logs/a", 29, false, false)));
        // Exactly 30 days is not older than 30 days.
        assert!(!old.matches(&version("logs/a", 30, false, false)));
        let mut unknown = version("logs/a", 90, false, false);
        unknown.last_modified = None;
        assert!(!old.matches(&unknown));
        let dated = filter(Some("2026-11-01"), false, false, &[]);
        assert!(dated.matches(&version("logs/a", 10, false, false)));
        assert!(!dated.matches(&version("logs/a", 5, false, false)));
    }

    #[test]
    fn noncurrent_and_marker_filters() {
        let noncurrent = filter(None, true, false, &[]);
        assert!(!noncurrent.matches(&version("logs/a", 1, true, false)));
        assert!(noncurrent.matches(&version("logs/a", 1, false, false)));
        assert!(noncurrent.matches(&version("logs/a", 1, false, true)));
        let markers = filter(None, false, true, &[]);
        assert!(markers.matches(&version("logs/a", 1, true, true)));
        assert!(!markers.matches(&version("logs/a", 1, true, false)));
        let both = filter(None, true, true, &[]);
        assert!(!both.matches(&version("logs/a", 1, true, true)));
        assert!(both.matches(&version("logs/a", 1, false, true)));
    }

    #[test]
    fn globs_are_relative_to_the_listed_folder() {
        let globs = filter(None, false, false, &["*.gz"]);
        assert!(globs.matches(&version("logs/a.gz", 1, true, false)));
        assert!(!globs.matches(&version("logs/a.txt", 1, true, false)));
        // `*` does not cross a folder boundary.
        assert!(!globs.matches(&version("logs/2024/a.gz", 1, true, false)));
        let deep = filter(None, false, false, &["**/*.gz"]);
        assert!(deep.matches(&version("logs/2024/a.gz", 1, true, false)));
        // Several globs must all match.
        let two = filter(None, false, false, &["a*", "*.gz"]);
        assert!(two.matches(&version("logs/a.gz", 1, true, false)));
        assert!(!two.matches(&version("logs/b.gz", 1, true, false)));
        assert!(VersionFilter::new(None, now(), false, false, &["[".into()], "").is_err());
    }

    #[test]
    fn every_filter_must_pass() {
        let all = filter(Some("30d"), true, true, &["*.gz"]);
        assert!(all.matches(&version("logs/a.gz", 40, false, true)));
        assert!(!all.matches(&version("logs/a.gz", 40, false, false)));
        assert!(!all.matches(&version("logs/a.gz", 40, true, true)));
        assert!(!all.matches(&version("logs/a.gz", 10, false, true)));
        assert!(!all.matches(&version("logs/a.txt", 40, false, true)));
    }
}
