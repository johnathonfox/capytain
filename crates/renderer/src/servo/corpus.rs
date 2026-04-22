// SPDX-License-Identifier: Apache-2.0

//! Headless corpus-rendering harness used by integration tests in
//! `crates/renderer/tests/corpus.rs`.
//!
//! This module is the Day 5 dual of the production `ServoRenderer`:
//! same `Preferences` gate (JS off in practice via the sanitizer, DOM
//! APIs locked down via `apply_reader_pane_preferences`), same delegate
//! shape, but a [`SoftwareRenderingContext`] instead of
//! `WindowRenderingContext`. That one swap is what makes the corpus
//! tests completely hardware-independent — `SoftwareRenderingContext`
//! renders into a CPU-side image buffer via osmesa and never touches
//! the native GL / Wayland / NVIDIA-EGL path the interactive renderer
//! stumbles on (see `docs/week-6-day-2-notes.md`).
//!
//! The public entry point is [`render_html_to_image`]. One
//! `CorpusRenderer` instance drives one Servo engine + one `WebView`;
//! callers typically hold a single instance and feed it fixture after
//! fixture via [`CorpusRenderer::render`].
//!
//! # Thread affinity
//!
//! Unlike the production `ServoRenderer`, this harness does NOT split
//! across threads — every call stays on the caller's thread. Servo
//! itself is still one-per-process, so integration tests must run
//! single-threaded (or share one renderer across all cases). The
//! `tests/corpus.rs` harness uses a single `#[test]` function to make
//! that sharing unambiguous.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use image::RgbaImage;
use servo::{
    EventLoopWaker, Preferences, Servo, ServoBuilder, SoftwareRenderingContext, WebView,
    WebViewBuilder, WebViewDelegate,
};

use super::delegate::CorpusDelegate;
use super::RendererError;

/// Render HTML source into an RgbaImage of the given size, using a
/// freshly-constructed Servo engine. Convenience wrapper around
/// [`CorpusRenderer`]; callers that render more than one document in a
/// single process MUST reuse a single `CorpusRenderer` instead, since
/// Servo enforces one engine per process.
pub fn render_html_to_image(
    html: &str,
    size: PhysicalSize<u32>,
) -> Result<RgbaImage, RendererError> {
    CorpusRenderer::new(size)?.render(html)
}

/// Long-lived corpus renderer. Holds one Servo instance + one WebView
/// in thread-local `Rc`s (no cross-thread marshaling needed in tests).
pub struct CorpusRenderer {
    _ctx: Rc<SoftwareRenderingContext>,
    servo: Rc<Servo>,
    webview: WebView,
}

impl CorpusRenderer {
    /// Build a new corpus renderer at the given pixel size. Servo is
    /// long-lived; every subsequent `render` call reuses the same
    /// engine and WebView, just swapping the loaded document.
    pub fn new(size: PhysicalSize<u32>) -> Result<Self, RendererError> {
        let ctx = SoftwareRenderingContext::new(size)
            .map_err(|e| RendererError::RenderingContext(format!("{e:?}")))?;
        let ctx = Rc::new(ctx);

        // The context must be current on this thread before anything
        // else. Forgetting this is the "surface stays blank" failure
        // mode that the design doc's paint contract (§6.1) is the
        // *other* half of.
        servo::RenderingContext::make_current(ctx.as_ref())
            .map_err(|e| RendererError::RenderingContext(format!("make_current: {e:?}")))?;

        let mut prefs = Preferences::default();
        super::apply_reader_pane_preferences(&mut prefs);

        let waker: Box<dyn EventLoopWaker> = Box::new(NoopWaker);

        let servo = Rc::new(
            ServoBuilder::default()
                .preferences(prefs)
                .event_loop_waker(waker)
                .build(),
        );

        let delegate: Rc<dyn WebViewDelegate> = Rc::new(CorpusDelegate::new(Rc::clone(&ctx)));

        let webview = WebViewBuilder::new(&servo, ctx.clone())
            .delegate(delegate)
            .build();

        // WebViewBuilder doesn't take a size; the WebView defaults to
        // zero. Resize explicitly so layout has a viewport to measure
        // against — without this, `take_screenshot` waits forever
        // because the compositor has nothing to render.
        webview.resize(size);
        webview.focus();
        webview.show();

        let renderer = Self {
            _ctx: ctx,
            servo,
            webview,
        };

        // Warm the pipeline with a throwaway render. The first page
        // load through a fresh Servo engine hits a slow path where
        // `take_screenshot` can return the pre-layout background —
        // subsequent loads reuse the warmed font cache, constellation,
        // and script thread. Discarding the first render cleanly
        // converts that one-shot flakiness into predictable cost
        // amortized across one corpus run.
        let _ = renderer.render("<!DOCTYPE html><html><body>warmup</body></html>");

        Ok(renderer)
    }

    /// Render one HTML document and return its final RgbaImage.
    ///
    /// Uses `WebView::take_screenshot` which internally waits for all
    /// frames, late images, render-blocking stylesheets, and web fonts
    /// to settle (design doc §6.3) — so the result is stable, not the
    /// half-laid-out frame that a naive paint-then-read would yield.
    pub fn render(&self, html: &str) -> Result<RgbaImage, RendererError> {
        let data_url = super::make_data_url(html, capytain_core::ColorScheme::Light)
            .map_err(|e| RendererError::RenderingContext(format!("data url: {e}")))?;

        self.webview.load(data_url);

        // Pump the event loop while the load works through Servo's
        // pipeline. Without this, requesting a screenshot immediately
        // races the load handshake and the callback can sit waiting
        // for a first frame that never materializes. 50 iterations
        // with 1ms between them is a reliable floor on this box; it
        // costs ~50ms total and is noise vs. Servo's own settling.
        for _ in 0..50 {
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(1));
        }

        // The screenshot callback runs on the main thread under Servo's
        // event-loop pump. Shuttle the result out via RefCell.
        let slot: Rc<RefCell<Option<Result<RgbaImage, String>>>> = Rc::new(RefCell::new(None));
        {
            let slot = slot.clone();
            self.webview.take_screenshot(None, move |r| {
                *slot.borrow_mut() = Some(r.map_err(|e| format!("{e:?}")));
            });
        }

        // Spin until the callback fires. Generous deadline — fonts and
        // images inside Servo's resource pipeline can take longer than
        // a bare layout pass.
        let deadline = Instant::now() + Duration::from_secs(60);
        while slot.borrow().is_none() {
            if Instant::now() > deadline {
                return Err(RendererError::RenderingContext(
                    "timed out waiting for take_screenshot callback (>60s)".into(),
                ));
            }
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(5));
        }

        let result = slot.borrow_mut().take().expect("slot filled above");
        match result {
            Ok(img) => Ok(img),
            Err(e) => Err(RendererError::RenderingContext(format!(
                "screenshot failed: {e}"
            ))),
        }
    }
}

/// No-op waker for the corpus harness. Production uses `DispatchingWaker`
/// to push `spin_event_loop` through the Tauri main-thread dispatcher;
/// here we're already on the loop-pumping thread and just spin in the
/// `render` tight loop, so `wake()` has nothing useful to do.
struct NoopWaker;
impl EventLoopWaker for NoopWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(NoopWaker)
    }
    fn wake(&self) {}
}
