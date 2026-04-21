// SPDX-License-Identifier: Apache-2.0

//! Servo-backed `EmailRenderer`. Under construction — see
//! `docs/servo-composition.md`.
//!
//! The skeleton below is deliberately minimal: it compiles, exposes the
//! shape the rest of the workspace will depend on, and carries `todo!()`
//! markers at every point where platform-specific work lands in Phase 0
//! Week 6 Days 2-4. Each `todo!()` panic is a concrete deliverable — the
//! PR that lands each platform's surface embedding should remove exactly
//! one of them.

use capytain_core::{EmailRenderer, RenderHandle, RenderPolicy};

/// Servo-backed renderer. Owns a Servo `WebView` (or the platform-specific
/// handle derived from one) and a link-click callback.
///
/// Construction is platform-specific and not part of the trait; see
/// `ServoRenderer::new_macos`, `new_windows`, `new_linux` once those land.
pub struct ServoRenderer {
    // Intentionally empty until Day 2. The platform-specific fields
    // (`NSView*`, `HWND`, `GtkWidget*`) will be held as opaque handles
    // behind a small platform-abstraction enum.
    _private: (),
}

impl EmailRenderer for ServoRenderer {
    fn render(&mut self, _sanitized_html: &str, _policy: RenderPolicy) -> RenderHandle {
        todo!("Phase 0 Week 6 Day 2+: wire Servo WebView::load_html")
    }

    fn on_link_click(&mut self, _cb: Box<dyn FnMut(url::Url) + Send + 'static>) {
        todo!("Phase 0 Week 6 Day 2+: intercept navigation events from Servo")
    }

    fn clear(&mut self) {
        todo!("Phase 0 Week 6 Day 2+: reset WebView state between messages")
    }

    fn destroy(&mut self) {
        todo!("Phase 0 Week 6 Day 2+: release native surface and shut down Servo")
    }
}
