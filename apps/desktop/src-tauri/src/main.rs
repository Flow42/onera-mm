//! Entry point for the Onera desktop application.
//!
//! Everything lives in the library half of this crate so that the compiled
//! smoke suite can build the same application the window does.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    onera_desktop::run();
}
