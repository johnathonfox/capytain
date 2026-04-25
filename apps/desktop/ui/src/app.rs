// SPDX-License-Identifier: Apache-2.0

//! Root Dioxus component for the Capytain UI (wasm32-only).
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

use capytain_ipc::{
    Account, AccountId, Folder, FolderId, MessageHeaders, MessageId, MessagePage, RenderedMessage,
    SortOrder,
};
use dioxus::prelude::*;
use serde::Serialize;
use wasm_bindgen::prelude::*;

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
        const cbId = window.__TAURI_INTERNALS__.transformCallback(handler);
        return await window.__TAURI_INTERNALS__.invoke('plugin:event|listen', {
            event,
            target: { kind: 'Any' },
            handler: cbId,
        });
    }
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = coreInvoke)]
    async fn core_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = tauriListen)]
    async fn tauri_listen(event: &str, handler: &js_sys::Function) -> Result<JsValue, JsValue>;
}

/// Thin wrapper around the Tauri `invoke` bridge. Serializes `args` to
/// JSON, forwards to JS, and deserializes the return value into `T`.
pub(crate) async fn invoke<T>(cmd: &str, args: impl Serialize) -> Result<T, String>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let js_args = serde_wasm_bindgen::to_value(&args).map_err(|e| e.to_string())?;
    let js_ret = core_invoke(cmd, js_args)
        .await
        .map_err(|e| format!("{e:?}"))?;
    serde_wasm_bindgen::from_value(js_ret).map_err(|e| e.to_string())
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

/// Per-folder revision counter. The desktop's sync engine emits a
/// `sync_event` over Tauri whenever a folder finishes a sync cycle;
/// the listener bumps this signal so message-list resources whose
/// `use_reactive!` deps include it auto-refetch.
///
/// One signal for the whole app means an event for folder A causes a
/// no-op refetch of folder B's list — at one cheap DB query that's a
/// fine tradeoff over per-folder bookkeeping. If it ever shows up in
/// profiles, swap to a `Signal<HashMap<(AccountId, FolderId), u64>>`
/// keyed by the changed folder.
pub type SyncTick = Signal<u64>;

// ---------- Root ----------

#[component]
pub fn App() -> Element {
    let selection = use_signal(Selection::default);
    let sync_tick: SyncTick = use_signal(|| 0u64);

    // Register the Tauri sync_event listener once at mount. The
    // closure leaks on purpose: it lives the lifetime of the app and
    // there's no useful unsubscribe point. wasm_bindgen `Closure` would
    // otherwise free its heap-allocated trampoline on Drop, leaving JS
    // with a dangling fn pointer.
    use_hook(move || {
        let mut tick = sync_tick;
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |_event: JsValue| {
            tick.with_mut(|n| *n = n.wrapping_add(1));
        });
        wasm_bindgen_futures::spawn_local(async move {
            let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
            if let Err(e) = tauri_listen("sync_event", func).await {
                web_sys_log(&format!("sync_event listen failed: {e:?}"));
            }
            // Hold the closure alive for the listener's lifetime. The
            // Box::leak is deliberate (see use_hook comment above).
            Box::leak(Box::new(cb));
        });
    });

    rsx! {
        main {
            class: "capytain-root",
            header {
                class: "topbar",
                h1 { "Capytain" }
                span {
                    class: "subtitle",
                    "Phase 0 Week 5 · text-only reader"
                }
                ServoTestButton {}
            }
            div {
                class: "panes",
                Sidebar { selection, sync_tick }
                MessageListPane { selection, sync_tick }
                ReaderPane { selection }
            }
        }
    }
}

/// Tiny `console.log` shim — saves pulling `web-sys` for one call.
fn web_sys_log(msg: &str) {
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
///    `capytain_mime::sanitize_email_html` (Phase 1 Week 7). This
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
fn compose_reader_html(rendered: &RenderedMessage) -> String {
    let subject = minimal_escape(&rendered.headers.subject);
    let from = rendered
        .headers
        .from
        .iter()
        .map(|a| {
            let name = a.display_name.as_deref().unwrap_or("");
            let addr = &a.address;
            if name.is_empty() {
                minimal_escape(addr)
            } else {
                format!("{} &lt;{}&gt;", minimal_escape(name), minimal_escape(addr))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let date = rendered.headers.date.to_rfc2822();

    let body_section = render_body_section(rendered);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    body {{ font: 14px/1.5 -apple-system, "Segoe UI", Roboto, sans-serif; color: #e6e8eb; background: #0f1115; margin: 0; padding: 1.25rem; }}
    h1.capytain-subject {{ font-size: 1.15rem; margin: 0 0 0.5rem; }}
    .capytain-meta {{ color: #8a929b; font-size: 0.85em; margin-bottom: 1rem; }}
    .capytain-body {{ color: inherit; }}
    .capytain-body pre {{ white-space: pre-wrap; word-wrap: break-word; margin: 0; font: inherit; }}
    .capytain-body a {{ color: #74b4ff; }}
  </style>
</head>
<body>
  <h1 class="capytain-subject">{subject}</h1>
  <div class="capytain-meta">From: {from} · {date}</div>
  <div class="capytain-body">{body_section}</div>
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
    // 3. "Nothing to show" hint.
    "<em>No body cached locally. Run <code>mailcli sync</code> to fetch.</em>".to_string()
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

// ---------- Phase 0 Week 6 validation helper ----------

/// Temporary Phase 0 button in the topbar that fires `reader_render`
/// with a fixed diagnostic document. Useful when no message is
/// selected (empty inbox, no accounts configured) to confirm the
/// Servo reader pane is alive. Removed in Phase 1 once the real
/// `Reader` component's auto-trigger covers the common path.
#[component]
fn ServoTestButton() -> Element {
    // In-flight flag: disable the button while an invoke is pending
    // so a single click can't spam `reader_render` through multiple
    // synthetic fires (observed during phase-0 headless probing —
    // `reader_render` logged ~6 times per physical click, suspected
    // focus/auto-activation in the webview's event path). Also
    // surfaces the call as user feedback — the button reads
    // "Rendering…" while the command is in flight.
    let mut in_flight = use_signal(|| false);

    let trigger = move |evt: Event<MouseData>| {
        // `stop_propagation` + `prevent_default` cover the two easy
        // paths for duplicate fires: event bubbling up to an ancestor
        // with its own click handler, and any latent form-submit
        // semantics the webview might layer on top of <button>.
        evt.stop_propagation();
        evt.prevent_default();

        if *in_flight.read() {
            return;
        }
        in_flight.set(true);

        spawn(async move {
            // Errors here are only observable from the Tauri-side
            // backend log (`tracing::warn!("reader_render: ...")`);
            // the UI crate has no logging surface, so swallow the
            // result — this is a maintainer-run validation button,
            // not a production flow.
            const TEST_HTML: &str = r#"<!DOCTYPE html>
<html><body style="font:14px/1.5 -apple-system,sans-serif;color:#e6e8eb;background:#0f1115;padding:1rem;">
<h1>Hello from Servo</h1>
<p>Phase 0 validation render. Select a message to render its body instead.</p>
<p><a href="https://example.com/capytain-link-click-test">Link-click callback test</a></p>
</body></html>"#;
            let _ = invoke::<()>(
                "reader_render",
                serde_json::json!({ "input": { "html": TEST_HTML } }),
            )
            .await;
            in_flight.set(false);
        });
    };

    rsx! {
        button {
            class: "servo-test",
            // `type="button"` so the webview never treats this as a
            // form submit (the default `type` is `submit` inside a
            // form context; we aren't in one here, but belt +
            // suspenders).
            r#type: "button",
            disabled: *in_flight.read(),
            onclick: trigger,
            if *in_flight.read() {
                "Rendering…"
            } else {
                "Render test page in Servo"
            }
        }
    }
}

// ---------- Sidebar: accounts + folders ----------

#[component]
fn Sidebar(selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let accounts = use_resource(|| async { invoke::<Vec<Account>>("accounts_list", ()).await });

    rsx! {
        aside {
            class: "sidebar",
            h2 { "Accounts" }
            match &*accounts.read_unchecked() {
                None => rsx! { p { "Loading…" } },
                Some(Err(e)) => rsx! { p { class: "error", "Error: {e}" } },
                Some(Ok(list)) if list.is_empty() => rsx! {
                    p {
                        class: "hint",
                        "No accounts yet. Run "
                        code { "mailcli auth add gmail <email>" }
                        "."
                    }
                },
                Some(Ok(list)) => rsx! {
                    ul {
                        class: "accounts",
                        UnifiedInboxRow { selection }
                        for a in list.iter().cloned() {
                            AccountRow { account: a, selection, sync_tick }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn UnifiedInboxRow(selection: Signal<Selection>) -> Element {
    let is_selected = selection.read().unified;
    rsx! {
        li {
            class: if is_selected { "unified-inbox selected" } else { "unified-inbox" },
            onclick: move |_| {
                let mut sel = selection.write();
                sel.unified = true;
                sel.account = None;
                sel.folder = None;
                sel.message = None;
            },
            strong { "Unified Inbox" }
        }
    }
}

#[component]
fn AccountRow(account: Account, selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let id = account.id.clone();
    let tick_value = sync_tick();
    let folders = use_resource(use_reactive!(|id, tick_value| async move {
        let _ = tick_value; // dep-only; refetches when sync_event fires
        invoke::<Vec<Folder>>("folders_list", serde_json::json!({ "account": id })).await
    }));

    let is_selected = selection
        .read()
        .account
        .as_ref()
        .is_some_and(|a| a.0 == account.id.0);

    rsx! {
        li {
            class: "account",
            div {
                class: if is_selected { "account-head selected" } else { "account-head" },
                onclick: {
                    let account_id = account.id.clone();
                    move |_| {
                        let mut sel = selection.write();
                        sel.account = Some(account_id.clone());
                        sel.folder = None;
                        sel.message = None;
                        sel.unified = false;
                    }
                },
                strong { "{account.display_name}" }
                span { class: "email", "{account.email_address}" }
            }
            if is_selected {
                match &*folders.read_unchecked() {
                    None => rsx! { p { class: "folder-loading", "Loading folders…" } },
                    Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p { class: "hint", "No folders synced yet." }
                    },
                    Some(Ok(list)) => rsx! {
                        ul {
                            class: "folders",
                            for f in list.iter().cloned() {
                                FolderRow { folder: f, selection }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn FolderRow(folder: Folder, selection: Signal<Selection>) -> Element {
    let is_selected = selection
        .read()
        .folder
        .as_ref()
        .is_some_and(|f| f.0 == folder.id.0);

    rsx! {
        li {
            class: if is_selected { "folder selected" } else { "folder" },
            onclick: {
                let folder_id = folder.id.clone();
                move |_| {
                    let mut sel = selection.write();
                    sel.folder = Some(folder_id.clone());
                    sel.message = None;
                    sel.unified = false;
                }
            },
            span { class: "folder-name", "{folder.name}" }
            if folder.unread_count > 0 {
                span { class: "unread-badge", "{folder.unread_count}" }
            }
        }
    }
}

// ---------- Middle pane: message list ----------

#[component]
fn MessageListPane(selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let unified = selection.read().unified;
    let folder_id = selection.read().folder.clone();

    rsx! {
        section {
            class: "message-list",
            if unified {
                UnifiedMessageList { selection, sync_tick }
            } else {
                match folder_id {
                    None => rsx! { p { class: "hint", "Select a folder to see its messages." } },
                    Some(fid) => rsx! { MessageList { folder: fid, selection, sync_tick } },
                }
            }
        }
    }
}

#[component]
fn UnifiedMessageList(selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let tick_value = sync_tick();
    let page = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<MessagePage>(
            "messages_list_unified",
            serde_json::json!({
                "limit": 50,
                "offset": 0,
                "sort": SortOrder::DateDesc,
            }),
        )
        .await
    }));

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { "Loading unified inbox…" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => rsx! {
                header {
                    class: "messages-head",
                    strong { "Unified Inbox · {messages.len()} / {total_count}" }
                    span { class: "unread", "{unread_count} unread" }
                }
                ul {
                    class: "messages",
                    for m in messages.iter().cloned() {
                        MessageRow { msg: m, selection }
                    }
                }
            },
        }
    }
}

#[component]
fn MessageList(folder: FolderId, selection: Signal<Selection>, sync_tick: SyncTick) -> Element {
    let folder_for_fetch = folder.clone();
    // Including `tick_value` in the reactive deps means every
    // `sync_event` from the desktop engine triggers a fresh
    // `messages_list` invoke — refetching the local DB cache so any
    // newly-synced rows surface immediately.
    let tick_value = sync_tick();
    let page = use_resource(use_reactive!(|folder_for_fetch, tick_value| async move {
        let _ = tick_value; // dep-only; not used in body
        invoke::<MessagePage>(
            "messages_list",
            serde_json::json!({
                "folder": folder_for_fetch,
                "limit": 50,
                "offset": 0,
                "sort": SortOrder::DateDesc,
            }),
        )
        .await
    }));

    rsx! {
        match &*page.read_unchecked() {
            None => rsx! { p { "Loading messages…" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
            Some(Ok(MessagePage { messages, total_count, unread_count })) => rsx! {
                header {
                    class: "messages-head",
                    strong { "{messages.len()} / {total_count}" }
                    span { class: "unread", "{unread_count} unread" }
                }
                ul {
                    class: "messages",
                    for m in messages.iter().cloned() {
                        MessageRow { msg: m, selection }
                    }
                }
            },
        }
    }
}

#[component]
fn MessageRow(msg: MessageHeaders, selection: Signal<Selection>) -> Element {
    let is_selected = selection
        .read()
        .message
        .as_ref()
        .is_some_and(|m| m.0 == msg.id.0);
    let from_display = msg
        .from
        .first()
        .map(|addr| {
            addr.display_name
                .clone()
                .unwrap_or_else(|| addr.address.clone())
        })
        .unwrap_or_default();
    let row_class = if is_selected {
        "message selected"
    } else if msg.flags.seen {
        "message"
    } else {
        "message unread"
    };

    rsx! {
        li {
            class: row_class,
            onclick: {
                let mid = msg.id.clone();
                move |_| selection.write().message = Some(mid.clone())
            },
            div { class: "from", "{from_display}" }
            div { class: "subject", "{msg.subject}" }
            div { class: "snippet", "{msg.snippet}" }
        }
    }
}

// ---------- Right pane: reader ----------

#[component]
fn ReaderPane(selection: Signal<Selection>) -> Element {
    let message_id = selection.read().message.clone();

    rsx! {
        section {
            class: "reader",
            match message_id {
                None => rsx! { p { class: "hint", "Select a message to read." } },
                Some(id) => rsx! { Reader { id } },
            }
        }
    }
}

#[component]
fn Reader(id: MessageId) -> Element {
    let id_for_fetch = id.clone();
    let msg = use_resource(use_reactive!(|id_for_fetch| async move {
        let rendered =
            invoke::<RenderedMessage>("messages_get", serde_json::json!({ "id": id_for_fetch }))
                .await?;
        // Hand the composed document to Servo so the right-hand
        // native reader pane renders in sync with this inline view.
        // Phase 0: naive headers + plaintext body wrapper — no
        // sanitization yet (plaintext is escaped by `minimal_escape`
        // before injection). Phase 1 replaces this with the
        // `sanitized_html` field from `RenderedMessage` once the
        // ammonia + adblock pipelines are live.
        let html = compose_reader_html(&rendered);
        let _ = invoke::<()>(
            "reader_render",
            serde_json::json!({ "input": { "html": html } }),
        )
        .await;
        Ok::<_, String>(rendered)
    }));

    rsx! {
        match &*msg.read_unchecked() {
            None => rsx! { p { "Loading…" } },
            Some(Err(e)) => rsx! { p { class: "error", "{e}" } },
            Some(Ok(rendered)) => rsx! {
                article {
                    class: "rendered-message",
                    header {
                        h2 { "{rendered.headers.subject}" }
                        div {
                            class: "from",
                            "From: "
                            for addr in rendered.headers.from.iter() {
                                span {
                                    { addr.display_name.clone().unwrap_or_default() }
                                    " <"
                                    { addr.address.clone() }
                                    "> "
                                }
                            }
                        }
                        div {
                            class: "date",
                            "Date: "
                            { rendered.headers.date.to_rfc2822() }
                        }
                    }
                    match (&rendered.body_text, &rendered.sanitized_html) {
                        (Some(text), _) => rsx! {
                            pre { class: "body-text", "{text}" }
                        },
                        (None, Some(_html)) => rsx! {
                            // HTML rendering lives in the Servo pane (Week 6). Until then,
                            // surface the fact that we have HTML but no renderer yet.
                            p { class: "hint",
                                "This message has only an HTML body. Servo rendering arrives in Phase 0 Week 6."
                            }
                        },
                        (None, None) => rsx! {
                            p { class: "hint", "No body yet — run "
                                code { "mailcli sync" }
                                " to fetch it."
                            }
                        },
                    }
                }
            },
        }
    }
}
