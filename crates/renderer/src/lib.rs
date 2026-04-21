// SPDX-License-Identifier: Apache-2.0

//! Servo-backed implementation of [`capytain_core::EmailRenderer`].
//!
//! # Status
//!
//! Pre-spike. The trait definition and test double ([`capytain_core::NullRenderer`])
//! are stable, but the Servo-backed implementation is still under construction —
//! Phase 0 Week 6 Days 2-4.
//!
//! See `docs/servo-composition.md` at the repo root for the pre-spike design,
//! including the current understanding of Servo's embedding API surface, the
//! three platform integration points (macOS `NSView`, Windows `HWND`, Linux
//! GTK widget), and the known pitfalls.
//!
//! # Feature flags
//!
//! - `servo` (off by default): compile the real Servo-backed renderer. When
//!   off, this crate exports nothing — consumers fall back to
//!   [`capytain_core::NullRenderer`] for tests and CLI paths.

#![cfg_attr(not(feature = "servo"), allow(dead_code))]

#[cfg(feature = "servo")]
mod servo;

#[cfg(feature = "servo")]
pub use servo::ServoRenderer;

// When the `servo` feature is off, we re-export the null renderer so
// downstream crates can depend on `capytain-renderer` and still have a
// working `EmailRenderer` implementation for compile checks and tests.
#[cfg(not(feature = "servo"))]
pub use capytain_core::NullRenderer;
