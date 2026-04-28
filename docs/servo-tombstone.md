<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Servo reader implementation — paused 2026-04-28

> **Status:** the Servo-backed email renderer is **disabled by default**
> on `main` as of 2026-04-28. The desktop app's Cargo `default` feature
> set is `[]` (was `["servo"]`); the body is rendered in a
> `<iframe sandbox="allow-scripts" srcdoc="...">` inside webkit2gtk
> instead. The Servo code path stays in tree, gated by
> `#[cfg(feature = "servo")]`, and can be re-enabled with
> `cargo build -p qsl-desktop --features servo`.
>
> This doc is the consolidation point for *why* we paused, *what
> shipped before pausing*, and *what would be needed to bring it
> back*. Granular design and post-spike findings live in
> [`servo-composition.md`](servo-composition.md); this is the
> tombstone-and-revival index.

---

## 1. Why we paused

### 1.1 The triggering incident (2026-04-28)

Smoking the v0.1 release build on a hybrid AMD+NVIDIA laptop running
off the AMD GPU produced a **blank reader-pane body**. Headers
rendered correctly via Dioxus; the email body area was empty, with an
"unstyled rectangle" visible where the offscreen `gtk::DrawingArea`
(margin_start=10_000) had partially drifted onto the visible area.

Diagnostic state at the time:

- `qsl_desktop::renderer_bridge: Servo renderer installed` — fired
  for both `main` and the popup-reader window.
- `script::network_listener: Ignoring InitiatorType::Other resource
  "https://cdn.robinhood.com/..."` — Servo's script engine was alive
  and parsing the email's HTML.
- `messages_trust_sender` round-tripped IPC successfully.
- `messages_open_in_window` total=521 ms — the popup window's Servo
  install path completed.
- The `apply_nvidia_wayland_workaround` ran unconditionally on Linux,
  forcing `LIBGL_ALWAYS_SOFTWARE=1 + MESA_LOADER_DRIVER_OVERRIDE=
  llvmpipe`. On NVIDIA + Wayland that's necessary; on AMD it's a
  punishment that produced (as far as we got with diagnostics)
  transparent/blank GL frames composited over the Tauri webview.

**Working hypothesis** (not confirmed — abandoned in favor of the
swap): the surfman + llvmpipe + GTK X11 child-window + AMD Mesa stack
produces a GL surface that Servo paints into successfully but the
compositor doesn't blit visibly. The `gtk::DrawingArea` is real, sized,
positioned over `.reader-body-fill`, and receives Servo's frames; the
frames just don't show. The popup-reader window using the same
architecture would have shown the same blank result, but we didn't
finish that test.

### 1.2 The decision

Two paths were on the table:

1. **Gate the workaround on detected NVIDIA** (`lspci | grep -i
   nvidia` at startup) so AMD takes the hardware-accelerated Mesa
   path. Cheap if it works, but doesn't help if the issue is deeper
   than llvmpipe (e.g., the GTK X11 child-window compositor path on
   AMD's Mesa drivers).

2. **Drop Servo for v0.1, render the body in a sandboxed iframe
   inside webkit2gtk.** GPU-agnostic, well-trodden, abandons process
   isolation but keeps every other architectural win.

We picked (2) — see commit `experiment(reader): swap Servo for
webkit2gtk iframe`. Process isolation can come back in v0.2 if there's
appetite, after surfman/Servo's NVIDIA path stabilizes upstream and
ideally after Tauri 2 migrates to GTK 4 (which would also kill the
GTK 3 child-subsurface gap that motivated `GDK_BACKEND=x11`).

---

## 2. What shipped (the architecture, frozen)

The full pre-pause architecture, intact behind `--features servo`:

### 2.1 Cargo

- `qsl-renderer` crate: feature-gated `servo` flag pulls in
  `servo = "0.1"`, `raw-window-handle`, `dpi`, `image`. Without the
  flag, the crate re-exports `qsl_core::NullRenderer` and the
  always-on `link_cleaner` module.
- `qsl-desktop` Cargo: `default = ["servo"]` flipped to `default = []`
  on 2026-04-28. `servo = ["qsl-renderer/servo"]` feature line stays.
- Servo brings ~15 GB of build artifacts via cmake / clang /
  gstreamer / mozjs_sys. See
  [`servo-composition.md` §7](servo-composition.md).

### 2.2 Linux integration (the only platform that landed)

- **`apps/desktop/src-tauri/src/linux_gtk.rs`** — owns a
  `gtk::Overlay` per Tauri window. Each overlay holds a
  `gtk::DrawingArea` pinned offscreen at `margin_start=10_000`
  until JS pushes a real bounding rect.
- **`renderer_bridge.rs`** — installs a `qsl_renderer::ServoRenderer`
  per window, registered in `AppState::servo_renderers`. The Servo
  install runs on the GTK main thread via `run_on_main_thread` —
  `gtk::Overlay::new()` panics off-main.
- **`apps/desktop/ui/src/app.rs`** — JS-side `startReaderBodyTracker`
  function watches `.reader-body-fill`'s bounding rect with a
  ResizeObserver + `window.resize` listener, rAF-coalesced, and
  pushes `(x, y, w, h)` to Rust via `reader_set_position`. Rust
  passes the rect to GTK via `LinuxGtkParent::set_position`.
- **`commands/reader.rs`** — `reader_render`, `reader_set_position`,
  `reader_clear` Tauri commands. `reader_set_position` includes a
  trailing-edge 80 ms debounce on `renderer.resize` (added in PR #88
  to mitigate window-resize flicker — see [§3.4](#34-reader-pane-resize-flicker)).
- **`crates/core/src/reader_html.rs::compose_reader_html`** —
  composes the wrapper HTML document with a click-forwarder
  `<script>` that postMessages anchor URLs to `window.parent`
  (iframe path) or sets `window.location.href` (Servo path). Both
  paths are still wired; the wrapper is platform-agnostic.

### 2.3 NVIDIA Wayland workaround

`qsl_renderer::apply_nvidia_wayland_workaround()` (called from
`main.rs` before any GL touches) sets four env vars **only if
currently unset**:

| Var | Value | Reason |
|---|---|---|
| `MESA_LOADER_DRIVER_OVERRIDE` | `llvmpipe` | NVIDIA EGL-Wayland explicit-sync bug ([surfman#354](https://github.com/servo/surfman/issues/354)) — force Mesa software EGL |
| `LIBGL_ALWAYS_SOFTWARE` | `1` | Force software GL across the stack |
| `__EGL_VENDOR_LIBRARY_FILENAMES` | `/usr/share/glvnd/egl_vendor.d/50_mesa.json` | Pin libglvnd to Mesa's vendor JSON so NVIDIA's `10_nvidia.json` doesn't get auto-discovered |
| `GDK_BACKEND` | `x11` | GTK 3 Wayland can't subsurface child widgets; force XWayland for the DrawingArea |

The `GDK_BACKEND=x11` line is **GPU-agnostic** — it addresses the
GTK 3 subsurface gap, not NVIDIA. Even on AMD it's load-bearing for
the Servo overlay placement to work at all.

The first three lines are **NVIDIA-specific**. On AMD they push the
stack onto llvmpipe (CPU rendering), which on the 2026-04-28 laptop
appeared to produce blank composition.

Detail: [`upstream/surfman-explicit-sync.md`](upstream/surfman-explicit-sync.md).

---

## 3. Known bugs / friction (frozen state)

### 3.1 Blank render on AMD + llvmpipe (2026-04-28 incident)

The triggering issue. **Unresolved.** See [§1.1](#11-the-triggering-incident-2026-04-28).
First diagnostic step if reviving Servo: gate
`apply_nvidia_wayland_workaround` on `lspci | grep -i nvidia` so
AMD takes the hardware Mesa path. If hardware GL on AMD also
produces blank composition, the issue is deeper (GTK X11 child-window
+ Servo's GL context interaction) and needs a Servo-side
investigation.

### 3.2 Reader-render multi-fire

`KNOWN_ISSUES.md` entry #2: a single click on the
"Render test page in Servo" button logged `reader_render` ~6 times at
~200 ms intervals during PR #30 headless probing. Cause unclear —
possibly Dioxus's reactive cycle re-firing, possibly Servo's input
dispatch, possibly the `use_resource` re-running. **Not reproduced
in interactive sessions to date**, hence parked.

### 3.3 Ubuntu CI corpus `take_screenshot` hang

`KNOWN_ISSUES.md` entry #1: `WebView::take_screenshot` indefinitely
blocks on `wait_for_rendering_to_be_ready` in headless Ubuntu CI.
Needs an upstream Servo patch (timeout knob or skip-image-cache flag).
No tractable in-tree fix.

### 3.4 Reader-pane resize flicker

`KNOWN_ISSUES.md` entry #3: even with the 80 ms `renderer.resize`
debounce, continuous window-resize drags show a perceptible mismatch
frame because Servo's offscreen surface size lags its viewport
handle. Real fix would need a `WebView::pause_paint`-shaped knob
(not currently exposed by Servo 0.1.x) or a snapshot-blit shim in
`qsl_renderer`.

### 3.5 GTK 3 child-subsurface gap

Documented at length in
[`week-6-day-4-gtk-integration.md`](week-6-day-4-gtk-integration.md).
`gdk_window_ensure_native()` on a Wayland `GtkDrawingArea` creates
a new `xdg_toplevel` rather than a `wl_subsurface`, so the Servo
pane appears as a separate top-level window. The
`GDK_BACKEND=x11` workaround routes through XWayland to dodge this.
Real fix: Tauri 2 migrating to GTK 4 (GDK 4 has per-widget
subsurface support).

### 3.6 NVIDIA EGL-Wayland explicit-sync bug

[surfman/surfman#354](https://github.com/servo/surfman/issues/354) /
[`upstream/surfman-explicit-sync.md`](upstream/surfman-explicit-sync.md).
NVIDIA's closed-source EGL-Wayland layer auto-joins the explicit-sync
protocol without supplying an acquire timeline point; the first
surfman commit trips Wayland protocol error 71 on KWin and any
compositor that advertises `wp_linux_drm_syncobj_surface_v1`. Real
fix needs an NVIDIA driver update; workaround is the four env vars
in `apply_nvidia_wayland_workaround`.

### 3.7 Build cost

~15 GB of build artifacts, ~25 minutes of cold-cache build time. Not
a *bug* but a meaningful cost — most of `cargo build`'s wall-clock
time on QSL is Servo. Dropping the feature flag reclaims it.

---

## 4. What would need to happen to revive Servo

In rough priority order:

1. **AMD blank-render diagnosis.** Either (a) confirm hardware Mesa
   on AMD works once the workaround is gated on detected NVIDIA, or
   (b) reproduce in a minimal Servo-only harness outside QSL to
   surface what's actually failing in the GTK X11 + AMD GL stack.
   Without this, Servo is unshippable on AMD hardware.
2. **Hardware-detect the workaround.** `lspci | grep -i nvidia` (or a
   richer `wgpu`-style adapter probe) at startup; only set the
   software-EGL trio when NVIDIA is present. The `GDK_BACKEND=x11`
   line stays unconditional.
3. **Tauri 2 → GTK 4.** Eliminates the `GDK_BACKEND=x11`
   workaround entirely. Tracking upstream — Tauri's plan is to migrate
   eventually, no firm date.
4. **Surfman NVIDIA fix lands upstream.** `surfman#354` resolution
   would let us drop the software-EGL trio entirely on NVIDIA.
   Probably contingent on NVIDIA driver fix.
5. **`WebView::pause_paint` (or equivalent) lands in Servo 0.1.x.**
   Real fix for the resize-flicker structural issue.
6. **Process-isolation case re-evaluated.** With ammonia stripping
   scripts upstream and the iframe sandbox blocking same-origin
   access, the security delta of "Servo in a separate process" vs
   "iframe in webkit2gtk" is real but smaller than it looks. Worth
   re-litigating before re-investing.

---

## 5. The webkit-iframe replacement (current default)

For completeness, what replaced Servo on the default build:

- **`<iframe class="reader-body-iframe" sandbox="allow-scripts"
  srcdoc="{compose_reader_html(rendered)}">`** — webkit2gtk renders
  the sanitized HTML directly. `sandbox="allow-scripts"` (no
  `allow-same-origin`) keeps the iframe at a unique null origin: it
  can run the click forwarder but can't read parent
  cookies/localStorage, can't navigate top-frame, no forms, no
  pop-ups.
- **Click forwarder** in `compose_reader_html`: anchor click →
  `e.preventDefault()` → `window.parent.postMessage({type:
  'qsl-link-click', url}, '*')`. Parent's `installReaderLinkListener`
  catches `qsl-link-click` and invokes `open_external_url`, which
  validates scheme (`http`/`https`/`mailto`) and shells to
  `webbrowser::open`. Same end behaviour as the Servo navigation
  delegate, different wire.
- **Removed**: rect tracker, `reader_clear` IPC nudges, palette↔Servo
  overlay coordination, compose↔Servo overlay coordination. None of
  it exists in the iframe model — z-index handles overlap natively.

---

## 6. References

- [`servo-composition.md`](servo-composition.md) — pre-spike design
  + post-spike findings (Day 5)
- [`upstream/surfman-explicit-sync.md`](upstream/surfman-explicit-sync.md)
  — NVIDIA Wayland explicit-sync details + upstream link
- [`week-6-day-2-notes.md`](week-6-day-2-notes.md) — Wayland debugging
  notes (`WAYLAND_DEBUG=client`)
- [`week-6-day-4-gtk-integration.md`](week-6-day-4-gtk-integration.md)
  — GTK 3 child-subsurface gap details
- [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md) entries #1, #2, #3
- Memory: `~/.claude/projects/-home-johnathon-src-qsl/memory/project_nvidia_wayland.md`
