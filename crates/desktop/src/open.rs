//! Support for `open_object`: the private folder that receives the downloaded
//! copies, their file names, cleanup, and the bandwidth limiter as the
//! engines' throttle.

use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bucketbridge_core::engine::{Throttle, ThrottleFuture};
use bucketbridge_core::transfer::BandwidthLimiter;
use bucketbridge_core::{Dirs, Error, Result};
use uuid::Uuid;

/// Folder inside the data folder that holds the copies made by `open_object`.
pub const OPEN_DIR_NAME: &str = "open";

/// Copies older than this are deleted at startup. They are not deleted
/// sooner because the application showing one may still have it open.
pub(crate) const KEEP_OPENED_FILES: Duration = Duration::from_secs(24 * 60 * 60);

/// Longest file name produced by [`local_name`], in bytes (most file
/// systems allow 255).
const MAX_NAME_BYTES: usize = 200;

/// Longest extension kept when a name is shortened, in bytes.
const MAX_EXTENSION_BYTES: usize = 32;

/// `data_dir/open`.
pub(crate) fn open_root(dirs: &Dirs) -> PathBuf {
    dirs.data_dir.join(OPEN_DIR_NAME)
}

/// Creates a new folder `root/<uuid>` (and `root`), readable by the current
/// user only on Unix, so two opens never share a folder. Blocking.
pub(crate) fn new_open_dir(root: &Path) -> Result<PathBuf> {
    create_private_dir(root)?;
    let dir = root.join(Uuid::new_v4().to_string());
    create_private_dir(&dir)?;
    Ok(dir)
}

fn create_private_dir(dir: &Path) -> Result<()> {
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

/// A local file name for `key`: its last segment, made valid on every OS
/// (the Windows rules are the strictest). The extension is kept so that the
/// OS picks the right application.
///
/// Characters that Windows forbids and control characters become `_`,
/// trailing dots and spaces are removed, a reserved device name gets a `_`
/// prefix, an empty result becomes `object`, and long names are shortened.
pub(crate) fn local_name(key: &str) -> String {
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

/// Deletes the entries of `root` that were last modified more than `age`
/// ago. Best effort and blocking: a copy that cannot be deleted (on Windows,
/// one that is still open) is kept for the next start. Symbolic links are
/// removed, never followed.
pub(crate) fn purge(root: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|elapsed| elapsed > age);
        if old {
            let path = entry.path();
            let _ = if file_type.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// The app's [`BandwidthLimiter`] as the engines' [`Throttle`].
pub(crate) struct LimiterThrottle(pub(crate) BandwidthLimiter);

impl Throttle for LimiterThrottle {
    fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
        Box::pin(self.0.acquire(bytes))
    }

    /// At most one second's worth of the current rate, as the engines ask.
    fn max_grant(&self) -> u64 {
        self.0.rate().map_or(u64::MAX, NonZeroU64::get)
    }
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
        let root = Path::new("/tmp/open-test");
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
            bucketbridge_core::path::safe_local_path(root, &name)
                .unwrap_or_else(|err| panic!("{key:?} -> {name:?}: {err}"));
        }
    }

    #[test]
    fn purge_removes_only_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh");
        fs::create_dir(&fresh).unwrap();
        fs::write(fresh.join("a.txt"), b"a").unwrap();
        purge(dir.path(), Duration::from_secs(3600));
        assert!(fresh.exists());
        purge(dir.path(), Duration::ZERO);
        assert!(!fresh.exists());
        purge(&dir.path().join("missing"), Duration::ZERO);
    }

    #[test]
    fn open_dirs_are_new_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("open");
        let first = new_open_dir(&root).unwrap();
        let second = new_open_dir(&root).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&root) && first.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&first).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
