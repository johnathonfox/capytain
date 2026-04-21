// SPDX-License-Identifier: Apache-2.0

//! `reader_*` Tauri commands — the reader-pane Servo renderer seam.
//!
//! Phase 0 Week 6 Day 2 scope: a single `reader_render` command that
//! takes a `MessageId` and renders hardcoded test HTML through
//! [`capytain_renderer::ServoRenderer`]. Real sanitized body fetching
//! (ammonia strip → adblock pass → `ServoRenderer::render`) lands in
//! Phase 1 alongside the remote-content policy.

use capytain_core::RenderPolicy;
use capytain_ipc::{IpcResult, MessageId};
use serde::Deserialize;
use tauri::{AppHandle, Runtime, State};

use crate::state::AppState;

/// Hardcoded test body used in place of a real sanitized email body.
/// The anchor is here so the human validator can click it and watch
/// the `EmailRenderer::on_link_click` callback fire end-to-end via
/// `tracing::info!` (see the callback registration in `main.rs`).
const HELLO_FROM_SERVO_HTML: &str = r#"<!DOCTYPE html>
<html>
<body>
  <h1>Hello from Servo</h1>
  <p>Phase 0 Week 6 Day 2 — hardcoded test HTML rendered via the
     Servo-backed <code>EmailRenderer</code>.</p>
  <p><a href="https://example.com/capytain-link-click-test">
     Click this link to exercise the navigation callback
  </a>.</p>
</body>
</html>"#;

#[derive(Debug, Deserialize)]
pub struct ReaderRenderInput {
    /// Identifies the message to render. Ignored in Phase 0 Week 6 —
    /// the body is always `HELLO_FROM_SERVO_HTML`. Wired-through-but-
    /// unused so the command signature matches the shape Phase 1 will
    /// actually use (load body blob → sanitize → render).
    pub id: MessageId,
}

/// `reader_render` — hand sanitized HTML to the Servo renderer.
///
/// Phase 0 Week 6 Day 2 always renders a fixed test document regardless
/// of the requested `id`. The command still takes `id` so the IPC shape
/// matches `COMMANDS.md §Reader::reader_render` and Phase 1 only has to
/// swap the hardcoded string for the real message-body lookup.
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
    tracing::info!(id = %input.id.0, "reader_render: rendering hardcoded Day 2 test HTML");

    // `servo_renderer` is an `Option` on `AppState` because `new_linux`
    // can fail (e.g. running under a `RawWindowHandle` variant we don't
    // support); the desktop app degrades gracefully rather than
    // aborting startup.
    let mut guard = state.servo_renderer.lock().await;
    if let Some(renderer) = guard.as_mut() {
        let _handle = renderer.render(HELLO_FROM_SERVO_HTML, RenderPolicy::strict());
    } else {
        tracing::warn!(
            "reader_render: ServoRenderer not available on this platform/build — skipping"
        );
    }

    Ok(())
}
