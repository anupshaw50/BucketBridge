//! Windows-independent helpers of the mount: drive letters and folders as
//! mount points, Windows path splitting, file names Windows can show, and
//! attribute values. Always compiled and tested on every platform; the
//! Dokan adapter (`windows`, `--features mount` on Windows) uses them.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::mount::NodeKind;

/// `FILE_ATTRIBUTE_READONLY`.
pub const ATTRIBUTE_READONLY: u32 = 0x1;
/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// `FILE_ATTRIBUTE_NORMAL`.
pub const ATTRIBUTE_NORMAL: u32 = 0x80;

/// Where a Windows mount appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountTarget {
    /// A drive letter, upper case (`'X'` for `X:`).
    Drive(char),
    /// An existing empty folder on an NTFS volume.
    Folder(PathBuf),
}

impl MountTarget {
    /// Parses what the user typed or picked: `X`, `x:`, `X:\` or `X:/` is a
    /// drive letter; anything else must be an absolute folder path.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if let Some(letter) = parse_drive_letter(text) {
            return Ok(Self::Drive(letter));
        }
        if text.is_empty() {
            return Err(Error::Usage("choose a drive letter or a folder".into()));
        }
        if !is_absolute_windows(text) {
            return Err(Error::Usage(format!(
                "{text} is neither a drive letter (for example X:) nor an absolute folder path"
            )));
        }
        Ok(Self::Folder(PathBuf::from(text)))
    }

    /// The text shown in lists and stored as the mount point: `X:` or the
    /// folder path.
    pub fn display(&self) -> String {
        match self {
            Self::Drive(letter) => format!("{letter}:"),
            Self::Folder(path) => path.display().to_string(),
        }
    }

    /// The name handed to the driver: `X:\` or the folder path.
    pub fn driver_name(&self) -> String {
        match self {
            Self::Drive(letter) => format!("{letter}:\\"),
            Self::Folder(path) => path.display().to_string(),
        }
    }

    /// The folder, when this target is one.
    pub fn folder(&self) -> Option<&Path> {
        match self {
            Self::Folder(path) => Some(path),
            Self::Drive(_) => None,
        }
    }
}

/// `X`, `x:`, `X:\` and `X:/` (any letter A-Z) give the upper-case letter.
pub fn parse_drive_letter(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let rest: String = chars.collect();
    matches!(rest.as_str(), "" | ":" | ":\\" | ":/").then(|| letter.to_ascii_uppercase())
}

fn is_absolute_windows(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    drive || text.starts_with("\\\\")
}

/// Letters that can still be offered, from the bitmask `GetLogicalDrives`
/// returns (bit 0 is `A:`). `A:` and `B:` are never offered.
pub fn free_letters(used_mask: u32) -> Vec<char> {
    ('C'..='Z')
        .enumerate()
        .filter(|(index, _)| used_mask & (1 << (index + 2)) == 0)
        .map(|(_, letter)| letter)
        .collect()
}

/// Splits a path the driver passes (`\photos\2026\a.jpg`) into names. The
/// root gives no names. Names with a colon (alternate data streams) or empty
/// names make the path invalid (`None`).
pub fn split_path(path: &str) -> Option<Vec<&str>> {
    let names: Vec<&str> = path
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();
    names
        .iter()
        .all(|name| !name.contains(':') && *name != "." && *name != "..")
        .then_some(names)
}

/// Whether Windows can show `name` as a file name: no reserved characters
/// (`< > : " | ? *`, control characters), no trailing dot or space, at most
/// 255 UTF-16 units, and not a reserved device name such as `CON`.
pub fn windows_name_ok(name: &str) -> bool {
    if name.is_empty() || name.encode_utf16().count() > 255 {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/')
    }) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|digit| digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit())
        });
    !reserved
}

/// The Windows attribute bits of a node: directories and files are both
/// read-only.
pub fn attributes(kind: NodeKind) -> u32 {
    match kind {
        NodeKind::Dir => ATTRIBUTE_DIRECTORY | ATTRIBUTE_READONLY,
        NodeKind::File => ATTRIBUTE_READONLY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_parsed_in_every_spelling() {
        for text in ["x", "X", "x:", "X:", "X:\\", "x:/"] {
            assert_eq!(parse_drive_letter(text), Some('X'), "{text}");
        }
        for text in ["", "1:", "XY", "X:\\a", "::", "é:"] {
            assert_eq!(parse_drive_letter(text), None, "{text}");
        }
    }

    #[test]
    fn targets_are_drives_or_absolute_folders() {
        assert_eq!(MountTarget::parse(" z: ").unwrap(), MountTarget::Drive('Z'));
        assert_eq!(MountTarget::Drive('Z').display(), "Z:");
        assert_eq!(MountTarget::Drive('Z').driver_name(), "Z:\\");
        let folder = MountTarget::parse("C:\\mnt\\bucket").unwrap();
        assert_eq!(folder.folder(), Some(Path::new("C:\\mnt\\bucket")));
        assert!(MountTarget::parse("\\\\server\\share").is_ok());
        assert!(MountTarget::parse("").is_err());
        assert!(MountTarget::parse("relative\\dir").is_err());
        assert!(MountTarget::parse("/mnt/x").is_err());
    }

    #[test]
    fn free_letters_skip_used_drives_and_floppies() {
        // A, C and E used.
        let mask = 0b10101;
        let free = free_letters(mask);
        assert!(!free.contains(&'A') && !free.contains(&'B'));
        assert!(!free.contains(&'C') && !free.contains(&'E'));
        assert!(free.contains(&'D') && free.contains(&'Z'));
        assert_eq!(free_letters(u32::MAX), Vec::<char>::new());
    }

    #[test]
    fn paths_split_into_names() {
        assert_eq!(split_path("\\"), Some(vec![]));
        assert_eq!(
            split_path("\\a\\b c\\d.txt"),
            Some(vec!["a", "b c", "d.txt"])
        );
        assert_eq!(split_path("a/b"), Some(vec!["a", "b"]));
        assert_eq!(split_path("\\a:stream"), None);
        assert_eq!(split_path("\\a\\..\\b"), None);
    }

    #[test]
    fn only_names_windows_can_show_are_listed() {
        assert!(windows_name_ok("report 2026.pdf"));
        assert!(windows_name_ok("naïve.txt"));
        for bad in [
            "",
            "a:b",
            "a?b",
            "a*",
            "a<b",
            "a\"b",
            "a|b",
            "trail.",
            "trail ",
            "con",
            "NUL.txt",
            "com1",
            "lpt9.log",
            "tab\there",
        ] {
            assert!(!windows_name_ok(bad), "{bad:?}");
        }
        assert!(windows_name_ok("com10"));
        assert!(!windows_name_ok(&"a".repeat(256)));
    }

    #[test]
    fn attributes_mark_everything_read_only() {
        assert_eq!(attributes(NodeKind::File), 0x1);
        assert_eq!(attributes(NodeKind::Dir), 0x11);
    }
}
