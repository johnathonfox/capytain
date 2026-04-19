// SPDX-License-Identifier: Apache-2.0

//! Capytain desktop UI entry point.
//!
//! Compiled to WASM by `dx build --platform web`, bundled into
//! `apps/desktop/ui/dist/`, and served by the Tauri shell. On non-wasm
//! hosts `main` is a no-op so `cargo check --workspace` stays clean
//! across the workspace without every contributor needing the wasm32
//! target installed.
//!
//! # Phase 0 Week 5 part 1 scope
//!
//! Proof of life only: render "Hello from Capytain" and invoke the
//! `accounts_list` Tauri command. Layout, sidebar, and message list
//! components land in Week 5 part 2 once the IPC surface is wider.

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
fn main() {
    dioxus::launch(app::App);
}

// On non-wasm targets (the default `cargo check` host), the binary is
// empty. `dx build --platform web` is the only supported way to
// produce a runnable artifact.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
