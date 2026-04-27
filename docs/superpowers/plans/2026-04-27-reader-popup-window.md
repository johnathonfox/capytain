<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Reader Popup Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Double-clicking a message row in the inbox opens that message in a new full Tauri window with the same Servo-rendered reader pane the inline reader uses.

**Architecture:** Per-window `Servo` instances. The current `Mutex<Option<Box<dyn EmailRenderer>>>` and process-wide `OnceLock<&'static LinuxGtkParent>` become per-window registries keyed by Tauri window label. Each Tauri `WebviewWindow` gets its own GTK overlay + DrawingArea + Servo instance, installed on first `reader_render` for that window. Closing a popup drops its renderer (Linux GTK widgets stay leaked — see "Known limitations"). The popup's Dioxus app reads the Tauri-injected `__QSL_READER_ID__` initialization-script global at boot and mounts `ReaderOnlyApp` instead of the three-pane shell.

**Tech Stack:** Tauri 2 (`WebviewWindowBuilder`, `Window::on_window_event`, `initialization_script`), Dioxus 0.7 (component branching at root), `gtk-rs` 0.18 (per-window Overlay reparenting), `qsl_renderer::ServoRenderer` (already per-instance — re-instantiable).

---

## Architectural decisions

### Why per-window Servo instances (not reparenting one)

Reparenting GTK widgets between top-level windows on Wayland is unsupported by the compositor protocol; KWin's `xdg_surface` rejects role changes. Even on X11 the surfman GL context binding to the original window's `GdkWindow` would have to be torn down and re-made. Spawning fresh Servo per popup costs ~2-3s init time (one-shot per window) but yields zero state-coherence work.

### Why window-label keying

Tauri's `Window` argument-injection automatically gives commands the calling window. `window.label()` is `String`, naturally a `HashMap` key, and the popup's URL contains the message id which we use as part of the label (`reader-<message_id>`). Same label across IPC + Servo registry + GTK parent registry = no separate id plumbing.

### Why `initialization_script` (not URL hash, not query string)

Tauri 2's `WebviewUrl::App(PathBuf)` resolves the path against the dev server / bundled assets root and **strips fragments**. Hash routing is for SPAs that own their URL bar, not Tauri webviews. `initialization_script` runs once, in the popup's webview, before the Dioxus wasm bundle boots — exactly the right hook for "tell this window which message it's for". The message id lands as `window.__QSL_READER_ID__`.

### Window cleanup is deferred (not full-cycle)

When the user closes a popup we drop the `Box<dyn EmailRenderer>` from `AppState`. The leaked `&'static LinuxGtkParent` (the `Box::leak`'d `gtk::Overlay` + `DrawingArea`) stays leaked. This is tolerated because:
- Each leak is ~a few KB plus the `gdk::Window` resources, dwarfed by the Servo dropped above it.
- A user opens at most a handful of popup windows per session.
- Properly cleaning up GTK widget hierarchies that have raw window handles still held by Servo internals is racy; the leak avoids that race entirely.

A periodic `n popup windows leaked` counter in tracing makes the cost visible if it ever matters.

---

## File structure

**Modify:**

- `apps/desktop/src-tauri/src/state.rs` — `servo_renderer: Mutex<Option<Box<dyn EmailRenderer>>>` → `servo_renderers: Mutex<HashMap<String, Box<dyn EmailRenderer>>>`.
- `apps/desktop/src-tauri/src/linux_gtk.rs` — replace `OnceLock<&'static LinuxGtkParent>` with `Mutex<HashMap<String, &'static LinuxGtkParent>>`. Public API: `register_parent(label, p)`, `parent(label)`, `remove_parent(label)`.
- `apps/desktop/src-tauri/src/renderer_bridge.rs` — split `install_servo_renderer(app)` into `install_for_window(app_handle, label) -> Result<Box<dyn EmailRenderer>, ...>`. The setup-time entry point in `main.rs::setup` calls it once with `"main"`.
- `apps/desktop/src-tauri/src/commands/reader.rs` — every command takes `tauri::Window` and uses `window.label()` for the lookup.
- `apps/desktop/src-tauri/src/commands/messages.rs` — add `messages_open_in_window` command.
- `apps/desktop/src-tauri/src/main.rs` — rename setup call site to use the per-window installer; register the new `messages_open_in_window` command in `invoke_handler!`.
- `apps/desktop/ui/src/app.rs` — add a JS shim returning `window.__QSL_READER_ID__`; root component branches on it; new `ReaderOnlyApp` component; new `ondoubleclick` handler on `MessageRowV2`.

**Create:**

- `apps/desktop/ui/src/reader_only.rs` — `ReaderOnlyApp` component (mounted when `__QSL_READER_ID__` is set). Reuses existing reader rendering via `compose_reader_html` already in `app.rs`.

**Don't touch:**

- `crates/renderer/*` — `ServoRenderer::new_linux` already accepts an `&LinuxGtkParent` per call; constructing a second instance is trivial. The cursor + link-click callbacks are per-instance too.
- `crates/mime/*` — sanitization is window-agnostic; the popup re-uses `messages_get` which already returns sanitized HTML.

---

## Known limitations

1. **GTK widget leak per popup** — see decision above. Logged but not freed.
2. **macOS / Windows code paths still build their own auxiliary window** — they're already per-process, but the multi-popup flow only hits the Linux path in this PR. Cross-platform popup flow lands when those `build_auxiliary_window` paths are removed (separate work).
3. **Servo state isolation** — popup windows do NOT share cookies, image cache, or DOM state with the main reader. Two popups of the same message render independently. Acceptable for a reader pane; the cookie isolation is a privacy plus.
4. **No "this message is open in a popup" inline-reader badge** — opening in popup leaves the inline reader unchanged. A follow-up could grey out the inline reader for popped-out messages, but it's not blocking.

---

## Implementation tasks

### Task 1: Per-label GTK parent registry

**Files:**
- Modify: `apps/desktop/src-tauri/src/linux_gtk.rs:55-75`

- [ ] **Step 1: Write failing tests for the new HashMap-shaped registry**

Add to the existing `#[cfg(test)] mod tests` (create one if absent) at the bottom of `linux_gtk.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // We can't construct a real LinuxGtkParent in a unit test (GTK
    // needs a display), so we test the registry's address-keyed
    // bookkeeping by leaking a sentinel and checking insert/get/remove.
    fn fresh_sentinel() -> &'static LinuxGtkParent {
        // Safety: never dereferenced in this test — we only check the
        // pointer-identity round-trip through the HashMap.
        unsafe { &*std::ptr::null::<LinuxGtkParent>() }
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
        register_parent("main", fresh_sentinel()); // idempotent / overwrite
        assert!(parent("main").is_some());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
QSL_SKIP_UI_BUILD=1 cargo test -p qsl-desktop --lib parent_registry_round_trip
```
Expected: FAIL — `register_parent` and `parent` don't take a label arg yet.

- [ ] **Step 3: Refactor the registry to keyed map**

Replace lines ~55-75 of `linux_gtk.rs` with:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

/// Per-window registry of leaked `LinuxGtkParent`s. Each Tauri window
/// (`"main"`, `"reader-<msg_id>"`, …) gets its own overlay + drawing
/// area, registered here when `install_servo_renderer_for_window` runs.
/// Look-ups are O(1) and the access pattern is one read per IPC call —
/// `Mutex` is fine.
static GTK_PARENTS: Mutex<Option<HashMap<String, &'static LinuxGtkParent>>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<String, &'static LinuxGtkParent>) -> R) -> R {
    let mut guard = GTK_PARENTS.lock().expect("GTK_PARENTS mutex poisoned");
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Get the registered parent for a window label, if any.
pub fn parent(label: &str) -> Option<&'static LinuxGtkParent> {
    with_registry(|m| m.get(label).copied())
}

/// Register the leaked parent for a window label. Overwrites any prior
/// entry for that label (used for hot-reload paths and re-installs).
pub fn register_parent(label: &str, p: &'static LinuxGtkParent) {
    with_registry(|m| {
        m.insert(label.to_string(), p);
    });
}

/// Drop the registry entry for a label. The pointed-at `LinuxGtkParent`
/// is `Box::leak`'d and stays in memory; this just removes the lookup
/// so future `reader_*` IPC calls for that label no-op.
pub fn remove_parent(label: &str) {
    with_registry(|m| {
        m.remove(label);
    });
}

#[cfg(test)]
fn clear_registry_for_test() {
    with_registry(|m| m.clear());
}
```

- [ ] **Step 4: Run tests, verify pass**

```
QSL_SKIP_UI_BUILD=1 cargo test -p qsl-desktop --lib parent_registry_round_trip registry_overwrites_same_label
```
Expected: PASS (2 tests).

- [ ] **Step 5: Update existing call sites in `linux_gtk.rs`**

Inside the same file, the cursor-callback closure (or anywhere else that reads `parent()` with no arg) — there should not be any such call yet inside `linux_gtk.rs` itself; the consumers are in `renderer_bridge.rs` and `commands/reader.rs`. Skip this step if `grep -n "parent()" linux_gtk.rs` returns nothing.

- [ ] **Step 6: Commit**

```
git add apps/desktop/src-tauri/src/linux_gtk.rs
git commit -m "refactor(desktop): per-window LinuxGtkParent registry

Replace the OnceLock<&'static LinuxGtkParent> with
Mutex<HashMap<String, &'static LinuxGtkParent>> keyed by Tauri window
label. Setup for popup-reader-window support.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Per-label Servo renderer registry on AppState

**Files:**
- Modify: `apps/desktop/src-tauri/src/state.rs:17-95`

- [ ] **Step 1: Refactor field declaration**

Change the field at `state.rs:68`:

```rust
    /// Servo-backed email renderers, keyed by Tauri window label
    /// (`"main"`, `"reader-<msg_id>"`, …). Empty when the `servo`
    /// feature is off. Each entry was installed by
    /// `install_servo_renderer_for_window` for that label.
    ///
    /// Wrapped in `tokio::sync::Mutex` because trait methods take
    /// `&mut self` (the renderer needs exclusive access during render).
    pub servo_renderers: Mutex<HashMap<String, Box<dyn EmailRenderer>>>,
```

- [ ] **Step 2: Update the initialiser**

Change `state.rs::AppState::new` (lines ~80-95) to:

```rust
            servo_renderers: Mutex::new(HashMap::new()),
```
(replacing the existing `servo_renderer: Mutex::new(None),` line).

- [ ] **Step 3: Compile-check expects errors**

```
QSL_SKIP_UI_BUILD=1 cargo check -p qsl-desktop
```
Expected: errors at every call site that referenced `servo_renderer` (singular). We fix those in Tasks 3 and 4.

- [ ] **Step 4: Commit (intentionally broken at workspace level)**

The next two tasks restore compilation.

```
git add apps/desktop/src-tauri/src/state.rs
git commit -m "refactor(state): rename servo_renderer to servo_renderers HashMap

Field-rename only; consumers are migrated in the follow-up commits.
Build is intentionally broken between this commit and the next.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Per-window Servo install path

**Files:**
- Modify: `apps/desktop/src-tauri/src/renderer_bridge.rs:69-180`
- Modify: `apps/desktop/src-tauri/src/main.rs::setup`

- [ ] **Step 1: Refactor `install_servo_renderer` into a per-window builder**

Replace the body of `install_servo_renderer` with a thin wrapper that calls the new per-window installer with `"main"`:

```rust
/// Install Servo for the main window. Called from Tauri's `setup`
/// hook. Popup windows install on first render via
/// `ensure_servo_for_window`.
pub fn install_servo_renderer<R: Runtime>(
    app: &tauri::App<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();
    let main_window = app
        .get_webview_window("main")
        .ok_or("main Tauri webview window missing at Servo install time")?;
    let renderer = build_servo_renderer_for_window(&app_handle, &main_window)?;
    let state: tauri::State<AppState> = app.state();
    let mut slot = tauri::async_runtime::block_on(state.servo_renderers.lock());
    slot.insert("main".to_string(), renderer);
    Ok(())
}

/// Install Servo for an arbitrary already-realized Tauri window. Used
/// by the popup-reader path: pop a `WebviewWindow`, then call this
/// with that window's handle. Idempotent — a second call for the
/// same label replaces the prior entry.
pub fn install_servo_renderer_for_window<R: Runtime>(
    app_handle: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = window.label().to_string();
    let renderer = build_servo_renderer_for_window(app_handle, window)?;
    let state: tauri::State<AppState> = app_handle.state();
    let mut slot = tauri::async_runtime::block_on(state.servo_renderers.lock());
    slot.insert(label, renderer);
    Ok(())
}

fn build_servo_renderer_for_window<R: Runtime>(
    app_handle: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) -> Result<Box<dyn EmailRenderer>, Box<dyn std::error::Error>> {
    let dispatcher: Arc<dyn MainThreadDispatch> = TauriDispatcher::new(app_handle.clone());
    let raw_renderer = build_servo_for(window, Arc::clone(&dispatcher))?;
    let mut renderer: Box<dyn EmailRenderer> = {
        tracing::info!(label = %window.label(), "qsl-desktop: Servo renderer installed");
        #[cfg(target_os = "linux")]
        let mut r = raw_renderer;
        #[cfg(target_os = "linux")]
        install_cursor_callback(&mut r, app_handle, window.label());
        Box::new(r)
    };
    renderer.on_link_click(Box::new(|url| {
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https" | "mailto") {
            tracing::warn!(%url, scheme, "qsl-desktop: rejecting non-http(s)/mailto link from reader");
            return;
        }
        let url_str = url.as_str();
        match webbrowser::open(url_str) {
            Ok(()) => tracing::info!(%url, "qsl-desktop: opened reader link in default browser"),
            Err(e) => tracing::warn!(%url, error = %e, "qsl-desktop: webbrowser::open failed"),
        }
    }));
    Ok(renderer)
}
```

Replace the platform-specific `build_servo_renderer` functions with `build_servo_for(window, dispatcher)`:

```rust
#[cfg(target_os = "linux")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    use crate::linux_gtk::LinuxGtkParent;
    let gtk_window = window
        .gtk_window()
        .map_err(|e| format!("cannot resolve GTK ApplicationWindow from window {}: {e}", window.label()))?;
    let parent: &'static LinuxGtkParent = Box::leak(Box::new(LinuxGtkParent::install(
        &gtk_window,
        READER_INITIAL_WIDTH as i32,
        READER_INITIAL_HEIGHT as i32,
    )?));
    crate::linux_gtk::register_parent(window.label(), parent);
    Ok(ServoRenderer::new_linux(
        dispatcher,
        parent,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(target_os = "macos")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_macos(
        dispatcher,
        window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(target_os = "windows")]
fn build_servo_for<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Ok(ServoRenderer::new_windows(
        dispatcher,
        window,
        PhysicalSize::new(READER_INITIAL_WIDTH, READER_INITIAL_HEIGHT),
    )?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn build_servo_for<R: Runtime>(
    _window: &tauri::WebviewWindow<R>,
    _dispatcher: Arc<dyn MainThreadDispatch>,
) -> Result<ServoRenderer, Box<dyn std::error::Error>> {
    Err("Servo renderer is not yet implemented on this platform".into())
}
```

Update `install_cursor_callback` to take a label and use `crate::linux_gtk::parent(label)`:

```rust
#[cfg(target_os = "linux")]
fn install_cursor_callback<R: Runtime>(
    renderer: &mut ServoRenderer,
    app_handle: &AppHandle<R>,
    label: &str,
) {
    let app_handle = app_handle.clone();
    let label = label.to_string();
    renderer.on_cursor_change(Box::new(move |cursor| {
        let css_name = cursor_to_css_name(cursor);
        let app_handle = app_handle.clone();
        let label = label.clone();
        let _ = app_handle.clone().run_on_main_thread(move || {
            use gtk::prelude::WidgetExt;
            let Some(parent) = crate::linux_gtk::parent(&label) else {
                return;
            };
            let Some(gdk_window) = parent.drawing_area.window() else {
                return;
            };
            let display = gdk::Window::display(&gdk_window);
            let gdk_cursor = gdk::Cursor::from_name(&display, css_name);
            gdk_window.set_cursor(gdk_cursor.as_ref());
        });
    }));
}
```

Delete `build_auxiliary_window`. The non-Linux `build_servo_for` arms now take the existing realized window directly.

- [ ] **Step 2: Verify the desktop crate compiles**

```
QSL_SKIP_UI_BUILD=1 cargo check -p qsl-desktop
```
Expected: still some errors in `commands/reader.rs` (next task) but `state.rs`, `renderer_bridge.rs`, `linux_gtk.rs` should compile clean.

- [ ] **Step 3: Commit**

```
git add apps/desktop/src-tauri/src/renderer_bridge.rs
git commit -m "refactor(renderer-bridge): per-window Servo installer

Split install_servo_renderer into install_servo_renderer_for_window
that takes any realized Tauri WebviewWindow. The setup-time installer
becomes a thin wrapper that calls it with 'main'. The popup path will
call it on first render of a new window.

Cursor callback gets the window label so it can look up the right
GTK parent in the new HashMap registry.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Window-scoped reader IPC commands

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands/reader.rs:41-201`

- [ ] **Step 1: Update `reader_render` to take `tauri::Window`**

```rust
#[tauri::command]
pub async fn reader_render(
    window: tauri::Window,
    state: State<'_, AppState>,
    input: ReaderRenderInput,
) -> IpcResult<()> {
    let label = window.label().to_string();
    tracing::debug!(window = %label, bytes = input.html.len(), "reader_render");

    // Lazy-install Servo for popup windows on first render. The main
    // window installs at setup time; a popup's first render is its
    // signal that a renderer is now needed.
    {
        let slot = state.servo_renderers.lock().await;
        if !slot.contains_key(&label) {
            drop(slot);
            #[cfg(feature = "servo")]
            if let Some(webview_window) = window.app_handle().get_webview_window(&label) {
                if let Err(e) = crate::renderer_bridge::install_servo_renderer_for_window(
                    &window.app_handle(),
                    &webview_window,
                ) {
                    tracing::warn!(window = %label, error = %e, "reader_render: lazy Servo install failed");
                    return Ok(());
                }
            }
        }
    }

    let mut guard = state.servo_renderers.lock().await;
    if let Some(renderer) = guard.get_mut(&label) {
        let _handle = renderer.render(&input.html, RenderPolicy::strict());
    } else {
        tracing::warn!(window = %label, "reader_render: no renderer installed for this window");
    }

    Ok(())
}
```

(Note the new `tauri::Window` parameter at first position — Tauri injects it automatically when the type appears in a command signature, no client-side change needed.)

- [ ] **Step 2: Update `reader_set_position` to take `tauri::Window`**

```rust
#[tauri::command]
pub async fn reader_set_position(
    window: tauri::Window,
    state: State<'_, AppState>,
    input: ReaderSetPositionInput,
) -> IpcResult<()> {
    let label = window.label().to_string();
    #[cfg(all(target_os = "linux", feature = "servo"))]
    {
        let Some(parent) = crate::linux_gtk::parent(&label) else {
            tracing::debug!(window = %label, "reader_set_position: GTK parent not registered yet");
            return Ok(());
        };
        let x = input.x.round() as i32;
        let y = input.y.round() as i32;
        let w = input.width.round() as i32;
        let h = input.height.round() as i32;
        tracing::debug!(window = %label, x, y, w, h, "reader_set_position");
        let app = window.app_handle();
        if let Err(e) = app.run_on_main_thread(move || parent.set_position(x, y, w, h)) {
            tracing::debug!(error = %e, "reader_set_position: GTK dispatch failed");
        }
        if w > 1 && h > 1 {
            let mut slot = state.servo_renderers.lock().await;
            if let Some(renderer) = slot.get_mut(&label) {
                renderer.resize(::dpi::PhysicalSize::new(w as u32, h as u32));
            }
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "servo")))]
    {
        let _ = window;
        let _ = state;
        let _ = input;
    }
    Ok(())
}
```

- [ ] **Step 3: Update `reader_clear` to take `tauri::Window`**

```rust
#[tauri::command]
pub async fn reader_clear(window: tauri::Window) -> IpcResult<()> {
    let label = window.label().to_string();
    #[cfg(all(target_os = "linux", feature = "servo"))]
    {
        let Some(parent) = crate::linux_gtk::parent(&label) else {
            return Ok(());
        };
        let app = window.app_handle();
        if let Err(e) = app.run_on_main_thread(move || parent.hide()) {
            tracing::debug!(error = %e, "reader_clear: dispatch failed");
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "servo")))]
    {
        let _ = window;
    }
    Ok(())
}
```

- [ ] **Step 4: Verify the desktop crate compiles**

```
QSL_SKIP_UI_BUILD=1 cargo check -p qsl-desktop
```
Expected: clean build.

- [ ] **Step 5: Commit**

```
git add apps/desktop/src-tauri/src/commands/reader.rs
git commit -m "refactor(reader-cmds): scope reader_* commands to calling window

reader_render / reader_set_position / reader_clear now use the
calling tauri::Window's label to look up the right Servo + GTK parent
in the per-window registries. Tauri auto-injects the Window argument,
so the JS-side invoke shape is unchanged.

reader_render also lazy-installs Servo on first call for any window
that doesn't already have one — that's how popup windows get their
renderer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `messages_open_in_window` Tauri command

**Files:**
- Modify: `apps/desktop/src-tauri/src/commands/messages.rs` (append at end)
- Modify: `apps/desktop/src-tauri/src/main.rs` (register command)

- [ ] **Step 1: Add the command at the end of `messages.rs`**

```rust
#[derive(Debug, Deserialize)]
pub struct MessagesOpenInWindowInput {
    pub id: MessageId,
}

/// `messages_open_in_window` — pop a new Tauri window that mounts the
/// reader-only Dioxus route for the supplied message id.
///
/// The popup's window label is `reader-<message_id>` (sanitized).
/// `reader_render` for that label will lazy-install a fresh Servo
/// instance on first call (see `commands/reader.rs::reader_render`).
/// On window close, AppState's per-window renderer is dropped and the
/// linux_gtk parent registry entry is removed; the leaked GTK widget
/// hierarchy stays in memory (a few KB per popup; see plan doc).
#[tauri::command]
pub async fn messages_open_in_window<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    input: MessagesOpenInWindowInput,
) -> IpcResult<()> {
    use tauri::Manager;

    // Sanitize the message id for use in a window label: only
    // [a-zA-Z0-9_-] are safe in Tauri labels per their docs. IMAP
    // ids contain `|` and `:`; replace with `_`.
    let safe_id: String = input
        .id
        .0
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let label = format!("reader-{safe_id}");

    // If the user double-clicks the same message twice, the second
    // call should focus the existing window rather than spawn a new one.
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }

    // initialization_script runs once in the new webview before our
    // wasm bundle boots. Setting __QSL_READER_ID__ here lets the
    // Dioxus root component branch on it without us having to
    // round-trip an IPC call at boot.
    let init_script = format!(
        "window.__QSL_READER_ID__ = {};",
        serde_json::to_string(&input.id.0).expect("serializing message id")
    );

    let _w = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title(format!("QSL — {}", input.id.0))
    .inner_size(720.0, 800.0)
    .initialization_script(&init_script)
    .build()
    .map_err(|e| qsl_ipc::IpcError::new(qsl_ipc::IpcErrorKind::Internal, format!("create reader window: {e}")))?;

    // Drop the renderer + GTK parent entry when the popup closes.
    // We only react to CloseRequested; the user can still cancel by
    // intercepting in their own listener if needed.
    {
        let app = app.clone();
        let label_for_close = label.clone();
        _w.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let app = app.clone();
                let label = label_for_close.clone();
                tauri::async_runtime::spawn(async move {
                    let state: tauri::State<AppState> = app.state();
                    state.servo_renderers.lock().await.remove(&label);
                    #[cfg(target_os = "linux")]
                    crate::linux_gtk::remove_parent(&label);
                    tracing::info!(window = %label, "popup reader window closed; renderer dropped");
                });
            }
        });
    }

    tracing::info!(window = %label, id = %input.id.0, "messages_open_in_window");
    Ok(())
}
```

- [ ] **Step 2: Register the command in `main.rs::invoke_handler!`**

Add the line `commands::messages::messages_open_in_window,` to the existing `invoke_handler!` macro list, alongside the other `messages::*` commands.

- [ ] **Step 3: Verify the desktop crate compiles**

```
QSL_SKIP_UI_BUILD=1 cargo check -p qsl-desktop
```
Expected: clean.

- [ ] **Step 4: Commit**

```
git add apps/desktop/src-tauri/src/commands/messages.rs apps/desktop/src-tauri/src/main.rs
git commit -m "feat(commands): messages_open_in_window — pop a reader window

New IPC command that creates a Tauri WebviewWindow titled
'reader-<safe_id>' with the message id injected into the popup's
JS context as window.__QSL_READER_ID__ via initialization_script.
The popup's first reader_render lazy-installs Servo for that label.

WindowEvent::CloseRequested drops the renderer and GTK parent entry;
GTK widgets stay leaked (a few KB each) — see plan doc for rationale.

Calling this for an already-open popup focuses the existing window
instead of spawning a duplicate.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Dioxus root branch + `ReaderOnlyApp`

**Files:**
- Create: `apps/desktop/ui/src/reader_only.rs`
- Modify: `apps/desktop/ui/src/lib.rs` (add `mod reader_only;`)
- Modify: `apps/desktop/ui/src/app.rs` (root branch + JS shim)

- [ ] **Step 1: Add a JS shim for the popup id**

Inside the existing `#[wasm_bindgen(inline_js = ...)]` block in `app.rs` (around line 50), append:

```javascript
    export function readerWindowMessageId() {
        return window.__QSL_READER_ID__ || null;
    }
```

And in the `extern "C"` block on the Rust side, add:

```rust
    fn readerWindowMessageId() -> JsValue;
```

- [ ] **Step 2: Create `reader_only.rs`**

```rust
// SPDX-License-Identifier: Apache-2.0

//! Standalone reader pane mounted in popup-window mode.
//!
//! Activated when `window.__QSL_READER_ID__` is set on boot — the
//! Tauri popup's `initialization_script` injects it before our wasm
//! bundle runs. The component fetches the message via `messages_get`,
//! renders the same headers + body + remote-content banner the inline
//! reader uses, and forwards to the Servo overlay via `reader_render`.

use dioxus::prelude::*;
use qsl_ipc::{MessageId, RenderedMessage};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::app::{compose_reader_html, format_reader_header, invoke, start_reader_body_tracker, web_sys_log, TAILWIND_CSS};

#[component]
pub fn ReaderOnlyApp(message_id: MessageId) -> Element {
    let id_for_resource = message_id.clone();
    let rendered = use_resource(move || {
        let id = id_for_resource.clone();
        async move {
            invoke::<RenderedMessage>("messages_get", serde_json::json!({ "input": { "id": id } })).await
        }
    });

    // Push reader-body bounding rects to the Rust side (same path the
    // inline reader uses). The popup's only reader-body element is
    // the .reader-body-fill div below; the tracker watches by class
    // selector so it works in any window.
    use_hook(|| {
        start_reader_body_tracker();
    });

    // Render Servo HTML once the message resolves.
    {
        let rendered_clone = rendered.clone();
        use_effect(move || {
            let read = rendered_clone.read_unchecked();
            let Some(Ok(msg)) = read.as_ref() else {
                return;
            };
            let html = compose_reader_html(msg);
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = invoke::<()>("reader_render", serde_json::json!({ "input": { "html": html } })).await {
                    web_sys_log(&format!("reader_render (popup): {e}"));
                }
            });
        });
    }

    rsx! {
        document::Stylesheet { href: TAILWIND_CSS }
        div {
            class: "popup-reader-shell",
            style: "display: grid; grid-template-rows: auto 1fr; height: 100vh;",
            match &*rendered.read_unchecked() {
                None => rsx! { p { class: "msglist-empty", "Loading message…" } },
                Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
                Some(Ok(msg)) => {
                    let header = format_reader_header(&msg.headers);
                    rsx! {
                        div { class: "reader-header-block",
                            "{header}"
                        }
                        div {
                            class: "reader-body-fill",
                            style: "min-height: 0;",
                        }
                    }
                }
            }
        }
    }
}
```

This component reuses three things from `app.rs`: `compose_reader_html`, `format_reader_header`, and `start_reader_body_tracker`. Make those `pub(crate)` if they aren't already.

- [ ] **Step 3: Add `mod reader_only;` to `apps/desktop/ui/src/lib.rs`**

Look up the file and append `pub mod reader_only;`. (If `lib.rs` doesn't exist and the entry point is `main.rs`, append it there.)

- [ ] **Step 4: Branch in the root `App` component**

In `apps/desktop/ui/src/app.rs`, just before the existing `App()` component body, change the wrapper so it detects popup mode:

```rust
#[component]
pub fn App() -> Element {
    // Popup mode: __QSL_READER_ID__ is set by the Tauri window's
    // initialization_script. Mount the reader-only component instead
    // of the three-pane shell.
    let popup_id: JsValue = readerWindowMessageId();
    if !popup_id.is_null() && !popup_id.is_undefined() {
        if let Some(id_str) = popup_id.as_string() {
            return rsx! {
                crate::reader_only::ReaderOnlyApp { message_id: MessageId(id_str) }
            };
        }
    }
    // Normal three-pane shell follows.
    full_app_shell()
}
```

Move the existing `App()` body into a new `fn full_app_shell() -> Element` function — easiest to do by renaming `App` → `full_app_shell` then adding the new `App` wrapper above.

- [ ] **Step 5: Workspace check + UI build**

```
QSL_SKIP_UI_BUILD=1 cargo check --workspace
```
Expected: clean.

If any of `compose_reader_html`, `format_reader_header`, or `start_reader_body_tracker` weren't already `pub(crate)`, the previous check will tell you. Fix visibility, re-check.

- [ ] **Step 6: Commit**

```
git add apps/desktop/ui/src/reader_only.rs apps/desktop/ui/src/lib.rs apps/desktop/ui/src/app.rs
git commit -m "feat(ui): ReaderOnlyApp — popup reader Dioxus route

Root App component branches on window.__QSL_READER_ID__ injected by
the popup's initialization_script. When set, mount ReaderOnlyApp
which fetches messages_get for the given id and renders the same
headers + Servo overlay the inline reader uses; otherwise mount the
three-pane shell as before.

Reuses compose_reader_html, format_reader_header, and
start_reader_body_tracker from the inline-reader path — no logic
duplication.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Double-click handler on `MessageRowV2`

**Files:**
- Modify: `apps/desktop/ui/src/app.rs` (`MessageRowV2` component)

- [ ] **Step 1: Add the handler**

Find `MessageRowV2` in `app.rs` (search for `fn MessageRowV2`). Add an `ondoubleclick` handler that invokes `messages_open_in_window`:

```rust
let id_for_popup = msg.id.clone();
let ondoubleclick = move |evt: Event<MouseData>| {
    evt.stop_propagation();
    let id = id_for_popup.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = invoke::<()>(
            "messages_open_in_window",
            serde_json::json!({ "input": { "id": id } }),
        ).await {
            web_sys_log(&format!("messages_open_in_window: {e}"));
        }
    });
};
```

Wire it on the row's outer `div` alongside the existing `onclick`:

```rust
div {
    class: ...,
    onclick: ...,
    ondoubleclick: ondoubleclick,
    ...
}
```

- [ ] **Step 2: Commit**

```
git add apps/desktop/ui/src/app.rs
git commit -m "feat(ui): double-click message row to open in popup window

MessageRowV2 now forwards ondoubleclick to the messages_open_in_window
IPC command. stop_propagation prevents the same event from also
firing onclick (single-click selection).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: Workspace verify + push

- [ ] **Step 1: Workspace clippy**

```
QSL_SKIP_UI_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings
```
Expected: zero warnings.

- [ ] **Step 2: Workspace tests**

```
QSL_SKIP_UI_BUILD=1 cargo test --workspace --no-fail-fast
```
Expected: zero failures. (No new tests beyond Task 1; the popup flow is integration-level and covered by manual smoke at the end.)

- [ ] **Step 3: Format check**

```
cargo fmt --all -- --check
```
If it fails, run `cargo fmt --all` and amend the relevant commit.

- [ ] **Step 4: Push branch**

```
git push -u origin feat/reader-popup-window
```

- [ ] **Step 5: Open PR**

```
gh pr create --title "feat(reader): double-click message → open in popup window" --body "$(cat <<'EOF'
## Summary

Double-clicking a row in the message list opens the message in a new full Tauri window with its own Servo-rendered reader pane. Closes the long-standing UX gap where you couldn't pop a message out for side-by-side reading.

**Architecture (full plan in \`docs/superpowers/plans/2026-04-27-reader-popup-window.md\`):**

- Per-window Servo. The single \`Mutex<Option<Box<dyn EmailRenderer>>>\` and \`OnceLock<&'static LinuxGtkParent>\` become per-label HashMaps. Each Tauri window — \`"main"\`, \`"reader-<msg_id>"\` — gets its own GTK overlay + DrawingArea + Servo instance.
- Popup install is lazy. The first \`reader_render\` IPC call for a new label triggers \`install_servo_renderer_for_window\`. No setup-time penalty for users who never pop.
- Routing is \`initialization_script\`. Tauri 2 strips URL fragments from \`WebviewUrl::App\`, so we inject \`window.__QSL_READER_ID__\` into the popup's JS context before the wasm bundle boots; the Dioxus root branches on it.
- Cleanup is partial. Closing a popup drops the renderer; the GTK widget hierarchy stays leaked (~few KB per popup). See plan doc § Known limitations for the rationale — full GTK cleanup races with raw window handles still held by Servo.

## Test plan

- [x] \`QSL_SKIP_UI_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings\`
- [x] \`QSL_SKIP_UI_BUILD=1 cargo test --workspace --no-fail-fast\`
- [x] \`cargo fmt --all -- --check\`
- [ ] Smoke: launch desktop, double-click an inbox row, popup window opens with the message rendered. Close popup, drop is logged.
- [ ] Smoke: double-click a second message; second popup opens independently. Both render correctly.
- [ ] Smoke: double-click the same message twice; second invocation focuses the existing popup instead of spawning a duplicate.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

### Task 9: Smoke test (interactive)

This task isn't automatable. After CI is green and the PR is open, run through the smoke list manually:

- [ ] **Step 1: Rebuild desktop**

```
QSL_SKIP_UI_BUILD=1 cargo run -p qsl-desktop --release
```

- [ ] **Step 2: Single-message popup**

In the running app: pick any inbox message, double-click the row. Confirm a new window opens with the message rendered. Close it.

- [ ] **Step 3: Multi-popup**

Double-click two different messages in quick succession. Confirm both windows open and each renders its own message correctly. Close both.

- [ ] **Step 4: Re-open same message**

Double-click a message twice. Confirm the second double-click focuses the existing window instead of stacking.

- [ ] **Step 5: Resize + scroll**

In an open popup, resize the window. Confirm Servo's overlay tracks the new size. Scroll the content. Confirm no flicker, no console errors.

- [ ] **Step 6: Close while inline reader is on the same message**

Open a message in the inline reader, then double-click to popup it. Close the popup. Confirm the inline reader is still rendered correctly (proves no cross-window state corruption).

If all five smoke checks pass, the PR is ready to merge.
