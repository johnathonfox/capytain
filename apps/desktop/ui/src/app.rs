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

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use qsl_ipc::{
    Account, AccountId, Attachment, Contact, Draft, DraftAttachment, DraftBodyKind, DraftId,
    EmailAddress, Folder, FolderId, MessageHeaders, MessageId, MessagePage, RenderedMessage,
    SortOrder, SyncEvent,
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
        // rAF-coalesce: ResizeObserver + window.resize fire faster than
        // we can render. Without this, a continuous splitter drag
        // floods Tauri with `reader_set_position` calls (60+ Hz), each
        // calling Servo `resize` and producing visible flicker. With
        // it, multiple events inside the same frame collapse to a
        // single push from rAF — the Rust side then dedups by (w, h)
        // and skips Servo resize when only position changed.
        let rafScheduled = false;
        const pushRaw = function() {
            const el = document.querySelector('.reader-body-fill');
            if (!el) return;
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
            if (w <= 0 || h <= 0) return;
            window.__TAURI_INTERNALS__
                .invoke('reader_set_position', {
                    input: { x: x, y: y, width: w, height: h },
                })
                .catch(function(e) { console.warn('reader_set_position:', e); });
        };
        const push = function() {
            if (rafScheduled) return;
            rafScheduled = true;
            requestAnimationFrame(function() {
                rafScheduled = false;
                pushRaw();
            });
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

    // Set the `data-theme` attribute on `<html>` so the CSS rules in
    // `tailwind.css` (the `:root[data-theme="light"]` block + the
    // `prefers-color-scheme` block for `data-theme="system"`) flip
    // tokens. Pass one of "system", "dark", "light"; anything else
    // is treated as "system" so a corrupt setting doesn't lock the
    // user out of the theme they expect.
    export function setRootTheme(theme) {
        const root = document.documentElement;
        if (!root) return;
        const t = (theme === "dark" || theme === "light") ? theme : "system";
        root.setAttribute("data-theme", t);
    }

    // Set the `data-density` attribute on `<html>` so the
    // `:root[data-density="compact"]` block in tailwind.css tightens
    // the row tokens. Default is "comfortable".
    export function setRootDensity(density) {
        const root = document.documentElement;
        if (!root) return;
        const d = density === "compact" ? "compact" : "comfortable";
        root.setAttribute("data-density", d);
    }

    // Returns the secondary-window route this Dioxus instance should
    // mount: `'settings'`, `'oauth-add'`, etc. `null` for the main
    // three-pane window (the global isn't set there). Set by the
    // host's `initialization_script` before wasm boots so the root
    // component can branch synchronously without a flash.
    export function readerWindowView() {
        return window.__QSL_VIEW__ || null;
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

    #[wasm_bindgen(js_name = setRootTheme)]
    pub(crate) fn set_root_theme(theme: &str);

    #[wasm_bindgen(js_name = setRootDensity)]
    pub(crate) fn set_root_density(density: &str);

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

    /// Returns the value of `window.__QSL_VIEW__` (`"settings"`,
    /// `"oauth-add"`, etc.), set by `WebviewWindowBuilder::
    /// initialization_script` in the corresponding open-window
    /// command. `JsValue::null` for the main three-pane window.
    #[wasm_bindgen(js_name = readerWindowView)]
    pub(crate) fn reader_window_view() -> JsValue;
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

/// Open context-menu state. `Some` while the right-click popover is
/// shown. The pixel coords are viewport-relative; the popover renders
/// itself with `position: fixed` at those coordinates. `is_unread`
/// drives the Mark read / Mark unread label without re-fetching the
/// flag.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageContextMenu {
    pub x: f64,
    pub y: f64,
    pub message_id: MessageId,
    pub is_unread: bool,
}

// ---------- Root ----------

#[component]
pub fn App() -> Element {
    // Secondary-window detection: the Tauri popup `initialization_script`
    // injects either `window.__QSL_READER_ID__` (popup reader) or
    // `window.__QSL_VIEW__` ('settings', 'oauth-add', …) before the
    // wasm bundle boots. Branch synchronously so the wrong shell
    // never paints.
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
    let view_js: JsValue = reader_window_view();
    if let Some(view) = view_js.as_string() {
        match view.as_str() {
            "settings" => return rsx! { crate::settings::SettingsApp {} },
            "oauth-add" => return rsx! { crate::oauth_add::OAuthAddApp {} },
            other => {
                web_sys_log(&format!(
                    "App: unknown __QSL_VIEW__ = {other}; falling through"
                ));
            }
        }
    }
    full_app_shell()
}

fn full_app_shell() -> Element {
    let selection = use_signal(Selection::default);
    let mut sync_tick: SyncTick = use_signal(|| 0u64);
    let mut folder_tokens: FolderTokens = use_signal(HashMap::new);
    let compose: Signal<Option<ComposeState>> = use_signal(|| None);
    let help_visible: Signal<bool> = use_signal(|| false);
    // Multi-select state. `bulk_selected` carries the currently-checked
    // message ids; the bulk-action bar reveals when it's non-empty and
    // the per-row checkboxes paint a filled state when their id is in
    // the set. Lives at App root so it survives folder switches and
    // can be read from any pane that wants to act on the selection.
    let bulk_selected: Signal<HashSet<MessageId>> = use_signal(HashSet::new);
    // Active search query — empty string means "no search active, show
    // the regular folder / unified inbox view". Sits at the App root
    // so it survives sidebar navigation; clearing it (via Esc on the
    // input or the bar's clear button) returns to the previous view
    // without touching `selection`.
    let search_query: Signal<String> = use_signal(String::new);

    // Account filter for the unified-inbox / folder views. `None`
    // shows every account; `Some(id)` scopes the message list to a
    // single account. Per post-phase-2.md assumption A1 the chip is
    // a filter, not an active-account swap — the sidebar still shows
    // every account's mailboxes side by side.
    let account_filter: Signal<Option<AccountId>> = use_signal(|| None);

    // Currently-visible message ids in display order. Each list
    // component (folder view, unified view, search results, threads)
    // publishes its rendered set so `j` / `k` can step through them
    // without re-fetching. Sorted by date-desc to match the rendering.
    let visible_messages: Signal<Vec<MessageId>> = use_signal(Vec::new);

    // Command palette visibility (⌘K) and recent-search ring buffer for
    // the palette's "Recent" section. Both live at App root so the
    // palette can pull across pane boundaries on open.
    let palette_visible: Signal<bool> = use_signal(|| false);
    let mut recent_searches: Signal<Vec<String>> = use_signal(Vec::new);

    // Undo-send deadline (epoch ms). `Some(t)` means a Send click is
    // armed but holding for the configured undo window; `None` means
    // no pending send. Lives at App root so the global Esc handler
    // (which doesn't see ComposePane's locals) can clear it on Cancel
    // — unwinding pending sends takes priority over closing compose.
    let undo_send_pending: Signal<Option<f64>> = use_signal(|| None);

    // Right-click context menu state. `Some` while the popover is
    // visible. Lives at App root so a single popover element renders
    // over the whole shell and dismiss-on-outside-click stays simple
    // (no per-row click-outside listeners). Both `MessageRowV2` and
    // `ThreadRow` write to it on right-click.
    let context_menu: Signal<Option<MessageContextMenu>> = use_signal(|| None);
    // Expose the context-menu signal to nested rows without threading
    // it through every intermediate list component (SearchList,
    // UnifiedInbox, MessageListV2, ThreadRow). The rows reach for it
    // via `use_context::<Signal<Option<MessageContextMenu>>>()`.
    use_context_provider(|| context_menu);

    // Capture cleared search queries into the recent ring buffer.
    // Watching the transition non-empty → empty avoids polluting the
    // ring with every keystroke while still recording every query the
    // user actually saw results for. Bounded to 8 entries; dedup keeps
    // a re-typed query from filling the buffer.
    {
        let mut last_query: Signal<String> = use_signal(String::new);
        let q_now = search_query.read().clone();
        use_effect(use_reactive!(|q_now| {
            // `peek()` reads without subscribing — `use_effect` re-runs
            // on any signal it has *read* (subscribed to), so doing
            // `last_query.read()` here while also `last_query.set(...)`
            // below creates a self-trigger loop that crashes the wasm
            // bundle (white screen on boot). The reactive trigger we
            // *do* want is `q_now` from the surrounding `use_reactive!`.
            let prev = last_query.peek().clone();
            if !prev.is_empty() && q_now.is_empty() {
                let trimmed = prev.trim().to_string();
                if !trimmed.is_empty() {
                    recent_searches.with_mut(|v| {
                        v.retain(|s| s != &trimmed);
                        v.insert(0, trimmed);
                        v.truncate(8);
                    });
                }
            }
            last_query.set(q_now);
        }));
    }

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
    // Same problem for the command palette: it's a CSS-level overlay
    // rendered inside the Tauri webview, but Servo's GTK widget paints
    // *above* the webview surface, so without intervention the
    // rendered email body shows through (and over) the palette wherever
    // the two intersect. Clear the overlay on open; force the JS-side
    // tracker to re-push the bounding rect on close so the overlay
    // reappears in its previous slot. Closing the palette doesn't
    // reflow `.reader-body-fill`, so the natural ResizeObserver fire
    // never happens — we have to nudge it.
    {
        let palette_open = *palette_visible.read();
        use_effect(use_reactive!(|palette_open| {
            if palette_open {
                wasm_bindgen_futures::spawn_local(async {
                    let _ = invoke::<()>("reader_clear", serde_json::json!({})).await;
                });
            } else {
                push_reader_body_rect();
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

    // Theme + density: read both from `app_settings` at boot and
    // apply to `<html>` via `data-theme` / `data-density` so the CSS
    // tokens flip. Subscribe to `app_settings_changed` so a flip in
    // the Settings window applies live across all open windows.
    // Default to "system" theme (follows `prefers-color-scheme`) and
    // "comfortable" density when the keys haven't been written.
    use_hook(|| {
        wasm_bindgen_futures::spawn_local(async {
            // `app_settings_get` returns Option<String> — `None` when
            // the user hasn't picked a value yet; default in that case.
            let theme = invoke::<Option<String>>(
                "app_settings_get",
                serde_json::json!({ "input": { "key": "appearance.theme" } }),
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "system".to_string());
            set_root_theme(&theme);

            let density = invoke::<Option<String>>(
                "app_settings_get",
                serde_json::json!({ "input": { "key": "appearance.density" } }),
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "comfortable".to_string());
            set_root_density(&density);
        });
    });
    // Listen for live changes from the Settings window. The Tauri
    // `app_settings_set` command emits `app_settings_changed` after
    // every successful write — payload is `{ key, value }`. We only
    // act on the two keys we care about; anything else falls through.
    use_hook(|| {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |payload: JsValue| {
            #[derive(serde::Deserialize)]
            struct Changed {
                key: String,
                value: String,
            }
            let Ok(evt) = serde_wasm_bindgen::from_value::<Changed>(payload) else {
                return;
            };
            match evt.key.as_str() {
                "appearance.theme" => set_root_theme(&evt.value),
                "appearance.density" => set_root_density(&evt.value),
                _ => {}
            }
        });
        wasm_bindgen_futures::spawn_local(async move {
            let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
            if let Err(e) = tauri_listen("app_settings_changed", func).await {
                web_sys_log(&format!("app_settings_changed listen failed: {e:?}"));
            }
            Box::leak(Box::new(cb));
        });
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

    // Install the document-level keydown listener once per session.
    // Dioxus's element-level `onkeydown` only fires when that element
    // (or a child) has focus, which doesn't cover the "no input
    // focused, just press `c`" case the Gmail-style scheme assumes.
    // A document listener catches the keystroke regardless of focus
    // and `is_typing()` swallows it again for `<input>` / `<textarea>`
    // / `[contenteditable]`. The closure leaks via `Box::leak` because
    // the listener lives for the app's lifetime.
    use_hook(move || {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |event_js: JsValue| {
            let Ok(event) = event_js.dyn_into::<web_sys::KeyboardEvent>() else {
                return;
            };
            let key = event.key();
            let ctrl_or_meta = event.ctrl_key() || event.meta_key();
            // Apply the typing-guard ONLY to unmodified keystrokes —
            // a bare `j` / `k` / `r` while focused in an input is the
            // user typing those letters, not a command. Modifier-
            // prefixed shortcuts like Ctrl+K are intentional commands
            // in every context (the keystroke produces no text input)
            // and must work even when focus is in the search bar or
            // compose body. Without this exemption Ctrl+K silently
            // dropped on every focused input.
            //
            // Escape during a pending undo-send is also exempted: it's
            // a modal-cancel keystroke, never typed text, and the user
            // expects "Esc cancels" to work even with focus in the
            // body textarea where they clicked Send from.
            let allow_through =
                ctrl_or_meta || (key == "Escape" && undo_send_pending.read().is_some());
            if !allow_through && is_typing_in_field() {
                return;
            }
            let Some(cmd) = crate::keyboard::parse(&key, ctrl_or_meta) else {
                return;
            };
            event.prevent_default();
            dispatch_keyboard_command(
                cmd,
                selection,
                compose,
                sync_tick,
                help_visible,
                search_query,
                visible_messages,
                palette_visible,
                undo_send_pending,
                context_menu,
            );
        });
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
                if let Err(e) = document.add_event_listener_with_callback("keydown", func) {
                    web_sys_log(&format!("keydown listener install failed: {e:?}"));
                }
            }
        }
        Box::leak(Box::new(cb));
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
            TopBar { account_filter, palette_visible }
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
                MessageListPaneV2 { selection, sync_tick, folder_tokens, bulk_selected, search_query, account_filter, visible_messages }
            }
            div {
                class: "shell-splitter",
                onmousedown: onmousedown_list,
            }
            div {
                class: "shell-pane shell-pane-reader",
                if compose.read().is_some() {
                    ComposePane { compose, sync_tick, undo_send_pending }
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
            if help_visible() {
                ShortcutsOverlay { visible: help_visible }
            }
            if palette_visible() {
                CommandPalette {
                    visible: palette_visible,
                    selection,
                    compose,
                    sync_tick,
                    search_query,
                    recent_searches,
                    help_visible,
                }
            }
            if context_menu.read().is_some() {
                MessageContextPopover { context_menu, compose, sync_tick, selection }
            }
        }
    }
}

/// Returns `true` when the document's currently-focused element is one
/// the user is typing into — `<input>`, `<textarea>`, `<select>`, or a
/// `[contenteditable]` element. Single-letter keyboard shortcuts must
/// not fire in those cases (otherwise `c` while editing the To: field
/// would open a new compose). Best-effort: when we can't reach the
/// document or the active element, default to `false` so the
/// shortcuts work — never `true` (the safe-against-misfire bias is
/// to *fire* the shortcut, since the user's intent is "act on this
/// app," and the shortcut is a no-op when no message is selected).
/// Best-effort detection of "this is a Mac" so the topbar pill and any
/// other shortcut hint can render `⌘K` on macOS and `Ctrl K` on
/// Linux / Windows. The shortcut binding is the same conceptually
/// (`event.metaKey || event.ctrlKey` is what `parse()` checks) but
/// users see different keycap glyphs on different keyboards.
///
/// Reads `navigator.platform` — deprecated by the spec but still
/// populated by every shipping browser engine, including Servo's
/// embedded webview. Falls back to `userAgent` substring sniff if
/// `platform` is absent. Falls back to "Ctrl K" if neither is
/// available — better to show the binding that actually works on the
/// platform we're most likely running on (Linux desktop) than to
/// confidently lie with `⌘K`.
fn palette_shortcut_label() -> &'static str {
    let Some(window) = web_sys::window() else {
        return "Ctrl K";
    };
    let nav = window.navigator();
    let platform = nav.platform().unwrap_or_default().to_ascii_lowercase();
    let ua = nav.user_agent().unwrap_or_default().to_ascii_lowercase();
    if platform.contains("mac") || ua.contains("mac os x") {
        "⌘K"
    } else {
        "Ctrl K"
    }
}

fn is_typing_in_field() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Some(document) = window.document() else {
        return false;
    };
    let Some(active) = document.active_element() else {
        return false;
    };
    let tag = active.tag_name();
    if tag.eq_ignore_ascii_case("INPUT")
        || tag.eq_ignore_ascii_case("TEXTAREA")
        || tag.eq_ignore_ascii_case("SELECT")
    {
        return true;
    }
    matches!(
        active.get_attribute("contenteditable").as_deref(),
        Some("true") | Some("")
    )
}

/// Turn a [`crate::keyboard::KeyboardCommand`] into actual side effects
/// against the App-root signals. Pulled out of the listener closure
/// for readability — the closure can stay tightly scoped to "parse +
/// guard," and this function owns the per-command dispatch.
#[allow(clippy::too_many_arguments)]
fn dispatch_keyboard_command(
    cmd: crate::keyboard::KeyboardCommand,
    mut selection: Signal<Selection>,
    mut compose: Signal<Option<ComposeState>>,
    mut sync_tick: SyncTick,
    mut help_visible: Signal<bool>,
    mut search_query: Signal<String>,
    visible_messages: Signal<Vec<MessageId>>,
    mut palette_visible: Signal<bool>,
    mut undo_send_pending: Signal<Option<f64>>,
    mut context_menu: Signal<Option<MessageContextMenu>>,
) {
    use crate::keyboard::KeyboardCommand;

    match cmd {
        KeyboardCommand::Compose => {
            // Empty compose. Default-account resolution happens inside
            // the compose pane when it can't read `selection.account`,
            // so we don't need to fetch the account list here.
            let default_account = selection.read().account.clone();
            compose.set(Some(ComposeState {
                default_account,
                draft_id: None,
            }));
        }
        KeyboardCommand::Cancel => {
            // Priority: pending undo-send → context menu → palette →
            // help → compose → search → selection. One press unwinds
            // one layer — matches Gmail / native dialog behaviour.
            // Pending undo-send sits above compose so Esc cancels the
            // in-flight send before it closes the pane (closing alone
            // wouldn't cancel — the pending future is decoupled).
            if undo_send_pending.read().is_some() {
                undo_send_pending.set(None);
            } else if context_menu.read().is_some() {
                context_menu.set(None);
            } else if *palette_visible.read() {
                palette_visible.set(false);
            } else if *help_visible.read() {
                help_visible.set(false);
            } else if compose.read().is_some() {
                compose.set(None);
            } else if !search_query.read().is_empty() {
                search_query.set(String::new());
            } else if selection.read().message.is_some() {
                selection.with_mut(|s| s.message = None);
            }
        }
        KeyboardCommand::TogglePalette => {
            let cur = *palette_visible.read();
            palette_visible.set(!cur);
        }
        KeyboardCommand::FocusSearch => {
            // Focus the search input by id — the bar lives at the
            // top of the message-list pane and renders unconditionally,
            // so the element is always in the DOM. `getElementById`
            // returns null only mid-mount; in that case `/` is a no-op.
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    if let Some(el) = document.get_element_by_id("qsl-search-input") {
                        if let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() {
                            let _ = input.focus();
                            input.select();
                        }
                    }
                }
            }
        }
        KeyboardCommand::ToggleHelp => {
            let cur = *help_visible.read();
            help_visible.set(!cur);
        }
        KeyboardCommand::Archive => {
            let Some(id) = selection.read().message.clone() else {
                return;
            };
            spawn(async move {
                let payload = serde_json::json!({
                    "input": { "ids": [id.clone()] }
                });
                if let Err(e) = invoke::<()>("messages_archive", payload).await {
                    web_sys_log(&format!("messages_archive: {e}"));
                    return;
                }
                selection.with_mut(|s| s.message = None);
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }
        KeyboardCommand::Delete => {
            let Some(id) = selection.read().message.clone() else {
                return;
            };
            spawn(async move {
                let payload = serde_json::json!({
                    "input": { "ids": [id.clone()] }
                });
                if let Err(e) = invoke::<()>("messages_delete", payload).await {
                    web_sys_log(&format!("messages_delete: {e}"));
                    return;
                }
                selection.with_mut(|s| s.message = None);
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }
        KeyboardCommand::NextMessage | KeyboardCommand::PrevMessage => {
            // Walk the published `visible_messages` list relative to the
            // current selection. Wraps at both ends so the user can hold
            // `j` to cycle the inbox without hitting an invisible stop.
            // No-op when the list is empty (compose pane open, search
            // returned nothing, etc.).
            let ids = visible_messages.read().clone();
            if ids.is_empty() {
                return;
            }
            let cur = selection.read().message.clone();
            let cur_idx = cur
                .as_ref()
                .and_then(|c| ids.iter().position(|m| m.0 == c.0));
            let next_idx = match (cmd, cur_idx) {
                (KeyboardCommand::NextMessage, None) => 0,
                (KeyboardCommand::PrevMessage, None) => ids.len() - 1,
                (KeyboardCommand::NextMessage, Some(i)) => (i + 1) % ids.len(),
                (KeyboardCommand::PrevMessage, Some(i)) => {
                    if i == 0 {
                        ids.len() - 1
                    } else {
                        i - 1
                    }
                }
                _ => unreachable!(),
            };
            selection.with_mut(|s| s.message = Some(ids[next_idx].clone()));
        }
        KeyboardCommand::Reply | KeyboardCommand::ReplyAll | KeyboardCommand::Forward => {
            let Some(id) = selection.read().message.clone() else {
                return;
            };
            let kind = match cmd {
                KeyboardCommand::Reply => ReplyKind::Reply,
                KeyboardCommand::ReplyAll => ReplyKind::ReplyAll,
                _ => ReplyKind::Forward,
            };
            // Reply / forward need the full RenderedMessage (subject
            // re-write, quoted body, etc.). Fetch it on demand — adds
            // ~one IPC round-trip but avoids lifting the reader's
            // already-rendered message out to a global signal.
            spawn(async move {
                let payload = serde_json::json!({
                    "input": { "id": id.clone(), "force_trusted": false }
                });
                match invoke::<RenderedMessage>("messages_get", payload).await {
                    Ok(rendered) => open_reply(rendered, compose, kind),
                    Err(e) => web_sys_log(&format!("messages_get for shortcut: {e}")),
                }
            });
        }
    }
}

/// Modal cheatsheet shown over the app shell when `?` is pressed.
/// Closing is via either pressing `?` or `Esc` (both routed through
/// the keyboard dispatcher) or clicking the backdrop.
/// Right-click popover anchored at the cursor coordinates carried by
/// `context_menu`. Operates on the single `message_id` that opened it
/// (NOT on the bulk selection — right-click is single-target by
/// convention; bulk lives in the `bulk-action-bar`). Dismisses on:
///   - any item click (after the action fires),
///   - clicking the transparent backdrop,
///   - Esc (handled by `dispatch_keyboard_command`'s Cancel arm; this
///     popover takes priority over palette / compose / search).
///
/// Reply / Reply-all / Forward fetch the `RenderedMessage` on demand
/// (one IPC round-trip) and call `open_reply` — the same path the
/// keyboard `r` / `a` / `f` shortcuts use, so the resulting compose
/// pane has identical headers + quoted-body.
#[component]
fn MessageContextPopover(
    mut context_menu: Signal<Option<MessageContextMenu>>,
    compose: Signal<Option<ComposeState>>,
    sync_tick: SyncTick,
    selection: Signal<Selection>,
) -> Element {
    let Some(state) = context_menu.read().clone() else {
        return rsx! {};
    };
    let MessageContextMenu {
        x,
        y,
        message_id,
        is_unread,
    } = state;
    // Pin to the viewport. The backdrop catches outside-clicks; the
    // popover stops propagation so clicking inside doesn't dismiss.
    let style = format!("position: fixed; left: {x}px; top: {y}px;");

    let mut dismiss = move || context_menu.set(None);

    let on_reply = {
        let id = message_id.clone();
        let compose_signal = compose;
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({
                    "input": { "id": id.clone(), "force_trusted": false }
                });
                match invoke::<RenderedMessage>("messages_get", payload).await {
                    Ok(rendered) => open_reply(rendered, compose_signal, ReplyKind::Reply),
                    Err(e) => web_sys_log(&format!("messages_get for context reply: {e}")),
                }
            });
        }
    };

    let on_reply_all = {
        let id = message_id.clone();
        let compose_signal = compose;
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({
                    "input": { "id": id.clone(), "force_trusted": false }
                });
                match invoke::<RenderedMessage>("messages_get", payload).await {
                    Ok(rendered) => open_reply(rendered, compose_signal, ReplyKind::ReplyAll),
                    Err(e) => web_sys_log(&format!("messages_get for context reply-all: {e}")),
                }
            });
        }
    };

    let on_forward = {
        let id = message_id.clone();
        let compose_signal = compose;
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({
                    "input": { "id": id.clone(), "force_trusted": false }
                });
                match invoke::<RenderedMessage>("messages_get", payload).await {
                    Ok(rendered) => open_reply(rendered, compose_signal, ReplyKind::Forward),
                    Err(e) => web_sys_log(&format!("messages_get for context forward: {e}")),
                }
            });
        }
    };

    let on_archive = {
        let id = message_id.clone();
        let mut sync_tick = sync_tick;
        let mut selection = selection;
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({ "input": { "ids": [id.clone()] } });
                if let Err(e) = invoke::<()>("messages_archive", payload).await {
                    web_sys_log(&format!("context archive: {e}"));
                    return;
                }
                selection.with_mut(|s| {
                    if s.message.as_ref().is_some_and(|m| m.0 == id.0) {
                        s.message = None;
                    }
                });
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }
    };

    let on_delete = {
        let id = message_id.clone();
        let mut sync_tick = sync_tick;
        let mut selection = selection;
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({ "input": { "ids": [id.clone()] } });
                if let Err(e) = invoke::<()>("messages_delete", payload).await {
                    web_sys_log(&format!("context delete: {e}"));
                    return;
                }
                selection.with_mut(|s| {
                    if s.message.as_ref().is_some_and(|m| m.0 == id.0) {
                        s.message = None;
                    }
                });
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }
    };

    // The toggle: if currently unread, mark read; if currently read,
    // mark unread. `is_unread` is captured at right-click time so the
    // label matches what the user saw on the row.
    let toggle_label = if is_unread {
        "Mark as read"
    } else {
        "Mark as unread"
    };
    let on_toggle_read = {
        let id = message_id.clone();
        let mut sync_tick = sync_tick;
        let mut menu = context_menu;
        let target_seen = is_unread; // unread → set seen=true
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({
                    "input": { "ids": [id.clone()], "seen": target_seen }
                });
                if let Err(e) = invoke::<()>("messages_mark_read", payload).await {
                    web_sys_log(&format!("context mark_read: {e}"));
                    return;
                }
                sync_tick.with_mut(|t| *t = t.wrapping_add(1));
            });
        }
    };

    let on_open_window = {
        let id = message_id.clone();
        let mut menu = context_menu;
        move |_| {
            menu.set(None);
            let id = id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = invoke::<()>(
                    "messages_open_in_window",
                    serde_json::json!({ "input": { "id": id } }),
                )
                .await
                {
                    web_sys_log(&format!("context open_in_window: {e}"));
                }
            });
        }
    };

    rsx! {
        div {
            class: "ctx-menu-backdrop",
            onclick: move |_| dismiss(),
            oncontextmenu: move |evt: Event<MouseData>| {
                // Right-click on the backdrop also dismisses, and we
                // suppress the browser's native menu so it doesn't
                // pile up on top of ours.
                evt.prevent_default();
                dismiss();
            },
            div {
                class: "ctx-menu",
                style: "{style}",
                onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                oncontextmenu: |evt: Event<MouseData>| evt.prevent_default(),
                button { class: "ctx-menu-item", r#type: "button", onclick: on_reply, "Reply" }
                button { class: "ctx-menu-item", r#type: "button", onclick: on_reply_all, "Reply all" }
                button { class: "ctx-menu-item", r#type: "button", onclick: on_forward, "Forward" }
                div { class: "ctx-menu-sep" }
                button { class: "ctx-menu-item", r#type: "button", onclick: on_toggle_read, "{toggle_label}" }
                button { class: "ctx-menu-item", r#type: "button", onclick: on_archive, "Archive" }
                button { class: "ctx-menu-item ctx-menu-item-danger", r#type: "button", onclick: on_delete, "Delete" }
                div { class: "ctx-menu-sep" }
                button { class: "ctx-menu-item", r#type: "button", onclick: on_open_window, "Open in new window" }
            }
        }
    }
}

#[component]
fn ShortcutsOverlay(mut visible: Signal<bool>) -> Element {
    rsx! {
        div {
            class: "shortcuts-overlay-backdrop",
            onclick: move |_| visible.set(false),
            div {
                class: "shortcuts-overlay",
                onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                h2 { class: "shortcuts-title", "Keyboard Shortcuts" }
                table {
                    class: "shortcuts-table",
                    tbody {
                        for (key, label) in [
                            ("⌘K", "Command palette"),
                            ("j", "Next message"),
                            ("k", "Previous message"),
                            ("c", "Compose"),
                            ("e", "Archive selected message"),
                            ("#", "Delete selected message"),
                            ("r", "Reply"),
                            ("a", "Reply all"),
                            ("f", "Forward"),
                            ("/", "Search mail"),
                            ("Esc", "Close palette / compose / search / selection"),
                            ("?", "Toggle this help"),
                        ] {
                            tr {
                                td {
                                    class: "shortcut-key",
                                    kbd { "{key}" }
                                }
                                td { class: "shortcut-label", "{label}" }
                            }
                        }
                    }
                }
                p {
                    class: "shortcuts-footnote",
                    "Shortcuts are ignored while typing in a field."
                }
            }
        }
    }
}

/// Command palette (⌘K). Centered overlay that fuzzy-matches the
/// query against three sources: every account's folders ("jump to"),
/// a static command list (compose / settings / add account / help),
/// and recent search queries. ESC / Cmd+K closes; arrow keys
/// navigate; Enter dispatches.
#[component]
fn CommandPalette(
    mut visible: Signal<bool>,
    mut selection: Signal<Selection>,
    mut compose: Signal<Option<ComposeState>>,
    sync_tick: SyncTick,
    mut search_query: Signal<String>,
    recent_searches: Signal<Vec<String>>,
    mut help_visible: Signal<bool>,
) -> Element {
    let mut query = use_signal(String::new);
    let mut active_idx = use_signal(|| 0usize);

    // Pull every account + its folders. Refetched on each open since
    // the palette only mounts when `visible` is true; closed-then-
    // reopened picks up brand-new accounts/folders without staleness.
    let folders = use_resource(|| async move {
        let accounts = invoke::<Vec<Account>>("accounts_list", ()).await?;
        let mut out: Vec<(Account, Vec<Folder>)> = Vec::with_capacity(accounts.len());
        for a in accounts {
            let folders_for_account = invoke::<Vec<Folder>>(
                "folders_list",
                serde_json::json!({ "input": { "account": a.id } }),
            )
            .await
            .unwrap_or_default();
            out.push((a, folders_for_account));
        }
        Ok::<_, String>(out)
    });

    // Build the entry list. Filter happens against the lowercase
    // search-text of each entry; substring is good enough for v0.1.
    let q = query.read().to_lowercase();
    // Static commands always appear so users can discover them.
    let mut entries: Vec<PaletteEntry> = vec![
        PaletteEntry::Command(PaletteCommand::Compose),
        PaletteEntry::Command(PaletteCommand::OpenSettings),
        PaletteEntry::Command(PaletteCommand::AddAccount),
        PaletteEntry::Command(PaletteCommand::ToggleHelp),
        PaletteEntry::Command(PaletteCommand::ClearReader),
    ];

    if let Some(Ok(account_groups)) = folders.read_unchecked().as_ref() {
        for (account, folder_list) in account_groups {
            for f in folder_list {
                entries.push(PaletteEntry::Folder {
                    account_id: account.id.clone(),
                    folder_id: f.id.clone(),
                    folder_label: crate::format::display_name_for_folder(&f.name).to_string(),
                    account_label: account.email_address.clone(),
                });
            }
        }
    }

    for s in recent_searches.read().iter() {
        entries.push(PaletteEntry::RecentSearch(s.clone()));
    }

    let filtered: Vec<PaletteEntry> = if q.is_empty() {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| e.search_text().to_lowercase().contains(&q))
            .collect()
    };

    // Clamp the active index inside the filtered range. Re-evaluating
    // on each render means a query that filters the list shorter
    // doesn't leave the highlight pointing past the end.
    let max_idx = filtered.len().saturating_sub(1);
    let cur_idx = (*active_idx.read()).min(max_idx);

    let mut dispatch_entry = move |entry: PaletteEntry, mut visible: Signal<bool>| {
        match entry {
            PaletteEntry::Folder {
                account_id,
                folder_id,
                ..
            } => {
                selection.with_mut(|s| {
                    s.account = Some(account_id);
                    s.folder = Some(folder_id);
                    s.message = None;
                    s.unified = false;
                });
            }
            PaletteEntry::Command(PaletteCommand::Compose) => {
                let default_account = selection.read().account.clone();
                compose.set(Some(ComposeState {
                    default_account,
                    draft_id: None,
                }));
            }
            PaletteEntry::Command(PaletteCommand::OpenSettings) => {
                spawn(async move {
                    if let Err(e) = invoke::<()>("settings_open", serde_json::json!({})).await {
                        web_sys_log(&format!("settings_open: {e}"));
                    }
                });
            }
            PaletteEntry::Command(PaletteCommand::AddAccount) => {
                spawn(async move {
                    if let Err(e) = invoke::<()>("oauth_add_open", serde_json::json!({})).await {
                        web_sys_log(&format!("oauth_add_open: {e}"));
                    }
                });
            }
            PaletteEntry::Command(PaletteCommand::ToggleHelp) => {
                let cur = *help_visible.read();
                help_visible.set(!cur);
            }
            PaletteEntry::Command(PaletteCommand::ClearReader) => {
                selection.with_mut(|s| s.message = None);
            }
            PaletteEntry::RecentSearch(s) => {
                search_query.set(s);
            }
        }
        let _ = sync_tick;
        visible.set(false);
    };

    rsx! {
        div {
            class: "palette-backdrop",
            onclick: move |_| visible.set(false),
            div {
                class: "palette-shell",
                onclick: |evt: Event<MouseData>| evt.stop_propagation(),
                input {
                    id: "qsl-palette-input",
                    class: "palette-input",
                    r#type: "text",
                    autocomplete: "off",
                    spellcheck: "false",
                    placeholder: "Jump to mailbox · run command · recent search",
                    value: "{query}",
                    oninput: move |evt: Event<FormData>| {
                        query.set(evt.value());
                        active_idx.set(0);
                    },
                    onkeydown: {
                        let entries_for_key = filtered.clone();
                        move |evt: Event<KeyboardData>| {
                            match evt.key() {
                                Key::Escape => {
                                    evt.prevent_default();
                                    visible.set(false);
                                }
                                Key::ArrowDown => {
                                    evt.prevent_default();
                                    if !entries_for_key.is_empty() {
                                        let next = (cur_idx + 1) % entries_for_key.len();
                                        active_idx.set(next);
                                    }
                                }
                                Key::ArrowUp => {
                                    evt.prevent_default();
                                    if !entries_for_key.is_empty() {
                                        let prev = if cur_idx == 0 {
                                            entries_for_key.len() - 1
                                        } else {
                                            cur_idx - 1
                                        };
                                        active_idx.set(prev);
                                    }
                                }
                                Key::Enter => {
                                    evt.prevent_default();
                                    if let Some(entry) = entries_for_key.get(cur_idx).cloned() {
                                        dispatch_entry(entry, visible);
                                    }
                                }
                                _ => {}
                            }
                        }
                    },
                    autofocus: true,
                }
                if filtered.is_empty() {
                    div { class: "palette-empty", "No matches." }
                } else {
                    div {
                        class: "palette-list",
                        for (i, entry) in filtered.iter().enumerate() {
                            {
                                let entry_for_click = entry.clone();
                                let row_class = if i == cur_idx {
                                    "palette-row palette-row-active"
                                } else {
                                    "palette-row"
                                };
                                rsx! {
                                    button {
                                        key: "{i}",
                                        class: row_class,
                                        r#type: "button",
                                        onmouseenter: move |_| active_idx.set(i),
                                        onclick: move |_| dispatch_entry(entry_for_click.clone(), visible),
                                        span { class: "palette-row-kind", "{entry.kind_label()}" }
                                        span { class: "palette-row-label", "{entry.primary_label()}" }
                                        if let Some(meta) = entry.secondary_label() {
                                            span { class: "palette-row-meta", "{meta}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div {
                    class: "palette-footnote",
                    "↑↓ navigate · ↵ select · Esc close"
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaletteEntry {
    Folder {
        account_id: AccountId,
        folder_id: FolderId,
        folder_label: String,
        account_label: String,
    },
    Command(PaletteCommand),
    RecentSearch(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteCommand {
    Compose,
    OpenSettings,
    AddAccount,
    ToggleHelp,
    ClearReader,
}

impl PaletteEntry {
    fn kind_label(&self) -> &'static str {
        match self {
            PaletteEntry::Folder { .. } => "jump",
            PaletteEntry::Command(_) => "cmd",
            PaletteEntry::RecentSearch(_) => "recent",
        }
    }

    fn primary_label(&self) -> String {
        match self {
            PaletteEntry::Folder { folder_label, .. } => folder_label.clone(),
            PaletteEntry::Command(cmd) => cmd.label().to_string(),
            PaletteEntry::RecentSearch(q) => q.clone(),
        }
    }

    fn secondary_label(&self) -> Option<String> {
        match self {
            PaletteEntry::Folder { account_label, .. } => Some(account_label.clone()),
            PaletteEntry::Command(_) => None,
            PaletteEntry::RecentSearch(_) => None,
        }
    }

    /// Text the filter substring-matches against. Includes both the
    /// primary label and the secondary so typing the account name
    /// surfaces every folder under it.
    fn search_text(&self) -> String {
        match self {
            PaletteEntry::Folder {
                folder_label,
                account_label,
                ..
            } => format!("{folder_label} {account_label}"),
            PaletteEntry::Command(cmd) => cmd.label().to_string(),
            PaletteEntry::RecentSearch(q) => q.clone(),
        }
    }
}

impl PaletteCommand {
    fn label(&self) -> &'static str {
        match self {
            PaletteCommand::Compose => "Compose new message",
            PaletteCommand::OpenSettings => "Open settings",
            PaletteCommand::AddAccount => "Add account",
            PaletteCommand::ToggleHelp => "Toggle keyboard shortcuts",
            PaletteCommand::ClearReader => "Close reader",
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

/// Top bar above the three-pane shell. Renders the `qsl` wordmark,
/// version, a `⌘K` command-palette pill (no-op until the palette
/// lands), and an account-filter chip on the right. Mono + dense
/// per `docs/ui-direction.md` § Top bar.
///
/// `account_filter` is the App-root signal that scopes the message
/// list to a single account. `None` = show every account; clicking
/// the chip opens a dropdown with the configured accounts plus an
/// "all accounts" reset.
#[component]
fn TopBar(account_filter: Signal<Option<AccountId>>, mut palette_visible: Signal<bool>) -> Element {
    let accounts = use_resource(|| async { invoke::<Vec<Account>>("accounts_list", ()).await });
    let mut chip_open: Signal<bool> = use_signal(|| false);

    let chip_label: String = match (
        account_filter.read().clone(),
        accounts.read_unchecked().as_ref(),
    ) {
        (None, Some(Ok(list))) if list.is_empty() => "no accounts".to_string(),
        (None, _) => "all accounts".to_string(),
        (Some(id), Some(Ok(list))) => list
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.email_address.clone())
            .unwrap_or_else(|| id.0.clone()),
        (Some(id), _) => id.0.clone(),
    };

    rsx! {
        header {
            class: "topbar",
            div {
                class: "topbar-left",
                span { class: "topbar-wordmark", "qsl" }
                span {
                    class: "topbar-version",
                    {env!("CARGO_PKG_VERSION")}
                }
            }
            button {
                class: "topbar-cmd-pill",
                r#type: "button",
                title: "Command palette ({palette_shortcut_label()})",
                onclick: move |_| {
                    let cur = *palette_visible.read();
                    palette_visible.set(!cur);
                },
                span { class: "topbar-cmd-key", "{palette_shortcut_label()}" }
                span { "search · jump · command" }
            }
            div {
                class: "topbar-right",
                div {
                    class: "topbar-chip-wrap",
                    button {
                        class: if chip_open() { "topbar-chip topbar-chip-open" } else { "topbar-chip" },
                        r#type: "button",
                        title: "Filter messages by account",
                        onclick: move |_| {
                            let cur = chip_open();
                            chip_open.set(!cur);
                        },
                        span { class: "topbar-chip-label", "{chip_label}" }
                        span { class: "topbar-chip-arrow", if chip_open() { "▴" } else { "▾" } }
                    }
                    if chip_open() {
                        div {
                            class: "topbar-chip-menu",
                            button {
                                class: if account_filter.read().is_none() {
                                    "topbar-chip-item topbar-chip-item-active"
                                } else {
                                    "topbar-chip-item"
                                },
                                r#type: "button",
                                onclick: move |_| {
                                    account_filter.set(None);
                                    chip_open.set(false);
                                },
                                "all accounts"
                            }
                            match &*accounts.read_unchecked() {
                                Some(Ok(list)) => rsx! {
                                    for a in list.iter().cloned() {
                                        {
                                            let id_for_select = a.id.clone();
                                            let id_for_match = a.id.clone();
                                            let active = account_filter.read().as_ref() == Some(&id_for_match);
                                            rsx! {
                                                button {
                                                    key: "{a.id.0}",
                                                    class: if active {
                                                        "topbar-chip-item topbar-chip-item-active"
                                                    } else {
                                                        "topbar-chip-item"
                                                    },
                                                    r#type: "button",
                                                    onclick: move |_| {
                                                        account_filter.set(Some(id_for_select.clone()));
                                                        chip_open.set(false);
                                                    },
                                                    "{a.email_address}"
                                                }
                                            }
                                        }
                                    }
                                },
                                Some(Err(e)) => rsx! {
                                    span { class: "topbar-chip-item topbar-chip-error", "Error: {e}" }
                                },
                                None => rsx! {},
                            }
                        }
                    }
                }
            }
        }
    }
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
            div {
                class: "status-left",
                span { class: "{dot_class}" }
                span { class: "status-label", "{label}" }
            }
            div {
                class: "status-center",
                // Capability flags (CONDSTORE / QRESYNC / IDLE) land here
                // once the connection-level negotiation surfaces in the
                // sync events. Phase 2 placeholder: blank center column
                // keeps the 3-column grid stable.
            }
            div {
                class: "status-right",
                span { "? help" }
            }
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
/// message is selected. Thin wrapper around
/// [`qsl_mime::compose_reader_html`] that translates from the IPC
/// `RenderedMessage` shape — both this UI path and the desktop popup
/// install path go through the same shared composer so the markup is
/// byte-identical and the placeholder / theming rules live in one
/// place.
pub(crate) fn compose_reader_html(rendered: &RenderedMessage) -> String {
    qsl_core::compose_reader_html(
        rendered.sanitized_html.as_deref(),
        rendered.body_text.as_deref(),
    )
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
fn ComposePane(
    compose: Signal<Option<ComposeState>>,
    sync_tick: SyncTick,
    undo_send_pending: Signal<Option<f64>>,
) -> Element {
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
    // Bcc starts hidden: most outgoing mail doesn't use it, so the
    // dense compose chrome shouldn't reserve a row by default. The
    // user reveals it via the `+bcc` link in the Cc row, OR a draft
    // hydration that finds non-empty bcc content auto-reveals it
    // (carrying over from a saved draft or a future reply-with-bcc
    // flow). Once shown in this compose session, stays shown.
    let mut bcc_revealed = use_signal(|| false);

    // Once-per-mount guard for "did we already append the signature?"
    // Without it the signature effect would re-run on every account
    // switch and stack copies into the body, or worse, append over a
    // user's edits. We append on first observation of a non-empty
    // account_id when the body is empty, then never again.
    let mut signature_applied = use_signal(|| false);
    let mut last_change = use_signal(|| 0u64);
    let mut save_status: Signal<SaveStatus> = use_signal(|| SaveStatus::Idle);
    let send_in_flight: Signal<bool> = use_signal(|| false);
    // Drives the countdown banner re-render. Updated by the timer in
    // `send_now` while `undo_send_pending` is `Some`. Local because
    // the App-level Cancel handler doesn't need to read it — only
    // `undo_send_pending` (the deadline) is the cross-component
    // contract; this is just for display.
    let undo_send_now_ms = use_signal(|| 0.0_f64);
    // Attachments. Populated either by the file picker (`+ Attach`
    // button) or rehydrated from the loaded draft. Round-trips through
    // every `drafts_save` call so the persisted draft row matches the
    // editor state.
    let mut attachments: Signal<Vec<DraftAttachment>> = use_signal(Vec::new);
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
                            let bcc_loaded = format_addrs(&d.bcc);
                            if !bcc_loaded.trim().is_empty() {
                                bcc_revealed.set(true);
                            }
                            bcc_str.set(bcc_loaded);
                            subject.set(d.subject);
                            body.set(d.body);
                            in_reply_to.set(d.in_reply_to);
                            references.set(d.references);
                            attachments.set(d.attachments);
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

    // Append the per-account signature on the first compose mount
    // for a fresh draft. Reads `compose.signature.<account_id>` from
    // app_settings; if non-empty, appends `\n\n-- \n<sig>` to the
    // body. RFC 3676 §4.3 sig delimiter (`-- ` with trailing space
    // and a literal newline) is what every modern client expects.
    // Skipped when:
    //   - draft was hydrated from disk (existing draft already has
    //     the user's prior body, including any earlier signature),
    //   - body already has content (user pasted before the
    //     signature lookup landed), or
    //   - this compose session has already applied a signature.
    {
        let initial_had_draft = initial.draft_id.is_some();
        let acc = account_id.read().clone();
        use_effect(use_reactive!(|acc| {
            if initial_had_draft {
                return;
            }
            if *signature_applied.read() {
                return;
            }
            let Some(account) = acc.as_ref() else {
                return;
            };
            if !body.read().is_empty() {
                return;
            }
            let key = crate::settings::compose_signature_key(account);
            wasm_bindgen_futures::spawn_local(async move {
                let payload = serde_json::json!({ "input": { "key": key } });
                match invoke::<Option<String>>("app_settings_get", payload).await {
                    Ok(Some(sig)) if !sig.trim().is_empty() => {
                        let mut current = body.read().clone();
                        if current.is_empty() && !*signature_applied.read() {
                            current.push_str("\n\n-- \n");
                            current.push_str(&sig);
                            body.set(current);
                            signature_applied.set(true);
                        }
                    }
                    _ => {}
                }
            });
        }));
    }

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
        let attachments_signal = attachments;
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
            let attachments_val = attachments_signal.read().clone();

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
                            "attachments": attachments_val,
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
        let attachments_signal = attachments;
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
            let attachments_val = attachments_signal.read().clone();
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
                            "attachments": attachments_val,
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
    //
    // Undo-send: when `compose.undo_send` is `5` / `10` / `30` (sec),
    // the click instead arms a banner with a deadline. The body of
    // this closure spawns a 250 ms tick loop that watches for the
    // user (or the Esc handler in `dispatch_keyboard_command`) clearing
    // `undo_send_pending`. If the deadline arrives with the same
    // pending value, we proceed to save + submit; if it changed, we
    // bail. Equality on the deadline (as opposed to a separate
    // generation counter) is what makes external cancellation work
    // without extra plumbing — the App-level Cancel handler just sets
    // the signal to `None` and our loop notices.
    let mut send_now = {
        let acc_signal = account_id;
        let to_signal = to_str;
        let cc_signal = cc_str;
        let bcc_signal = bcc_str;
        let subject_signal = subject;
        let body_signal = body;
        let in_reply_to_signal = in_reply_to;
        let references_signal = references;
        let attachments_signal = attachments;
        let mut draft_signal = draft_id;
        let mut status_signal = save_status;
        let mut sync_tick = sync_tick;
        let mut compose_signal = compose;
        let mut sending = send_in_flight;
        let mut undo_pending = undo_send_pending;
        let mut undo_now_ms = undo_send_now_ms;
        move || {
            if *sending.read() {
                return;
            }
            if undo_pending.read().is_some() {
                return;
            }
            let acc = acc_signal.read().clone();
            let Some(_) = acc else { return };
            let to = parse_addrs(&to_signal.read());
            let cc = parse_addrs(&cc_signal.read());
            let bcc = parse_addrs(&bcc_signal.read());
            if to.is_empty() && cc.is_empty() && bcc.is_empty() {
                status_signal.set(SaveStatus::Error("Add at least one recipient".into()));
                return;
            }
            wasm_bindgen_futures::spawn_local(async move {
                // Read the undo-send setting once per click. `off`
                // (or unset) means "send immediately"; otherwise we
                // hold the click for the configured number of seconds.
                let setting = invoke::<Option<String>>(
                    "app_settings_get",
                    serde_json::json!({ "input": { "key": "compose.undo_send" } }),
                )
                .await
                .ok()
                .flatten();
                let hold_secs: u64 = match setting.as_deref() {
                    Some("5") => 5,
                    Some("10") => 10,
                    Some("30") => 30,
                    _ => 0,
                };

                if hold_secs > 0 {
                    let now_ms = js_sys::Date::now();
                    let deadline_ms = now_ms + (hold_secs as f64) * 1000.0;
                    undo_pending.set(Some(deadline_ms));
                    undo_now_ms.set(now_ms);
                    loop {
                        gloo_timers::future::sleep(std::time::Duration::from_millis(250)).await;
                        let cur = *undo_pending.read();
                        if cur != Some(deadline_ms) {
                            // Cancelled (set to None) or replaced
                            // (deadline rearmed by another click).
                            return;
                        }
                        let now = js_sys::Date::now();
                        undo_now_ms.set(now);
                        if now >= deadline_ms {
                            break;
                        }
                    }
                    let cur = *undo_pending.read();
                    if cur != Some(deadline_ms) {
                        return;
                    }
                    undo_pending.set(None);
                }

                // Re-snapshot the form. Edits during the hold window
                // are intentionally honored — "send in 30s, fix typos
                // in the meantime" is part of the value of undo-send.
                let acc = match acc_signal.read().clone() {
                    Some(a) => a,
                    None => return,
                };
                let to = parse_addrs(&to_signal.read());
                let cc = parse_addrs(&cc_signal.read());
                let bcc = parse_addrs(&bcc_signal.read());
                let subject_text = subject_signal.read().clone();
                let body_text = body_signal.read().clone();
                let id = draft_signal.read().clone();
                let in_reply_to_val = in_reply_to_signal.read().clone();
                let references_val = references_signal.read().clone();
                let attachments_val = attachments_signal.read().clone();

                sending.set(true);
                status_signal.set(SaveStatus::Saving);

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
                            "attachments": attachments_val,
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

    // Cancel a pending undo-send. Idempotent — clicking when nothing
    // is pending is a no-op. Used by the on-screen Undo button; the
    // global Esc handler in `dispatch_keyboard_command` clears the
    // signal directly.
    let mut cancel_undo_send = {
        let mut undo_pending = undo_send_pending;
        let mut status = save_status;
        move || {
            if undo_pending.read().is_some() {
                undo_pending.set(None);
                status.set(SaveStatus::Idle);
            }
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

    let subject_label = {
        let s = subject.read().clone();
        if s.is_empty() {
            "(no subject)".to_string()
        } else {
            s
        }
    };
    let body_chars = body.read().chars().count();
    let body_words = body.read().split_whitespace().count();

    rsx! {
        section {
            class: "compose-pane",
            header {
                class: "compose-head",
                strong { "qsl" }
                span { class: "compose-statusline-key", "//" }
                span { "compose · {subject_label}" }
                span { class: "compose-spacer" }
                ComposeStatusLabel { status: save_status }
                button {
                    class: "compose-action danger",
                    r#type: "button",
                    onclick: move |_| discard(),
                    "discard"
                }
                button {
                    class: "compose-action secondary",
                    r#type: "button",
                    onclick: move |_| manual_save(),
                    "save"
                }
                button {
                    class: "compose-action secondary",
                    r#type: "button",
                    onclick: move |_| close_compose(),
                    span { class: "compose-statusline-key", "⌘W" }
                    " close"
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
                    // Cc row: full AddressField, plus a `+bcc` reveal
                    // link inline when Bcc is still hidden. Once Bcc is
                    // showing, the link is gone — there's no need to
                    // re-hide it for the duration of this compose
                    // session.
                    div {
                        class: "compose-cc-row",
                        AddressField {
                            label: "Cc",
                            value: cc_str,
                            on_change: bump,
                        }
                        if !*bcc_revealed.read() {
                            button {
                                r#type: "button",
                                class: "compose-bcc-reveal",
                                title: "Show the Bcc field",
                                onclick: move |_| bcc_revealed.set(true),
                                "+bcc"
                            }
                        }
                    }
                    if *bcc_revealed.read() {
                        AddressField {
                            label: "Bcc",
                            value: bcc_str,
                            on_change: bump,
                        }
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
                    // Attachments row. The chip list mirrors the
                    // `attachments` signal — picking files via the
                    // button appends; clicking × removes. Empty list
                    // collapses to just the button.
                    AttachmentsRow {
                        attachments,
                        on_change: bump,
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
                        },
                        // Ctrl/Cmd+Enter sends. The document-level
                        // keyboard parser is bypassed inside textareas
                        // (see `is_typing_in_field`), so this handler
                        // owns the send shortcut for the compose pane.
                        onkeydown: {
                            let mut send_now_kbd = send_now;
                            move |evt: Event<KeyboardData>| {
                                if (evt.modifiers().contains(Modifiers::CONTROL)
                                    || evt.modifiers().contains(Modifiers::META))
                                    && evt.key() == Key::Enter
                                {
                                    evt.prevent_default();
                                    send_now_kbd();
                                }
                            }
                        }
                    }
                }
            }
            // Status line — the ⌘↵ send affordance is the only send
            // surface in the new design (ui-direction.md § Compose).
            // Click triggers send_now; ⌘↵ on the textarea above does
            // the same. No button-shaped Send element appears anywhere.
            footer {
                class: "compose-statusline",
                div {
                    class: "compose-statusline-left",
                    span { "plain" }
                    span { class: "status-sep", "·" }
                    span { "{body_words}w / {body_chars}c" }
                }
                div {
                    class: "compose-statusline-center",
                    ComposeStatusLabel { status: save_status }
                }
                div {
                    class: "compose-statusline-right",
                    if let Some(deadline) = *undo_send_pending.read() {
                        // Pending undo-send. Replace the Send button
                        // with Undo + countdown. The button gets
                        // `autofocus` so Esc on it (without leaving
                        // the pane) cancels — the document-level
                        // Cancel handler also clears the signal, so
                        // any-focus Esc still works.
                        {
                            let now = *undo_send_now_ms.read();
                            let remaining_ms = (deadline - now).max(0.0);
                            let secs = (remaining_ms / 1000.0).ceil() as i64;
                            rsx! {
                                span { class: "compose-undo-countdown", "Sending in {secs}s" }
                                button {
                                    class: "compose-statusline-send danger",
                                    r#type: "button",
                                    title: "Cancel send (Esc)",
                                    autofocus: true,
                                    onclick: move |_| cancel_undo_send(),
                                    span { class: "compose-statusline-key", "Esc" }
                                    " undo"
                                }
                            }
                        }
                    } else {
                        button {
                            class: "compose-statusline-send",
                            r#type: "button",
                            title: "Send (⌘↵)",
                            disabled: *send_in_flight.read(),
                            onclick: move |_| send_now(),
                            span { class: "compose-statusline-key", "⌘↵" }
                            " "
                            if *send_in_flight.read() { "sending…" } else { "send" }
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

/// Compose-pane attachments row: a list of attachment chips plus an
/// "Attach" button that opens the OS file picker via
/// `compose_pick_attachments`. The picker returns one
/// [`DraftAttachment`] per chosen file; the list is appended (existing
/// attachments are preserved) and `on_change` fires so the auto-save
/// effect bumps `last_change` for the next save round-trip.
///
/// File size is rendered as a humanized string (KB/MB) so the user
/// sees that "they're about to send a 14 MB attachment" before
/// hitting Send.
#[component]
fn AttachmentsRow(
    mut attachments: Signal<Vec<DraftAttachment>>,
    on_change: EventHandler<()>,
) -> Element {
    let pick = move |_| {
        let mut atts = attachments;
        let on_change = on_change;
        wasm_bindgen_futures::spawn_local(async move {
            let payload = serde_json::json!({ "input": { "title": "Attach files" } });
            match invoke::<Vec<DraftAttachment>>("compose_pick_attachments", payload).await {
                Ok(picked) if picked.is_empty() => {
                    // User cancelled the dialog. No-op.
                }
                Ok(picked) => {
                    atts.with_mut(|v| v.extend(picked));
                    on_change.call(());
                }
                Err(e) => {
                    web_sys_log(&format!("compose_pick_attachments: {e}"));
                }
            }
        });
    };

    let items = attachments.read().clone();

    rsx! {
        div {
            class: "compose-attachments-row",
            label { class: "label", "Files" }
            div {
                class: "compose-attachments-list",
                for (idx, att) in items.iter().enumerate().map(|(i, a)| (i, a.clone())) {
                    {
                        let on_change_remove = on_change;
                        let label = format!(
                            "{} ({})",
                            att.filename,
                            humanize_bytes(att.size_bytes)
                        );
                        rsx! {
                            span {
                                key: "{idx}-{att.path}",
                                class: "compose-attachment-chip",
                                title: "{att.path}",
                                "{label}"
                                button {
                                    class: "compose-attachment-remove",
                                    r#type: "button",
                                    title: "Remove this attachment",
                                    onclick: move |_| {
                                        attachments.with_mut(|v| {
                                            if idx < v.len() {
                                                v.remove(idx);
                                            }
                                        });
                                        on_change_remove.call(());
                                    },
                                    "×"
                                }
                            }
                        }
                    }
                }
                button {
                    class: "compose-attachment-add",
                    r#type: "button",
                    title: "Attach files (opens file picker)",
                    onclick: pick,
                    "+ Attach"
                }
            }
        }
    }
}

/// Render a byte count as a short, humanized string. Approximate — the
/// goal is "user sees that this is small/big" not exact accounting.
fn humanize_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.0} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
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

    // Autocomplete dropdown state. Each `AddressField` instance has
    // its own — multiple fields don't share a dropdown so opening
    // To: doesn't squash a Cc: dropdown that's already open.
    let suggestions: Signal<Vec<Contact>> = use_signal(Vec::new);
    let highlighted: Signal<usize> = use_signal(|| 0usize);
    let dropdown_open: Signal<bool> = use_signal(|| false);

    // Re-fetch suggestions whenever the *active segment* (the chunk
    // after the last comma/semicolon) changes and is at least 2
    // characters. Below the 2-char threshold the dropdown closes —
    // surfacing the whole table on a single character is noisy and
    // the IPC round-trip is wasted.
    let value_str = value.read().clone();
    let active = active_segment(&value_str).to_string();
    use_effect(use_reactive!(|active| {
        let mut suggestions = suggestions;
        let mut dropdown_open = dropdown_open;
        let mut highlighted = highlighted;
        if active.trim().len() < 2 {
            suggestions.set(Vec::new());
            dropdown_open.set(false);
            highlighted.set(0);
            return;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let payload = serde_json::json!({ "input": { "prefix": active.trim(), "limit": 8 } });
            match invoke::<Vec<Contact>>("contacts_query", payload).await {
                Ok(rows) => {
                    let any = !rows.is_empty();
                    suggestions.set(rows);
                    dropdown_open.set(any);
                    highlighted.set(0);
                }
                Err(e) => {
                    web_sys_log(&format!("contacts_query: {e}"));
                    suggestions.set(Vec::new());
                    dropdown_open.set(false);
                }
            }
        });
    }));

    let snapshot = suggestions.read().clone();
    let snapshot_for_keys = snapshot.clone();
    let is_open = dropdown_open();
    let active_idx = highlighted();

    rsx! {
        div {
            class: "field-row field-row-autocomplete",
            label { class: "label", r#for: "{id}", "{label}" }
            div {
                class: "compose-input-wrap",
                input {
                    id: "{id}",
                    class: "compose-input",
                    r#type: "text",
                    placeholder: "name@example.com, another@example.com",
                    value: "{value.read()}",
                    autocomplete: "off",
                    oninput: {
                        let mut value = value;
                        move |evt: Event<FormData>| {
                            value.set(evt.value());
                            on_change.call(());
                        }
                    },
                    onkeydown: {
                        let mut value = value;
                        let on_change_for_keys = on_change;
                        let suggestions_for_keys = suggestions;
                        let mut highlighted = highlighted;
                        let mut dropdown_open = dropdown_open;
                        move |evt: Event<KeyboardData>| {
                            if !is_open {
                                return;
                            }
                            let key = evt.key().to_string();
                            let len = suggestions_for_keys.read().len();
                            if len == 0 {
                                return;
                            }
                            match key.as_str() {
                                "ArrowDown" => {
                                    evt.prevent_default();
                                    highlighted.set((active_idx + 1) % len);
                                }
                                "ArrowUp" => {
                                    evt.prevent_default();
                                    highlighted.set(if active_idx == 0 {
                                        len - 1
                                    } else {
                                        active_idx - 1
                                    });
                                }
                                "Enter" | "Tab" => {
                                    let pick = suggestions_for_keys
                                        .read()
                                        .get(active_idx)
                                        .cloned();
                                    if let Some(pick) = pick {
                                        evt.prevent_default();
                                        let next = replace_active_segment(
                                            &value.read(),
                                            &format_contact(&pick),
                                        );
                                        value.set(next);
                                        dropdown_open.set(false);
                                        on_change_for_keys.call(());
                                    }
                                }
                                "Escape" => {
                                    evt.prevent_default();
                                    dropdown_open.set(false);
                                }
                                _ => {}
                            }
                        }
                    },
                    // Closing on blur uses a small delay (gloo
                    // sleep-then-set) so a click on a dropdown row
                    // gets a chance to fire its onmousedown before
                    // the dropdown unmounts. Without the delay the
                    // input loses focus, the dropdown closes, and
                    // the click never lands.
                    onblur: {
                        let mut dropdown_open = dropdown_open;
                        move |_| {
                            wasm_bindgen_futures::spawn_local(async move {
                                gloo_timers::future::sleep(
                                    std::time::Duration::from_millis(150),
                                )
                                .await;
                                dropdown_open.set(false);
                            });
                        }
                    },
                }
                if is_open && !snapshot.is_empty() {
                    div {
                        class: "compose-autocomplete",
                        for (i, contact) in snapshot.iter().enumerate().collect::<Vec<_>>() {
                            div {
                                key: "{contact.address}",
                                class: if i == active_idx {
                                    "compose-autocomplete-row active"
                                } else {
                                    "compose-autocomplete-row"
                                },
                                // `onmousedown` (not `onclick`) so we
                                // beat the input's onblur — clicking
                                // a row should pick it, not just
                                // close the dropdown.
                                onmousedown: {
                                    let mut value = value;
                                    let mut dropdown_open = dropdown_open;
                                    let on_change = on_change;
                                    let pick = contact.clone();
                                    move |evt: Event<MouseData>| {
                                        evt.prevent_default();
                                        let next = replace_active_segment(
                                            &value.read(),
                                            &format_contact(&pick),
                                        );
                                        value.set(next);
                                        dropdown_open.set(false);
                                        on_change.call(());
                                    }
                                },
                                if let Some(name) = contact.display_name.as_deref() {
                                    if !name.is_empty() {
                                        span {
                                            class: "compose-autocomplete-name",
                                            "{name}"
                                        }
                                    }
                                }
                                span {
                                    class: "compose-autocomplete-address",
                                    "{contact.address}"
                                }
                            }
                        }
                        // Suppress an unused-var warning for the moved snapshot.
                        // (No-op block: the iterator above borrowed it once.)
                        {
                            let _ = &snapshot_for_keys;
                            rsx! {}
                        }
                    }
                }
            }
        }
    }
}

/// Slice of `line` that's currently being typed — everything after
/// the last `,` or `;`. Used to scope autocomplete to the active
/// address rather than the whole field. Trims leading whitespace
/// so `"alice@…, bo"` returns `"bo"`, not `" bo"`.
fn active_segment(line: &str) -> &str {
    let split_at = line.rfind([',', ';']).map(|i| i + 1).unwrap_or(0);
    line[split_at..].trim_start()
}

/// Replace the active segment of `line` with `replacement`, keeping
/// the leading addresses and the comma. Used when the user picks
/// a dropdown row.
fn replace_active_segment(line: &str, replacement: &str) -> String {
    let split_at = line.rfind([',', ';']).map(|i| i + 1).unwrap_or(0);
    let prefix = &line[..split_at];
    // Reattach with a single space after the separator so subsequent
    // typing produces `addr1, addr2` not `addr1,addr2`.
    let glue = if prefix.is_empty() { "" } else { " " };
    format!("{prefix}{glue}{replacement}, ")
}

/// Format a Contact as the user-facing one-line address form. Uses
/// `Name <addr>` when a display name is present, just `addr`
/// otherwise — matches what `format_addrs` produces for hand-typed
/// entries.
fn format_contact(c: &Contact) -> String {
    match c.display_name.as_deref() {
        Some(name) if !name.is_empty() => format!("{name} <{}>", c.address),
        _ => c.address.clone(),
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
    // Refetch on every sync_event tick so a brand-new account from
    // `accounts_add_oauth` appears in the sidebar without requiring
    // the user to restart. The bootstrap-sync started inside
    // `accounts_add_oauth` emits per-folder sync_events; the first
    // bumps `sync_tick` here.
    let tick_value = sync_tick();
    let accounts = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<Vec<Account>>("accounts_list", ()).await
    }));

    // The sidebar's compose button is gone in the new design — compose
    // is keyboard-driven via `c` (KeyboardCommand::Compose, dispatched
    // by `dispatch_keyboard_command`) per `docs/ui-direction.md` § Sidebar.
    // The `compose` signal is still threaded through for the empty-state
    // CTA (see SidebarAccountBlock's "Add account" button).
    let _ = compose;

    rsx! {
        aside {
            class: "sidebar",
            div {
                class: "sidebar-scroll",
                match &*accounts.read_unchecked() {
                    None => rsx! {},
                    Some(Err(e)) => rsx! { p { class: "sidebar-account-email", "Error: {e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        div {
                            class: "sidebar-empty-state",
                            p {
                                class: "sidebar-empty-blurb",
                                "No accounts configured yet."
                            }
                            button {
                                class: "sidebar-empty-add",
                                r#type: "button",
                                onclick: |_| {
                                    spawn(async {
                                        if let Err(e) = invoke::<()>("oauth_add_open", serde_json::json!({})).await {
                                            web_sys_log(&format!("oauth_add_open: {e}"));
                                        }
                                    });
                                },
                                "Add an account"
                            }
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        for a in list.iter().cloned() {
                            SidebarAccountBlock { account: a, selection, sync_tick }
                        }
                    },
                }
            }
            div {
                class: "sidebar-footer",
                button {
                    class: "sidebar-settings-btn",
                    r#type: "button",
                    title: "Open settings",
                    onclick: |_| {
                        spawn(async {
                            if let Err(e) = invoke::<()>("settings_open", serde_json::json!({})).await {
                                web_sys_log(&format!("settings_open: {e}"));
                            }
                        });
                    },
                    span { class: "sidebar-settings-icon", "⚙" }
                    span { "Settings" }
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
    bulk_selected: Signal<HashSet<MessageId>>,
    search_query: Signal<String>,
    account_filter: Signal<Option<AccountId>>,
    visible_messages: Signal<Vec<MessageId>>,
) -> Element {
    let unified = selection.read().unified;
    let folder_id = selection.read().folder.clone();
    let any_checked = !bulk_selected.read().is_empty();
    let active_query = search_query.read().clone();
    let searching = !active_query.is_empty();
    rsx! {
        section {
            class: "msglist",
            SearchBar { search_query }
            if any_checked {
                BulkActionBar { bulk_selected, sync_tick }
            }
            if searching {
                SearchResults { query: active_query, selection, bulk_selected, account_filter, visible_messages }
            } else if unified {
                UnifiedMessageListV2 { selection, sync_tick, bulk_selected, account_filter, visible_messages }
            } else {
                match folder_id {
                    None => rsx! {
                        p {
                            class: "msglist-empty",
                            "Select a mailbox to view messages."
                        }
                    },
                    Some(fid) => rsx! {
                        MessageListV2 { folder: fid, selection, folder_tokens, bulk_selected, visible_messages }
                    },
                }
            }
        }
    }
}

/// Persistent search input above the message list. Uncontrolled-ish
/// pattern: the input value is bound to `search_query` via `oninput`,
/// and the parent re-renders the results list when the signal flips
/// from empty → non-empty (or back).
///
/// Esc on the input clears the query and blurs (returning the user
/// to the regular list). The keyboard cheatsheet's `/` shortcut
/// focuses this element by id.
#[component]
fn SearchBar(mut search_query: Signal<String>) -> Element {
    let value = search_query.read().clone();
    let has_value = !value.is_empty();
    rsx! {
        div {
            class: "msglist-search",
            input {
                id: "qsl-search-input",
                class: "msglist-search-input",
                r#type: "search",
                placeholder: "Search mail (try is:unread, from:alice, before:2026-01-01)",
                value: "{value}",
                autocomplete: "off",
                spellcheck: "false",
                oninput: move |e| search_query.set(e.value()),
                onkeydown: move |e: Event<KeyboardData>| {
                    if e.key().to_string() == "Escape" {
                        e.prevent_default();
                        search_query.set(String::new());
                        if let Some(window) = web_sys::window() {
                            if let Some(document) = window.document() {
                                if let Some(active) = document.active_element() {
                                    if let Ok(input) = active.dyn_into::<web_sys::HtmlInputElement>() {
                                        let _ = input.blur();
                                    }
                                }
                            }
                        }
                    }
                },
            }
            if has_value {
                button {
                    class: "msglist-search-clear",
                    r#type: "button",
                    title: "Clear search (Esc)",
                    onclick: move |_| search_query.set(String::new()),
                    "×"
                }
            }
        }
    }
}

/// Search results pane. Calls `messages_search` for the current
/// `query` (parsed Gmail-style on the backend) and renders the
/// matching headers as `MessageRowV2`s — same row component the
/// folder / unified inbox lists use, so selection / multi-select /
/// styling all behave identically.
#[component]
fn SearchResults(
    query: String,
    selection: Signal<Selection>,
    bulk_selected: Signal<HashSet<MessageId>>,
    account_filter: Signal<Option<AccountId>>,
    visible_messages: Signal<Vec<MessageId>>,
) -> Element {
    let q_for_fetch = query.clone();
    let page = use_resource(use_reactive!(|q_for_fetch| async move {
        invoke::<MessagePage>(
            "messages_search",
            serde_json::json!({
                "input": { "query": q_for_fetch, "limit": 100, "offset": 0 },
            }),
        )
        .await
    }));

    // Publish the rendered ids for `j` / `k` navigation. Has to live
    // in a `use_effect` — writing to a signal during render panics
    // in Dioxus 0.7. Re-runs on `page` or `account_filter` changes.
    {
        let mut visible_messages = visible_messages;
        use_effect(move || {
            let read = page.read_unchecked();
            let Some(Ok(MessagePage { messages, .. })) = read.as_ref() else {
                visible_messages.set(Vec::new());
                return;
            };
            let filter = account_filter.read().clone();
            let ids: Vec<MessageId> = match filter {
                Some(ref id) => messages
                    .iter()
                    .filter(|m| m.account_id == *id)
                    .map(|m| m.id.clone())
                    .collect(),
                None => messages.iter().map(|m| m.id.clone()).collect(),
            };
            visible_messages.set(ids);
        });
    }

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { class: "msglist-empty", "Searching…" } },
            Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => {
                // Account filter is applied client-side — `messages_search`
                // is account-agnostic so we can scope without a re-query.
                let filter = account_filter.read().clone();
                let filtered: Vec<_> = match filter {
                    Some(ref id) => messages.iter().filter(|m| m.account_id == *id).cloned().collect(),
                    None => messages.clone(),
                };
                let shown = filtered.len() as u32;
                rsx! {
                    MessageListHeader {
                        title: format!("Search: {query}"),
                        shown,
                        total: *total_count,
                        unread: *unread_count,
                    }
                    div {
                        class: "msglist-scroll",
                        if filtered.is_empty() {
                            p { class: "msglist-empty", "No matches." }
                        } else {
                            for m in filtered.into_iter() {
                                MessageRowV2 { msg: m, selection, bulk_selected }
                            }
                        }
                    }
                }
            },
        }
    }
}

/// Floating action bar that overlays the top of the message list when
/// at least one row is checked. The selection lives at the App root,
/// so checkboxes and the bar both follow folder switches; clearing
/// happens explicitly via the "Clear" button or after a successful
/// bulk action. All four IPC commands already accept arrays of ids,
/// so each handler is a one-shot invoke + sync_tick bump.
#[component]
fn BulkActionBar(bulk_selected: Signal<HashSet<MessageId>>, sync_tick: SyncTick) -> Element {
    let count = bulk_selected.read().len();
    let label = if count == 1 {
        "1 selected".to_string()
    } else {
        format!("{count} selected")
    };

    // Snapshot the ids on click — `bulk_selected` is a signal, so
    // reading it inside the spawned future would force the closure to
    // capture a Signal (fine, but unnecessary) and risk reading after
    // a concurrent toggle. Snapshotting keeps the bulk action atomic
    // against the in-flight selection state at click time.
    let snapshot_ids = move |bulk_selected: Signal<HashSet<MessageId>>| -> Vec<MessageId> {
        bulk_selected.read().iter().cloned().collect()
    };

    rsx! {
        div {
            class: "bulk-action-bar",
            span { class: "bulk-action-count", "{label}" }
            button {
                class: "bulk-action",
                r#type: "button",
                title: "Archive selected (move to All Mail / Archive)",
                onclick: {
                    let mut bulk_selected = bulk_selected;
                    let mut sync_tick = sync_tick;
                    move |_| {
                        let ids = snapshot_ids(bulk_selected);
                        if ids.is_empty() {
                            return;
                        }
                        spawn(async move {
                            let payload = serde_json::json!({ "input": { "ids": ids } });
                            if let Err(e) = invoke::<()>("messages_archive", payload).await {
                                web_sys_log(&format!("bulk messages_archive: {e}"));
                                return;
                            }
                            bulk_selected.with_mut(|s| s.clear());
                            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
                "Archive"
            }
            button {
                class: "bulk-action",
                r#type: "button",
                title: "Mark selected as read",
                onclick: {
                    let mut bulk_selected = bulk_selected;
                    let mut sync_tick = sync_tick;
                    move |_| {
                        let ids = snapshot_ids(bulk_selected);
                        if ids.is_empty() {
                            return;
                        }
                        spawn(async move {
                            let payload = serde_json::json!({
                                "input": { "ids": ids, "seen": true },
                            });
                            if let Err(e) = invoke::<()>("messages_mark_read", payload).await {
                                web_sys_log(&format!("bulk messages_mark_read: {e}"));
                                return;
                            }
                            bulk_selected.with_mut(|s| s.clear());
                            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
                "Mark read"
            }
            button {
                class: "bulk-action",
                r#type: "button",
                title: "Mark selected as unread",
                onclick: {
                    let mut bulk_selected = bulk_selected;
                    let mut sync_tick = sync_tick;
                    move |_| {
                        let ids = snapshot_ids(bulk_selected);
                        if ids.is_empty() {
                            return;
                        }
                        spawn(async move {
                            let payload = serde_json::json!({
                                "input": { "ids": ids, "seen": false },
                            });
                            if let Err(e) = invoke::<()>("messages_mark_read", payload).await {
                                web_sys_log(&format!("bulk messages_mark_read: {e}"));
                                return;
                            }
                            bulk_selected.with_mut(|s| s.clear());
                            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
                "Mark unread"
            }
            button {
                class: "bulk-action bulk-action-delete",
                r#type: "button",
                title: "Delete selected (move to Trash on Gmail; permanent on JMAP)",
                onclick: {
                    let mut bulk_selected = bulk_selected;
                    let mut sync_tick = sync_tick;
                    move |_| {
                        let ids = snapshot_ids(bulk_selected);
                        if ids.is_empty() {
                            return;
                        }
                        spawn(async move {
                            let payload = serde_json::json!({ "input": { "ids": ids } });
                            if let Err(e) = invoke::<()>("messages_delete", payload).await {
                                web_sys_log(&format!("bulk messages_delete: {e}"));
                                return;
                            }
                            bulk_selected.with_mut(|s| s.clear());
                            sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
                "Delete"
            }
            button {
                class: "bulk-action bulk-action-clear",
                r#type: "button",
                title: "Clear selection",
                onclick: {
                    let mut bulk_selected = bulk_selected;
                    move |_| bulk_selected.with_mut(|s| s.clear())
                },
                "Clear"
            }
        }
    }
}

#[component]
fn UnifiedMessageListV2(
    selection: Signal<Selection>,
    sync_tick: SyncTick,
    bulk_selected: Signal<HashSet<MessageId>>,
    account_filter: Signal<Option<AccountId>>,
    visible_messages: Signal<Vec<MessageId>>,
) -> Element {
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

    // Publish the rendered ids for `j` / `k` navigation. Has to live
    // in a `use_effect` — writing to a signal during render panics
    // in Dioxus 0.7. Re-runs on `page` or `account_filter` changes.
    {
        let mut visible_messages = visible_messages;
        use_effect(move || {
            let read = page.read_unchecked();
            let Some(Ok(MessagePage { messages, .. })) = read.as_ref() else {
                visible_messages.set(Vec::new());
                return;
            };
            let filter = account_filter.read().clone();
            let ids: Vec<MessageId> = match filter {
                Some(ref id) => messages
                    .iter()
                    .filter(|m| m.account_id == *id)
                    .map(|m| m.id.clone())
                    .collect(),
                None => messages.iter().map(|m| m.id.clone()).collect(),
            };
            visible_messages.set(ids);
        });
    }

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { class: "msglist-empty", "Loading…" } },
            Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => {
                // Account filter applies post-fetch — `messages_list_unified`
                // returns every account so a single chip flip doesn't pay
                // a re-query cost.
                let filter = account_filter.read().clone();
                let filtered: Vec<_> = match filter {
                    Some(ref id) => messages.iter().filter(|m| m.account_id == *id).cloned().collect(),
                    None => messages.clone(),
                };
                let shown = filtered.len() as u32;
                rsx! {
                    MessageListHeader {
                        title: "Unified Inbox".to_string(),
                        shown,
                        total: *total_count,
                        unread: *unread_count,
                    }
                    div {
                        class: "msglist-scroll",
                        if filtered.is_empty() {
                            p { class: "msglist-empty", "No messages." }
                        } else {
                            for m in filtered.into_iter() {
                                MessageRowV2 { msg: m, selection, bulk_selected }
                            }
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
    bulk_selected: Signal<HashSet<MessageId>>,
    visible_messages: Signal<Vec<MessageId>>,
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

    // Publish the rendered ids for `j` / `k` navigation. Lives in a
    // `use_effect` — writing to a signal during render panics in
    // Dioxus 0.7. Threads expose their head only; entering one
    // requires an explicit click.
    {
        let mut visible_messages = visible_messages;
        use_effect(move || {
            let read = page.read_unchecked();
            let Some(Ok(MessagePage { messages, .. })) = read.as_ref() else {
                visible_messages.set(Vec::new());
                return;
            };
            let ids: Vec<MessageId> = messages.iter().map(|m| m.id.clone()).collect();
            visible_messages.set(ids);
        });
    }

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { class: "msglist-empty", "Loading…" } },
            Some(Err(e)) => rsx! { p { class: "msglist-empty", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => {
                rsx! {
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
                            for item in crate::threading::group_by_thread(messages.clone()) {
                                match item {
                                    crate::threading::MessageListItem::Single(m) => rsx! {
                                        MessageRowV2 { msg: m, selection, bulk_selected }
                                    },
                                    crate::threading::MessageListItem::Thread { head, members } => rsx! {
                                        ThreadRow { head, members, selection, bulk_selected }
                                    },
                                }
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
fn MessageRowV2(
    msg: MessageHeaders,
    selection: Signal<Selection>,
    bulk_selected: Signal<HashSet<MessageId>>,
) -> Element {
    let is_selected = selection
        .read()
        .message
        .as_ref()
        .is_some_and(|m| m.0 == msg.id.0);
    let is_checked = bulk_selected.read().contains(&msg.id);
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
    let date = crate::format::format_relative_date(msg.date, chrono::Utc::now());
    let subject = if msg.subject.is_empty() {
        "(no subject)".to_string()
    } else {
        msg.subject.clone()
    };
    let snippet = msg.snippet.clone();
    let (flag_glyph, flag_class) = crate::format::flag_glyph(&msg.flags);
    let flag_div_class = format!("msg-row-flag {flag_class}");
    // Subject + preview render on a single line per ui-direction.md.
    let subject_line = if snippet.is_empty() {
        subject.clone()
    } else {
        format!("{subject} · {snippet}")
    };
    // `checked` is its own row state — the row stays a `selected` row
    // only when *opened* in the reader; bulk-checking just adds a
    // visual marker without taking over reader focus.
    let row_class = match (is_selected, is_checked, unread) {
        (true, _, true) => "msg-row selected unread",
        (true, _, false) => "msg-row selected",
        (false, true, true) => "msg-row checked unread",
        (false, true, false) => "msg-row checked",
        (false, false, true) => "msg-row unread",
        (false, false, false) => "msg-row",
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

    let mut context_menu = use_context::<Signal<Option<MessageContextMenu>>>();
    let id_for_ctx = msg.id.clone();
    let oncontextmenu = move |evt: Event<MouseData>| {
        evt.prevent_default();
        evt.stop_propagation();
        let coords = evt.client_coordinates();
        context_menu.set(Some(MessageContextMenu {
            x: coords.x,
            y: coords.y,
            message_id: id_for_ctx.clone(),
            is_unread: unread,
        }));
    };

    rsx! {
        div {
            class: row_class,
            onclick: {
                let mid = msg.id.clone();
                move |_| selection.write().message = Some(mid.clone())
            },
            ondoubleclick: ondoubleclick,
            oncontextmenu: oncontextmenu,
            // Checkbox sits where the avatar normally lives. Click is
            // stop-propagation'd so checking a row doesn't also open
            // the reader (and vice-versa: clicking elsewhere on the
            // row never toggles the box).
            div {
                class: "msg-row-check",
                onclick: {
                    let mid = msg.id.clone();
                    let mut bulk_selected = bulk_selected;
                    move |evt: Event<MouseData>| {
                        evt.stop_propagation();
                        let id = mid.clone();
                        bulk_selected.with_mut(|set| {
                            if !set.remove(&id) {
                                set.insert(id);
                            }
                        });
                    }
                },
                input {
                    r#type: "checkbox",
                    checked: is_checked,
                    // The wrapping div handles the click; the input
                    // itself is non-interactive so the row keeps a
                    // single source of truth for toggle behaviour.
                    onclick: move |evt: Event<MouseData>| evt.stop_propagation(),
                    readonly: true,
                }
            }
            div { class: "{flag_div_class}", "{flag_glyph}" }
            div { class: "msg-row-from", "{from_name}" }
            div { class: "msg-row-subject", "{subject_line}" }
            div { class: "msg-row-time", "{date}" }
        }
    }
}

/// Wrapper row rendered when [`crate::threading::group_by_thread`]
/// rolled two-or-more consecutive same-thread messages into one. The
/// row itself is a `MessageRowV2`-shaped header for the newest member
/// (`head`), augmented with a count badge and a chevron toggle.
/// Clicking the body selects the head; clicking the chevron expands
/// inline with the older members rendered indented below.
#[component]
fn ThreadRow(
    head: MessageHeaders,
    members: Vec<MessageHeaders>,
    selection: Signal<Selection>,
    bulk_selected: Signal<HashSet<MessageId>>,
) -> Element {
    let mut expanded = use_signal(|| false);
    let count = members.len();
    // "Unread" promotes to the parent row whenever any member is
    // unseen — matches Gmail and avoids surprising the user when the
    // collapsed row hides an unread reply.
    let any_unread = members.iter().any(|m| !m.flags.seen);

    let is_head_selected = selection
        .read()
        .message
        .as_ref()
        .is_some_and(|m| m.0 == head.id.0);
    // Thread "checked" means every member is in the bulk set. Toggling
    // the head's checkbox flips that all-or-nothing — bulk archive of
    // a thread therefore archives the whole conversation, not just
    // the head. Dragging individual members out of the set requires
    // expanding the thread.
    let all_checked =
        !members.is_empty() && members.iter().all(|m| bulk_selected.read().contains(&m.id));

    let from_addr = head.from.first();
    let from_name = from_addr
        .map(|a| {
            a.display_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| a.address.clone())
        })
        .unwrap_or_default();
    let date = crate::format::format_relative_date(head.date, chrono::Utc::now());
    let subject = if head.subject.is_empty() {
        "(no subject)".to_string()
    } else {
        head.subject.clone()
    };
    let snippet = head.snippet.clone();
    let (head_glyph, head_flag_class) = crate::format::flag_glyph(&head.flags);
    let head_flag_div_class = format!("msg-row-flag {head_flag_class}");
    let head_subject_line = if snippet.is_empty() {
        subject.clone()
    } else {
        format!("{subject} · {snippet}")
    };
    let is_expanded = expanded();
    let row_class = match (is_head_selected, all_checked, any_unread) {
        (true, _, true) => "msg-row thread-row selected unread",
        (true, _, false) => "msg-row thread-row selected",
        (false, true, true) => "msg-row thread-row checked unread",
        (false, true, false) => "msg-row thread-row checked",
        (false, false, true) => "msg-row thread-row unread",
        (false, false, false) => "msg-row thread-row",
    };
    let group_class = if is_expanded {
        "thread-group expanded"
    } else {
        "thread-group"
    };

    let id_for_popup = head.id.clone();
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

    let mut context_menu = use_context::<Signal<Option<MessageContextMenu>>>();
    let id_for_ctx = head.id.clone();
    let head_unread = !head.flags.seen;
    let oncontextmenu = move |evt: Event<MouseData>| {
        evt.prevent_default();
        evt.stop_propagation();
        let coords = evt.client_coordinates();
        context_menu.set(Some(MessageContextMenu {
            x: coords.x,
            y: coords.y,
            message_id: id_for_ctx.clone(),
            is_unread: head_unread,
        }));
    };

    rsx! {
        div {
            class: group_class,
            div {
                class: row_class,
                onclick: {
                    let mid = head.id.clone();
                    let mut selection = selection;
                    move |_| selection.write().message = Some(mid.clone())
                },
                ondoubleclick: ondoubleclick,
                oncontextmenu: oncontextmenu,
                div {
                    class: "msg-row-check",
                    onclick: {
                        let member_ids: Vec<MessageId> =
                            members.iter().map(|m| m.id.clone()).collect();
                        let mut bulk_selected = bulk_selected;
                        move |evt: Event<MouseData>| {
                            evt.stop_propagation();
                            bulk_selected.with_mut(|set| {
                                if all_checked {
                                    for id in &member_ids {
                                        set.remove(id);
                                    }
                                } else {
                                    for id in &member_ids {
                                        set.insert(id.clone());
                                    }
                                }
                            });
                        }
                    },
                    input {
                        r#type: "checkbox",
                        checked: all_checked,
                        onclick: move |evt: Event<MouseData>| evt.stop_propagation(),
                        readonly: true,
                    }
                }
                div { class: "{head_flag_div_class}", "{head_glyph}" }
                div {
                    class: "msg-row-from",
                    "{from_name}"
                    span {
                        class: "thread-count",
                        title: "{count} messages in thread",
                        "{count}"
                    }
                }
                div { class: "msg-row-subject", "{head_subject_line}" }
                div {
                    class: "msg-row-time",
                    "{date}"
                    button {
                        class: "thread-toggle",
                        r#type: "button",
                        title: if is_expanded { "Collapse thread" } else { "Expand thread" },
                        onclick: move |evt: Event<MouseData>| {
                            evt.stop_propagation();
                            let v = expanded();
                            expanded.set(!v);
                        },
                        if is_expanded { "▾" } else { "▸" }
                    }
                }
            }
            if is_expanded {
                div {
                    class: "thread-members",
                    // Skip the head — already rendered above. Members
                    // are in DateDesc order (newest first); rendering
                    // them as-is makes "click to select" feel like
                    // walking the conversation backwards in time.
                    for m in members.iter().skip(1).cloned() {
                        MessageRowV2 { msg: m, selection, bulk_selected }
                    }
                }
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
                Some(id) => rsx! { ReaderV2 { id, selection, sync_tick, compose } },
            }
        }
    }
}

#[component]
fn ReaderV2(
    id: MessageId,
    selection: Signal<Selection>,
    sync_tick: SyncTick,
    compose: Signal<Option<ComposeState>>,
) -> Element {
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
                    // Toolbar above the header — keyboard hints replace
                    // the old pill action buttons (ui-direction.md §
                    // "Message view"). Clicking still triggers the
                    // action; the visible `[k]` reads as the canonical
                    // shortcut so the chrome reinforces the keyboard
                    // language instead of competing with it.
                    div {
                        class: "reader-toolbar",
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: "Reply (r)",
                            onclick: {
                                let r = rendered.clone();
                                move |_| open_reply(r.clone(), compose, ReplyKind::Reply)
                            },
                            span { class: "reader-hint-key", "[r]" }
                            " reply"
                        }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: "Reply to all (a)",
                            onclick: {
                                let r = rendered.clone();
                                move |_| open_reply(r.clone(), compose, ReplyKind::ReplyAll)
                            },
                            span { class: "reader-hint-key", "[a]" }
                            " reply-all"
                        }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: "Forward (f)",
                            onclick: {
                                let r = rendered.clone();
                                move |_| open_reply(r.clone(), compose, ReplyKind::Forward)
                            },
                            span { class: "reader-hint-key", "[f]" }
                            " forward"
                        }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: "Archive (e)",
                            onclick: {
                                let id = rendered.headers.id.clone();
                                let mut selection = selection;
                                let mut sync_tick = sync_tick;
                                move |_| {
                                    let id = id.clone();
                                    spawn(async move {
                                        let payload = serde_json::json!({
                                            "input": { "ids": [id.clone()] }
                                        });
                                        if let Err(e) =
                                            invoke::<()>("messages_archive", payload).await
                                        {
                                            web_sys_log(&format!("messages_archive: {e}"));
                                            return;
                                        }
                                        selection.with_mut(|sel| sel.message = None);
                                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                                    });
                                }
                            },
                            span { class: "reader-hint-key", "[e]" }
                            " archive"
                        }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: if rendered.headers.flags.flagged { "Unflag (s)" } else { "Flag (s)" },
                            onclick: {
                                let id = rendered.headers.id.clone();
                                let next_flagged = !rendered.headers.flags.flagged;
                                let mut sync_tick = sync_tick;
                                move |_| {
                                    let id = id.clone();
                                    spawn(async move {
                                        let payload = serde_json::json!({
                                            "input": {
                                                "ids": [id.clone()],
                                                "flagged": next_flagged,
                                            }
                                        });
                                        if let Err(e) =
                                            invoke::<()>("messages_flag", payload).await
                                        {
                                            web_sys_log(&format!("messages_flag: {e}"));
                                            return;
                                        }
                                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                                    });
                                }
                            },
                            span { class: "reader-hint-key", "[s]" }
                            if rendered.headers.flags.flagged { " unflag" } else { " flag" }
                        }
                        span { class: "reader-hint-spacer" }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: if rendered.headers.flags.seen { "Mark unread (u)" } else { "Mark read (u)" },
                            onclick: {
                                let id = rendered.headers.id.clone();
                                let next_seen = !rendered.headers.flags.seen;
                                let mut sync_tick = sync_tick;
                                move |_| {
                                    let id = id.clone();
                                    spawn(async move {
                                        let payload = serde_json::json!({
                                            "input": {
                                                "ids": [id.clone()],
                                                "seen": next_seen,
                                            }
                                        });
                                        if let Err(e) =
                                            invoke::<()>("messages_mark_read", payload).await
                                        {
                                            web_sys_log(&format!("messages_mark_read: {e}"));
                                            return;
                                        }
                                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                                    });
                                }
                            },
                            span { class: "reader-hint-key", "[u]" }
                            if rendered.headers.flags.seen { " unread" } else { " read" }
                        }
                        button {
                            class: "reader-hint",
                            r#type: "button",
                            title: "Delete (#)",
                            onclick: {
                                let id = rendered.headers.id.clone();
                                let mut selection = selection;
                                let mut sync_tick = sync_tick;
                                move |_| {
                                    let id = id.clone();
                                    spawn(async move {
                                        let payload = serde_json::json!({
                                            "input": { "ids": [id.clone()] }
                                        });
                                        if let Err(e) =
                                            invoke::<()>("messages_delete", payload).await
                                        {
                                            web_sys_log(&format!("messages_delete: {e}"));
                                            return;
                                        }
                                        selection.with_mut(|sel| sel.message = None);
                                        sync_tick.with_mut(|t| *t = t.wrapping_add(1));
                                    });
                                }
                            },
                            span { class: "reader-hint-key", "[#]" }
                            " delete"
                        }
                    }
                    div {
                        class: "reader-header-block",
                        h1 { class: "reader-subject", "{subject}" }
                        div {
                            class: "reader-sender-card",
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
