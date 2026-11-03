//! Options of a sync beyond where it copies and which way (saved jobs, v0.2.0).
//!
//! [`SyncOptions`] is what a saved sync job stores and what `sync_analyze` and
//! `bb sync` accept: the file filters, the exclusion rules and the analysis options
//! (skip empty folders, skip duplicate content, the metadata cache, parallel analysis).
//! Everything defaults to the behaviour of a sync without options.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filter::{Filter, Glob};
use crate::ignore::IgnoreRules;
use crate::model::FilterSpec;

/// Default number of remote prefixes listed at the same time.
pub const DEFAULT_PARALLELISM: u32 = 4;
/// Largest accepted `parallelism`.
pub const MAX_PARALLELISM: u32 = 32;
/// Most inline include or exclude lines in one set of options.
pub const MAX_RULE_LINES: usize = 1000;

fn default_true() -> bool {
    true
}

fn default_parallelism() -> u32 {
    DEFAULT_PARALLELISM
}

/// Filters and analysis options of a sync. Unset fields keep the plain behaviour, and
/// a stored value that is missing (an older `config.toml`) takes its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncOptions {
    /// Prefix, glob, size, date and storage-class criteria (see `docs/sync.md`).
    pub filter: FilterSpec,
    /// Include globs: when not empty, only paths matching one of them are synced.
    pub include: Vec<String>,
    /// Gitignore-style exclusion rules, one per entry (see [`crate::ignore`]).
    pub exclude: Vec<String>,
    /// A file of exclusion rules, read at every run; its rules come after `exclude`.
    pub exclude_file: Option<String>,
    /// Ignore empty folders (the default and the behaviour of every earlier version).
    /// When false, an empty local folder gets a folder marker in S3 (upload and
    /// S3-to-S3 also copy markers), and a marker in S3 becomes an empty local folder
    /// (download).
    #[serde(default = "default_true")]
    pub skip_empty_folders: bool,
    /// Treat a file as equal when its size and checksum match, even if the
    /// modification times differ.
    pub skip_duplicate_content: bool,
    /// Use the local metadata cache of remote listings (never for a sync that deletes).
    #[serde(default = "default_true")]
    pub use_cache: bool,
    /// Remote prefixes listed at the same time (1 to [`MAX_PARALLELISM`]).
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            filter: FilterSpec::default(),
            include: Vec::new(),
            exclude: Vec::new(),
            exclude_file: None,
            skip_empty_folders: true,
            skip_duplicate_content: false,
            use_cache: true,
            parallelism: DEFAULT_PARALLELISM,
        }
    }
}

impl SyncOptions {
    /// Checks the options without touching the file system: bounds, globs and rules.
    /// The exclusion file is read (and checked) by [`SyncOptions::compile_filter`].
    pub fn validate(&self) -> Result<()> {
        if self.parallelism == 0 || self.parallelism > MAX_PARALLELISM {
            return Err(Error::Usage(format!(
                "parallelism must be between 1 and {MAX_PARALLELISM}"
            )));
        }
        if self.include.len() > MAX_RULE_LINES || self.exclude.len() > MAX_RULE_LINES {
            return Err(Error::Usage(format!(
                "at most {MAX_RULE_LINES} include and {MAX_RULE_LINES} exclude rules are allowed"
            )));
        }
        for pattern in &self.include {
            Glob::new(pattern.trim())
                .map_err(|err| Error::Usage(format!("include pattern '{pattern}': {err}")))?;
        }
        IgnoreRules::parse(&self.exclude)?;
        if let Some(file) = &self.exclude_file
            && file.trim().is_empty()
        {
            return Err(Error::Usage("the exclusion file path is empty".into()));
        }
        Filter::compile(&self.filter)?;
        Ok(())
    }

    /// The compiled filter: the criteria, the include globs and the exclusion rules
    /// (inline rules first, then the file's). Reads the exclusion file (blocking); a
    /// missing or unreadable file is an error, never silently "no rules".
    pub fn compile_filter(&self) -> Result<Filter> {
        self.validate()?;
        let filter = Filter::compile(&self.filter)?;
        let include = self
            .include
            .iter()
            .map(|pattern| Glob::new(pattern.trim()))
            .collect::<Result<Vec<_>>>()?;
        let mut rules = IgnoreRules::parse(&self.exclude)?;
        if let Some(file) = self.exclude_file.as_deref() {
            rules.extend(IgnoreRules::from_file(Path::new(file.trim()))?);
        }
        Ok(filter.with_path_rules(include, rules))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn defaults_keep_the_plain_behaviour() {
        let options = SyncOptions::default();
        assert!(options.skip_empty_folders);
        assert!(!options.skip_duplicate_content);
        assert!(options.use_cache);
        assert_eq!(options.parallelism, 4);
        assert!(options.compile_filter().unwrap().is_unrestricted());
        let empty: SyncOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, options);
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut options = SyncOptions {
            parallelism: 0,
            ..SyncOptions::default()
        };
        assert_eq!(options.validate().unwrap_err().kind(), ErrorKind::Usage);
        options.parallelism = MAX_PARALLELISM + 1;
        assert!(options.validate().is_err());
        options.parallelism = 2;
        options.include = vec!["[bad".into()];
        assert!(options.validate().is_err());
        options.include.clear();
        options.exclude = vec!["[bad".into()];
        assert!(options.validate().is_err());
        options.exclude.clear();
        options.filter.min_size = Some(10);
        options.filter.max_size = Some(1);
        assert!(options.validate().is_err());
    }

    #[test]
    fn the_filter_combines_inline_rules_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rules.txt");
        std::fs::write(&file, "# generated\n*.tmp\n!keep.tmp\n").unwrap();
        let options = SyncOptions {
            include: vec!["**/*".into()],
            exclude: vec!["*.bak".into(), "keep.tmp".into()],
            exclude_file: Some(file.display().to_string()),
            ..SyncOptions::default()
        };
        let filter = options.compile_filter().unwrap();
        assert!(!filter.matches_path("a.bak"));
        assert!(!filter.matches_path("x/a.tmp"));
        // The file's negation comes after the inline rule, so it wins.
        assert!(filter.matches_path("keep.tmp"));
        assert!(filter.matches_path("doc.txt"));

        let missing = SyncOptions {
            exclude_file: Some(dir.path().join("none").display().to_string()),
            ..SyncOptions::default()
        };
        assert_eq!(
            missing.compile_filter().unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }
}
