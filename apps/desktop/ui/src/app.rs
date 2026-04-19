// SPDX-License-Identifier: Apache-2.0

//! Root Dioxus component for the Capytain UI (wasm32-only).
//!
//! Phase 0 Week 5 part 1 proof of life: render a heading, call
//! `invoke("accounts_list")`, and dump the returned `Vec<Account>`
//! below. Real layout lands in Week 5 part 2.

use capytain_ipc::Account;
use dioxus::prelude::*;
use wasm_bindgen::prelude::*;

/// Tauri's global `invoke` function lives on `window.__TAURI__.core`
/// in Tauri 2. The extern binds to it once; the `core_invoke` free
/// function below serializes the arg map and awaits the JS promise.
#[wasm_bindgen(inline_js = r#"
    export async function coreInvoke(cmd, args) {
        return await window.__TAURI__.core.invoke(cmd, args);
    }
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = coreInvoke)]
    async fn core_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

/// Call a Tauri command and deserialize its JSON response into `T`.
pub(crate) async fn invoke<T: for<'de> serde::Deserialize<'de>>(
    cmd: &str,
    args: impl serde::Serialize,
) -> Result<T, String> {
    let js_args = serde_wasm_bindgen::to_value(&args).map_err(|e| e.to_string())?;
    let js_ret = core_invoke(cmd, js_args)
        .await
        .map_err(|e| format!("{e:?}"))?;
    serde_wasm_bindgen::from_value(js_ret).map_err(|e| e.to_string())
}

#[component]
pub fn App() -> Element {
    // One-shot fetch on mount. `Result<Vec<Account>, String>` so render
    // logic can show spinner / error / list without extra state.
    let accounts = use_resource(move || async move {
        invoke::<Vec<Account>>("accounts_list", serde_json::Value::Null).await
    });

    rsx! {
        main {
            class: "capytain-root",
            h1 { "Hello from Capytain" }
            p {
                class: "subtitle",
                "Phase 0 Week 5 · Tauri + Dioxus proof of life."
            }
            section {
                class: "accounts",
                h2 { "Accounts" }
                match &*accounts.read_unchecked() {
                    None => rsx! { p { "Loading accounts…" } },
                    Some(Err(e)) => rsx! { p { class: "error", "Failed to load accounts: {e}" } },
                    Some(Ok(list)) if list.is_empty() => rsx! {
                        p { "No accounts yet. Run "
                            code { "mailcli auth add gmail <email>" }
                            " to add one."
                        }
                    },
                    Some(Ok(list)) => rsx! {
                        ul {
                            for a in list.iter() {
                                li { key: "{a.id.0}",
                                    strong { "{a.display_name}" }
                                    " · "
                                    span { "{a.email_address}" }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}
