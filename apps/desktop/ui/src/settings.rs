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
use qsl_ipc::{Account, AccountId};

use crate::app::{invoke, web_sys_log, TAILWIND_CSS};

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
fn AccountsTab(tick: SettingsTick) -> Element {
    let tick_value = *tick.read();
    let accounts = use_resource(use_reactive!(|tick_value| async move {
        let _ = tick_value;
        invoke::<Vec<Account>>("accounts_list", serde_json::json!({})).await
    }));
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
        }
    }
}

#[component]
fn NotifyToggle(account_id: AccountId, initial: bool, tick: SettingsTick) -> Element {
    let mut enabled = use_signal(|| initial);
    rsx! {
        input {
            class: "settings-checkbox",
            r#type: "checkbox",
            checked: "{enabled}",
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
                checked: "{selected}",
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
                    checked: "{checked}",
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
