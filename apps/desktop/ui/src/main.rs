// SPDX-License-Identifier: Apache-2.0

//! QSL desktop UI entry point.
//!
//! Compiled to WASM by `dx build --platform web`, bundled into
//! `apps/desktop/ui/dist/`, and served by the Tauri shell. On non-wasm
//! hosts `main` is a no-op so `cargo check --workspace` stays clean
//! across the workspace without every contributor needing the wasm32
//! target installed.
//!
//! # Phase 0 Week 5 part 1 scope
//!
//! Proof of life only: render "Hello from QSL" and invoke the
//! `accounts_list` Tauri command. Layout, sidebar, and message list
//! components land in Week 5 part 2 once the IPC surface is wider.

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod format;
#[cfg(target_arch = "wasm32")]
mod keyboard;
#[cfg(target_arch = "wasm32")]
mod oauth_add;
#[cfg(target_arch = "wasm32")]
mod reader_only;
#[cfg(target_arch = "wasm32")]
mod reply;
#[cfg(target_arch = "wasm32")]
mod settings;
#[cfg(target_arch = "wasm32")]
mod threading;

#[cfg(target_arch = "wasm32")]
fn main() {
    // Route `tracing::*!` calls into the browser's DevTools console.
    // The shell side already initializes `qsl-telemetry`'s stderr
    // subscriber; this is the UI-side counterpart so a single
    // `RUST_LOG=info`-style filter (statically capped here at INFO)
    // surfaces both halves of an operation.
    let cfg = tracing_wasm::WASMLayerConfigBuilder::new()
        .set_max_level(tracing::Level::INFO)
        .build();
    tracing_wasm::set_as_global_default_with_config(cfg);
    tracing::info!("qsl ui: wasm bundle loaded");

    dioxus::launch(app::App);
}

// `format`, `reply`, `threading`, and `keyboard` are plain
// `chrono` / IPC-type / pure-fn logic with no wasm-only deps, so
// keep their tests reachable from `cargo test` on the host.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod format;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod keyboard;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod reply;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod threading;

// On non-wasm targets (the default `cargo check` host), the binary is
// empty. `dx build --platform web` is the only supported way to
// produce a runnable artifact.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
