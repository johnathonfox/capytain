// SPDX-License-Identifier: Apache-2.0

//! Root Dioxus component for the QSL UI (wasm32-only).
//!
//! Phase 0 Week 5 renders a three-pane layout:
//!
//!   ┌──────────┬─────────────┬────────────────────┐
//!   │ Sidebar  │ Message     │ Reader pane        │
//!   │ accounts │ list for    │ headers + text/    │
//!   │ + folders│ selected    │ plain body         │
//!   │          │ folder      │                    │
//!   └──────────┴─────────────┴────────────────────┘
//!
//! All three panes are driven by global signals (selected account,
//! selected folder, selected message). Data comes from Tauri commands
//! over the `window.__TAURI_INTERNALS__.invoke` bridge; HTML
//! rendering via Servo arrives in Week 6.

use std::collections::HashMap;

use dioxus::prelude::*;
use qsl_ipc::{
    Account, AccountId, Attachment, Draft, DraftBodyKind, DraftId, EmailAddress, Folder, FolderId,
    MessageHeaders, MessageId, MessagePage, RenderedMessage, SortOrder, SyncEvent,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Stylesheet bundled into the wasm output. The `asset!` macro
/// hashes the file at compile time, copies it into the dx output
/// directory, and gives us back a stable URL — the older
/// `Dioxus.toml [web.resource] style = [...]` entry injects the
/// `<link>` tag but doesn't actually copy the file in dx 0.7, so
/// without `asset!` the stylesheet 404s and the UI renders
/// unstyled. Linked in `App()` via `document::Stylesheet`.
pub(crate) const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

// ---------- Tauri bridge ----------

// `window.__TAURI_INTERNALS__.invoke` is the stable IPC hook Tauri 2
// exposes on every window without configuration. The `window.__TAURI__`
// convenience namespace is only bound when `app.withGlobalTauri: true`
// is set in `tauri.conf.json`, which we don't enable — so reach for the
// internal hook directly. This is exactly what `@tauri-apps/api`'s
// `invoke()` wraps under the hood.
//
// `tauriListen` mirrors `@tauri-apps/api/event#listen`: registers the
// Rust callback via `transformCallback` (which assigns it a numeric id
// the Rust side can later use to unlisten) and tells the event plugin
// to start delivering. We use the same `kind: 'Any'` target the JS API
// defaults to so events emitted from any window reach this listener.
#[wasm_bindgen(inline_js = r#"
    export async function coreInvoke(cmd, args) {
        return await window.__TAURI_INTERNALS__.invoke(cmd, args);
    }
    export async function tauriListen(event, handler) {
        // Tauri's event hub delivers `{ event, id, payload }` to the
        // wrapped callback. Strip the wrapper before calling into
        // wasm so the Rust side gets just the typed payload — every
        // current consumer wants the payload, not the metadata.
        const wrapped = function(raw) {
            try { handler(raw && raw.payload); }
            catch (e) { console.warn('event handler:', e); }
        };
        const cbId = window.__TAURI_INTERNALS__.transformCallback(wrapped);
        return await window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
            event,
            target: { kind: 'Any' },
            handler: cbId,
        });
    }

    // Watch `.reader-body-fill`'s bounding rect and push it to the
    // Rust side over `reader_set_position`. ResizeObserver fires
    // whenever the element's content-box changes shape (window
    // resize, splitter drag, compose pane open/close); window resize
    // alone doesn't change the element's content-box if the column
    // is `1fr`, so we also listen on `window.resize` to catch
    // viewport-relative shifts that don't change the element size
    // but do change its `(x, y)`. Idempotent — repeat calls
    // tear down the previous observer first.
    export function startReaderBodyTracker() {
        if (window.__qslReaderTracker) {
            window.__qslReaderTracker.dispose();
        }
        const push = function() {
            const el = document.querySelector('.reader-body-fill');
            if (!el) {
                console.log('[qsl] push: no .reader-body-fill yet');
                return;
            }
            const r = el.getBoundingClientRect();
            // `getBoundingClientRect` returns CSS pixels; the GTK
            // overlay positions widgets in device pixels. On
            // fractional-scaling displays (KDE Wayland especially)
            // those two coordinate systems differ by
            // `devicePixelRatio` — multiply now so GTK lands the
            // surface where the user actually sees the slot.
            const dpr = window.devicePixelRatio || 1;
            const x = r.x * dpr;
            const y = r.y * dpr;
            const w = r.width * dpr;
            const h = r.height * dpr;
            console.log(
                '[qsl] push css',
                'x=' + r.x.toFixed(1),
                'y=' + r.y.toFixed(1),
                'w=' + r.width.toFixed(1),
                'h=' + r.height.toFixed(1),
                'dpr=' + dpr,
                '→ device',
                'x=' + x.toFixed(1),
                'w=' + w.toFixed(1)
            );
            if (w <= 0 || h <= 0) return;
            window.__TAURI_INTERNALS__
                .invoke('reader_set_position', {
                    input: { x: x, y: y, width: w, height: h },
                })
                .catch(function(e) { console.warn('reader_set_position:', e); });
        };
        // Run once now in case the element is already mounted.
        push();
        // ResizeObserver is per-element. Use a MutationObserver as a
        // boot-time safety net for the first paint when the element
        // doesn't exist yet — it'll start watching as soon as the
        // node is added.
        let resizeObs = null;
        const tryAttach = function() {
            const el = document.querySelector('.reader-body-fill');
            if (!el || resizeObs) return;
            resizeObs = new ResizeObserver(push);
            resizeObs.observe(el);
            push();
        };
        tryAttach();
        const mutObs = new MutationObserver(tryAttach);
        mutObs.observe(document.body, { childList: true, subtree: true });
        // Window resize: rect's (x, y) can shift even if the element
        // itself didn't change size (rare, but happens during splitter
        // drag if the window edge moves).
        const onResize = function() { push(); };
        window.addEventListener('resize', onResize);

        window.__qslReaderTracker = {
            dispose: function() {
                if (resizeObs) resizeObs.disconnect();
                mutObs.disconnect();
                window.removeEventListener('resize', onResize);
            },
            push: push,
        };
    }

    // Returns the popup-window message id, or null when this isn't
    // a popup. The Tauri `messages_open_in_window` command sets
    // `window.__QSL_READER_ID__` via initialization_script before
    // the wasm bundle boots — the Dioxus root reads it and mounts
    // the reader-only component instead of the three-pane shell.
    export function readerWindowMessageId() {
        return window.__QSL_READER_ID__ || null;
    }

    // Returns the JSON-serialized RenderedMessage the Tauri command
    // pre-fetched for this popup, or null when the preload isn't
    // available (older host, fetch failed, etc.). Stringifying on
    // the JS side means the wasm code can deserialize through the
    // same serde path it uses for IPC results.
    export function readerWindowPreload() {
        return window.__QSL_READER_PRELOAD__
            ? JSON.stringify(window.__QSL_READER_PRELOAD__)
            : null;
    }

    // Force one push on demand. Useful right after the user clicks
    // a different message — Dioxus may have re-laid-out the body
    // slot and we want Servo's surface re-positioned in the same
    // animation frame.
    export function pushReaderBodyRect() {
        if (window.__qslReaderTracker) {
            window.__qslReaderTracker.push();
        }
    }
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = coreInvoke)]
    async fn core_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = tauriListen)]
    async fn tauri_listen(event: &str, handler: &js_sys::Function) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_name = startReaderBodyTracker)]
    pub(crate) fn start_reader_body_tracker();

    #[wasm_bindgen(js_name = pushReaderBodyRect)]
    pub(crate) fn push_reader_body_rect();

    /// Returns the message id this popup window is for, set by the
    /// Tauri `WebviewWindowBuilder::initialization_script` in
    /// `messages_open_in_window`. `JsValue::null` when the global
    /// isn't set, i.e. this is the main three-pane window.
    #[wasm_bindgen(js_name = readerWindowMessageId)]
    pub(crate) fn reader_window_message_id() -> JsValue;

    /// Returns the JSON-stringified `RenderedMessage` the Tauri host
    /// pre-fetched into `window.__QSL_READER_PRELOAD__`, or
    /// `JsValue::null` when no preload is available. The reader-only
    /// component uses this to render the popup body without a
    /// follow-up `messages_get` IPC round-trip.
    #[wasm_bindgen(js_name = readerWindowPreload)]
    pub(crate) fn reader_window_preload() -> JsValue;
}

/// Thin wrapper around the Tauri `invoke` bridge. Serializes `args` to
/// JSON, forwards to JS, and deserializes the return value into `T`.
///
/// The return path round-trips through `JSON.stringify` +
/// `serde_json::from_str` instead of `serde_wasm_bindgen::from_value`.
/// `serde-wasm-bindgen` 0.6 silently dropped `Option<FolderRole>` to
/// `None` for several externally-tagged unit-variant tag values that the
/// Rust side ships as plain JSON strings, so the sidebar saw INBOX +
/// All Mail and lost the rest of the [Gmail]/* mailboxes. Tauri 2's
/// command results are JSON-encoded over the wire anyway, so the
/// round-trip is just re-stringifying the value the JS bridge already
/// `JSON.parse`d — no information loss, and `serde_json` handles
/// externally-tagged enums correctly.
pub(crate) async fn invoke<T>(cmd: &str, args: impl Serialize) -> Result<T, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let js_args = serde_wasm_bindgen::to_value(&args).map_err(|e| e.to_string())?;
    let js_ret = core_invoke(cmd, js_args)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let json = js_sys::JSON::stringify(&js_ret)
        .map_err(|e| format!("invoke {cmd}: JSON.stringify: {e:?}"))?
        .as_string()
        .ok_or_else(|| format!("invoke {cmd}: JSON.stringify returned non-string"))?;
    serde_json::from_str(&json).map_err(|e| format!("invoke {cmd}: {e}"))
}

// ---------- Selection state ----------

/// Global selection state shared across all three panes.
///
/// `unified` toggles between the per-folder view and the unified
/// inbox (every account's INBOX-role folder merged). When set, the
/// message-list pane invokes `messages_list_unified` and ignores
/// `account` / `folder`. Selecting a regular folder clears it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub account: Option<AccountId>,
    pub folder: Option<FolderId>,
    pub message: Option<MessageId>,
    pub unified: bool,
}

/// Compose-pane open state. `None` means the message-list pane is
/// shown in the middle; `Some` means the compose form is shown
/// instead, optionally rehydrated from a persisted draft id.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposeState {
    /// Default account to compose from. Pre-filled from the current
    /// selection if one is highlighted, otherwise the first
    /// available account.
    pub default_account: Option<AccountId>,
    /// Draft to rehydrate. `None` opens a fresh compose; `Some`
    /// pulls fields via `drafts_load` on mount.
    pub draft_id: Option<DraftId>,
}

/// Global revision counter, bumped on every `sync_event`. Drives
/// resources whose work is structurally cross-folder — folder lists in
/// the sidebar (unread counts can change in any folder) and the
/// unified inbox.
pub type SyncTick = Signal<u64>;

/// Per-folder revision counter. The sync listener bumps the entry for
/// the folder named in `SyncEvent::FolderSynced` / `FolderError`, so a
/// per-folder `MessageListV2` only refetches when *its* folder
/// actually synced. Without this, a 10-folder bootstrap pass would
/// trigger 10 refetches of the visible folder; with it, only the
/// matching event triggers work.
pub type FolderTokens = Signal<HashMap<FolderId, u64>>;

// ---------- Root ----------

#[component]
pub fn App() -> Element {
    // Popup mode detection: the Tauri popup window's
    // `initialization_script` injects `window.__QSL_READER_ID__`
    // before the wasm bundle boots. When that's set, mount the
    // standalone reader instead of the three-pane shell.
    let popup_id_js: JsValue = reader_window_message_id();
    if !popup_id_js.is_null() && !popup_id_js.is_undefined() {
        if let Some(id_str) = popup_id_js.as_string() {
            return rsx! {
                crate::reader_only::ReaderOnlyApp {
                    message_id: MessageId(id_str)
                }
            };
        }
    }
    full_app_shell()
}

fn full_app_shell() -> Element {
    let selection = use_signal(Selection::default);
    let mut sync_tick: SyncTick = use_signal(|| 0u64);
    let mut folder_tokens: FolderTokens = use_signal(HashMap::new);
    let compose: Signal<Option<ComposeState>> = use_signal(|| None);

    // ComposePane occupies the reader slot when active, so the Servo
    // overlay surface — which paints over `.reader-body-fill` and is
    // positioned by a JS-side ResizeObserver — must be hidden whenever
    // compose opens. ReaderPaneV2 has its own `reader_clear` effect
    // for the no-message case, but it's unmounted while ComposePane
    // is up, so the reactive guard goes here at the App level.
    {
        let composing = compose.read().is_some();
        use_effect(use_reactive!(|composing| {
            if composing {
                wasm_bindgen_futures::spawn_local(async {
                    let _ = invoke::<()>("reader_clear", serde_json::json!({})).await;
                });
            }
        }));
    }
    // Most recent sync_event payload, rendered in the bottom status
    // bar. `None` means we haven't seen any events yet — the bar
    // shows "Initializing…" / "Syncing…" until the first event lands.
    let mut sync_status: Signal<SyncStatus> = use_signal(|| SyncStatus::Initializing);

    // Pane widths in CSS pixels. Defaults match the original
    // 200/280/rest layout. Drag handlers below mutate these.
    let mut sidebar_w = use_signal(|| 240u32);
    let mut list_w = use_signal(|| 320u32);
    // `Some((target, start_x_in_client_pixels, start_width))` while
    // a splitter is being dragged; `None` otherwise. The overlay div
    // renders only when this is `Some`, keeping the iframe from
    // capturing pointer events mid-drag.
    let mut drag = use_signal(|| Option::<(SplitTarget, f64, u32)>::None);

    // Listen for sync engine events: bump sync_tick so cross-folder
    // resources (sidebar, unified inbox) refetch, bump the per-folder
    // token so the message list for that specific folder refetches,
    // and update the status bar signal with the parsed payload. The
    // JS-side `tauriListen` wrapper passes us just `event.payload`,
    // which deserializes straight into a `SyncEvent` enum.
    use_hook(move || {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |payload: JsValue| {
            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            if let Ok(evt) = serde_wasm_bindgen::from_value::<SyncEvent>(payload) {
                let folder = match &evt {
                    SyncEvent::FolderSynced { folder, .. }
                    | SyncEvent::FolderError { folder, .. } => folder.clone(),
                };
                folder_tokens.with_mut(|m| {
                    let entry = m.entry(folder).or_insert(0);
                    *entry = entry.wrapping_add(1);
                });
                sync_status.set(SyncStatus::from_event(&evt));
            }
        });
        wasm_bindgen_futures::spawn_local(async move {
            let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
            if let Err(e) = tauri_listen("sync_event", func).await {
                web_sys_log(&format!("sync_event listen failed: {e:?}"));
            }
            Box::leak(Box::new(cb));
        });
    });

    // Reader-body tracker: watches the `.reader-body-fill` element's
    // bounding rect (ResizeObserver + window resize) and pushes
    // `(x, y, w, h)` to the Rust side via `reader_set_position`,
    // which moves Servo's overlay surface to track. Servo handles
    // link clicks natively via `on_link_click` → `webbrowser::open`,
    // so we no longer need the iframe-side postMessage bridge.
    use_hook(|| {
        start_reader_body_tracker();
    });

    // Tell the Rust side the UI is mounted so the sync engine can
    // start its bootstrap pass. Without this signal the engine waits
    // up to 10s before kicking off, but firing it explicitly keeps
    // the gate tight on a healthy launch. Fires once per app session.
    use_hook(|| {
        wasm_bindgen_futures::spawn_local(async {
            if let Err(e) = invoke::<()>("ui_ready", serde_json::json!({})).await {
                web_sys_log(&format!("ui_ready: {e}"));
            }
        });
    });

    let onmousemove_shell = move |e: Event<MouseData>| {
        let Some((target, start_x, start_w)) = *drag.read() else {
            return;
        };
        let dx = e.client_coordinates().x - start_x;
        let new_w = (start_w as f64 + dx).round();
        let clamped = match target {
            SplitTarget::Sidebar => new_w.clamp(160.0, 480.0) as u32,
            SplitTarget::List => new_w.clamp(220.0, 720.0) as u32,
        };
        match target {
            SplitTarget::Sidebar => sidebar_w.set(clamped),
            SplitTarget::List => list_w.set(clamped),
        }
    };
    let onmouseup_shell = move |_| drag.set(None);

    let onmousedown_sidebar = move |e: Event<MouseData>| {
        e.prevent_default();
        drag.set(Some((
            SplitTarget::Sidebar,
            e.client_coordinates().x,
            sidebar_w(),
        )));
    };
    let onmousedown_list = move |e: Event<MouseData>| {
        e.prevent_default();
        drag.set(Some((
            SplitTarget::List,
            e.client_coordinates().x,
            list_w(),
        )));
    };

    let grid_style = format!(
        "grid-template-columns: {}px 5px {}px 5px 1fr;",
        sidebar_w(),
        list_w()
    );
    let dragging = drag.read().is_some();

    rsx! {
        document::Stylesheet { href: TAILWIND_CSS }
        div {
            class: "app-shell",
            style: "{grid_style}",
            onmousemove: onmousemove_shell,
            onmouseup: onmouseup_shell,
            onmouseleave: onmouseup_shell,
            div {
                class: "shell-pane shell-pane-sidebar",
                SidebarV2 { selection, sync_tick, compose }
            }
            div {
                class: "shell-splitter",
                onmousedown: onmousedown_sidebar,
            }
            div {
                class: "shell-pane shell-pane-list",
                MessageListPaneV2 { selection, sync_tick, folder_tokens }
            }
            div {
                class: "shell-splitter",
                onmousedown: onmousedown_list,
            }
            div {
                class: "shell-pane shell-pane-reader",
                if compose.read().is_some() {
                    ComposePane { compose, sync_tick }
                } else {
                    ReaderPaneV2 { selection, sync_tick, compose }
                }
            }
            // Full-window overlay during drag so the reader's iframe
            // (and any other webview child) can't capture pointer
            // events. Pointer events still bubble to `.app-shell`,
            // which is where the move/up handlers live.
            if dragging {
                div { class: "shell-drag-overlay" }
            }
            StatusBar { status: sync_status }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitTarget {
    Sidebar,
    List,
}

// ---------- Status bar ----------

/// Sync activity surfaced in the bottom status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    /// No sync_event has landed yet — engine is still in its
    /// pre-bootstrap wait or just kicking off.
    Initializing,
    /// Most recent successful folder cycle. `live=false` rows
    /// (bootstrap pass) get a slightly different label.
    Synced {
        folder: String,
        added: u32,
        updated: u32,
        live: bool,
    },
    /// Most recent failure, kept around until the next successful
    /// cycle replaces it.
    Failed { folder: String, error: String },
}

impl SyncStatus {
    fn from_event(evt: &SyncEvent) -> Self {
        match evt {
            SyncEvent::FolderSynced {
                folder,
                added,
                updated,
                live,
                ..
            } => SyncStatus::Synced {
                folder: short_folder_label(&folder.0),
                added: *added,
                updated: *updated,
                live: *live,
            },
            SyncEvent::FolderError { folder, error, .. } => SyncStatus::Failed {
                folder: short_folder_label(&folder.0),
                error: error.clone(),
            },
        }
    }
}

/// Render a folder id like "imap:user@host:INBOX" → "INBOX". Falls
/// back to the raw id when it doesn't carry a colon-separated tail.
fn short_folder_label(folder_id: &str) -> String {
    folder_id
        .rsplit_once(':')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| folder_id.to_string())
}

#[component]
fn StatusBar(status: Signal<SyncStatus>) -> Element {
    let snapshot = status.read().clone();
    let (dot_class, label) = match &snapshot {
        SyncStatus::Initializing => ("status-dot working", "Initializing…".to_string()),
        SyncStatus::Synced {
            folder,
            added,
            updated,
            live,
        } => {
            let prefix = if *live { "Synced" } else { "Loaded" };
            let counts = if *added == 0 && *updated == 0 {
                String::new()
            } else if *updated == 0 {
                format!(" · {added} new")
            } else if *added == 0 {
                format!(" · {updated} updated")
            } else {
                format!(" · {added} new · {updated} updated")
            };
            ("status-dot ok", format!("{prefix} {folder}{counts}"))
        }
        SyncStatus::Failed { folder, error } => (
            "status-dot error",
            format!("Sync failed: {folder} — {error}"),
        ),
    };

    rsx! {
        footer {
            class: "status-bar",
            span { class: "{dot_class}" }
            span { class: "status-label", "{label}" }
        }
    }
}

/// Tiny `console.log` shim — saves pulling `web-sys` for one call.
pub(crate) fn web_sys_log(msg: &str) {
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console)]
        fn log(s: &str);
    }
    log(msg);
}

// ---------- Reader-pane HTML composer ----------

/// Build the HTML document the Servo reader pane renders when a
/// message is selected.
///
/// Preference order for the body section:
///
/// 1. `rendered.sanitized_html` — populated by `messages_get` via
///    `qsl_mime::sanitize_email_html` (Phase 1 Week 7). This
///    is the normal path for modern email and carries the original
///    layout, tables, inline styles, etc.
/// 2. `rendered.body_text` escaped through [`minimal_escape`] and
///    wrapped in `<pre>` — fallback for messages that have no
///    `text/html` alternative or whose sanitized HTML came back
///    empty (very aggressive strip).
/// 3. A small "no body cached yet" hint otherwise.
///
/// Headers (subject, from, date) are always escaped via
/// `minimal_escape`; they never go through the ammonia pipeline
/// because they're always plain text at source.
pub(crate) fn compose_reader_html(rendered: &RenderedMessage) -> String {
    let body_section = render_body_section(rendered);

    // Headers (subject / from / date / recipients) are rendered by
    // the Dioxus side as a styled card; Servo's pane is body-only,
    // so the user sees each piece of info exactly once whether
    // Servo is reparented next to the webview (Linux) or running
    // in a separate auxiliary window (macOS / Windows).
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    body {{
      font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      color: #e6e8eb;
      background: #0f1115;
      margin: 0;
      padding: 1.25rem;
    }}
    @media (prefers-color-scheme: light) {{
      body {{ color: #14161a; background: #ffffff; }}
    }}
    .qsl-body {{ color: inherit; }}
    .qsl-body pre {{ white-space: pre-wrap; word-wrap: break-word; margin: 0; font: inherit; }}
    .qsl-body a {{ color: #74b4ff; }}
    @media (prefers-color-scheme: light) {{
      .qsl-body a {{ color: #2563eb; }}
    }}
  </style>
</head>
<body>
  <div class="qsl-body">{body_section}</div>
  <script>
    // Click forwarder. Servo's `request_navigation` fires on every
    // navigation Servo initiates, but plain anchor clicks inside a
    // `data:` URL document don't always make it through Servo's
    // input pipeline (GTK DrawingArea doesn't auto-forward mouse
    // events to Servo's input system on Linux). Catching the click
    // in JS and explicitly setting `window.location.href` triggers
    // a navigation request that the renderer delegate intercepts
    // and routes to `webbrowser::open`. The delegate denies the
    // navigation in-page, so the email content stays put.
    document.addEventListener('click', function(e) {{
      var node = e.target;
      while (node && node.nodeName !== 'A') node = node.parentNode;
      if (node && node.href) {{
        e.preventDefault();
        try {{ window.location.href = node.href; }} catch (err) {{}}
      }}
    }}, true);
  </script>
</body>
</html>"#
    )
}

/// Pick the right body rendering for the reader pane. Separated
/// from `compose_reader_html` so the preference order is easy to
/// read and test.
fn render_body_section(rendered: &RenderedMessage) -> String {
    // 1. Sanitized HTML if present and non-empty after trim.
    if let Some(html) = rendered.sanitized_html.as_deref() {
        if !html.trim().is_empty() {
            return html.to_string();
        }
    }
    // 2. Plaintext body through minimal_escape + <pre>.
    if let Some(text) = rendered.body_text.as_deref() {
        if !text.trim().is_empty() {
            return format!("<pre>{}</pre>", minimal_escape(text));
        }
    }
    // 3. "Nothing to show" — `messages_get` already lazy-fetches the
    // body if it's missing locally, so reaching this branch means
    // the message genuinely has no body content (headers-only, or
    // a fetch error already surfaced).
    "<em>No body content available for this message.</em>".to_string()
}

/// Minimal HTML escaping for text content. Not a full sanitizer —
/// for Phase 0 it's only used on fields we *know* are plain text
/// (subject, display-name, address, plaintext body). Phase 1's
/// ammonia pass replaces this for anything that started as HTML.
fn minimal_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Two-letter initials for an avatar circle, mirroring
/// `account_initials` but pulled from a free-form display name +
/// fallback address.
fn address_initials(name: &str, addr: &str) -> String {
    let mut chars: Vec<char> = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .take(2)
        .collect();
    if chars.is_empty() {
        if let Some(first) = addr.chars().next() {
            chars.push(first.to_ascii_uppercase());
        }
    }
    chars.iter().collect()
}

/// Compact byte-size formatter for attachment chips. Uses
/// IEC-style binary units (KiB/MiB) with one decimal of precision
/// for the `<10 MiB` range and integer truncation past that. Free
/// function so the [`AttachmentChips`] component stays focused.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    }
}

// ---------- Middle pane: compose ----------

#[component]
fn ComposePane(compose: Signal<Option<ComposeState>>, sync_tick: SyncTick) -> Element {
    let initial = compose.read().clone();
    let Some(initial) = initial else {
        return rsx! { p { class: "hint", "No compose state — this shouldn't render." } };
    };

    let accounts = use_resource(|| async { invoke::<Vec<Account>>("accounts_list", ()).await });

    // Pre-load draft if we were opened with one. `loaded` flips
    // true after the load completes (or immediately for fresh
    // composes) so the form fields don't render before they're
    // populated.
    let draft_id = use_signal(|| initial.draft_id.clone());
    let mut account_id = use_signal(|| initial.default_account.clone());
    let mut to_str = use_signal(String::new);
    let mut cc_str = use_signal(String::new);
    let mut bcc_str = use_signal(String::new);
    let mut subject = use_signal(String::new);
    let mut body = use_signal(String::new);
    let mut loaded = use_signal(|| initial.draft_id.is_none());
    let mut last_change = use_signal(|| 0u64);
    let mut save_status: Signal<SaveStatus> = use_signal(|| SaveStatus::Idle);
    let send_in_flight: Signal<bool> = use_signal(|| false);
    // Reply context from the original draft (when opened via Reply /
    // Reply-All / Forward). Not edited by the user but must survive
    // every save round-trip so the eventual `messages_send` call
    // sees the correct `In-Reply-To` / `References` in storage.
    let mut in_reply_to = use_signal(|| None::<String>);
    let mut references = use_signal(Vec::<String>::new);

    // One-shot draft hydration. `use_hook` runs at mount; the
    // dependency-free `use_future` would refire on every render.
    use_hook({
        let initial_id = initial.draft_id.clone();
        move || {
            if let Some(id) = initial_id {
                wasm_bindgen_futures::spawn_local(async move {
                    let result = invoke::<Draft>(
                        "drafts_load",
                        serde_json::json!({ "input": { "id": id } }),
                    )
                    .await;
                    match result {
                        Ok(d) => {
                            account_id.set(Some(d.account_id));
                            to_str.set(format_addrs(&d.to));
                            cc_str.set(format_addrs(&d.cc));
                            bcc_str.set(format_addrs(&d.bcc));
                            subject.set(d.subject);
                            body.set(d.body);
                            in_reply_to.set(d.in_reply_to);
                            references.set(d.references);
                            loaded.set(true);
                        }
                        Err(e) => {
                            web_sys_log(&format!("drafts_load: {e}"));
                            loaded.set(true);
                        }
                    }
                });
            }
        }
    });

    // Auto-save: every change bumps `last_change`. A spawned future
    // sleeps 5s, then checks whether `last_change` advanced; if not,
    // it persists. Multiple concurrent futures are fine — only the
    // one whose snapshot still matches will actually save.
    use_effect({
        let acc_signal = account_id;
        let to_signal = to_str;
        let cc_signal = cc_str;
        let bcc_signal = bcc_str;
        let subject_signal = subject;
        let body_signal = body;
        let in_reply_to_signal = in_reply_to;
        let references_signal = references;
        let mut draft_signal = draft_id;
        let last_change_signal = last_change;
        let mut status_signal = save_status;
        let mut sync_tick = sync_tick;
        move || {
            let current = last_change_signal();
            if current == 0 {
                return;
            }
            let acc = acc_signal.read().clone();
            let Some(acc) = acc else { return };
            let to = parse_addrs(&to_signal.read());
            let cc = parse_addrs(&cc_signal.read());
            let bcc = parse_addrs(&bcc_signal.read());
            let subject_text = subject_signal.read().clone();
            let body_text = body_signal.read().clone();
            let id = draft_signal.read().clone();
            let in_reply_to_val = in_reply_to_signal.read().clone();
            let references_val = references_signal.read().clone();

            wasm_bindgen_futures::spawn_local(async move {
                gloo_timers::future::sleep(std::time::Duration::from_secs(5)).await;
                if last_change_signal() != current {
                    return; // a more recent edit will handle the save
                }
                let payload = serde_json::json!({
                    "input": {
                        "draft": {
                            "id": id,
                            "account_id": acc,
                            "to": to,
                            "cc": cc,
                            "bcc": bcc,
                            "subject": subject_text,
                            "body": body_text,
                            "body_kind": DraftBodyKind::Plain,
                            "attachments": Vec::<()>::new(),
                            "in_reply_to": in_reply_to_val,
                            "references": references_val,
                        }
                    }
                });
                match invoke::<DraftId>("drafts_save", payload).await {
                    Ok(new_id) => {
                        draft_signal.set(Some(new_id));
                        status_signal.set(SaveStatus::Saved);
                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                    }
                    Err(e) => {
                        web_sys_log(&format!("drafts_save (auto): {e}"));
                        status_signal.set(SaveStatus::Error(e));
                    }
                }
            });
        }
    });

    // Manual save. Mirrors the auto-save closure but skips the 5s
    // wait. Used for the toolbar Save button + onkeydown Ctrl-S.
    let mut manual_save = {
        let acc_signal = account_id;
        let to_signal = to_str;
        let cc_signal = cc_str;
        let bcc_signal = bcc_str;
        let subject_signal = subject;
        let body_signal = body;
        let in_reply_to_signal = in_reply_to;
        let references_signal = references;
        let mut draft_signal = draft_id;
        let mut status_signal = save_status;
        let mut sync_tick = sync_tick;
        move || {
            let acc = acc_signal.read().clone();
            let Some(acc) = acc else { return };
            let to = parse_addrs(&to_signal.read());
            let cc = parse_addrs(&cc_signal.read());
            let bcc = parse_addrs(&bcc_signal.read());
            let subject_text = subject_signal.read().clone();
            let body_text = body_signal.read().clone();
            let id = draft_signal.read().clone();
            let in_reply_to_val = in_reply_to_signal.read().clone();
            let references_val = references_signal.read().clone();
            status_signal.set(SaveStatus::Saving);
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({
                    "input": {
                        "draft": {
                            "id": id,
                            "account_id": acc,
                            "to": to,
                            "cc": cc,
                            "bcc": bcc,
                            "subject": subject_text,
                            "body": body_text,
                            "body_kind": DraftBodyKind::Plain,
                            "attachments": Vec::<()>::new(),
                            "in_reply_to": in_reply_to_val,
                            "references": references_val,
                        }
                    }
                });
                match invoke::<DraftId>("drafts_save", payload).await {
                    Ok(new_id) => {
                        draft_signal.set(Some(new_id));
                        status_signal.set(SaveStatus::Saved);
                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                    }
                    Err(e) => {
                        web_sys_log(&format!("drafts_save (manual): {e}"));
                        status_signal.set(SaveStatus::Error(e));
                    }
                }
            });
        }
    };

    // Send: save the draft (so the server-side row reflects current
    // editor state), then enqueue an `OP_SUBMIT_MESSAGE` outbox row
    // via `messages_send`. On success, close the compose pane —
    // the drain worker takes over from there.
    let mut send_now = {
        let acc_signal = account_id;
        let to_signal = to_str;
        let cc_signal = cc_str;
        let bcc_signal = bcc_str;
        let subject_signal = subject;
        let body_signal = body;
        let in_reply_to_signal = in_reply_to;
        let references_signal = references;
        let mut draft_signal = draft_id;
        let mut status_signal = save_status;
        let mut sync_tick = sync_tick;
        let mut compose_signal = compose;
        let mut sending = send_in_flight;
        move || {
            if *sending.read() {
                return;
            }
            let acc = acc_signal.read().clone();
            let Some(acc) = acc else { return };
            let to = parse_addrs(&to_signal.read());
            let cc = parse_addrs(&cc_signal.read());
            let bcc = parse_addrs(&bcc_signal.read());
            if to.is_empty() && cc.is_empty() && bcc.is_empty() {
                status_signal.set(SaveStatus::Error("Add at least one recipient".into()));
                return;
            }
            let subject_text = subject_signal.read().clone();
            let body_text = body_signal.read().clone();
            let id = draft_signal.read().clone();
            let in_reply_to_val = in_reply_to_signal.read().clone();
            let references_val = references_signal.read().clone();
            sending.set(true);
            status_signal.set(SaveStatus::Saving);
            wasm_bindgen_futures::spawn_local(async move {
                let save_payload = serde_json::json!({
                    "input": {
                        "draft": {
                            "id": id,
                            "account_id": acc,
                            "to": to,
                            "cc": cc,
                            "bcc": bcc,
                            "subject": subject_text,
                            "body": body_text,
                            "body_kind": DraftBodyKind::Plain,
                            "attachments": Vec::<()>::new(),
                            "in_reply_to": in_reply_to_val,
                            "references": references_val,
                        }
                    }
                });
                let saved_id = match invoke::<DraftId>("drafts_save", save_payload).await {
                    Ok(id) => id,
                    Err(e) => {
                        web_sys_log(&format!("messages_send: pre-save: {e}"));
                        status_signal.set(SaveStatus::Error(e));
                        sending.set(false);
                        return;
                    }
                };
                draft_signal.set(Some(saved_id.clone()));
                let send_payload = serde_json::json!({
                    "input": { "draft_id": saved_id }
                });
                match invoke::<()>("messages_send", send_payload).await {
                    Ok(()) => {
                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                        compose_signal.set(None);
                    }
                    Err(e) => {
                        web_sys_log(&format!("messages_send: {e}"));
                        status_signal.set(SaveStatus::Error(format!("Send failed: {e}")));
                        sending.set(false);
                    }
                }
            });
        }
    };

    let mut discard = {
        let mut compose_signal = compose;
        let mut sync_tick = sync_tick;
        move || {
            if let Some(id) = draft_id.read().clone() {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = invoke::<()>(
                        "drafts_delete",
                        serde_json::json!({ "input": { "id": id } }),
                    )
                    .await;
                });
            }
            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            compose_signal.set(None);
        }
    };

    let mut close_compose = {
        let mut compose_signal = compose;
        move || compose_signal.set(None)
    };

    let bump = move || {
        last_change.with_mut(|n| *n = n.wrapping_add(1));
        save_status.set(SaveStatus::Dirty);
    };

    rsx! {
        section {
            class: "compose-pane",
            header {
                class: "compose-head",
                strong { "New message" }
                span { class: "compose-spacer" }
                ComposeStatusLabel { status: save_status }
                button {
                    class: "compose-action secondary",
                    r#type: "button",
                    onclick: move |_| close_compose(),
                    "Close"
                }
                button {
                    class: "compose-action danger",
                    r#type: "button",
                    onclick: move |_| discard(),
                    "Discard"
                }
                button {
                    class: "compose-action secondary",
                    r#type: "button",
                    onclick: move |_| manual_save(),
                    "Save"
                }
                button {
                    class: "compose-action primary",
                    r#type: "button",
                    disabled: *send_in_flight.read(),
                    onclick: move |_| send_now(),
                    if *send_in_flight.read() { "Sending…" } else { "Send" }
                }
            }
            if !*loaded.read() {
                p { class: "hint", "Loading draft…" }
            } else {
                div {
                    class: "compose-form",
                    div {
                        class: "field-row",
                        label { class: "label", r#for: "compose-from", "From" }
                        select {
                            id: "compose-from",
                            value: account_id.read().as_ref().map(|a| a.0.clone()).unwrap_or_default(),
                            oninput: {
                                let mut bump = bump;
                                move |evt: Event<FormData>| {
                                    let raw = evt.value();
                                    if raw.is_empty() {
                                        account_id.set(None);
                                    } else {
                                        account_id.set(Some(AccountId(raw)));
                                    }
                                    bump();
                                }
                            },
                            match &*accounts.read_unchecked() {
                                None => rsx! { option { value: "", "Loading…" } },
                                Some(Err(_)) => rsx! { option { value: "", "(error loading accounts)" } },
                                Some(Ok(list)) => rsx! {
                                    if list.is_empty() {
                                        option { value: "", "No accounts" }
                                    }
                                    for a in list.iter() {
                                        {
                                            let label = format!(
                                                "{} — {}",
                                                a.display_name, a.email_address
                                            );
                                            rsx! {
                                                option {
                                                    value: "{a.id.0}",
                                                    "{label}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    AddressField {
                        label: "To",
                        value: to_str,
                        on_change: bump,
                    }
                    AddressField {
                        label: "Cc",
                        value: cc_str,
                        on_change: bump,
                    }
                    AddressField {
                        label: "Bcc",
                        value: bcc_str,
                        on_change: bump,
                    }
                    div {
                        class: "field-row",
                        label { class: "label", r#for: "compose-subject", "Subject" }
                        input {
                            id: "compose-subject",
                            class: "compose-input",
                            r#type: "text",
                            value: "{subject}",
                            oninput: {
                                let mut bump = bump;
                                move |evt: Event<FormData>| {
                                    subject.set(evt.value());
                                    bump();
                                }
                            }
                        }
                    }
                    textarea {
                        class: "compose-body",
                        rows: "20",
                        placeholder: "Write your message…",
                        value: "{body}",
                        oninput: {
                            let mut bump = bump;
                            move |evt: Event<FormData>| {
                                body.set(evt.value());
                                bump();
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SaveStatus {
    Idle,
    Dirty,
    Saving,
    Saved,
    Error(String),
}

#[component]
fn ComposeStatusLabel(status: Signal<SaveStatus>) -> Element {
    rsx! {
        match &*status.read() {
            SaveStatus::Idle => rsx! { span { class: "compose-status muted", "" } },
            SaveStatus::Dirty => rsx! { span { class: "compose-status dirty", "Unsaved changes" } },
            SaveStatus::Saving => rsx! { span { class: "compose-status muted", "Saving…" } },
            SaveStatus::Saved => rsx! { span { class: "compose-status muted", "Saved" } },
            SaveStatus::Error(e) => rsx! { span { class: "compose-status error", "Save failed: {e}" } },
        }
    }
}

#[component]
fn AddressField(label: String, value: Signal<String>, on_change: EventHandler<()>) -> Element {
    let id = format!("compose-{}", label.to_ascii_lowercase());
    rsx! {
        div {
            class: "field-row",
            label { class: "label", r#for: "{id}", "{label}" }
            input {
                id: "{id}",
                class: "compose-input",
                r#type: "text",
                placeholder: "name@example.com, another@example.com",
                value: "{value.read()}",
                oninput: {
                    let mut value = value;
                    move |evt: Event<FormData>| {
                        value.set(evt.value());
                        on_change.call(());
                    }
                }
            }
        }
    }
}

/// Parse a comma- or semicolon-separated address line into typed
/// [`EmailAddress`] entries. Strips whitespace, drops empty
/// segments. The Phase 2 v1 path doesn't try to extract display
/// names — `Name <addr@host>` segments collapse to `addr@host`. A
/// proper RFC 5322 address parser arrives with the SMTP /
/// JMAP submission week (18 / 19).
fn parse_addrs(line: &str) -> Vec<EmailAddress> {
    line.split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|raw| {
            let address = if let (Some(open), Some(close)) = (raw.find('<'), raw.rfind('>')) {
                raw[open + 1..close].trim().to_string()
            } else {
                raw.to_string()
            };
            EmailAddress {
                address,
                display_name: None,
            }
        })
        .collect()
}

/// Render a Vec of typed addresses back into a comma-separated input
/// line for re-editing.
fn format_addrs(addrs: &[EmailAddress]) -> String {
    addrs
        .iter()
        .map(|a| match &a.display_name {
            Some(name) if !name.is_empty() => format!("{name} <{}>", a.address),
            _ => a.address.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------- Sidebar ----------

/// Flat list of every account's
/// well-known mailboxes followed by a Labels group of user-defined
/// folders. Click handlers update `selection` directly; the message
/// list and reader panes follow once their lift-ins land.
#[component]
fn SidebarV2(
    selection: Signal<Selection>,
    sync_tick: SyncTick,
    compose: Signal<Option<ComposeState>>,
) -> Element {
    let accounts = use_resource(|| async { invoke::<Vec<Account>>("accounts_list", ()).await });

    let open_compose = {
        let mut compose = compose;
        move |_| {
            // Default the From account: prefer whatever's currently
            // selected, otherwise fall back to the first configured
            // account so the form has a sensible value on open.
            let default_account = selection.read().account.clone().or_else(|| {
                accounts
                    .read_unchecked()
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|list| list.first().map(|a| a.id.clone()))
            });
            compose.set(Some(ComposeState {
                default_account,
                draft_id: None,
            }));
        }
    };

    rsx! {
        aside {
            class: "sidebar",
            button {
                class: "sidebar-compose-btn",
                r#type: "button",
                onclick: open_compose,
                span { class: "sidebar-compose-plus", "+" }
                span { "Compose" }
            }
            div {
                class: "sidebar-scroll",
                match &*accounts.read_unchecked() {
                    None => rsx! {},
                    Some(Err(e)) => rsx! { p { class: "sidebar-account-email", "Error: {e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p {
                            class: "sidebar-account-email",
                            style: "padding: 14px 16px;",
                            "No accounts configured."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for a in list.iter().cloned() {
                            SidebarAccountBlock { account: a, selection, sync_tick }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn SidebarAccountBlock(
    account: Account,
    selection: Signal<Selection>,
    sync_tick: SyncTick,
) -> Element {
    let id = account.id.clone();
    let tick_value = sync_tick();
    let folders = use_resource(use_reactive!(|id, tick_value| async move {
        let _ = tick_value;
        invoke::<Vec<Folder>>(
            "folders_list",
            serde_json::json!({ "input": { "account": id } }),
        )
        .await
    }));

    // Auto-select INBOX on first folder-list load, but only if the
    // user hasn't already picked something. Runs once per account
    // block, fires whenever the resource transitions to Ready and
    // the selection is still empty.
    {
        let mut selection = selection;
        let account_id = account.id.clone();
        use_effect(move || {
            if selection.read().folder.is_some() || selection.read().unified {
                return;
            }
            let read = folders.read_unchecked();
            let Some(Ok(list)) = read.as_ref() else {
                return;
            };
            let inbox = list
                .iter()
                .find(|f| matches!(f.role, Some(qsl_ipc::FolderRole::Inbox)));
            if let Some(inbox) = inbox {
                let mut sel = selection.write();
                sel.account = Some(account_id.clone());
                sel.folder = Some(inbox.id.clone());
                sel.message = None;
                sel.unified = false;
            }
        });
    }

    rsx! {
        div {
            class: "sidebar-account-header",
            span { class: "sidebar-account-label", "{account.display_name}" }
            span { class: "sidebar-account-email", "{account.email_address}" }
        }
        match &*folders.read_unchecked() {
            None => rsx! {},
            Some(Err(e)) => rsx! { p { class: "sidebar-account-email", "{e}" } },
            Some(Ok(list)) => {
                let (mailboxes, labels) = split_mailboxes_labels(list.clone());
                rsx! {
                    p { class: "sidebar-group-label", "Mailboxes" }
                    for f in mailboxes.into_iter() {
                        SidebarMailboxRow {
                            folder: f,
                            account_id: account.id.clone(),
                            selection,
                        }
                    }
                    if !labels.is_empty() {
                        p { class: "sidebar-group-label", "Labels" }
                        for f in labels.into_iter() {
                            SidebarLabelRow {
                                folder: f,
                                account_id: account.id.clone(),
                                selection,
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SidebarMailboxRow(
    folder: Folder,
    account_id: AccountId,
    selection: Signal<Selection>,
) -> Element {
    let is_selected = selection
        .read()
        .folder
        .as_ref()
        .is_some_and(|f| f.0 == folder.id.0);
    let unread = folder.unread_count;
    let role = folder.role.clone();
    rsx! {
        div {
            class: if is_selected { "sidebar-row selected" } else { "sidebar-row" },
            onclick: {
                let folder_id = folder.id.clone();
                let account_id = account_id.clone();
                move |_| {
                    let mut sel = selection.write();
                    sel.account = Some(account_id.clone());
                    sel.folder = Some(folder_id.clone());
                    sel.message = None;
                    sel.unified = false;
                }
            },
            div {
                class: "sidebar-row-left",
                MailboxRoleIcon { role: role.clone() }
                span {
                    class: "sidebar-row-name",
                    "{crate::format::display_name_for_folder(&folder.name)}"
                }
            }
            if unread > 0 {
                span {
                    class: if is_selected { "sidebar-unread-active" } else { "sidebar-unread-inactive" },
                    "{unread}"
                }
            }
        }
    }
}

#[component]
fn SidebarLabelRow(folder: Folder, account_id: AccountId, selection: Signal<Selection>) -> Element {
    let is_selected = selection
        .read()
        .folder
        .as_ref()
        .is_some_and(|f| f.0 == folder.id.0);
    let unread = folder.unread_count;
    let color = label_color(&folder.name);
    rsx! {
        div {
            class: if is_selected { "sidebar-row selected" } else { "sidebar-row" },
            onclick: {
                let folder_id = folder.id.clone();
                let account_id = account_id.clone();
                move |_| {
                    let mut sel = selection.write();
                    sel.account = Some(account_id.clone());
                    sel.folder = Some(folder_id.clone());
                    sel.message = None;
                    sel.unified = false;
                }
            },
            div {
                class: "sidebar-row-left",
                span {
                    class: "sidebar-label-dot",
                    style: "background: {color};",
                }
                span {
                    class: "sidebar-row-name",
                    "{crate::format::display_name_for_folder(&folder.name)}"
                }
            }
            if unread > 0 {
                span {
                    class: if is_selected { "sidebar-unread-active" } else { "sidebar-unread-inactive" },
                    "{unread}"
                }
            }
        }
    }
}

/// Inline SVG icon for a mailbox row. Drawn rather than imported so
/// the sidebar has zero asset dependencies and the strokes can match
/// the surrounding text color via `currentColor`.
#[component]
fn MailboxRoleIcon(role: Option<qsl_ipc::FolderRole>) -> Element {
    use qsl_ipc::FolderRole;
    // 16px box, 1.5 stroke, lucide-style geometry.
    match role {
        Some(FolderRole::Inbox) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                polyline { points: "22 12 16 12 14 15 10 15 8 12 2 12" }
                path { d: "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" }
            }
        },
        Some(FolderRole::Flagged) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                polygon { points: "12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" }
            }
        },
        Some(FolderRole::Sent) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                line { x1: "22", y1: "2", x2: "11", y2: "13" }
                polygon { points: "22 2 15 22 11 13 2 9 22 2" }
            }
        },
        Some(FolderRole::Drafts) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                path { d: "M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" }
                path { d: "M18.5 2.5a2.121 2.121 0 1 1 3 3L12 15l-4 1 1-4 9.5-9.5z" }
            }
        },
        Some(FolderRole::Spam) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                path { d: "M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" }
                line { x1: "12", y1: "9", x2: "12", y2: "13" }
                line { x1: "12", y1: "17", x2: "12.01", y2: "17" }
            }
        },
        Some(FolderRole::Trash) => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                polyline { points: "3 6 5 6 21 6" }
                path { d: "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" }
            }
        },
        // Archive, All, Important all collapse to the box/archive
        // glyph since they're conceptually "everything else"
        // mailboxes.
        _ => rsx! {
            svg {
                width: "16", height: "16", view_box: "0 0 24 24",
                fill: "none", stroke: "currentColor", stroke_width: "1.75",
                stroke_linecap: "round", stroke_linejoin: "round",
                polyline { points: "21 8 21 21 3 21 3 8" }
                rect { x: "1", y: "3", width: "22", height: "5" }
                line { x1: "10", y1: "12", x2: "14", y2: "12" }
            }
        },
    }
}

/// Split the backend's folder list into `(mailboxes, labels)`.
/// Mailboxes are well-known roles in priority order; labels are
/// everything else, alphabetized. `Important` falls into Labels per
/// the reference design — Gmail's per-label affordance reads more
/// like a tag than a destination folder.
fn split_mailboxes_labels(folders: Vec<Folder>) -> (Vec<Folder>, Vec<Folder>) {
    use qsl_ipc::FolderRole;
    fn band(role: &Option<FolderRole>) -> Option<u8> {
        match role {
            Some(FolderRole::Inbox) => Some(0),
            Some(FolderRole::Flagged) => Some(1),
            Some(FolderRole::Sent) => Some(2),
            Some(FolderRole::Drafts) => Some(3),
            Some(FolderRole::All) => Some(4),
            Some(FolderRole::Spam) => Some(5),
            Some(FolderRole::Trash) => Some(6),
            Some(FolderRole::Archive) => Some(7),
            _ => None,
        }
    }
    let mut mailboxes: Vec<Folder> = folders
        .iter()
        .filter(|f| band(&f.role).is_some())
        .cloned()
        .collect();
    mailboxes.sort_by_key(|f| band(&f.role).unwrap_or(u8::MAX));
    let mut labels: Vec<Folder> = folders
        .into_iter()
        .filter(|f| band(&f.role).is_none())
        .collect();
    labels.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (mailboxes, labels)
}

// ---------- Message list ----------

#[component]
fn MessageListPaneV2(
    selection: Signal<Selection>,
    sync_tick: SyncTick,
    folder_tokens: FolderTokens,
) -> Element {
    let unified = selection.read().unified;
    let folder_id = selection.read().folder.clone();
    rsx! {
        section {
            class: "msglist",
            if unified {
                UnifiedMessageListV2 { selection, sync_tick }
            } else {
                match folder_id {
                    None => rsx! {
                        p {
                            class: "msglist-empty",
                            "Select a mailbox to view messages."
                        }
                    },
                    Some(fid) => rsx! {
                        MessageListV2 { folder: fid, selection, folder_tokens }
                    },
                }
            }
        }
    }
}

#[component]
fn UnifiedMessageListV2(selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let tick_value = sync_tick();
    let page = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<MessagePage>(
            "messages_list_unified",
            serde_json::json!({
                "input": { "limit": 100, "offset": 0, "sort": SortOrder::DateDesc },
            }),
        )
        .await
    }));
    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { class: "msglist-empty", "Loading…" } },
            Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => rsx! {
                MessageListHeader {
                    title: "Unified Inbox".to_string(),
                    shown: messages.len() as u32,
                    total: *total_count,
                    unread: *unread_count,
                }
                div {
                    class: "msglist-scroll",
                    if messages.is_empty() {
                        p { class: "msglist-empty", "No messages." }
                    } else {
                        for m in messages.iter().cloned() {
                            MessageRowV2 { msg: m, selection }
                        }
                    }
                }
            },
        }
    }
}

#[component]
fn MessageListV2(
    folder: FolderId,
    selection: Signal<Selection>,
    folder_tokens: FolderTokens,
) -> Element {
    let mut visible_limit = use_signal(|| 200u32);
    let folder_for_fetch = folder.clone();
    // Read this folder's per-folder token. Reading the signal here
    // makes use_reactive! see a u64 dep that only changes when *this*
    // folder synced — refetches no longer fan out across all open
    // message lists when an unrelated folder pushes an event.
    let tick_value = folder_tokens.read().get(&folder).copied().unwrap_or(0u64);
    let limit_value = visible_limit();
    let page = use_resource(use_reactive!(
        |folder_for_fetch, tick_value, limit_value| async move {
            let _ = tick_value;
            invoke::<MessagePage>(
                "messages_list",
                serde_json::json!({
                    "input": {
                        "folder": folder_for_fetch,
                        "limit": limit_value,
                        "offset": 0,
                        "sort": SortOrder::DateDesc,
                    },
                }),
            )
            .await
        }
    ));

    // Lazy-fetch new IMAP messages whenever this folder is opened.
    // Fires once per `folder` value change. The Tauri command emits
    // `sync_event` when it finishes, which bumps this folder's entry
    // in `folder_tokens` and refetches the list above — so any newly-
    // arrived headers slide into the visible page automatically.
    {
        let folder_for_refresh = folder.clone();
        use_effect(use_reactive!(|folder_for_refresh| {
            let folder = folder_for_refresh.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = invoke::<()>(
                    "messages_refresh_folder",
                    serde_json::json!({ "input": { "folder": folder } }),
                )
                .await
                {
                    web_sys_log(&format!("messages_refresh_folder: {e}"));
                }
            });
        }));
    }

    // Auto-select the newest message on folder change (or initial
    // load) when nothing is selected yet. The first item is always
    // newest because the list is fetched with `SortOrder::DateDesc`.
    {
        let mut selection = selection;
        use_effect(move || {
            if selection.read().message.is_some() {
                return;
            }
            let read = page.read_unchecked();
            let Some(Ok(MessagePage { messages, .. })) = read.as_ref() else {
                return;
            };
            if let Some(first) = messages.first() {
                selection.write().message = Some(first.id.clone());
            }
        });
    }

    let mut loading_older = use_signal(|| false);
    let mut tail_exhausted = use_signal(|| false);

    // Pixel distance from the bottom that triggers the next batch.
    // 200px ≈ 3 message rows on the default theme — enough lead time
    // for the IPC + Tauri round-trip to complete before the user
    // hits true bottom on a fast scroll.
    const LOAD_THRESHOLD_PX: f64 = 200.0;

    // Infinite scroll. Each scroll event near the bottom of the
    // list dispatches one `messages_load_older` of 50, gated by
    // `loading_older` so a fast scroll doesn't queue several. After
    // a load lands, `visible_limit` grows and the new rows extend
    // the scrollable area downward — the user has to keep scrolling
    // for the next batch, which is the natural debounce.
    let onscroll_msglist = {
        let folder = folder.clone();
        let mut folder_tokens = folder_tokens;
        move |e: Event<ScrollData>| {
            if *loading_older.read() || *tail_exhausted.read() {
                return;
            }
            let scroll_top = e.data().scroll_top();
            let client_h = f64::from(e.data().client_height());
            let scroll_h = f64::from(e.data().scroll_height());
            if scroll_h - scroll_top - client_h >= LOAD_THRESHOLD_PX {
                return;
            }
            let folder = folder.clone();
            let folder_for_bump = folder.clone();
            loading_older.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let result = invoke::<u32>(
                    "messages_load_older",
                    serde_json::json!({
                        "input": { "folder": folder, "limit": 50 },
                    }),
                )
                .await;
                match result {
                    Ok(0) => {
                        tail_exhausted.set(true);
                        visible_limit.with_mut(|n| *n = n.saturating_add(50));
                    }
                    Ok(n) => {
                        visible_limit.with_mut(|m| *m = m.saturating_add(n.max(50)));
                        folder_tokens.with_mut(|m| {
                            let entry = m.entry(folder_for_bump).or_insert(0);
                            *entry = entry.wrapping_add(1);
                        });
                    }
                    Err(e) => {
                        web_sys_log(&format!("messages_load_older: {e}"));
                    }
                }
                loading_older.set(false);
            });
        }
    };

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { class: "msglist-empty", "Loading…" } },
            Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => rsx! {
                MessageListHeader {
                    title: folder_title_from_selection(&folder, &selection),
                    shown: messages.len() as u32,
                    total: *total_count,
                    unread: *unread_count,
                }
                div {
                    class: "msglist-scroll",
                    onscroll: onscroll_msglist,
                    if messages.is_empty() {
                        p { class: "msglist-empty", "No messages in this mailbox." }
                    } else {
                        for m in messages.iter().cloned() {
                            MessageRowV2 { msg: m, selection }
                        }
                    }
                }
                div {
                    class: "msglist-footer",
                    if *tail_exhausted.read() {
                        span { class: "msglist-tail-hint", "All older messages loaded." }
                    } else if *loading_older.read() {
                        span { class: "msglist-tail-hint", "Loading more…" }
                    }
                }
            },
        }
    }
}

#[component]
fn MessageListHeader(title: String, shown: u32, total: u32, unread: u32) -> Element {
    rsx! {
        div {
            class: "msglist-header",
            span { class: "msglist-header-title", "{title}" }
            span {
                class: "msglist-header-count",
                "{shown} of {total} · {unread} unread"
            }
        }
    }
}

#[component]
fn MessageRowV2(msg: MessageHeaders, selection: Signal<Selection>) -> Element {
    let is_selected = selection
        .read()
        .message
        .as_ref()
        .is_some_and(|m| m.0 == msg.id.0);
    let unread = !msg.flags.seen;
    let from_addr = msg.from.first();
    let from_name = from_addr
        .map(|a| {
            a.display_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| a.address.clone())
        })
        .unwrap_or_default();
    let avatar_initials = address_initials(
        &from_name,
        from_addr.map(|a| a.address.as_str()).unwrap_or(""),
    );
    let date = crate::format::format_relative_date(msg.date, chrono::Utc::now());
    let subject = if msg.subject.is_empty() {
        "(no subject)".to_string()
    } else {
        msg.subject.clone()
    };
    let snippet = msg.snippet.clone();
    let row_class = match (is_selected, unread) {
        (true, true) => "msg-row selected unread",
        (true, false) => "msg-row selected",
        (false, true) => "msg-row unread",
        (false, false) => "msg-row",
    };

    let id_for_popup = msg.id.clone();
    let ondoubleclick = move |evt: Event<MouseData>| {
        evt.stop_propagation();
        let id = id_for_popup.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = invoke::<()>(
                "messages_open_in_window",
                serde_json::json!({ "input": { "id": id } }),
            )
            .await
            {
                web_sys_log(&format!("messages_open_in_window: {e}"));
            }
        });
    };

    rsx! {
        div {
            class: row_class,
            onclick: {
                let mid = msg.id.clone();
                move |_| selection.write().message = Some(mid.clone())
            },
            ondoubleclick: ondoubleclick,
            div { class: "msg-row-avatar", "{avatar_initials}" }
            div {
                class: "msg-row-line1",
                span { class: "msg-row-from", "{from_name}" }
                if unread {
                    span { class: "msg-row-unread-dot" }
                }
                span { class: "msg-row-time", "{date}" }
            }
            div { class: "msg-row-subject", "{subject}" }
            if !snippet.is_empty() {
                div { class: "msg-row-snippet", "{snippet}" }
            }
        }
    }
}

/// Best-effort title for the message-list header. The selection only
/// holds the folder id; the sidebar already has the name in scope but
/// we'd need to either lift it up or refetch. For now, render the
/// last segment of the folder id (which is the canonical name on
/// IMAP) and run it through the shared display-name mapper so
/// "INBOX" shows as "Inbox" — matching the sidebar.
fn folder_title_from_selection(folder: &FolderId, _selection: &Signal<Selection>) -> String {
    let raw = &folder.0;
    let leaf = raw.rsplit_once(':').map(|(_, name)| name).unwrap_or(raw);
    crate::format::display_name_for_folder(leaf).to_string()
}

// ---------- Reader pane ----------

/// What variant of compose to open when a reader-action button is
/// clicked. Drives the pre-fill logic in [`open_reply`].
#[derive(Copy, Clone)]
enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

/// Build a `DraftInput` from the opened message + reply variant,
/// persist it via `drafts_save`, then point the compose pane at it.
/// Runs entirely off-main as a spawned future so the click handler
/// returns immediately. Errors are logged to the console; we don't
/// surface a UI banner since the only failure mode is a broken IPC
/// channel (which is recoverable on its own).
fn open_reply(
    rendered: RenderedMessage,
    mut compose: Signal<Option<ComposeState>>,
    kind: ReplyKind,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let account_id = rendered.headers.account_id.clone();

        // Fetch accounts so reply-all can drop the user's own
        // address from the cc list. A miss (account vanished, list
        // failed) just leaves the user in the cc — they can edit.
        let self_email = if matches!(kind, ReplyKind::ReplyAll) {
            invoke::<Vec<Account>>("accounts_list", ())
                .await
                .ok()
                .and_then(|accts| {
                    accts
                        .into_iter()
                        .find(|a| a.id == account_id)
                        .map(|a| a.email_address)
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        let (to, cc, subject, in_reply_to, references, body) = match kind {
            ReplyKind::Reply => {
                let to = crate::reply::reply_to_recipients(&rendered.headers);
                let subject = crate::reply::reply_subject(&rendered.headers.subject);
                let body = crate::reply::quote_body_for_reply(&rendered, None);
                (
                    to,
                    Vec::<EmailAddress>::new(),
                    subject,
                    rendered.headers.rfc822_message_id.clone(),
                    crate::reply::reply_references(&rendered.headers),
                    body,
                )
            }
            ReplyKind::ReplyAll => {
                let to = crate::reply::reply_to_recipients(&rendered.headers);
                let cc = crate::reply::reply_all_cc(&rendered.headers, &to, &self_email);
                let subject = crate::reply::reply_subject(&rendered.headers.subject);
                let body = crate::reply::quote_body_for_reply(&rendered, None);
                (
                    to,
                    cc,
                    subject,
                    rendered.headers.rfc822_message_id.clone(),
                    crate::reply::reply_references(&rendered.headers),
                    body,
                )
            }
            ReplyKind::Forward => {
                let subject = crate::reply::forward_subject(&rendered.headers.subject);
                let body = crate::reply::forward_body(&rendered);
                (
                    Vec::<EmailAddress>::new(),
                    Vec::<EmailAddress>::new(),
                    subject,
                    None,
                    Vec::<String>::new(),
                    body,
                )
            }
        };

        let payload = serde_json::json!({
            "input": {
                "draft": {
                    "id": Option::<DraftId>::None,
                    "account_id": account_id,
                    "to": to,
                    "cc": cc,
                    "bcc": Vec::<EmailAddress>::new(),
                    "subject": subject,
                    "body": body,
                    "body_kind": DraftBodyKind::Plain,
                    "attachments": Vec::<()>::new(),
                    "in_reply_to": in_reply_to,
                    "references": references,
                }
            }
        });
        match invoke::<DraftId>("drafts_save", payload).await {
            Ok(draft_id) => {
                compose.set(Some(ComposeState {
                    default_account: Some(account_id),
                    draft_id: Some(draft_id),
                }));
            }
            Err(e) => {
                web_sys_log(&format!("open_reply: drafts_save: {e}"));
            }
        }
    });
}

#[component]
fn ReaderPaneV2(
    selection: Signal<Selection>,
    sync_tick: SyncTick,
    compose: Signal<Option<ComposeState>>,
) -> Element {
    let message_id = selection.read().message.clone();

    // Hide Servo's overlay surface whenever no message is selected.
    // The Dioxus placeholder text shows through the gap. Without
    // this the previous render would freeze in place under the
    // "Select a message" copy.
    {
        let has_message = message_id.is_some();
        use_effect(use_reactive!(|has_message| {
            if !has_message {
                wasm_bindgen_futures::spawn_local(async {
                    let _ = invoke::<()>("reader_clear", serde_json::json!({})).await;
                });
            }
        }));
    }

    rsx! {
        section {
            class: "reader-pane",
            match message_id {
                None => rsx! { div { class: "reader-empty", "Select a message to read." } },
                Some(id) => rsx! { ReaderV2 { id, sync_tick, compose } },
            }
        }
    }
}

#[component]
fn ReaderV2(id: MessageId, sync_tick: SyncTick, compose: Signal<Option<ComposeState>>) -> Element {
    // `force_trusted` is the one-shot "Load images" override. Resets
    // to false when the user navigates to a different message — see
    // the use_effect below — so a previously-loaded message doesn't
    // leak its trust state into the next one.
    let mut force_trusted: Signal<bool> = use_signal(|| false);
    {
        let id_for_reset = id.clone();
        use_effect(use_reactive!(|id_for_reset| {
            let _ = id_for_reset;
            force_trusted.set(false);
        }));
    }

    let id_for_fetch = id.clone();
    let force_trusted_val = *force_trusted.read();
    let msg = use_resource(use_reactive!(
        |id_for_fetch, force_trusted_val| async move {
            let rendered = invoke::<RenderedMessage>(
                "messages_get",
                serde_json::json!({
                    "input": {
                        "id": id_for_fetch,
                        "force_trusted": force_trusted_val,
                    }
                }),
            )
            .await?;
            // Hand the body straight to Servo's overlay surface. Doing
            // it inside the resource closure means it fires once per
            // successful message fetch — putting it in a `use_effect`
            // outside the closure would only fire on initial mount and
            // miss subsequent message switches.
            let html = compose_reader_html(&rendered);
            let _ = invoke::<()>(
                "reader_render",
                serde_json::json!({ "input": { "html": html } }),
            )
            .await;
            // Force one tracker push so the overlay surface lands in
            // the right slot on the same frame the new content paints.
            push_reader_body_rect();
            Ok::<_, String>(rendered)
        }
    ));

    // Mark-as-read on selection. Fires once per `id` change. The
    // command also queues an outbox entry for the server flag write,
    // so the `\Seen` flag eventually propagates over IMAP. Bump
    // sync_tick so the message-list row updates its visual state
    // (bold → normal) immediately rather than waiting for the next
    // sync cycle.
    {
        let id_for_mark = id.clone();
        let mut sync_tick = sync_tick;
        use_effect(use_reactive!(|id_for_mark| {
            let id = id_for_mark.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = invoke::<()>(
                    "messages_mark_read",
                    serde_json::json!({ "input": { "ids": [id], "seen": true } }),
                )
                .await
                {
                    web_sys_log(&format!("messages_mark_read: {e}"));
                }
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }));
    }

    rsx! {
        match &*msg.read_unchecked() {
            None => rsx! { div { class: "reader-scroll", p { class: "reader-body-loading", "Loading…" } } },
            Some(Err(e)) => rsx! { div { class: "reader-scroll", p { class: "reader-body-loading", "{e}" } } },
            Some(Ok(rendered)) => {
                let primary = rendered.headers.from.first();
                let from_name = primary
                    .map(|a| {
                        a.display_name
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| a.address.clone())
                    })
                    .unwrap_or_default();
                let from_addr = primary.map(|a| a.address.clone()).unwrap_or_default();
                let initials = address_initials(&from_name, &from_addr);
                let date = crate::format::format_relative_date(
                    rendered.headers.date,
                    chrono::Utc::now(),
                );
                let date_full = rendered.headers.date.to_rfc2822();
                let body_doc = compose_reader_html(rendered);
                let subject = if rendered.headers.subject.is_empty() {
                    "(no subject)".to_string()
                } else {
                    rendered.headers.subject.clone()
                };

                // Push the body HTML to the Servo overlay surface.
                // `reader_render` is a no-op when Servo isn't
                // installed (slot is None), so this is safe across
                // platforms/builds. After the render, force one
                // tracker push so the surface lands in the right
                // spot on the same frame the new content paints.
                let render_payload = body_doc.clone();
                use_effect(move || {
                    let payload = render_payload.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let _ = invoke::<()>(
                            "reader_render",
                            serde_json::json!({ "input": { "html": payload } }),
                        )
                        .await;
                    });
                    push_reader_body_rect();
                });

                rsx! {
                    div {
                        class: "reader-header-block",
                        h1 { class: "reader-subject", "{subject}" }
                        div {
                            class: "reader-sender-card",
                            div { class: "reader-sender-avatar", "{initials}" }
                            div {
                                class: "reader-sender-meta",
                                span { class: "reader-sender-name", "{from_name}" }
                                span { class: "reader-sender-addr", "{from_addr}" }
                            }
                            span {
                                class: "reader-sender-date",
                                title: "{date_full}",
                                "{date}"
                            }
                            div {
                                class: "reader-actions",
                                button {
                                    class: "reader-action",
                                    r#type: "button",
                                    title: "Reply",
                                    onclick: {
                                        let r = rendered.clone();
                                        move |_| open_reply(r.clone(), compose, ReplyKind::Reply)
                                    },
                                    "Reply"
                                }
                                button {
                                    class: "reader-action",
                                    r#type: "button",
                                    title: "Reply to all",
                                    onclick: {
                                        let r = rendered.clone();
                                        move |_| open_reply(r.clone(), compose, ReplyKind::ReplyAll)
                                    },
                                    "Reply All"
                                }
                                button {
                                    class: "reader-action",
                                    r#type: "button",
                                    title: "Forward",
                                    onclick: {
                                        let r = rendered.clone();
                                        move |_| open_reply(r.clone(), compose, ReplyKind::Forward)
                                    },
                                    "Forward"
                                }
                            }
                        }
                        if !rendered.headers.to.is_empty() {
                            ReaderRecipientRow {
                                label: "To".to_string(),
                                addrs: rendered.headers.to.clone(),
                            }
                        }
                        if !rendered.headers.cc.is_empty() {
                            ReaderRecipientRow {
                                label: "Cc".to_string(),
                                addrs: rendered.headers.cc.clone(),
                            }
                        }
                        if !rendered.attachments.is_empty() {
                            ReaderAttachments { attachments: rendered.attachments.clone() }
                        }
                        if rendered.remote_content_blocked {
                            RemoteContentBanner {
                                account_id: rendered.headers.account_id.clone(),
                                sender_addr: rendered
                                    .headers
                                    .from
                                    .first()
                                    .map(|a| a.address.clone())
                                    .unwrap_or_default(),
                                force_trusted,
                            }
                        }
                    }
                    // Body slot — transparent placeholder. The
                    // ResizeObserver wired in App() watches this
                    // element's bounding rect and pushes it to
                    // Rust over `reader_set_position`, which moves
                    // Servo's `gtk::DrawingArea` to overlap exactly
                    // here. Visible content is painted by Servo,
                    // not by Dioxus, so this div is intentionally
                    // empty.
                    div { class: "reader-body-fill" }
                }
            }
        }
    }
}

#[component]
fn ReaderRecipientRow(label: String, addrs: Vec<EmailAddress>) -> Element {
    rsx! {
        div {
            class: "reader-recipients",
            span { class: "reader-recipients-label", "{label}:" }
            for a in addrs.iter().cloned() {
                {
                    let name = a.display_name.clone().unwrap_or_default();
                    let addr = a.address.clone();
                    let title = if name.is_empty() {
                        addr.clone()
                    } else {
                        format!("{name} <{addr}>")
                    };
                    let display = if name.is_empty() { addr } else { name };
                    rsx! {
                        span { class: "reader-chip", title: "{title}", "{display}" }
                    }
                }
            }
        }
    }
}

/// Banner that appears above the reader body when the sanitizer
/// blocked remote content for this message. "Load images" flips
/// `force_trusted` for the current render only; "Always load from
/// this sender" persists an `remote_content_opt_ins` row via
/// `messages_trust_sender`, then re-fetches so subsequent loads
/// pick up the trust state.
#[component]
fn RemoteContentBanner(
    account_id: AccountId,
    sender_addr: String,
    force_trusted: Signal<bool>,
) -> Element {
    let mut trusting: Signal<bool> = use_signal(|| false);
    let sender_label = if sender_addr.is_empty() {
        "this sender".to_string()
    } else {
        sender_addr.clone()
    };

    let load_once = {
        let mut force_trusted = force_trusted;
        move |_| force_trusted.set(true)
    };

    let trust_sender = {
        let account_id = account_id.clone();
        let sender_addr = sender_addr.clone();
        let mut force_trusted = force_trusted;
        move |_| {
            if sender_addr.is_empty() || *trusting.read() {
                return;
            }
            trusting.set(true);
            let account_id = account_id.clone();
            let sender_addr = sender_addr.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match invoke::<()>(
                    "messages_trust_sender",
                    serde_json::json!({
                        "input": {
                            "account_id": account_id,
                            "email_address": sender_addr,
                        }
                    }),
                )
                .await
                {
                    Ok(()) => {
                        // Trigger a re-render through the same signal
                        // the per-render override uses; the resource
                        // closure re-fires and the next `messages_get`
                        // sees the new opt-in row.
                        force_trusted.set(true);
                    }
                    Err(e) => {
                        web_sys_log(&format!("messages_trust_sender: {e}"));
                    }
                }
                trusting.set(false);
            });
        }
    };

    rsx! {
        div {
            class: "reader-remote-banner",
            span {
                class: "reader-remote-banner-text",
                "Images blocked for privacy."
            }
            div {
                class: "reader-remote-banner-actions",
                button {
                    class: "reader-remote-banner-button",
                    r#type: "button",
                    onclick: load_once,
                    "Load images"
                }
                button {
                    class: "reader-remote-banner-button",
                    r#type: "button",
                    disabled: sender_addr.is_empty() || *trusting.read(),
                    onclick: trust_sender,
                    title: "{sender_label}",
                    if *trusting.read() {
                        "Saving…"
                    } else {
                        "Always load from this sender"
                    }
                }
            }
        }
    }
}

#[component]
fn ReaderAttachments(attachments: Vec<Attachment>) -> Element {
    rsx! {
        div {
            class: "reader-attachments",
            for a in attachments.iter() {
                {
                    let name = if a.filename.is_empty() {
                        "(untitled)".to_string()
                    } else {
                        a.filename.clone()
                    };
                    let size = format_bytes(a.size);
                    rsx! {
                        span {
                            class: "reader-attachment",
                            title: "{name} · {size}",
                            "📎 {name} · {size}"
                        }
                    }
                }
            }
        }
    }
}

/// Deterministic palette pick for a label dot. Hash the folder name
/// down to one of six accent colors so the same label keeps the same
/// dot across renders without persisting a color choice anywhere.
fn label_color(name: &str) -> &'static str {
    const PALETTE: &[&str] = &[
        "#D85A30", "#378ADD", "#1D9E75", "#A858C8", "#D8A030", "#30A0C8",
    ];
    let mut hash: u32 = 5381;
    for byte in name.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
    }
    PALETTE[(hash as usize) % PALETTE.len()]
}
