//! The `relay_drag` command: a drag of objects that leaves one main window
//! and ends over another (docs/desktop-api.md, "Drag and drop").
//!
//! A web view cannot hand a drag to another window of the app, so the window
//! that holds the pointer reports it here. The command finds the main window
//! under the screen cursor and sends it `drag://incoming` with the position
//! in that window; the other windows are told that the drag left them. The
//! receiving window decides what the drop means. Nothing is read or changed
//! here: the command only forwards a small message between windows.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

use super::CommandResult;
use crate::MAIN_WINDOW;
use crate::error::CommandError;
use bucketbridge_core::Error;

/// The window message of a drag that reaches another window.
pub const DRAG_INCOMING: &str = "drag://incoming";

/// Most keys one drag may carry.
const MAX_KEYS: usize = 10_000;

/// The objects being dragged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DragItems {
    account_id: String,
    bucket: String,
    keys: Vec<String>,
}

/// What a window that is not the source receives.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DragIncoming {
    phase: &'static str,
    /// Position in the receiving window, in CSS pixels from its top left.
    x: f64,
    y: f64,
    items: Option<DragItems>,
    /// The move key was held: move instead of copy.
    #[serde(rename = "move")]
    move_items: bool,
}

/// Where a window is on the screen, in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// The window other than `exclude` whose client area holds the screen point
/// `cursor`, with the point in CSS pixels of that window.
pub fn window_at(
    windows: &[Placed],
    cursor: (f64, f64),
    exclude: &str,
) -> Option<(String, f64, f64)> {
    windows
        .iter()
        .filter(|window| window.label != exclude && window.scale > 0.0)
        .find(|window| {
            cursor.0 >= window.x
                && cursor.0 < window.x + window.width
                && cursor.1 >= window.y
                && cursor.1 < window.y + window.height
        })
        .map(|window| {
            (
                window.label.clone(),
                (cursor.0 - window.x) / window.scale,
                (cursor.1 - window.y) / window.scale,
            )
        })
}

fn is_main(label: &str) -> bool {
    label == MAIN_WINDOW || label.starts_with("main-")
}

fn usage(message: &str) -> CommandError {
    CommandError::from(Error::Usage(message.to_owned()))
}

/// Reports where a drag is. `phase` is `"over"` (the pointer moved), `"drop"`
/// (the button was released) or `"leave"` (the drag ended without a drop or
/// came back into the calling window). `over` and `drop` need `items`.
/// Returns whether another main window is under the screen cursor and was
/// told: after a `drop` that returns true the other window carries it out.
/// Windows that are not under the cursor are told `leave`.
#[tauri::command(rename_all = "snake_case")]
pub async fn relay_drag<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    phase: String,
    items: Option<DragItems>,
    move_items: bool,
) -> CommandResult<bool> {
    let phase: &'static str = match phase.as_str() {
        "over" => "over",
        "drop" => "drop",
        "leave" => "leave",
        _ => return Err(usage("phase must be over, drop or leave")),
    };
    if phase != "leave" {
        let Some(items) = &items else {
            return Err(usage("a drag without items cannot be relayed"));
        };
        if items.keys.is_empty() || items.keys.len() > MAX_KEYS {
            return Err(usage("a drag carries between 1 and 10000 keys"));
        }
    }
    let placed: Vec<Placed> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| is_main(label))
        .filter_map(|(label, other)| {
            let position = other.inner_position().ok()?;
            let size = other.inner_size().ok()?;
            Some(Placed {
                label,
                x: f64::from(position.x),
                y: f64::from(position.y),
                width: f64::from(size.width),
                height: f64::from(size.height),
                scale: other.scale_factor().ok()?,
            })
        })
        .collect();
    let target = if phase == "leave" {
        None
    } else {
        app.cursor_position()
            .ok()
            .and_then(|cursor| window_at(&placed, (cursor.x, cursor.y), window.label()))
    };
    for other in &placed {
        if other.label == window.label() {
            continue;
        }
        let message = match &target {
            Some((label, x, y)) if *label == other.label => DragIncoming {
                phase,
                x: *x,
                y: *y,
                items: items.clone(),
                move_items,
            },
            _ => DragIncoming {
                phase: "leave",
                x: 0.0,
                y: 0.0,
                items: None,
                move_items: false,
            },
        };
        // An emit only fails while the app shuts down.
        let _ = app.emit_to(other.label.as_str(), DRAG_INCOMING, message);
    }
    Ok(target.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(label: &str, x: f64, y: f64, width: f64, height: f64, scale: f64) -> Placed {
        Placed {
            label: label.to_owned(),
            x,
            y,
            width,
            height,
            scale,
        }
    }

    #[test]
    fn the_window_under_the_cursor_gets_css_pixels() {
        let windows = [
            placed("main", 0.0, 0.0, 2000.0, 1200.0, 2.0),
            placed("main-1", 2000.0, 100.0, 1000.0, 800.0, 1.0),
        ];
        assert_eq!(
            window_at(&windows, (2250.0, 300.0), "main"),
            Some(("main-1".into(), 250.0, 200.0))
        );
        // The calling window is never the target.
        assert_eq!(window_at(&windows, (100.0, 100.0), "main"), None);
        assert_eq!(
            window_at(&windows, (100.0, 100.0), "main-1"),
            Some(("main".into(), 50.0, 50.0))
        );
        // Edges: the left and top edges are inside, the right and bottom are not.
        assert!(window_at(&windows, (2000.0, 100.0), "main").is_some());
        assert_eq!(window_at(&windows, (3000.0, 300.0), "main"), None);
        assert_eq!(window_at(&windows, (2500.0, 900.0), "main"), None);
        assert_eq!(window_at(&windows, (5000.0, 5000.0), "main"), None);
    }

    #[test]
    fn a_window_without_a_scale_is_skipped() {
        let windows = [placed("main-1", 0.0, 0.0, 100.0, 100.0, 0.0)];
        assert_eq!(window_at(&windows, (10.0, 10.0), "main"), None);
    }

    #[test]
    fn only_main_windows_take_part() {
        assert!(is_main("main"));
        assert!(is_main("main-3"));
        assert!(!is_main("mainframe"));
        assert!(!is_main("other"));
    }

    #[test]
    fn the_message_has_the_documented_shape() {
        let message = DragIncoming {
            phase: "drop",
            x: 1.5,
            y: 2.0,
            items: Some(DragItems {
                account_id: "a".into(),
                bucket: "b".into(),
                keys: vec!["k/".into()],
            }),
            move_items: true,
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            serde_json::json!({
                "phase": "drop",
                "x": 1.5,
                "y": 2.0,
                "items": { "account_id": "a", "bucket": "b", "keys": ["k/"] },
                "move": true
            })
        );
    }
}
