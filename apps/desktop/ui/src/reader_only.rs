// SPDX-License-Identifier: Apache-2.0

//! Standalone reader pane mounted in popup-window mode.
//!
//! The Tauri `messages_open_in_window` IPC command pops a new
//! `WebviewWindow` and injects `window.__QSL_READER_ID__` into the
//! webview's JS context via `initialization_script`. The app root
//! reads that global at boot (`reader_window_message_id`) and mounts
//! [`ReaderOnlyApp`] when it's set.
//!
//! `ReaderOnlyApp` reuses the same `compose_reader_html` plus
//! `reader_render` plus `start_reader_body_tracker` plumbing the
//! inline reader uses, so a popup paints with the same Servo overlay
//! path. The popup's window-scoped `reader_render` IPC call
//! lazy-installs a fresh Servo instance for this window's label on
//! first invocation — see the reader-command module on the desktop
//! side for details.

use dioxus::prelude::*;
use qsl_ipc::{MessageId, RenderedMessage};
use serde::Serialize;

use crate::app::{
    compose_reader_html, invoke, push_reader_body_rect, reader_window_preload,
    start_reader_body_tracker, web_sys_log, TAILWIND_CSS,
};

#[derive(Serialize)]
struct MessagesGetArgs<'a> {
    input: GetInner<'a>,
}

#[derive(Serialize)]
struct GetInner<'a> {
    id: &'a MessageId,
}

/// Read `window.__QSL_READER_PRELOAD__` into a `RenderedMessage`. The
/// Tauri `messages_open_in_window` command pre-fetches the message and
/// JSON-encodes it into that global before the wasm bundle boots, so
/// the popup can mount instantly. Returns `None` when the global is
/// missing (preload disabled, host fetch failed) or unparseable.
fn take_reader_preload() -> Option<RenderedMessage> {
    let json = reader_window_preload().as_string()?;
    match serde_json::from_str::<RenderedMessage>(&json) {
        Ok(msg) => Some(msg),
        Err(e) => {
            web_sys_log(&format!("ReaderOnlyApp: preload parse failed: {e}"));
            None
        }
    }
}

#[component]
pub fn ReaderOnlyApp(message_id: MessageId) -> Element {
    let id_for_resource = message_id.clone();
    // `use_hook` runs the closure exactly once when the component
    // first mounts — perfect for draining `__QSL_READER_PRELOAD__`,
    // which is a one-shot global. If the preload is present we
    // bypass the IPC entirely; otherwise `use_resource` issues
    // `messages_get` like before.
    let preload: Option<RenderedMessage> = use_hook(take_reader_preload);
    let rendered = use_resource(move || {
        let id = id_for_resource.clone();
        let preload = preload.clone();
        async move {
            if let Some(msg) = preload {
                Ok(msg)
            } else {
                invoke::<RenderedMessage>(
                    "messages_get",
                    MessagesGetArgs {
                        input: GetInner { id: &id },
                    },
                )
                .await
            }
        }
    });

    // Same body tracker the inline reader uses — watches the
    // `.reader-body-fill` element below and pushes its bounding rect
    // to the Rust side over `reader_set_position`.
    use_hook(start_reader_body_tracker);

    rsx! {
        document::Stylesheet { href: TAILWIND_CSS }
        div {
            class: "popup-reader-shell",
            style: "display: grid; grid-template-rows: auto 1fr; height: 100vh; background: #0f1115; color: #e6e8eb;",
            match &*rendered.read_unchecked() {
                None => rsx! {
                    p { class: "msglist-empty", style: "padding: 1rem;", "Loading message…" }
                },
                Some(Err(e)) => rsx! {
                    p { class: "msglist-empty", style: "padding: 1rem;", "{e}" }
                },
                Some(Ok(msg)) => {
                    let primary = msg.headers.from.first();
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
                        msg.headers.date,
                        chrono::Utc::now(),
                    );
                    let date_full = msg.headers.date.to_rfc2822();
                    let subject = if msg.headers.subject.is_empty() {
                        "(no subject)".to_string()
                    } else {
                        msg.headers.subject.clone()
                    };
                    let body_doc = compose_reader_html(msg);

                    // Push the body to the Servo overlay surface.
                    // First call for this window's label triggers a
                    // lazy Servo install on the Rust side.
                    let render_payload = body_doc.clone();
                    use_effect(move || {
                        let payload = render_payload.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Err(e) = invoke::<()>(
                                "reader_render",
                                serde_json::json!({ "input": { "html": payload } }),
                            )
                            .await
                            {
                                web_sys_log(&format!("reader_render (popup): {e}"));
                            }
                        });
                        push_reader_body_rect();
                    });

                    rsx! {
                        div {
                            class: "reader-header-block",
                            style: "padding: 1rem 1.25rem; border-bottom: 1px solid #20242c;",
                            h1 {
                                class: "reader-subject",
                                style: "margin: 0 0 0.5rem 0; font-size: 1.25rem; font-weight: 600;",
                                "{subject}"
                            }
                            div {
                                class: "reader-sender-card",
                                style: "display: flex; gap: 0.5rem; align-items: baseline; font-size: 0.875rem; color: #a8adb6;",
                                span { style: "color: #e6e8eb; font-weight: 500;", "{from_name}" }
                                span { "{from_addr}" }
                                span { style: "margin-left: auto;", title: "{date_full}", "{date}" }
                            }
                        }
                        div {
                            class: "reader-body-fill",
                            style: "min-height: 0; position: relative;",
                        }
                    }
                }
            }
        }
    }
}
