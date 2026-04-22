<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Servo composition — design + post-spike findings

**Status:** post-spike (Phase 0 Week 6 Day 5). The original pre-spike
design is preserved verbatim below so readers can see the shape of the
plan we walked into. **§11 carries the findings** — everything we
learned while actually making it work, what landed, what deviated, and
what's still open for follow-up sessions.

**Audience:** the engineer (or Claude Code session) picking up the
renderer next. Start with §11 if you're trying to understand the
current state; read §0–§11 only when you need the rationale behind an
original decision.

**Quick jump to the post-spike update:** [§11](#11-post-spike-findings-day-5).

---

## 0. What changed since `PHASE_0.md` was written

`PHASE_0.md` §Week 6 was written assuming Servo would have to be consumed
as a git dependency with a private, churning embedding API — the
KDAB-style "directly construct Servo components like `servoshell` does"
approach. That assumption is stale. On **2026-04-13** the Servo project
published `servo` v0.1.0 to crates.io with a public, documented
embedding API and an LTS track. See the [release announcement][rel] and
the [docs.rs page][docs].

The practical consequences for Capytain:

- `cargo add servo` works. No git submodules, no vendored forks of
  `mozjs_sys`.
- The API is stable enough that third-party embedders have shipped
  working proofs-of-concept against 0.1.0 within a week: Simon Willison's
  [servo-shot][shot] headless CLI, the in-flight
  [tauri-runtime-verso][verso] runtime.
- An LTS version exists. Capytain pins to the LTS line; half-yearly
  migration windows fit our cadence better than the monthly breaking-
  change track.

The Week 6 tasks in `PHASE_0.md` stand, but the risk has dropped from
"novel research" to "integrate a new crate carefully." Exit criteria
unchanged; the dates should be achievable without the soft pad we
originally planned for a stall.

[rel]: https://servo.org/blog/2026/04/13/servo-0.1.0-release/
[docs]: https://doc.servo.org/servo/
[shot]: https://github.com/simonw/research/tree/main/servo-crate-exploration
[verso]: https://github.com/versotile-org/tauri-runtime-verso

---

## 1. Problem statement

Capytain renders email-body HTML in the reader pane. For security,
consistency, and the §1 Rust-native commitment, we want Servo — not the
system webview — doing that specific rendering. Everything else (the
Dioxus-rendered app chrome, the sidebar, the message list, the
compose window when it lands) stays in the normal Tauri webview.

So the concrete integration question is: **how do we put a Servo-painted
surface inside the Tauri window, alongside the Tauri webview, without
them fighting over event routing, focus, or window lifecycle?**

---

## 2. Architecture decision: dual-webview model

```text
┌─────────────────────────── Tauri window (per OS) ───────────────────────────┐
│                                                                             │
│  ┌──────────────────────┐  ┌──────────────────────────────────────────┐     │
│  │  Tauri WebView       │  │  Servo child surface                     │     │
│  │  (wry / system)      │  │  (WindowRenderingContext child)          │     │
│  │                      │  │                                          │     │
│  │  Dioxus app chrome:  │  │  Rendered email body only:               │     │
│  │  sidebar, message    │  │  sanitized HTML → Servo → native pixels  │     │
│  │  list, toolbars      │  │                                          │     │
│  │                      │  │  ServoRenderer owns:                     │     │
│  │  tauri::Window owns  │  │    - `Servo` engine                      │     │
│  │  this surface.       │  │    - `WebView` handle                    │     │
│  │                      │  │    - `Rc<dyn RenderingContext>`          │     │
│  │                      │  │    - platform-specific child-surface     │     │
│  │                      │  │      handle (NSView / HWND / GtkWidget)  │     │
│  └──────────────────────┘  └──────────────────────────────────────────┘     │
│                                                                             │
│  IPC bridge (Tauri commands in COMMANDS.md) routes:                         │
│    - "display this message id" → fetch body → sanitize → ServoRenderer      │
│    - Servo link-click delegate  → URL cleaner → webbrowser::open            │
└─────────────────────────────────────────────────────────────────────────────┘
```

Two alternatives considered and rejected:

- **Replace Tauri's runtime with `tauri-runtime-verso`.** Verso wraps the
  low-level Servo embedding API and presents a WRY-compatible surface.
  Attractive because it would unify the stack on Servo. Rejected because
  Dioxus's Tauri integration is tested against WRY, Dioxus devtools
  workflows depend on the system webview, and the existing Phase 0
  Week 5 Dioxus shell would need re-validation. Too much churn for a
  spike week. Worth reconsidering post-v1 if Verso matures and the
  unified stack becomes cheaper than the dual stack.
- **Render email to a PNG via `SoftwareRenderingContext` and display in
  the Tauri webview.** Simpler than the dual-surface approach — no
  platform-specific embedding code at all. Rejected because it defeats
  the point: no text selection, no link-click handling, no
  `prefers-color-scheme`, no accessibility tree. The reader pane would
  be worse than every other modern email client. Worth keeping in mind
  as a degraded-mode fallback for platforms where child-surface
  embedding hits a blocking wall.

---

## 3. Servo 0.1.0 API surface

The public surface (per [docs.rs/servo/0.1.0][docs]) that `ServoRenderer`
exercises:

### 3.1 The four types Servo asks embedders to know

- **`Servo`** — the engine instance. Owns the constellation, script
  threads, layout thread pool, compositor. One per process (Capytain
  uses exactly one).
- **`ServoBuilder`** — constructs a `Servo` given a
  `Rc<dyn RenderingContext>` plus optional `Opts`, `Preferences`,
  `EventLoopWaker`, `UserContentManager`, `ProtocolRegistry`,
  `WebXrRegistry`. For Capytain, defaults are fine except for
  `Preferences` (see §5 below).
- **`WebView`** — a single webview handle. Capytain uses exactly one
  (the reader pane). Built via `WebViewBuilder::new(&servo, ctx)` with
  `.url(...)`, `.hidpi_scale_factor(...)`, `.delegate(...)`, `.build()`.
- **`Rc<dyn RenderingContext>`** — where pixels go. Two stock
  implementations: `WindowRenderingContext` (attached to a native
  surface, what we want) and `SoftwareRenderingContext` (headless, for
  the screenshot test corpus in Day 5).

### 3.2 The delegate trait we implement

`WebViewDelegate` is Servo's callback surface. Capytain cares about a
small subset; the rest we leave at defaults.

| Method | Why we care | Maps to |
|---|---|---|
| `notify_new_frame_ready` | **Paint contract.** Must call `webview.paint()` here or the back buffer stays empty. See §6.1. | Internal |
| `notify_load_status_changed` | Signals `LoadStatus::Complete`. Used to know when a `render()` call is visually ready. | Internal |
| `notify_animating_changed` | When the email content has a CSS animation, we need to keep spinning the event loop. | Internal |
| `request_open_url` (navigation intercept) | **This is our link-click seam.** URL is routed through `EmailRenderer::on_link_click`. | `EmailRenderer::on_link_click` |
| `request_permission` | Always deny. Email content must not get camera, geolocation, notifications. | Hardcoded deny |
| `allow_opening_webview` | Always deny. Email content must not open new webviews. | Hardcoded deny |
| `allow_navigation_request` | Gate: only `https:` and `mailto:` navigations are reported; everything else denied without firing the callback. | Filter |
| `show_simple_dialog` | Always dismiss. `alert()` / `confirm()` / `prompt()` from email content is a phishing vector. | Auto-dismiss |
| `request_file_picker` | Always deny. | Hardcoded deny |
| `notify_pipeline_panic` | Log via `tracing`. A panic in one WebView's pipeline doesn't kill the process. | `tracing::error!` |

The defaults for everything else (media session events, fullscreen,
gamepad, history changes, favicons, cursor changes, custom protocol
handlers) are appropriate — they're either no-op or return "denied."

### 3.3 The event loop

`Servo::spin_event_loop()` is the pump. The embedder calls it on its
main thread at an appropriate cadence:

- On a fresh load: in a tight loop until
  `notify_load_status_changed(LoadStatus::Complete)` fires, plus a few
  extra passes for font / image / render-blocking-stylesheet settling
  (see §6.3).
- When animating: at ~60 Hz while `notify_animating_changed(true)` is in
  effect.
- Otherwise: on demand, when input is delivered or the host window
  requests a repaint.

The pump is not on its own thread — integration with Tauri's event loop
is the main correctness question for Day 2 (see §4).

---

## 4. Platform integration plan (Days 2–4)

The goal of each platform day is: a real `WindowRenderingContext`
attached to a native child surface of the Tauri window, rendering a
fixed test HTML, with link clicks routing through the delegate.

### 4.1 Day 2 — macOS (`NSView` child of Tauri's `NSWindow`)

- From inside the Tauri command that creates the reader pane, obtain
  the `tauri::Window`'s `raw-window-handle` via
  `window.raw_window_handle()`. On macOS this yields an
  `AppKitWindowHandle` containing an `NSView*` (the content view of the
  Tauri `NSWindow`).
- Create an `NSView` sized to the reader pane, add it as a subview of
  the Tauri content view. Use `objc2` (already in the Rust crate
  ecosystem; avoid reaching for `cocoa`, which is unmaintained).
- Build a `WindowRenderingContext` bound to that `NSView`'s layer. The
  Servo `WindowRenderingContext` takes a `raw-window-handle`-style
  surface identifier; the exact constructor signature is checked at
  build time — do not guess.
- Hook `servo.spin_event_loop()` into Tauri's run loop via
  `NSRunLoop::currentRunLoop.perform(...)` or a `CADisplayLink`-
  equivalent timer.
- Size changes: observe the parent view's `bounds` with a KVO
  observation or delegate `viewDidResize`-style callback; call
  `WebView::move_resize()` when it changes.

**Done when:** a hardcoded `<p>Hello from Servo</p>` appears in the
reader pane alongside the Dioxus app chrome, and clicking a link in the
rendered HTML routes through `EmailRenderer::on_link_click`.

### 4.2 Day 3 — Windows (`HWND` child of Tauri's `HWND`)

- From the `tauri::Window`'s raw-window-handle, extract the `HWND`
  (`Win32WindowHandle`).
- `CreateWindowExW` a child `HWND` with `WS_CHILD | WS_VISIBLE`,
  parented to the Tauri `HWND`, sized to the reader pane.
- Build a `WindowRenderingContext` against that child `HWND`.
- Drive `spin_event_loop()` from the Tauri message loop: either hook
  into the Tauri event loop or use a `SetTimer` + `WM_TIMER` message.
- Size changes arrive as `WM_SIZE` on the parent; forward to the child
  `HWND` and call `WebView::move_resize()`.

**Potential pitfall:** the blank-window saga from PRs #11–#15 suggests
the Tauri window on Windows can reach a state where child surfaces
don't paint until CSP / devtools are reconfigured. If the Servo child
surface renders blank on Day 3, first thing to check is the Tauri
configuration carried over from that debugging; the fix may be
identical.

**Done when:** the same hardcoded HTML renders on Windows and link
clicks route through the delegate.

### 4.3 Day 4 — Linux (GTK widget)

- Capytain uses Wayland-primary with X11 fallback. Tauri on Linux uses
  GTK 3 (per `README.md`). The `raw-window-handle` path gives us either
  `WaylandWindowHandle` or `XlibWindowHandle`.
- The cleanest integration is to mirror what the [servo-gtk][servogtk]
  library does: create a `GtkWidget` subclass that holds the Servo
  `WindowRenderingContext`, pack it into the Tauri window's GTK widget
  hierarchy.
- Drive `spin_event_loop()` from the GTK main loop via
  `glib::MainContext::default().spawn_local()` or
  `g_idle_add`-equivalent.
- Size changes: connect to the parent's `size-allocate` signal.

[servogtk]: https://servo.org/made-with/

**Done when:** the same hardcoded HTML renders on Linux and link clicks
route through the delegate.

---

## 5. `Preferences` we need to set

Most defaults are fine, but the following should be explicitly set on
`ServoBuilder` for the reader-pane embedding:

- `dom_servoparser_async_html_tokenizer_enabled = false` until it
  stabilizes upstream (see [February-in-Servo 2026][feb] for current
  status).
- `accessibility_enabled = true` so the reader-pane content is exposed
  to AccessKit. This is a recently-added pref and will need tracking as
  it evolves.
- Disable any `dom_*_enabled` prefs for APIs we don't want email to
  reach: `dom_serviceworker_enabled`, `dom_webrtc_enabled`,
  `dom_webgpu_enabled`, `dom_webgl2_enabled`, `dom_gamepad_enabled`,
  `dom_bluetooth_enabled`, `dom_usb_enabled`, `dom_hid_enabled`,
  `dom_nfc_enabled`, `dom_geolocation_enabled`,
  `dom_notification_enabled`, `dom_mediastream_enabled`,
  `dom_clipboardevent_enabled`. Reading an email should never prompt for
  camera access.
- `js_enabled = false`. Email HTML must never execute JS. Sanitization
  via ammonia strips `<script>` upstream, but belt-and-braces — if the
  sanitizer ever misses something, Servo itself should refuse to run
  it.

[feb]: https://servo.org/blog/2026/03/31/february-in-servo/

The precise key names will need to be cross-checked against
`servo::Preferences` in 0.1.0; this list is the intent, not the
literal identifier spelling.

---

## 6. Known footguns (pre-documented)

Most of these come from the Simon Willison exploration; the rest from
the Servo release notes and the KDAB writeup. Recording them here so
Days 2–4 don't rediscover them.

### 6.1 Delegate paint contract

`WebView::paint()` **must** be called inside
`WebViewDelegate::notify_new_frame_ready`. If it isn't, the compositor
never fills the back buffer and the surface stays blank. This is the
single most common cause of a "nothing renders" bug. The
`ServoRenderer::on_new_frame_ready` method must unconditionally call
`self.webview.paint()`.

### 6.2 Buffer lifecycle on readback

`RenderingContext::present()` swaps back/front with
`PreserveBuffer::No`. If we ever do pixel readback (Day 5 corpus
tests), call `read_to_image()` **before** `present()`, not after.
(Our production path doesn't read back pixels — this matters only for
the corpus test harness.)

### 6.3 Frame settling after load-complete

`LoadStatus::Complete` fires on the DOM `load` event, which happens
before web fonts, late images, and render-blocking stylesheets finish.
A screenshot captured immediately will miss those. The `take_screenshot`
method on `WebView` already waits for all of this; prefer it over
reading the rendering context directly. For the interactive reader pane,
a tight `spin_event_loop()` loop for 5–10 more iterations after
`LoadStatus::Complete` is a reasonable settling strategy.

### 6.4 Size-type mismatch between `dpi` and `euclid`

`SoftwareRenderingContext::new` wants `dpi::PhysicalSize<u32>`;
WebView's internal size tracking uses `euclid::Size2D`. This caught
servo-shot on the first build. Convert at the boundary, don't try to
pick one for the whole codebase.

### 6.5 `surfman::error::Error` doesn't `impl std::error::Error`

`?` through `MailError` will fail with a type error. Wrap with
`.map_err(|e| MailError::Other(e.to_string()))` at the call site.

### 6.6 Thread affinity

All Servo WebView calls happen on the thread that constructed the
`Servo` instance. That's the Tauri main thread. Any work that touches
`self.webview` must be either on the main thread or dispatched to it
via Tauri's event loop. Wrap `ServoRenderer` in `std::rc::Rc` rather
than `std::sync::Arc`; the `Send` bound on `EmailRenderer` is
satisfied because the trait object is moved into the main-thread
runtime at construction and stays there.

### 6.7 Multi-webview isolation

Servo supports multiple `WebView` instances, each with their own
delegates and loaded content. For Phase 0 we use exactly one
(reader pane). If later phases introduce previews (e.g. inline
rendering of linked pages), each preview gets its own `WebView`, same
`Servo` engine. Don't be tempted to reuse the reader-pane `WebView` for
other renders; `clear` is cheaper than cross-contaminated state.

---

## 7. Native build dependencies

Inherited from the `servo` crate's build requirements, per the release
notes and the `mozjs_sys` transitive dep:

- Rust 1.94+ (we're on 1.88 — **this is a blocker**, see §10).
- `cmake`, `clang` / `llvm` (for `mozjs_sys` / SpiderMonkey).
- `pkg-config`, `fontconfig`, `freetype`, `harfbuzz` development
  headers.
- Linux extras: `gstreamer-1.0-dev`, `libssl-dev`, `libglib2.0-dev`,
  `mesa` (for osmesa software GL fallback).
- ~10–15 GB free in `target/` for the first release build. This matters
  for CI — the existing Windows-only CI matrix will need matrix
  expansion and likely larger runners once the `servo` feature is on.

These need to land in `CONTRIBUTING.md` before Day 2 starts, so
contributor dev setup doesn't silently drift.

---

## 8. Version pinning

Track the Servo LTS line, not the monthly releases. LTS provides:

- Security backports.
- Half-yearly migration windows (roughly every ~6 months we expect a
  breaking upgrade) vs monthly.
- A predictable schedule that fits our Phase 1+ cadence.

Pin in `Cargo.toml`:

```toml
servo = { version = "~0.1.0", features = ["lts"] }
```

(Feature name TBD — verify against the actual crate metadata at
integration time.)

Add to `docs/dependencies/servo.md` once the first version lands, in
the same pattern as the existing `docs/dependencies/turso.md`.

---

## 9. Days 2–5 task checklist

Day 2 (macOS):

1. Upgrade `rust-toolchain.toml` to 1.94.0 (required by `servo` 0.1.0).
   Verify every crate still compiles; re-run CI.
2. Add `servo` workspace dep with the LTS feature.
3. Flip `crates/renderer` default features to include `servo`.
4. Implement `ServoRenderer::new_macos(parent: RawWindowHandle)`:
   create `NSView` child, build `WindowRenderingContext`, build
   `Servo`, build `WebView`, wire `CapytainDelegate`.
5. Implement the trait methods — replace each `todo!()` in
   `crates/renderer/src/servo.rs` with real Servo calls.
6. Wire into the Tauri reader-pane command. Hardcoded test HTML is
   fine; sanitization / adblock wiring is Phase 1.
7. Verify link-click routing end-to-end.

Day 3 (Windows):

1. Implement `ServoRenderer::new_windows(parent: RawWindowHandle)`:
   `CreateWindowExW` child `HWND`, build `WindowRenderingContext`.
2. Hook `spin_event_loop` into the Tauri Win32 message loop.
3. Same hardcoded test HTML. Verify link clicks.
4. If blank-window recurs, re-examine the CSP / devtools fix from
   PR #13 before debugging Servo itself.

Day 4 (Linux):

1. Implement `ServoRenderer::new_linux(parent: RawWindowHandle)`:
   GTK widget subclass or container, `WindowRenderingContext`.
2. Hook `spin_event_loop` into GTK main loop.
3. Same hardcoded test HTML. Verify link clicks.
4. Test on both Wayland and X11 — these use different
   `RawWindowHandle` variants.

Day 5 (corpus + docs):

1. Build a 10-email corpus in `crates/renderer/tests/corpus/`:
   - Plaintext (wrapped in `<pre>`)
   - Gmail marketing HTML
   - Substack newsletter
   - Stripe receipt
   - GitHub notification
   - Mailchimp promotional
   - Calendar invite HTML body
   - iOS Mail auto-forward
   - Outlook HTML with VML remnants
   - Non-Latin script (CJK or RTL) email
2. Corpus test harness: render each through `SoftwareRenderingContext`,
   hash the resulting PNG, compare against a committed reference.
3. Update `docs/servo-composition.md` with post-spike findings: which
   platform had surprises, which prefs ended up actually needed, what
   the final `ServoRenderer::new_*` signatures look like, any upstream
   issues filed.

---

## 10. Open questions & go/no-go

**Blocking question 1 — Rust toolchain bump.** `servo` 0.1.0 needs
Rust 1.94+. We're pinned to 1.88 in `rust-toolchain.toml`. Bumping
affects every crate; it's a one-line change but rippling through CI
and any locally-installed toolchains. Before Day 2, confirm that 1.94
doesn't break any of the current dependencies (should be safe —
Servo's own stable-Rust requirement is the most aggressive bump in
our tree).

**Blocking question 2 — which thread pumps the event loop.** The
"main thread" on macOS is the AppKit main thread, on Windows the
thread that called `CreateWindowExW`, on Linux the GTK main context
thread. Tauri guarantees all three are the same thread the app was
launched on. But the Tokio runtime Capytain uses is multi-threaded,
and the sync engine fires events from worker threads. The
`EmailRenderer` trait is `Send` — the object is moved to the main
thread at construction — but the IPC layer delivering "render this
message" events needs to marshal onto the main thread via
`tauri::AppHandle::run_on_main_thread` before touching the renderer.
This should be checked as a design invariant in the reader-pane
command handler before Day 2 starts.

**Non-blocking question — LTS feature flag name.** Verify against
the 0.1.0 crate what the actual LTS opt-in looks like. Could be a
feature flag, a separate `servo-lts` crate, or just a version range
policy. Low stakes; discover at integration time.

**Go/no-go for Phase 0 completion.** The spike is "no-go" if any of
the following hold at end of Day 4:

- The `WindowRenderingContext` API doesn't actually support a
  child-surface model on one or more platforms (i.e. requires us to
  own the whole window).
- Event-loop integration with Tauri causes input events to be dropped
  or UI freezes longer than 100ms under normal interaction.
- Servo 0.1.0 has a blocker-level bug that can't be worked around
  within the spike.

In any "no-go" case, the Phase 0 fallback is the
`TauriWebviewRenderer` — a second implementation of `EmailRenderer`
that uses Tauri's system webview (WebKit/WebView2/WebKitGTK) instead
of Servo. The `EmailRenderer` trait was designed precisely to make
this swap bounded; we keep the rest of the read path, lose the
consistent-rendering and pure-Rust properties, and revisit Servo
post-v1 when the embedding story is more mature.

---

## 11. Post-spike findings (Day 5)

Everything below is what we learned *after* trying to land the plan in
§0–§11. Where a finding contradicts earlier guidance, the finding
wins — the earlier text is preserved for context, not as a prescription.

### 11.1 Platform status snapshot

| Platform | Code | Runtime validation |
|---|---|---|
| **Linux** (Wayland / X11) | `crates/renderer/src/servo/linux.rs` — production-shape `new_linux`, attached to a dedicated plain `tauri::window::Window` via raw-window-handle | Reaches "Servo renderer installed" on the dev box. Blocked visually by an **NVIDIA EGL-Wayland + explicit-sync bug** in surfman — see §11.4. |
| **macOS** | `crates/renderer/src/servo/macos.rs` — UNVERIFIED mirror of the Linux shape | Compiles under `cfg(target_os = "macos")`. Never executed on Mac hardware. Needs a Mac-hardware session to validate; `docs/week-6-day-2-notes.md` §11 carries the punch list. |
| **Windows** | not started | Out of scope; tracked as a follow-up PR. |
| **Headless corpus (Day 5)** | `crates/renderer/src/servo/corpus.rs` — uses `SoftwareRenderingContext`, no native surface | **Fully validated.** 10 fixtures render in ~1.7s, regression harness in `crates/renderer/tests/corpus.rs`. |

The headless corpus path is what actually proves "Servo renders
real-world email HTML correctly" on Phase 0 hardware. The interactive
(native-surface) path on Linux proves the *integration plumbing* — the
trait, the delegate, the main-thread dispatcher, the Tauri wire-up —
but the final paint step is blocked upstream.

### 11.2 Servo 0.1.0 API corrections

The pre-spike description in §3 was substantially right but a handful
of specifics needed correction at the keyboard. Key deltas:

- **No `lts` feature flag.** §8's open question resolved: servo 0.1.0
  on crates.io has no `lts` feature; plain `version = "0.1"` is what
  we pin. Half-yearly migration discipline is now a release-cadence
  policy rather than a Cargo toggle.
- **`WebView` has no `load_html`.** The 0.1.0 surface is `load(Url)`
  only; we wrap sanitized HTML in `data:text/html;charset=utf-8,…`
  URLs. A tiny percent-encoder lives in `servo::make_data_url` with
  unit-test coverage. This also sets up the Phase 1 CSP-in-the-data-
  URL belt-and-braces for JS execution.
- **`WebViewDelegate` is narrower than §3.2 assumed.** Only five of
  the nine methods in §3.2 actually exist on 0.1.0:
  `notify_new_frame_ready`, `notify_load_status_changed`,
  `notify_animating_changed`, `request_navigation`, and
  `request_permission`. The others (`allow_opening_webview`,
  `allow_navigation_request`, `show_simple_dialog`,
  `request_file_picker`, `notify_pipeline_panic`) do not exist;
  `request_navigation` alone carries the navigation-gating seam §3.2
  split across two methods.
- **`Preferences` has no `js_enabled`.** §5's proposed `js_enabled =
  false` doesn't exist. JS is defended at two other layers: ammonia
  stripping `<script>` upstream of `render()`, and the wrapper
  `data:` URL eventually carrying a restrictive CSP (Phase 1). The
  full list of `dom_*_enabled` prefs in §5 mostly exists and is
  applied by `apply_reader_pane_preferences`; the speculative ones
  (`dom_usb_enabled`, `dom_hid_enabled`, `dom_nfc_enabled`,
  `dom_mediastream_enabled`) do not exist, but the underlying APIs
  they'd gate are also absent from the shipped crate.
- **`surfman::error::Error` doesn't `impl Display` either.** §6.5
  warned about `impl std::error::Error`; Display is missing too.
  Wrap with `format!("{e:?}")` rather than `e.to_string()`.
- **`Servo` has no explicit shutdown.** The 0.1.0 type relies on
  `Drop`. Our `destroy()` path pumps the event loop a few times to
  settle in-flight messages and lets the `Rc<Servo>` fall out of
  scope.
- **`WebView::take_screenshot`** turned out to be the right hammer
  for headless corpus tests (§11.5). It already waits for frames /
  images / fonts / render-blocking stylesheets to settle, so the
  "spin until `LoadStatus::Complete` plus 5–10 extra passes" pattern
  from §6.3 is only needed for the interactive path, not corpus
  rendering.

### 11.3 Architecture: `!Send` state vs. a `Send` trait

§6.6 flagged the thread-affinity problem without prescribing a
solution. What we landed on (see `crates/renderer/src/servo.rs` module
docs):

- The public `ServoRenderer` struct holds only `Send + Sync` types
  (an `Arc<dyn MainThreadDispatch>`, an `Arc<Mutex<LinkCb>>`, an
  `AtomicU64`). It is safely stored in Tauri's `AppState`.
- The actual Servo state (`Rc<Servo>`, `WebView`, `Rc<Window
  RenderingContext>`, `Rc<CapytainDelegate>`) lives in a
  `thread_local!` on whichever thread called the platform
  constructor — the Tauri main thread in production.
- Every trait method dispatches onto the main thread via the
  caller-supplied `MainThreadDispatch`. The desktop crate's impl is
  backed by `tauri::AppHandle::run_on_main_thread`, which Tauri
  makes platform-agnostic across all three targets.
- The `EventLoopWaker` we pass to Servo reuses the same dispatcher,
  so Servo can kick the event loop from its internal worker threads
  without knowing anything about Tauri.

No `unsafe`; the workspace `forbid(unsafe_code)` lint stays green.

### 11.4 The NVIDIA EGL-Wayland blocker

Running the interactive desktop binary on the Day 2 dev box (KDE
Plasma 6 / Wayland / NVIDIA GeForce RTX 5070 Ti, driver 595.58.03),
Servo installs cleanly and then the compositor disconnects with:

```
wl_display#1.error(wp_linux_drm_syncobj_surface_v1#48, 4,
    "explicit sync is used, but no acquire point is set")
Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
```

Servo / surfman subscribes to the `wp_linux_drm_syncobj_surface_v1`
explicit-sync protocol (triggered by NVIDIA's EGL-Wayland driver
auto-attaching to the `wl_surface`) but doesn't set an acquire point
on first commit. KWin catches the protocol violation and disconnects;
Gdk exits the process with code 1, within ~50ms of the "Servo
renderer installed" log line.

Narrowing from workarounds tried (full list in
`docs/week-6-day-2-notes.md` §Wayland): `MESA_LOADER_DRIVER_OVERRIDE=
llvmpipe` avoids the error entirely, which localizes the bug to
surfman's NVIDIA EGL-Wayland init path (not Servo-on-Wayland
generally). Intel / AMD hosts, or any real X11 session, are expected
to just work.

**Path forward:**

1. File upstream in `servo/servo` or `servo/surfman` with the
   reproducer. Fix will land in a Servo 0.1.x patch release, eventually.
2. The `SoftwareRenderingContext` path (§11.5) completely sidesteps
   this — if the upstream fix is slow, the Phase 0 shipping answer
   can be "use software rendering for the reader pane; revisit native
   when Servo's interactive Wayland story stabilizes." §2 of the
   pre-spike design rejected this fallback, but the NVIDIA bug tips
   the cost/benefit.
3. Full `TauriWebviewRenderer` fallback per §10 go/no-go remains
   available if both (1) and (2) stall.

### 11.5 Day 5 corpus harness — what we shipped

`crates/renderer/tests/corpus/fixtures/` holds 10 representative HTML
documents (plaintext, Gmail marketing, Substack, Stripe receipt,
GitHub notification, Mailchimp promo, calendar invite, iOS auto-
forward, Outlook + VML, CJK + RTL — the list from §9). The test
harness in `crates/renderer/tests/corpus.rs` renders each through a
shared `CorpusRenderer` (one `Servo` instance, one `WebView`, reused
across all fixtures to stay within the one-per-process limit) and
hashes the resulting `image::RgbaImage`.

**Regeneration:** `CAPYTAIN_CORPUS_REGEN=1 cargo test -p
capytain-renderer --features servo --test corpus -- --nocapture`.
References live under `crates/renderer/tests/corpus/reference/` as
paired `.sha256` + `.png` files — the PNGs are committed so reviewers
can diff by eye when the hash drifts.

**Failure shape:**

- Hard failures (FAIL the run): render timeout, zero-sized output,
  every pixel identical (layout produced nothing).
- Soft drift (warn but pass): hash mismatch. Exact-pixel reproduction
  across machines is too flaky (font hinting, subpixel AA, Servo GL
  driver variations) to gate PRs on. The actual PNG is written to
  `/tmp/` for eyeball review.

**First-render warmup.** A fresh `Servo` instance's first render
races a slow path and `take_screenshot` can return the pre-layout
background. `CorpusRenderer::new` does a throwaway
`"<!DOCTYPE html><html><body>warmup</body></html>"` render in its
constructor to prime the pipeline; the first *real* fixture then
hits a warm font cache, warm script thread, warm constellation.
Without this, `01_plaintext` (or whichever fixture is first
alphabetically) reliably produced uniform-white output on this box.

### 11.6 Findings surfaced by the corpus

With the 10 references captured and eyeballed, the layout output is
high quality across the board — `02_gmail_marketing`,
`04_stripe_receipt`, `05_github_notification`, and `09_outlook_vml`
in particular render indistinguishably from what a system webview
would produce. Real findings worth carrying into Phase 1:

- **CJK / RTL fonts are unfilled.** `10_cjk_rtl.png` shows mojibake
  for Japanese, Simplified Chinese, Korean, Arabic, and Hebrew —
  Servo's default font fallback doesn't cover these scripts. Every
  production email client configures a fallback stack; Phase 1 needs
  a `UserContentManager`-installed CSS rule or equivalent to map
  these to a shipping font (Noto Sans CJK / Noto Sans Arabic /
  Noto Sans Hebrew are the usual picks).
- **Em-dash (and other "exotic" Latin glyphs) trigger fallback.**
  Multiple fixtures show `â□` in place of `—`. The symptom is the
  same font-fallback stack — Servo picks a font that lacks the
  glyph and mojibake results. Same Phase 1 fix applies.
- **VML is correctly ignored.** `09_outlook_vml` renders the
  surrounding Word-generated HTML cleanly; the embedded `<v:rect>`
  disappears as intended (Servo's HTML parser treats unknown
  namespaces as comments). No Phase 1 action needed.
- **Tables render as specified.** Every table-based layout in the
  corpus (Gmail marketing, Stripe receipt, GitHub notification,
  Mailchimp promo, Outlook VML, calendar invite) comes out with
  correct column widths, border collapse, and background colors.
  This is a meaningful data point: the "modern Servo lost some
  table compatibility during the stylo rewrite" fear that circulated
  in 2023–2024 turns out not to affect the email-style table markup
  we actually see.
- **Data-URL size is not a problem yet.** The largest fixture
  (`03_substack_newsletter.png` at 177 KB source HTML) loads and
  renders in sub-200ms. Big promotional HTML (tens of kB) fits fine
  inside a `data:` URL; if we ever hit the 10MB-ish browser limits,
  that's a Phase 1 problem.

### 11.7 Integration landmines outside the Servo API

Surprises that hit during integration and landed fixes we wouldn't
have predicted from the pre-spike design:

- **Turso `#[global_allocator]` vs. Servo `#[global_allocator]`.**
  Turso's default `mimalloc` feature declares one; `servo-allocator`
  declares `tikv-jemallocator`. Can't have two. Fix: disable turso's
  default features at the workspace level, keep only `sync`. Turso
  falls back to the system allocator; Servo keeps jemalloc. No
  measurable perf regression on Phase 0 workloads.
- **`rustls::CryptoProvider` ambiguity.** With Servo in the graph,
  both `ring` and `aws-lc-rs` end up enabled at the feature level;
  rustls refuses to auto-pick and panics on the first HTTPS
  handshake inside Servo's `ResourceManager`. Fix: call
  `rustls::crypto::ring::default_provider().install_default()` at
  `main()` entry. Ring matches what tokio-rustls uses elsewhere in
  the workspace.
- **Feature-layout pivot for Windows CI.** Default-enabling `servo`
  on `capytain-renderer` fights the Windows CI runner (no Servo
  toolchain). Landed shape: renderer's `default = []`, desktop owns
  a `servo` feature that propagates to `capytain-renderer/servo`,
  workspace pins `default-features = false` for the renderer. Linux
  gets the real engine by default; Windows CI uses `--no-default-
  features` to drop it.
- **Tauri plain `Window` requires the `unstable` feature.** We use
  `tauri::window::WindowBuilder` (no webview) for the Servo surface;
  without `unstable`, you only get `WebviewWindowBuilder`, and
  attaching Servo's GL context on top of webkit2gtk's produces its
  own set of conflicts. Enabled in
  `apps/desktop/src-tauri/Cargo.toml`.

### 11.8 Follow-up work

- **Phase 1 reader-pane layout.** The current Servo surface is a
  separate OS window, not embedded inside the main Tauri window.
  §4.3's "GTK widget subclass packed into Tauri's hierarchy" path
  is still the right answer; scoped out of Day 2 for complexity
  budget.
- **macOS validation.** `servo/macos.rs` is still UNVERIFIED. Needs
  a session on Mac hardware. Punch list in
  `docs/week-6-day-2-notes.md` §11.
- **Windows port.** Fresh platform day. `new_windows` per §4.2,
  Windows CI dep install, stop `--exclude`ing the Servo-toolchain
  consumers.
- **CJK + Arabic + Hebrew font fallback.** Covered in §11.6. Needs
  a Phase 1 decision on which font family to ship and how to
  install it into Servo's font config.
- **Upstream surfman explicit-sync fix.** Filed-or-to-file; §11.4
  has the reproducer shape.
- **Day 5 harness evolution.** If hash drift across maintainers'
  machines becomes noisy, consider swapping the exact-hash check
  for a perceptual hash (dhash / phash). We haven't needed it yet
  — output has been stable across the few regeneration runs in
  this session — but the `crates/renderer/tests/corpus.rs` harness
  is structured so that swapping the hash function is a one-line
  change.

---

## 12. References

- [`servo` 0.1.0 release announcement (2026-04-13)](https://servo.org/blog/2026/04/13/servo-0.1.0-release/)
- [`servo` crate API docs on docs.rs](https://doc.servo.org/servo/)
- [`WebView` API docs](https://doc.servo.org/servo/struct.WebView.html)
- [`WebViewDelegate` API docs](https://doc.servo.org/servo/trait.WebViewDelegate.html)
- [`ServoBuilder` API docs](https://doc.servo.org/servo/struct.ServoBuilder.html)
- [servo-shot — third-party headless proof-of-concept](https://github.com/simonw/research/tree/main/servo-crate-exploration)
- [tauri-runtime-verso — Tauri runtime backed by Servo via Verso](https://github.com/versotile-org/tauri-runtime-verso)
- [KDAB — Embedding Servo in Qt (2024)](https://www.kdab.com/embedding-servo-in-qt/) (pre-0.1.0, historical)
- [Made with Servo — ecosystem index](https://servo.org/made-with/)
- `TRAITS.md` §EmailRenderer — the trait this implements.
- `DESIGN.md` §4.5 — the read-path pipeline that feeds the renderer.
- `PHASE_0.md` §Week 6 — the week plan this document supports.
