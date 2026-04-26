// SPDX-License-Identifier: Apache-2.0

//! Servo-backed `EmailRenderer`.
//!
//! Architecture (summary — see `docs/servo-composition.md` for the full
//! design):
//!
//! ```text
//! ┌─── any thread ────────────────┐       ┌────── main thread ─────────┐
//! │                               │       │                            │
//! │  ServoRenderer (Send + Sync)  │──────►│  MAIN_THREAD_STATE         │
//! │   • Arc<dyn MainThreadDispatch│  main │    thread_local<RefCell>   │
//! │   • Arc<Mutex<LinkCb>>        │ thread│    • Rc<Servo>             │
//! │   • AtomicU64 handle counter  │       │    • WebView               │
//! │                               │       │    • Rc<WindowRenderingCtx>│
//! │                               │       │    • Rc<CapytainDelegate>  │
//! └───────────────────────────────┘       └────────────────────────────┘
//! ```
//!
//! The [`EmailRenderer`] trait requires `Send`, but every Servo type lives
//! in an `Rc` and must stay on the thread that built the `Servo` instance
//! (see design doc §6.6 "Thread affinity"). Rather than unsafely assert
//! `Send`, the implementation splits in two:
//!
//! 1. [`ServoRenderer`] — a Send + Sync proxy that downstream code (the
//!    Tauri desktop app) stores and calls from any thread.
//! 2. [`MainThreadState`] — the actual Servo state, stored in a
//!    `thread_local!` on whatever thread called `new_linux` / `new_macos`
//!    (the Tauri main thread in production).
//!
//! Calls on `ServoRenderer` marshal work to the main thread via the
//! [`MainThreadDispatch`] trait object supplied by the caller. The caller
//! (the desktop crate) backs this with `tauri::AppHandle::run_on_main_thread`,
//! which is platform-agnostic across macOS / Windows / Linux.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use capytain_core::{ColorScheme, EmailRenderer, RenderHandle, RenderPolicy};
use servo::{
    DevicePoint, EventLoopWaker, InputEvent, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, Preferences, RenderingContext, Servo, ServoBuilder,
    WebView, WebViewBuilder, WebViewPoint, WindowRenderingContext,
};

mod corpus;
mod delegate;
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub use corpus::{render_html_to_image, CorpusRenderer};
use delegate::{CapytainDelegate, LinkCb};

/// On Linux, force Mesa's llvmpipe software EGL before any GL code
/// runs. Bypasses the `wp_linux_drm_syncobj_surface_v1` protocol error
/// NVIDIA's closed-source EGL-Wayland layer triggers when the
/// compositor advertises explicit sync (KWin on Wayland) — filed
/// upstream as servo/surfman#354 and documented in
/// `docs/upstream/surfman-explicit-sync.md`.
///
/// Each variable is only set if currently unset, so a developer can
/// override with native EGL to reproduce the bug (or test against a
/// driver fix) by exporting the variable before launch. Safe to call
/// from anywhere before the first `RenderingContext` construction —
/// callers include the desktop bin at `main()` entry and the corpus
/// integration test at the top of its single `#[test]` function.
///
/// Software rendering is the right default for the reader pane
/// (~720×560 email HTML) and for the corpus harness (800×600 static
/// documents); neither is a GPU-bound workload.
pub fn apply_nvidia_wayland_workaround() {
    #[cfg(target_os = "linux")]
    {
        const OVERRIDES: &[(&str, &str)] = &[
            ("MESA_LOADER_DRIVER_OVERRIDE", "llvmpipe"),
            ("LIBGL_ALWAYS_SOFTWARE", "1"),
            (
                "__EGL_VENDOR_LIBRARY_FILENAMES",
                "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
            ),
            // GTK 3's Wayland backend can't actually subsurface an
            // arbitrary child widget — `gdk_window_ensure_native` on
            // the DrawingArea creates a brand-new `xdg_toplevel`
            // rather than a `wl_subsurface` of the main window, so
            // Servo's `WindowRenderingContext` ends up drawing into
            // what the compositor shows as a separate top-level
            // window (verified via `WAYLAND_DEBUG=client` —
            // duplicate `get_xdg_surface` + `get_toplevel` calls).
            // X11 has real child-window support; force every client
            // (Tauri, Dioxus webview, GTK, Servo/surfman) through
            // XWayland so the DrawingArea's backing window is an
            // actual X11 child of the main window and surfman's
            // Xlib backend binds to it inside the Tauri frame. Fix
            // properly once Tauri 2 migrates to GTK 4 (GDK 4 has
            // per-widget subsurface support).
            ("GDK_BACKEND", "x11"),
        ];

        let mut applied: Vec<&'static str> = Vec::new();
        for (name, value) in OVERRIDES {
            if std::env::var_os(name).is_none() {
                std::env::set_var(name, value);
                applied.push(name);
            }
        }

        if applied.is_empty() {
            tracing::debug!("NVIDIA EGL-Wayland workaround skipped (all vars already set)");
        } else {
            tracing::info!(
                vars = ?applied,
                "applied NVIDIA EGL-Wayland workaround (servo/surfman#354)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// A caller-supplied way to post work onto the thread that owns Servo.
///
/// The renderer is stored in a `Send + Sync` outer proxy
/// ([`ServoRenderer`]); every trait method has to reach back to the main
/// thread where the `Servo` engine, `WebView`, and rendering context live.
/// The desktop app implements this trait backed by
/// `tauri::AppHandle::run_on_main_thread`.
///
/// Implementations must be cheap to clone across threads. The dispatcher
/// may be called from Servo's internal worker threads (via
/// `EventLoopWaker::wake`), from Tokio runtime workers, or from the
/// main thread itself.
pub trait MainThreadDispatch: Send + Sync + 'static {
    /// Post a task to run on the main thread. The implementation must
    /// guarantee the task runs (eventually) on exactly the thread that
    /// constructed the renderer; order of posted tasks is preserved.
    fn dispatch(&self, task: Box<dyn FnOnce() + Send + 'static>);
}

/// Errors that can happen while constructing a Servo-backed renderer.
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    /// The `RawWindowHandle` variant didn't match the expected platform
    /// (e.g. passed `Win32WindowHandle` to `new_linux`).
    #[error("unsupported window handle variant for this platform: {0}")]
    UnsupportedWindowHandle(&'static str),

    /// `surfman::error::Error` doesn't `impl std::error::Error` (see
    /// design doc §6.5), so we stringify at the call site.
    #[error("failed to create Servo rendering context: {0}")]
    RenderingContext(String),
}

/// Servo-backed `EmailRenderer`. See module docs for the architecture.
///
/// All fields are `Send + Sync`; the real Servo state lives in
/// `MAIN_THREAD_STATE` on whatever thread `new_linux` / `new_macos` was
/// called from.
pub struct ServoRenderer {
    dispatch: Arc<dyn MainThreadDispatch>,
    link_cb: Arc<Mutex<LinkCb>>,
    next_handle: AtomicU64,
}

impl ServoRenderer {
    /// Common bit: build the `MainThreadState` from an already-constructed
    /// rendering context. Called from every platform-specific constructor
    /// after the context has been wired up. Must be on the main thread.
    fn install_state_on_main_thread(
        rendering_context: Rc<WindowRenderingContext>,
        dispatch: Arc<dyn MainThreadDispatch>,
        link_cb: Arc<Mutex<LinkCb>>,
    ) {
        let waker: Box<dyn EventLoopWaker> = Box::new(DispatchingWaker {
            dispatch: Arc::clone(&dispatch),
        });

        let mut preferences = Preferences::default();
        apply_reader_pane_preferences(&mut preferences);

        let servo = Rc::new(
            ServoBuilder::default()
                .preferences(preferences)
                .event_loop_waker(waker)
                .build(),
        );

        let delegate = Rc::new(CapytainDelegate::new(
            Rc::clone(&rendering_context),
            Arc::clone(&link_cb),
        ));

        let webview: WebView = WebViewBuilder::new(&servo, rendering_context.clone())
            .delegate(delegate.clone())
            .build();

        webview.focus();
        webview.show();

        MAIN_THREAD_STATE.with(|cell| {
            *cell.borrow_mut() = Some(MainThreadState {
                servo,
                webview,
                rendering_context,
                _delegate: delegate,
            });
        });
    }
}

impl EmailRenderer for ServoRenderer {
    fn render(&mut self, sanitized_html: &str, policy: RenderPolicy) -> RenderHandle {
        let handle = RenderHandle(self.next_handle.fetch_add(1, Ordering::Relaxed));
        // Email HTML is not served from a URL. Servo's WebView API exposes
        // only `load(Url)` — there is no `load_html` — so we encode the
        // document as a `data:` URL. The sanitizer upstream has already
        // stripped anything dangerous; the `data:` scheme is a legitimate
        // content channel, not a workaround.
        let data_url = match make_data_url(sanitized_html, policy.color_scheme) {
            Ok(u) => u,
            Err(err) => {
                tracing::warn!(error = %err, "failed to build data: URL, skipping render");
                return handle;
            }
        };

        self.dispatch.dispatch(Box::new(move || {
            MAIN_THREAD_STATE.with(|cell| {
                if let Some(state) = cell.borrow().as_ref() {
                    state.webview.load(data_url);
                }
            });
        }));

        handle
    }

    fn on_link_click(&mut self, cb: Box<dyn FnMut(url::Url) + Send + 'static>) {
        *self.link_cb.lock().expect("link_cb poisoned") = Some(cb);
    }

    fn resize(&mut self, size: dpi::PhysicalSize<u32>) {
        // Servo's WebView locks its viewport / layout size when the
        // surface is created. The native surface (GTK DrawingArea on
        // Linux, NSView on macOS, HWND on Windows) can be resized
        // independently — without this call, the host can grow the
        // widget but Servo keeps painting into the original
        // PhysicalSize. Marshal onto the main thread because the
        // WebView API is `!Send`.
        self.dispatch.dispatch(Box::new(move || {
            MAIN_THREAD_STATE.with(|cell| {
                if let Some(state) = cell.borrow().as_ref() {
                    state.webview.resize(size);
                }
            });
        }));
    }

    fn clear(&mut self) {
        let empty_url = url::Url::parse("about:blank").expect("about:blank is a valid URL");
        self.dispatch.dispatch(Box::new(move || {
            MAIN_THREAD_STATE.with(|cell| {
                if let Some(state) = cell.borrow().as_ref() {
                    state.webview.load(empty_url);
                }
            });
        }));
    }

    fn destroy(&mut self) {
        self.dispatch.dispatch(Box::new(move || {
            MAIN_THREAD_STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().take() {
                    // Servo 0.1.0 exposes no explicit shutdown API — the
                    // engine relies on Drop. Pump the event loop a few
                    // times so in-flight messages settle before the Rc
                    // goes out of scope at the end of this closure.
                    for _ in 0..5 {
                        state.servo.spin_event_loop();
                    }
                }
            });
        }));
    }
}

// ---------------------------------------------------------------------------
// Main-thread state
// ---------------------------------------------------------------------------

use std::rc::Rc;

thread_local! {
    /// The Servo engine, webview, and rendering context. Populated by a
    /// platform-specific constructor (`new_linux`, `new_macos`) on the
    /// thread that owns the Tauri event loop; never touched from any
    /// other thread.
    static MAIN_THREAD_STATE: RefCell<Option<MainThreadState>> = const { RefCell::new(None) };
}

struct MainThreadState {
    servo: Rc<Servo>,
    webview: WebView,
    rendering_context: Rc<WindowRenderingContext>,
    _delegate: Rc<CapytainDelegate>,
}

// ---------------------------------------------------------------------------
// Input event forwarding
// ---------------------------------------------------------------------------
//
// Servo's WebView doesn't auto-receive input from a host-supplied
// rendering surface (`WindowRenderingContext` is paint-only). The
// embedder is responsible for translating native pointer/keyboard
// events into [`InputEvent`] and calling `WebView::notify_input_event`.
//
// On the desktop Linux build the host widget is a `gtk::DrawingArea`
// owned by `linux_gtk::LinuxGtkParent`. Its `button-press-event`,
// `button-release-event`, `motion-notify-event`, and `leave-notify-event`
// handlers call into these helpers, which:
//
// 1. Are public free functions so the desktop crate doesn't need to
//    pull `servo`'s internal embedder-traits types into scope.
// 2. Run only on the main thread — they read `MAIN_THREAD_STATE`
//    directly without going through `MainThreadDispatch`. The GTK
//    signal handlers fire on the main thread by definition, so this
//    is sound.
// 3. Are no-ops if the renderer isn't installed (ServoRenderer never
//    constructed, install_state_on_main_thread never called).

/// Forward a pointer-button press at device-pixel coordinates `(x, y)`
/// (relative to the WebView's surface) to Servo. `button` is a GDK
/// button code (1=left, 2=middle, 3=right).
pub fn forward_pointer_button_press(button: u32, x: f32, y: f32) {
    forward_button(button, MouseButtonAction::Down, x, y);
}

/// Forward a pointer-button release. See [`forward_pointer_button_press`].
pub fn forward_pointer_button_release(button: u32, x: f32, y: f32) {
    forward_button(button, MouseButtonAction::Up, x, y);
}

/// Forward a pointer-move event at device-pixel coordinates `(x, y)`.
pub fn forward_pointer_move(x: f32, y: f32) {
    let event = InputEvent::MouseMove(MouseMoveEvent::new(WebViewPoint::Device(
        DevicePoint::new(x, y),
    )));
    forward(event);
}

/// Forward a "pointer left the surface" event. The host widget calls
/// this on `leave-notify-event` so Servo can reset hover state, drop
/// any in-flight drag, and stop animating the cursor.
pub fn forward_pointer_left_viewport() {
    let event = InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
        focus_moving_to_another_iframe: false,
    });
    forward(event);
}

fn forward_button(button: u32, action: MouseButtonAction, x: f32, y: f32) {
    // GDK buttons: 1=left, 2=middle, 3=right, 8=back, 9=forward.
    // Servo's `MouseButton::from(u64)` uses 0=left, 1=middle, 2=right
    // — different numbering, so map explicitly rather than rely on
    // implicit conversion.
    let mapped = match button {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        8 => MouseButton::Back,
        9 => MouseButton::Forward,
        other => MouseButton::Other(other as u16),
    };
    let event = InputEvent::MouseButton(MouseButtonEvent::new(
        action,
        mapped,
        WebViewPoint::Device(DevicePoint::new(x, y)),
    ));
    forward(event);
}

fn forward(event: InputEvent) {
    MAIN_THREAD_STATE.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            state.webview.notify_input_event(event);
        }
    });
}

// ---------------------------------------------------------------------------
// EventLoopWaker — dispatches `spin_event_loop` onto the main thread
// ---------------------------------------------------------------------------

struct DispatchingWaker {
    dispatch: Arc<dyn MainThreadDispatch>,
}

impl EventLoopWaker for DispatchingWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(DispatchingWaker {
            dispatch: Arc::clone(&self.dispatch),
        })
    }

    fn wake(&self) {
        self.dispatch.dispatch(Box::new(|| {
            MAIN_THREAD_STATE.with(|cell| {
                if let Some(state) = cell.borrow().as_ref() {
                    state.servo.spin_event_loop();
                    // After spin, the delegate will have called
                    // webview.paint() if a frame is ready; surface the
                    // paint to the display.
                    state.rendering_context.present();
                }
            });
        }));
    }
}

// ---------------------------------------------------------------------------
// Preferences for the reader pane
// ---------------------------------------------------------------------------

/// Apply the reader-pane preferences described in design doc §5.
///
/// Servo 0.1.0's `Preferences` struct does not expose a single `js_enabled`
/// toggle (the design doc named one that doesn't exist in the shipped
/// crate). JavaScript execution is defended against in two other layers:
/// (a) `ammonia` strips `<script>` upstream before `render()` is called,
/// and (b) a Phase 1 CSP on the `data:` URL further restricts execution.
/// We still disable every sensitive DOM API here as belt-and-braces.
fn apply_reader_pane_preferences(prefs: &mut Preferences) {
    // Email content must never reach any of these APIs.
    prefs.dom_serviceworker_enabled = false;
    prefs.dom_webrtc_enabled = false;
    prefs.dom_webgpu_enabled = false;
    prefs.dom_webgl2_enabled = false;
    prefs.dom_gamepad_enabled = false;
    prefs.dom_bluetooth_enabled = false;
    prefs.dom_geolocation_enabled = false;
    prefs.dom_notification_enabled = false;
    prefs.dom_clipboardevent_enabled = false;

    // Turn off the async HTML tokenizer until it stabilizes upstream
    // (design doc §5 flags this as pending per February-in-Servo 2026).
    prefs.dom_servoparser_async_html_tokenizer_enabled = false;

    // Expose the rendered reader pane to AccessKit — accessibility is
    // a first-class concern for Capytain (PRINCIPLES.md §accessibility).
    prefs.accessibility_enabled = true;
}

// ---------------------------------------------------------------------------
// HTML → data: URL
// ---------------------------------------------------------------------------

/// Wrap sanitized HTML in a minimal document and encode it as a `data:`
/// URL that Servo's `WebView::load` can consume.
///
/// Percent-encoded because `data:text/html` URLs with raw HTML would
/// break on `#` fragments, `%` signs, and anything else the URL parser
/// treats as structural.
fn make_data_url(
    sanitized_html: &str,
    color_scheme: ColorScheme,
) -> Result<url::Url, url::ParseError> {
    let scheme_str = match color_scheme {
        ColorScheme::Dark => "dark",
        // `ColorScheme` is `#[non_exhaustive]` (capytain-core), so a
        // wildcard is required; every other variant — Light today,
        // future additions — maps to the default light rendering.
        _ => "light",
    };
    // A minimal host document that carries the color scheme hint into the
    // rendered content via both the CSS property and the meta tag that
    // browsers check for `prefers-color-scheme` matching.
    let full_doc = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>:root {{ color-scheme: {scheme}; }}</style>\
         </head><body>{body}</body></html>",
        scheme = scheme_str,
        body = sanitized_html,
    );
    let encoded = percent_encode(&full_doc);
    url::Url::parse(&format!("data:text/html;charset=utf-8,{encoded}"))
}

/// Tiny percent-encoder for the subset of characters that break `data:`
/// URLs. Good enough for the reader pane; real sanitization is ammonia's
/// job upstream.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let encode = matches!(
            b,
            // Control chars, whitespace Servo won't tolerate in a URL,
            // and the URL-structural characters.
            0..=0x20 | 0x7f | b'%' | b'#' | b'?' | b'&' | b'+' | b'\\'
        );
        if encode {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Unit tests (data-URL encoder — platform constructors need a real Servo
// engine and are covered by the integration work)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_round_trips_a_simple_body() {
        let u = make_data_url("<p>Hello</p>", ColorScheme::Light).unwrap();
        assert_eq!(u.scheme(), "data");
        // The body must appear somewhere in the URL (encoded).
        let s = u.as_str();
        assert!(s.contains("Hello"));
        assert!(s.contains("color-scheme"));
    }

    #[test]
    fn data_url_encodes_percent_and_hash_and_whitespace() {
        let u = make_data_url("100% <a href=\"#x\">link</a>", ColorScheme::Dark).unwrap();
        let s = u.as_str();
        // Percent, hash, and the space must all have been percent-encoded.
        assert!(s.contains("%25"));
        assert!(s.contains("%23"));
        assert!(s.contains("%20"));
    }

    #[test]
    fn color_scheme_propagates_into_document() {
        let dark = make_data_url("<p>x</p>", ColorScheme::Dark).unwrap();
        let light = make_data_url("<p>x</p>", ColorScheme::Light).unwrap();
        assert!(dark.as_str().contains("dark"));
        assert!(light.as_str().contains("light"));
        assert_ne!(dark, light);
    }
}
