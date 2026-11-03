//! File names of edit copies and of the "save as a copy" key.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// Longest file name produced by [`local_name`], in bytes (most file
/// systems allow 255).
const MAX_NAME_BYTES: usize = 200;

/// Longest extension kept when a name is shortened, in bytes.
const MAX_EXTENSION_BYTES: usize = 32;

/// A local file name for `key`: its last segment, made valid on every OS
/// (the Windows rules are the strictest). The extension is kept so that the
/// OS picks the right application.
///
/// Characters that Windows forbids and control characters become `_`,
/// trailing dots and spaces are removed, a reserved device name gets a `_`
/// prefix, an empty result becomes `object`, and long names are shortened.
pub fn local_name(key: &str) -> String {
    let last = key.rsplit('/').next().unwrap_or(key);
    let replaced: String = last
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = shorten(replaced);
    let kept = name.trim_end_matches(['.', ' ']).len();
    name.truncate(kept);
    if name.is_empty() {
        name.push_str("object");
    }
    if is_reserved_device_name(&name) {
        name.insert(0, '_');
    }
    name
}

/// Cuts `name` to [`MAX_NAME_BYTES`], keeping a short extension.
fn shorten(name: String) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= MAX_EXTENSION_BYTES => name.split_at(dot),
        _ => (name.as_str(), ""),
    };
    let mut end = MAX_NAME_BYTES - extension.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &stem[..end])
}

/// Windows device names (`CON`, `NUL`, `COM1`, ...), whatever the extension.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str()) {
        return true;
    }
    ["COM", "LPT"].iter().any(|base| {
        stem.strip_prefix(base).is_some_and(|rest| {
            let mut chars = rest.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
            )
        })
    })
}

/// The key of the copy that "save as a copy" creates: `name (edited).ext`
/// next to `key`, or `name (edited N).ext` for `n` of 2 and more (the copy
/// that already exists is never overwritten).
pub fn copy_key(key: &str, n: u32) -> String {
    let (dir, name) = match key.rfind('/') {
        Some(slash) => key.split_at(slash + 1),
        None => ("", key),
    };
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    if n <= 1 {
        format!("{dir}{stem} (edited){extension}")
    } else {
        format!("{dir}{stem} (edited {n}){extension}")
    }
}

/// Creates `dir` (and its parents), readable by the current user only on
/// Unix. Blocking.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|err| Error::io(format!("creating {}", dir.display()), err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_the_last_segment_and_extension() {
        assert_eq!(local_name("reports/2026/q3.pdf"), "q3.pdf");
        assert_eq!(local_name("a.txt"), "a.txt");
        assert_eq!(local_name("photos/.hidden"), ".hidden");
    }

    #[test]
    fn names_are_valid_on_windows() {
        assert_eq!(
            local_name("x/a<b>:c\"d|e?f*g\\h.txt"),
            "a_b__c_d_e_f_g_h.txt"
        );
        assert_eq!(local_name("x/tab\there.txt"), "tab_here.txt");
        assert_eq!(local_name("x/trailing. . "), "trailing");
        assert_eq!(local_name("x/.."), "object");
        assert_eq!(local_name("x/."), "object");
        assert_eq!(local_name(""), "object");
        assert_eq!(local_name("x/CON"), "_CON");
        assert_eq!(local_name("x/nul.txt"), "_nul.txt");
        assert_eq!(local_name("x/com1.log"), "_com1.log");
        assert_eq!(local_name("x/LPT\u{b9}"), "_LPT\u{b9}");
        assert_eq!(local_name("x/console.txt"), "console.txt");
        assert_eq!(local_name("x/COM10.txt"), "COM10.txt");
    }

    #[test]
    fn long_names_are_shortened_on_a_char_boundary() {
        let long = format!("{}.jpeg", "\u{e9}".repeat(150));
        let name = local_name(&long);
        assert!(name.len() <= MAX_NAME_BYTES);
        assert!(name.ends_with(".jpeg"));
        let without_extension = "z".repeat(300);
        assert_eq!(local_name(&without_extension).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn every_name_is_a_safe_local_path() {
        let root = Path::new("/tmp/edit-test");
        for key in [
            "a/b/c.txt",
            "x/a<b>.txt",
            "x/..",
            "x/C:evil",
            "x/CON.txt",
            "x/\u{0}nul",
            "x/trailing...",
        ] {
            let name = local_name(key);
            crate::path::safe_local_path(root, &name)
                .unwrap_or_else(|err| panic!("{key:?} -> {name:?}: {err}"));
        }
    }

    #[test]
    fn copy_keys_sit_next_to_the_original() {
        assert_eq!(copy_key("a/b/report.txt", 1), "a/b/report (edited).txt");
        assert_eq!(copy_key("report.txt", 2), "report (edited 2).txt");
        assert_eq!(copy_key("dir/Makefile", 1), "dir/Makefile (edited)");
        assert_eq!(copy_key("x/.env", 1), "x/.env (edited)");
        assert_eq!(copy_key("a.tar.gz", 1), "a.tar (edited).gz");
    }
}
