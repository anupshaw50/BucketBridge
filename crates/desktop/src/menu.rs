//! Native menu bar (macOS only).
//!
//! On macOS the web view only receives the standard editing shortcuts
//! (Cmd+C, Cmd+V, Cmd+X, Cmd+A, Cmd+Z, Shift+Cmd+Z) when the application menu
//! contains the matching Edit items, so the app installs a small menu built from
//! predefined items. Windows and Linux handle these shortcuts inside the web
//! view without a menu, so no menu bar is installed there.

use tauri::menu::{AboutMetadata, Menu, MenuBuilder, SubmenuBuilder};
use tauri::{AppHandle, Runtime};

/// Builds the application menu: app menu (About, Services, Hide, Quit),
/// Edit, and Window.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let package = app.package_info();
    let about = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        ..AboutMetadata::default()
    };

    let app_menu = SubmenuBuilder::new(app, &package.name)
        .about(Some(about))
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;

    MenuBuilder::new(app)
        .items(&[&app_menu, &edit_menu, &window_menu])
        .build()
}
