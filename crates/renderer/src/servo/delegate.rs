// SPDX-License-Identifier: Apache-2.0

//! `WebViewDelegate` that wires Servo's webview callbacks to the
//! Capytain renderer — the narrow subset described in design doc §3.2
//! that actually exists on Servo 0.1.0's `WebViewDelegate` trait.
//!
//! Methods listed in the design doc that the 0.1.0 trait does *not*
//! expose (`allow_opening_webview`, `allow_navigation_request`,
//! `show_simple_dialog`, `request_file_picker`, `notify_pipeline_panic`)
//! aren't implemented here. Their job is either already done by the
//! default implementations on the trait, or covered by the other
//! methods below — for example, `request_navigation` already gives us
//! the navigation-gating seam that `allow_navigation_request` would
//! have provided.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use servo::{
    LoadStatus, NavigationRequest, PermissionRequest, RenderingContext, WebView,
    WebViewDelegate, WindowRenderingContext,
};

/// Shared slot for the caller-registered link-click callback.
///
/// Stored as an `Option` because callers register the callback after
/// construction, via `EmailRenderer::on_link_click`; a rendered page that
/// arrives before any callback is registered just silently drops the
/// link-click.
pub type LinkCb = Option<Box<dyn FnMut(url::Url) + Send + 'static>>;

/// The delegate Servo calls back into.
///
/// Constructed with handles to the shared rendering context (so
/// [`notify_new_frame_ready`](WebViewDelegate::notify_new_frame_ready)
/// can drive the paint + present cycle without reaching into the outer
/// renderer) and to the shared link-click callback slot.
pub struct CapytainDelegate {
    rendering_context: Rc<WindowRenderingContext>,
    link_cb: Arc<Mutex<LinkCb>>,
}

impl CapytainDelegate {
    pub fn new(
        rendering_context: Rc<WindowRenderingContext>,
        link_cb: Arc<Mutex<LinkCb>>,
    ) -> Self {
        Self {
            rendering_context,
            link_cb,
        }
    }
}

impl WebViewDelegate for CapytainDelegate {
    /// The paint contract documented in `docs/servo-composition.md` §6.1:
    /// when Servo signals a new frame, we must call `webview.paint()` or
    /// the back buffer never gets filled and the surface stays blank.
    fn notify_new_frame_ready(&self, webview: WebView) {
        webview.paint();
        // `paint()` fills the back buffer; `present()` swaps it to front.
        // §6.2 warns that present swaps with `PreserveBuffer::No`, which
        // matters for pixel readback paths (Day 5 corpus tests) but not
        // here — present immediately for interactive display.
        self.rendering_context.present();
    }

    /// Load-status transitions are useful as observation points; no
    /// behavioral hook is required for the Day 2 deliverable. Left as a
    /// traced log so the dev-loop can see frame settling.
    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        match status {
            LoadStatus::Started => tracing::debug!("servo: load started"),
            LoadStatus::HeadParsed => tracing::debug!("servo: <head> parsed"),
            LoadStatus::Complete => tracing::debug!("servo: load complete"),
        }
    }

    /// The link-click seam. Servo 0.1.0 calls this for every navigation
    /// the webview initiates (link click, form submit, `window.location`,
    /// etc.). We deny every navigation — the reader pane is a one-shot
    /// render, not a browser — but first surface the URL to the caller's
    /// [`EmailRenderer::on_link_click`](capytain_core::EmailRenderer::on_link_click)
    /// callback *if* it's a scheme we route (https + mailto).
    ///
    /// Per design doc §3.2, filtering to `https:` and `mailto:` happens
    /// here rather than downstream — other schemes (`file:`, `about:`,
    /// `javascript:`) are silently denied.
    fn request_navigation(&self, _webview: WebView, navigation_request: NavigationRequest) {
        let url = navigation_request.url.clone();
        let scheme = url.scheme();
        if scheme == "https" || scheme == "mailto" {
            if let Ok(mut slot) = self.link_cb.lock() {
                if let Some(cb) = slot.as_mut() {
                    cb(url);
                }
            }
        } else {
            tracing::debug!(%url, "servo: ignoring navigation with non-http(s)/mailto scheme");
        }
        // Whether we reported it upstream or not, the navigation does
        // not proceed inside the Servo WebView itself.
        navigation_request.deny();
    }

    /// Email content must never prompt for camera, microphone,
    /// geolocation, notifications, etc. Deny every permission request.
    fn request_permission(&self, _webview: WebView, request: PermissionRequest) {
        tracing::debug!(feature = ?request.feature(), "servo: denying permission request");
        request.deny();
    }

    /// Forward animation-state changes to `tracing` so we can see if
    /// email content unexpectedly keeps the event loop hot (e.g., a CSS
    /// animation in a promotional email). No behavioral change in
    /// Phase 0; we pump the loop at a fixed cadence regardless.
    fn notify_animating_changed(&self, _webview: WebView, animating: bool) {
        tracing::debug!(animating, "servo: animating state changed");
    }
}
