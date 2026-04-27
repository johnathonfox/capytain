// SPDX-License-Identifier: Apache-2.0

//! Linux (GTK on Wayland or X11) platform constructor for
//! [`ServoRenderer`].
//!
//! Scope: accept already-constructed `RawDisplayHandle` +
//! `RawWindowHandle` (the caller — the Tauri desktop app — provides a
//! target window, see `apps/desktop/src-tauri/src/lib.rs`). Servo's
//! `WindowRenderingContext` creates an OpenGL surface via surfman on
//! whichever backing handle the pair points at — `WaylandWindowHandle` +
//! `WaylandDisplayHandle` in the Wayland case, `XlibWindowHandle` +
//! `XlibDisplayHandle` in the X11 case. The renderer itself doesn't need
//! to know which one; surfman picks up the right GL binding.
//!
//! The "create a GTK widget subclass that hosts the rendering context"
//! refinement from design doc §4.3 is deferred to follow-up work — for
//! Day 2, the caller just hands us a window handle and we render onto
//! that window's surface. Proper child-widget integration with Tauri's
//! existing GTK hierarchy is tracked in `docs/week-6-day-2-notes.md`.

use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, WindowHandle};
use servo::WindowRenderingContext;

use super::delegate::{CursorCb, LinkCb};
use super::{MainThreadDispatch, RendererError, ServoRenderer};

impl ServoRenderer {
    /// Construct a Servo-backed renderer on Linux.
    ///
    /// Must be called on the thread that will own the renderer (the Tauri
    /// main thread in production). Installs the `Servo` / `WebView` /
    /// rendering-context trio into the module-level `MAIN_THREAD_STATE`
    /// `thread_local!` and returns a `Send + Sync` proxy ([`ServoRenderer`])
    /// that the caller can move into Tauri state and call from any thread.
    ///
    /// # Arguments
    ///
    /// - `dispatch`: a main-thread dispatcher (typically backed by
    ///   `tauri::AppHandle::run_on_main_thread`). Every subsequent
    ///   trait-method call on the returned `ServoRenderer` marshals work
    ///   onto the thread this was called from via this dispatcher.
    /// - `parent`: an object whose `raw-window-handle` gives us both a
    ///   `DisplayHandle` and a `WindowHandle`. On Linux Tauri, the Tauri
    ///   `tauri::Window` is this object.
    /// - `size`: initial size of the Servo rendering surface in physical
    ///   pixels.
    pub fn new_linux<H>(
        dispatch: Arc<dyn MainThreadDispatch>,
        parent: &H,
        size: PhysicalSize<u32>,
    ) -> Result<Self, RendererError>
    where
        H: HasDisplayHandle + HasWindowHandle,
    {
        let display_handle = parent
            .display_handle()
            .map_err(|e| RendererError::RenderingContext(format!("display handle: {e}")))?;
        let window_handle = parent
            .window_handle()
            .map_err(|e| RendererError::RenderingContext(format!("window handle: {e}")))?;

        validate_linux_handle(window_handle)?;

        // `surfman::error::Error` does not `impl std::error::Error` and
        // does not `impl Display` either (design doc §6.5 footgun —
        // slightly worse than documented). Wrap via the Debug formatter
        // at the boundary rather than trying to `?` through.
        let rendering_context = WindowRenderingContext::new(display_handle, window_handle, size)
            .map_err(|e| RendererError::RenderingContext(format!("{e:?}")))?;
        let rendering_context = Rc::new(rendering_context);

        let link_cb: Arc<Mutex<LinkCb>> = Arc::new(Mutex::new(None));
        let cursor_cb: Arc<Mutex<CursorCb>> = Arc::new(Mutex::new(None));

        let webview_id = Self::install_state_on_main_thread(
            rendering_context,
            Arc::clone(&dispatch),
            Arc::clone(&link_cb),
            Arc::clone(&cursor_cb),
        );

        Ok(ServoRenderer {
            dispatch,
            link_cb,
            cursor_cb,
            next_handle: AtomicU64::new(0),
            webview_id,
        })
    }
}

/// Reject `RawWindowHandle` variants that aren't Wayland or X11. Kept
/// separate so the platform check reads clearly at the call site.
fn validate_linux_handle(handle: WindowHandle<'_>) -> Result<(), RendererError> {
    use raw_window_handle::RawWindowHandle;
    match handle.as_raw() {
        RawWindowHandle::Wayland(_) | RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => Ok(()),
        other => Err(RendererError::UnsupportedWindowHandle(match other {
            RawWindowHandle::Win32(_) => "Win32 (use new_windows)",
            RawWindowHandle::AppKit(_) => "AppKit (use new_macos)",
            _ => "unknown non-Linux variant",
        })),
    }
}
