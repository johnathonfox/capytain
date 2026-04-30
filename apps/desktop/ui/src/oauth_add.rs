// SPDX-License-Identifier: Apache-2.0

//! Add-account window — mounted when `window.__QSL_VIEW__ === "oauth-add"`.
//!
//! Three states:
//!
//!   1. **Provider picker** — list providers from `oauth_providers_list`
//!      and ask the user to type their email address. Submitting flips
//!      to the waiting state.
//!   2. **Waiting** — `accounts_add_oauth` is in flight, the OS
//!      browser is open on the provider's consent page, and we're
//!      blocked on the loopback redirect. The wasm side just renders
//!      "Waiting for browser approval…".
//!   3. **Result** — success or error. Success state offers a
//!      "Close" button; error state offers "Try again" which clears
//!      the error and returns to the picker. The success path also
//!      auto-closes the window after a short pause so the user
//!      doesn't have to click through a no-op.
//!
//! On success, the host has already written the account row + token
//! and kicked a one-shot bootstrap sync; the main window's existing
//! `sync_event` listener picks it up and the sidebar refetches.

use dioxus::prelude::*;
use qsl_ipc::Account;
use serde::Deserialize;
use wasm_bindgen::JsCast;

use crate::app::{invoke, web_sys_log, TAILWIND_CSS};

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ProviderInfo {
    slug: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq)]
enum FlowState {
    Picker,
    Waiting { provider_name: String },
    Success(Account),
    Error(String),
}

#[component]
pub fn OAuthAddApp() -> Element {
    crate::app::use_appearance_hooks();
    let providers = use_resource(|| async {
        invoke::<Vec<ProviderInfo>>("oauth_providers_list", serde_json::json!({})).await
    });
    let mut state = use_signal(|| FlowState::Picker);
    let mut email = use_signal(String::new);
    let selected = use_signal(|| Option::<ProviderInfo>::None);

    rsx! {
        document::Stylesheet { href: TAILWIND_CSS }
        div {
            class: "oauth-shell",
            h1 { class: "oauth-title", "Add account" }
            match state.read().clone() {
                FlowState::Picker => rsx! {
                    p {
                        class: "oauth-blurb",
                        "Pick your provider and enter the email address you want to add. \
                         A browser tab will open for you to approve access; QSL only \
                         stores a refresh token, not a password."
                    }
                    div {
                        class: "oauth-provider-grid",
                        match &*providers.read_unchecked() {
                            None => rsx! { p { class: "oauth-status", "Loading providers…" } },
                            Some(Err(e)) => rsx! { p { class: "oauth-status oauth-error", "{e}" } },
                            Some(Ok(list)) => rsx! {
                                for p in list.iter().cloned() {
                                    ProviderButton {
                                        provider: p,
                                        selected,
                                    }
                                }
                            },
                        }
                    }
                    label { class: "oauth-label", "Email address" }
                    input {
                        class: "oauth-input",
                        r#type: "email",
                        placeholder: "you@example.com",
                        autocomplete: "email",
                        value: "{email}",
                        oninput: move |e| email.set(e.value()),
                    }
                    button {
                        class: "oauth-button oauth-button-primary",
                        r#type: "button",
                        disabled: selected.read().is_none() || email.read().trim().is_empty(),
                        onclick: move |_| {
                            let Some(provider) = selected.read().clone() else { return; };
                            let email_value = email.read().trim().to_string();
                            if email_value.is_empty() { return; }
                            state.set(FlowState::Waiting {
                                provider_name: provider.name.clone(),
                            });
                            spawn(async move {
                                let payload = serde_json::json!({
                                    "input": {
                                        "provider": provider.slug,
                                        "email": email_value,
                                    },
                                });
                                match invoke::<Account>("accounts_add_oauth", payload).await {
                                    Ok(account) => {
                                        state.set(FlowState::Success(account));
                                        // Auto-close after a beat so the user doesn't
                                        // need to acknowledge the success state.
                                        let _ = wasm_bindgen_futures::JsFuture::from(
                                            js_sys::Promise::new(&mut |resolve, _| {
                                                let window = web_sys::window().expect("window");
                                                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                                                    &resolve,
                                                    1500,
                                                );
                                            }),
                                        )
                                        .await;
                                        close_window();
                                    }
                                    Err(e) => {
                                        web_sys_log(&format!("accounts_add_oauth: {e}"));
                                        state.set(FlowState::Error(e));
                                    }
                                }
                            });
                        },
                        "Continue"
                    }
                },
                FlowState::Waiting { provider_name } => rsx! {
                    p {
                        class: "oauth-status",
                        "Waiting for browser approval at {provider_name}…"
                    }
                    p {
                        class: "oauth-note",
                        "Your default browser should have opened. Approve access to \
                         continue, then return here."
                    }
                },
                FlowState::Success(account) => rsx! {
                    p {
                        class: "oauth-status oauth-success",
                        "Added {account.email_address}."
                    }
                    p {
                        class: "oauth-note",
                        "Closing this window — your inbox will populate shortly."
                    }
                    button {
                        class: "oauth-button",
                        r#type: "button",
                        onclick: move |_| close_window(),
                        "Close now"
                    }
                },
                FlowState::Error(msg) => rsx! {
                    p { class: "oauth-status oauth-error", "{msg}" }
                    button {
                        class: "oauth-button oauth-button-primary",
                        r#type: "button",
                        onclick: move |_| state.set(FlowState::Picker),
                        "Try again"
                    }
                },
            }
        }
    }
}

#[component]
fn ProviderButton(provider: ProviderInfo, mut selected: Signal<Option<ProviderInfo>>) -> Element {
    let is_active = selected
        .read()
        .as_ref()
        .map(|p| p.slug == provider.slug)
        .unwrap_or(false);
    let class = if is_active {
        "oauth-provider oauth-provider-active"
    } else {
        "oauth-provider"
    };
    let provider_for_click = provider.clone();
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| selected.set(Some(provider_for_click.clone())),
            "{provider.name}"
        }
    }
}

/// Close this Tauri window via the same `core:window:allow-close`
/// permission the main window already has. We can't import
/// `tauri::Window` from wasm, so go through the JS bridge with the
/// plugin command directly.
fn close_window() {
    if let Some(window) = web_sys::window() {
        if let Some(close_fn) = js_sys::Reflect::get(&window, &"close".into())
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Function>().ok())
        {
            let _ = close_fn.call0(&window);
        }
    }
}
