// SPDX-License-Identifier: Apache-2.0

//! Servo-backed `EmailRenderer`.
//!
//! Architecture (summary — see `docs/servo-composition.md` for the full
//! design):
//!
//! ```text
//! ┌─── any thread ────────────────┐       ┌────── main thread ─────────┐
//! │                               │       │                            │
//! │  ServoRenderer (Send + Sync)  │──────►│  SERVO_RUNTIME (singleton) │
//! │   • Arc<dyn MainThreadDispatch│  main │    Rc<Servo>               │
//! │   • Arc<Mutex<LinkCb>>        │ thread│                            │
//! │   • AtomicU64 handle counter  │       │  WEBVIEWS (id → state)     │
//! │   • u64 webview_id            │       │    • WebView               │
//! │                               │       │    • Rc<WindowRenderingCtx>│
//! │                               │       │    • Rc<QslDelegate>       │
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
//! 2. Two main-thread `thread_local!`s: `SERVO_RUNTIME` (one `Rc<Servo>`
//!    for the process — `servo_config::opts::OPTIONS` is a process-global
//!    `OnceCell` and would panic on a second `ServoBuilder::build`) and
//!    `WEBVIEWS` (one entry per popup, keyed by the `webview_id` carried
//!    on each `ServoRenderer`). Each entry owns its own `WebView`,
//!    `WindowRenderingContext`, and delegate; the runtime's
//!    `spin_event_loop` services every webview at once.
//!
//! Calls on `ServoRenderer` marshal work to the main thread via the
//! [`MainThreadDispatch`] trait object supplied by the caller. The caller
//! (the desktop crate) backs this with `tauri::AppHandle::run_on_main_thread`,
//! which is platform-agnostic across macOS / Windows / Linux.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use qsl_core::{ColorScheme, EmailRenderer, RenderHandle, RenderPolicy};
use servo::{
    DevicePoint, EventLoopWaker, InputEvent, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, Preferences, RenderingContext, Servo, ServoBuilder,
    WebView, WebViewBuilder, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext,
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
use delegate::{CursorCb, LinkCb, QslDelegate};

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
/// `WEBVIEWS` on whatever thread `new_linux` / `new_macos` was called
/// from, keyed by [`Self::webview_id`]. The shared Servo runtime lives
/// in `SERVO_RUNTIME` on the same thread.
pub struct ServoRenderer {
    dispatch: Arc<dyn MainThreadDispatch>,
    link_cb: Arc<Mutex<LinkCb>>,
    cursor_cb: Arc<Mutex<CursorCb>>,
    next_handle: AtomicU64,
    webview_id: u64,
}

impl ServoRenderer {
    /// Register a callback fired when Servo wants the host to switch
    /// the OS cursor (hover over a link, text, resize handle, etc.).
    /// Replaces any previously-registered cursor callback.
    pub fn on_cursor_change(&mut self, cb: Box<dyn FnMut(servo::Cursor) + Send + 'static>) {
        *self.cursor_cb.lock().expect("cursor_cb poisoned") = Some(cb);
    }

    /// Identifier for this renderer's `WebView` inside the per-thread
    /// `WEBVIEWS` registry. The desktop crate threads this id back into
    /// [`forward_pointer_button_press`] etc. so GTK signal handlers
    /// dispatch input to the correct popup window.
    pub fn webview_id(&self) -> u64 {
        self.webview_id
    }
}

/// Process-global counter that hands out webview ids. Starts at 1 so
/// `0` can stand in as "no webview yet" inside `LinuxGtkParent`'s
/// `AtomicU64` slot before the renderer is constructed.
static NEXT_WEBVIEW_ID: AtomicU64 = AtomicU64::new(1);

impl ServoRenderer {
    /// Common bit: build (or reuse) the shared Servo runtime, attach a
    /// new `WebView` for the supplied rendering context, and register
    /// it in the per-thread `WEBVIEWS` map under a freshly-allocated
    /// `webview_id`. Called from every platform-specific constructor
    /// after the context has been wired up. Must be on the main thread.
    ///
    /// The `dispatch` argument is consumed for the `EventLoopWaker`
    /// only on the very first call (when the Servo runtime is
    /// constructed); subsequent calls reuse the runtime that the first
    /// dispatcher already wired up. In practice every dispatcher in
    /// QSL is a `TauriDispatcher` cloning the same `AppHandle`, so
    /// "the first one wins" is functionally identical to "use mine."
    fn install_state_on_main_thread(
        rendering_context: Rc<WindowRenderingContext>,
        dispatch: Arc<dyn MainThreadDispatch>,
        link_cb: Arc<Mutex<LinkCb>>,
        cursor_cb: Arc<Mutex<CursorCb>>,
    ) -> u64 {
        // Bootstrap the process-global Servo runtime on first call.
        // `servo_config::opts::OPTIONS` is a `OnceCell` initialized
        // inside `ServoBuilder::build`; calling `build` a second time
        // panics with "Already initialized" (servo-config 0.1.0
        // opts.rs:246), so we hold exactly one `Rc<Servo>` for the
        // life of the process and clone it for every additional
        // `WebView`.
        SERVO_RUNTIME.with(|slot| {
            let mut s = slot.borrow_mut();
            if s.is_none() {
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
                *s = Some(servo);
            }
        });

        let servo: Rc<Servo> = SERVO_RUNTIME.with(|slot| {
            Rc::clone(
                slot.borrow()
                    .as_ref()
                    .expect("SERVO_RUNTIME initialized just above"),
            )
        });

        let delegate = Rc::new(QslDelegate::new(
            Rc::clone(&rendering_context),
            Arc::clone(&link_cb),
            Arc::clone(&cursor_cb),
        ));

        let webview: WebView = WebViewBuilder::new(&servo, rendering_context.clone())
            .delegate(delegate.clone())
            .build();

        webview.focus();
        webview.show();

        let id = NEXT_WEBVIEW_ID.fetch_add(1, Ordering::Relaxed);
        WEBVIEWS.with(|cell| {
            cell.borrow_mut().insert(
                id,
                WebViewState {
                    webview,
                    rendering_context,
                    _delegate: delegate,
                },
            );
        });
        id
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

        let id = self.webview_id;
        self.dispatch.dispatch(Box::new(move || {
            WEBVIEWS.with(|cell| {
                if let Some(state) = cell.borrow().get(&id) {
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
        let id = self.webview_id;
        self.dispatch.dispatch(Box::new(move || {
            WEBVIEWS.with(|cell| {
                if let Some(state) = cell.borrow().get(&id) {
                    state.webview.resize(size);
                }
            });
        }));
    }

    fn clear(&mut self) {
        let empty_url = url::Url::parse("about:blank").expect("about:blank is a valid URL");
        let id = self.webview_id;
        self.dispatch.dispatch(Box::new(move || {
            WEBVIEWS.with(|cell| {
                if let Some(state) = cell.borrow().get(&id) {
                    state.webview.load(empty_url);
                }
            });
        }));
    }

    fn destroy(&mut self) {
        let id = self.webview_id;
        self.dispatch.dispatch(Box::new(move || {
            let removed = WEBVIEWS.with(|cell| cell.borrow_mut().remove(&id));
            if removed.is_some() {
                // Servo 0.1.0 exposes no explicit shutdown API — the
                // engine relies on Drop. Pump the runtime's event loop
                // a few times so in-flight messages settle before the
                // entry's Rcs go out of scope at the end of this
                // closure. The runtime itself stays alive for the
                // lifetime of the process.
                SERVO_RUNTIME.with(|s| {
                    if let Some(servo) = s.borrow().as_ref() {
                        for _ in 0..5 {
                            servo.spin_event_loop();
                        }
                    }
                });
            }
        }));
    }
}

// ---------------------------------------------------------------------------
// Main-thread state
// ---------------------------------------------------------------------------

use std::rc::Rc;

thread_local! {
    /// The shared Servo runtime. Populated by the first
    /// platform-specific constructor (`new_linux`, `new_macos`) on the
    /// thread that owns the Tauri event loop; reused on every
    /// subsequent constructor call. `servo_config::opts::OPTIONS` is a
    /// process-global `OnceCell` so a second `ServoBuilder::build`
    /// would panic with "Already initialized" — see opts.rs:246 in
    /// servo-config 0.1.0.
    static SERVO_RUNTIME: RefCell<Option<Rc<Servo>>> = const { RefCell::new(None) };

    /// One entry per active `WebView`, keyed by `webview_id`. The main
    /// reader and every popup window each get their own entry; all
    /// share the single `SERVO_RUNTIME` above. `spin_event_loop` is a
    /// runtime-level call that services every webview at once.
    static WEBVIEWS: RefCell<HashMap<u64, WebViewState>> = RefCell::new(HashMap::new());
}

struct WebViewState {
    webview: WebView,
    rendering_context: Rc<WindowRenderingContext>,
    _delegate: Rc<QslDelegate>,
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
// 2. Run only on the main thread — they read `WEBVIEWS` directly
//    without going through `MainThreadDispatch`. The GTK signal
//    handlers fire on the main thread by definition, so this is sound.
// 3. Are no-ops if no `WEBVIEWS` entry matches `webview_id`. The
//    `LinuxGtkParent` carries an `AtomicU64` slot that's left at `0`
//    until the renderer registers its id, so signal handlers fire
//    harmlessly during the brief window between widget realization
//    and renderer construction.

/// Forward a pointer-button press at device-pixel coordinates `(x, y)`
/// (relative to the WebView's surface) to Servo. `button` is a GDK
/// button code (1=left, 2=middle, 3=right).
pub fn forward_pointer_button_press(webview_id: u64, button: u32, x: f32, y: f32) {
    forward_button(webview_id, button, MouseButtonAction::Down, x, y);
}

/// Forward a pointer-button release. See [`forward_pointer_button_press`].
pub fn forward_pointer_button_release(webview_id: u64, button: u32, x: f32, y: f32) {
    forward_button(webview_id, button, MouseButtonAction::Up, x, y);
}

/// Forward a pointer-move event at device-pixel coordinates `(x, y)`.
pub fn forward_pointer_move(webview_id: u64, x: f32, y: f32) {
    let event = InputEvent::MouseMove(MouseMoveEvent::new(WebViewPoint::Device(DevicePoint::new(
        x, y,
    ))));
    forward(webview_id, event);
}

/// Forward a wheel/scroll event. `(dx, dy)` are in the line-mode
/// convention (wheel "notch" units) using Servo's sign convention:
///
/// - `dy > 0` ⇒ view scrolls **up** (revealing more content above).
/// - `dy < 0` ⇒ view scrolls **down**.
/// - `dx > 0` ⇒ view scrolls left, `dx < 0` ⇒ right.
///
/// `(x, y)` is the cursor position in device pixels at the time of
/// the wheel event. The caller (the GTK signal handler) is
/// responsible for converting GDK's "user wants to scroll" sign
/// convention into Servo's "view moves" convention before calling.
pub fn forward_pointer_wheel(webview_id: u64, dx: f32, dy: f32, x: f32, y: f32) {
    let event = InputEvent::Wheel(WheelEvent::new(
        WheelDelta {
            x: dx as f64,
            y: dy as f64,
            z: 0.0,
            mode: WheelMode::DeltaLine,
        },
        WebViewPoint::Device(DevicePoint::new(x, y)),
    ));
    forward(webview_id, event);
}

/// Forward a "pointer left the surface" event. The host widget calls
/// this on `leave-notify-event` so Servo can reset hover state, drop
/// any in-flight drag, and stop animating the cursor.
pub fn forward_pointer_left_viewport(webview_id: u64) {
    let event = InputEvent::MouseLeftViewport(MouseLeftViewportEvent {
        focus_moving_to_another_iframe: false,
    });
    forward(webview_id, event);
}

fn forward_button(webview_id: u64, button: u32, action: MouseButtonAction, x: f32, y: f32) {
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
    forward(webview_id, event);
}

fn forward(webview_id: u64, event: InputEvent) {
    if webview_id == 0 {
        return;
    }
    WEBVIEWS.with(|cell| {
        if let Some(state) = cell.borrow().get(&webview_id) {
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
            // Servo runs one event loop for the whole runtime; spinning
            // it dispatches paints to whichever webviews have new
            // frames pending. Each delegate's
            // `notify_new_frame_ready` already calls
            // `rendering_context.present()` for its own surface during
            // the spin, but we re-present every registered context
            // afterwards as a backstop — matches the single-webview
            // pre-refactor behavior and is harmless when nothing
            // changed.
            SERVO_RUNTIME.with(|s| {
                if let Some(servo) = s.borrow().as_ref() {
                    servo.spin_event_loop();
                }
            });
            WEBVIEWS.with(|m| {
                for state in m.borrow().values() {
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
    // a first-class concern for QSL (PRINCIPLES.md §accessibility).
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
        // `ColorScheme` is `#[non_exhaustive]` (qsl-core), so a
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

/// Tiny percent-encoder for the bytes that break `data:` URLs. Good
/// enough for the reader pane; real sanitization is ammonia's job
/// upstream.
///
/// Non-ASCII bytes (>= 0x80) MUST be percent-escaped, not pushed as
/// `b as char`. `&str::bytes()` yields the UTF-8 representation of each
/// codepoint; for a multi-byte char like `®` (0xC2 0xAE) those bytes
/// get cast to Latin-1 codepoints (`Â` and `®`) and re-encoded back to
/// UTF-8 as 0xC3 0x82 0xC2 0xAE. Servo then renders that as `Â®` —
/// the exact mojibake reported in `docs/QSL_BACKLOG_FIXES.md` item 1.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let encode = matches!(
            b,
            0..=0x20 | 0x7f..=0xff | b'%' | b'#' | b'?' | b'&' | b'+' | b'\\'
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

    /// Regression for the mojibake reported in QSL_BACKLOG_FIXES.md
    /// item 1. UTF-8 multi-byte sequences must round-trip as their
    /// percent-encoded byte sequences, not as the Latin-1 codepoint
    /// reinterpretation of each individual byte.
    #[test]
    fn percent_encode_escapes_non_ascii_bytes() {
        // ® → 0xC2 0xAE in UTF-8.
        assert_eq!(percent_encode("®"), "%C2%AE");
        // — (em dash) → 0xE2 0x80 0x94.
        assert_eq!(percent_encode("—"), "%E2%80%94");
        // " (left double quotation mark) → 0xE2 0x80 0x9C.
        assert_eq!(percent_encode("\u{201c}"), "%E2%80%9C");
        // Mixed ASCII + non-ASCII.
        assert_eq!(percent_encode("Hands®"), "Hands%C2%AE");
    }

    #[test]
    fn data_url_round_trips_utf8_through_percent_decode() {
        // Build a body with the exact characters from the user's bug
        // report: ® inside a marketing-style sentence.
        let body = r#"<p>Good Hands® policy</p>"#;
        let u = make_data_url(body, ColorScheme::Light).unwrap();
        let s = u.as_str();
        // The encoded byte sequence for ® must be present, and the
        // Latin-1 reinterpretation (Â) must NOT.
        assert!(s.contains("%C2%AE"), "® not percent-encoded as %C2%AE: {s}");
        assert!(
            !s.contains("Â"),
            "Latin-1 mojibake leaked into data URL: {s}"
        );
    }
}
