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
use dpi;
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

#[derive(Debug, Deserialize)]
pub struct ReaderSetPositionInput {
    /// Bounding rect of the `.reader-body-fill` slot in window-
    /// relative CSS pixels. CSS rect coordinates can be negative
    /// during transitions; the Rust side clamps before passing to
    /// GTK. `f64` because `getBoundingClientRect` returns floats.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// `reader_set_position` — push the reader-body element's bounding
/// rect at GTK so Servo's overlay surface tracks the slot.
///
/// Called from the UI's `ResizeObserver` whenever
/// `.reader-body-fill` changes shape (window resize, splitter drag,
/// compose pane open/close, etc.). Rust clamps + casts to i32 and
/// hands off to `LinuxGtkParent::set_position` on the GTK main
/// thread via Tauri's `run_on_main_thread`.
///
/// No-ops on platforms / builds without the Servo install. Returns
/// `Ok(())` regardless so the UI can fire blindly without branching
/// on platform.
#[tauri::command]
pub async fn reader_set_position(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    input: ReaderSetPositionInput,
) -> IpcResult<()> {
    #[cfg(all(target_os = "linux", feature = "servo"))]
    {
        let Some(parent) = crate::linux_gtk::parent() else {
            tracing::debug!("reader_set_position: GTK parent not registered yet");
            return Ok(());
        };
        let x = input.x.round() as i32;
        let y = input.y.round() as i32;
        let w = input.width.round() as i32;
        let h = input.height.round() as i32;
        tracing::info!(x, y, w, h, "reader_set_position");
        if let Err(e) = app.run_on_main_thread(move || parent.set_position(x, y, w, h)) {
            tracing::debug!(error = %e, "reader_set_position: GTK dispatch failed (app shutdown?)");
        }

        // Servo's WebView locks its viewport at the size passed to
        // `new_linux`. Without this resize the host widget grows but
        // Servo keeps painting into the original 720x560. The
        // `EmailRenderer::resize` default impl is a no-op so this is
        // safe even when Servo isn't installed.
        if w > 1 && h > 1 {
            let mut slot = state.servo_renderer.lock().await;
            if let Some(renderer) = slot.as_mut() {
                renderer.resize(dpi::PhysicalSize::new(w as u32, h as u32));
            }
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "servo")))]
    {
        let _ = app;
        let _ = state;
        let _ = input;
    }
    Ok(())
}

/// `reader_clear` — move Servo's overlay surface off-screen.
///
/// Called when the user deselects a message or opens the Compose
/// pane: the Dioxus reader pane shows a placeholder ("Select a
/// message to read") and Servo's surface should be invisible
/// rather than freezing the previous render in place. Same
/// no-op-on-other-platforms shape as `reader_set_position`.
#[tauri::command]
pub async fn reader_clear(app: tauri::AppHandle) -> IpcResult<()> {
    #[cfg(all(target_os = "linux", feature = "servo"))]
    {
        let Some(parent) = crate::linux_gtk::parent() else {
            return Ok(());
        };
        if let Err(e) = app.run_on_main_thread(move || parent.hide()) {
            tracing::debug!(error = %e, "reader_clear: dispatch failed (app shutdown?)");
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "servo")))]
    {
        let _ = app;
    }
    Ok(())
}
