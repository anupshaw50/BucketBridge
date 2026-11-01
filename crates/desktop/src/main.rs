// Release builds on Windows use the GUI subsystem so no console window opens.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    bucketbridge_desktop::run()
}
