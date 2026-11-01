//! Local crash reporter (task T14). See docs/logging.md.
//!
//! [`install`] adds a panic hook that writes a plain-text report to
//! `crash_dir/crash-<UTC timestamp>-<pid>.txt` (a `-N` suffix is added if that name is
//! taken) and then calls the previously installed hook. A report holds the panic
//! message, the source location, the thread, a backtrace (always captured, whatever
//! `RUST_BACKTRACE` says), the application version, the OS and the CPU architecture.
//! Nothing is ever uploaded: the report stays on disk until the user reads and
//! dismisses it. At most [`MAX_REPORTS_PER_PROCESS`] reports are written per process.
//!
//! [`last_report`] returns the newest report still in `crash_dir`; [`dismiss`] moves a
//! report into `crash_dir/dismissed/`. The path given to [`dismiss`] comes from the UI
//! and is treated as untrusted: it must canonicalize to a file directly inside
//! `crash_dir` whose name has the report format.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Folder inside the crash folder that receives dismissed reports.
pub const DISMISSED_DIR: &str = "dismissed";
const REPORT_PREFIX: &str = "crash-";
const REPORT_SUFFIX: &str = ".txt";
const NAME_TIME_FORMAT: &str = "%Y%m%dT%H%M%S%.6fZ";
/// Reports are read up to this size (a report is a few kilobytes).
const MAX_REPORT_BYTES: u64 = 1024 * 1024;
const MAX_NAME_ATTEMPTS: u32 = 1000;
/// At most this many reports are written per process, so that a panic loop (for
/// example a task that keeps panicking and is restarted) cannot fill the disk.
pub const MAX_REPORTS_PER_PROCESS: u64 = 10;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static REPORTS_WRITTEN: AtomicU64 = AtomicU64::new(0);

fn usage(message: impl Into<String>) -> Error {
    Error::Usage(message.into())
}

fn not_found(message: impl Into<String>) -> Error {
    Error::NotFound(message.into())
}

fn io_error(context: impl Into<String>, err: io::Error) -> Error {
    Error::io(context, err)
}

/// A crash report on disk. Serializes exactly as `CrashReport` in
/// docs/desktop-api.md: `{ "path": string, "created_at": RFC 3339 string,
/// "contents": string }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// Absolute path of the report; pass it to [`dismiss`].
    pub path: String,
    /// When the report was written (from its file name), UTC.
    pub created_at: DateTime<Utc>,
    /// The report text (cut after 1 MiB).
    pub contents: String,
}

/// Installs the crash-report panic hook in front of the current hook. Only the first
/// call has an effect; later calls (and calls from a panicking thread) return false.
pub fn install(crash_dir: impl Into<PathBuf>, app_version: impl Into<String>) -> bool {
    if thread::panicking() || INSTALLED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let crash_dir = crash_dir.into();
    let app_version = app_version.into();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Never panic in here: a panic inside a panic hook aborts the process.
        // A 64-bit counter cannot wrap around in practice.
        let allowed = REPORTS_WRITTEN.fetch_add(1, Ordering::SeqCst) < MAX_REPORTS_PER_PROCESS;
        if allowed
            && let Ok(path) =
                save_report(&crash_dir, Utc::now(), &render_report(info, &app_version))
        {
            let _ = writeln!(
                io::stderr(),
                "A crash report was saved to {}",
                path.display()
            );
        }
        previous(info);
    }));
    true
}

fn render_report(info: &PanicHookInfo<'_>, app_version: &str) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(the panic payload is not text)");
    let location = info.location().map_or_else(
        || "unknown".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    );
    let current = thread::current();
    let thread_name = current.name().unwrap_or("<unnamed>");
    format!(
        "BucketBridge crash report\n\
         This report was written on this computer only and has not been sent anywhere.\n\
         \n\
         Time:     {time}\n\
         Version:  {app_version}\n\
         OS:       {os}\n\
         Arch:     {arch}\n\
         Process:  {pid}\n\
         Thread:   {thread_name} ({thread_id:?})\n\
         Location: {location}\n\
         \n\
         Message:\n\
         {message}\n\
         \n\
         Backtrace:\n\
         {backtrace}\n",
        time = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        pid = std::process::id(),
        thread_id = current.id(),
        backtrace = std::backtrace::Backtrace::force_capture(),
    )
}

/// Writes `contents` as a new report named after `now` and the process id.
fn save_report(crash_dir: &Path, now: DateTime<Utc>, contents: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(crash_dir)?;
    let stamp = now.format(NAME_TIME_FORMAT).to_string();
    let pid = std::process::id();
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let name = if attempt == 0 {
            format!("{REPORT_PREFIX}{stamp}-{pid}{REPORT_SUFFIX}")
        } else {
            format!("{REPORT_PREFIX}{stamp}-{pid}-{attempt}{REPORT_SUFFIX}")
        };
        let path = crash_dir.join(name);
        match create_new(&path) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many crash reports with the same time stamp",
    ))
}

#[cfg(unix)]
fn create_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Parses a report file name into its time stamp and collision counter.
fn parse_report_name(name: &str) -> Option<(DateTime<Utc>, u32)> {
    let middle = name
        .strip_prefix(REPORT_PREFIX)?
        .strip_suffix(REPORT_SUFFIX)?;
    let mut parts = middle.split('-');
    let stamp = parts.next()?;
    let pid = parts.next()?;
    let counter = match parts.next() {
        Some(counter) => counter.parse::<u32>().ok().filter(|counter| *counter > 0)?,
        None => 0,
    };
    if parts.next().is_some() || pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let time = NaiveDateTime::parse_from_str(stamp, NAME_TIME_FORMAT).ok()?;
    Some((time.and_utc(), counter))
}

/// The newest report directly inside `crash_dir` (dismissed reports are not
/// considered), or `None` when there is none or the folder does not exist.
pub fn last_report(crash_dir: &Path) -> Result<Option<CrashReport>> {
    let listing = match fs::read_dir(crash_dir) {
        Ok(listing) => listing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(io_error(
                format!("cannot list the crash folder {}", crash_dir.display()),
                err,
            ));
        }
    };
    let mut newest: Option<(DateTime<Utc>, u32, String)> = None;
    for item in listing {
        let entry = item.map_err(|err| {
            io_error(
                format!("cannot list the crash folder {}", crash_dir.display()),
                err,
            )
        })?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some((created_at, counter)) = parse_report_name(&name) else {
            continue;
        };
        // `DirEntry::file_type` does not follow symbolic links: only real files count.
        if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
            continue;
        }
        let candidate = (created_at, counter, name);
        if newest.as_ref().is_none_or(|current| candidate > *current) {
            newest = Some(candidate);
        }
    }
    let Some((created_at, _, name)) = newest else {
        return Ok(None);
    };
    let path = std::path::absolute(crash_dir.join(&name))
        .map_err(|err| io_error("cannot resolve the crash folder", err))?;
    let contents = match read_report(&path) {
        Ok(contents) => contents,
        // Dismissed in the meantime.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(io_error(format!("cannot read {}", path.display()), err));
        }
    };
    let path = path.into_os_string().into_string().map_err(|_| {
        io_error(
            "cannot report the crash",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the crash folder path is not valid UTF-8",
            ),
        )
    })?;
    Ok(Some(CrashReport {
        path,
        created_at,
        contents,
    }))
}

fn read_report(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > MAX_REPORT_BYTES;
    bytes.truncate(usize::try_from(MAX_REPORT_BYTES).unwrap_or(usize::MAX));
    let mut contents = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        contents.push_str("\n[the rest of the report is not shown]\n");
    }
    Ok(contents)
}

/// Moves the report at `path` into `crash_dir/dismissed/`.
///
/// `path` must be absolute and resolve (after following symbolic links and `..`) to a
/// regular file directly inside `crash_dir` with a report file name; anything else is a
/// usage error. A well-formed report path that no longer exists is `not_found`.
pub fn dismiss(crash_dir: &Path, path: &str) -> Result<()> {
    let rejected = || usage("not a crash report in the crash folder");
    if path.is_empty() || path.contains('\0') {
        return Err(rejected());
    }
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(rejected());
    }
    let given_name = candidate.file_name().and_then(OsStr::to_str);
    if given_name.and_then(parse_report_name).is_none() {
        return Err(rejected());
    }
    let folder = fs::canonicalize(crash_dir).map_err(|_| rejected())?;
    let resolved = match fs::canonicalize(candidate) {
        Ok(resolved) => resolved,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let in_folder = candidate
                .parent()
                .and_then(|parent| fs::canonicalize(parent).ok())
                .is_some_and(|parent| parent == folder);
            return Err(if in_folder {
                not_found("the crash report no longer exists")
            } else {
                rejected()
            });
        }
        Err(_) => return Err(rejected()),
    };
    if resolved.parent() != Some(folder.as_path()) {
        return Err(rejected());
    }
    let Some(name) = resolved
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| parse_report_name(name).is_some())
    else {
        return Err(rejected());
    };
    let is_file = fs::symlink_metadata(&resolved).is_ok_and(|meta| meta.is_file());
    if !is_file {
        return Err(rejected());
    }
    let dismissed = folder.join(DISMISSED_DIR);
    fs::create_dir_all(&dismissed)
        .map_err(|err| io_error(format!("cannot create {}", dismissed.display()), err))?;
    let target = free_name(&dismissed, name)?;
    fs::rename(&resolved, &target).map_err(|err| {
        io_error(
            format!("cannot move {} to {}", resolved.display(), target.display()),
            err,
        )
    })
}

/// `dir/name`, or `dir/<stem>-dupN.txt` when that is taken.
fn free_name(dir: &Path, name: &str) -> Result<PathBuf> {
    let stem = name.strip_suffix(REPORT_SUFFIX).unwrap_or(name);
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let candidate = if attempt == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem}-dup{attempt}{REPORT_SUFFIX}"))
        };
        match fs::symlink_metadata(&candidate) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(err) => {
                return Err(io_error(
                    format!("cannot check {}", candidate.display()),
                    err,
                ));
            }
            Ok(_) => {}
        }
    }
    Err(io_error(
        "cannot dismiss the crash report",
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many dismissed reports with the same name",
        ),
    ))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::error::ErrorKind;

    fn at(secs: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 29, 8, 0, secs).unwrap()
    }

    fn crash_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let crash = dir.path().join("crash");
        fs::create_dir_all(&crash).unwrap();
        (dir, crash)
    }

    fn path_text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn crash_report_serializes_as_in_the_contract() {
        let report = CrashReport {
            path: "/data/crash/crash-20261029T080005.000000Z-7.txt".to_owned(),
            created_at: at(5),
            contents: "boom".to_owned(),
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "path": "/data/crash/crash-20261029T080005.000000Z-7.txt",
                "created_at": "2026-10-29T08:00:05Z",
                "contents": "boom"
            })
        );
        assert_eq!(
            serde_json::from_value::<CrashReport>(value).unwrap(),
            report
        );
    }

    #[test]
    fn report_names_round_trip() {
        let (_dir, crash) = crash_dir();
        let path = save_report(&crash, at(5), "x").unwrap();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(
            name,
            format!("crash-20261029T080005.000000Z-{}.txt", std::process::id())
        );
        assert_eq!(parse_report_name(&name), Some((at(5), 0)));
        assert_eq!(
            parse_report_name("crash-20261029T080005.000000Z-77-3.txt"),
            Some((at(5), 3))
        );
        for bad in [
            "crash-.txt",
            "crash-20261029T080005.000000Z.txt",
            "crash-20261029T080005.000000Z-.txt",
            "crash-20261029T080005.000000Z-12a.txt",
            "crash-20261029T080005.000000Z-1-0.txt",
            "crash-20261029T080005.000000Z-1-2-3.txt",
            "crash-garbage-1.txt",
            "Crash-20261029T080005.000000Z-1.txt",
            "crash-20261029T080005.000000Z-1.log",
        ] {
            assert_eq!(parse_report_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn same_time_stamp_gets_a_counter() {
        let (_dir, crash) = crash_dir();
        let first = save_report(&crash, at(1), "first").unwrap();
        let second = save_report(&crash, at(1), "second").unwrap();
        assert_ne!(first, second);
        let report = last_report(&crash).unwrap().unwrap();
        assert_eq!(report.contents, "second");
        assert_eq!(report.created_at, at(1));
    }

    #[test]
    fn last_report_picks_the_newest_and_ignores_other_files() {
        let (_dir, crash) = crash_dir();
        assert!(last_report(&crash).unwrap().is_none());
        assert!(last_report(&crash.join("missing")).unwrap().is_none());
        save_report(&crash, at(1), "old").unwrap();
        let newest = save_report(&crash, at(9), "newest").unwrap();
        save_report(&crash, at(5), "middle").unwrap();
        fs::write(crash.join("notes.txt"), "not a report").unwrap();
        fs::create_dir_all(crash.join("crash-20261029T080059.000000Z-1.txt")).unwrap();
        fs::create_dir_all(crash.join(DISMISSED_DIR)).unwrap();
        fs::write(
            crash
                .join(DISMISSED_DIR)
                .join("crash-20261029T080058.000000Z-1.txt"),
            "dismissed",
        )
        .unwrap();
        let report = last_report(&crash).unwrap().unwrap();
        assert_eq!(report.contents, "newest");
        assert_eq!(report.created_at, at(9));
        assert_eq!(
            Path::new(&report.path),
            std::path::absolute(&newest).unwrap()
        );
    }

    #[test]
    fn dismiss_moves_reports_and_reveals_the_previous_one() {
        let (_dir, crash) = crash_dir();
        save_report(&crash, at(1), "older").unwrap();
        save_report(&crash, at(2), "newer").unwrap();
        let report = last_report(&crash).unwrap().unwrap();
        dismiss(&crash, &report.path).unwrap();
        let name = Path::new(&report.path).file_name().unwrap();
        assert!(crash.join(DISMISSED_DIR).join(name).is_file());
        assert_eq!(last_report(&crash).unwrap().unwrap().contents, "older");
        // Dismissing it again: the report is gone.
        assert_eq!(
            dismiss(&crash, &report.path).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let older = last_report(&crash).unwrap().unwrap();
        dismiss(&crash, &older.path).unwrap();
        assert!(last_report(&crash).unwrap().is_none());
    }

    #[test]
    fn dismissing_a_name_that_was_dismissed_before_keeps_both() {
        let (_dir, crash) = crash_dir();
        let path = save_report(&crash, at(3), "first").unwrap();
        let text = path_text(&path);
        dismiss(&crash, &text).unwrap();
        fs::write(&path, "second").unwrap();
        dismiss(&crash, &text).unwrap();
        let mut names: Vec<String> = fs::read_dir(crash.join(DISMISSED_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|name| name.contains("-dup1")));
    }

    #[test]
    fn dismiss_rejects_paths_outside_the_crash_folder() {
        let (dir, crash) = crash_dir();
        let report = save_report(&crash, at(4), "report").unwrap();
        let name = report.file_name().unwrap().to_str().unwrap().to_owned();
        let outside = dir.path().join(&name);
        fs::write(&outside, "outside").unwrap();
        fs::create_dir_all(crash.join(DISMISSED_DIR)).unwrap();
        let in_dismissed = crash.join(DISMISSED_DIR).join(&name);
        fs::write(&in_dismissed, "already dismissed").unwrap();
        fs::create_dir_all(crash.join("sub")).unwrap();
        let nested = crash.join("sub").join(&name);
        fs::write(&nested, "nested").unwrap();
        fs::write(crash.join("notes.txt"), "not a report").unwrap();
        let folder_name = "crash-20261029T080006.000000Z-1.txt";
        fs::create_dir_all(crash.join(folder_name)).unwrap();

        let attempts = vec![
            String::new(),
            name.clone(),
            format!("crash/{name}"),
            path_text(&outside),
            path_text(&crash.join("..").join(&name)),
            path_text(&crash.join("sub").join("..").join("..").join(&name)),
            path_text(&in_dismissed),
            path_text(&nested),
            path_text(&crash.join("notes.txt")),
            path_text(&crash.join(folder_name)),
            path_text(&crash),
            format!("{}\0", path_text(&report)),
        ];
        for attempt in &attempts {
            let err = dismiss(&crash, attempt).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{attempt:?}");
        }
        // A missing crash folder cannot contain the report.
        let err = dismiss(&dir.path().join("nowhere"), &path_text(&report)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        // Nothing was moved or deleted.
        assert!(
            report.is_file() && outside.is_file() && in_dismissed.is_file() && nested.is_file()
        );
        // A path through ".." that ends up inside the folder is fine.
        let roundabout = crash.join("sub").join("..").join(&name);
        dismiss(&crash, &path_text(&roundabout)).unwrap();
        assert!(!report.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dismiss_does_not_follow_symlinks_out_of_the_folder() {
        use std::os::unix::fs::symlink;

        let (dir, crash) = crash_dir();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep me").unwrap();
        let link = crash.join("crash-20261029T080007.000000Z-1.txt");
        symlink(&victim, &link).unwrap();
        assert!(last_report(&crash).unwrap().is_none());
        let err = dismiss(&crash, &path_text(&link)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
        assert!(fs::symlink_metadata(&link).is_ok());

        // A crash folder reached through a symbolic link works.
        let alias = dir.path().join("alias");
        symlink(&crash, &alias).unwrap();
        let report = save_report(&alias, at(8), "via alias").unwrap();
        let found = last_report(&alias).unwrap().unwrap();
        assert_eq!(found.contents, "via alias");
        dismiss(&alias, &found.path).unwrap();
        assert!(!report.exists());
    }

    #[test]
    fn large_reports_are_cut_when_read() {
        let (_dir, crash) = crash_dir();
        let big = "x".repeat(usize::try_from(MAX_REPORT_BYTES).unwrap() + 10);
        save_report(&crash, at(1), &big).unwrap();
        let report = last_report(&crash).unwrap().unwrap();
        assert!(
            report
                .contents
                .ends_with("[the rest of the report is not shown]\n")
        );
        assert!(report.contents.len() < big.len() + 64);
    }
}
