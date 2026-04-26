// SPDX-License-Identifier: Apache-2.0

//! Servo-backed implementation of [`capytain_core::EmailRenderer`].
//!
//! # Status
//!
//! Phase 0 Week 6 Day 2. The Linux embedding is validated on real hardware;
//! the macOS embedding is written to the design doc but unverified (the
//! implementer did not have a Mac). Windows lands in a follow-up PR.
//!
//! See `docs/servo-composition.md` for the pre-spike design — the three
//! platform integration points, the Servo 0.1.0 API surface we exercise,
//! and the known footguns documented in §6 (paint contract,
//! `surfman::error::Error` not implementing `std::error::Error`, thread
//! affinity on the main thread, etc.).
//!
//! # Feature flags
//!
//! - `servo` (default on): compile the real Servo-backed renderer. When
//!   off, this crate re-exports [`capytain_core::NullRenderer`] so
//!   downstream crates that consume `capytain-renderer` still have a
//!   working [`EmailRenderer`](capytain_core::EmailRenderer) without
//!   needing the Servo native toolchain installed.

#![cfg_attr(not(feature = "servo"), allow(dead_code))]

// Pure-Rust outbound-URL cleaner. Independent of the Servo
// feature so the cleaner is testable and reusable from anywhere
// in the workspace (mailcli, future Tauri compose-hover preview,
// etc.) without dragging the Servo native toolchain.
pub mod link_cleaner;

#[cfg(feature = "servo")]
mod servo;

#[cfg(feature = "servo")]
pub use servo::{
    apply_nvidia_wayland_workaround, forward_pointer_button_press, forward_pointer_button_release,
    forward_pointer_left_viewport, forward_pointer_move, render_html_to_image, CorpusRenderer,
    MainThreadDispatch, RendererError, ServoRenderer,
};

// When the `servo` feature is off, we re-export the null renderer so
// downstream crates can depend on `capytain-renderer` and still have a
// working `EmailRenderer` implementation for compile checks and tests.
#[cfg(not(feature = "servo"))]
pub use capytain_core::NullRenderer;
