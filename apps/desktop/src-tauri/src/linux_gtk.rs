// SPDX-License-Identifier: Apache-2.0
#![allow(unsafe_code)]
// Every unsafe block in this module is a single FFI boundary crossing
// to GDK / GLib, or a `raw_window_handle::*::borrow_raw` call. See
// `docs/week-6-day-4-gtk-integration.md` step 2 for the workspace-
// wide `forbid → deny` rationale.

//! Linux GTK child-widget integration for the Servo reader pane.
//!
//! Tauri 2 on Linux renders its main window as a
//! `gtk::ApplicationWindow` wrapping a `webkit2gtk::WebView`. This
//! module reparents that hierarchy through a `gtk::Overlay`, attaches
//! a `gtk::DrawingArea` as an overlay child, and hands the
//! DrawingArea's raw display/window handles to Servo's
//! `WindowRenderingContext`.
//!
//! The shape after `install`:
//!
//! ```text
//! gtk::ApplicationWindow (Tauri-owned)
//! └── gtk::Overlay (new)
//!     ├── webkit2gtk::WebView (main child: Dioxus chrome)
//!     └── gtk::DrawingArea   (overlay child: Servo reader surface,
//!                              positioned via margins to overlap
//!                              Dioxus's `.reader-body-fill` slot)
//! ```
//!
//! Phase 2 Week 16: rebuilt from `gtk::Paned` to `gtk::Overlay`. The
//! Paned approach permanently reserved a fixed horizontal slice for
//! the reader, which collided with the new CSS-grid three-pane shell
//! (the third Dioxus pane and Servo were fighting for the same
//! ~720px). The Overlay path lets Dioxus drive Servo's allocation:
//! the UI watches `.reader-body-fill`'s `getBoundingClientRect()` and
//! pushes the rect to [`LinuxGtkParent::set_position`] over the
//! `reader_set_position` Tauri command, which updates the
//! DrawingArea's margins so Servo paints exactly over the slot.
//!
//! See `docs/week-6-day-4-gtk-integration.md` for the original design
//! and the hardware-gating rationale; `docs/upstream/surfman-explicit-sync.md`
//! for why the native NVIDIA EGL-Wayland path is broken and we rely
//! on the Mesa llvmpipe fallback (`qsl_renderer::apply_nvidia_wayland_workaround`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use gdk::prelude::*;
use glib::translate::ToGlibPtr;
use gtk::prelude::*;
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle, WindowHandle, XlibDisplayHandle, XlibWindowHandle,
};

/// Per-window registry of leaked `LinuxGtkParent`s, keyed by Tauri
/// window label (`"main"`, `"reader-<msg_id>"`, …). Each registered
/// entry was installed by `install_servo_renderer_for_window` and
/// stays alive for the lifetime of the process — see the `Box::leak`
/// rationale on `LinuxGtkParent::install`. `Mutex` rather than
/// `RwLock` because contention is nil and the Mutex API is simpler.
static GTK_PARENTS: Mutex<Option<HashMap<String, &'static LinuxGtkParent>>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<String, &'static LinuxGtkParent>) -> R) -> R {
    let mut guard = GTK_PARENTS.lock().expect("GTK_PARENTS mutex poisoned");
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Get the registered parent for a window label, if any. Commands
/// that depend on Servo being live for that window (position updates,
/// surface clear) use this and silently no-op when Servo isn't
/// installed for the calling window.
pub fn parent(label: &str) -> Option<&'static LinuxGtkParent> {
    with_registry(|m| m.get(label).copied())
}

/// Register the leaked parent under a window label. Overwrites any
/// prior entry for the same label (idempotent on re-install paths).
pub fn register_parent(label: &str, p: &'static LinuxGtkParent) {
    with_registry(|m| {
        m.insert(label.to_string(), p);
    });
}

/// Drop the registry entry for a label. The pointed-at
/// `LinuxGtkParent` is `Box::leak`'d and stays in memory; this just
/// removes the lookup so future `reader_*` IPC calls for that label
/// no-op. Used by the popup-window close handler.
pub fn remove_parent(label: &str) {
    with_registry(|m| {
        m.remove(label);
    });
}

#[cfg(test)]
fn clear_registry_for_test() {
    with_registry(|m| m.clear());
}

/// Reparented Tauri main window. Holds both the Overlay and the new
/// DrawingArea so their lifetimes are tied to the Tauri app —
/// dropping this struct would destroy the GDK window Servo is
/// painting to.
pub struct LinuxGtkParent {
    /// Kept alive so the widget hierarchy doesn't get torn down.
    _overlay: gtk::Overlay,
    /// The child widget Servo paints into. Public so callers can
    /// wire `connect_size_allocate` for reader-pane resize events.
    pub drawing_area: gtk::DrawingArea,
    /// Identifier of the `WebView` registered in `qsl_renderer`'s
    /// per-thread `WEBVIEWS` map for this parent. Set by
    /// [`Self::set_webview_id`] right after the renderer is built.
    /// Stays at `0` until then; signal handlers treat `0` as a
    /// "no webview yet" no-op.
    webview_id: AtomicU64,
}

// SAFETY: GTK widgets are not thread-safe at the type level, but the
// `&'static LinuxGtkParent` references stored in `GTK_PARENT` are
// only dereferenced from closures passed through
// `tauri::AppHandle::run_on_main_thread`. That marshals the closure
// onto GTK's main thread before it runs, so every method call on the
// inner widgets executes on the thread that constructed them. The
// static itself just holds the pointer; reading/writing the pointer
// is fine across threads.
unsafe impl Send for LinuxGtkParent {}
unsafe impl Sync for LinuxGtkParent {}

impl LinuxGtkParent {
    /// Reparent the current child of `app_window` inside a new
    /// `Overlay`, attaching a fresh `DrawingArea` as an overlay
    /// child. Realizes the DrawingArea so its backing `gdk::Window`
    /// is available before `handles()` is called.
    pub fn install(
        app_window: &gtk::ApplicationWindow,
        initial_width: i32,
        initial_height: i32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let t_install_start = std::time::Instant::now();
        // Diagnostic: log what Tauri's `gtk_window()` actually handed
        // us. If this is anything other than the top-level
        // ApplicationWindow, our reparenting walks the wrong
        // hierarchy and Servo ends up getting a handle to a surface
        // that doesn't live inside the user-visible main window.
        tracing::debug!(
            widget_type = %glib::ObjectExt::type_(app_window).name(),
            is_toplevel = app_window.is_toplevel(),
            "linux_gtk: reparenting into Tauri's main gtk_window"
        );

        // Pull the existing child (Tauri's webkit2gtk container) out
        // of the ApplicationWindow so we can wrap it in an Overlay.
        let original = app_window
            .child()
            .ok_or("main window has no child widget")?;
        tracing::debug!(
            child_type = %glib::ObjectExt::type_(&original).name(),
            "linux_gtk: removing original child from app_window"
        );
        app_window.remove(&original);

        let overlay = gtk::Overlay::new();
        // The webview is the main child — it fills the entire
        // overlay (and therefore the window). The DrawingArea is
        // layered on top via `add_overlay`, positioned via margins.
        overlay.add(&original);

        let drawing_area = gtk::DrawingArea::new();
        drawing_area.set_size_request(initial_width, initial_height);

        // `app_paintable(true)` tells GTK's default draw path to
        // leave the widget's backing surface alone so whatever
        // Servo's `WindowRenderingContext` writes to the
        // `gdk::Window` stays visible. Without this flag, GTK3
        // clears the DrawingArea to the theme background on every
        // draw cycle and Servo's paint disappears.
        drawing_area.set_app_paintable(true);
        // GTK widgets focus on click only when `can_focus` is set;
        // without this, keyboard focus stays on the webview and
        // never reaches the Servo surface.
        drawing_area.set_can_focus(true);
        // Subscribe the DrawingArea to the pointer events we
        // forward to Servo. By default GTK widgets only receive a
        // narrow set of events (expose, configure, etc.); button-
        // press / motion / leave have to be opted into explicitly,
        // otherwise the X11/Wayland event mask filters them out
        // before they reach signal handlers.
        drawing_area.add_events(
            gdk::EventMask::BUTTON_PRESS_MASK
                | gdk::EventMask::BUTTON_RELEASE_MASK
                | gdk::EventMask::POINTER_MOTION_MASK
                | gdk::EventMask::SCROLL_MASK
                | gdk::EventMask::LEAVE_NOTIFY_MASK
                | gdk::EventMask::ENTER_NOTIFY_MASK,
        );
        // Pin the DrawingArea to the top-left of the overlay.
        // Margins push it to the right slot; without
        // `Align::Start`, GtkOverlay would center / fill the
        // overlay instead of honouring our margins.
        drawing_area.set_halign(gtk::Align::Start);
        drawing_area.set_valign(gtk::Align::Start);
        // Start positioned off-screen so the surface is invisible
        // until the UI's `ResizeObserver` pushes a real rect via
        // `set_position`. Nothing is selected at install time, so
        // a visible Servo surface would be a flash of dead pixels.
        drawing_area.set_margin_start(10_000);
        drawing_area.set_margin_top(0);

        // Pointer signal handlers are wired in
        // [`Self::wire_input_forwarding`] after the renderer assigns a
        // webview_id. Wiring them here would mean the closures had no
        // way to identify which `WebView` to forward to, since the
        // renderer is constructed strictly after this function returns.

        overlay.add_overlay(&drawing_area);

        app_window.add(&overlay);
        app_window.show_all();

        // Force realization so `gdk::Window` is available. GTK 3
        // normally defers realization until the widget is drawn;
        // Servo wants the window handle immediately.
        drawing_area.realize();

        // Force the DrawingArea's `gdk::Window` to have a real
        // native backing (wl_subsurface on Wayland, separate X
        // Window on X11). GTK3 implements child widget windows as
        // client-side regions by default; surfman can't bind a GL
        // context to those — there's no real surface to swap
        // buffers on.
        if let Some(gdk_window) = drawing_area.window() {
            gdk_window.ensure_native();
        } else {
            tracing::warn!(
                "linux_gtk: DrawingArea has no gdk::Window after realize — Servo will fail"
            );
        }

        // Pump the GTK main loop until the DrawingArea has a real
        // size allocation. `show_all` + `realize` create the
        // `gdk::Window` but the layout pass that sizes the overlay
        // children hasn't run yet — at install time the gdk_window
        // width is `1`, which means surfman's
        // `WindowRenderingContext` gets a 1x1 surface handle and
        // ends up creating its own top-level wl_surface to render
        // into (visible as a separate window).
        let t_layout_start = std::time::Instant::now();
        let mut layout_iters = 0u32;
        let layout_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < layout_deadline {
            let size = drawing_area
                .window()
                .map(|w| (w.width(), w.height()))
                .unwrap_or((0, 0));
            if size.0 > 1 && size.1 > 1 {
                break;
            }
            layout_iters += 1;
            if !gtk::main_iteration_do(false) {
                // Nothing more queued; the widget isn't going to
                // lay itself out without a displayed compositor
                // frame — break and let Servo see whatever size we
                // have so the failure log below fires.
                break;
            }
        }
        tracing::info!(
            layout_pump_ms = t_layout_start.elapsed().as_millis() as u64,
            iters = layout_iters,
            "linux_gtk: install layout pump finished"
        );

        if let Some(gdk_window) = drawing_area.window() {
            let (w, h) = (gdk_window.width(), gdk_window.height());
            tracing::debug!(
                drawing_area_size = ?(w, h),
                window_type = ?gdk_window.window_type(),
                "linux_gtk: DrawingArea gdk::Window after layout pump"
            );
            if w <= 1 || h <= 1 {
                tracing::warn!(
                    size = ?(w, h),
                    "linux_gtk: DrawingArea still 0/1 pixel at handle-extract time — Servo \
                     will probably create a separate top-level surface"
                );
            }
        }

        let parent = Self {
            _overlay: overlay,
            drawing_area,
            webview_id: AtomicU64::new(0),
        };
        tracing::info!(
            total_ms = t_install_start.elapsed().as_millis() as u64,
            "linux_gtk: install complete"
        );
        Ok(parent)
    }

    /// Record the `webview_id` for this parent's renderer. Called by
    /// the renderer-bridge code immediately after `ServoRenderer::new_linux`
    /// returns. Once set, GTK signal handlers wired by
    /// [`Self::wire_input_forwarding`] forward pointer events to that
    /// id; before this is called signal handlers no-op via the
    /// `webview_id == 0` early-return inside `qsl_renderer::forward`.
    pub fn set_webview_id(&self, id: u64) {
        self.webview_id.store(id, Ordering::Relaxed);
    }

    /// Wire GTK pointer signal handlers to forward into Servo. Must
    /// be called once after the parent has been leaked into
    /// `'static` storage and registered against a `webview_id`. The
    /// closures capture `&'static LinuxGtkParent` so they can re-read
    /// the id atomically on every event — letting them survive the
    /// brief gap between widget construction and renderer install
    /// without dispatching to a stale id.
    pub fn wire_input_forwarding(parent: &'static LinuxGtkParent) {
        // Wire pointer events into Servo's input pipeline. GDK
        // delivers `(x, y)` in widget-local coordinates in device
        // pixels — the same units `WebViewPoint::Device` expects.
        // Each handler returns `Stop` so GTK doesn't bubble the
        // event back up to the webview underneath.
        parent
            .drawing_area
            .connect_button_press_event(move |_w, ev| {
                let id = parent.webview_id.load(Ordering::Relaxed);
                let (x, y) = ev.position();
                qsl_renderer::forward_pointer_button_press(id, ev.button(), x as f32, y as f32);
                glib::Propagation::Stop
            });
        parent
            .drawing_area
            .connect_button_release_event(move |_w, ev| {
                let id = parent.webview_id.load(Ordering::Relaxed);
                let (x, y) = ev.position();
                qsl_renderer::forward_pointer_button_release(id, ev.button(), x as f32, y as f32);
                glib::Propagation::Stop
            });
        parent
            .drawing_area
            .connect_motion_notify_event(move |_w, ev| {
                let id = parent.webview_id.load(Ordering::Relaxed);
                let (x, y) = ev.position();
                qsl_renderer::forward_pointer_move(id, x as f32, y as f32);
                glib::Propagation::Stop
            });
        parent
            .drawing_area
            .connect_leave_notify_event(move |_w, _ev| {
                let id = parent.webview_id.load(Ordering::Relaxed);
                qsl_renderer::forward_pointer_left_viewport(id);
                glib::Propagation::Stop
            });
        // Wheel/scroll forwarding. GDK reports either a discrete
        // `ScrollDirection::{Up,Down,Left,Right}` (mouse wheel notch)
        // or `ScrollDirection::Smooth` with `delta()` in scroll units
        // (touchpad two-finger). GDK's sign convention is "user wants
        // to move the viewport in this direction"; Servo's
        // `WheelDelta` says "view scrolls in this direction" with the
        // opposite sign, so we negate before forwarding.
        parent.drawing_area.connect_scroll_event(move |_w, ev| {
            use gdk::ScrollDirection;
            let id = parent.webview_id.load(Ordering::Relaxed);
            let (x, y) = ev.position();
            let (dx, dy) = match ev.direction() {
                ScrollDirection::Up => (0.0_f32, 1.0_f32),
                ScrollDirection::Down => (0.0, -1.0),
                ScrollDirection::Left => (1.0, 0.0),
                ScrollDirection::Right => (-1.0, 0.0),
                ScrollDirection::Smooth => {
                    let (sdx, sdy) = ev.delta();
                    (-(sdx as f32), -(sdy as f32))
                }
                _ => (0.0, 0.0),
            };
            qsl_renderer::forward_pointer_wheel(id, dx, dy, x as f32, y as f32);
            glib::Propagation::Stop
        });
    }

    /// Move and resize the DrawingArea to overlap the reader-body
    /// slot in window-relative coordinates. Called from the UI's
    /// `ResizeObserver` whenever the reader column's bounding rect
    /// changes (window resize, splitter drag, etc.). Must run on
    /// the GTK main thread — Tauri commands invoke this through
    /// `app_handle.run_on_main_thread`.
    pub fn set_position(&self, x: i32, y: i32, w: i32, h: i32) {
        // Clamp to non-negative; CSS rects can be slightly negative
        // during transitions and GTK's setters reject some negative
        // inputs.
        let x = x.max(0);
        let y = y.max(0);
        let w = w.max(1);
        let h = h.max(1);
        self.drawing_area.set_margin_start(x);
        self.drawing_area.set_margin_top(y);
        self.drawing_area.set_size_request(w, h);
    }

    /// Move the DrawingArea entirely off-screen. Used when the user
    /// deselects a message or opens the compose pane — the reader
    /// pane shows a placeholder text from Dioxus, and Servo's
    /// surface should be invisible.
    pub fn hide(&self) {
        self.drawing_area.set_margin_start(10_000);
        self.drawing_area.set_margin_top(0);
        self.drawing_area.set_size_request(1, 1);
    }

    /// Return the raw display + window handles for Servo's
    /// `WindowRenderingContext`. Panics if the DrawingArea's
    /// `gdk::Window` isn't realized — should never happen after
    /// `install` returned `Ok` since we explicitly realize.
    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        let gdk_window = self
            .drawing_area
            .window()
            .expect("DrawingArea must be realized before handles()");
        let display = gdk_window.display();

        // Dispatch on the active display backend. GTK 3 on Linux
        // runs on either Wayland or X11/XWayland depending on the
        // session and `GDK_BACKEND` env. `dynamic_cast` is the
        // canonical gtk-rs type check: it consumes the object and
        // returns it wrapped in the more specific type on match or
        // the original on miss.
        if let Ok(x11_display) = display.clone().dynamic_cast::<gdkx11::X11Display>() {
            let x11_window = gdk_window
                .clone()
                .dynamic_cast::<gdkx11::X11Window>()
                .expect("X11 display paired with non-X11 window — should be unreachable");
            extract_x11(&x11_window, &x11_display)
        } else {
            extract_wayland(&gdk_window, &display)
        }
    }
}

impl HasDisplayHandle for LinuxGtkParent {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        let (raw, _) = self.handles();
        // SAFETY: the raw handle references GDK-owned storage that
        // outlives `self`. Both the `gdk::Display` and the
        // DrawingArea's `gdk::Window` are anchored to the Tauri
        // `ApplicationWindow`, which lives for the process's
        // lifetime. `LinuxGtkParent` is leaked into `GTK_PARENT`,
        // so it isn't dropped until the app exits.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl HasWindowHandle for LinuxGtkParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let (_, raw) = self.handles();
        // SAFETY: see `display_handle`.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn extract_wayland(
    gdk_window: &gdk::Window,
    display: &gdk::Display,
) -> (RawDisplayHandle, RawWindowHandle) {
    // SAFETY: `GdkWaylandDisplay` / `GdkWaylandWindow` are opaque
    // re-types of `GdkDisplay` / `GdkWindow` whose only purpose is
    // giving the Wayland FFI a more specific parameter type; we've
    // confirmed the base display isn't X11, and every GDK backend
    // on Linux is either X11 or Wayland, so the base pointer is
    // safely reinterpretable as the Wayland-specific opaque type.
    // Both returned pointers (`wl_display`, `wl_surface`) are owned
    // by GDK for the lifetime of the display / window. The
    // `gdk::Display` lives for the process; the DrawingArea (and
    // thus the `gdk::Window`) is leaked in
    // `renderer_bridge::install_servo_renderer`, so the raw handles
    // stay valid for any `borrow_raw` later.
    unsafe {
        // Annotate the generic `to_glib_none()` return type so
        // rustc picks the `*mut GdkDisplay` / `*mut GdkWindow`
        // impl (each type implements `ToGlibPtr` multiple times
        // across the backend-specific wrapper types).
        let gdk_display_ptr: *mut gdk::ffi::GdkDisplay = ToGlibPtr::to_glib_none(display).0;
        let gdk_window_ptr: *mut gdk::ffi::GdkWindow = ToGlibPtr::to_glib_none(gdk_window).0;

        let wl_display_obj_ptr = gdk_display_ptr.cast::<gdk_wayland_sys::GdkWaylandDisplay>();
        let wl_window_obj_ptr = gdk_window_ptr.cast::<gdk_wayland_sys::GdkWaylandWindow>();

        let wl_display_ptr =
            gdk_wayland_sys::gdk_wayland_display_get_wl_display(wl_display_obj_ptr);
        let wl_surface_ptr = gdk_wayland_sys::gdk_wayland_window_get_wl_surface(wl_window_obj_ptr);

        let display_handle = WaylandDisplayHandle::new(
            NonNull::new(wl_display_ptr.cast::<c_void>())
                .expect("gdk_wayland_display_get_wl_display returned null"),
        );
        let window_handle = WaylandWindowHandle::new(
            NonNull::new(wl_surface_ptr.cast::<c_void>())
                .expect("gdk_wayland_window_get_wl_surface returned null"),
        );

        (
            RawDisplayHandle::Wayland(display_handle),
            RawWindowHandle::Wayland(window_handle),
        )
    }
}

fn extract_x11(
    x11_window: &gdkx11::X11Window,
    x11_display: &gdkx11::X11Display,
) -> (RawDisplayHandle, RawWindowHandle) {
    // SAFETY: the gdkx11 FFI functions take `*mut GdkX11Display` /
    // `*mut GdkX11Window`; we supply them via `ToGlibPtr` on the
    // typed wrappers above. GDK owns the returned `Display*` / XID
    // for the lifetime of the display / window; same leak reasoning
    // as `extract_wayland`. `screen_number` defaults to 0 — the
    // reader pane doesn't do multi-screen DPI math, and
    // `XlibDisplayHandle`'s `screen` field is advisory.
    unsafe {
        let xdisplay = gdkx11::ffi::gdk_x11_display_get_xdisplay(x11_display.to_glib_none().0);
        let xid = gdkx11::ffi::gdk_x11_window_get_xid(x11_window.to_glib_none().0);

        let display_handle = XlibDisplayHandle::new(NonNull::new(xdisplay.cast::<c_void>()), 0);
        let window_handle = XlibWindowHandle::new(xid);

        (
            RawDisplayHandle::Xlib(display_handle),
            RawWindowHandle::Xlib(window_handle),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // We can't construct a real LinuxGtkParent in a unit test (GTK
    // needs a display + main loop running), so we exercise the
    // registry's address-keyed bookkeeping with a sentinel pointer
    // we never dereference.
    fn fresh_sentinel() -> &'static LinuxGtkParent {
        // SAFETY: the registry only stores and returns the pointer.
        // No code path in these tests dereferences it.
        unsafe { &*std::ptr::NonNull::<LinuxGtkParent>::dangling().as_ptr() }
    }

    #[test]
    fn parent_registry_round_trip() {
        clear_registry_for_test();
        let p = fresh_sentinel();
        register_parent("main", p);
        assert!(parent("main").is_some());
        assert!(parent("missing").is_none());
        remove_parent("main");
        assert!(parent("main").is_none());
    }

    #[test]
    fn registry_overwrites_same_label() {
        clear_registry_for_test();
        register_parent("main", fresh_sentinel());
        register_parent("main", fresh_sentinel());
        assert!(parent("main").is_some());
    }
}
