// SPDX-License-Identifier: Apache-2.0

//! `reader_*` Tauri commands — the reader-pane Servo renderer seam.
//!
//! Phase 0 Week 6 scope: `reader_render` takes raw HTML from the UI
//! and hands it to [`capytain_renderer::ServoRenderer`]. The UI is
//! responsible for composing the HTML (today: format headers + plain
//! text body into a minimal styled document in `apps/desktop/ui`).
//! Real sanitization (ammonia strip → adblock pass) arrives in Phase
//! 1 alongside the remote-content policy; this seam lets the reader
//! pane light up end-to-end on selection before that work lands.

use capytain_core::RenderPolicy;
use capytain_ipc::IpcResult;
use serde::Deserialize;
use tauri::{AppHandle, Runtime, State};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ReaderRenderInput {
    /// Fully-formed HTML document to render in the Servo reader pane.
    /// Phase 0: composed by the UI from `RenderedMessage` headers +
    /// plaintext body. Phase 1: replaced with the sanitized HTML
    /// returned by `messages_get` once ammonia / adblock pipelines
    /// are live.
    pub html: String,
}

/// `reader_render` — hand HTML to the Servo renderer.
///
/// # Thread affinity
///
/// `capytain_renderer::ServoRenderer` is `Send + Sync` at the type
/// level, but every Servo `WebView` call has to happen on the thread
/// that constructed the engine (design doc §6.6). The renderer handles
/// this internally: each trait-method call marshals onto the Tauri
/// main thread via the `MainThreadDispatch` we installed at startup.
/// That makes this command safe to invoke from any Tauri async
/// worker thread — which is where `#[tauri::command] async fn` runs.
#[tauri::command]
pub async fn reader_render<R: Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
    input: ReaderRenderInput,
) -> IpcResult<()> {
    tracing::info!(bytes = input.html.len(), "reader_render");

    // `servo_renderer` is an `Option` on `AppState` because `new_linux`
    // can fail (e.g. running under a `RawWindowHandle` variant we don't
    // support); the desktop app degrades gracefully rather than
    // aborting startup.
    let mut guard = state.servo_renderer.lock().await;
    if let Some(renderer) = guard.as_mut() {
        let _handle = renderer.render(&input.html, RenderPolicy::strict());
    } else {
        tracing::warn!(
            "reader_render: ServoRenderer not available on this platform/build — skipping"
        );
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct OpenExternalUrlInput {
    pub url: String,
}

/// `open_external_url` — hand an http(s) / mailto URL to the OS
/// default browser.
///
/// Triggered when the user clicks a link inside the reader pane's
/// email iframe. The iframe runs a tiny click interceptor that
/// `postMessage`s the URL to the parent window; the Dioxus app
/// invokes this command in response. We deliberately allow only
/// `http`, `https`, and `mailto` schemes — `javascript:`,
/// `file://`, etc. would be a privilege-escalation hand-off from
/// untrusted email content to the host OS.
#[tauri::command]
pub async fn open_external_url(input: OpenExternalUrlInput) -> IpcResult<()> {
    let url = input.url.trim();
    let lower = url.to_ascii_lowercase();
    let allowed = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:");
    if !allowed {
        tracing::warn!(%url, "open_external_url: rejecting non-http(s)/mailto scheme");
        return Err(capytain_ipc::IpcError::new(
            capytain_ipc::IpcErrorKind::Permission,
            format!("unsupported URL scheme: {url}"),
        ));
    }

    if let Err(e) = webbrowser::open(url) {
        tracing::warn!(%url, error = %e, "open_external_url: webbrowser::open failed");
        return Err(capytain_ipc::IpcError::new(
            capytain_ipc::IpcErrorKind::Internal,
            format!("failed to open URL: {e}"),
        ));
    }
    tracing::info!(%url, "open_external_url");
    Ok(())
}
