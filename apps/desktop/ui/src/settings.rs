// SPDX-License-Identifier: Apache-2.0

//! Settings window — mounted when `window.__QSL_VIEW__ === "settings"`.
//!
//! Five tabs:
//!
//!   - **Accounts** — list configured accounts; each row exposes
//!     display-name + signature edits, the per-account notification
//!     toggle, and a "Remove" button. Header has "Add account",
//!     stubbed pending PR-O1.
//!   - **Appearance** — theme (system/dark/light) and density
//!     (compact/comfortable). Both persist to the
//!     `app_settings_v1` k/v table; the main window re-reads on
//!     focus to pick up changes.
//!   - **Notifications** — master enable/disable. Per-account is
//!     edited from the Accounts tab.
//!   - **Shortcuts** — read-only viewer of the keymap from
//!     `crate::keyboard`.
//!   - **Privacy** — "Always load remote images" master toggle. The
//!     existing per-sender opt-in stays available from the reader's
//!     banner.

use dioxus::prelude::*;
use qsl_ipc::{Account, AccountId, Folder, FolderId, FolderRole, HistorySyncStatus};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use crate::app::{invoke, tauri_listen, web_sys_log, TAILWIND_CSS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Accounts,
    Appearance,
    Compose,
    Notifications,
    Shortcuts,
    Privacy,
}

impl Tab {
    const ALL: &'static [Tab] = &[
        Tab::Accounts,
        Tab::Appearance,
        Tab::Compose,
        Tab::Notifications,
        Tab::Shortcuts,
        Tab::Privacy,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Accounts => "Accounts",
            Tab::Appearance => "Appearance",
            Tab::Compose => "Compose",
            Tab::Notifications => "Notifications",
            Tab::Shortcuts => "Shortcuts",
            Tab::Privacy => "Privacy",
        }
    }
}

/// Bumped each time a mutation completes so child resources refetch.
type SettingsTick = Signal<u64>;

#[component]
pub fn SettingsApp() -> Element {
    // Match `<html data-theme=…>` to the user's persisted preference
    // and subscribe to live changes — otherwise the Settings window
    // (which runs as its own webview with its own document) paints
    // dark regardless of what the user picked, making the Theme
    // radio look broken even when the main window flipped correctly.
    crate::app::use_appearance_hooks();
    let active_tab = use_signal(|| Tab::Accounts);
    let tick: SettingsTick = use_signal(|| 0u64);
    rsx! {
        document::Stylesheet { href: TAILWIND_CSS }
        div {
            class: "settings-shell",
            aside {
                class: "settings-tabs",
                h1 { class: "settings-title", "Settings" }
                ul {
                    class: "settings-tab-list",
                    for tab in Tab::ALL.iter().copied() {
                        SettingsTabButton { active: active_tab, tab }
                    }
                }
            }
            section {
                class: "settings-content",
                match *active_tab.read() {
                    Tab::Accounts => rsx! { AccountsTab { tick } },
                    Tab::Appearance => rsx! { AppearanceTab { tick } },
                    Tab::Compose => rsx! { ComposeTab { tick } },
                    Tab::Notifications => rsx! { NotificationsTab { tick } },
                    Tab::Shortcuts => rsx! { ShortcutsTab {} },
                    Tab::Privacy => rsx! { PrivacyTab { tick } },
                }
            }
        }
    }
}

#[component]
fn SettingsTabButton(mut active: Signal<Tab>, tab: Tab) -> Element {
    let is_active = *active.read() == tab;
    let class = if is_active {
        "settings-tab-button settings-tab-button-active"
    } else {
        "settings-tab-button"
    };
    rsx! {
        li {
            button {
                class: "{class}",
                r#type: "button",
                onclick: move |_| active.set(tab),
                "{tab.label()}"
            }
        }
    }
}

// ---------- Accounts ----------

#[component]
fn AccountsTab(mut tick: SettingsTick) -> Element {
    let tick_value = *tick.read();
    let accounts = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<Vec<Account>>("accounts_list", serde_json::json!({})).await
    }));

    // Listen for the host's `accounts_changed` event so an OAuth add
    // completed in the popup window propagates back here without
    // requiring the user to close + reopen Settings. The Remove
    // button bumps `tick` directly already; the event covers the
    // cross-window add path.
    use_hook(move || {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |_payload: JsValue| {
            tick.with_mut(|t| *t = t.wrapping_add(1));
        });
        wasm_bindgen_futures::spawn_local(async move {
            let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
            if let Err(e) = tauri_listen("accounts_changed", func).await {
                web_sys_log(&format!("settings accounts_changed listen failed: {e:?}"));
            }
            Box::leak(Box::new(cb));
        });
    });
    rsx! {
        div {
            class: "settings-section",
            div {
                class: "settings-section-header",
                h2 { class: "settings-section-title", "Accounts" }
                button {
                    class: "settings-button",
                    r#type: "button",
                    title: "Open the add-account window",
                    onclick: |_| {
                        spawn(async {
                            if let Err(e) = invoke::<()>(
                                "oauth_add_open",
                                serde_json::json!({}),
                            ).await {
                                web_sys_log(&format!("oauth_add_open: {e}"));
                            }
                        });
                    },
                    "+ Add account"
                }
            }
            match &*accounts.read_unchecked() {
                None => rsx! { p { class: "settings-empty", "Loading accounts…" } },
                Some(Err(e)) => rsx! { p { class: "settings-empty settings-error", "{e}" } },
                Some(Ok(list)) if list.is_empty() => rsx! {
                    p { class: "settings-empty", "No accounts configured." }
                },
                Some(Ok(list)) => rsx! {
                    ul {
                        class: "settings-account-list",
                        for acct in list.iter().cloned() {
                            AccountRow { account: acct, tick }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn AccountRow(account: Account, tick: SettingsTick) -> Element {
    let id = account.id.clone();
    let mut display_name = use_signal(|| account.display_name.clone());
    let mut signature = use_signal(|| account.signature.clone().unwrap_or_default());
    let initial_notify = account.notify_enabled;
    rsx! {
        li {
            class: "settings-account-row",
            div {
                class: "settings-account-summary",
                div {
                    class: "settings-account-summary-text",
                    div { class: "settings-account-name", "{account.display_name}" }
                    div { class: "settings-account-email", "{account.email_address}" }
                }
                button {
                    class: "settings-button settings-button-danger",
                    r#type: "button",
                    title: "Remove this account from QSL",
                    onclick: {
                        let id = id.clone();
                        let mut tick = tick;
                        move |_| {
                            let id = id.clone();
                            spawn(async move {
                                let payload = serde_json::json!({
                                    "input": { "id": id }
                                });
                                if let Err(e) = invoke::<()>("accounts_remove", payload).await {
                                    web_sys_log(&format!("accounts_remove: {e}"));
                                    return;
                                }
                                tick.with_mut(|t| *t = t.wrapping_add(1));
                            });
                        }
                    },
                    "Remove"
                }
            }
            div {
                class: "settings-field",
                label { class: "settings-label", "Display name" }
                div {
                    class: "settings-field-row",
                    input {
                        class: "settings-input",
                        r#type: "text",
                        value: "{display_name}",
                        oninput: move |e| display_name.set(e.value()),
                    }
                    button {
                        class: "settings-button",
                        r#type: "button",
                        onclick: {
                            let id = id.clone();
                            let mut tick = tick;
                            move |_| {
                                let id = id.clone();
                                let name = display_name.read().clone();
                                spawn(async move {
                                    let payload = serde_json::json!({
                                        "input": { "id": id, "display_name": name }
                                    });
                                    if let Err(e) = invoke::<()>("accounts_set_display_name", payload).await {
                                        web_sys_log(&format!("accounts_set_display_name: {e}"));
                                        return;
                                    }
                                    tick.with_mut(|t| *t = t.wrapping_add(1));
                                });
                            }
                        },
                        "Save"
                    }
                }
            }
            div {
                class: "settings-field",
                label { class: "settings-label", "Signature" }
                textarea {
                    class: "settings-textarea",
                    rows: "4",
                    value: "{signature}",
                    oninput: move |e| signature.set(e.value()),
                }
                div {
                    class: "settings-field-row settings-field-row-end",
                    button {
                        class: "settings-button",
                        r#type: "button",
                        onclick: {
                            let id = id.clone();
                            let mut tick = tick;
                            move |_| {
                                let id = id.clone();
                                let sig = signature.read().clone();
                                let payload_sig = if sig.is_empty() {
                                    None
                                } else {
                                    Some(sig)
                                };
                                spawn(async move {
                                    let payload = serde_json::json!({
                                        "input": { "id": id, "signature": payload_sig }
                                    });
                                    if let Err(e) = invoke::<()>("accounts_set_signature", payload).await {
                                        web_sys_log(&format!("accounts_set_signature: {e}"));
                                        return;
                                    }
                                    tick.with_mut(|t| *t = t.wrapping_add(1));
                                });
                            }
                        },
                        "Save signature"
                    }
                }
            }
            div {
                class: "settings-field",
                label {
                    class: "settings-checkbox-label",
                    NotifyToggle { account_id: id.clone(), initial: initial_notify, tick }
                    span { "Show desktop notifications for new mail on this account" }
                }
            }
            AccountHistorySync { account_id: id.clone() }
        }
    }
}

/// Per-account "Pull full mail history" panel. Sits at the bottom of
/// every AccountRow and surfaces the state of the
/// `history_sync_state` table for that account.
#[component]
fn AccountHistorySync(account_id: AccountId) -> Element {
    // Local refresh tick — bumped on user actions and on every
    // `sync_event` of kind `history_sync_progress` for this account.
    let mut tick = use_signal(|| 0u64);

    // Subscribe to the engine's sync event stream so progress
    // updates re-render this panel without polling. The closure
    // filters on the event's `kind` + `account` so unrelated events
    // (FolderSynced, other accounts) don't trigger needless rerenders.
    {
        let acct_for_listen = account_id.0.clone();
        use_hook(move || {
            let acct = acct_for_listen.clone();
            let cb = Closure::<dyn FnMut(JsValue)>::new(move |payload: JsValue| {
                let value: serde_json::Value = match serde_wasm_bindgen::from_value(payload) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if kind != "history_sync_progress" {
                    return;
                }
                let evt_account = value.get("account").and_then(|v| v.as_str()).unwrap_or("");
                if evt_account != acct {
                    return;
                }
                tick.with_mut(|t| *t = t.wrapping_add(1));
            });
            wasm_bindgen_futures::spawn_local(async move {
                let func = cb.as_ref().unchecked_ref::<js_sys::Function>();
                if let Err(e) = tauri_listen("sync_event", func).await {
                    web_sys_log(&format!("settings sync_event listen failed: {e:?}"));
                }
                // Settings is a short-lived window; leaking the
                // closure here matches the app.rs pattern and keeps
                // the listener alive until the window is dropped.
                Box::leak(Box::new(cb));
            });
        });
    }

    let acct_for_rows = account_id.clone();
    let tick_value = *tick.read();
    let rows = use_resource(use_reactive!(|tick_value, acct_for_rows| async move {
        let _ = tick_value;
        invoke::<Vec<HistorySyncStatus>>(
            "history_sync_list",
            serde_json::json!({ "input": { "account": acct_for_rows } }),
        )
        .await
    }));

    let acct_for_folders = account_id.clone();
    let folders = use_resource(use_reactive!(|acct_for_folders| async move {
        invoke::<Vec<Folder>>(
            "folders_list",
            serde_json::json!({ "input": { "account": acct_for_folders } }),
        )
        .await
    }));

    rsx! {
        div {
            class: "settings-field",
            label { class: "settings-label", "Mail history" }
            div {
                class: "settings-history-sync",
                p {
                    class: "settings-history-sync-blurb",
                    "Pull every older message for a folder, in the background. ",
                    "Resumable — closing QSL while a pull is running picks up where you left off on next launch."
                }
                match (&*rows.read_unchecked(), &*folders.read_unchecked()) {
                    (None, _) | (_, None) => rsx! {
                        p { class: "settings-empty", "Loading…" }
                    },
                    (Some(Err(e)), _) => rsx! {
                        p { class: "settings-empty settings-error", "{e}" }
                    },
                    (_, Some(Err(e))) => rsx! {
                        p { class: "settings-empty settings-error", "{e}" }
                    },
                    (Some(Ok(rows)), Some(Ok(folders))) => rsx! {
                        HistorySyncStartRow {
                            account_id: account_id.clone(),
                            folders: folders.clone(),
                            existing_rows: rows.clone(),
                            tick,
                        }
                        if !rows.is_empty() {
                            ul {
                                class: "settings-history-sync-list",
                                for row in rows.iter().cloned() {
                                    HistorySyncRowView { row, tick }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Folder picker + start button. Only shows folders that don't
/// already have a history-sync row to avoid double-starting.
#[component]
fn HistorySyncStartRow(
    account_id: AccountId,
    folders: Vec<Folder>,
    existing_rows: Vec<HistorySyncStatus>,
    mut tick: Signal<u64>,
) -> Element {
    let already: std::collections::HashSet<String> = existing_rows
        .iter()
        .filter(|r| r.status == "running" || r.status == "pending" || r.status == "completed")
        .map(|r| r.folder.0.clone())
        .collect();

    // Default selection: the All Mail / Archive role if available,
    // else Inbox, else first folder. Skips folders that are already
    // running or completed (the user can re-pick those via the
    // per-row Resume / Restart buttons).
    let default_folder: Option<FolderId> = folders
        .iter()
        .find(|f| f.role == Some(FolderRole::All) && !already.contains(&f.id.0))
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.role == Some(FolderRole::Archive) && !already.contains(&f.id.0))
        })
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.role == Some(FolderRole::Inbox) && !already.contains(&f.id.0))
        })
        .or_else(|| folders.iter().find(|f| !already.contains(&f.id.0)))
        .map(|f| f.id.clone());

    let mut selected = use_signal(|| default_folder.clone());
    let cur_value = selected
        .read()
        .as_ref()
        .map(|f| f.0.clone())
        .unwrap_or_default();

    let candidates: Vec<&Folder> = folders
        .iter()
        .filter(|f| !already.contains(&f.id.0))
        .collect();

    if candidates.is_empty() {
        return rsx! {
            p {
                class: "settings-empty",
                "Every folder has already been pulled or is in progress."
            }
        };
    }

    rsx! {
        div {
            class: "settings-history-sync-start",
            select {
                class: "settings-input",
                value: "{cur_value}",
                onchange: move |evt: Event<FormData>| {
                    selected.set(Some(FolderId(evt.value())));
                },
                for f in candidates {
                    option {
                        value: "{f.id.0}",
                        "{folder_label(f)}"
                    }
                }
            }
            button {
                class: "settings-button",
                r#type: "button",
                onclick: {
                    let acct = account_id.clone();
                    move |_| {
                        let Some(folder) = selected.read().clone() else { return };
                        let acct = acct.clone();
                        spawn(async move {
                            if let Err(e) = invoke::<()>(
                                "history_sync_start",
                                serde_json::json!({
                                    "input": { "account": acct, "folder": folder }
                                }),
                            )
                            .await
                            {
                                web_sys_log(&format!("history_sync_start: {e}"));
                                return;
                            }
                            tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
                "Pull full history"
            }
        }
    }
}

/// One existing row's progress + action buttons.
#[component]
fn HistorySyncRowView(row: HistorySyncStatus, mut tick: Signal<u64>) -> Element {
    let percent = match (row.fetched, row.total_estimate) {
        (_, Some(0)) | (_, None) => None,
        (f, Some(total)) => {
            let pct = (f as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
            Some(pct)
        }
    };
    // "Completed with zero new messages" means the pager walked the
    // tail and found nothing not already in storage — the user's
    // ahead-of-bootstrap mail is fully synced. Render that as
    // "Already up to date" so the row doesn't read as a 0% pull.
    let already_up_to_date = row.status == "completed" && row.fetched == 0;
    let status_label = if already_up_to_date {
        "Already up to date"
    } else {
        match row.status.as_str() {
            "running" => "Running",
            "pending" => "Queued",
            "completed" => "Complete",
            "canceled" => "Canceled",
            "error" => "Error",
            other => other,
        }
    };
    // Hide the "0 / ~N (0%)" line for the up-to-date case (the
    // status label already conveys everything) and the queued case
    // (no fetch has happened yet, so the percentage is meaningless).
    let progress_text = if already_up_to_date || row.status == "pending" {
        String::new()
    } else {
        match (row.total_estimate, percent) {
            (Some(total), Some(p)) => format!("{} / ~{} ({:.0}%)", row.fetched, total, p),
            _ => format!("{} fetched", row.fetched),
        }
    };

    let acct_for_actions = row.account.clone();
    let folder_for_actions = row.folder.clone();
    let is_running = row.status == "running" || row.status == "pending";
    let is_resumable = row.status == "canceled" || row.status == "error";

    rsx! {
        li {
            class: "settings-history-sync-row",
            div {
                class: "settings-history-sync-row-info",
                div { class: "settings-history-sync-row-folder", "{row.folder_label}" }
                if progress_text.is_empty() {
                    div { class: "settings-history-sync-row-status", "{status_label}" }
                } else {
                    div { class: "settings-history-sync-row-status", "{status_label} · {progress_text}" }
                }
                if let Some(err) = &row.last_error {
                    div { class: "settings-history-sync-row-error", "{err}" }
                }
            }
            if let Some(p) = percent {
                if !already_up_to_date {
                    div {
                        class: "settings-history-sync-progress",
                        div {
                            class: "settings-history-sync-progress-fill",
                            style: "width: {p}%;",
                        }
                    }
                }
            }
            div {
                class: "settings-history-sync-row-actions",
                if is_running {
                    button {
                        class: "settings-button settings-button-danger",
                        r#type: "button",
                        onclick: {
                            let acct = acct_for_actions.clone();
                            let folder = folder_for_actions.clone();
                            move |_| {
                                let acct = acct.clone();
                                let folder = folder.clone();
                                spawn(async move {
                                    if let Err(e) = invoke::<()>(
                                        "history_sync_cancel",
                                        serde_json::json!({
                                            "input": { "account": acct, "folder": folder }
                                        }),
                                    )
                                    .await
                                    {
                                        web_sys_log(&format!("history_sync_cancel: {e}"));
                                        return;
                                    }
                                    tick.with_mut(|t| *t = t.wrapping_add(1));
                                });
                            }
                        },
                        "Cancel"
                    }
                } else if is_resumable {
                    button {
                        class: "settings-button",
                        r#type: "button",
                        onclick: {
                            let acct = acct_for_actions.clone();
                            let folder = folder_for_actions.clone();
                            move |_| {
                                let acct = acct.clone();
                                let folder = folder.clone();
                                spawn(async move {
                                    if let Err(e) = invoke::<()>(
                                        "history_sync_start",
                                        serde_json::json!({
                                            "input": { "account": acct, "folder": folder }
                                        }),
                                    )
                                    .await
                                    {
                                        web_sys_log(&format!("history_sync_start: {e}"));
                                        return;
                                    }
                                    tick.with_mut(|t| *t = t.wrapping_add(1));
                                });
                            }
                        },
                        "Resume"
                    }
                }
            }
        }
    }
}

fn folder_label(f: &Folder) -> String {
    if let Some(role) = &f.role {
        match role {
            FolderRole::All => return "All Mail".to_string(),
            FolderRole::Inbox => return "Inbox".to_string(),
            FolderRole::Archive => return "Archive".to_string(),
            FolderRole::Sent => return "Sent".to_string(),
            FolderRole::Drafts => return "Drafts".to_string(),
            FolderRole::Trash => return "Trash".to_string(),
            FolderRole::Spam => return "Spam".to_string(),
            _ => {}
        }
    }
    f.name.clone()
}

#[component]
fn NotifyToggle(account_id: AccountId, initial: bool, tick: SettingsTick) -> Element {
    let mut enabled = use_signal(|| initial);
    let is_enabled = *enabled.read();
    rsx! {
        input {
            class: "settings-checkbox",
            r#type: "checkbox",
            checked: is_enabled,
            onchange: move |e: Event<FormData>| {
                let next = matches!(e.value().as_str(), "true" | "on");
                enabled.set(next);
                let id = account_id.clone();
                let mut tick = tick;
                spawn(async move {
                    let payload = serde_json::json!({
                        "input": { "id": id, "enabled": next }
                    });
                    if let Err(err) = invoke::<()>("accounts_set_notify_enabled", payload).await {
                        web_sys_log(&format!("accounts_set_notify_enabled: {err}"));
                        return;
                    }
                    tick.with_mut(|t| *t = t.wrapping_add(1));
                });
            },
        }
    }
}

// ---------- Appearance ----------

const KEY_THEME: &str = "appearance.theme";
const KEY_DENSITY: &str = "appearance.density";
const KEY_NOTIFY_MASTER: &str = "notifications.master";
const KEY_REMOTE_IMAGES: &str = "privacy.remote_images_always";

#[component]
fn AppearanceTab(tick: SettingsTick) -> Element {
    rsx! {
        div {
            class: "settings-section",
            h2 { class: "settings-section-title", "Appearance" }
            SettingsRadioGroup {
                setting_key: KEY_THEME,
                label: "Theme",
                default_value: "system",
                options: vec![("system", "Match system"), ("dark", "Dark"), ("light", "Light")],
                tick,
            }
            SettingsRadioGroup {
                setting_key: KEY_DENSITY,
                label: "Density",
                default_value: "comfortable",
                options: vec![("comfortable", "Comfortable"), ("compact", "Compact")],
                tick,
            }
        }
    }
}

#[component]
fn SettingsRadioGroup(
    setting_key: &'static str,
    label: &'static str,
    default_value: &'static str,
    options: Vec<(&'static str, &'static str)>,
    tick: SettingsTick,
) -> Element {
    let key_for_fetch = setting_key.to_string();
    let tick_value = *tick.read();
    let current = use_resource(use_reactive!(|key_for_fetch, tick_value| async move {
        let _ = tick_value;
        invoke::<Option<String>>(
            "app_settings_get",
            serde_json::json!({ "input": { "key": key_for_fetch } }),
        )
        .await
    }));
    let current_value: String = match &*current.read_unchecked() {
        Some(Ok(Some(v))) => v.clone(),
        _ => default_value.to_string(),
    };
    rsx! {
        div {
            class: "settings-field",
            label { class: "settings-label", "{label}" }
            div {
                class: "settings-radio-group",
                for (value, display) in options.iter().cloned() {
                    SettingsRadioOption {
                        setting_key: setting_key.to_string(),
                        value: value.to_string(),
                        display: display.to_string(),
                        selected: current_value == value,
                        tick,
                    }
                }
            }
        }
    }
}

#[component]
fn SettingsRadioOption(
    setting_key: String,
    value: String,
    display: String,
    selected: bool,
    tick: SettingsTick,
) -> Element {
    rsx! {
        label {
            class: "settings-radio-label",
            input {
                r#type: "radio",
                name: "{setting_key}",
                // Pass a real `bool` so dioxus uses the IDL property
                // setter (`el.checked = bool`) rather than treating it
                // as an HTML attribute. Stringifying produces
                // `checked="false"`, which is still attribute-present
                // and renders as checked — leaving every option in the
                // group looking selected and the browser silently
                // picking one. See ARCH-2026-04-30.
                checked: selected,
                onchange: {
                    let key = setting_key.clone();
                    let value = value.clone();
                    let mut tick = tick;
                    move |_| {
                        let key = key.clone();
                        let value = value.clone();
                        spawn(async move {
                            let payload = serde_json::json!({
                                "input": { "key": key, "value": value }
                            });
                            if let Err(e) = invoke::<()>("app_settings_set", payload).await {
                                web_sys_log(&format!("app_settings_set: {e}"));
                                return;
                            }
                            tick.with_mut(|t| *t = t.wrapping_add(1));
                        });
                    }
                },
            }
            span { "{display}" }
        }
    }
}

// ---------- Compose ----------

/// `app_settings` key for an account's signature, e.g.
/// `compose.signature.<account_id>`. Free function so the compose
/// pane can reuse the exact same key when reading the value.
pub fn compose_signature_key(account_id: &AccountId) -> String {
    format!("compose.signature.{}", account_id.0)
}

/// Setting that toggles the undo-send window. Values: `"off"`,
/// `"5"`, `"10"`, `"30"` (seconds).
pub const KEY_UNDO_SEND: &str = "compose.undo_send";

#[component]
fn ComposeTab(tick: SettingsTick) -> Element {
    let tick_value = *tick.read();
    let accounts = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<Vec<Account>>("accounts_list", serde_json::json!({})).await
    }));
    rsx! {
        div {
            class: "settings-section",
            h2 { class: "settings-section-title", "Send" }
            SettingsRadioGroup {
                setting_key: KEY_UNDO_SEND,
                label: "Undo send",
                default_value: "off",
                options: vec![
                    ("off", "Off"),
                    ("5", "5 s"),
                    ("10", "10 s"),
                    ("30", "30 s"),
                ],
                tick,
            }
            p {
                class: "settings-note",
                "After Send, your message holds in a status-bar countdown. "
                "Press Esc within the window to cancel."
            }

            h2 { class: "settings-section-title", "Signatures" }
            p {
                class: "settings-note",
                "One signature per account, appended to a fresh compose body. "
                "Edit drafts in-place; existing drafts are not retroactively "
                "changed when you update a signature here."
            }
            match &*accounts.read_unchecked() {
                None => rsx! { p { class: "settings-note", "Loading accounts…" } },
                Some(Err(e)) => rsx! { p { class: "settings-note", "Error: {e}" } },
                Some(Ok(list)) if list.is_empty() => rsx! {
                    p {
                        class: "settings-note",
                        "Add an account first — signatures are per-identity."
                    }
                },
                Some(Ok(list)) => rsx! {
                    for a in list.iter().cloned() {
                        SignatureField {
                            account_id: a.id.clone(),
                            display: format!("{} — {}", a.display_name, a.email_address),
                            tick,
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn SignatureField(account_id: AccountId, display: String, tick: SettingsTick) -> Element {
    let key = compose_signature_key(&account_id);
    let key_for_fetch = key.clone();
    let tick_value = *tick.read();
    // Read current value via app_settings_get; default empty.
    let current = use_resource(use_reactive!(|key_for_fetch, tick_value| async move {
        let _ = tick_value;
        invoke::<Option<String>>(
            "app_settings_get",
            serde_json::json!({ "input": { "key": key_for_fetch } }),
        )
        .await
    }));
    let initial: String = match &*current.read_unchecked() {
        Some(Ok(Some(v))) => v.clone(),
        _ => String::new(),
    };
    // Local editor state — auto-saves on debounce (no per-keystroke
    // round-trip, no Save button to forget).
    let mut value = use_signal(|| initial.clone());
    // Re-sync local state whenever the resource resolves with a fresh
    // string (e.g. settings reopened after an external edit).
    use_effect(use_reactive!(|initial| {
        value.set(initial.clone());
    }));
    let mut tick_ref = tick;
    let key_for_save = key;
    rsx! {
        div {
            class: "settings-field settings-signature-row",
            label { class: "settings-label", "{display}" }
            textarea {
                class: "settings-signature-input",
                rows: "4",
                placeholder: "-- \nYour signature here",
                value: "{value}",
                oninput: move |evt: Event<FormData>| {
                    value.set(evt.value());
                },
                onblur: {
                    let key_for_save = key_for_save.clone();
                    move |_| {
                        let payload = serde_json::json!({
                            "input": {
                                "key": key_for_save,
                                "value": (*value.read()).clone(),
                            }
                        });
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Err(e) = invoke::<()>("app_settings_set", payload).await {
                                web_sys_log(&format!("save signature: {e}"));
                            }
                        });
                        // Bump the parent tick so other consumers see
                        // the new value on their next poll.
                        let cur = *tick_ref.read();
                        tick_ref.set(cur.wrapping_add(1));
                    }
                },
            }
        }
    }
}

// ---------- Notifications ----------

#[component]
fn NotificationsTab(tick: SettingsTick) -> Element {
    rsx! {
        div {
            class: "settings-section",
            h2 { class: "settings-section-title", "Notifications" }
            BoolSettingRow {
                setting_key: KEY_NOTIFY_MASTER,
                label: "Show desktop notifications for new mail",
                default_on: true,
                tick,
            }
            p {
                class: "settings-note",
                "Per-account toggles live in the Accounts tab."
            }
        }
    }
}

// ---------- Shortcuts ----------

#[component]
fn ShortcutsTab() -> Element {
    rsx! {
        div {
            class: "settings-section",
            h2 { class: "settings-section-title", "Keyboard shortcuts" }
            table {
                class: "settings-shortcuts",
                tbody {
                    for (key, label) in [
                        ("c", "Compose"),
                        ("e", "Archive selected message"),
                        ("#", "Delete selected message"),
                        ("r", "Reply"),
                        ("a", "Reply all"),
                        ("f", "Forward"),
                        ("/", "Search mail"),
                        ("Esc", "Close compose / clear search / clear selection"),
                        ("?", "Toggle help overlay"),
                    ] {
                        tr {
                            td { kbd { "{key}" } }
                            td { "{label}" }
                        }
                    }
                }
            }
            p {
                class: "settings-note",
                "Shortcuts are ignored while typing in a field."
            }
        }
    }
}

// ---------- Privacy ----------

#[component]
fn PrivacyTab(tick: SettingsTick) -> Element {
    rsx! {
        div {
            class: "settings-section",
            h2 { class: "settings-section-title", "Privacy" }
            BoolSettingRow {
                setting_key: KEY_REMOTE_IMAGES,
                label: "Always load remote images and tracked content",
                default_on: false,
                tick,
            }
            p {
                class: "settings-note",
                "Per-sender exceptions stay available from the reader pane's banner."
            }
        }
    }
}

#[component]
fn BoolSettingRow(
    setting_key: &'static str,
    label: &'static str,
    default_on: bool,
    tick: SettingsTick,
) -> Element {
    let key_for_fetch = setting_key.to_string();
    let tick_value = *tick.read();
    let current = use_resource(use_reactive!(|key_for_fetch, tick_value| async move {
        let _ = tick_value;
        invoke::<Option<String>>(
            "app_settings_get",
            serde_json::json!({ "input": { "key": key_for_fetch } }),
        )
        .await
    }));
    let checked = match &*current.read_unchecked() {
        Some(Ok(Some(v))) => v == "true",
        _ => default_on,
    };
    rsx! {
        div {
            class: "settings-field",
            label {
                class: "settings-checkbox-label",
                input {
                    class: "settings-checkbox",
                    r#type: "checkbox",
                    checked: checked,
                    onchange: {
                        let key = setting_key.to_string();
                        let mut tick = tick;
                        move |e: Event<FormData>| {
                            let next = matches!(e.value().as_str(), "true" | "on");
                            let key = key.clone();
                            let value = if next { "true" } else { "false" }.to_string();
                            spawn(async move {
                                let payload = serde_json::json!({
                                    "input": { "key": key, "value": value }
                                });
                                if let Err(err) = invoke::<()>("app_settings_set", payload).await {
                                    web_sys_log(&format!("app_settings_set: {err}"));
                                    return;
                                }
                                tick.with_mut(|t| *t = t.wrapping_add(1));
                            });
                        }
                    },
                }
                span { "{label}" }
            }
        }
    }
}
