// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! QSL desktop shell entry point.
//!
//! Boots Tauri 2, opens the Turso-backed DB, installs [`AppState`] on the
//! Tauri manager, and registers every `qsl-*` command. The Dioxus
//! UI rides in Tauri's webview and calls these commands over the
//! standard `invoke` bridge.
//!
//! # Runtime shape
//!
//! - Tauri owns the event loop and the tokio runtime (via its built-in
//!   `tauri::async_runtime`).
//! - On `setup`, we resolve the OS data directory with `directories`,
//!   open the database, run pending migrations, and hand the handle to
//!   [`AppState`]. The reader pane lives inside the host webview as a
//!   sandboxed `<iframe srcdoc>`, so there's no auxiliary renderer
//!   process to attach. (See `docs/servo-tombstone.md` for the
//!   previous Servo-backed reader implementation.)

mod backend_factory;
mod commands;
mod imap_idle;
mod jmap_push;
mod reconnect;
mod state;
mod sync_engine;

use std::path::PathBuf;

use directories::ProjectDirs;
use qsl_storage::{run_migrations, TursoConn};
use tauri::Manager;

use crate::state::AppState;

fn main() {
    // Linux webview workaround. webkit2gtk's DMA-BUF renderer asks
    // libgbm for framebuffers; on hybrid AMD/NVIDIA boxes (or any rig
    // where libgbm lands on the NVIDIA proprietary stack) the GBM
    // allocator returns `Invalid argument` because the proprietary
    // driver doesn't expose the format modifiers webkit wants, and the
    // webview paints nothing — the chrome shows but the body is blank.
    // Rolling webkit back to its SHM rendering path bypasses GBM
    // entirely and makes the webview render normally. The path is a
    // shade slower than DMA-BUF on hardware where DMA-BUF actually
    // works, but the performance hit is negligible for an email client.
    //
    // Gated on `is_none()` so a user export still wins, and confined
    // to Linux because macOS / Windows webviews don't go through this
    // path at all.
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            // SAFETY: called pre-main, before any other thread can
            // observe or mutate the process environment.
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
        }
    }

    // Telemetry: route Tauri / plugin logs through `tracing`. Matches
    // the mailcli pattern so operators can use the same `RUST_LOG`
    // filters. `init` returns an error if it has already been called in
    // this process (e.g. a hot-reloaded test harness); we log and
    // continue rather than panic.
    if let Err(e) = qsl_telemetry::init(None) {
        eprintln!("qsl-telemetry: {e}");
    }

    // Install a rustls `CryptoProvider` before any TLS traffic starts.
    // tokio-rustls / hyper-rustls / lettre all reach rustls indirectly;
    // installing `ring` once at startup avoids the auto-pick panic
    // when more than one provider feature is enabled in the dep graph.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // An earlier call already installed a provider. That's fine;
        // we don't want to panic on hot-reload or double-init.
        tracing::debug!("rustls CryptoProvider was already installed; continuing");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // Resolve data dir + open DB on the Tauri async runtime so
            // we don't block the UI thread. `block_on` here is fine: we
            // only do it once at startup, before any window is shown.
            // Tauri's setup hook returns `Box<dyn Error>` (not Send+Sync),
            // while `bootstrap_state` produces the Send+Sync variant so
            // it stays usable in other async contexts — unsize the error
            // explicitly to bridge the two.
            let state = tauri::async_runtime::block_on(bootstrap_state())
                .map_err(|e| -> Box<dyn std::error::Error> { e })?;
            app.manage(state);

            // Phase 1 Week 10: kick off a background sync of every
            // configured account so the UI sees fresh mail without the
            // user having to run `mailcli sync` first. Live IDLE
            // watchers are layered on in PR 7b — the engine module
            // already exposes the right seam for them.
            sync_engine::spawn(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ui_ready,
            commands::accounts::accounts_list,
            commands::folders::folders_list,
            commands::messages::messages_list,
            commands::messages::messages_list_unified,
            commands::messages::messages_search,
            commands::messages::messages_mark_read,
            commands::messages::messages_flag,
            commands::messages::messages_move,
            commands::messages::messages_archive,
            commands::messages::messages_delete,
            commands::messages::messages_get,
            commands::messages::messages_trust_sender,
            commands::messages::messages_load_older,
            commands::messages::messages_refresh_folder,
            commands::messages::messages_send,
            commands::messages::messages_open_in_window,
            commands::messages::messages_open_attachment,
            commands::drafts::drafts_save,
            commands::drafts::drafts_load,
            commands::drafts::drafts_list,
            commands::drafts::drafts_delete,
            commands::reader::open_external_url,
            commands::contacts::contacts_query,
            commands::accounts::accounts_set_display_name,
            commands::accounts::accounts_set_signature,
            commands::accounts::accounts_set_notify_enabled,
            commands::accounts::accounts_remove,
            commands::accounts::accounts_add_oauth,
            commands::settings::settings_open,
            commands::settings::oauth_add_open,
            commands::settings::oauth_add_close,
            commands::settings::oauth_providers_list,
            commands::settings::app_settings_get,
            commands::settings::app_settings_set,
            commands::compose::compose_pick_attachments,
            commands::history_sync::history_sync_start,
            commands::history_sync::history_sync_cancel,
            commands::history_sync::history_sync_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running QSL");
}

/// Resolve the OS data directory, open the Turso database, run pending
/// migrations, and build the shared [`AppState`].
///
/// Kept as a free function so the bootstrap logic is testable and so
/// `setup` stays a one-liner.
async fn bootstrap_state() -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
    let data_dir = resolve_data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("qsl.db");
    let db = TursoConn::open(&db_path).await?;
    // Run migrations on the IPC connection only — they're idempotent
    // at the SQLite layer regardless, but doing it once before the
    // sync connection opens means schema is in place by the time the
    // sync engine touches the file.
    run_migrations(&db).await?;
    // Second connection to the same file for the sync engine. WAL
    // mode is enabled by `TursoConn::open`, so reads on `db` won't
    // block while `sync_db` is mid-transaction. See `AppState::sync_db`
    // for the full rationale.
    let sync_db = TursoConn::open(&db_path).await?;

    tracing::info!(
        data_dir = %data_dir.display(),
        db = %db_path.display(),
        "qsl desktop ready"
    );

    Ok(AppState::new(db, sync_db, data_dir))
}

/// Mirror of mailcli's data-dir resolution so both binaries read and
/// write the same Turso file by default.
fn resolve_data_dir() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let dirs =
        ProjectDirs::from("app", "qsl", "qsl").ok_or("could not resolve OS data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}
