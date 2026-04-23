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
//! 3. [`apply_nvidia_wayland_workaround`] — a Linux-only env-var
//!    override that forces Mesa's llvmpipe EGL path, sidestepping the
//!    NVIDIA EGL-Wayland explicit-sync protocol error tracked in
//!    `docs/upstream/surfman-explicit-sync.md` (servo/surfman#354).
//!    Called from `main` before Tauri starts the event loop.

use std::sync::Arc;

use capytain_core::EmailRenderer;
use capytain_renderer::{MainThreadDispatch, ServoRenderer};
use dpi::PhysicalSize;
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

/// Logical size of the Servo reader window in device-independent pixels.
/// Fixed for Day 2; the resize path (tracking the GTK/AppKit/HWND parent)
/// lands in the Phase 1 reader-pane layout work.
const READER_WINDOW_WIDTH: u32 = 720;
const READER_WINDOW_HEIGHT: u32 = 560;

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

    // Dedicated auxiliary OS window for the Servo surface. **Plain**
    // `tauri::window::WindowBuilder`, not `WebviewWindowBuilder`: if we
    // create a second webkit2gtk-backed window here and then attach
    // Servo's surfman context on top, both GL contexts fight over the
    // same `wl_surface` and Wayland disconnects with protocol error
    // 71. The Dioxus chrome stays in the *main* Tauri webview window;
    // this window is a bare OS surface used exclusively by Servo.
    //
    // Kept as a separate OS window rather than embedded inside the
    // main Tauri window because `WindowRenderingContext` paints over
    // its target's full surface — §4.3 of the design doc describes
    // the proper in-window child-widget integration that's deferred
    // to Phase 1 (see docs/week-6-day-2-notes.md).
    let reader_window = tauri::window::WindowBuilder::new(&app_handle, "servo-reader")
        .title("Capytain Reader (Servo)")
        .inner_size(
            f64::from(READER_WINDOW_WIDTH),
            f64::from(READER_WINDOW_HEIGHT),
        )
        .resizable(true)
        .visible(true)
        .build()?;

    // Ensure the OS window is realized before we query its raw handle —
    // on X11/XWayland the underlying `Window` handle isn't available
    // until the window has been mapped at least once.
    reader_window.show()?;

    let dispatcher: Arc<dyn MainThreadDispatch> = TauriDispatcher::new(app_handle.clone());

    // Construct the platform-appropriate renderer. Unsupported platforms
    // (Windows for now) skip the Servo install entirely.
    let servo_renderer = build_servo_renderer(&reader_window, Arc::clone(&dispatcher));

    let mut renderer: Box<dyn EmailRenderer> = match servo_renderer {
        Ok(r) => {
            tracing::info!("capytain-desktop: Servo renderer installed");
            Box::new(r)
        }
        Err(e) => {
            tracing::warn!("capytain-desktop: Servo renderer unavailable on this platform: {e}");
            return Ok(());
        }
    };

    // Register the link-click callback so the human validator can
    // observe navigation events routing through the trait.
    renderer.on_link_click(Box::new(|url| {
        tracing::info!(%url, "capytain-desktop: link clicked in reader pane");
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

/// Platform fan-out. Kept as a free function so the `install_servo_renderer`
/// body reads linearly regardless of how many platforms we support.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
fn build_servo_renderer<R: Runtime>(
    parent: &tauri::Window<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_linux(
        dispatcher,
        parent,
        PhysicalSize::new(READER_WINDOW_WIDTH, READER_WINDOW_HEIGHT),
    )?)
}

#[cfg(target_os = "macos")]
fn build_servo_renderer<R: Runtime>(
    parent: &tauri::Window<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_macos(
        dispatcher,
        parent,
        PhysicalSize::new(READER_WINDOW_WIDTH, READER_WINDOW_HEIGHT),
    )?)
}

#[cfg(target_os = "windows")]
fn build_servo_renderer<R: Runtime>(
    parent: &tauri::Window<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_windows(
        dispatcher,
        parent,
        PhysicalSize::new(READER_WINDOW_WIDTH, READER_WINDOW_HEIGHT),
    )?)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd",
    target_os = "netbsd"
)))]
fn build_servo_renderer<R: Runtime>(
    _parent: &tauri::Window<R>,
    _dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Err("Servo renderer is not yet implemented on this platform".into())
}

/// On Linux, force Mesa's llvmpipe software EGL before any GL code
/// runs. Bypasses the `wp_linux_drm_syncobj_surface_v1` protocol error
/// NVIDIA's closed-source EGL-Wayland layer triggers when the
/// compositor advertises explicit sync — see
/// `docs/upstream/surfman-explicit-sync.md` (filed as
/// servo/surfman#354).
///
/// Each variable is only set if currently unset, so a developer can
/// override with native EGL to reproduce the bug (or test against a
/// driver fix) by exporting the variable before launch. Intended for
/// call from `main` after the telemetry init and before any Tauri /
/// GTK / Servo code touches GL.
///
/// Linux software rendering is fine for the reader pane: CPU cost of
/// rendering email HTML at 720x560 is negligible.
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
        ];

        let mut applied: Vec<&'static str> = Vec::new();
        for (name, value) in OVERRIDES {
            if std::env::var_os(name).is_none() {
                std::env::set_var(name, value);
                applied.push(name);
            }
        }

        if applied.is_empty() {
            tracing::debug!(
                "capytain-desktop: NVIDIA EGL-Wayland workaround skipped (all vars already set)"
            );
        } else {
            tracing::info!(
                vars = ?applied,
                "capytain-desktop: applied NVIDIA EGL-Wayland workaround (servo/surfman#354)"
            );
        }
    }
}
