// SPDX-License-Identifier: Apache-2.0

//! The `EmailRenderer` trait — the one seam the rest of the workspace sees
//! for HTML rendering of email bodies.
//!
//! The real implementation (Servo-backed) lives in `crates/renderer` and is
//! loaded via feature flag. This module defines the trait itself plus a
//! [`NullRenderer`] used by tests and by any crate that needs to drive the
//! read path without spinning up a native webview.
//!
//! Trait shape is per `TRAITS.md` §EmailRenderer. Phase 0 Week 6 lands the
//! Servo-backed implementation across macOS / Windows / Linux; until then
//! downstream crates target the trait and use [`NullRenderer`] in tests.
//!
//! # Pipeline position
//!
//! The renderer sits at the end of the read pipeline:
//!
//! ```text
//! raw HTML  ──►  ammonia (sanitize)  ──►  adblock (filter-list)  ──►  EmailRenderer::render
//! ```
//!
//! Sanitization and filter-list matching happen *before* `render` is called.
//! The renderer takes sanitized, filtered input and renders it as-is. On
//! link clicks, the URL is passed to the registered callback *after* URL
//! cleaning (tracker stripping, redirect unwrapping) has already been
//! performed by the surrounding layer.

use serde::{Deserialize, Serialize};

/// The one abstraction for rendering a single email body into a native
/// surface.
///
/// Implementations hold a reference to the host window's native surface
/// (`NSView` on macOS, `HWND` on Windows, GTK widget on Linux) and paint
/// into it. The surface is attached at construction; the trait only covers
/// per-message lifecycle (`render`/`clear`) and final teardown (`destroy`).
///
/// # Lifecycle
///
/// - Construction attaches the renderer to a host surface (implementation-
///   specific; not part of the trait).
/// - `render` is called once per message displayed.
/// - `clear` is called between messages to drop the current render state.
/// - `destroy` is called once at shutdown. After `destroy`, no further
///   trait method may be called.
///
/// # Thread safety
///
/// The trait requires `Send` but not `Sync`. Most native webview APIs are
/// single-threaded per surface; callers are expected to drive a given
/// renderer instance from one thread (typically the Tauri main thread).
///
/// # Link clicks
///
/// The URL passed to the `on_link_click` callback has already been cleaned
/// (trackers stripped, redirects unwrapped) per `DESIGN.md` §4.5 layer 4.
/// The renderer just reports "user clicked this URL"; the surrounding code
/// is responsible for any further policy (e.g. opening the system browser).
pub trait EmailRenderer: Send {
    /// Render sanitized HTML into the renderer's surface. Returns a handle
    /// identifying this render; the handle is valid until the next `render`
    /// or `clear` call and is used by tests and diagnostics to correlate
    /// events with the render that produced them.
    fn render(&mut self, sanitized_html: &str, policy: RenderPolicy) -> RenderHandle;

    /// Register a callback fired when the user clicks a link in the rendered
    /// HTML. The URL has already been cleaned per `DESIGN.md` §4.5 layer 4.
    ///
    /// Only the most recently registered callback is active; registering a
    /// new one replaces the previous one.
    fn on_link_click(&mut self, cb: Box<dyn FnMut(url::Url) + Send + 'static>);

    /// Clear the current render. The next call to [`EmailRenderer::render`]
    /// creates a fresh surface state — nothing persists across renders.
    fn clear(&mut self);

    /// Tear down the renderer and release OS resources. After this call,
    /// the renderer must not be used.
    fn destroy(&mut self);
}

/// Policy knobs passed on every `render` call.
///
/// Fields are `pub` so callers can construct the struct with literal syntax;
/// new fields will be added over time by introducing them with a sensible
/// default via a helper constructor rather than `#[non_exhaustive]`.
#[derive(Debug, Clone)]
pub struct RenderPolicy {
    /// When `false`, external images are replaced with placeholders and
    /// their URLs are never fetched. When `true`, external images load
    /// subject to the filter-list pass that already ran upstream.
    pub allow_remote_images: bool,

    /// Signal from the allow-list pass upstream. Used by filter-list rules
    /// that distinguish "trusted sender, looser rules" from the default.
    /// Even when `true`, filter-list hits still block.
    pub sender_is_trusted: bool,

    /// Color scheme to report to the rendered document via the
    /// `prefers-color-scheme` media query and the `color-scheme` CSS
    /// property.
    pub color_scheme: ColorScheme,
}

impl RenderPolicy {
    /// Safe-by-default policy: remote images blocked, sender untrusted,
    /// light color scheme.
    pub fn strict() -> Self {
        Self {
            allow_remote_images: false,
            sender_is_trusted: false,
            color_scheme: ColorScheme::Light,
        }
    }
}

/// The color scheme to apply to the rendered document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ColorScheme {
    Light,
    Dark,
}

/// Opaque handle to a single `render` call. Currently a monotonically
/// increasing counter; callers must not depend on the numeric value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderHandle(pub u64);

// ---------------------------------------------------------------------------
// NullRenderer — test double
// ---------------------------------------------------------------------------

/// A no-op [`EmailRenderer`] used in tests and by any crate that needs to
/// drive the read path without a native webview attached.
///
/// Call history is kept in memory:
///
/// - [`NullRenderer::renders`] returns the `(html, policy)` pairs passed to
///   [`EmailRenderer::render`], in order.
/// - [`NullRenderer::clear_count`] returns the number of `clear` calls.
/// - [`NullRenderer::was_destroyed`] returns whether `destroy` has been
///   called.
/// - [`NullRenderer::fire_link_click`] invokes the registered callback with
///   a supplied URL, letting tests exercise the link-click path without a
///   real webview.
pub struct NullRenderer {
    rendered: Vec<(String, RenderPolicy)>,
    clears: usize,
    destroyed: bool,
    next_handle: u64,
    link_cb: Option<Box<dyn FnMut(url::Url) + Send + 'static>>,
}

impl NullRenderer {
    /// Construct a fresh null renderer with no recorded calls.
    pub fn new() -> Self {
        Self {
            rendered: Vec::new(),
            clears: 0,
            destroyed: false,
            next_handle: 0,
            link_cb: None,
        }
    }

    /// Return the sequence of (html, policy) pairs passed to `render`.
    pub fn renders(&self) -> &[(String, RenderPolicy)] {
        &self.rendered
    }

    /// Return the number of times `clear` was called.
    pub fn clear_count(&self) -> usize {
        self.clears
    }

    /// Return whether `destroy` has been called.
    pub fn was_destroyed(&self) -> bool {
        self.destroyed
    }

    /// Invoke the registered link-click callback, if any, with `url`.
    /// No-op when no callback is registered.
    pub fn fire_link_click(&mut self, url: url::Url) {
        if let Some(cb) = self.link_cb.as_mut() {
            cb(url);
        }
    }
}

impl Default for NullRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl EmailRenderer for NullRenderer {
    fn render(&mut self, sanitized_html: &str, policy: RenderPolicy) -> RenderHandle {
        let handle = RenderHandle(self.next_handle);
        self.next_handle += 1;
        self.rendered.push((sanitized_html.to_owned(), policy));
        handle
    }

    fn on_link_click(&mut self, cb: Box<dyn FnMut(url::Url) + Send + 'static>) {
        self.link_cb = Some(cb);
    }

    fn clear(&mut self) {
        self.clears += 1;
    }

    fn destroy(&mut self) {
        self.destroyed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn null_renderer_records_renders_and_issues_monotonic_handles() {
        let mut r = NullRenderer::new();
        let h0 = r.render("<p>one</p>", RenderPolicy::strict());
        let h1 = r.render(
            "<p>two</p>",
            RenderPolicy {
                allow_remote_images: true,
                sender_is_trusted: true,
                color_scheme: ColorScheme::Dark,
            },
        );

        assert_ne!(h0, h1);
        assert_eq!(h0, RenderHandle(0));
        assert_eq!(h1, RenderHandle(1));

        let renders = r.renders();
        assert_eq!(renders.len(), 2);
        assert_eq!(renders[0].0, "<p>one</p>");
        assert!(!renders[0].1.allow_remote_images);
        assert_eq!(renders[1].0, "<p>two</p>");
        assert!(renders[1].1.allow_remote_images);
        assert_eq!(renders[1].1.color_scheme, ColorScheme::Dark);
    }

    #[test]
    fn null_renderer_tracks_clear_and_destroy() {
        let mut r = NullRenderer::new();
        assert_eq!(r.clear_count(), 0);
        assert!(!r.was_destroyed());

        r.clear();
        r.clear();
        r.destroy();

        assert_eq!(r.clear_count(), 2);
        assert!(r.was_destroyed());
    }

    #[test]
    fn link_click_callback_is_invoked_with_the_cleaned_url() {
        let mut r = NullRenderer::new();
        let captured: Arc<Mutex<Vec<url::Url>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let captured = Arc::clone(&captured);
            r.on_link_click(Box::new(move |u| captured.lock().unwrap().push(u)));
        }

        let u1 = url::Url::parse("https://example.com/a").unwrap();
        let u2 = url::Url::parse("https://example.com/b").unwrap();
        r.fire_link_click(u1.clone());
        r.fire_link_click(u2.clone());

        let seen = captured.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], u1);
        assert_eq!(seen[1], u2);
    }

    #[test]
    fn replacing_the_callback_drops_the_previous_one() {
        let mut r = NullRenderer::new();

        let first_calls: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let second_calls: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

        {
            let first_calls = Arc::clone(&first_calls);
            r.on_link_click(Box::new(move |_| *first_calls.lock().unwrap() += 1));
        }
        {
            let second_calls = Arc::clone(&second_calls);
            r.on_link_click(Box::new(move |_| *second_calls.lock().unwrap() += 1));
        }

        r.fire_link_click(url::Url::parse("https://example.com/").unwrap());

        assert_eq!(*first_calls.lock().unwrap(), 0);
        assert_eq!(*second_calls.lock().unwrap(), 1);
    }

    #[test]
    fn color_scheme_round_trips_through_serde() {
        let light = serde_json::to_string(&ColorScheme::Light).unwrap();
        let dark = serde_json::to_string(&ColorScheme::Dark).unwrap();
        assert_eq!(light, "\"Light\"");
        assert_eq!(dark, "\"Dark\"");

        let back: ColorScheme = serde_json::from_str(&dark).unwrap();
        assert_eq!(back, ColorScheme::Dark);
    }

    /// Compile-time check that `NullRenderer` is usable through a `dyn`
    /// trait object — the shape every consumer uses it through.
    #[test]
    fn null_renderer_is_dyn_compatible() {
        let mut r: Box<dyn EmailRenderer> = Box::new(NullRenderer::new());
        let _h = r.render("", RenderPolicy::strict());
        r.clear();
        r.destroy();
    }
}
