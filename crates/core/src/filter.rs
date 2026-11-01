//! Object filter engine (task T12 "Filters") and the CLI's ordered include/exclude rules.
//!
//! [`Filter`] compiles a [`FilterSpec`] once and then evaluates it on listing entries
//! ([`Filter::matches_object`]) and on local files ([`Filter::matches_local`]).
//! [`IncludeExclude`] implements the `--include` / `--exclude` options of `bb cp`.
//!
//! # Paths
//!
//! Every path criterion looks at a "/"-separated path *relative to the listing prefix*:
//! when browsing `photos/` the key `photos/2024/a.jpg` is tested as `2024/a.jpg`. For a
//! folder sync the relative path is the row's `rel_path`.
//!
//! # Glob syntax
//!
//! Globs are parsed by the `globset` crate and always matched the same way on every
//! operating system (a backslash inside a key is never treated as a separator):
//!
//! - `*` matches any run of characters except `/`; `*.jpg` matches `a.jpg` but not
//!   `2024/a.jpg`.
//! - `**` as a whole path segment matches any number of segments, including none:
//!   `**/*.jpg` matches `a.jpg` and `2024/06/a.jpg`; `raw/**` matches everything below
//!   `raw/` (but not `raw` itself).
//! - `?` matches exactly one byte other than `/`, which means one ASCII character. Use `*`
//!   to match a non-ASCII character.
//! - `[abc]`, `[a-z]` and `[!abc]` match one ASCII character from (or not from) the set.
//!   A class never matches `/`, like `*` and `?`: `a[!x]b` matches `acb` but not `a/b`.
//! - `{jpg,png}` matches any of the comma-separated alternatives.
//! - `\` escapes the next character: `\*` matches a literal `*`.
//! - Matching is case-sensitive: `*.JPG` does not match `a.jpg`.
//!
//! # Other criteria
//!
//! - `prefix`: plain, case-sensitive string prefix of the relative path (not limited to
//!   whole segments: `20` matches `2024/a.jpg` and `2025/b.jpg`).
//! - `min_size` / `max_size`: both bounds are inclusive (`min_size <= size <= max_size`).
//! - `modified_after` / `modified_before`: a half-open interval, `modified_after` is
//!   inclusive and `modified_before` is exclusive (`after <= modified < before`), so
//!   "after 2024-01-01, before 2025-01-01" selects exactly the year 2024. An entry
//!   without a modification time never satisfies a date bound.
//! - `storage_classes`: compared case-insensitively; an entry without a storage class is
//!   `STANDARD`, and so is a local file (it would be uploaded as `STANDARD`). An empty
//!   list accepts every class.
//! - An empty `prefix` or `glob` string counts as "not set".
//!
//! A spec whose glob does not parse, whose size bounds are reversed (`min_size >
//! max_size`), whose date interval is empty (`modified_after >= modified_before`) or that
//! lists an empty storage class is rejected with a usage error.

use chrono::{DateTime, Utc};
use globset::GlobBuilder;
use regex_automata::meta::Regex;
use regex_automata::util::syntax;

use crate::error::{Error, Result};
use crate::model::{FileMeta, FilterSpec, ObjectEntry};

/// Storage class assumed for entries that do not report one, and for local files.
pub const DEFAULT_STORAGE_CLASS: &str = "STANDARD";

/// Size limit for the automaton compiled from one glob (same limit globset uses).
const GLOB_SIZE_LIMIT: usize = 10 * (1 << 20);

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

/// Rewrites the character classes of the regex globset derived from a pattern so that
/// they never match `/`. globset lets a negated class (`[!x]`, regex `[^x]`) and a class
/// that spans `/` cross a separator even with `literal_separator` on, which would let
/// `a[!x]b` match `a/b`. In globset's output a class starts at an unescaped `[` (a literal
/// one is written `\[`), every member inside is escaped, and it ends at the next
/// unescaped `]`.
fn exclude_separator_from_classes(regex: &str) -> String {
    let mut out = String::with_capacity(regex.len() + 16);
    let mut chars = regex.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                out.extend(chars.next());
            }
            '[' => {
                let mut class = String::new();
                while let Some(member) = chars.next() {
                    match member {
                        ']' => break,
                        '\\' => {
                            class.push(member);
                            class.extend(chars.next());
                        }
                        other => class.push(other),
                    }
                }
                match class.strip_prefix('^') {
                    // Not these members, and not "/".
                    Some(members) => {
                        out.push_str("[^/");
                        out.push_str(members);
                        out.push(']');
                    }
                    // These members, except "/" when the set spans it.
                    None => {
                        out.push_str("[[");
                        out.push_str(&class);
                        out.push_str("]--[/]]");
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A compiled glob with the semantics described in the [module docs](self).
#[derive(Debug, Clone)]
pub struct Glob {
    pattern: String,
    regex: Regex,
}

impl Glob {
    /// Compiles `pattern`; an empty or malformed pattern is a usage error.
    pub fn new(pattern: &str) -> Result<Self> {
        if pattern.is_empty() {
            return Err(usage("empty glob pattern"));
        }
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .case_insensitive(false)
            .build()
            .map_err(|err| usage(err.to_string()))?;
        // The regex globset derives from the pattern is matched directly against the
        // bytes of the path. Going through `globset::Candidate` instead would rewrite
        // `\` to `/` on Windows and make filters behave differently per OS.
        let regex = Regex::builder()
            .syntax(syntax::Config::new().utf8(false).dot_matches_new_line(true))
            .configure(
                Regex::config()
                    .utf8_empty(false)
                    .nfa_size_limit(Some(GLOB_SIZE_LIMIT))
                    .hybrid_cache_capacity(GLOB_SIZE_LIMIT),
            )
            .build(&exclude_separator_from_classes(glob.regex()))
            .map_err(|err| usage(format!("glob '{pattern}' cannot be compiled: {err}")))?;
        Ok(Self {
            pattern: pattern.to_owned(),
            regex,
        })
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Whether the whole of `path` matches the pattern.
    pub fn is_match(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

/// A compiled [`FilterSpec`].
#[derive(Debug, Clone, Default)]
pub struct Filter {
    prefix: Option<String>,
    glob: Option<Glob>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    modified_after: Option<DateTime<Utc>>,
    modified_before: Option<DateTime<Utc>>,
    /// Upper-cased, trimmed, never empty.
    storage_classes: Vec<String>,
}

impl Filter {
    /// Validates and compiles `spec`.
    pub fn compile(spec: &FilterSpec) -> Result<Self> {
        let prefix = spec.prefix.clone().filter(|prefix| !prefix.is_empty());
        let glob = match spec.glob.as_deref() {
            None | Some("") => None,
            Some(pattern) => Some(Glob::new(pattern)?),
        };
        if let (Some(min), Some(max)) = (spec.min_size, spec.max_size)
            && min > max
        {
            return Err(usage(format!(
                "min_size ({min}) is larger than max_size ({max})"
            )));
        }
        if let (Some(after), Some(before)) = (spec.modified_after, spec.modified_before)
            && after >= before
        {
            return Err(usage(
                "modified_after must be earlier than modified_before (the interval is empty)",
            ));
        }
        let mut storage_classes = Vec::with_capacity(spec.storage_classes.len());
        for class in &spec.storage_classes {
            let class = class.trim();
            if class.is_empty() {
                return Err(usage("storage_classes contains an empty name"));
            }
            storage_classes.push(class.to_ascii_uppercase());
        }
        Ok(Self {
            prefix,
            glob,
            min_size: spec.min_size,
            max_size: spec.max_size,
            modified_after: spec.modified_after,
            modified_before: spec.modified_before,
            storage_classes,
        })
    }

    /// True when the filter accepts everything.
    pub fn is_unrestricted(&self) -> bool {
        !self.has_path_criteria() && !self.has_metadata_criteria()
    }

    /// True when the filter has a prefix or a glob.
    pub fn has_path_criteria(&self) -> bool {
        self.prefix.is_some() || self.glob.is_some()
    }

    /// True when the filter has size, date or storage-class criteria.
    pub fn has_metadata_criteria(&self) -> bool {
        self.min_size.is_some()
            || self.max_size.is_some()
            || self.modified_after.is_some()
            || self.modified_before.is_some()
            || !self.storage_classes.is_empty()
    }

    /// Prefix and glob criteria only, on a relative path.
    pub fn matches_path(&self, rel_path: &str) -> bool {
        self.prefix
            .as_deref()
            .is_none_or(|prefix| rel_path.starts_with(prefix))
            && self
                .glob
                .as_ref()
                .is_none_or(|glob| glob.is_match(rel_path))
    }

    /// Whether anything below the folder `dir_rel_path` (relative, without a trailing
    /// "/"; empty for the root) can satisfy the prefix criterion. Globs are not used for
    /// pruning, so with only a glob this is always true.
    pub fn may_match_below(&self, dir_rel_path: &str) -> bool {
        let Some(prefix) = self.prefix.as_deref() else {
            return true;
        };
        if dir_rel_path.is_empty() {
            return true;
        }
        let mut dir = String::with_capacity(dir_rel_path.len() + 1);
        dir.push_str(dir_rel_path);
        dir.push('/');
        prefix.starts_with(&dir) || dir.starts_with(prefix)
    }

    /// Size, date and storage-class criteria only. `storage_class: None` means
    /// [`DEFAULT_STORAGE_CLASS`].
    pub fn matches_metadata(
        &self,
        size: u64,
        modified: Option<DateTime<Utc>>,
        storage_class: Option<&str>,
    ) -> bool {
        if self.min_size.is_some_and(|min| size < min) {
            return false;
        }
        if self.max_size.is_some_and(|max| size > max) {
            return false;
        }
        if self.modified_after.is_some() || self.modified_before.is_some() {
            let Some(modified) = modified else {
                return false;
            };
            if self.modified_after.is_some_and(|after| modified < after) {
                return false;
            }
            if self
                .modified_before
                .is_some_and(|before| modified >= before)
            {
                return false;
            }
        }
        if !self.storage_classes.is_empty() {
            let class = storage_class
                .map(str::trim)
                .filter(|class| !class.is_empty())
                .unwrap_or(DEFAULT_STORAGE_CLASS);
            if !self
                .storage_classes
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(class))
            {
                return false;
            }
        }
        true
    }

    /// Metadata criteria on a listing entry.
    pub fn matches_object_metadata(&self, entry: &ObjectEntry) -> bool {
        self.matches_metadata(
            entry.size,
            entry.last_modified,
            entry.storage_class.as_deref(),
        )
    }

    /// Metadata criteria on a local file (storage class [`DEFAULT_STORAGE_CLASS`]).
    pub fn matches_local_metadata(&self, meta: &FileMeta) -> bool {
        self.matches_metadata(meta.size, meta.modified, None)
    }

    /// Evaluates every criterion on a listing entry whose key is relative to
    /// `listing_prefix`. Entries outside `listing_prefix` never match. Folder entries
    /// (`is_prefix`, or a key ending in "/") have no size, date or class of their own:
    /// they match when something below them can satisfy the prefix criterion (see
    /// [`Filter::may_match_below`]), so a filtered folder view stays navigable.
    pub fn matches_object(&self, entry: &ObjectEntry, listing_prefix: &str) -> bool {
        let Some(rel_path) = entry.key.strip_prefix(listing_prefix) else {
            return false;
        };
        if entry.is_prefix || rel_path.ends_with('/') {
            return self.may_match_below(rel_path.trim_end_matches('/'));
        }
        self.matches_path(rel_path) && self.matches_object_metadata(entry)
    }

    /// Evaluates every criterion on a local file.
    pub fn matches_local(&self, rel_path: &str, meta: &FileMeta) -> bool {
        self.matches_path(rel_path) && self.matches_local_metadata(meta)
    }
}

/// Kind of one `--include` / `--exclude` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// `--include`: paths matching the glob are included.
    Include,
    /// `--exclude`: paths matching the glob are excluded.
    Exclude,
}

/// Ordered include/exclude glob rules for the CLI.
///
/// Everything is included by default; the rules are applied in order and the last rule
/// whose glob matches decides. So `--exclude '*' --include '*.jpg'` keeps only `.jpg`
/// files at the top level, while `--include '*.jpg' --exclude '*'` keeps nothing. Globs
/// use the syntax described in the [module docs](self) and are matched against the
/// path relative to the source folder or prefix.
#[derive(Debug, Clone, Default)]
pub struct IncludeExclude {
    rules: Vec<(RuleKind, Glob)>,
}

impl IncludeExclude {
    /// Builds the rules in the given order. A malformed or empty glob is a usage error.
    pub fn new<I, S>(rules: I) -> Result<Self>
    where
        I: IntoIterator<Item = (RuleKind, S)>,
        S: AsRef<str>,
    {
        let rules = rules
            .into_iter()
            .map(|(kind, pattern)| Glob::new(pattern.as_ref()).map(|glob| (kind, glob)))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { rules })
    }

    /// Builds the rules from two option lists that carry their command-line positions
    /// (for example clap's `ArgMatches::indices_of`), restoring the order the user typed.
    pub fn from_positions<I, E, S>(includes: I, excludes: E) -> Result<Self>
    where
        I: IntoIterator<Item = (usize, S)>,
        E: IntoIterator<Item = (usize, S)>,
        S: AsRef<str>,
    {
        let mut positioned: Vec<(usize, RuleKind, S)> = includes
            .into_iter()
            .map(|(position, pattern)| (position, RuleKind::Include, pattern))
            .chain(
                excludes
                    .into_iter()
                    .map(|(position, pattern)| (position, RuleKind::Exclude, pattern)),
            )
            .collect();
        positioned.sort_by_key(|(position, _, _)| *position);
        Self::new(
            positioned
                .into_iter()
                .map(|(_, kind, pattern)| (kind, pattern)),
        )
    }

    /// Number of rules.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when there are no rules (everything is included).
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether `rel_path` is included: the last matching rule decides, and a path no
    /// rule matches is included.
    pub fn is_included(&self, rel_path: &str) -> bool {
        self.rules
            .iter()
            .rev()
            .find(|(_, glob)| glob.is_match(rel_path))
            .is_none_or(|(kind, _)| *kind == RuleKind::Include)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::error::ErrorKind;
    use crate::model::Checksums;

    fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    fn object(
        key: &str,
        size: u64,
        modified: Option<DateTime<Utc>>,
        class: Option<&str>,
    ) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: None,
            last_modified: modified,
            storage_class: class.map(str::to_owned),
            checksums: Checksums::default(),
            is_prefix: false,
        }
    }

    fn glob_filter(glob: &str) -> Filter {
        Filter::compile(&FilterSpec {
            glob: Some(glob.to_owned()),
            ..FilterSpec::default()
        })
        .unwrap()
    }

    #[test]
    fn star_does_not_cross_separator_but_double_star_does() {
        let top = glob_filter("*.jpg");
        assert!(top.matches_path("a.jpg"));
        assert!(top.matches_path(".jpg"));
        assert!(!top.matches_path("2024/a.jpg"));
        assert!(!top.matches_path("a.jpeg"));

        let deep = glob_filter("**/*.jpg");
        assert!(deep.matches_path("a.jpg"));
        assert!(deep.matches_path("2024/a.jpg"));
        assert!(deep.matches_path("2024/06/a.jpg"));
        assert!(!deep.matches_path("2024/06/a.png"));

        let below = glob_filter("raw/**");
        assert!(below.matches_path("raw/a"));
        assert!(below.matches_path("raw/a/b/c"));
        assert!(!below.matches_path("raw"));
        assert!(!below.matches_path("rawx/a"));

        let middle = glob_filter("a/**/z.txt");
        assert!(middle.matches_path("a/z.txt"));
        assert!(middle.matches_path("a/b/c/z.txt"));
        assert!(!middle.matches_path("b/z.txt"));

        let everything = glob_filter("**");
        assert!(everything.matches_path("x"));
        assert!(everything.matches_path("x/y/z"));
    }

    #[test]
    fn question_mark_classes_alternates_and_escapes() {
        let one = glob_filter("a?c");
        assert!(one.matches_path("abc"));
        assert!(!one.matches_path("a/c"));
        assert!(!one.matches_path("ac"));

        let class = glob_filter("[ab].txt");
        assert!(class.matches_path("a.txt"));
        assert!(!class.matches_path("c.txt"));
        let negated = glob_filter("[!ab].txt");
        assert!(negated.matches_path("c.txt"));
        assert!(!negated.matches_path("a.txt"));

        let alternates = glob_filter("*.{jpg,png}");
        assert!(alternates.matches_path("a.jpg"));
        assert!(alternates.matches_path("a.png"));
        assert!(!alternates.matches_path("a.gif"));

        let escaped = glob_filter(r"\*.txt");
        assert!(escaped.matches_path("*.txt"));
        assert!(!escaped.matches_path("a.txt"));
    }

    #[test]
    fn classes_never_match_the_separator() {
        // A negated class used to cross "/" (globset lets it).
        let negated = glob_filter("a[!x]b");
        assert!(negated.matches_path("acb"));
        assert!(!negated.matches_path("axb"));
        assert!(!negated.matches_path("a/b"));
        let caret = glob_filter("a[^x]b");
        assert!(caret.matches_path("acb"));
        assert!(!caret.matches_path("a/b"));
        // A range that spans "/" (0x2B to 0x30) does not match it either.
        let range = glob_filter("a[+-0]b");
        assert!(range.matches_path("a+b"));
        assert!(range.matches_path("a0b"));
        assert!(!range.matches_path("a/b"));
        assert!(!glob_filter("a[/]b").matches_path("a/b"));
        // Escaped brackets and classes next to other tokens keep working.
        assert!(glob_filter(r"\[a\].txt").matches_path("[a].txt"));
        assert!(glob_filter("[]x].txt").matches_path("].txt"));
        assert!(glob_filter("**/[!_]*.txt").matches_path("d/e/a.txt"));
        assert!(!glob_filter("**/[!_]*.txt").matches_path("d/_a.txt"));
        assert!(glob_filter("d[!a-c]/f").matches_path("dz/f"));
        assert!(!glob_filter("d[!a-c]/f").matches_path("d/f"));
        // A non-ASCII member is matched byte-wise, as documented for classes.
        assert!(glob_filter("[!\u{e9}]x").matches_path("zx"));
    }

    #[test]
    fn globs_are_case_sensitive_and_unicode_safe() {
        let upper = glob_filter("*.JPG");
        assert!(upper.matches_path("A.JPG"));
        assert!(!upper.matches_path("a.jpg"));

        let star = glob_filter("caf*.txt");
        assert!(star.matches_path("caf\u{e9}.txt"));
        // `?` is one byte, so it does not match the two-byte "e with acute accent".
        let question = glob_filter("caf?.txt");
        assert!(!question.matches_path("caf\u{e9}.txt"));
        assert!(question.matches_path("cafe.txt"));

        let newline = glob_filter("*");
        assert!(newline.matches_path("line\nbreak"));
    }

    #[test]
    fn backslash_in_key_is_never_a_separator() {
        let filter = glob_filter("dir/*");
        assert!(filter.matches_path("dir/a.txt"));
        assert!(!filter.matches_path("dir\\a.txt"));
        let star = glob_filter("*");
        assert!(star.matches_path("dir\\a.txt"));
        let literal = glob_filter(r"dir\\a.txt");
        assert!(literal.matches_path("dir\\a.txt"));
    }

    #[test]
    fn invalid_globs_are_usage_errors() {
        for bad in ["[a", "a{b", "a\\"] {
            let err = Filter::compile(&FilterSpec {
                glob: Some(bad.to_owned()),
                ..FilterSpec::default()
            })
            .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{bad}");
        }
        assert_eq!(Glob::new("").unwrap_err().kind(), ErrorKind::Usage);
    }

    #[test]
    fn empty_strings_count_as_unset() {
        let filter = Filter::compile(&FilterSpec {
            prefix: Some(String::new()),
            glob: Some(String::new()),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(filter.is_unrestricted());
        assert!(filter.matches_path("anything/at/all"));
    }

    #[test]
    fn prefix_is_a_plain_string_prefix() {
        let filter = Filter::compile(&FilterSpec {
            prefix: Some("20".to_owned()),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(filter.matches_path("2024/a.jpg"));
        assert!(filter.matches_path("2025"));
        assert!(!filter.matches_path("1999/a.jpg"));
        assert!(!filter.matches_path("x/2024/a.jpg"));
        assert!(filter.has_path_criteria());
        assert!(!filter.has_metadata_criteria());
    }

    #[test]
    fn may_match_below_prunes_by_prefix_only() {
        let filter = Filter::compile(&FilterSpec {
            prefix: Some("photos/2024/".to_owned()),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(filter.may_match_below(""));
        assert!(filter.may_match_below("photos"));
        assert!(filter.may_match_below("photos/2024"));
        assert!(filter.may_match_below("photos/2024/06"));
        assert!(!filter.may_match_below("photos/2023"));
        assert!(!filter.may_match_below("docs"));
        assert!(!filter.may_match_below("photos/20"));

        let partial = Filter::compile(&FilterSpec {
            prefix: Some("pho".to_owned()),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(partial.may_match_below("photos"));
        assert!(!partial.may_match_below("docs"));

        assert!(glob_filter("*.jpg").may_match_below("anything"));
    }

    #[test]
    fn size_bounds_are_inclusive() {
        let filter = Filter::compile(&FilterSpec {
            min_size: Some(10),
            max_size: Some(20),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(!filter.matches_metadata(9, None, None));
        assert!(filter.matches_metadata(10, None, None));
        assert!(filter.matches_metadata(20, None, None));
        assert!(!filter.matches_metadata(21, None, None));

        let exact = Filter::compile(&FilterSpec {
            min_size: Some(5),
            max_size: Some(5),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(exact.matches_metadata(5, None, None));

        let err = Filter::compile(&FilterSpec {
            min_size: Some(21),
            max_size: Some(20),
            ..FilterSpec::default()
        })
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn date_bounds_are_after_inclusive_before_exclusive() {
        let filter = Filter::compile(&FilterSpec {
            modified_after: Some(ts(2024, 1, 1)),
            modified_before: Some(ts(2025, 1, 1)),
            ..FilterSpec::default()
        })
        .unwrap();
        let just_before = ts(2024, 1, 1) - chrono::TimeDelta::nanoseconds(1);
        assert!(!filter.matches_metadata(0, Some(just_before), None));
        assert!(filter.matches_metadata(0, Some(ts(2024, 1, 1)), None));
        assert!(filter.matches_metadata(0, Some(ts(2024, 12, 31)), None));
        assert!(!filter.matches_metadata(0, Some(ts(2025, 1, 1)), None));
        // Unknown modification time never satisfies a date bound.
        assert!(!filter.matches_metadata(0, None, None));

        let only_after = Filter::compile(&FilterSpec {
            modified_after: Some(ts(2024, 1, 1)),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(only_after.matches_metadata(0, Some(ts(2030, 1, 1)), None));
        assert!(!only_after.matches_metadata(0, None, None));

        for (after, before) in [
            (ts(2025, 1, 1), ts(2024, 1, 1)),
            (ts(2024, 1, 1), ts(2024, 1, 1)),
        ] {
            let err = Filter::compile(&FilterSpec {
                modified_after: Some(after),
                modified_before: Some(before),
                ..FilterSpec::default()
            })
            .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage);
        }
    }

    #[test]
    fn storage_classes_default_to_standard_and_ignore_case() {
        let glacier = Filter::compile(&FilterSpec {
            storage_classes: vec![" glacier ".to_owned(), "DEEP_ARCHIVE".to_owned()],
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(glacier.matches_metadata(0, None, Some("GLACIER")));
        assert!(glacier.matches_metadata(0, None, Some("deep_archive")));
        assert!(!glacier.matches_metadata(0, None, Some("STANDARD")));
        assert!(!glacier.matches_metadata(0, None, None));
        assert!(!glacier.matches_local_metadata(&FileMeta {
            size: 1,
            modified: None
        }));

        let standard = Filter::compile(&FilterSpec {
            storage_classes: vec!["standard".to_owned()],
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(standard.matches_metadata(0, None, None));
        assert!(standard.matches_metadata(0, None, Some("")));
        assert!(standard.matches_local_metadata(&FileMeta {
            size: 1,
            modified: None
        }));

        let any = Filter::compile(&FilterSpec::default()).unwrap();
        assert!(any.matches_metadata(0, None, Some("ANYTHING")));

        let err = Filter::compile(&FilterSpec {
            storage_classes: vec!["  ".to_owned()],
            ..FilterSpec::default()
        })
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn objects_are_matched_relative_to_the_listing_prefix() {
        let filter = Filter::compile(&FilterSpec {
            prefix: Some("2024/".to_owned()),
            glob: Some("**/*.jpg".to_owned()),
            min_size: Some(1),
            ..FilterSpec::default()
        })
        .unwrap();
        assert!(filter.matches_object(&object("photos/2024/a.jpg", 5, None, None), "photos/"));
        assert!(!filter.matches_object(&object("photos/2024/a.jpg", 0, None, None), "photos/"));
        assert!(!filter.matches_object(&object("photos/2023/a.jpg", 5, None, None), "photos/"));
        assert!(!filter.matches_object(&object("other/2024/a.jpg", 5, None, None), "photos/"));
        // Without a listing prefix the full key is relative.
        assert!(!filter.matches_object(&object("photos/2024/a.jpg", 5, None, None), ""));
        assert!(filter.matches_object(&object("2024/a.jpg", 5, None, None), ""));
    }

    #[test]
    fn folder_entries_only_use_the_prefix_criterion() {
        let filter = Filter::compile(&FilterSpec {
            prefix: Some("2024/".to_owned()),
            glob: Some("*.jpg".to_owned()),
            modified_after: Some(ts(2024, 1, 1)),
            ..FilterSpec::default()
        })
        .unwrap();
        let mut folder = object("photos/2024/", 0, None, None);
        folder.is_prefix = true;
        assert!(filter.matches_object(&folder, "photos/"));
        let mut other = object("photos/2023/", 0, None, None);
        other.is_prefix = true;
        assert!(!filter.matches_object(&other, "photos/"));
        // A zero-byte folder marker is a folder as well.
        assert!(filter.matches_object(&object("photos/2024/06/", 0, None, None), "photos/"));
    }

    #[test]
    fn local_files_use_path_and_metadata() {
        let filter = Filter::compile(&FilterSpec {
            glob: Some("*.txt".to_owned()),
            max_size: Some(100),
            ..FilterSpec::default()
        })
        .unwrap();
        let small = FileMeta {
            size: 100,
            modified: Some(ts(2024, 1, 1)),
        };
        let big = FileMeta {
            size: 101,
            modified: Some(ts(2024, 1, 1)),
        };
        assert!(filter.matches_local("a.txt", &small));
        assert!(!filter.matches_local("a.txt", &big));
        assert!(!filter.matches_local("a.md", &small));
    }

    #[test]
    fn include_exclude_defaults_to_included() {
        let rules = IncludeExclude::default();
        assert!(rules.is_empty());
        assert!(rules.is_included("anything"));
    }

    #[test]
    fn later_rules_take_precedence() {
        let only_jpg =
            IncludeExclude::new([(RuleKind::Exclude, "*"), (RuleKind::Include, "*.jpg")]).unwrap();
        assert!(only_jpg.is_included("a.jpg"));
        assert!(!only_jpg.is_included("a.png"));
        // `*` does not cross "/", so nested files are matched by neither rule.
        assert!(only_jpg.is_included("dir/a.png"));

        let nothing =
            IncludeExclude::new([(RuleKind::Include, "*.jpg"), (RuleKind::Exclude, "*")]).unwrap();
        assert!(!nothing.is_included("a.jpg"));
        assert!(!nothing.is_included("a.png"));

        let layered = IncludeExclude::new([
            (RuleKind::Exclude, "**/*.tmp"),
            (RuleKind::Include, "keep/**"),
            (RuleKind::Exclude, "keep/secret/**"),
        ])
        .unwrap();
        assert!(!layered.is_included("a.tmp"));
        assert!(!layered.is_included("x/a.tmp"));
        assert!(layered.is_included("keep/a.tmp"));
        assert!(!layered.is_included("keep/secret/a.txt"));
        assert!(layered.is_included("other/a.txt"));
        assert_eq!(layered.len(), 3);
    }

    #[test]
    fn rules_from_positions_restore_command_line_order() {
        // bb cp ... --exclude '*' (position 2) --include '*.jpg' (position 4)
        let rules = IncludeExclude::from_positions([(4, "*.jpg")], [(2, "*")]).unwrap();
        assert!(rules.is_included("a.jpg"));
        assert!(!rules.is_included("a.png"));
        // ... --include '*.jpg' (position 2) --exclude '*' (position 4)
        let rules = IncludeExclude::from_positions([(2, "*.jpg")], [(4, "*")]).unwrap();
        assert!(!rules.is_included("a.jpg"));
    }

    #[test]
    fn invalid_rules_are_usage_errors() {
        let err = IncludeExclude::new([(RuleKind::Include, "[")]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        let err = IncludeExclude::new([(RuleKind::Exclude, "")]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
}
