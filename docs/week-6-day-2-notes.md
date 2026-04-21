<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Phase 0 Week 6 Day 2 — notes from the Servo embedding pass

Companion to `docs/servo-composition.md`: records what the Day 2
implementer actually found integrating Servo 0.1.0 against the
pre-spike design. Things here fall into three buckets — deviations
from the design doc that stuck, open questions resolved, and the
macOS follow-up punch list.

---

## Deviations from the design doc that stuck

### 1. No `lts` feature on the `servo` crate

Design doc §8 flagged "verify LTS feature name" as a non-blocking
open question. Resolution: **there is no LTS feature.** The crate at
`servo = "0.1"` on crates.io declares no `lts` feature and does not
ship a separate LTS-tracked crate. We pin `~0.1.0` (cargo's default
0.x caret) and rely on the Servo project's release cadence for
compatible updates; the half-yearly migration cadence §8 envisioned
still applies, we just don't label it in `Cargo.toml`.

### 2. `WebView` takes a URL, not raw HTML

The Day 1 stub's `todo!()` message pointed at `WebView::load_html`,
but the 0.1.0 `WebView` API exposes only `load(Url)`. `render()`
therefore builds a `data:text/html;charset=utf-8,…` URL (percent-
encoded) and hands that to Servo. A minimal custom percent-encoder
lives alongside, and is unit-tested.

The `data:` URL approach is the legitimate content channel — not a
workaround. It plays nicely with a future CSP (`meta http-equiv=
"Content-Security-Policy"` in the wrapping HTML) as a belt-and-braces
layer on top of ammonia sanitization.

### 3. `WebViewDelegate` is narrower than the design doc assumed

The design doc §3.2 table lists nine methods. The shipped 0.1.0
trait surface has five of them:

| Method | In 0.1.0? | Notes |
|---|---|---|
| `notify_new_frame_ready` | yes | paint contract from §6.1 |
| `notify_load_status_changed` | yes | just traced |
| `notify_animating_changed` | yes | just traced |
| `request_navigation` | yes | renamed from `request_open_url` |
| `request_permission` | yes | always deny |
| `allow_opening_webview` | **no** | |
| `allow_navigation_request` | **no** | folded into `request_navigation` |
| `show_simple_dialog` | **no** | |
| `request_file_picker` | **no** | |
| `notify_pipeline_panic` | **no** | |

`request_navigation` alone provides the navigation-gating seam that
the design doc split across two methods. The remaining missing
methods don't exist on 0.1.0's surface — the defaults ship the
"deny" behavior implicitly. `CapytainDelegate` implements only the
five that exist.

### 4. `Preferences` has no `js_enabled`

Design doc §5 names `js_enabled = false` as a pref. 0.1.0's
`Preferences` struct does not expose this field. JavaScript execution
is defended against in three layers without it:

1. `ammonia` strips `<script>` upstream before `render()` is called.
2. A Phase 1 CSP baked into the wrapper `data:` URL — `<meta
   http-equiv="Content-Security-Policy" content="script-src 'none'">`
   — will further block execution even if a `<script>` somehow
   survives sanitization.
3. The long list of disabled `dom_*_enabled` prefs (documented in
   `apply_reader_pane_preferences`) removes every sensitive API the
   design doc names.

This means some email content could theoretically run JS on the
rendered data if all of (1) and (2) failed — but Servo at that point
would still be denied camera, clipboard, geolocation, etc. Worth
tracking upstream for a global JS off-switch if one lands.

Also missing from 0.1.0:

- `dom_usb_enabled`, `dom_hid_enabled`, `dom_nfc_enabled`,
  `dom_mediastream_enabled` — these fields do not exist on the
  `Preferences` struct. The design doc named them speculatively.
  The APIs they would gate are, as best we can tell, not
  exposed at all on 0.1.0 — so this is less of a gap than it reads.

### 5. `surfman::error::Error` doesn't `impl Display` either

Design doc §6.5 says "`?` through `MailError` will fail with a type
error. Wrap with `.map_err(|e| MailError::Other(e.to_string()))` at
the call site." Reality is slightly worse: `surfman::error::Error`
doesn't `impl Display` at all (only `Debug`), so `e.to_string()`
doesn't work. We wrap with `format!("{e:?}")` instead.

### 6. `Servo` has no `shutdown` / `deinit` / `start_shutdown`

The 0.1.0 `Servo` struct relies on `Drop` for cleanup. `destroy` on
the trait therefore just pumps the event loop a few times to settle
in-flight messages and then lets the `Rc<Servo>` fall out of scope.

---

## Architecture decisions the design doc left open

### 7. "ServoRenderer is `!Send` but the trait is `Send`"

Design doc §6.6 flagged this but didn't prescribe a solution. The
approach we landed on:

- `ServoRenderer` (the trait impl) holds only `Send + Sync` types:
  `Arc<dyn MainThreadDispatch>`, `Arc<Mutex<LinkCb>>`, `AtomicU64`.
- The actual Servo state (`Rc<Servo>`, `WebView`, `Rc<WindowRendering
  Context>`, `Rc<CapytainDelegate>`) lives in a `thread_local!` on
  whichever thread called `new_linux` / `new_macos` — the Tauri main
  thread in production.
- Every trait method dispatches onto the main thread via the caller-
  supplied `MainThreadDispatch`. The desktop crate backs this with
  `tauri::AppHandle::run_on_main_thread`, which Tauri makes platform-
  agnostic across Linux / macOS / Windows.
- The `EventLoopWaker` we pass to Servo also uses the dispatcher, so
  Servo can kick the event loop from its internal worker threads.

No `unsafe`; the workspace `forbid(unsafe_code)` lint stays green.

### 8. The Turso × Servo allocator conflict

Turso's default feature bundle includes `mimalloc`, which declares a
`#[global_allocator]`. Servo includes `servo-allocator` which also
declares one (tikv-jemallocator). A binary may have exactly one
`#[global_allocator]`. Linking `capytain-desktop` (which depends on
both) fails with:

```
error: the `#[global_allocator]` in turso conflicts with global
allocator in: servo_allocator
```

**Fix applied:** disable turso's default features at the workspace
decl, re-enabling just `sync` (the feature `capytain-storage`
actually needs). Servo keeps its jemalloc; turso falls back to the
system allocator. No measurable perf change observed in the Phase 0
scoped workloads; revisit in Phase 1 if storage-heavy paths get
sluggish.

### 9. Feature-flag propagation for Windows CI

Design doc assumed Servo would be default-on in `capytain-renderer`.
That fights with the Windows CI job — the `windows-latest` runner
doesn't ship the SpiderMonkey / cmake / clang-cl toolchain, so every
Windows clippy run would try (and fail) to build Servo.

**What we did instead:** `capytain-renderer` keeps a `servo` feature
but leaves it **off** by default. `capytain-desktop` owns a
desktop-level `servo` feature (default-on) that propagates to
`capytain-renderer/servo`. `apps/desktop/src-tauri` depends on the
renderer with `default-features = false` so the only way Servo gets
linked is through the desktop feature toggle.

This means:
- `cargo build -p capytain-desktop` — Linux dev path — servo on.
- `cargo build -p capytain-desktop --no-default-features` — Windows
  CI path — servo off; the desktop binary links a `None`-shaped
  renderer via `AppState::servo_renderer: Mutex<Option<_>>`, and
  reader commands degrade gracefully.
- `cargo clippy --workspace` on Linux — servo on throughout.
- Windows CI explicitly `--exclude`s both `capytain-renderer` and
  `capytain-desktop` from the workspace run and verifies them
  separately in their no-servo shapes.

---

## Day 2 scope that did NOT land — tracked for Phase 1 / the macOS session

### 10. Child-surface integration with Tauri's main window

The design doc §4.3 Linux plan described creating a `GtkWidget`
subclass that holds the `WindowRenderingContext` and packing it
into Tauri's GTK widget hierarchy. That would put the Servo reader
pane inside the Tauri main window, alongside the Dioxus chrome —
the "dual-webview model" from §2.

**What Day 2 ships instead:** a second, dedicated `tauri::Webview
Window` labeled `"servo-reader"` with its own raw-window-handle,
displayed side-by-side with the main window. This is working-as-
implemented for the embedding spike: link clicks route correctly,
paint/present lifecycle works, delegate callbacks fire as expected.

**Why deferred:** getting Servo's subsurface to compose cleanly with
the webkit2gtk surface already in the Tauri main window needs
enough GTK3 expertise and trial-and-error that it didn't fit in the
one-day budget. A separate PR in Phase 1 handles:

- Hooking into Tauri's GTK hierarchy via `tauri::Window::gtk_window()`.
- Creating a child `GtkDrawingArea` inside the existing main window's
  container, sized to the reader-pane region the Dioxus layout
  reserves.
- Forwarding `size-allocate` signals to `WindowRenderingContext::
  resize` + `WebView::resize`.

The same pattern applies to macOS (§4.1's `NSView`-as-subview) and
Windows (§4.2's `WS_CHILD` `HWND`).

### 11. macOS follow-up punch list

`crates/renderer/src/servo/macos.rs` compiles under `cfg(target_os =
"macos")` but is marked **UNVERIFIED** at module level and inside
the `new_macos` function body. A future Mac-hardware session needs
to:

1. Actually run `cargo build -p capytain-desktop` on Mac and see if
   it links. Servo's macOS build path has its own native-dep story
   that §7 doesn't fully enumerate.
2. Validate that `WindowRenderingContext::new` accepts the
   `AppKitWindowHandle` variant that Tauri produces on macOS and
   paints into the underlying `NSView`. If it needs a separate
   `CAMetalLayer` or similar, that's where `objc2` lands.
3. Check that `tauri::AppHandle::run_on_main_thread` correctly
   dispatches onto the AppKit main thread — the thread that called
   `NSApplication::run` — not some Tokio worker.
4. Confirm that the `servo-reader` window actually renders "Hello
   from Servo" and that clicking the test anchor routes through
   the `on_link_click` tracing log.
5. If any of the above fail, the `// UNVERIFIED` comment in
   `new_macos` is the starting point for correction. Update this
   section with findings.

### 12. Windows port

Not in scope for Day 2 at all. Tracked as a follow-up PR: implement
`ServoRenderer::new_windows` per design doc §4.2, install the Servo
native build deps on the Windows CI runner, and extend the CI matrix
to stop `--exclude`ing `capytain-renderer` / `capytain-desktop`.

---

## Validation status (Linux — this session)

- Target display server for validation: **Wayland** (`wayland-0`),
  on a CachyOS / Linux 7.0.0 kernel box.
- **Runtime validation is gated on having `cmake` installed** for
  `mozjs_sys` / SpiderMonkey's build.rs. `cargo check` and `cargo
  test` both pass without `cmake` because neither runs the SpiderMonkey
  native link step; `cargo build --release` is where the tree actually
  exercises that. This is a one-command fix on the validating host
  (`sudo pacman -S cmake` on CachyOS / Arch) — but it's worth
  calling out here so subsequent sessions don't hit the same wall.
- Whatever display server ends up validating the build — Wayland or
  X11 — should get documented in the PR description that lands this
  work, not here. Both should work; surfman abstracts the difference.
- X11 path: untested; confirm on an X11 host before trusting the
  "works on both" claim.
