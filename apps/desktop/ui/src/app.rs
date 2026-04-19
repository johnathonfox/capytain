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
//! over the `window.__TAURI__.core.invoke` bridge; HTML rendering via
//! Servo arrives in Week 6.

use capytain_ipc::{
    Account, AccountId, Folder, FolderId, MessageHeaders, MessageId, MessagePage, RenderedMessage,
    SortOrder,
};
use dioxus::prelude::*;
use serde::Serialize;
use wasm_bindgen::prelude::*;

// ---------- Tauri bridge ----------

#[wasm_bindgen(inline_js = r#"
    export async function coreInvoke(cmd, args) {
        return await window.__TAURI__.core.invoke(cmd, args);
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
        invoke::<RenderedMessage>("messages_get", serde_json::json!({ "id": id_for_fetch })).await
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
