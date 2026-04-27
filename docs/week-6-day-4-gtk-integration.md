<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Phase 0 Week 6 Day 4 — Linux GTK child-widget integration

**Status:** design + implementation plan, **no code landed**.

This document is the Day 4 deliverable's equivalent of
`docs/week-6-day-2-notes.md` but written *before* the implementation
session rather than after. It captures enough of the plan that the
next Linux-hardware-owning session can walk through it straight to
code, without re-doing the research.

## Why a plan instead of code

Day 2 shipped a "separate OS window for Servo" design that meets
the trait contract but diverges from the `docs/servo-composition.md`
§4.3 target ("pack the Servo surface *inside* Tauri's main GTK
window, alongside the Dioxus chrome"). Day 4 is supposed to close
that gap.

Running through the work in detail exposed a harder constraint: the
runtime path still hits the NVIDIA EGL-Wayland surfman bug
documented in `docs/week-6-day-2-notes.md` (upstream draft at
`docs/upstream/surfman-explicit-sync.md`). Writing the GTK
reparenting code without being able to run it end-to-end would ship
~150 lines of `unsafe` FFI against `gdkx11::ffi::*` and
`gdkwayland::ffi::*` that we can't validate on the session's
hardware — the NVIDIA bug aborts the process before the reparent
even renders. That's a worse state than no code: it converts a
live design gap into silent tech debt.

The plan below is the explicit handoff: the next session with
Intel/AMD Linux hardware, or running after the upstream surfman fix
lands, picks this doc up and turns it into a one- or two-hundred-
line PR with confidence that the approach will actually work.

## Target shape

```text
┌────────────── gtk::ApplicationWindow (Tauri-owned) ──────────────┐
│                                                                  │
│ ┌────────────── gtk::Paned (horizontal, NEW) ──────────────────┐ │
│ │                                                              │ │
│ │  ┌─── webkit2gtk::WebView ───┐ ┌─── gtk::DrawingArea ──────┐ │ │
│ │  │ (original, reparented)    │ │ (new, hosts Servo paint)  │ │ │
│ │  │                           │ │                           │ │ │
│ │  │ Dioxus chrome:            │ │ ServoRenderer attaches    │ │ │
│ │  │ sidebar, message list     │ │ WindowRenderingContext    │ │ │
│ │  │                           │ │ to this widget's          │ │ │
│ │  │                           │ │ gdk::Window               │ │ │
│ │  └───────────────────────────┘ └───────────────────────────┘ │ │
│ │                                                              │ │
│ └──────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

## Implementation plan

### Step 1: Dependency additions

`apps/desktop/src-tauri/Cargo.toml`, under
`[target.'cfg(target_os = "linux")'.dependencies]`:

```toml
gtk = "0.18"
gdk = "0.18"
gdkx11 = "0.18"      # X11 raw-handle extraction
gdkwayland-sys = "0.18"  # Wayland raw-handle FFI
x11-dl = "2"         # `Xlib::open()` handle for XCreateSimpleWindow peers
```

All of these are already transitive deps (from Tauri 2's Linux
backend — see `cargo tree`), so lockfile churn is minimal. Adding
them as direct deps only promotes them to the desktop crate's
public visibility.

### Step 2: Workspace `unsafe_code` lint

Change `Cargo.toml` workspace lints from `forbid` to `deny`:

```toml
[workspace.lints.rust]
unsafe_code = "deny"
```

`forbid` blocks even per-module `#[allow(unsafe_code)]`. `deny`
keeps unsafe rejected by default but allows a documented per-block
opt-in at the exact call sites that need it. The unsafe blocks are
limited to:

1. `gdk::Window::from_glib_full(raw_ptr)` — taking ownership of a
   C-owned `GdkWindow*`.
2. `raw_window_handle::{DisplayHandle, WindowHandle}::borrow_raw` —
   these are unsafe because they assert pointer validity across the
   borrow lifetime.
3. `gdk_x11_display_get_xdisplay` / `gdk_wayland_window_get_wl_surface`
   FFI calls.

### Step 3: New module — `apps/desktop/src-tauri/src/linux_gtk.rs`

Helper module that:

- Takes a `gtk::ApplicationWindow` (from `tauri::Window::gtk_window()`).
- Restructures its child hierarchy to inject a `gtk::Paned` with the
  original content on one side and a new `gtk::DrawingArea` on the
  other.
- Realizes the DrawingArea (ensures it has a backing `gdk::Window`).
- Returns a `LinuxGtkParent` struct implementing
  `raw_window_handle::HasDisplayHandle` and
  `raw_window_handle::HasWindowHandle` against the DrawingArea's
  `gdk::Window`.

Skeleton (pseudocode — filenames + call shapes are real, unsafe
blocks are explicit):

```rust
#![allow(unsafe_code)]
// Justification: Linux GTK/X11/Wayland raw-handle extraction. Every
// unsafe block is a single FFI boundary crossing or a documented
// raw_window_handle::borrow_raw call. See docs/week-6-day-4-gtk-
// integration.md step 2.

use gdk::prelude::*;
use gtk::prelude::*;
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    WindowHandle, XlibDisplayHandle, XlibWindowHandle,
};
use std::ffi::c_void;
use std::ptr::NonNull;

pub struct LinuxGtkParent {
    _paned: gtk::Paned,            // keeps the hierarchy alive
    drawing_area: gtk::DrawingArea, // the surface Servo paints into
}

impl LinuxGtkParent {
    pub fn install(
        app_window: &gtk::ApplicationWindow,
        reader_width: i32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Pull the existing child (likely the webkit2gtk-wrapping
        // container) out of the ApplicationWindow.
        let original = app_window
            .child()
            .ok_or("main window has no child widget")?;
        app_window.remove(&original);

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.pack1(&original, true, true);

        let drawing_area = gtk::DrawingArea::new();
        drawing_area.set_size_request(reader_width, -1);
        paned.pack2(&drawing_area, true, false);

        app_window.set_child(Some(&paned));
        app_window.show_all();
        drawing_area.realize();

        Ok(Self {
            _paned: paned,
            drawing_area,
        })
    }

    /// Return the raw handles for Servo's `WindowRenderingContext`.
    /// Panics if the DrawingArea's `gdk::Window` is not realized —
    /// should never happen after `install` returned Ok.
    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        let gdk_window = self
            .drawing_area
            .window()
            .expect("DrawingArea must be realized");
        let display = gdk_window.display();

        // Dispatch on the active display backend. Tauri 2 on Linux
        // supports both Wayland and X11; which one is active at
        // runtime is up to the host session.
        if let Some(wayland_display) = display
            .downcast_ref::<gdkwayland::WaylandDisplay>()
        {
            extract_wayland(&gdk_window, wayland_display)
        } else if let Some(x11_display) =
            display.downcast_ref::<gdkx11::X11Display>()
        {
            extract_x11(&gdk_window, x11_display)
        } else {
            panic!("unsupported GDK backend — only Wayland and X11 are handled");
        }
    }
}

fn extract_wayland(
    gdk_window: &gdk::Window,
    display: &gdkwayland::WaylandDisplay,
) -> (RawDisplayHandle, RawWindowHandle) {
    // SAFETY: the FFI getters below return pointers owned by GDK
    // that live as long as the gdk::Display / gdk::Window. Both
    // outlive LinuxGtkParent (they're anchored to the Tauri
    // ApplicationWindow, itself owned by tauri::App for the life
    // of the process).
    unsafe {
        let wl_display_ptr = gdkwayland_sys::gdk_wayland_display_get_wl_display(
            display.as_ref().to_glib_none().0,
        );
        let wl_surface_ptr = gdkwayland_sys::gdk_wayland_window_get_wl_surface(
            gdk_window.to_glib_none().0,
        );

        let display_handle = WaylandDisplayHandle::new(
            NonNull::new(wl_display_ptr.cast::<c_void>())
                .expect("wl_display non-null"),
        );
        let window_handle = WaylandWindowHandle::new(
            NonNull::new(wl_surface_ptr.cast::<c_void>())
                .expect("wl_surface non-null"),
        );

        (
            RawDisplayHandle::Wayland(display_handle),
            RawWindowHandle::Wayland(window_handle),
        )
    }
}

fn extract_x11(
    gdk_window: &gdk::Window,
    display: &gdkx11::X11Display,
) -> (RawDisplayHandle, RawWindowHandle) {
    // SAFETY: same reasoning as extract_wayland — pointers owned
    // by GDK for the lifetime of the display/window.
    unsafe {
        let xdisplay = gdkx11::ffi::gdk_x11_display_get_xdisplay(
            display.as_ref().to_glib_none().0,
        );
        let xid = gdkx11::ffi::gdk_x11_window_get_xid(
            gdk_window.to_glib_none().0,
        );
        let screen = (*display.default_screen().to_glib_none().0).screen_num as i32;

        let display_handle = XlibDisplayHandle::new(
            NonNull::new(xdisplay.cast::<c_void>()),
            screen,
        );
        let window_handle = XlibWindowHandle::new(xid as u64);

        (
            RawDisplayHandle::Xlib(display_handle),
            RawWindowHandle::Xlib(window_handle),
        )
    }
}

impl HasDisplayHandle for LinuxGtkParent {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        let (raw, _) = self.handles();
        // SAFETY: the raw handle references GDK-owned storage that
        // outlives `self` (the Tauri ApplicationWindow owns both
        // DrawingArea and its gdk::Window).
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl HasWindowHandle for LinuxGtkParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let (_, raw) = self.handles();
        // SAFETY: see display_handle.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}
```

### Step 4: Rewire `renderer_bridge::install_servo_renderer`

Replace the current `tauri::window::WindowBuilder::new(&app_handle,
"servo-reader")` path with:

```rust
#[cfg(target_os = "linux")]
let parent = {
    let main_window = app
        .get_webview_window("main")
        .ok_or("main window not present when installing Servo renderer")?;
    let gtk_app_window = main_window.gtk_window()
        .map_err(|e| format!("cannot get GTK ApplicationWindow: {e}"))?;
    linux_gtk::LinuxGtkParent::install(&gtk_app_window, READER_WINDOW_WIDTH as i32)?
};

let servo_renderer = ServoRenderer::new_linux(
    Arc::clone(&dispatcher),
    &parent,
    PhysicalSize::new(READER_WINDOW_WIDTH, READER_WINDOW_HEIGHT),
)?;
```

Store `parent` in `AppState` so the Paned + DrawingArea widgets stay
alive for the lifetime of the app (dropping them would destroy the
GDK window Servo is painting to).

### Step 5: Handle size changes

Connect `app_window.connect_size_allocate(|_, alloc| { … })` (GTK3
signal) to forward new dimensions to `WebView::resize` via the
`MainThreadDispatch`. Without this, resizing the Tauri window leaves
the Servo WebView stuck at the initial pane size.

Sketch:

```rust
let dispatcher_clone = Arc::clone(&dispatcher);
let drawing_area_clone = parent.drawing_area.clone();
drawing_area_clone.connect_size_allocate(move |_, alloc| {
    let size = PhysicalSize::new(alloc.width() as u32, alloc.height() as u32);
    dispatcher_clone.dispatch(Box::new(move || {
        super::MAIN_THREAD_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state.webview.resize(size);
            }
        });
    }));
});
```

### Step 6: Delete the sibling-window code path

Remove the `tauri::window::WindowBuilder::new(&app_handle,
"servo-reader")` lines in `renderer_bridge.rs`. Remove the `unstable`
Tauri feature if no other caller needs it. Drop the `servo-reader`
Tauri window config if it was in `tauri.conf.json` (it wasn't, but
double-check before landing).

## Testing plan (for the hardware session picking this up)

Runtime gates that need to pass before this is safe to merge:

1. App launches without the NVIDIA EGL-Wayland protocol error. This
   requires either:
   - Running on Intel/AMD hardware, or
   - Running on a compositor that doesn't advertise
     `wp_linux_drm_syncobj_surface_v1`, or
   - The upstream surfman fix (see
     `docs/upstream/surfman-explicit-sync.md`) having landed.
2. The Dioxus chrome renders in the left pane and "Hello from Servo"
   renders in the right pane, simultaneously visible.
3. Resizing the Tauri window resizes both panes cleanly, and Servo's
   content reflows (not pixel-stretched).
4. Clicking the test link in the Servo pane fires the
   `on_link_click` callback (verify via the existing `tracing::info!`
   in `renderer_bridge::install_servo_renderer`).
5. The corpus tests still pass (they use `SoftwareRenderingContext`
   and shouldn't be affected, but worth confirming).
6. `cargo clippy --workspace --all-targets -- -D warnings` clean.
7. `reuse lint` clean.

## Open questions for the implementation session

- Does `gtk::Paned`'s default divider position land where we want, or
  does the split need explicit `set_position(reader_width)`?
- `gdk::Window::from_glib_full` vs `gdk::Window::from_glib_none` — is
  the returned pointer refcounted? Get this right or leak GDK state.
- Should the reader pane default to right-side (`pack2`) or a
  configurable layout? Current sketch assumes right-side; Dioxus UI
  may want a different default.
- `set_size_request` vs `set_position` for initial sizing — Paned's
  position API is easier but the `size_request` path propagates
  better across DPI changes.
- The `connect_size_allocate` sketch races the DrawingArea's own
  realization. Needs the "defer first resize until after show_all
  completes" pattern that wry uses — check wry's source.

## Relationship to other Week 6 deferred work

- Day 4 GTK integration and `docs/week-6-day-2-notes.md`'s NVIDIA
  Wayland escalation path share the same critical-path dependency:
  either the upstream surfman fix ships, or a non-NVIDIA Linux host
  validates both pieces simultaneously. A single session closes
  both.
- The Windows port (Day 3, PR #20) has the analogue of this GTK
  work — `CreateWindowExW` child `HWND` — also deferred. Similar
  scope; similar "wait for Windows hardware" gating.
- macOS (Day 2 `new_macos`, UNVERIFIED) has the same shape via
  `NSView` subviews. When one platform's child-surface integration
  lands validated, the other two have concrete reference impls to
  mirror.
