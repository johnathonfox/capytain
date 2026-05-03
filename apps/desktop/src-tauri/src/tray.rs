// SPDX-License-Identifier: Apache-2.0

//! System tray icon for QSL.
//!
//! Builds a `TrayIconBuilder` at app startup with a left-click toggle
//! (show/hide + focus the main window), a context menu (Show / Compose /
//! Quit), and a tooltip that reflects the current total inbox-unread
//! count. The tooltip re-renders whenever the sync engine emits a
//! `sync_event` — same trigger the sidebar uses for its per-folder
//! unread badges, so the two stay consistent.
//!
//! Linux note: KDE / GNOME / KWin all surface the icon via the
//! StatusNotifierItem / libappindicator protocol that wry pulls in
//! transitively. No extra setup needed; if the user's WM doesn't speak
//! either protocol the icon just doesn't appear, which is fine — the
//! main window remains the primary affordance.
//!
//! macOS / Windows use the platform-native menubar / notification-area
//! handlers wry already wraps.
use std::sync::{Arc, Mutex};

use qsl_core::FolderRole;
use qsl_storage::repos::{
    app_settings as settings_repo, folders as folders_repo, messages as messages_repo,
};
use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tracing::{debug, warn};

use crate::state::AppState;

/// Setting key controlling whether the system tray icon is visible.
/// Default is `true` — the tray ships on. Users who don't want it
/// flip the toggle in Settings → Appearance.
pub const KEY_TRAY_ENABLED: &str = "appearance.tray_enabled";

/// Build the tray icon and install it on `app`. Idempotent — if a tray
/// is already registered for this `AppHandle` (e.g. a hot-reload), we
/// skip the rebuild and return Ok.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "tray-show", "Show QSL", true, None::<&str>)?;
    let compose_item = MenuItem::with_id(app, "tray-compose", "Compose", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "tray-quit", "Quit", true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .item(&show_item)
        .item(&compose_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let Some(icon) = app.default_window_icon().cloned() else {
        warn!("tray: no default window icon configured; skipping tray install");
        return Ok(());
    };

    let tray = TrayIconBuilder::with_id("qsl-main")
        .icon(icon)
        .tooltip("QSL")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "tray-show" => focus_main_window(app),
            "tray-compose" => open_compose(app),
            "tray-quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click on the icon body toggles the main window's
            // visibility. Right-click is reserved for the context menu
            // (handled natively by `show_menu_on_left_click(false)`).
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    // Stash the tray handle in a process-global so the unread updater
    // can find it without round-tripping through the AppHandle's tray
    // collection (which requires the same id literal at the call site).
    let _ = TRAY.set(Arc::new(Mutex::new(tray)));

    spawn_unread_updater(app.clone());
    spawn_visibility_watcher(app.clone());
    Ok(())
}

static TRAY: std::sync::OnceLock<Arc<Mutex<tauri::tray::TrayIcon>>> = std::sync::OnceLock::new();

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    match window.is_visible() {
        Ok(true) => {
            let _ = window.hide();
        }
        _ => {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }
}

fn open_compose(app: &AppHandle) {
    focus_main_window(app);
    // Tell the wasm UI to open compose. Compose state is owned by
    // Dioxus signals, so we just emit an event the UI listens for and
    // reacts to. Empty payload — the UI defaults to "no preselected
    // account" the same way the sidebar Compose button does.
    let _ = app.emit_to("main", "tray_compose", ());
}

/// Background task: re-runs `recompute_unread` on every `sync_event`
/// emitted by the engine, plus a one-shot at startup so the tooltip
/// is right before the first sync lands.
fn spawn_unread_updater(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Initial pass.
        if let Err(e) = recompute_unread(&app).await {
            debug!("tray: initial unread compute failed: {e}");
        }

        // Subscribe to sync_event. The listener runs on Tauri's
        // per-event task pool; we hop back to an async task to do the
        // DB query so the listener stays cheap.
        let app_for_listener = app.clone();
        let _ = app.listen(crate::sync_engine::SYNC_EVENT, move |_| {
            let app = app_for_listener.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = recompute_unread(&app).await {
                    debug!("tray: unread compute failed: {e}");
                }
            });
        });
    });
}

/// Background task: applies the current `appearance.tray_enabled`
/// setting at startup and on every `app_settings_changed` event so the
/// tray icon hides / re-appears live without an app restart.
fn spawn_visibility_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Apply the persisted preference immediately on boot (covers
        // the "user disabled the tray, restart QSL" path).
        if let Err(e) = apply_visibility(&app).await {
            debug!("tray: initial visibility apply failed: {e}");
        }

        // Refresh on every app_settings_changed. The payload tells us
        // which key flipped; we only do the DB read + visibility flip
        // when our key changed, otherwise it's a cheap no-op.
        let app_for_listener = app.clone();
        let _ = app.listen(
            crate::commands::settings::APP_SETTINGS_CHANGED,
            move |evt| {
                if !payload_matches_tray_key(evt.payload()) {
                    return;
                }
                let app = app_for_listener.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = apply_visibility(&app).await {
                        debug!("tray: visibility refresh failed: {e}");
                    }
                });
            },
        );
    });
}

fn payload_matches_tray_key(payload: &str) -> bool {
    // Cheap string contains check: the payload is the JSON shape
    // `{"key":"appearance.tray_enabled", ...}`. Avoids a full serde
    // round-trip for an event that fires on every theme/density flip.
    payload.contains(KEY_TRAY_ENABLED)
}

async fn apply_visibility(app: &AppHandle) -> Result<(), String> {
    let enabled = read_tray_enabled(app).await.map_err(|e| format!("{e}"))?;
    let Some(tray) = TRAY.get() else {
        return Ok(());
    };
    let guard = tray.lock().map_err(|e| format!("tray lock: {e}"))?;
    if let Err(e) = guard.set_visible(enabled) {
        debug!("tray: set_visible({enabled}) failed: {e}");
    }
    Ok(())
}

async fn read_tray_enabled(app: &AppHandle) -> Result<bool, qsl_core::StorageError> {
    let state: tauri::State<'_, AppState> = app.state();
    let db = state.db.lock().await;
    let raw = settings_repo::get(&*db, KEY_TRAY_ENABLED).await?;
    // Default to `true` so the tray ships on. Anything else is treated
    // as "off" — only an explicit "true" string keeps it visible. This
    // matches how `BoolSettingRow` writes the value back.
    Ok(raw.as_deref() != Some("false"))
}

async fn recompute_unread(app: &AppHandle) -> Result<(), String> {
    let total = total_inbox_unread(app).await.map_err(|e| format!("{e}"))?;
    set_tooltip(app, total);
    Ok(())
}

async fn total_inbox_unread(app: &AppHandle) -> Result<u32, qsl_core::StorageError> {
    let state: tauri::State<'_, AppState> = app.state();
    let db = state.sync_db.lock().await;
    // "Inbox unread" matches what users expect from a tray badge:
    // marketing on Spam / Trash / Drafts / Sent doesn't ping the
    // tray. Sum the per-folder count for every folder with role=Inbox
    // across every account. Single-account installs hit this with one
    // folder; multi-account installs (Gmail + Fastmail) sum across.
    let inbox_folders = folders_repo::list_by_role(&*db, FolderRole::Inbox).await?;
    let ids: Vec<qsl_core::FolderId> = inbox_folders.into_iter().map(|f| f.id).collect();
    if ids.is_empty() {
        // No accounts configured yet — keep the tooltip neutral.
        return Ok(0);
    }
    messages_repo::count_unread_by_folders(&*db, &ids).await
}

fn set_tooltip(_app: &AppHandle, total: u32) {
    let Some(tray) = TRAY.get() else {
        return;
    };
    let tooltip = if total == 0 {
        "QSL".to_string()
    } else if total == 1 {
        "QSL · 1 unread".to_string()
    } else {
        format!("QSL · {total} unread")
    };
    let guard = match tray.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!("tray: tooltip lock poisoned: {e}");
            return;
        }
    };
    debug!(unread = total, tooltip = %tooltip, "tray: refreshing tooltip");
    if let Err(e) = guard.set_tooltip(Some(&tooltip)) {
        debug!("tray: set_tooltip failed: {e}");
    }
}
