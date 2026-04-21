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

    // Dedicated auxiliary window for the Servo surface. Kept as a
    // separate OS window — not embedded inside the main Tauri window —
    // because Servo's `WindowRenderingContext` paints over its target
    // window's full surface, which would cover the Dioxus chrome in
    // the main window. Proper in-window child-surface embedding
    // (§4.3 of the design doc) is tracked in docs/week-6-day-2-notes.md;
    // this side-by-side arrangement is the honest Day 2 shape.
    let reader_window = tauri::WebviewWindowBuilder::new(
        &app_handle,
        "servo-reader",
        tauri::WebviewUrl::App("about:blank".into()),
    )
    .title("Capytain Reader (Servo)")
    .inner_size(
        f64::from(READER_WINDOW_WIDTH),
        f64::from(READER_WINDOW_HEIGHT),
    )
    .resizable(true)
    .build()?;

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
            tracing::warn!(
                "capytain-desktop: Servo renderer unavailable on this platform: {e}"
            );
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
    parent: &tauri::WebviewWindow<R>,
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
    parent: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_macos(
        dispatcher,
        parent,
        PhysicalSize::new(READER_WINDOW_WIDTH, READER_WINDOW_HEIGHT),
    )?)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd"
)))]
fn build_servo_renderer<R: Runtime>(
    _parent: &tauri::WebviewWindow<R>,
    _dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Err("Servo renderer is not yet implemented on this platform".into())
}
