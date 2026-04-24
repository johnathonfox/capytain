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
#[wasm_bindgen(inline_js = r#"
    export async function coreInvoke(cmd, args) {
        return await window.__TAURI_INTERNALS__.invoke(cmd, args);
    }
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = coreInvoke)]
    async fn core_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub account: Option<AccountId>,
    pub folder: Option<FolderId>,
    pub message: Option<MessageId>,
}

// ---------- Root ----------

#[component]
pub fn App() -> Element {
    let selection = use_signal(Selection::default);

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
                Sidebar { selection }
                MessageListPane { selection }
                ReaderPane { selection }
            }
        }
    }
}

// ---------- Phase 0 reader-pane HTML composer ----------

/// Build the HTML document the Servo reader pane renders when a
/// message is selected. Phase 0 composes the document in the UI
/// from `RenderedMessage` fields so the Servo seam (`reader_render`)
/// can stay a pure "render this HTML" command; Phase 1 swaps this
/// for `rendered.sanitized_html` once the ammonia / adblock pipelines
/// populate it server-side.
///
/// Plaintext body content is passed through [`minimal_escape`] before
/// injection to prevent inline `<script>` or other HTML smuggling
/// via otherwise-innocent text/plain bodies. Headers (subject, from,
/// date) are already escaped the same way.
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
    let body = rendered
        .body_text
        .as_deref()
        .map(minimal_escape)
        .unwrap_or_else(|| {
            "<em>No plaintext body stored locally. Run <code>mailcli sync</code> to fetch.</em>"
                .to_string()
        });

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <style>
    body {{ font: 14px/1.5 -apple-system, "Segoe UI", Roboto, sans-serif; color: #e6e8eb; background: #0f1115; margin: 0; padding: 1.25rem; }}
    h1 {{ font-size: 1.15rem; margin: 0 0 0.5rem; }}
    .meta {{ color: #8a929b; font-size: 0.85em; margin-bottom: 1rem; }}
    pre {{ white-space: pre-wrap; word-wrap: break-word; margin: 0; font: inherit; }}
  </style>
</head>
<body>
  <h1>{subject}</h1>
  <div class="meta">From: {from} · {date}</div>
  <pre>{body}</pre>
</body>
</html>"#
    )
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
fn Sidebar(selection: Signal<Selection>) -> Element {
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
                        for a in list.iter().cloned() {
                            AccountRow { account: a, selection }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn AccountRow(account: Account, selection: Signal<Selection>) -> Element {
    let id = account.id.clone();
    let folders = use_resource(use_reactive!(|id| async move {
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
                        selection.write().account = Some(account_id.clone());
                        selection.write().folder = None;
                        selection.write().message = None;
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
                    selection.write().folder = Some(folder_id.clone());
                    selection.write().message = None;
                }
            },
            span { "{folder.name}" }
        }
    }
}

// ---------- Middle pane: message list ----------

#[component]
fn MessageListPane(selection: Signal<Selection>) -> Element {
    let folder_id = selection.read().folder.clone();

    rsx! {
        section {
            class: "message-list",
            match folder_id {
                None => rsx! { p { class: "hint", "Select a folder to see its messages." } },
                Some(fid) => rsx! { MessageList { folder: fid, selection } },
            }
        }
    }
}

#[component]
fn MessageList(folder: FolderId, selection: Signal<Selection>) -> Element {
    let folder_for_fetch = folder.clone();
    let page = use_resource(use_reactive!(|folder_for_fetch| async move {
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
