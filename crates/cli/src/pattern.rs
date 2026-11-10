//! Wildcards in the last segment of an S3 URI or a local path:
//! `s3://bucket/logs/*.gz`, `./photos/*.jpg`.
//!
//! The pattern uses the glob syntax of `--include` and `--exclude` (see
//! `docs/cli.md`): `*` and `?` never cross a `/`, `[abc]` and `{a,b}` work,
//! and a backslash escapes the next character. Only the last segment may
//! contain wildcards; the folder in front of it is literal. A pattern that
//! matches nothing is a `not_found` error, never a silent success.

use std::path::{Path, PathBuf};

use bucketbridge_core::filter::Glob;
use bucketbridge_core::model::ObjectEntry;
use bucketbridge_core::{Error, Result, S3, S3Uri};

use crate::i18n::trf;
use crate::paging::SeenTokens;

/// Keys requested per listing page.
const PAGE_SIZE: u32 = 1000;

/// Whether `text` holds a wildcard that is not escaped with a backslash:
/// `*`, `?`, `[` or `{`.
pub fn has_magic(text: &str) -> bool {
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '*' | '?' | '[' | '{' => return true,
            _ => {}
        }
    }
    false
}

/// Whether the last segment of a path is a pattern: it has a wildcard, or a
/// backslash (which escapes the next character, so `a\*` names the object
/// `a*`; write `\\` for a backslash). On Windows a backslash separates
/// folders and is not an escape.
pub fn is_pattern(segment: &str) -> bool {
    has_magic(segment) || (cfg!(not(windows)) && segment.contains('\\'))
}

/// The wildcard of an S3 URI: the folder in front of it and the compiled
/// pattern for the names in that folder.
#[derive(Debug, Clone)]
pub struct S3Pattern {
    /// The literal folder (empty, or ending with `/`).
    pub dir: String,
    /// The last segment as written.
    pub name: String,
    glob: Glob,
    bucket: String,
}

impl S3Pattern {
    /// The URI of the folder in front of the wildcard.
    pub fn dir_uri(&self) -> Result<S3Uri> {
        S3Uri::new(&self.bucket, &self.dir)
    }

    /// Whether a name directly in the folder matches.
    pub fn is_match(&self, name: &str) -> bool {
        self.glob.is_match(name)
    }

    /// Whether `key` (a full key in the pattern's bucket) is directly in the
    /// folder with a matching name.
    pub fn matches_key(&self, key: &str) -> bool {
        key.strip_prefix(self.dir.as_str())
            .is_some_and(|name| self.is_match(name))
    }

    /// The pattern as a URI, for messages.
    pub fn display(&self) -> String {
        format!("s3://{}/{}{}", self.bucket, self.dir, self.name)
    }
}

/// The wildcard in `uri`, when its last segment has one. A wildcard in the
/// bucket or in a folder in front of the last segment is a usage error.
pub fn s3_pattern(uri: &S3Uri) -> Result<Option<S3Pattern>> {
    let key = uri.key();
    let (dir, name) = key
        .rsplit_once('/')
        .map_or(("", key), |(dir, name)| (&key[..=dir.len()], name));
    if has_magic(dir) {
        return Err(only_last_segment(&uri.to_string()));
    }
    if !is_pattern(name) {
        return Ok(None);
    }
    Ok(Some(S3Pattern {
        dir: dir.to_owned(),
        name: name.to_owned(),
        glob: compile(name)?,
        bucket: uri.bucket().to_owned(),
    }))
}

fn compile(name: &str) -> Result<Glob> {
    Glob::new(name).map_err(|err| {
        Error::Usage(trf(
            "invalid pattern '{pattern}': {error}",
            &[("pattern", name), ("error", &err.to_string())],
        ))
    })
}

fn only_last_segment(what: &str) -> Error {
    Error::Usage(trf(
        "{what}: wildcards work only in the last part of a path, after the last /",
        &[("what", what)],
    ))
}

/// The objects (and, with `folders`, the folders) directly in the pattern's
/// folder whose names match, sorted by key. No match is `not_found`.
pub async fn list_matches(s3: &S3, pattern: &S3Pattern, folders: bool) -> Result<Vec<ObjectEntry>> {
    let mut found: Vec<ObjectEntry> = Vec::new();
    let what = trf("listing {uri}", &[("uri", &pattern.display())]);
    let mut seen = SeenTokens::default();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .list_objects(
                &pattern.bucket,
                &pattern.dir,
                false,
                token.as_deref(),
                PAGE_SIZE,
            )
            .await?;
        for entry in page.entries {
            let name = entry
                .key
                .strip_prefix(pattern.dir.as_str())
                .unwrap_or(&entry.key);
            let name = name.strip_suffix('/').unwrap_or(name);
            if entry.is_prefix {
                if !folders {
                    continue;
                }
            } else if name.is_empty() {
                continue;
            }
            if pattern.is_match(name) {
                found.push(entry);
            }
        }
        match page.next_token {
            Some(next) => token = Some(seen.follow(next, &what)?),
            None => break,
        }
    }
    if found.is_empty() {
        return Err(no_match(
            if folders {
                "no objects or folders match {pattern}"
            } else {
                "no objects match {pattern}"
            },
            &pattern.display(),
        ));
    }
    found.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(found)
}

fn no_match(template: &str, pattern: &str) -> Error {
    Error::NotFound(trf(template, &[("pattern", pattern)]))
}

/// The files that match a local pattern, sorted. `None` when the last
/// component has no wildcard, or when a file of exactly that name exists
/// (a file called `a[1].txt` is that file). Directories never match: a
/// folder needs `-r`. No match is `not_found`.
pub fn local_matches(path: &Path) -> Result<Option<Vec<PathBuf>>> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !is_pattern(name) || path.exists() {
        return Ok(None);
    }
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = dir
        && has_magic(&dir.to_string_lossy())
    {
        return Err(only_last_segment(&path.display().to_string()));
    }
    let glob = compile(name)?;
    let dir = dir.unwrap_or(Path::new("."));
    let entries = std::fs::read_dir(dir).map_err(|err| {
        Error::io(
            trf("reading '{path}'", &[("path", &dir.display().to_string())]),
            err,
        )
    })?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::io(
                trf("reading '{path}'", &[("path", &dir.display().to_string())]),
                err,
            )
        })?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        // A pattern does not match hidden files unless it names the dot.
        if file_name.starts_with('.') && !name.starts_with('.') {
            continue;
        }
        if glob.is_match(file_name) && entry.path().is_file() {
            files.push(path.with_file_name(file_name));
        }
    }
    if files.is_empty() {
        return Err(no_match(
            "no files match {pattern}",
            &path.display().to_string(),
        ));
    }
    files.sort();
    Ok(Some(files))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(text: &str) -> S3Uri {
        S3Uri::parse(text).unwrap()
    }

    #[test]
    fn wildcards_are_found_unless_escaped() {
        for yes in ["*.gz", "a?", "[ab]", "{a,b}", "x\\\\*", "dir/*"] {
            assert!(has_magic(yes), "{yes}");
        }
        for no in ["plain.txt", "a\\*b", "dir/sub/", "", "a-b_c.d"] {
            assert!(!has_magic(no), "{no}");
        }
    }

    #[test]
    fn s3_patterns_split_the_last_segment() {
        let pattern = s3_pattern(&uri("s3://b/logs/2024/*.gz")).unwrap().unwrap();
        assert_eq!(pattern.dir, "logs/2024/");
        assert_eq!(pattern.name, "*.gz");
        assert!(pattern.is_match("a.gz"));
        assert!(!pattern.is_match("a.txt"));
        // `*` does not cross a folder.
        assert!(!pattern.is_match("sub/a.gz"));
        assert_eq!(pattern.dir_uri().unwrap(), uri("s3://b/logs/2024/"));
        assert_eq!(pattern.display(), "s3://b/logs/2024/*.gz");

        let top = s3_pattern(&uri("s3://b/*.gz")).unwrap().unwrap();
        assert_eq!(top.dir, "");
        assert!(top.is_match("x.gz"));

        let braces = s3_pattern(&uri("s3://b/p/*.{jpg,png}")).unwrap().unwrap();
        assert!(braces.is_match("a.png") && !braces.is_match("a.gif"));
        let class = s3_pattern(&uri("s3://b/p/img[0-9].jpg")).unwrap().unwrap();
        assert!(class.is_match("img7.jpg") && !class.is_match("imgx.jpg"));
        let one = s3_pattern(&uri("s3://b/p/a?.txt")).unwrap().unwrap();
        assert!(one.is_match("ab.txt") && !one.is_match("abc.txt"));
    }

    #[test]
    fn plain_and_escaped_keys_are_not_patterns() {
        assert!(s3_pattern(&uri("s3://b/logs/a.gz")).unwrap().is_none());
        assert!(s3_pattern(&uri("s3://b/logs/")).unwrap().is_none());
        assert!(s3_pattern(&uri("s3://b")).unwrap().is_none());
        // A backslash escapes: the pattern `a\*.gz` matches the key `a*.gz`.
        let escaped = s3_pattern(&uri("s3://b/logs/a\\*.gz")).unwrap().unwrap();
        assert!(escaped.is_match("a*.gz") && !escaped.is_match("ab.gz"));
    }

    #[test]
    fn a_wildcard_before_the_last_segment_is_a_usage_error() {
        for bad in ["s3://b/*/a.gz", "s3://b/logs/*/", "s3://b/l*/x/a.gz"] {
            let err = s3_pattern(&uri(bad)).unwrap_err();
            assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Usage, "{bad}");
        }
    }

    #[test]
    fn a_malformed_pattern_is_a_usage_error() {
        let err = s3_pattern(&uri("s3://b/logs/[a")).unwrap_err();
        assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Usage);
    }

    #[test]
    fn local_patterns_match_files_in_one_folder() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.jpg", "a.jpg", "c.png", ".hidden.jpg"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub.jpg")).unwrap();
        std::fs::write(dir.path().join("sub.jpg").join("deep.jpg"), "x").unwrap();
        let found = local_matches(&dir.path().join("*.jpg")).unwrap().unwrap();
        let names: Vec<&str> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        // Sorted; no folder, no file below a folder, no hidden file.
        assert_eq!(names, ["a.jpg", "b.jpg"]);
        let dot = local_matches(&dir.path().join(".*.jpg")).unwrap().unwrap();
        assert_eq!(dot.len(), 1);
    }

    #[test]
    fn local_literals_and_misses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a[1].txt"), "x").unwrap();
        // The name exists as written: a plain file, not a pattern.
        assert!(
            local_matches(&dir.path().join("a[1].txt"))
                .unwrap()
                .is_none()
        );
        // No wildcard at all.
        assert!(
            local_matches(&dir.path().join("plain.txt"))
                .unwrap()
                .is_none()
        );
        // Nothing matches: not_found.
        let err = local_matches(&dir.path().join("*.zip")).unwrap_err();
        assert_eq!(err.kind(), bucketbridge_core::ErrorKind::NotFound);
        // A wildcard in the folder.
        let err = local_matches(&dir.path().join("*").join("*.txt")).unwrap_err();
        assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Usage);
        // A folder that does not exist is an io error.
        let err = local_matches(&dir.path().join("missing").join("*.txt")).unwrap_err();
        assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Io);
    }
}
