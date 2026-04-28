// SPDX-License-Identifier: Apache-2.0

//! Reader-pane Tauri commands.
//!
//! The reader pane is a sandboxed `<iframe srcdoc>` inside the host
//! webview, so the rendering itself doesn't need an IPC seam. The
//! one command that survives is [`open_external_url`]: clicked
//! anchors inside the iframe `postMessage` their URLs up to the
//! parent webview, which forwards them here so the Rust side can
//! validate the scheme, strip tracking params, and shell out to the
//! OS default browser.

use qsl_core::clean_outbound_url;
use qsl_ipc::IpcResult;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize)]
pub struct OpenExternalUrlInput {
    pub url: String,
}

/// `open_external_url` — hand an http(s) / mailto URL to the OS
/// default browser, after stripping tracking params and unwrapping
/// known redirect services.
///
/// Triggered when the user clicks a link inside the reader pane's
/// email iframe. We deliberately allow only `http`, `https`, and
/// `mailto` schemes — `javascript:`, `file://`, etc. would be a
/// privilege-escalation hand-off from untrusted email content to
/// the host OS.
#[tauri::command]
pub async fn open_external_url(input: OpenExternalUrlInput) -> IpcResult<()> {
    let raw = input.url.trim();
    let lower = raw.to_ascii_lowercase();
    let allowed = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:");
    if !allowed {
        tracing::warn!(url = %raw, "open_external_url: rejecting non-http(s)/mailto scheme");
        return Err(qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Permission,
            format!("unsupported URL scheme: {raw}"),
        ));
    }

    // `mailto:` URLs go straight to the OS handler — the link
    // cleaner only knows about web-tracker patterns. For http(s)
    // we parse, clean, and serialize before handing off; on parse
    // failure we fall back to the raw URL (pathological cases like
    // a relative URL slipping through still get the OS's own
    // resolution).
    let target = if lower.starts_with("mailto:") {
        raw.to_string()
    } else {
        match Url::parse(raw) {
            Ok(parsed) => clean_outbound_url(parsed).to_string(),
            Err(e) => {
                tracing::debug!(url = %raw, error = %e, "open_external_url: url parse failed; passing raw");
                raw.to_string()
            }
        }
    };

    if let Err(e) = webbrowser::open(&target) {
        tracing::warn!(url = %target, error = %e, "open_external_url: webbrowser::open failed");
        return Err(qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Internal,
            format!("failed to open URL: {e}"),
        ));
    }
    tracing::info!(url = %target, "open_external_url");
    Ok(())
}
