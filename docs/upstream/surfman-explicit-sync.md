<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Upstream bug report — `servo/surfman`

**Status:** **filed as
[servo/surfman#354](https://github.com/servo/surfman/issues/354)**
on 2026-04-22. This file is kept as the authored-locally copy so
the body stays version-controlled alongside the investigation it
summarizes (`docs/week-6-day-2-notes.md` § NVIDIA), and so future
follow-up comments or re-filings against a different repo can draft
from a tracked source.

**Target repo:** `servo/surfman` (26 open issues, actively
maintained). If the surfman triage redirects the report (e.g. to
`servo/servo` or upstream NVIDIA), update the "Status" line above
and the linked issue.

**Issue title as filed:** `Wayland: missing acquire point on
wp_linux_drm_syncobj_surface_v1 commit with NVIDIA EGL driver (KWin
disconnects with protocol error)`

---

## Summary

On Wayland with the proprietary NVIDIA driver + KWin compositor, the
first surfman commit triggers a `wp_linux_drm_syncobj_surface_v1`
protocol error the instant a `WindowRenderingContext`-backed Servo
`WebView` is installed — "explicit sync is used, but no acquire
point is set". KWin disconnects; Gdk exits with `Error 71 (Protocol
error)` within ~50ms of the renderer becoming active. The embedder
process dies with exit code 1.

## Environment

- **Compositor:** KWin (KDE Plasma 6) on Wayland, session
  `wayland-0`
- **GPU / driver:** NVIDIA GeForce RTX 5070 Ti, proprietary driver
  595.58.03
- **OS:** CachyOS (Arch-derivative), Linux 7.0.0
- **surfman:** 0.11.0 (from crates.io, transitive via
  `servo = "0.1.0"`)
- **Servo:** 0.1.0 (released 2026-04-13)
- **rust:** 1.94.0 stable

## Minimal reproduction shape

In a Tauri 2 app, create a plain `tauri::window::Window` (no
webview) and hand its `raw-window-handle` to
`servo::WindowRenderingContext::new`, then build a Servo instance +
`WebView` with that context and call `load(data:text/html;…)` on
the WebView. No other surfaces share the `wl_surface`; this isn't
a dual-context conflict.

Relevant log excerpt with `WAYLAND_DEBUG=client`:

```
[...] -> xdg_toplevel#46.set_title("Capytain Reader (Servo)")
INFO  paint::painter: Running on NVIDIA GeForce RTX 5070 Ti/PCIe/SSE2 with OpenGL version 3.2.0 NVIDIA 595.58.03
INFO  capytain-desktop: Servo renderer installed
[Display Queue] wl_display#1.error(wp_linux_drm_syncobj_surface_v1#48, 4,
    "explicit sync is used, but no acquire point is set")
Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
```

KWin's interpretation: the client (NVIDIA EGL-Wayland driver, via
surfman's Wayland backend) subscribed to
`wp_linux_drm_syncobj_surface_v1` for the surface but committed a
buffer without setting the acquire timeline point that the protocol
requires. KWin's protocol validator rejects the commit as a fatal
protocol error.

surfman 0.11.0 itself does not reference the `wp_linux_drm_syncobj`
strings — grep is clean — so the protocol subscription appears to
be happening inside NVIDIA's EGL-Wayland library when it sees KWin
advertising the interface. From the surfman side, that means the
first `EGLSwapBuffers` (or whatever path triggers the initial
commit) is missing the per-surface acquire-point wiring that
NVIDIA's driver expects to participate in.

## Workarounds confirmed in testing

| Workaround | Result |
|---|---|
| `MESA_LOADER_DRIVER_OVERRIDE=llvmpipe LIBGL_ALWAYS_SOFTWARE=1 __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json` | **Bypasses the error.** Forces Mesa llvmpipe software GL instead of the NVIDIA EGL-Wayland path; no protocol error, app continues to run. Confirms the bug is NVIDIA-EGL-specific, not generic-Wayland. |
| `GDK_BACKEND=x11` (force XWayland) | Gdk falls back to XWayland but `parent.display_handle()` on the Tauri plain window then returns `HandleNotAvailable`, so the Servo path silently disables and the app runs without the reader. Unrelated to the sync issue but worth noting. |
| `KWIN_DRM_NO_EXPLICIT_SYNC=1` | No effect. That env affects KWin's own DRM scanout sync, not whether it advertises `wp_linux_drm_syncobj_surface_v1` to clients. |
| `LIBGL_ALWAYS_SOFTWARE=1` alone | NVIDIA's EGL-Wayland driver ignores it. |
| Replacing the Tauri `WebviewWindow` parent with a plain `tauri::window::Window` (no webview) | Same error. Rules out "two GL contexts fighting over one `wl_surface`" — the error reproduces with Servo as the only subscriber. |

## Why filing against surfman

The protocol object comes from NVIDIA's closed-source EGL-Wayland
layer, which participates in the protocol automatically when it
sees KWin advertising it. But the observable fix has to live
somewhere the embedder can reach — either in surfman's Wayland
backend's first-commit path (setting an acquire point before
calling `EGLSwapBuffers`) or in an embedder-facing opt-out knob.
Filing here since surfman is the Servo-side Wayland owner; happy
to re-file against `servo/servo` or upstream NVIDIA if preferred.

## Impact on downstream

For Capytain (a pure-Rust email client embedding Servo for the
reader pane), this is the single blocker between compile-validated
Servo integration on Linux and runtime-validated end-to-end
rendering on the host hardware class that's currently most common
in Linux dev boxes (NVIDIA + Wayland + KWin or similar). The
`SoftwareRenderingContext` path works fine as a headless corpus
harness but isn't viable as the user-facing reader pane because
it loses native text selection, scrolling, and the
`prefers-color-scheme` media query path.

Happy to run deeper diagnostics or test patches. The full Day 2
investigation including the WAYLAND_DEBUG protocol dump is linked
in the summary above.
