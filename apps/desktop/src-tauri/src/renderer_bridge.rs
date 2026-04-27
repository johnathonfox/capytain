// SPDX-License-Identifier: Apache-2.0

//! Bridge between Tauri's AppHandle and `qsl_renderer`.
//!
//! Two pieces live here:
//!
//! 1. [`TauriDispatcher`] — the [`qsl_renderer::MainThreadDispatch`]
//!    implementation backed by [`tauri::AppHandle::run_on_main_thread`].
//!    One instance per app; handed to the renderer at construction and
//!    to its internal `EventLoopWaker` for Servo to drive the loop.
//!
//! 2. [`install_servo_renderer`] — the setup-time wiring that builds a
//!    dedicated Servo reader window, attaches the renderer to it, and
//!    registers the link-click callback.
//!
//! The Linux NVIDIA EGL-Wayland env-var workaround lives in
//! `qsl_renderer::apply_nvidia_wayland_workaround` and is called
//! directly from `main` — shared with the corpus integration test.

use std::sync::Arc;

use dpi::PhysicalSize;
use qsl_core::EmailRenderer;
use qsl_renderer::{MainThreadDispatch, ServoRenderer};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

/// Initial size of the Servo reader surface in device-independent
/// pixels. The UI's `ResizeObserver` pushes real `(x, y, w, h)`
/// rects via the `reader_set_position` Tauri command as soon as the
/// `.reader-body-fill` element measures itself, so this is just a
/// safe pre-layout default.
const READER_INITIAL_WIDTH: u32 = 720;
const READER_INITIAL_HEIGHT: u32 = 560;

/// `MainThreadDispatch` backed by Tauri's cross-platform run-loop
/// scheduler.
pub struct TauriDispatcher<R: Runtime> {
    handle: AppHandle<R>,
}

impl<R: Runtime> TauriDispatcher<R> {
    pub fn new(handle: AppHandle<R>) -> Arc<Self> {
        Arc::new(Self { handle })
    }
}

impl<R: Runtime> MainThreadDispatch for TauriDispatcher<R> {
    fn dispatch(&self, task: Box<dyn FnOnce() + Send + 'static>) {
        // `run_on_main_thread` returns `tauri::Result<()>` but the only
        // failure mode is "app is shutting down"; in that case the task
        // is harmlessly dropped and nothing more needs to happen.
        if let Err(e) = self.handle.run_on_main_thread(task) {
            tracing::debug!("TauriDispatcher: run_on_main_thread failed (app shutdown?): {e}");
        }
    }
}

/// Install the Servo renderer for the Tauri main window. Must be
/// called from the Tauri `setup` hook (which runs on the main
/// thread); the renderer's construction path itself must happen on
/// the main thread per design doc §6.6.
///
/// Popup reader windows install lazily on first render — see
/// [`install_servo_renderer_for_window`] which `commands::reader::reader_render`
/// calls when its lookup misses for a popup label.
///
/// Returns `Ok(())` on successful install. Returns `Ok(())` (with a
/// log at `warn`) when the platform can't host a Servo surface — the
/// app continues to run without the reader pane in that case.
/// Genuine failures (e.g. window-create errors) are returned.
pub fn install_servo_renderer<R: Runtime>(
    app: &tauri::App<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();
    let main_window = app
        .get_webview_window("main")
        .ok_or("main Tauri webview window missing at Servo install time")?;

    let renderer = match build_servo_for_window(&app_handle, &main_window) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("qsl-desktop: Servo renderer unavailable on this platform: {e}");
            return Ok(());
        }
    };

    let state: tauri::State<AppState> = app.state();
    let mut slot = tauri::async_runtime::block_on(state.servo_renderers.lock());
    slot.insert("main".to_string(), renderer);
    Ok(())
}

/// Install Servo for an arbitrary already-realized Tauri window. Used
/// by the popup-reader path: pop a `WebviewWindow`, then this gets
/// called on first `reader_render` for that label. Idempotent — a
/// second call for the same label replaces the prior entry, dropping
/// the old renderer.
#[cfg(feature = "servo")]
pub fn install_servo_renderer_for_window<R: Runtime>(
    app_handle: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = window.label().to_string();
    let renderer = build_servo_for_window(app_handle, window)?;
    let state: tauri::State<AppState> = app_handle.state();
    let mut slot = tauri::async_runtime::block_on(state.servo_renderers.lock());
    slot.insert(label, renderer);
    Ok(())
}

/// Construct and configure a `Box<dyn EmailRenderer>` for the given
/// already-realized Tauri window. Wires the link-click callback and
/// (Linux only) the cursor callback. The returned renderer is the
/// caller's to drop into the per-window `AppState::servo_renderers`
/// HashMap.
fn build_servo_for_window<R: Runtime>(
    app_handle: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) -> Result<Box<dyn EmailRenderer>, Box<dyn std::error::Error>> {
    let dispatcher: Arc<dyn MainThreadDispatch> = TauriDispatcher::new(app_handle.clone());
    let raw = build_servo_for(window, Arc::clone(&dispatcher))?;
    tracing::info!(label = %window.label(), "qsl-desktop: Servo renderer installed");

    // Linux gets the cursor callback — the same DrawingArea that
    // Servo paints into is what GDK changes the system cursor on.
    // macOS / Windows host Servo in a separate OS window with native
    // pointer feedback so they need no extra wiring here.
    #[cfg(target_os = "linux")]
    let mut renderer: Box<dyn EmailRenderer> = {
        let mut r = raw;
        install_cursor_callback(&mut r, app_handle, window.label());
        Box::new(r)
    };
    #[cfg(not(target_os = "linux"))]
    let mut renderer: Box<dyn EmailRenderer> = Box::new(raw);

    // Link clicks open in the OS default browser. `javascript:` /
    // `file://` schemes are rejected up front — Servo doesn't sandbox
    // content the way our previous iframe did, so a redirect from
    // email content to one of those would be real privilege escalation
    // if forwarded blindly.
    renderer.on_link_click(Box::new(|url| {
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https" | "mailto") {
            tracing::warn!(%url, scheme, "qsl-desktop: rejecting non-http(s)/mailto link from reader");
            return;
        }
        let url_str = url.as_str();
        match webbrowser::open(url_str) {
            Ok(()) => tracing::info!(%url, "qsl-desktop: opened reader link in default browser"),
            Err(e) => tracing::warn!(%url, error = %e, "qsl-desktop: webbrowser::open failed"),
        }
    }));

    Ok(renderer)
}

/// Platform fan-out. Linux reparents the supplied window through a
/// `gtk::Overlay` (see `linux_gtk::LinuxGtkParent`) and registers the
/// resulting parent under the window's label. macOS / Windows hand
/// the window's raw handle directly to Servo — no reparenting needed
/// since each Tauri window already has its own native surface.
#[cfg(target_os = "linux")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    use crate::linux_gtk::LinuxGtkParent;

    let gtk_window = window.gtk_window().map_err(|e| {
        format!(
            "cannot resolve GTK ApplicationWindow from Tauri window {}: {e}",
            window.label()
        )
    })?;

    // `Box::leak` rather than store in `AppState`: the GTK widgets
    // are `!Send`, and AppState must be `Send + Sync`. The parent's
    // actual lifetime requirement is "as long as Servo holds a raw
    // handle to the `gdk::Window`", which is the lifetime of the
    // process — leak matches that exactly. Closing a popup window
    // drops the renderer above this layer; the leaked widgets stay
    // (a few KB each, see plan doc § Known limitations).
    let parent: &'static LinuxGtkParent = Box::leak(Box::new(LinuxGtkParent::install(
        &gtk_window,
        READER_INITIAL_WIDTH as i32,
        READER_INITIAL_HEIGHT as i32,
    )?));
    crate::linux_gtk::register_parent(window.label(), parent);

    let renderer = ServoRenderer::new_linux(
        dispatcher,
        parent,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?;

    // The renderer just registered a `WebView` against the shared
    // Servo runtime under a fresh `webview_id`. Wire that id back to
    // the parent so the GTK pointer signal handlers know which
    // popup's webview to dispatch input to. Wire the signal handlers
    // last — before this point pointer events fall on the floor (the
    // `webview_id == 0` no-op branch in `qsl_renderer::forward`),
    // which is fine because the user can't generate input on a window
    // that isn't visible yet.
    parent.set_webview_id(renderer.webview_id());
    LinuxGtkParent::wire_input_forwarding(parent);

    Ok(renderer)
}

#[cfg(target_os = "macos")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_macos(
        dispatcher,
        window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(target_os = "windows")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_windows(
        dispatcher,
        window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn build_servo_for<R: Runtime>(
    _window: &tauri::WebviewWindow<R>,
    _dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Err("Servo renderer is not yet implemented on this platform".into())
}

/// Wire Servo's cursor-change notifications to the Linux GTK
/// DrawingArea so the OS pointer changes when the user hovers over
/// links, text runs, resize handles, etc. inside the reader pane.
///
/// `notify_cursor_changed` fires from Servo's main-thread paint cycle;
/// we still bounce through `run_on_main_thread` so we don't depend on
/// that invariant — the marshalling crosses no boundary when the
/// caller is already on the GTK thread.
#[cfg(target_os = "linux")]
fn install_cursor_callback<R: Runtime>(
    renderer: &mut ServoRenderer,
    app_handle: &AppHandle<R>,
    label: &str,
) {
    let app_handle = app_handle.clone();
    let label = label.to_string();
    renderer.on_cursor_change(Box::new(move |cursor| {
        let css_name = cursor_to_css_name(cursor);
        let app_handle = app_handle.clone();
        let label = label.clone();
        let _ = app_handle.clone().run_on_main_thread(move || {
            use gtk::prelude::WidgetExt;
            let Some(parent) = crate::linux_gtk::parent(&label) else {
                return;
            };
            let Some(gdk_window) = parent.drawing_area.window() else {
                return;
            };
            let display = gdk::Window::display(&gdk_window);
            let gdk_cursor = gdk::Cursor::from_name(&display, css_name);
            gdk_window.set_cursor(gdk_cursor.as_ref());
        });
    }));
}

/// Map Servo's `Cursor` enum to the CSS cursor names that GDK 3's
/// `gdk_cursor_new_from_name` accepts (the W3C `cursor` property
/// keywords). Anything GDK doesn't recognize falls back to the
/// theme's default cursor — so unknown values surface as a no-op
/// rather than a crash.
#[cfg(target_os = "linux")]
fn cursor_to_css_name(cursor: qsl_renderer::Cursor) -> &'static str {
    use qsl_renderer::Cursor;
    match cursor {
        Cursor::None => "none",
        Cursor::Default => "default",
        Cursor::Pointer => "pointer",
        Cursor::ContextMenu => "context-menu",
        Cursor::Help => "help",
        Cursor::Progress => "progress",
        Cursor::Wait => "wait",
        Cursor::Cell => "cell",
        Cursor::Crosshair => "crosshair",
        Cursor::Text => "text",
        Cursor::VerticalText => "vertical-text",
        Cursor::Alias => "alias",
        Cursor::Copy => "copy",
        Cursor::Move => "move",
        Cursor::NoDrop => "no-drop",
        Cursor::NotAllowed => "not-allowed",
        Cursor::Grab => "grab",
        Cursor::Grabbing => "grabbing",
        Cursor::EResize => "e-resize",
        Cursor::NResize => "n-resize",
        Cursor::NeResize => "ne-resize",
        Cursor::NwResize => "nw-resize",
        Cursor::SResize => "s-resize",
        Cursor::SeResize => "se-resize",
        Cursor::SwResize => "sw-resize",
        Cursor::WResize => "w-resize",
        Cursor::EwResize => "ew-resize",
        Cursor::NsResize => "ns-resize",
        Cursor::NeswResize => "nesw-resize",
        Cursor::NwseResize => "nwse-resize",
        Cursor::ColResize => "col-resize",
        Cursor::RowResize => "row-resize",
        Cursor::AllScroll => "all-scroll",
        Cursor::ZoomIn => "zoom-in",
        Cursor::ZoomOut => "zoom-out",
    }
}
