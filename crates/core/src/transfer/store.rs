//! On-disk format of the transfer queue and atomic, durable writes.
//!
//! ```json
//! {"version": 1, "tasks": [{"task": { ...TransferTask... }, "resume": null}]}
//! ```
//!
//! A task whose cancellation was cut short by a shutdown also carries
//! `"cancel_pending": true`; readers that do not know the field ignore it.
//!
//! Tasks are stored in creation order. A save writes a temporary file in the
//! same directory, flushes it to disk, renames it over the queue file and (on
//! Unix) flushes the directory, so a crash leaves either the old or the new
//! file. A file that cannot be read is reported and never overwritten: no
//! queue is opened from it, so nothing can save over it.

use std::collections::HashSet;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::io_error;
use crate::error::{Error, Result};
use crate::model::TransferTask;

/// Version written into the file; files with another version are rejected.
pub(super) const FORMAT_VERSION: u32 = 1;

/// One task with its runner's resume state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredTask {
    pub(super) task: TransferTask,
    #[serde(default)]
    pub(super) resume: Option<Value>,
    /// A cancel whose cleanup had not finished when the queue was saved; the
    /// next open runs the cleanup again. Omitted when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) cancel_pending: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    tasks: &'a [StoredTask],
}

#[derive(Deserialize)]
struct FileOwned {
    version: u32,
    tasks: Vec<StoredTask>,
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

/// Reads the queue file. A missing file is an empty queue.
pub(super) fn load(path: &Path) -> Result<Vec<StoredTask>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(io_error(
                format!("cannot read the transfer queue file {}", path.display()),
                error,
            ));
        }
    };
    let file: FileOwned = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(parse_error) => {
            if let Ok(header) = serde_json::from_slice::<Header>(&bytes)
                && header.version != FORMAT_VERSION
            {
                return Err(version_error(path, header.version));
            }
            return Err(damaged(path, parse_error.to_string()));
        }
    };
    if file.version != FORMAT_VERSION {
        return Err(version_error(path, file.version));
    }
    let mut ids = HashSet::with_capacity(file.tasks.len());
    for stored in &file.tasks {
        if !ids.insert(stored.task.id.as_str()) {
            return Err(damaged(
                path,
                format!("task id {} appears more than once", stored.task.id),
            ));
        }
    }
    Ok(file.tasks)
}

/// Atomically replaces the queue file with `tasks`.
pub(super) fn save(path: &Path, tasks: &[StoredTask]) -> Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let failed = |what: &str, error: io::Error| {
        io_error(
            format!(
                "cannot save the transfer queue to {} ({what})",
                path.display()
            ),
            error,
        )
    };
    fs::create_dir_all(&dir).map_err(|e| failed("creating the directory", e))?;
    let prefix = format!(
        ".{}.",
        path.file_name()
            .map_or_else(|| "transfers".into(), |name| name.to_string_lossy())
    );
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(&dir)
        .map_err(|e| failed("creating a temporary file", e))?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        let file = FileRef {
            version: FORMAT_VERSION,
            tasks,
        };
        serde_json::to_writer(&mut writer, &file).map_err(|e| failed("writing", e.into()))?;
        writer.flush().map_err(|e| failed("writing", e))?;
    }
    temp.as_file()
        .sync_all()
        .map_err(|e| failed("flushing to disk", e))?;
    temp.persist(path)
        .map_err(|e| failed("replacing the file", e.error))?;
    sync_dir(&dir).map_err(|e| failed("flushing the directory", e))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    // Directories cannot be opened for syncing on this platform; the rename
    // itself is journaled by the file system.
    Ok(())
}

fn damaged(path: &Path, detail: String) -> Error {
    io_error(
        format!(
            "the transfer queue file {} is damaged and was left unchanged (move it elsewhere \
             to start with an empty queue)",
            path.display()
        ),
        io::Error::new(io::ErrorKind::InvalidData, detail),
    )
}

fn version_error(path: &Path, version: u32) -> Error {
    io_error(
        format!(
            "the transfer queue file {} has format version {version}, but this version of \
             BucketBridge reads version {FORMAT_VERSION}; it was left unchanged",
            path.display()
        ),
        io::Error::new(io::ErrorKind::InvalidData, "unsupported format version"),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::error::ErrorKind;
    use crate::model::{Location, TransferKind, TransferState};

    fn stored(id: &str, resume: Option<Value>) -> StoredTask {
        let at = Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).unwrap();
        StoredTask {
            task: TransferTask {
                id: id.into(),
                account_id: "acct".into(),
                group_id: Some("group".into()),
                kind: TransferKind::Upload,
                src: Location::Local {
                    path: PathBuf::from("/data/a.bin"),
                },
                dst: Some(Location::S3 {
                    bucket: "bucket".into(),
                    key: "a.bin".into(),
                }),
                bytes_total: Some(100),
                bytes_done: 40,
                speed: 0,
                eta: None,
                state: TransferState::Stopped,
                error: None,
                attempts: 2,
                created_at: at,
                updated_at: at,
            },
            resume,
            cancel_pending: false,
        }
    }

    #[test]
    fn missing_file_is_an_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("nope.json")).unwrap().is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("transfers.json");
        let tasks = vec![
            stored("one", Some(json!({"upload_id": "x", "parts": [1, 2]}))),
            StoredTask {
                cancel_pending: true,
                ..stored("two", None)
            },
        ];
        save(&path, &tasks).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].task, tasks[0].task);
        assert_eq!(loaded[0].resume, tasks[0].resume);
        assert!(!loaded[0].cancel_pending);
        assert_eq!(loaded[1].task.id, "two");
        assert_eq!(loaded[1].resume, None);
        assert!(loaded[1].cancel_pending);
        // The documented shape, including the contract's field names; the
        // cancel flag is only written when set.
        let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["version"], json!(1));
        assert!(raw["tasks"][0].get("cancel_pending").is_none());
        assert_eq!(raw["tasks"][1]["cancel_pending"], json!(true));
        assert_eq!(raw["tasks"][0]["task"]["state"], json!("stopped"));
        assert_eq!(raw["tasks"][0]["task"]["src"]["type"], json!("local"));
        assert_eq!(raw["tasks"][0]["task"]["dst"]["type"], json!("s3"));
        assert_eq!(
            raw["tasks"][0]["task"]["created_at"],
            json!("2026-10-29T08:15:00Z")
        );
        // Overwriting leaves no temporary files behind.
        save(&path, &tasks[..1]).unwrap();
        assert_eq!(load(&path).unwrap().len(), 1);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("transfers.json")]);
    }

    #[test]
    fn damaged_files_are_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.json");
        let duplicate = serde_json::to_vec(&FileRef {
            version: 1,
            tasks: &[stored("same", None), stored("same", None)],
        })
        .unwrap();
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"{not json".to_vec(), "damaged"),
            (Vec::new(), "damaged"),
            (b"{\"version\": 1}".to_vec(), "damaged"),
            (
                b"{\"version\": 1, \"tasks\": [{\"task\": 5}]}".to_vec(),
                "damaged",
            ),
            (
                b"{\"version\": 2, \"tasks\": []}".to_vec(),
                "format version 2",
            ),
            (
                b"{\"version\": 7, \"tasks\": \"new layout\"}".to_vec(),
                "format version 7",
            ),
            (duplicate, "appears more than once"),
        ];
        for (contents, expected) in cases {
            fs::write(&path, &contents).unwrap();
            let error = load(&path).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Io);
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("transfers.json"), "{message}");
            assert_eq!(fs::read(&path).unwrap(), contents, "file must be untouched");
        }
    }

    #[test]
    fn unreadable_path_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be.
        let error = load(dir.path()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
    }

    #[test]
    fn save_reports_unwritable_locations() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, b"x").unwrap();
        let error = save(&blocker.join("transfers.json"), &[]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert!(error.to_string().contains("cannot save the transfer queue"));
    }
}
