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
//! module reparents that hierarchy through a `gtk::Paned`, adds a
//! sibling `gtk::DrawingArea`, and hands the DrawingArea's raw
//! display/window handles to Servo's `WindowRenderingContext`.
//!
//! The shape after `install`:
//!
//! ```text
//! gtk::ApplicationWindow (Tauri-owned)
//! └── gtk::Paned (new, horizontal)
//!     ├── webkit2gtk::WebView (pack1: original Dioxus chrome)
//!     └── gtk::DrawingArea   (pack2: Servo reader surface)
//! ```
//!
//! See `docs/week-6-day-4-gtk-integration.md` for the full design
//! and the hardware-gating rationale; `docs/upstream/surfman-explicit-sync.md`
//! for why the native NVIDIA EGL-Wayland path is broken and we rely
//! on the Mesa llvmpipe fallback (`capytain_renderer::apply_nvidia_wayland_workaround`).

use std::ffi::c_void;
use std::ptr::NonNull;

use gdk::prelude::*;
use glib::translate::ToGlibPtr;
use gtk::prelude::*;
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle, WindowHandle, XlibDisplayHandle, XlibWindowHandle,
};

/// Result of reparenting Tauri's main `ApplicationWindow` into a
/// horizontal `Paned`. Holds both the Paned and the new DrawingArea
/// so their lifetimes are tied to the Tauri app; dropping this
/// struct would destroy the GDK window Servo is painting to.
pub struct LinuxGtkParent {
    /// Kept alive so the widget hierarchy doesn't get torn down.
    _paned: gtk::Paned,
    /// The child widget Servo paints into. Public so callers can
    /// wire `connect_size_allocate` for reader-pane resize events.
    pub drawing_area: gtk::DrawingArea,
}

impl LinuxGtkParent {
    /// Reparent the current child of `app_window` inside a new
    /// `Paned`, packing a fresh `DrawingArea` as the right-hand
    /// side. Realizes the DrawingArea so its backing `gdk::Window`
    /// is available before `handles()` is called.
    pub fn install(
        app_window: &gtk::ApplicationWindow,
        reader_width: i32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Pull the existing child (Tauri's webkit2gtk container) out
        // of the ApplicationWindow so we can wrap it in a Paned.
        let original = app_window
            .child()
            .ok_or("main window has no child widget")?;
        app_window.remove(&original);

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        // pack1 = left/top (Dioxus chrome — can grow/shrink).
        // pack2 = right/bottom (Servo reader — fixed-ish).
        paned.pack1(&original, true, true);

        let drawing_area = gtk::DrawingArea::new();
        drawing_area.set_size_request(reader_width, -1);

        // `app_paintable(true)` tells GTK's default draw path to
        // leave the widget's backing surface alone so whatever
        // Servo's `WindowRenderingContext` writes to the
        // `gdk::Window` stays visible. Without this flag, GTK3
        // clears the DrawingArea to the theme background on every
        // draw cycle and Servo's paint disappears as soon as the
        // widget is ever repainted.
        drawing_area.set_app_paintable(true);

        paned.pack2(&drawing_area, true, false);

        app_window.add(&paned);
        app_window.show_all();

        // Explicit split so the reader pane gets a predictable
        // width on launch. `Paned`'s default position is
        // unspecified (depends on first-child min-request); pinning
        // it keeps the ratio stable across theme changes.
        let allocation = app_window.allocation();
        let total_width = allocation.width().max(reader_width + 200);
        paned.set_position(total_width - reader_width);

        // Force realization so `gdk::Window` is available. GTK 3
        // normally defers realization until the widget is drawn;
        // Servo wants the window handle immediately.
        drawing_area.realize();

        // Force the DrawingArea's `gdk::Window` to have a real
        // native backing (wl_subsurface on Wayland, separate X
        // Window on X11). GTK3 by default implements child widget
        // windows as client-side regions inside the parent's native
        // surface — cheap, but surfman can't bind a GL context to a
        // client-side region because there's no real surface to
        // swap buffers on. Without `ensure_native`, Servo paints
        // into a synthetic window that GDK never presents to the
        // compositor, and the reader pane stays blank.
        if let Some(gdk_window) = drawing_area.window() {
            gdk_window.ensure_native();
        }

        Ok(Self {
            _paned: paned,
            drawing_area,
        })
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
        // the original on miss. The gdkx11 FFI takes
        // `*mut GdkX11Display` / `*mut GdkX11Window`, so we need
        // both typed wrappers for the X11 branch; the gdkwayland
        // FFI takes generic pointers and gets them via glib's
        // `ToGlibPtr` on the base `gdk::Display` / `gdk::Window`.
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
        // lifetime. `LinuxGtkParent` is stored in `AppState` and
        // isn't dropped until the app exits.
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
    // `gdk::Display` lives for the process; the DrawingArea
    // (and thus the `gdk::Window`) is leaked in
    // `renderer_bridge::build_servo_renderer`, so the raw handles
    // stay valid for any `borrow_raw` later.
    unsafe {
        // Annotate the generic `to_glib_none()` return type so rustc
        // picks the `*mut GdkDisplay` / `*mut GdkWindow` impl (each
        // type implements `ToGlibPtr` multiple times across the
        // backend-specific wrapper types).
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
