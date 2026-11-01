//! Path normaliser: mapping between local paths and S3 keys.
//!
//! * [`relative_path_to_key`] turns a relative local path into a `/`-separated
//!   key segment for uploads.
//! * [`join_key`] puts a key segment under a prefix.
//! * [`strip_parent`] implements the folder semantics of the transfer
//!   commands: the parent part of a selected key is removed, so the selected
//!   name lands directly under the destination (`photos/2024/` copied to
//!   `backup/` gives `backup/2024/...`).
//! * [`safe_local_path`] maps a relative key to a path inside a destination
//!   folder for downloads.
//!
//! # Security boundary
//!
//! Object keys come from the server and must be treated as untrusted input.
//! A key such as `../../.ssh/authorized_keys` must never make a download
//! write outside the folder the user picked. [`safe_local_path`] therefore
//! rejects, on every operating system: empty keys, NUL bytes, backslashes,
//! absolute keys (a leading `/`, which also covers `//server/share`), empty
//! segments (`a//b`), `.` and `..` segments, and any segment that starts
//! with a drive letter (an ASCII letter followed by `:`, as in `C:` or
//! `c:foo`; on Windows such a segment would replace the whole path when
//! joined). On Windows it additionally rejects characters that are not valid
//! in file names (`<>:"|?*` and control characters, so also NTFS alternate
//! data streams such as `file:stream`), segments ending in a dot or a space
//! (Windows silently strips those, so `".. "` would become `".."`), and
//! reserved device names (`CON`, `NUL`, `COM1`, `LPT1`, ...). The rules are
//! exposed through [`validate_relative_key`] with an explicit [`PathStyle`]
//! so the Windows rules are tested on every OS.
//!
//! What is out of scope: symbolic links that already exist inside the
//! destination folder are followed (they were created by the local user,
//! not by the server), and keys that differ only in case or Unicode
//! normalisation map to the same file on case-insensitive or
//! normalisation-insensitive file systems (macOS, Windows). Callers that
//! write many files should detect such collisions.
//!
//! # No Unicode normalisation
//!
//! Names are passed through byte for byte in both directions. macOS may
//! hand out decomposed (NFD) names while Linux and Windows usually use
//! composed (NFC) names; normalising would make a key differ from the name
//! the user sees, break round trips (a downloaded file would not upload to
//! the same key), and could merge two distinct keys into one file. Keeping
//! names verbatim is the only lossless choice.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// File-name rules of an operating system family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStyle {
    /// Linux, macOS and other Unix-like systems.
    Unix,
    /// Windows.
    Windows,
}

impl PathStyle {
    /// The style of the platform this binary was compiled for.
    pub const fn native() -> Self {
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Unix
        }
    }
}

/// Converts a relative local path into a `/`-separated key segment.
///
/// OS separators become `/` and `.` components are dropped. Absolute paths,
/// drive or UNC prefixes, `..` components and names that are not valid
/// UTF-8 are rejected, as is a path with no name at all (`""` or `"."`).
pub fn relative_path_to_key(rel: &Path) -> Result<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| {
                    Error::Usage(format!(
                        "the file name {:?} in '{}' is not valid UTF-8 and cannot be used as an S3 key",
                        name,
                        rel.display()
                    ))
                })?;
                parts.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(Error::Usage(format!(
                    "'{}' contains '..'; only paths inside the selected folder can be uploaded",
                    rel.display()
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Usage(format!(
                    "'{}' is an absolute path; expected a path relative to the selected folder",
                    rel.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(Error::Usage(format!(
            "'{}' does not name a file or folder",
            rel.display()
        )));
    }
    Ok(parts.join("/"))
}

/// Joins a key prefix and a relative key segment with exactly one `/` at the
/// join point added when needed. An empty prefix returns `rel` unchanged;
/// `rel` is not modified.
///
/// `join_key("backup/", "2024/a.jpg")` and `join_key("backup", "2024/a.jpg")`
/// both give `backup/2024/a.jpg`.
pub fn join_key(prefix: &str, rel: &str) -> String {
    if prefix.is_empty() {
        rel.to_owned()
    } else if prefix.ends_with('/') {
        format!("{prefix}{rel}")
    } else {
        format!("{prefix}/{rel}")
    }
}

/// The parent part of a selected key: everything up to and including the
/// last `/` before its final segment (a trailing `/` of a prefix does not
/// count). `photos/2024/` and `photos/2024/a.jpg` give `photos/` and
/// `photos/2024/`; a top-level key gives `""`.
pub fn parent_prefix(key: &str) -> &str {
    let trimmed = key.strip_suffix('/').unwrap_or(key);
    match trimmed.rfind('/') {
        Some(idx) => &key[..=idx],
        None => "",
    }
}

/// Removes the parent part of `selected` from `key`, where `key` is
/// `selected` itself or, when `selected` is a prefix (ends with `/`), a key
/// below it.
///
/// Selecting `photos/2024/` maps `photos/2024/b/c.jpg` to `2024/b/c.jpg`;
/// selecting `photos/2024/a.jpg` maps it to `a.jpg`.
pub fn strip_parent<'k>(selected: &str, key: &'k str) -> Result<&'k str> {
    let inside = if selected.ends_with('/') {
        key.starts_with(selected)
    } else {
        key == selected
    };
    if selected.is_empty() || !inside {
        return Err(Error::Usage(format!(
            "key '{key}' is not inside the selection '{selected}'"
        )));
    }
    Ok(&key[parent_prefix(selected).len()..])
}

/// Validates a key that is relative to a destination folder and returns its
/// segments, applying the rules of `style` (see the module documentation).
///
/// One trailing `/` is allowed and marks a folder; it does not produce a
/// segment.
pub fn validate_relative_key(rel_key: &str, style: PathStyle) -> Result<Vec<&str>> {
    let reject = |why: &str| {
        Err(Error::Usage(format!(
            "unsafe key '{}': {why}",
            rel_key.escape_debug()
        )))
    };
    if rel_key.is_empty() {
        return reject("the key is empty");
    }
    if rel_key.contains('\0') {
        return reject("it contains a NUL character");
    }
    if rel_key.contains('\\') {
        return reject("it contains a backslash");
    }
    if rel_key.starts_with('/') {
        return reject("it is absolute (starts with '/')");
    }
    let body = rel_key.strip_suffix('/').unwrap_or(rel_key);
    let mut segments = Vec::new();
    for segment in body.split('/') {
        if segment.is_empty() {
            return reject("it contains an empty path segment ('//')");
        }
        if segment == "." || segment == ".." {
            return reject(&format!("it contains a '{segment}' segment"));
        }
        let mut chars = segment.chars();
        if let (Some(first), Some(':')) = (chars.next(), chars.next())
            && first.is_ascii_alphabetic()
        {
            return reject("it contains a drive letter");
        }
        if style == PathStyle::Windows
            && let Err(why) = check_windows_segment(segment)
        {
            return reject(&why);
        }
        segments.push(segment);
    }
    Ok(segments)
}

fn check_windows_segment(segment: &str) -> std::result::Result<(), String> {
    if let Some(bad) = segment
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_ascii_control())
    {
        return Err(format!(
            "the character {bad:?} is not allowed in Windows file names"
        ));
    }
    if segment.ends_with('.') || segment.ends_with(' ') {
        return Err(format!(
            "the name '{segment}' ends with a dot or a space, which Windows removes"
        ));
    }
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches(' ');
    if is_reserved_device_name(stem) {
        return Err(format!("'{segment}' is a reserved device name on Windows"));
    }
    Ok(())
}

fn is_reserved_device_name(stem: &str) -> bool {
    const FIXED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if FIXED.iter().any(|name| stem.eq_ignore_ascii_case(name)) {
        return true;
    }
    let upper = stem.to_ascii_uppercase();
    for base in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(base) {
            let mut chars = rest.chars();
            if let (Some(digit), None) = (chars.next(), chars.next())
                && matches!(digit, '0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}')
            {
                return true;
            }
        }
    }
    false
}

/// Maps a key relative to the stripped prefix (see [`strip_parent`]) to a
/// path inside `dest_root`, or rejects it if it could escape `dest_root` or
/// cannot be represented faithfully on this OS.
///
/// A key ending in `/` (a folder) maps to the folder path.
pub fn safe_local_path(dest_root: &Path, rel_key: &str) -> Result<PathBuf> {
    let segments = validate_relative_key(rel_key, PathStyle::native())?;
    let mut path = dest_root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    // The segment rules make this impossible; keep the boundary explicit.
    if !path.starts_with(dest_root) || path == dest_root {
        return Err(Error::Usage(format!(
            "unsafe key '{}': it does not map to a path inside '{}'",
            rel_key.escape_debug(),
            dest_root.display()
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_become_keys() {
        assert_eq!(relative_path_to_key(Path::new("a.txt")).unwrap(), "a.txt");
        assert_eq!(
            relative_path_to_key(Path::new("photos/2024/a.jpg")).unwrap(),
            "photos/2024/a.jpg"
        );
        assert_eq!(
            relative_path_to_key(Path::new("./photos/./a.jpg")).unwrap(),
            "photos/a.jpg"
        );
        assert_eq!(
            relative_path_to_key(Path::new("photos//a.jpg/")).unwrap(),
            "photos/a.jpg"
        );
        assert_eq!(
            relative_path_to_key(Path::new("ünï/ファイル")).unwrap(),
            "ünï/ファイル"
        );
    }

    #[test]
    fn relative_path_rejections() {
        for (input, expected) in [
            ("", "does not name a file"),
            (".", "does not name a file"),
            ("./", "does not name a file"),
            ("../x", "contains '..'"),
            ("a/../b", "contains '..'"),
            ("/etc/passwd", "absolute path"),
        ] {
            let err = relative_path_to_key(Path::new(input)).unwrap_err();
            assert!(err.to_string().contains(expected), "{input}: {err}");
        }
    }

    #[test]
    fn no_unicode_normalisation() {
        let nfd = "e\u{301}.txt";
        let nfc = "\u{e9}.txt";
        assert_eq!(relative_path_to_key(Path::new(nfd)).unwrap(), nfd);
        assert_eq!(relative_path_to_key(Path::new(nfc)).unwrap(), nfc);
        assert_ne!(
            safe_local_path(Path::new("dest"), nfd).unwrap(),
            safe_local_path(Path::new("dest"), nfc).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_are_rejected() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let name = Path::new(OsStr::from_bytes(b"bad\xffname"));
        let err = relative_path_to_key(name).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn backslash_is_an_ordinary_character_in_unix_names() {
        assert_eq!(relative_path_to_key(Path::new("a\\b")).unwrap(), "a\\b");
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_become_keys() {
        assert_eq!(
            relative_path_to_key(Path::new(r"photos\2024\a.jpg")).unwrap(),
            "photos/2024/a.jpg"
        );
        for input in [r"C:\x", r"C:x", r"\\server\share\x", r"\x", r"..\x"] {
            assert!(relative_path_to_key(Path::new(input)).is_err(), "{input}");
        }
    }

    #[test]
    fn join_key_cases() {
        assert_eq!(join_key("", "a/b"), "a/b");
        assert_eq!(join_key("backup/", "2024/a.jpg"), "backup/2024/a.jpg");
        assert_eq!(join_key("backup", "2024/a.jpg"), "backup/2024/a.jpg");
        assert_eq!(join_key("a//", "b"), "a//b");
        assert_eq!(join_key("p", ""), "p/");
    }

    #[test]
    fn parent_prefix_cases() {
        assert_eq!(parent_prefix("photos/2024/"), "photos/");
        assert_eq!(parent_prefix("photos/2024/a.jpg"), "photos/2024/");
        assert_eq!(parent_prefix("a.jpg"), "");
        assert_eq!(parent_prefix("2024/"), "");
        assert_eq!(parent_prefix(""), "");
        assert_eq!(parent_prefix("/"), "");
    }

    #[test]
    fn strip_parent_implements_folder_semantics() {
        assert_eq!(
            strip_parent("photos/2024/", "photos/2024/a.jpg").unwrap(),
            "2024/a.jpg"
        );
        assert_eq!(
            strip_parent("photos/2024/", "photos/2024/b/c.jpg").unwrap(),
            "2024/b/c.jpg"
        );
        assert_eq!(
            strip_parent("photos/2024/", "photos/2024/").unwrap(),
            "2024/"
        );
        assert_eq!(
            strip_parent("photos/2024/a.jpg", "photos/2024/a.jpg").unwrap(),
            "a.jpg"
        );
        assert_eq!(strip_parent("top/", "top/x").unwrap(), "top/x");
        assert_eq!(strip_parent("a.jpg", "a.jpg").unwrap(), "a.jpg");
        assert_eq!(
            join_key(
                "backup/",
                strip_parent("photos/2024/", "photos/2024/a.jpg").unwrap()
            ),
            "backup/2024/a.jpg"
        );
    }

    #[test]
    fn strip_parent_rejects_keys_outside_the_selection() {
        assert!(strip_parent("photos/2024/", "photos/2025/a.jpg").is_err());
        assert!(strip_parent("photos/2024/a.jpg", "photos/2024/a.jpg.bak").is_err());
        assert!(strip_parent("photos/2024", "photos/2024/a.jpg").is_err());
        assert!(strip_parent("", "a").is_err());
    }

    #[test]
    fn safe_local_path_maps_inside_root() {
        let root = Path::new("dest");
        assert_eq!(safe_local_path(root, "a.jpg").unwrap(), root.join("a.jpg"));
        assert_eq!(
            safe_local_path(root, "2024/b/c.jpg").unwrap(),
            root.join("2024").join("b").join("c.jpg")
        );
        assert_eq!(safe_local_path(root, "2024/").unwrap(), root.join("2024"));
        assert_eq!(
            safe_local_path(root, "...hidden").unwrap(),
            root.join("...hidden")
        );
        assert_eq!(
            safe_local_path(root, "a..b/c").unwrap(),
            root.join("a..b").join("c")
        );
    }

    /// Inputs that must be rejected on every OS.
    const ALWAYS_UNSAFE: &[&str] = &[
        "",
        "/",
        "../../.ssh/authorized_keys",
        "..",
        "../",
        "a/../../b",
        "a/..",
        "./a",
        "a/./b",
        "a/.",
        "/etc/passwd",
        "//server/share/x",
        "a//b",
        "a/b//",
        "a\\b",
        "..\\..\\x",
        "\\\\server\\share\\x",
        "\\\\?\\C:\\x",
        "C:",
        "C:/Windows/system.ini",
        "c:foo",
        "x/C:/y",
        "x/d:",
        "x/b:c",
        "a\0b",
        "\0",
    ];

    #[test]
    fn unsafe_keys_are_rejected_in_both_styles() {
        for style in [PathStyle::Unix, PathStyle::Windows] {
            for key in ALWAYS_UNSAFE {
                assert!(
                    validate_relative_key(key, style).is_err(),
                    "{style:?} accepted {key:?}"
                );
            }
        }
        for key in ALWAYS_UNSAFE {
            assert!(
                safe_local_path(Path::new("dest"), key).is_err(),
                "native accepted {key:?}"
            );
        }
    }

    /// Inputs that are fine on Unix but unsafe or unrepresentable on Windows.
    const WINDOWS_ONLY_UNSAFE: &[&str] = &[
        "file.txt:stream",
        "a/bc:d",
        "10:30:00.log",
        "x/::$DATA",
        "what?.txt",
        "star*.txt",
        "pipe|name",
        "quote\"name",
        "less<than",
        "more>than",
        "tab\tname",
        "bell\u{7}",
        "trailing.",
        "dir./x",
        ".. ",
        "...",
        "trailing ",
        "CON",
        "con",
        "con.txt",
        "dir/NUL",
        "Nul.tar.gz",
        "aux",
        "PRN.log",
        "COM1",
        "com9.txt",
        "LPT1",
        "lpt\u{b9}",
        "COM0",
        "CONIN$",
        "conout$.x",
        "CON .txt",
    ];

    #[test]
    fn windows_rules_apply_on_every_os() {
        for key in WINDOWS_ONLY_UNSAFE {
            assert!(
                validate_relative_key(key, PathStyle::Windows).is_err(),
                "Windows accepted {key:?}"
            );
            assert!(
                validate_relative_key(key, PathStyle::Unix).is_ok(),
                "Unix rejected {key:?}"
            );
        }
        for key in [
            "CONSOLE",
            "COM10",
            "lpt",
            "auxiliary.txt",
            "nul_file",
            "comX",
        ] {
            assert!(
                validate_relative_key(key, PathStyle::Windows).is_ok(),
                "Windows rejected {key:?}"
            );
        }
    }

    #[test]
    fn native_style_matches_target() {
        assert_eq!(PathStyle::native() == PathStyle::Windows, cfg!(windows));
        for key in WINDOWS_ONLY_UNSAFE {
            assert_eq!(
                safe_local_path(Path::new("dest"), key).is_err(),
                cfg!(windows),
                "{key:?}"
            );
        }
    }

    #[test]
    fn rejection_messages_are_precise() {
        let cases = [
            ("../../.ssh/authorized_keys", "'..' segment"),
            ("a\\b", "backslash"),
            ("C:/x", "drive letter"),
            ("/abs", "absolute"),
            ("a//b", "empty path segment"),
            ("", "empty"),
            ("a\0b", "NUL"),
        ];
        for (key, expected) in cases {
            let err = validate_relative_key(key, PathStyle::Unix).unwrap_err();
            assert!(err.to_string().contains(expected), "{key:?}: {err}");
        }
        let err = validate_relative_key("CON", PathStyle::Windows).unwrap_err();
        assert!(err.to_string().contains("reserved device name"), "{err}");
    }

    #[test]
    fn segments_are_returned() {
        assert_eq!(
            validate_relative_key("a/b/c", PathStyle::Unix).unwrap(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            validate_relative_key("a/b/", PathStyle::Windows).unwrap(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn huge_keys_are_handled() {
        let key = "a/".repeat(10_000) + "f";
        let path = safe_local_path(Path::new("dest"), &key).unwrap();
        assert_eq!(path.components().count(), 10_002);
    }
}
