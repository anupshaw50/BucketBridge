//! The FUSE adapter of the read-only mount: a thin shell around
//! [`MountCore`]. Compiled only with the `mount` cargo feature.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, LockOwner,
    MountOption, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request,
};
use tokio::runtime::Handle;

use super::{Attr, MountCore, NodeKind, ROOT_INODE};
use crate::error::{Error, ErrorKind, Result};

/// Number of kernel request threads.
const THREADS: usize = 4;

/// A running mount. Dropping it unmounts.
pub struct MountHandle {
    session: Option<fuser::BackgroundSession>,
    mountpoint: PathBuf,
}

impl MountHandle {
    /// The folder the bucket is mounted on.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Whether the session ended (for example after `umount` from outside).
    pub fn is_finished(&self) -> bool {
        self.session.as_ref().is_none_or(|s| s.guard.is_finished())
    }

    /// Unmounts and waits for the session thread.
    pub fn unmount(mut self) -> Result<()> {
        match self.session.take() {
            Some(session) => session
                .umount_and_join()
                .map_err(|err| Error::io("unmounting", err)),
            None => Ok(()),
        }
    }
}

/// Mounts `core` on `mountpoint` (an existing, empty folder). `handle` is the
/// async runtime the S3 calls run on.
pub fn mount(core: Arc<MountCore>, mountpoint: &Path, handle: Handle) -> Result<MountHandle> {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::NoExec,
        MountOption::FSName(format!("s3:{}", core.bucket())),
        MountOption::Subtype("bucketbridge".to_owned()),
    ];
    config.n_threads = Some(THREADS);
    let fs = BucketFs { core, handle };
    let session = fuser::spawn_mount(fs, mountpoint, &config)
        .map_err(|err| Error::Usage(format!("cannot mount on {}: {err}", mountpoint.display())))?;
    Ok(MountHandle {
        session: Some(session),
        mountpoint: mountpoint.to_path_buf(),
    })
}

struct BucketFs {
    core: Arc<MountCore>,
    handle: Handle,
}

fn errno(err: &Error) -> Errno {
    match err.kind() {
        ErrorKind::NotFound => Errno::ENOENT,
        ErrorKind::Auth | ErrorKind::Locked | ErrorKind::MfaRequired => Errno::EACCES,
        ErrorKind::Usage => Errno::EINVAL,
        _ => Errno::EIO,
    }
}

fn file_attr(attr: &Attr, uid: u32, gid: u32) -> FileAttr {
    let modified = attr
        .modified
        .and_then(|t| u64::try_from(t.timestamp()).ok())
        .map_or(UNIX_EPOCH, |secs| UNIX_EPOCH + Duration::from_secs(secs));
    let dir = attr.kind == NodeKind::Dir;
    FileAttr {
        ino: INodeNo(attr.ino),
        size: attr.size,
        blocks: attr.size.div_ceil(512),
        atime: modified,
        mtime: modified,
        ctime: modified,
        crtime: modified,
        kind: if dir {
            FileType::Directory
        } else {
            FileType::RegularFile
        },
        perm: if dir { 0o555 } else { 0o444 },
        nlink: if dir { 2 } else { 1 },
        uid,
        gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn kind_of(kind: NodeKind) -> FileType {
    match kind {
        NodeKind::Dir => FileType::Directory,
        NodeKind::File => FileType::RegularFile,
    }
}

impl Filesystem for BucketFs {
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self
            .handle
            .block_on(self.core.lookup(u64::from(parent), name))
        {
            Ok(attr) => reply.entry(
                &self.core.options().attr_ttl,
                &file_attr(&attr, req.uid(), req.gid()),
                Generation(0),
            ),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn getattr(&self, req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.core.getattr(u64::from(ino)) {
            Ok(attr) => reply.attr(
                &self.core.options().attr_ttl,
                &file_attr(&attr, req.uid(), req.gid()),
            ),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        reply.opened(FileHandle(0), fuser::FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self
            .handle
            .block_on(self.core.read(u64::from(ino), offset, size))
        {
            Ok(bytes) => reply.data(&bytes),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let ino = u64::from(ino);
        let entries = match self.handle.block_on(self.core.readdir(ino)) {
            Ok(entries) => entries,
            Err(err) => {
                reply.error(errno(&err));
                return;
            }
        };
        let parent = self.core.parent(ino).unwrap_or(ROOT_INODE);
        let mut all = vec![
            (ino, FileType::Directory, ".".to_owned()),
            (parent, FileType::Directory, "..".to_owned()),
        ];
        all.extend(
            entries
                .into_iter()
                .map(|e| (e.ino, kind_of(e.kind), e.name)),
        );
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (index, (child, kind, name)) in all.into_iter().enumerate().skip(skip) {
            if reply.add(INodeNo(child), (index + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}
