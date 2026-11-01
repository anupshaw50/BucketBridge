//! The Windows adapter of the read-only mount: a thin shell around
//! [`MountCore`] on the Dokan user-mode library (the `dokan` crate, MIT).
//! Compiled only with the `mount` cargo feature on Windows; the Dokan driver
//! and library are installed by the user (see docs/mount.md).
//!
//! The volume is mounted write-protected and case sensitive (S3 keys are).
//! Every operation that would change something answers "media is write
//! protected". Names Windows cannot show are left out of listings
//! ([`windows_name_ok`](super::target::windows_name_ok)).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dokan::{
    CreateFileInfo, DiskSpaceInfo, FileInfo, FileSystemHandler, FileSystemMounter,
    FileTimeOperation, FillDataError, FillDataResult, FindData, IO_SECURITY_CONTEXT, MountFlags,
    MountOptions, OperationInfo, OperationResult, VolumeInfo,
};
use dokan_sys::win32::{
    FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_IF,
};
use tokio::runtime::Handle;
use widestring::{U16CStr, U16CString};
use winapi::shared::ntdef::NTSTATUS;
use winapi::shared::ntstatus::{
    STATUS_ACCESS_DENIED, STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_PARAMETER,
    STATUS_MEDIA_WRITE_PROTECTED, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION,
    STATUS_OBJECT_NAME_INVALID, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_UNEXPECTED_IO_ERROR,
};
use winapi::um::winnt::{
    FILE_APPEND_DATA, FILE_CASE_PRESERVED_NAMES, FILE_CASE_SENSITIVE_SEARCH, FILE_UNICODE_ON_DISK,
    FILE_WRITE_DATA,
};

use super::target::{self, MountTarget};
use super::{Attr, MountCore, NodeKind, ROOT_INODE};
use crate::error::{Error, ErrorKind, Result};

/// How often the mount thread checks whether the volume went away.
const POLL: Duration = Duration::from_millis(250);

/// How long Dokan waits for one request (S3 calls can be slow) before it
/// gives up and unmounts.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A running mount. Dropping it unmounts.
pub struct MountHandle {
    thread: Option<JoinHandle<()>>,
    stop: mpsc::Sender<()>,
    mountpoint: PathBuf,
}

impl MountHandle {
    /// The drive letter (`X:`) or folder the bucket is mounted on.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Whether the volume ended (for example after it was ejected from
    /// outside).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Unmounts and waits for the mount thread.
    pub fn unmount(mut self) -> Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<()> {
        let _ = self.stop.send(());
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| Error::Internal("the mount thread panicked".into())),
            None => Ok(()),
        }
    }
}

impl Drop for MountHandle {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

/// The drive letters that are free now (`C:` and up), for the mount dialog.
pub fn free_drive_letters() -> Vec<char> {
    let mask = ('A'..='Z')
        .enumerate()
        .filter(|(_, letter)| Path::new(&format!("{letter}:\\")).exists())
        .fold(0_u32, |mask, (index, _)| mask | (1 << index));
    target::free_letters(mask)
}

static INIT: Once = Once::new();

/// Mounts `core` on `mountpoint`: a free drive letter (`X:`) or an existing
/// empty folder on an NTFS volume. `handle` is the async runtime the S3 calls
/// run on.
pub fn mount(core: Arc<MountCore>, mountpoint: &Path, handle: Handle) -> Result<MountHandle> {
    let target = MountTarget::parse(&mountpoint.to_string_lossy())?;
    match &target {
        MountTarget::Drive(letter) => {
            if !free_drive_letters().contains(letter) {
                return Err(Error::Usage(format!(
                    "{letter}: is not a free drive letter"
                )));
            }
        }
        MountTarget::Folder(path) => {
            if !path.is_dir() {
                return Err(Error::Usage(format!(
                    "{} is not an existing folder",
                    path.display()
                )));
            }
        }
    }
    let name = U16CString::from_str(target.driver_name())
        .map_err(|_| Error::Usage("the mount point contains a NUL character".into()))?;
    let bucket = core.bucket().to_owned();
    // Dokan keeps global state; initialise it once for the whole process.
    INIT.call_once(dokan::init);

    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("bucketbridge-mount".into())
        .spawn(move || {
            let closed = Arc::new(AtomicBool::new(false));
            let handler = BucketFs {
                core,
                handle,
                bucket,
                closed: Arc::clone(&closed),
            };
            let options = MountOptions {
                single_thread: false,
                flags: MountFlags::WRITE_PROTECT | MountFlags::CASE_SENSITIVE,
                timeout: REQUEST_TIMEOUT,
                allocation_unit_size: 4096,
                sector_size: 512,
                ..MountOptions::default()
            };
            let mut mounter = FileSystemMounter::new(&handler, &name, &options);
            match mounter.mount() {
                Ok(fs) => {
                    let _ = ready_tx.send(Ok(()));
                    loop {
                        match stop_rx.recv_timeout(POLL) {
                            Err(RecvTimeoutError::Timeout) if !closed.load(Ordering::Acquire) => {}
                            _ => break,
                        }
                    }
                    if !closed.load(Ordering::Acquire) {
                        let _ = dokan::unmount(&name);
                    }
                    // Dropping the file system waits until Dokan finished.
                    drop(fs);
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(Error::Usage(format!(
                        "cannot mount on {}: {err}",
                        target.display()
                    ))));
                }
            }
        })
        .map_err(|err| Error::io("starting the mount thread", err))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(MountHandle {
            thread: Some(thread),
            stop: stop_tx,
            mountpoint: mountpoint.to_path_buf(),
        }),
        Ok(Err(err)) => {
            let _ = thread.join();
            Err(err)
        }
        Err(_) => Err(Error::Internal(
            "the mount thread ended unexpectedly".into(),
        )),
    }
}

struct BucketFs {
    core: Arc<MountCore>,
    handle: Handle,
    bucket: String,
    closed: Arc<AtomicBool>,
}

/// What an open file object remembers.
struct Open {
    ino: u64,
}

fn status(err: &Error) -> NTSTATUS {
    match err.kind() {
        ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        ErrorKind::Auth | ErrorKind::Locked | ErrorKind::MfaRequired => STATUS_ACCESS_DENIED,
        ErrorKind::Usage => STATUS_INVALID_PARAMETER,
        _ => STATUS_UNEXPECTED_IO_ERROR,
    }
}

fn time_of(attr: &Attr) -> SystemTime {
    attr.modified
        .and_then(|t| u64::try_from(t.timestamp()).ok())
        .map_or(UNIX_EPOCH, |secs| UNIX_EPOCH + Duration::from_secs(secs))
}

impl BucketFs {
    /// Walks `path` from the root, one lookup per name.
    fn resolve(&self, path: &U16CStr) -> OperationResult<Attr> {
        let text = path.to_string().map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
        let names = target::split_path(&text).ok_or(STATUS_OBJECT_NAME_INVALID)?;
        let mut ino = ROOT_INODE;
        let mut attr = self.core.getattr(ino).map_err(|e| status(&e))?;
        for name in names {
            if attr.kind != NodeKind::Dir {
                return Err(STATUS_OBJECT_NAME_NOT_FOUND);
            }
            attr = self
                .handle
                .block_on(self.core.lookup(ino, name))
                .map_err(|e| status(&e))?;
            ino = attr.ino;
        }
        Ok(attr)
    }

    fn info_of(attr: &Attr) -> FileInfo {
        let time = time_of(attr);
        FileInfo {
            attributes: target::attributes(attr.kind),
            creation_time: time,
            last_access_time: time,
            last_write_time: time,
            file_size: attr.size,
            number_of_links: 1,
            file_index: attr.ino,
        }
    }
}

impl<'c, 'h: 'c> FileSystemHandler<'c, 'h> for BucketFs {
    type Context = Open;

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        _security_context: &IO_SECURITY_CONTEXT,
        desired_access: u32,
        _file_attributes: u32,
        _share_access: u32,
        create_disposition: u32,
        create_options: u32,
        _info: &mut OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<CreateFileInfo<Self::Context>> {
        let attr = match self.resolve(file_name) {
            Ok(attr) => attr,
            Err(STATUS_OBJECT_NAME_NOT_FOUND) => {
                // Creating anything is refused; opening a missing name is not found.
                return Err(if create_disposition == FILE_OPEN {
                    STATUS_OBJECT_NAME_NOT_FOUND
                } else {
                    STATUS_MEDIA_WRITE_PROTECTED
                });
            }
            Err(other) => return Err(other),
        };
        match create_disposition {
            FILE_OPEN | FILE_OPEN_IF => {}
            FILE_CREATE => return Err(STATUS_OBJECT_NAME_COLLISION),
            _ => return Err(STATUS_MEDIA_WRITE_PROTECTED),
        }
        let dir = attr.kind == NodeKind::Dir;
        if dir && create_options & FILE_NON_DIRECTORY_FILE != 0 {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        if !dir && create_options & FILE_DIRECTORY_FILE != 0 {
            return Err(STATUS_NOT_A_DIRECTORY);
        }
        if create_options & FILE_DELETE_ON_CLOSE != 0
            || (!dir && desired_access & (FILE_WRITE_DATA | FILE_APPEND_DATA) != 0)
        {
            return Err(STATUS_ACCESS_DENIED);
        }
        Ok(CreateFileInfo {
            context: Open { ino: attr.ino },
            is_dir: dir,
            new_file_created: false,
        })
    }

    fn read_file(
        &'h self,
        _file_name: &U16CStr,
        offset: i64,
        buffer: &mut [u8],
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<u32> {
        let offset = u64::try_from(offset).map_err(|_| STATUS_INVALID_PARAMETER)?;
        let size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let bytes = self
            .handle
            .block_on(self.core.read(context.ino, offset, size))
            .map_err(|e| status(&e))?;
        let len = bytes.len().min(buffer.len());
        buffer[..len].copy_from_slice(&bytes[..len]);
        Ok(u32::try_from(len).unwrap_or(u32::MAX))
    }

    fn get_file_information(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<FileInfo> {
        let attr = self.core.getattr(context.ino).map_err(|e| status(&e))?;
        Ok(Self::info_of(&attr))
    }

    fn find_files(
        &'h self,
        _file_name: &U16CStr,
        mut fill_find_data: impl FnMut(&FindData) -> FillDataResult,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<()> {
        let entries = self
            .handle
            .block_on(self.core.readdir(context.ino))
            .map_err(|e| status(&e))?;
        for entry in entries {
            if !target::windows_name_ok(&entry.name) {
                continue;
            }
            let Ok(attr) = self.core.getattr(entry.ino) else {
                continue;
            };
            let Ok(file_name) = U16CString::from_str(&entry.name) else {
                continue;
            };
            let time = time_of(&attr);
            let data = FindData {
                attributes: target::attributes(attr.kind),
                creation_time: time,
                last_access_time: time,
                last_write_time: time,
                file_size: attr.size,
                file_name,
            };
            match fill_find_data(&data) {
                Ok(()) | Err(FillDataError::NameTooLong) => {}
                Err(FillDataError::BufferFull) => {
                    return Err(winapi::shared::ntstatus::STATUS_BUFFER_OVERFLOW);
                }
            }
        }
        Ok(())
    }

    fn get_disk_free_space(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<DiskSpaceInfo> {
        Ok(DiskSpaceInfo {
            byte_count: 1 << 40,
            free_byte_count: 0,
            available_byte_count: 0,
        })
    }

    fn get_volume_information(
        &'h self,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<VolumeInfo> {
        let label: String = format!("s3 {}", self.bucket).chars().take(32).collect();
        Ok(VolumeInfo {
            name: U16CString::from_str(label).map_err(|_| STATUS_INVALID_PARAMETER)?,
            serial_number: 0x4242_5230,
            max_component_length: 255,
            fs_flags: FILE_CASE_SENSITIVE_SEARCH | FILE_CASE_PRESERVED_NAMES | FILE_UNICODE_ON_DISK,
            fs_name: U16CString::from_str("BucketBridge").map_err(|_| STATUS_INVALID_PARAMETER)?,
        })
    }

    fn mounted(
        &'h self,
        _mount_point: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<()> {
        Ok(())
    }

    fn unmounted(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<()> {
        self.closed.store(true, Ordering::Release);
        Ok(())
    }

    fn write_file(
        &'h self,
        _file_name: &U16CStr,
        _offset: i64,
        _buffer: &[u8],
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<u32> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn flush_file_buffers(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Ok(())
    }

    fn set_file_attributes(
        &'h self,
        _file_name: &U16CStr,
        _file_attributes: u32,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn set_file_time(
        &'h self,
        _file_name: &U16CStr,
        _creation_time: FileTimeOperation,
        _last_access_time: FileTimeOperation,
        _last_write_time: FileTimeOperation,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn delete_file(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn delete_directory(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn move_file(
        &'h self,
        _file_name: &U16CStr,
        _new_file_name: &U16CStr,
        _replace_if_existing: bool,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn set_end_of_file(
        &'h self,
        _file_name: &U16CStr,
        _offset: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }

    fn set_allocation_size(
        &'h self,
        _file_name: &U16CStr,
        _alloc_size: i64,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        Err(STATUS_MEDIA_WRITE_PROTECTED)
    }
}
