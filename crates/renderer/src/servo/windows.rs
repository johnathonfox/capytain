// SPDX-License-Identifier: Apache-2.0
//
// # UNVERIFIED
//
// This module was written without access to a Windows development box.
// It compiles under `cfg(target_os = "windows")` and follows
// `docs/servo-composition.md` §4.2 and the pattern established by the
// validated `new_linux` / (also-UNVERIFIED) `new_macos` constructors —
// but it has not been run, and review by a Windows-hardware engineer
// is expected to find real bugs.
//
// Treat every API choice here as a plausible default, not a verified
// call path. A follow-up session on Windows hardware will validate and
// correct this module. Use the `docs/week-6-day-3-notes.md` file (to
// be created by that session) to record findings the same way Day 2
// recorded Linux-specific discoveries in `docs/week-6-day-2-notes.md`.
//
// Deliberate choices that may need revisiting:
//
// - The constructor accepts a `Win32WindowHandle` (the Tauri main
//   window's `HWND`) via raw-window-handle and hands it straight to
//   `WindowRenderingContext::new`. Matches Linux/macOS shape: the
//   caller owns the widget tree; the renderer paints on whatever
//   surface it's handed. §4.2 also describes a richer path where the
//   renderer `CreateWindowExW`s a `WS_CHILD | WS_VISIBLE` child HWND
//   parented to Tauri's HWND — deferred to the Windows-hardware
//   session's integration pass; the current shape is the analogue of
//   what Linux ships in `new_linux`.
// - Event-loop integration is via the caller's `MainThreadDispatch`,
//   which on Windows is backed by `tauri::AppHandle::run_on_main_thread`
//   → the Win32 message loop on the thread that called
//   `CreateWindowExW`. No Windows-specific runloop code is needed
//   inside the renderer itself.
// - No `SetTimer` / `WM_TIMER` plumbing. `DispatchingWaker` (in
//   `crates/renderer/src/servo.rs`) already delivers `spin_event_loop`
//   ticks via the dispatcher; that's the same hook Linux/macOS use and
//   it reaches the Windows message loop correctly through Tauri's
//   cross-platform abstraction.
//
// One discovery flagged in the pre-spike design doc §4.2: the
// blank-window saga from PRs #11–#15 (pre-renderer, CSP / devtools
// related) on Windows might reappear if the Servo child surface
// renders blank on the first real session. If that happens, the first
// suspect is the Tauri-config CSP carryover from those PRs, not the
// Servo plumbing here.

//! Windows (Win32 `HWND`) platform constructor for [`ServoRenderer`].
//! Unverified — see module-level comment above.

use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, WindowHandle};
use servo::WindowRenderingContext;

use super::delegate::{CursorCb, LinkCb};
use super::{MainThreadDispatch, RendererError, ServoRenderer};

impl ServoRenderer {
    /// Construct a Servo-backed renderer on Windows.
    ///
    /// **Unverified** — this signature and body were written against
    /// the design doc and Servo documentation, without access to
    /// Windows hardware. Expect breakage. See module-level comment.
    ///
    /// Like `new_linux` / `new_macos`, this must be called on the
    /// thread that will own the renderer — the Tauri main thread,
    /// which on Windows is the thread that owns the `HWND` message
    /// loop (i.e. the thread that called `CreateWindowExW` on the
    /// Tauri window).
    ///
    /// # Arguments
    ///
    /// - `dispatch`: main-thread dispatcher, typically backed by
    ///   `tauri::AppHandle::run_on_main_thread`.
    /// - `parent`: object whose `raw-window-handle` gives us a
    ///   `Win32WindowHandle` (the Tauri window's HWND + HINSTANCE).
    ///   On Windows Tauri, the Tauri `tauri::Window` implements this.
    /// - `size`: initial Servo surface size in physical pixels.
    pub fn new_windows<H>(
        dispatch: Arc<dyn MainThreadDispatch>,
        parent: &H,
        size: PhysicalSize<u32>,
    ) -> Result<Self, RendererError>
    where
        H: HasDisplayHandle + HasWindowHandle,
    {
        // UNVERIFIED: written without Windows hardware access; needs validation.
        let display_handle = parent
            .display_handle()
            .map_err(|e| RendererError::RenderingContext(format!("display handle: {e}")))?;
        let window_handle = parent
            .window_handle()
            .map_err(|e| RendererError::RenderingContext(format!("window handle: {e}")))?;

        validate_windows_handle(window_handle)?;

        // `WindowRenderingContext::new` on Windows routes through
        // surfman's Win32 / WGL path (with ANGLE as the GL→D3D
        // translation layer — see `mozangle` in the Servo dep graph).
        // The renderer does not directly call any Win32 API — all
        // HWND / DC interaction is internal to surfman / Servo.
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

/// Reject `RawWindowHandle` variants that aren't `Win32`. Kept separate
/// so the platform check reads clearly at the call site.
fn validate_windows_handle(handle: WindowHandle<'_>) -> Result<(), RendererError> {
    use raw_window_handle::RawWindowHandle;
    match handle.as_raw() {
        RawWindowHandle::Win32(_) => Ok(()),
        other => Err(RendererError::UnsupportedWindowHandle(match other {
            RawWindowHandle::AppKit(_) => "AppKit (use new_macos)",
            RawWindowHandle::Wayland(_) | RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
                "Linux (use new_linux)"
            }
            RawWindowHandle::WinRt(_) => "WinRT (UWP) — not supported by the Win32 path",
            _ => "unknown non-Windows variant",
        })),
    }
}
