// SPDX-License-Identifier: Apache-2.0

//! `settings_*` and `app_settings_*` Tauri commands.
//!
//! Two pieces:
//!
//!   - `settings_open` pops a dedicated Settings window mounting
//!     the Dioxus `SettingsApp` route. Mirrors the popup-reader
//!     pattern: build a `WebviewWindowBuilder` with `__QSL_VIEW__ =
//!     'settings'` injected via `initialization_script`, the Dioxus
//!     root branches on that and mounts a different component tree.
//!     A repeat call focuses the existing window.
//!
//!   - `app_settings_get` / `app_settings_set` are thin wrappers
//!     around the `app_settings_v1` k/v table (Appearance / Privacy
//!     tab state — theme, density, "always load remote images"
//!     master toggle).

use qsl_ipc::IpcResult;
use qsl_storage::repos::app_settings as app_settings_repo;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct AppSettingsGetInput {
    pub key: String,
}

/// `app_settings_get` — read a global setting key. Returns `None`
/// (serialized as `null`) when the key has never been written; the
/// UI is responsible for defaulting.
#[tauri::command]
pub async fn app_settings_get(
    state: State<'_, AppState>,
    input: AppSettingsGetInput,
) -> IpcResult<Option<String>> {
    let db = state.db.lock().await;
    let value = app_settings_repo::get(&*db, &input.key).await?;
    Ok(value)
}

#[derive(Debug, Deserialize)]
pub struct AppSettingsSetInput {
    pub key: String,
    pub value: String,
}

/// `app_settings_set` — upsert a global setting key.
#[tauri::command]
pub async fn app_settings_set(
    state: State<'_, AppState>,
    input: AppSettingsSetInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    app_settings_repo::set(&*db, &input.key, &input.value).await?;
    tracing::info!(key = %input.key, "app_settings_set");
    Ok(())
}

/// `settings_open` — show the Settings window. Idempotent: if the
/// window already exists this just brings it to the front.
///
/// The window mounts a fresh `index.html` with
/// `window.__QSL_VIEW__ = 'settings'` set before wasm boots; the
/// Dioxus root branches on that and mounts `SettingsApp` instead of
/// the three-pane shell.
#[tauri::command]
pub async fn settings_open<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> IpcResult<()> {
    use tauri::Manager;

    const LABEL: &str = "settings";

    if let Some(existing) = app.get_webview_window(LABEL) {
        if let Err(e) = existing.set_focus() {
            tracing::warn!(error = %e, "settings_open: set_focus failed");
        }
        return Ok(());
    }

    let init_script = "window.__QSL_VIEW__ = 'settings';";
    let _window =
        tauri::WebviewWindowBuilder::new(&app, LABEL, tauri::WebviewUrl::App("index.html".into()))
            .title("QSL — Settings")
            .inner_size(720.0, 560.0)
            .initialization_script(init_script)
            .devtools(true)
            .build()
            .map_err(|e| {
                qsl_ipc::IpcError::new(
                    qsl_ipc::IpcErrorKind::Internal,
                    format!("create settings window: {e}"),
                )
            })?;

    tracing::info!("settings_open: window built");
    Ok(())
}
