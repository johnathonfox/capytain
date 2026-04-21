// SPDX-License-Identifier: Apache-2.0
//
// # UNVERIFIED
//
// This module was written without access to macOS hardware. It compiles
// under `cfg(target_os = "macos")` and follows `docs/servo-composition.md`
// §4.1 and the `objc2` 0.6 documentation faithfully — but it has not
// been run, and review by a Mac-hardware engineer is expected to find
// real bugs.
//
// Treat every API choice here as a plausible default, not a verified
// call path. A follow-up session on Mac hardware (`docs/week-6-day-2-
// notes.md` § macOS follow-up) will validate and correct this module.
//
// Deliberate choices that may need revisiting:
//
// - `objc2`, not the unmaintained `cocoa` crate. The plan specified this.
// - The constructor accepts a `RawWindowHandle` (specifically
//   `AppKitWindowHandle`) from the caller rather than building its own
//   `NSView`. Matching the Linux path: the caller (the Tauri desktop
//   crate) owns the widget tree; the renderer paints on whatever surface
//   it's handed. The "create an `NSView` subview of the Tauri content
//   view" step from §4.1 is deferred to the desktop crate's integration
//   pass.
// - Event-loop integration is via the caller's `MainThreadDispatch`,
//   which on macOS can be backed by `NSRunLoop::mainRunLoop.perform(...)`
//   or the `dispatch_async(dispatch_get_main_queue(), ...)` block
//   pattern. Tauri's `AppHandle::run_on_main_thread` handles this,
//   consistent with the Linux constructor — no macOS-specific runloop
//   code is needed inside the renderer itself.

//! macOS (AppKit `NSView` / `NSWindow`) platform constructor for
//! [`ServoRenderer`]. Unverified — see module-level comment above.

use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, WindowHandle};
use servo::WindowRenderingContext;

use super::delegate::LinkCb;
use super::{MainThreadDispatch, RendererError, ServoRenderer};

impl ServoRenderer {
    /// Construct a Servo-backed renderer on macOS.
    ///
    /// **Unverified** — this signature and body were written against the
    /// design doc and Servo/objc2 documentation, without access to Mac
    /// hardware. Expect breakage. See module-level comment.
    ///
    /// Like `new_linux`, this must be called on the thread that will own
    /// the renderer (the Tauri main thread — which on macOS is the
    /// AppKit main thread, the one that called `NSApplication::run`).
    ///
    /// # Arguments
    ///
    /// - `dispatch`: main-thread dispatcher, typically backed by
    ///   `tauri::AppHandle::run_on_main_thread`.
    /// - `parent`: object whose `raw-window-handle` gives us an
    ///   `AppKitWindowHandle` (content view pointer of the Tauri
    ///   `NSWindow`). On macOS Tauri, the Tauri `tauri::Window`
    ///   implements this.
    /// - `size`: initial Servo surface size in physical pixels.
    pub fn new_macos<H>(
        dispatch: Arc<dyn MainThreadDispatch>,
        parent: &H,
        size: PhysicalSize<u32>,
    ) -> Result<Self, RendererError>
    where
        H: HasDisplayHandle + HasWindowHandle,
    {
        // UNVERIFIED: written without macOS hardware access; needs validation.
        let display_handle = parent
            .display_handle()
            .map_err(|e| RendererError::RenderingContext(format!("display handle: {e}")))?;
        let window_handle = parent
            .window_handle()
            .map_err(|e| RendererError::RenderingContext(format!("window handle: {e}")))?;

        validate_macos_handle(window_handle)?;

        // `WindowRenderingContext::new` on macOS routes through surfman,
        // which on AppKit binds a CAMetalLayer / CAOpenGLLayer to the
        // `NSView` pointed at by the raw window handle. The renderer
        // does not directly call any AppKit API — all AppKit interaction
        // is internal to surfman / Servo.
        let rendering_context = WindowRenderingContext::new(display_handle, window_handle, size)
            .map_err(|e| RendererError::RenderingContext(format!("{e:?}")))?;
        let rendering_context = Rc::new(rendering_context);

        let link_cb: Arc<Mutex<LinkCb>> = Arc::new(Mutex::new(None));

        Self::install_state_on_main_thread(
            rendering_context,
            Arc::clone(&dispatch),
            Arc::clone(&link_cb),
        );

        Ok(ServoRenderer {
            dispatch,
            link_cb,
            next_handle: AtomicU64::new(0),
        })
    }
}

/// Reject `RawWindowHandle` variants that aren't `AppKit`. Kept separate
/// so the platform check reads clearly at the call site.
fn validate_macos_handle(handle: WindowHandle<'_>) -> Result<(), RendererError> {
    use raw_window_handle::RawWindowHandle;
    match handle.as_raw() {
        RawWindowHandle::AppKit(_) => Ok(()),
        other => Err(RendererError::UnsupportedWindowHandle(match other {
            RawWindowHandle::Win32(_) => "Win32 (use new_windows)",
            RawWindowHandle::Wayland(_) | RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
                "Linux (use new_linux)"
            }
            _ => "unknown non-macOS variant",
        })),
    }
}
