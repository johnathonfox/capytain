// SPDX-License-Identifier: Apache-2.0

//! Background sync engine for the desktop shell.
//!
//! Phase 1 Week 10 PR 7a (startup pass) + PR 7b (live IDLE).
//!
//! On app start, [`spawn`] kicks off a tokio task that:
//!
//! 1. Bootstrap pass: walks every account, runs
//!    [`capytain_sync::sync_account`], emits
//!    [`SyncEvent::FolderSynced`] per folder.
//! 2. For each IMAP account, spawns one
//!    [`crate::imap_idle::spawn_watcher`] per discovered folder.
//!    Watchers send [`BackendEvent`]s back over an internal mpsc.
//! 3. Reactive loop: consumes the internal mpsc, debounces 500ms
//!    of activity per (account, folder), then runs
//!    [`capytain_sync::sync_folder`] for the changed folder and
//!    emits [`SyncEvent::FolderSynced`].
//!
//! JMAP accounts get the bootstrap pass but no live watcher; their
//! EventSource push lands in Phase 1 Week 11.

use std::collections::HashMap;
use std::time::Duration;

use capytain_core::{Account, AccountId, BackendEvent, BackendKind, Folder, FolderId};
use capytain_ipc::SyncEvent;
use capytain_storage::{repos, BlobStore};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::backend_factory;
use crate::imap_idle;
use crate::state::AppState;

/// Tauri event name the engine emits on. The UI subscribes via
/// `tauri::event::listen("sync_event", …)`.
pub const SYNC_EVENT: &str = "sync_event";

/// How long the reactive loop coalesces a burst of `BackendEvent`s
/// before triggering a sync. A flag-flip on a marketing newsletter
/// can produce three FETCHes inside ~50ms; debouncing collapses
/// them into a single `sync_folder` call.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Buffer size for the internal `BackendEvent` channel. One value
/// per watcher per debounce window is plenty; the larger we make
/// this the longer a backed-up engine can stall a watcher's
/// `tx.send`.
const EVENT_CHANNEL_BUFFER: usize = 64;

/// Spawn the engine task. Returns immediately; the task runs in the
/// background until the app exits.
pub fn spawn(app: AppHandle) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run(&app).await {
            warn!("sync engine fatal: {e}");
        }
    })
}

async fn run(app: &AppHandle) -> Result<(), String> {
    let accounts = list_accounts(app).await?;
    info!(count = accounts.len(), "sync engine: bootstrap pass");

    // Bootstrap sync + collect (account, folder) pairs to watch.
    let mut watch_targets: Vec<(AccountId, Folder)> = Vec::new();
    for account in &accounts {
        match bootstrap_account(app, account).await {
            Ok(folders) if matches!(account.kind, BackendKind::ImapSmtp) => {
                for folder in folders {
                    watch_targets.push((account.id.clone(), folder));
                }
            }
            Ok(_) => {
                // JMAP — no IDLE; bootstrap covered the initial sync.
                debug!(account = %account.id.0, "skipping IDLE for JMAP account");
            }
            Err(e) => {
                warn!(account = %account.id.0, "bootstrap failed: {e}");
            }
        }
    }

    if watch_targets.is_empty() {
        info!("sync engine: no IMAP folders to watch — exiting");
        return Ok(());
    }

    // Spawn watchers on a shared mpsc.
    let (tx, mut rx) = mpsc::channel::<(AccountId, BackendEvent)>(EVENT_CHANNEL_BUFFER);
    let mut handles = Vec::with_capacity(watch_targets.len());
    for (account_id, folder) in &watch_targets {
        let watcher_tx = tx.clone();
        let account_for_tag = account_id.clone();
        let (forward_tx, mut forward_rx) = mpsc::channel::<BackendEvent>(8);
        // The watcher emits BackendEvent (no account tag); rebroadcast
        // with an account_id attached so the reactive loop can dispatch.
        let forwarder_tx = watcher_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = forward_rx.recv().await {
                if forwarder_tx
                    .send((account_for_tag.clone(), ev))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        handles.push(imap_idle::spawn_watcher(
            app.clone(),
            account_id.clone(),
            folder.id.clone(),
            forward_tx,
        ));
    }
    // Drop the engine's copy of `tx` so `rx.recv()` returns `None`
    // when every watcher exits — otherwise the loop would hang
    // forever.
    drop(tx);
    info!(
        count = handles.len(),
        "sync engine: live IDLE watchers spawned"
    );

    // Index the targets by (account, folder) for fast lookup.
    let folder_by_id: HashMap<(AccountId, FolderId), Folder> = watch_targets
        .into_iter()
        .map(|(acct, folder)| ((acct, folder.id.clone()), folder))
        .collect();

    // Reactive loop with per-(account, folder) debouncing.
    let mut pending: HashMap<(AccountId, FolderId), tokio::time::Instant> = HashMap::new();
    let blobs = {
        let state: tauri::State<'_, AppState> = app.state();
        BlobStore::new(state.data_dir.join("blobs"))
    };

    loop {
        // Wait for either a new event or for the next debounced
        // sync to fire.
        let next_deadline = pending.values().min().copied();
        let timeout_fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            match next_deadline {
                Some(t) => Box::pin(tokio::time::sleep_until(t)),
                None => Box::pin(std::future::pending::<()>()),
            };

        tokio::select! {
            evt = rx.recv() => {
                match evt {
                    None => {
                        info!("sync engine: all watchers exited");
                        return Ok(());
                    }
                    Some((account_id, BackendEvent::FolderChanged { folder })) => {
                        let key = (account_id, folder);
                        pending.insert(key, tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Some((account_id, BackendEvent::ConnectionLost)) => {
                        debug!(account = %account_id.0, "watcher reports ConnectionLost");
                    }
                    Some((account_id, BackendEvent::ConnectionRestored)) => {
                        debug!(account = %account_id.0, "watcher reports ConnectionRestored");
                    }
                    Some((_, other)) => {
                        debug!(event = ?other, "ignoring unhandled BackendEvent");
                    }
                }
            }
            _ = timeout_fut => {
                let now = tokio::time::Instant::now();
                let due: Vec<_> = pending
                    .iter()
                    .filter(|(_, t)| **t <= now)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in due {
                    pending.remove(&key);
                    let (account_id, _folder_id) = &key;
                    let Some(folder) = folder_by_id.get(&key) else { continue; };
                    sync_one_folder(app, &blobs, account_id, folder).await;
                }
            }
        }
    }
}

async fn list_accounts(app: &AppHandle) -> Result<Vec<Account>, String> {
    let state: tauri::State<'_, AppState> = app.state();
    let db = state.db.lock().await;
    repos::accounts::list(&*db)
        .await
        .map_err(|e| format!("list accounts: {e}"))
}

/// Run the initial sync_account pass for one account and emit the
/// per-folder events. Returns the list of folders the backend
/// advertised so the engine can spawn watchers for them.
async fn bootstrap_account(app: &AppHandle, account: &Account) -> Result<Vec<Folder>, String> {
    let state: tauri::State<'_, AppState> = app.state();
    let backend = backend_factory::get_or_open(&state, &account.id)
        .await
        .map_err(|e| format!("open backend: {e}"))?;

    let blobs = BlobStore::new(state.data_dir.join("blobs"));

    let db = state.db.lock().await;
    let outcomes = capytain_sync::sync_account(&*db, backend.as_ref(), Some(&blobs), Some(200))
        .await
        .map_err(|e| format!("sync_account: {e}"))?;
    drop(db);

    let mut folders = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        let event = match &outcome.result {
            Ok(report) => SyncEvent::FolderSynced {
                account: account.id.clone(),
                folder: outcome.folder_id.clone(),
                added: report.added as u32,
                updated: report.updated as u32,
                flag_updates: report.flag_updates as u32,
                removed: report.removed as u32,
            },
            Err(e) => SyncEvent::FolderError {
                account: account.id.clone(),
                folder: outcome.folder_id.clone(),
                error: format!("{e}"),
            },
        };
        if let Err(e) = app.emit(SYNC_EVENT, &event) {
            warn!("emit sync_event: {e}");
        }

        // Build a minimal Folder for the watch list. We only need
        // the id; the watcher uses the folder path string from
        // FolderId, not the rest.
        if outcome.result.is_ok() {
            folders.push(Folder {
                id: outcome.folder_id.clone(),
                account_id: account.id.clone(),
                name: outcome.folder_id.0.clone(),
                path: outcome.folder_id.0.clone(),
                role: None,
                unread_count: 0,
                total_count: 0,
                parent: None,
            });
        }
    }
    Ok(folders)
}

/// Re-sync a single folder in response to a debounced
/// `FolderChanged` event, then emit `SyncEvent::FolderSynced`.
async fn sync_one_folder(
    app: &AppHandle,
    blobs: &BlobStore,
    account_id: &AccountId,
    folder: &Folder,
) {
    let state: tauri::State<'_, AppState> = app.state();
    let backend = match backend_factory::get_or_open(&state, account_id).await {
        Ok(b) => b,
        Err(e) => {
            warn!(account = %account_id.0, "open backend for refresh: {e}");
            return;
        }
    };

    let db = state.db.lock().await;
    let result =
        capytain_sync::sync_folder(&*db, backend.as_ref(), Some(blobs), folder, Some(200)).await;
    drop(db);

    let event = match result {
        Ok(report) => {
            debug!(
                account = %account_id.0,
                folder = %folder.id.0,
                added = report.added,
                flag_updates = report.flag_updates,
                "live sync_folder"
            );
            SyncEvent::FolderSynced {
                account: account_id.clone(),
                folder: folder.id.clone(),
                added: report.added as u32,
                updated: report.updated as u32,
                flag_updates: report.flag_updates as u32,
                removed: report.removed as u32,
            }
        }
        Err(e) => SyncEvent::FolderError {
            account: account_id.clone(),
            folder: folder.id.clone(),
            error: format!("{e}"),
        },
    };
    if let Err(e) = app.emit(SYNC_EVENT, &event) {
        warn!("emit sync_event: {e}");
    }
}
