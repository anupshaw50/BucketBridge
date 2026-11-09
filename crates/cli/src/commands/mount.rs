//! `bb mount`: a read-only mount of a bucket (T04; FUSE, or Dokan on
//! Windows). Only with the `mount` cargo feature.

use std::sync::Arc;

use bucketbridge_core::Error;
use bucketbridge_core::mount::{self, MountCore, MountOptions};

use crate::cli::MountArgs;
use crate::context::Context;
use crate::i18n::trf;
use crate::output::{Failure, Output};

/// Mounts the bucket and keeps it mounted until Ctrl-C or until the folder
/// is unmounted from outside (`umount`, `fusermount -u`, eject on Windows).
pub async fn run(ctx: &Context, args: &MountArgs, out: &mut Output) -> Result<(), Failure> {
    if !mount::fuse_installed() {
        return Err(Error::Usage(mount::driver_hint().into()).into());
    }
    // On Windows a free drive letter (`X:`) is a valid mount point too.
    let drive =
        cfg!(windows) && mount::target::parse_drive_letter(&args.dir.to_string_lossy()).is_some();
    if !drive && !args.dir.is_dir() {
        return Err(Error::Usage(trf(
            "{path} is not an existing folder",
            &[("path", &args.dir.display().to_string())],
        ))
        .into());
    }
    let core = Arc::new(MountCore::new(
        ctx.s3.clone(),
        &args.bucket,
        &args.prefix,
        MountOptions::default(),
    ));
    // Fail early (access denied, no such bucket) instead of mounting a folder
    // that only returns I/O errors.
    core.readdir(mount::ROOT_INODE).await?;
    let runtime = tokio::runtime::Handle::current();
    let dir = args.dir.clone();
    let handle = tokio::task::spawn_blocking(move || mount::backend::mount(core, &dir, runtime))
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))??;
    out.notice(&trf(
        "mounted s3://{bucket} on {path} (read-only); press Ctrl-C to unmount",
        &[
            ("bucket", &args.bucket),
            ("path", args.dir.display().to_string().as_str()),
        ],
    ));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                if handle.is_finished() {
                    return Ok(());
                }
            }
        }
    }
    tokio::task::spawn_blocking(move || handle.unmount())
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))??;
    Ok(())
}
