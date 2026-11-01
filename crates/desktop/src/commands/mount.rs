//! Drive mount commands (docs/desktop-api.md, "Mount as drive (T04)").
//!
//! A thin layer over `bucketbridge_core::mount`. Builds without the `mount`
//! cargo feature answer `mount_status` with `supported: false` and reject
//! `mount_bucket` with a `usage` error, so the UI never offers the action.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use bucketbridge_core::Error;
use bucketbridge_core::mount::{self, target};
use bucketbridge_core::s3uri::validate_bucket_name;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

use super::{CommandResult, logged, require_absolute};
use crate::state::AppState;

/// `{ mounts }` after a mount or an unmount.
pub const MOUNT_CHANGED: &str = "mount://changed";

/// One active mount.
#[derive(Debug, Clone, Serialize)]
pub struct MountInfo {
    /// Account the bucket belongs to.
    pub account_id: String,
    /// Mounted bucket.
    pub bucket: String,
    /// Mounted prefix; empty for the whole bucket.
    pub prefix: String,
    /// Absolute folder it is mounted on.
    pub mount_point: String,
}

/// Answer of `mount_status`.
#[derive(Debug, Clone, Serialize)]
pub struct MountStatus {
    /// This build can mount (the `mount` feature on Linux, macOS or Windows).
    pub supported: bool,
    /// The filesystem driver is installed on this machine: FUSE (macFUSE,
    /// FUSE-T, `fusermount`) or Dokan on Windows.
    pub fuse_installed: bool,
    /// `linux`, `macos`, `windows` or another `std::env::consts::OS` value.
    pub platform: String,
    /// Windows: drive letters that are free now (`["D", "E", ...]`); empty
    /// elsewhere.
    pub drive_letters: Vec<String>,
    /// Active mounts.
    pub mounts: Vec<MountInfo>,
}

/// Payload of `mount://changed`.
#[derive(Clone, Serialize)]
struct MountsChanged {
    mounts: Vec<MountInfo>,
}

struct Active {
    info: MountInfo,
    #[cfg(feature = "mount")]
    handle: mount::backend::MountHandle,
}

/// The mounts of this app run; dropping it unmounts them all.
#[derive(Default)]
pub struct MountRegistry {
    active: Mutex<HashMap<String, Active>>,
}

impl MountRegistry {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forgets mounts whose session ended (unmounted from outside) and lists
    /// the rest.
    fn list(&self) -> Vec<MountInfo> {
        #[allow(unused_mut)] // only the mount feature prunes ended sessions
        let mut active = self.lock();
        #[cfg(feature = "mount")]
        active.retain(|_, a| !a.handle.is_finished());
        let mut mounts: Vec<MountInfo> = active.values().map(|a| a.info.clone()).collect();
        mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
        mounts
    }

    /// Unmounts everything (app exit).
    pub fn clear(&self) {
        self.lock().clear();
    }
}

fn status(registry: &MountRegistry) -> MountStatus {
    MountStatus {
        supported: mount::supported(),
        fuse_installed: mount::fuse_installed(),
        platform: std::env::consts::OS.to_owned(),
        drive_letters: if mount::supported() {
            mount::free_drive_letters()
                .into_iter()
                .map(String::from)
                .collect()
        } else {
            Vec::new()
        },
        mounts: registry.list(),
    }
}

/// Whether mounting works here, and the active mounts.
#[tauri::command(rename_all = "snake_case")]
pub async fn mount_status(state: State<'_, AppState>) -> CommandResult<MountStatus> {
    let core = state.core()?;
    Ok(status(&core.mounts))
}

/// Mounts a bucket (or the `prefix` of it) read-only on `mount_point`.
#[tauri::command(rename_all = "snake_case")]
pub async fn mount_bucket<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    prefix: Option<String>,
    mount_point: String,
) -> CommandResult<MountInfo> {
    let core = state.core()?;
    let info = logged("mounting the bucket", async {
        validate_bucket_name(&bucket)?;
        // A drive letter (Windows) is normalised to `X:`; everything else is a folder.
        let mount_point = if cfg!(windows) {
            target::MountTarget::parse(&mount_point)?.display()
        } else {
            require_absolute("the mount point", &mount_point)?;
            mount_point
        };
        if !mount::supported() {
            return Err(Error::Usage(
                "this build of BucketBridge cannot mount buckets (build with the mount feature)"
                    .into(),
            ));
        }
        if !mount::fuse_installed() {
            return Err(Error::Usage(mount::driver_hint().into()));
        }
        let path = std::path::Path::new(&mount_point);
        let is_drive = cfg!(windows) && target::parse_drive_letter(&mount_point).is_some();
        if !is_drive && !path.is_dir() {
            return Err(Error::Usage(format!(
                "the mount point {mount_point} is not an existing folder"
            )));
        }
        if core.mounts.lock().contains_key(&mount_point) {
            return Err(Error::Conflict(format!("{mount_point} is already a mount")));
        }
        let s3 = core.session.s3_for(&account_id).await?;
        let prefix = prefix.unwrap_or_default();
        let info = MountInfo {
            account_id,
            bucket: bucket.clone(),
            prefix: prefix.clone(),
            mount_point: mount_point.clone(),
        };
        #[cfg(feature = "mount")]
        {
            let fs = std::sync::Arc::new(mount::MountCore::new(
                s3,
                &bucket,
                &prefix,
                mount::MountOptions::default(),
            ));
            let runtime = tokio::runtime::Handle::current();
            let owned = std::path::PathBuf::from(&mount_point);
            let handle = tauri::async_runtime::spawn_blocking(move || {
                mount::backend::mount(fs, &owned, runtime)
            })
            .await
            .map_err(|err| Error::Internal(format!("a background task failed: {err}")))??;
            core.mounts.lock().insert(
                mount_point.clone(),
                Active {
                    info: info.clone(),
                    handle,
                },
            );
            tracing::info!(
                target: "bucketbridge::desktop",
                "mounted s3://{bucket}/{prefix} on {mount_point}"
            );
        }
        #[cfg(not(feature = "mount"))]
        drop(s3);
        Ok(info)
    })
    .await?;
    let _ = app.emit(
        MOUNT_CHANGED,
        MountsChanged {
            mounts: core.mounts.list(),
        },
    );
    Ok(info)
}

/// Unmounts the mount on `mount_point`.
#[tauri::command(rename_all = "snake_case")]
pub async fn unmount_bucket<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    mount_point: String,
) -> CommandResult<()> {
    let core = state.core()?;
    logged("unmounting", async {
        let removed = core.mounts.lock().remove(&mount_point);
        let Some(active) = removed else {
            return Err(Error::NotFound(format!("{mount_point} is not a mount")));
        };
        #[cfg(feature = "mount")]
        {
            let result = tauri::async_runtime::spawn_blocking(move || active.handle.unmount())
                .await
                .map_err(|err| Error::Internal(format!("a background task failed: {err}")))?;
            result?;
        }
        #[cfg(not(feature = "mount"))]
        drop(active);
        tracing::info!(target: "bucketbridge::desktop", "unmounted {mount_point}");
        Ok(())
    })
    .await?;
    let _ = app.emit(
        MOUNT_CHANGED,
        MountsChanged {
            mounts: core.mounts.list(),
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_registry_lists_nothing() {
        let registry = MountRegistry::default();
        assert!(registry.list().is_empty());
        registry.clear();
    }

    #[test]
    fn status_reports_the_build() {
        let status = status(&MountRegistry::default());
        assert_eq!(status.supported, mount::supported());
        if !status.supported {
            assert!(!status.fuse_installed);
            assert!(status.drive_letters.is_empty());
        }
        assert_eq!(status.platform, std::env::consts::OS);
    }
}
