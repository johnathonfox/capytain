// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! QSL desktop shell entry point.
//!
//! Boots Tauri 2, opens the Turso-backed DB, installs [`AppState`] on the
//! Tauri manager, and registers every `qsl-*` command. The Dioxus
//! UI rides in Tauri's webview and calls these commands over the
//! standard `invoke` bridge.
//!
//! # Runtime shape (Phase 0 Week 5–6)
//!
//! - Tauri owns the event loop and the tokio runtime (via its built-in
//!   `tauri::async_runtime`).
//! - On `setup`, we resolve the OS data directory with `directories`,
//!   open the database, run pending migrations, and hand the handle to
//!   [`AppState`]. Then — when the `servo` feature is on (default for
//!   Linux / macOS) — we build the auxiliary reader window and attach
//!   the Servo-backed `EmailRenderer` to it. That has to happen on the
//!   main thread, where the Tauri `setup` hook runs.

mod backend_factory;
mod commands;
mod imap_idle;
mod jmap_push;
#[cfg(all(feature = "servo", target_os = "linux"))]
mod linux_gtk;
#[cfg(feature = "servo")]
mod renderer_bridge;
mod state;
mod sync_engine;

use std::path::PathBuf;

use directories::ProjectDirs;
use qsl_storage::{run_migrations, TursoConn};
use tauri::Manager;

use crate::state::AppState;

fn main() {
    // Telemetry: route Tauri / plugin logs through `tracing`. Matches
    // the mailcli pattern so operators can use the same `RUST_LOG`
    // filters. `init` returns an error if it has already been called in
    // this process (e.g. a hot-reloaded test harness); we log and
    // continue rather than panic.
    if let Err(e) = qsl_telemetry::init(None) {
        eprintln!("qsl-telemetry: {e}");
    }

    // Install a rustls `CryptoProvider` before any TLS traffic starts.
    // With the `servo` feature on, both `ring` and `aws-lc-rs` end up
    // in the dep graph (Servo's hyper-rustls and our keyring /
    // tokio-rustls pull them in respectively); rustls then refuses to
    // auto-pick and panics at the first HTTPS handshake — see
    // docs/week-6-day-2-notes.md. Explicitly installing `ring` keeps
    // the desktop app consistent with the rest of the workspace.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // An earlier call already installed a provider. That's fine;
        // we don't want to panic on hot-reload or double-init.
        tracing::debug!("rustls CryptoProvider was already installed; continuing");
    }

    // On Linux + NVIDIA proprietary driver + Wayland + KWin (and
    // plausibly other explicit-sync-advertising compositors), the
    // first surfman commit tears the Wayland connection with
    // `wp_linux_drm_syncobj_surface_v1` protocol error 71. Force
    // Mesa's llvmpipe EGL before Tauri / GTK / Servo touch GL. No-op
    // on non-Linux. See docs/upstream/surfman-explicit-sync.md.
    #[cfg(feature = "servo")]
    qsl_renderer::apply_nvidia_wayland_workaround();

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

            // Phase 2 Week 16 follow-up: install Servo over a
            // `gtk::Overlay` so it can be positioned over the
            // Dioxus `.reader-body-fill` slot. The UI pushes
            // bounding rects via the `reader_set_position` IPC
            // command on every reader-pane layout change.
            #[cfg(feature = "servo")]
            renderer_bridge::install_servo_renderer(app)?;

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
            commands::messages::messages_mark_read,
            commands::messages::messages_flag,
            commands::messages::messages_move,
            commands::messages::messages_delete,
            commands::messages::messages_get,
            commands::messages::messages_load_older,
            commands::messages::messages_refresh_folder,
            commands::drafts::drafts_save,
            commands::drafts::drafts_load,
            commands::drafts::drafts_list,
            commands::drafts::drafts_delete,
            commands::reader::reader_render,
            commands::reader::reader_set_position,
            commands::reader::reader_clear,
            commands::reader::open_external_url,
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
