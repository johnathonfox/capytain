// SPDX-License-Identifier: Apache-2.0

//! Tauri build-time code generator.
//!
//! `tauri_build::build()` reads `tauri.conf.json`, validates it, generates
//! the `gen/` glue that `tauri::generate_context!()` consumes at compile
//! time, and wires up platform-specific metadata (Windows resources,
//! macOS Info.plist snippets, Linux desktop entry). Keep this file a
//! one-liner — all configuration lives in `tauri.conf.json`.

fn main() {
    tauri_build::build();
}
