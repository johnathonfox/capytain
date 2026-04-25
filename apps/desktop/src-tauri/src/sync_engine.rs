// SPDX-License-Identifier: Apache-2.0

//! Background sync engine for the desktop shell.
//!
//! Phase 1 Week 10 PR 7a (this module): on app start, spawn a tokio
//! task that walks every configured account, runs
//! [`capytain_sync::sync_account`] against it, and emits one
//! [`SyncEvent::FolderSynced`] per folder via Tauri to the Dioxus
//! UI. The list pane subscribes to those events to refresh the
//! header list when the user's selection matches.
//!
//! PR 7b adds live IDLE watchers on top of this scaffold: each
//! watcher dials its own session via
//! [`capytain_imap_client::dial_session`], runs
//! [`capytain_imap_client::watch_folder`], and feeds its
//! `BackendEvent::FolderChanged` notifications back through the
//! same trigger path that startup uses.

use capytain_ipc::SyncEvent;
use capytain_storage::{repos, BlobStore};
use tauri::{AppHandle, Emitter, Manager};
use tracing::{info, warn};

use crate::backend_factory;
use crate::state::AppState;

/// Tauri event name the engine emits on. The UI subscribes via
/// `tauri::event::listen("sync_event", …)`.
pub const SYNC_EVENT: &str = "sync_event";

/// Spawn the engine task. Returns immediately; the task runs in the
/// background until the app exits.
///
/// On startup it walks every account row in the database. For each:
///
/// 1. Open or reuse the cached [`MailBackend`](capytain_core::MailBackend).
/// 2. Run [`capytain_sync::sync_account`] with a 200-message-per-folder cap.
/// 3. Emit one [`SyncEvent`] per folder outcome (success → `FolderSynced`,
///    failure → `FolderError`).
///
/// Per-account failures (auth refresh fail, dial fail) are logged
/// and the engine moves on to the next account — one broken account
/// shouldn't block sync for the others.
pub fn spawn(app: AppHandle) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_startup_sync(&app).await {
            warn!("sync engine startup failed: {e}");
        }
    })
}

async fn run_startup_sync(app: &AppHandle) -> Result<(), String> {
    let state: tauri::State<'_, AppState> = app.state();
    let accounts = {
        let db = state.db.lock().await;
        repos::accounts::list(&*db)
            .await
            .map_err(|e| format!("list accounts: {e}"))?
    };
    info!(count = accounts.len(), "sync engine: bootstrap pass");

    let blobs = BlobStore::new(state.data_dir.join("blobs"));

    for account in accounts {
        let backend = match backend_factory::get_or_open(&state, &account.id).await {
            Ok(b) => b,
            Err(e) => {
                warn!(account = %account.id.0, "open backend: {e}");
                continue;
            }
        };

        let db = state.db.lock().await;
        let outcomes = match capytain_sync::sync_account(
            &*db,
            backend.as_ref(),
            Some(&blobs),
            Some(200),
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!(account = %account.id.0, "sync_account: {e}");
                continue;
            }
        };
        drop(db);

        for outcome in outcomes {
            let event = match outcome.result {
                Ok(report) => SyncEvent::FolderSynced {
                    account: account.id.clone(),
                    folder: outcome.folder_id,
                    added: report.added as u32,
                    updated: report.updated as u32,
                    flag_updates: report.flag_updates as u32,
                    removed: report.removed as u32,
                },
                Err(e) => SyncEvent::FolderError {
                    account: account.id.clone(),
                    folder: outcome.folder_id,
                    error: format!("{e}"),
                },
            };
            if let Err(e) = app.emit(SYNC_EVENT, &event) {
                warn!("emit sync_event: {e}");
            }
        }
    }
    Ok(())
}
