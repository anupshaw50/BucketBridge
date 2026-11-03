//! Gitignore-style exclusion rules for sync (saved jobs, `--exclude`, `--exclude-from`).
//!
//! [`IgnoreRules`] parses rule lines the way `.gitignore` files are written:
//!
//! - blank lines and lines starting with `#` are skipped (`\#` is a literal `#`),
//! - a leading `!` negates the rule: a path it matches is included again (`\!` is a
//!   literal `!`). The last matching rule decides,
//! - a trailing `/` makes the rule match folders only,
//! - a pattern with a `/` at the start or in the middle is anchored to the synced root
//!   (a leading `/` is dropped); a pattern without one matches at any depth,
//! - `*`, `?`, `[...]` and `**` follow the glob syntax of [`crate::filter`],
//! - trailing spaces are removed unless escaped with `\`.
//!
//! As in git, excluding a folder excludes everything below it, and a rule cannot
//! include a file again when one of its folders is excluded.
//!
//! Sync only sees files, so [`IgnoreRules::is_excluded`] takes a file path and checks
//! each of its folders (`a`, `a/b`) as a folder, then the file itself.

use crate::error::{Error, Result};
use crate::filter::Glob;

/// Largest rule file that is read (bytes).
pub const MAX_RULES_FILE_BYTES: u64 = 1024 * 1024;

/// Most rules accepted in one set.
pub const MAX_RULES: usize = 10_000;

#[derive(Debug, Clone)]
struct Rule {
    glob: Glob,
    negated: bool,
    folder_only: bool,
}

/// A compiled set of gitignore-style rules.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
}

impl IgnoreRules {
    /// Parses rule lines (see the [module docs](self)). A pattern that does not compile
    /// is a usage error that names the rule.
    pub fn parse<I, S>(lines: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut rules = Vec::new();
        for (number, line) in lines.into_iter().enumerate() {
            if let Some(rule) = parse_line(line.as_ref())
                .map_err(|err| Error::Usage(format!("exclusion rule {}: {err}", number + 1)))?
            {
                rules.push(rule);
                if rules.len() > MAX_RULES {
                    return Err(Error::Usage(format!(
                        "more than {MAX_RULES} exclusion rules"
                    )));
                }
            }
        }
        Ok(Self { rules })
    }

    /// Parses the text of a rules file.
    pub fn parse_text(text: &str) -> Result<Self> {
        Self::parse(text.lines())
    }

    /// Reads and parses a rules file (blocking). Larger than [`MAX_RULES_FILE_BYTES`] or
    /// not UTF-8 is a usage error; a missing file is `not_found`.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        Self::parse_text(&read_rules_text(path)?)
    }

    /// Number of rules (comments and blank lines do not count).
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// True when there are no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Adds the rules of `other` after this set's (so they win on conflicts).
    pub fn extend(&mut self, other: IgnoreRules) {
        self.rules.extend(other.rules);
    }

    /// Whether the file `rel_path` (`/`-separated, relative to the synced root) is
    /// excluded, directly or through one of its folders.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        for (end, _) in rel_path.match_indices('/') {
            if self.decide(&rel_path[..end], true) {
                return true;
            }
        }
        self.decide(rel_path, false)
    }

    /// Whether the folder `dir_rel_path` (no trailing `/`) is excluded, so that nothing
    /// below it needs to be visited.
    pub fn is_folder_excluded(&self, dir_rel_path: &str) -> bool {
        if self.rules.is_empty() || dir_rel_path.is_empty() {
            return false;
        }
        for (end, _) in dir_rel_path.match_indices('/') {
            if self.decide(&dir_rel_path[..end], true) {
                return true;
            }
        }
        self.decide(dir_rel_path, true)
    }

    /// The last matching rule decides; no match means "not excluded".
    fn decide(&self, path: &str, is_folder: bool) -> bool {
        self.rules
            .iter()
            .rev()
            .find(|rule| (is_folder || !rule.folder_only) && rule.glob.is_match(path))
            .is_some_and(|rule| !rule.negated)
    }
}

/// Reads the text of a rules file (blocking), without a byte order mark. The checks are
/// those of [`IgnoreRules::from_file`].
pub fn read_rules_text(path: &std::path::Path) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|err| file_error(path, "cannot read the exclusion file", &err))?;
    if !meta.is_file() {
        return Err(Error::Usage(format!(
            "the exclusion file '{}' is not a file",
            path.display()
        )));
    }
    if meta.len() > MAX_RULES_FILE_BYTES {
        return Err(Error::Usage(format!(
            "the exclusion file '{}' is larger than {MAX_RULES_FILE_BYTES} bytes",
            path.display()
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|err| file_error(path, "cannot read the exclusion file", &err))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::Usage(format!(
            "the exclusion file '{}' is not valid UTF-8",
            path.display()
        ))
    })?;
    Ok(text.trim_start_matches('\u{feff}').to_owned())
}

fn file_error(path: &std::path::Path, what: &str, err: &std::io::Error) -> Error {
    let message = format!("{what} '{}': {err}", path.display());
    if err.kind() == std::io::ErrorKind::NotFound {
        Error::NotFound(message)
    } else {
        Error::Usage(message)
    }
}

fn parse_line(line: &str) -> Result<Option<Rule>> {
    let line = line.trim_end_matches('\r');
    let mut text = line.to_owned();
    // Trailing spaces do not count unless escaped.
    while text.ends_with(' ') && !text.ends_with("\\ ") {
        text.pop();
    }
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }
    let (negated, text) = match text.strip_prefix('!') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, text),
    };
    // `\#` and `\!` at the start are literal characters.
    let text = if text.starts_with("\\#") || text.starts_with("\\!") {
        text[1..].to_owned()
    } else {
        text
    };
    let (text, folder_only) = match text.strip_suffix('/') {
        Some(rest) => (rest.to_owned(), true),
        None => (text, false),
    };
    let anchored = text.starts_with('/') || text.trim_end_matches("/**").contains('/');
    let body = text.trim_start_matches('/');
    if body.is_empty() {
        return Ok(None);
    }
    let pattern = if anchored || body.starts_with("**/") {
        body.to_owned()
    } else {
        format!("**/{body}")
    };
    let glob = Glob::new(&pattern).map_err(|err| match err {
        Error::Usage(message) => Error::Usage(format!("'{line}': {message}")),
        other => other,
    })?;
    Ok(Some(Rule {
        glob,
        negated,
        folder_only,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> IgnoreRules {
        IgnoreRules::parse_text(text).unwrap()
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let set = rules("# a comment\n\n   \n*.tmp\n");
        assert_eq!(set.len(), 1);
        assert!(set.is_excluded("a.tmp"));
        assert!(set.is_excluded("deep/er/a.tmp"));
        assert!(!set.is_excluded("a.txt"));
    }

    #[test]
    fn escaped_hash_and_bang_are_literal() {
        let set = rules("\\#notes\n\\!wow\n");
        assert!(set.is_excluded("#notes"));
        assert!(set.is_excluded("sub/!wow"));
        assert!(!set.is_excluded("notes"));
    }

    #[test]
    fn negation_includes_again_and_the_last_rule_wins() {
        let set = rules("*.log\n!keep.log\n");
        assert!(set.is_excluded("a.log"));
        assert!(!set.is_excluded("keep.log"));
        assert!(!set.is_excluded("sub/keep.log"));
        let reversed = rules("!keep.log\n*.log\n");
        assert!(reversed.is_excluded("keep.log"));
    }

    #[test]
    fn trailing_slash_matches_folders_only() {
        let set = rules("build/\n");
        assert!(set.is_excluded("build/out.bin"));
        assert!(set.is_excluded("x/build/deep/out.bin"));
        // A file named build is not a folder.
        assert!(!set.is_excluded("build"));
        assert!(!set.is_excluded("rebuild/out.bin"));
        assert!(set.is_folder_excluded("x/build"));
        assert!(!set.is_folder_excluded("x/src"));
    }

    #[test]
    fn anchored_and_unanchored_patterns() {
        let set = rules("/top.txt\ndocs/old.txt\nname.md\n");
        assert!(set.is_excluded("top.txt"));
        assert!(!set.is_excluded("sub/top.txt"));
        assert!(set.is_excluded("docs/old.txt"));
        assert!(!set.is_excluded("x/docs/old.txt"));
        assert!(set.is_excluded("a/b/name.md"));
        assert!(set.is_excluded("name.md"));
    }

    #[test]
    fn double_star_spans_folders() {
        let set = rules("logs/**/debug.log\n**/cache/**\n");
        assert!(set.is_excluded("logs/debug.log"));
        assert!(set.is_excluded("logs/a/b/debug.log"));
        assert!(!set.is_excluded("other/debug.log"));
        assert!(set.is_excluded("x/cache/y.bin"));
        assert!(set.is_excluded("cache/y.bin"));
    }

    #[test]
    fn a_file_cannot_be_included_again_below_an_excluded_folder() {
        let set = rules("vendor/\n!vendor/keep.txt\n");
        assert!(set.is_excluded("vendor/keep.txt"));
        let only_files = rules("vendor/*\n!vendor/keep.txt\n");
        assert!(!only_files.is_excluded("vendor/keep.txt"));
        assert!(only_files.is_excluded("vendor/other.txt"));
    }

    #[test]
    fn trailing_spaces_are_dropped_unless_escaped() {
        let set = rules("a.txt   \nb\\ \n");
        assert!(set.is_excluded("a.txt"));
        assert!(set.is_excluded("b "));
    }

    #[test]
    fn bad_patterns_name_the_rule() {
        let err = IgnoreRules::parse_text("ok\n[unclosed\n").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("rule 2"), "{message}");
    }

    #[test]
    fn rules_files_are_read_with_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules");
        std::fs::write(&path, "\u{feff}*.bak\r\n!keep.bak\r\n").unwrap();
        let set = IgnoreRules::from_file(&path).unwrap();
        assert!(set.is_excluded("x.bak"));
        assert!(!set.is_excluded("keep.bak"));
        let missing = IgnoreRules::from_file(&dir.path().join("none")).unwrap_err();
        assert_eq!(missing.kind(), crate::error::ErrorKind::NotFound);
        let folder = IgnoreRules::from_file(dir.path()).unwrap_err();
        assert_eq!(folder.kind(), crate::error::ErrorKind::Usage);
    }

    #[test]
    fn extend_lets_later_rules_win() {
        let mut first = rules("*.txt\n");
        first.extend(rules("!a.txt\n"));
        assert!(!first.is_excluded("a.txt"));
        assert!(first.is_excluded("b.txt"));
    }
}
