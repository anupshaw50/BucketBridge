//! The `open_new_window` command: another main window over the same core.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Deserialize;
use tauri::{
    AppHandle, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use super::CommandResult;
use crate::MAIN_WINDOW;
use crate::error::CommandError;
use bucketbridge_core::Error;

/// Query keys the UI reads at startup to open a location.
const QUERY_ACCOUNT: &str = "bb_account";
const QUERY_BUCKET: &str = "bb_bucket";
const QUERY_PREFIX: &str = "bb_prefix";

/// Size and minimum size of a window that cannot copy the caller's.
const DEFAULT_SIZE: (f64, f64) = (1280.0, 800.0);
const MIN_SIZE: (f64, f64) = (960.0, 600.0);
/// How far a new window is shifted from the one it was opened from, so that
/// it does not cover it exactly.
const CASCADE_OFFSET: i32 = 32;

/// Counter behind the labels `main-1`, `main-2`, ...
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);

/// Where the new window opens.
#[derive(Debug, Deserialize)]
pub struct WindowLocation {
    account_id: String,
    bucket: String,
    #[serde(default)]
    prefix: String,
}

/// Opens another main window and returns its label. The windows share the
/// session, queue and stores of the app (they are one process), and every
/// event reaches every window. With `location` the new window opens that
/// folder of that account. It has the size of the calling window and sits a
/// little below and to the right of it. Closing a window never stops the
/// app; the last window closing does.
#[tauri::command(rename_all = "snake_case")]
pub async fn open_new_window<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    location: Option<WindowLocation>,
) -> CommandResult<String> {
    let label = free_label(&app);
    let url = WebviewUrl::App(PathBuf::from(start_page(location.as_ref())));
    let mut builder = WebviewWindowBuilder::new(&app, &label, url)
        .title("BucketBridge")
        .min_inner_size(MIN_SIZE.0, MIN_SIZE.1)
        .resizable(true)
        .prevent_overflow()
        .inner_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Ok(size) = window.inner_size() {
        let logical = size.to_logical::<f64>(scale);
        builder = builder.inner_size(
            logical.width.max(MIN_SIZE.0),
            logical.height.max(MIN_SIZE.1),
        );
    }
    match window.outer_position() {
        Ok(PhysicalPosition { x, y }) => {
            let offset = f64::from(CASCADE_OFFSET);
            builder =
                builder.position(f64::from(x) / scale + offset, f64::from(y) / scale + offset);
        }
        Err(_) => builder = builder.center(),
    }
    builder.build().map_err(|error| {
        CommandError::from(Error::Internal(format!(
            "the new window could not be opened: {error}"
        )))
    })?;
    Ok(label)
}

/// The first `main-N` label no window uses.
fn free_label<R: Runtime>(app: &AppHandle<R>) -> String {
    loop {
        let label = format!(
            "{MAIN_WINDOW}-{}",
            NEXT_WINDOW.fetch_add(1, Ordering::Relaxed)
        );
        if app.get_webview_window(&label).is_none() {
            return label;
        }
    }
}

/// `index.html`, with the location in the query when there is one.
fn start_page(location: Option<&WindowLocation>) -> String {
    match location {
        None => "index.html".to_owned(),
        Some(at) => format!(
            "index.html?{QUERY_ACCOUNT}={}&{QUERY_BUCKET}={}&{QUERY_PREFIX}={}",
            encode(&at.account_id),
            encode(&at.bucket),
            encode(&at.prefix)
        ),
    }
}

/// Percent-encodes everything but the unreserved characters of RFC 3986.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_page_carries_an_encoded_location() {
        assert_eq!(start_page(None), "index.html");
        let at = WindowLocation {
            account_id: "a-1".into(),
            bucket: "data".into(),
            prefix: "my docs/\u{e9}&x=1/".into(),
        };
        assert_eq!(
            start_page(Some(&at)),
            "index.html?bb_account=a-1&bb_bucket=data&bb_prefix=my%20docs%2F%C3%A9%26x%3D1%2F"
        );
    }
}
