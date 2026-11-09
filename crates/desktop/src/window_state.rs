//! Remembers the size, position and maximised state of the main window in
//! `window-state.json` (data folder) and restores them at the next start,
//! bringing a window that would be off-screen (a monitor was removed)
//! on screen.
//!
//! Positions and sizes are physical pixels. The geometry functions are pure.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::PoisonError;

use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};

/// File in the data folder.
pub const FILE_NAME: &str = "window-state.json";

/// The strip at the top of a window (the title bar) that must stay reachable.
const GRAB_HEIGHT: i64 = 40;
/// How much of that strip must be on a monitor, at least.
const GRAB_MIN_WIDTH: i64 = 100;
const GRAB_MIN_HEIGHT: i64 = 20;
/// Sizes outside this range are ignored as corrupt.
const SIZE_RANGE: std::ops::RangeInclusive<u32> = 200..=30_000;

/// The saved geometry of the window when it is not maximised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Left edge of the outer window.
    pub x: i32,
    /// Top edge of the outer window.
    pub y: i32,
    /// Inner width.
    pub width: u32,
    /// Inner height.
    pub height: u32,
    /// Whether the window was maximised.
    pub maximized: bool,
}

/// A monitor's area in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    /// Left edge.
    pub x: i64,
    /// Top edge.
    pub y: i64,
    /// Width.
    pub width: i64,
    /// Height.
    pub height: i64,
}

impl Area {
    fn overlap(self, x: i64, y: i64, width: i64, height: i64) -> (i64, i64) {
        let w = (self.x + self.width).min(x + width) - self.x.max(x);
        let h = (self.y + self.height).min(y + height) - self.y.max(y);
        (w.max(0), h.max(0))
    }
}

/// Whether enough of the title bar of `saved` is on one of `monitors` for
/// the user to grab it.
pub fn is_reachable(saved: &SavedWindow, monitors: &[Area]) -> bool {
    monitors.iter().any(|monitor| {
        let (w, h) = monitor.overlap(
            i64::from(saved.x),
            i64::from(saved.y),
            i64::from(saved.width),
            GRAB_HEIGHT,
        );
        w >= GRAB_MIN_WIDTH && h >= GRAB_MIN_HEIGHT
    })
}

/// `saved` made fit for the current monitors: unchanged when reachable,
/// otherwise centred on `primary` (or the first monitor) with a size that fits
/// it. `None` when no monitor is known (nothing can be checked).
pub fn fit(saved: SavedWindow, monitors: &[Area], primary: Option<Area>) -> Option<SavedWindow> {
    if monitors.is_empty() {
        return None;
    }
    if is_reachable(&saved, monitors) {
        return Some(saved);
    }
    let target = primary
        .filter(|p| monitors.contains(p))
        .unwrap_or(monitors[0]);
    let width = i64::from(saved.width).min(target.width).max(1);
    let height = i64::from(saved.height).min(target.height).max(1);
    let x = target.x + (target.width - width) / 2;
    let y = target.y + (target.height - height) / 2;
    Some(SavedWindow {
        x: i32::try_from(x).ok()?,
        y: i32::try_from(y).ok()?,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        maximized: saved.maximized,
    })
}

/// Reads the file; a missing, unreadable or implausible file gives `None`.
pub fn read(path: &Path) -> Option<SavedWindow> {
    let text = fs::read_to_string(path).ok()?;
    let saved: SavedWindow = serde_json::from_str(&text).ok()?;
    (SIZE_RANGE.contains(&saved.width) && SIZE_RANGE.contains(&saved.height)).then_some(saved)
}

/// Writes the file through a temporary file, so a crash never leaves half of
/// it.
pub fn write(path: &Path, saved: &SavedWindow) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(serde_json::to_string(saved)?.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

fn areas<R: Runtime>(window: &WebviewWindow<R>) -> (Vec<Area>, Option<Area>) {
    let to_area = |monitor: &tauri::Monitor| Area {
        x: i64::from(monitor.position().x),
        y: i64::from(monitor.position().y),
        width: i64::from(monitor.size().width),
        height: i64::from(monitor.size().height),
    };
    let monitors = window
        .available_monitors()
        .map(|list| list.iter().map(to_area).collect())
        .unwrap_or_default();
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| to_area(&monitor));
    (monitors, primary)
}

/// Restores `saved` on `window`, correcting an off-screen position.
pub(crate) fn apply<R: Runtime>(window: &WebviewWindow<R>, saved: SavedWindow) {
    let (monitors, primary) = areas(window);
    // Without a monitor list nothing can be checked; the saved values are
    // used as they are.
    let fitted = fit(saved, &monitors, primary).unwrap_or(saved);
    let _ = window.set_size(PhysicalSize::new(fitted.width, fitted.height));
    let _ = window.set_position(PhysicalPosition::new(fitted.x, fitted.y));
    if fitted.maximized {
        let _ = window.maximize();
    }
}

/// Moves a window that is off every monitor back on screen (used for a
/// window nothing was saved for).
pub(crate) fn ensure_on_screen<R: Runtime>(window: &WebviewWindow<R>) {
    let (Ok(position), Ok(size)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    let current = SavedWindow {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: false,
    };
    let (monitors, primary) = areas(window);
    if let Some(fitted) = fit(current, &monitors, primary)
        && fitted != current
    {
        let _ = window.set_size(PhysicalSize::new(fitted.width, fitted.height));
        let _ = window.set_position(PhysicalPosition::new(fitted.x, fitted.y));
    }
}

/// Records the geometry of `window` in `memory` after it moved or resized.
/// A minimised or full-screen window records nothing; a maximised one only
/// sets the flag, so the normal size comes back after un-maximising.
pub(crate) fn track<R: Runtime>(window: &WebviewWindow<R>, memory: &Mutex<Option<SavedWindow>>) {
    if window.is_minimized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false) {
        return;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let mut memory = memory.lock().unwrap_or_else(PoisonError::into_inner);
    if maximized && let Some(saved) = memory.as_mut() {
        saved.maximized = true;
        return;
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    *memory = Some(SavedWindow {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const SECOND: Area = Area {
        x: 1920,
        y: 0,
        width: 1280,
        height: 1024,
    };

    fn at(x: i32, y: i32, width: u32, height: u32) -> SavedWindow {
        SavedWindow {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn a_visible_window_is_kept() {
        let saved = at(100, 80, 1280, 800);
        assert_eq!(fit(saved, &[SCREEN], Some(SCREEN)), Some(saved));
        // Partly off the right edge, title bar still reachable.
        let edge = at(1800, 80, 1280, 800);
        assert_eq!(fit(edge, &[SCREEN], Some(SCREEN)), Some(edge));
        // On the second monitor.
        let second = at(2000, 100, 1000, 700);
        assert_eq!(fit(second, &[SCREEN, SECOND], Some(SCREEN)), Some(second));
    }

    #[test]
    fn a_window_on_a_removed_monitor_is_centred_on_the_primary() {
        let saved = at(2000, 100, 1000, 700);
        let fitted = fit(saved, &[SCREEN], Some(SCREEN)).unwrap();
        assert_eq!(fitted, at(460, 190, 1000, 700));
        assert!(is_reachable(&fitted, &[SCREEN]));
    }

    #[test]
    fn a_window_far_off_or_above_the_screen_is_brought_back() {
        for saved in [
            at(-5000, -5000, 1280, 800),
            at(100, -3000, 1280, 800),
            at(100, 5000, 1280, 800),
            at(-1270, 100, 1280, 800),
        ] {
            assert!(!is_reachable(&saved, &[SCREEN]), "{saved:?}");
            let fitted = fit(saved, &[SCREEN], None).unwrap();
            assert!(is_reachable(&fitted, &[SCREEN]), "{saved:?}");
        }
    }

    #[test]
    fn an_oversized_window_is_shrunk_to_the_monitor() {
        let saved = at(5000, 0, 4000, 3000);
        let fitted = fit(saved, &[SCREEN], Some(SCREEN)).unwrap();
        assert_eq!(fitted, at(0, 0, 1920, 1080));
    }

    #[test]
    fn the_maximised_flag_survives_a_correction() {
        let mut saved = at(9000, 9000, 1000, 700);
        saved.maximized = true;
        assert!(fit(saved, &[SCREEN], Some(SCREEN)).unwrap().maximized);
    }

    #[test]
    fn without_monitors_nothing_is_checked() {
        assert_eq!(fit(at(0, 0, 1000, 700), &[], None), None);
    }

    #[test]
    fn the_file_round_trips_and_bad_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(read(&path), None);
        let saved = SavedWindow {
            maximized: true,
            ..at(-20, 30, 1400, 900)
        };
        write(&path, &saved).unwrap();
        assert_eq!(read(&path), Some(saved));
        assert!(!path.with_extension("json.tmp").exists());
        fs::write(&path, "not json").unwrap();
        assert_eq!(read(&path), None);
        fs::write(
            &path,
            r#"{"x":0,"y":0,"width":1,"height":99999,"maximized":false}"#,
        )
        .unwrap();
        assert_eq!(read(&path), None);
    }
}
