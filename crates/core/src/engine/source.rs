//! What an upload reads from: a local file, or any other positional byte
//! source.

use std::fmt;
use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};

/// The identity of an upload source's content when it was opened. An upload
/// resumes only while the fingerprint is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    /// Size in bytes.
    pub size: u64,
    /// Modification time, for sources that have one (files).
    pub modified: Option<DateTime<Utc>>,
}

/// Bytes to upload, read by position.
///
/// The reading methods block; the engines call them on Tokio's blocking
/// thread pool, from several threads at once for concurrent parts.
pub trait UploadSource: Send + Sync + fmt::Debug {
    /// Length in bytes; fixed for the whole upload.
    fn size(&self) -> u64;

    /// Fills `buf` with the bytes starting at `offset`. Fails (for example
    /// with [`io::ErrorKind::UnexpectedEof`]) when fewer bytes exist.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// The content identity used to decide whether a saved upload may
    /// resume.
    fn fingerprint(&self) -> SourceFingerprint;

    /// Checks again that the content has not changed since it was opened;
    /// called before a multipart upload is completed.
    fn is_unchanged(&self) -> io::Result<bool>;

    /// A description for messages, such as the quoted path.
    fn describe(&self) -> String;
}

/// A local file opened for upload. Reads are positional, so concurrent
/// parts share one handle and nothing beyond the requested range is read.
#[derive(Debug)]
pub struct FileSource {
    path: PathBuf,
    file: File,
    fingerprint: SourceFingerprint,
}

impl FileSource {
    /// Opens a regular file (blocking). Symbolic links are followed.
    ///
    /// Fails with [`Error::Usage`] when the path is not a regular file and
    /// with [`Error::Io`] when it cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).io_context(|| format!("opening '{}' for upload", path.display()))?;
        let metadata = file
            .metadata()
            .io_context(|| format!("reading the metadata of '{}'", path.display()))?;
        if !metadata.is_file() {
            return Err(Error::Usage(format!(
                "'{}' is not a regular file",
                path.display()
            )));
        }
        Ok(FileSource {
            path: path.to_path_buf(),
            file,
            fingerprint: fingerprint_of(&metadata),
        })
    }

    /// The path the file was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl UploadSource for FileSource {
    fn size(&self) -> u64 {
        self.fingerprint.size
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, buf, offset)
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.fingerprint.clone()
    }

    /// Compares the size and modification time of the open handle, which
    /// catches changes made in place. A file that was replaced at its path
    /// keeps being read consistently from the original handle.
    fn is_unchanged(&self) -> io::Result<bool> {
        Ok(fingerprint_of(&self.file.metadata()?) == self.fingerprint)
    }

    fn describe(&self) -> String {
        format!("'{}'", self.path.display())
    }
}

/// The fingerprint of a file from its metadata.
pub(crate) fn fingerprint_of(metadata: &Metadata) -> SourceFingerprint {
    SourceFingerprint {
        size: metadata.len(),
        modified: metadata.modified().ok().map(DateTime::<Utc>::from),
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the file is shorter than expected",
                ));
            }
            Ok(read) => {
                buf = &mut buf[read..];
                offset += read as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use std::io::Write;

    #[test]
    fn reads_ranges_by_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        std::fs::write(&path, &data).unwrap();
        let source = FileSource::open(&path).unwrap();
        assert_eq!(source.size(), 5000);
        assert_eq!(source.path(), path);
        assert!(source.describe().contains("data.bin"));
        let mut buf = vec![0u8; 100];
        source.read_exact_at(4000, &mut buf).unwrap();
        assert_eq!(buf, data[4000..4100]);
        source.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(buf, data[..100]);
        let err = source.read_exact_at(4950, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let fingerprint = source.fingerprint();
        assert_eq!(fingerprint.size, 5000);
        assert!(fingerprint.modified.is_some());
        assert!(source.is_unchanged().unwrap());
    }

    #[test]
    fn in_place_changes_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grow.bin");
        std::fs::write(&path, b"abc").unwrap();
        let source = FileSource::open(&path).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"def").unwrap();
        drop(file);
        assert!(!source.is_unchanged().unwrap());
    }

    #[test]
    fn folders_and_missing_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let missing = FileSource::open(&dir.path().join("missing")).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::Io, "{missing}");
        assert!(missing.to_string().contains("missing"), "{missing}");
        let folder = FileSource::open(dir.path()).unwrap_err();
        assert!(
            matches!(folder.kind(), ErrorKind::Usage | ErrorKind::Io),
            "{folder}"
        );
    }
}
