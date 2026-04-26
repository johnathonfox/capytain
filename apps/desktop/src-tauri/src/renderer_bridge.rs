// SPDX-License-Identifier: Apache-2.0

//! Bridge between Tauri's AppHandle and `capytain_renderer`.
//!
//! Two pieces live here:
//!
//! 1. [`TauriDispatcher`] — the [`capytain_renderer::MainThreadDispatch`]
//!    implementation backed by [`tauri::AppHandle::run_on_main_thread`].
//!    One instance per app; handed to the renderer at construction and
//!    to its internal `EventLoopWaker` for Servo to drive the loop.
//!
//! 2. [`install_servo_renderer`] — the setup-time wiring that builds a
//!    dedicated Servo reader window, attaches the renderer to it, and
//!    registers the link-click callback.
//!
//! The Linux NVIDIA EGL-Wayland env-var workaround lives in
//! `capytain_renderer::apply_nvidia_wayland_workaround` and is called
//! directly from `main` — shared with the corpus integration test.

use std::sync::Arc;

use capytain_core::EmailRenderer;
use capytain_renderer::{MainThreadDispatch, ServoRenderer};
use dpi::PhysicalSize;
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

/// Install the Servo renderer on the Tauri app, storing it in
/// [`AppState::servo_renderer`]. Must be called from the Tauri `setup`
/// hook (which runs on the main thread); the renderer's construction
/// path itself must happen on the main thread per design doc §6.6.
///
/// Returns `Ok(())` on successful install. Returns `Ok(())` (with a
/// log at `warn`) when the platform can't host a Servo surface — the
/// app continues to run without the reader pane in that case.
/// Genuine failures (e.g. window-create errors) are returned.
pub fn install_servo_renderer<R: Runtime>(
    app: &tauri::App<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();

    let dispatcher: Arc<dyn MainThreadDispatch> = TauriDispatcher::new(app_handle.clone());

    // Construct the platform-appropriate renderer. On Linux, this
    // reparents the Tauri main window through a `gtk::Overlay` so
    // the Servo surface layers over the Dioxus webkit2gtk view; the
    // UI's `ResizeObserver` keeps it positioned over the
    // `.reader-body-fill` element. macOS / Windows still go through
    // a separate OS window until their respective `NSView` / `HWND`
    // child-surface wiring lands (week-6-day-4 doc, §"Relationship
    // to other Week 6 deferred work").
    let servo_renderer = build_servo_renderer(app, &app_handle, Arc::clone(&dispatcher));

    let mut renderer: Box<dyn EmailRenderer> = match servo_renderer {
        Ok(mut r) => {
            tracing::info!("capytain-desktop: Servo renderer installed");
            #[cfg(target_os = "linux")]
            install_cursor_callback(&mut r, &app_handle);
            Box::new(r)
        }
        Err(e) => {
            tracing::warn!("capytain-desktop: Servo renderer unavailable on this platform: {e}");
            return Ok(());
        }
    };

    // Register the link-click callback so links in Servo-rendered
    // bodies open in the OS default browser, matching the iframe
    // path's `open_external_url` command. Reject non-http(s)/mailto
    // schemes server-side too — Servo doesn't sandbox content the
    // way our iframe does, so a `javascript:` or `file://` URL
    // here would be a real privilege escalation if forwarded blindly.
    renderer.on_link_click(Box::new(|url| {
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https" | "mailto") {
            tracing::warn!(%url, scheme, "capytain-desktop: rejecting non-http(s)/mailto link from reader");
            return;
        }
        let url_str = url.as_str();
        match webbrowser::open(url_str) {
            Ok(()) => tracing::info!(%url, "capytain-desktop: opened reader link in default browser"),
            Err(e) => tracing::warn!(%url, error = %e, "capytain-desktop: webbrowser::open failed"),
        }
    }));

    // Drop the renderer into AppState. `try_state` because setup() can
    // run before `manage()` in some Tauri configurations — in ours
    // `bootstrap_state` already called `app.manage(state)` just above
    // in `main`, so this lookup always succeeds.
    let state: tauri::State<AppState> = app.state();
    let mut slot = tauri::async_runtime::block_on(state.servo_renderer.lock());
    *slot = Some(renderer);

    Ok(())
}

/// Platform fan-out. Kept as a free function so the
/// `install_servo_renderer` body reads linearly regardless of how
/// many platforms we support. Linux uses a child-widget attached to
/// the main Tauri window (see `linux_gtk::LinuxGtkParent`); macOS /
/// Windows still create a sibling OS window until their respective
/// child-surface wiring lands.
#[cfg(target_os = "linux")]
fn build_servo_renderer<R: Runtime>(
    app: &tauri::App<R>,
    _app_handle: &AppHandle<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    use crate::linux_gtk::LinuxGtkParent;

    let main_window = app
        .get_webview_window("main")
        .ok_or("main Tauri webview window missing at Servo install time")?;
    let gtk_window = main_window
        .gtk_window()
        .map_err(|e| format!("cannot resolve GTK ApplicationWindow from Tauri main window: {e}"))?;

    // `Box::leak` rather than store in `AppState`: the `gtk::Paned`
    // + `DrawingArea` are `!Send` (GTK objects live on the main
    // thread), and AppState must be `Send + Sync` for Tauri's
    // `State<T>`. The parent's actual lifetime requirement is
    // "as long as Servo holds a raw handle to the `gdk::Window`",
    // which is the lifetime of the process — leak matches that
    // exactly and avoids a `!Send` field in AppState.
    let parent: &'static LinuxGtkParent = Box::leak(Box::new(LinuxGtkParent::install(
        &gtk_window,
        READER_INITIAL_WIDTH as i32,
        READER_INITIAL_HEIGHT as i32,
    )?));
    // Stash the leaked reference so the `reader_set_position` /
    // `reader_clear` IPC commands can reach it. `register_parent`
    // is idempotent (`OnceLock::set` ignores duplicates) so calling
    // it again on a hot-reload doesn't panic.
    crate::linux_gtk::register_parent(parent);

    let renderer = ServoRenderer::new_linux(
        dispatcher,
        parent,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?;

    Ok(renderer)
}

#[cfg(target_os = "macos")]
fn build_servo_renderer<R: Runtime>(
    _app: &tauri::App<R>,
    app_handle: &AppHandle<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    let reader_window = build_auxiliary_window(app_handle)?;
    Ok(ServoRenderer::new_macos(
        dispatcher,
        &reader_window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(target_os = "windows")]
fn build_servo_renderer<R: Runtime>(
    _app: &tauri::App<R>,
    app_handle: &AppHandle<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    let reader_window = build_auxiliary_window(app_handle)?;
    Ok(ServoRenderer::new_windows(
        dispatcher,
        &reader_window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn build_servo_renderer<R: Runtime>(
    _app: &tauri::App<R>,
    _app_handle: &AppHandle<R>,
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
) {
    let app_handle = app_handle.clone();
    renderer.on_cursor_change(Box::new(move |cursor| {
        let css_name = cursor_to_css_name(cursor);
        let app_handle = app_handle.clone();
        let _ = app_handle.clone().run_on_main_thread(move || {
            use gtk::prelude::WidgetExt;
            let Some(parent) = crate::linux_gtk::parent() else { return };
            let Some(gdk_window) = parent.drawing_area.window() else { return };
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
fn cursor_to_css_name(cursor: capytain_renderer::Cursor) -> &'static str {
    use capytain_renderer::Cursor;
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

/// Auxiliary OS window used on the platforms that don't yet have
/// native child-surface wiring (macOS, Windows). Linux now uses the
/// `linux_gtk::LinuxGtkParent` reparenting path instead.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build_auxiliary_window<R: Runtime>(
    app_handle: &AppHandle<R>,
) -> Result<tauri::Window<R>, Box<dyn std::error::Error>> {
    let reader_window = tauri::window::WindowBuilder::new(app_handle, "servo-reader")
        .title("Capytain Reader (Servo)")
        .inner_size(
            f64::from(READER_INITIAL_WIDTH),
            f64::from(READER_INITIAL_HEIGHT),
        )
        .resizable(true)
        .visible(true)
        .build()?;
    // Ensure the OS window is realized before its raw handle is
    // queried — X11/XWayland doesn't expose the native window handle
    // until the surface has been mapped.
    reader_window.show()?;
    Ok(reader_window)
}
