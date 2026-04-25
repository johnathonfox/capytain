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
use crate::jmap_push;
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

    // Bootstrap sync + collect watchers. IMAP accounts get one
    // watcher per folder (FolderChanged events); JMAP accounts get
    // one watcher per account (AccountChanged events).
    let mut imap_targets: Vec<(AccountId, Folder)> = Vec::new();
    let mut jmap_accounts: Vec<AccountId> = Vec::new();
    for account in &accounts {
        match bootstrap_account(app, account).await {
            Ok(folders) => match account.kind {
                BackendKind::ImapSmtp => {
                    for folder in folders {
                        imap_targets.push((account.id.clone(), folder));
                    }
                }
                BackendKind::Jmap => {
                    jmap_accounts.push(account.id.clone());
                }
                _ => {
                    debug!(account = %account.id.0, kind = ?account.kind, "no live watcher for backend kind");
                }
            },
            Err(e) => {
                warn!(account = %account.id.0, "bootstrap failed: {e}");
            }
        }
    }

    if imap_targets.is_empty() && jmap_accounts.is_empty() {
        info!("sync engine: nothing to watch — exiting");
        return Ok(());
    }

    // Single shared mpsc for both IMAP and JMAP watchers. Each
    // forwarder tags its watcher's BackendEvent with the source
    // account_id so the reactive loop can dispatch.
    let (tx, mut rx) = mpsc::channel::<(AccountId, BackendEvent)>(EVENT_CHANNEL_BUFFER);
    let mut watcher_count = 0usize;

    for (account_id, folder) in &imap_targets {
        let forward_tx = spawn_forwarder(account_id.clone(), tx.clone());
        let _handle = imap_idle::spawn_watcher(
            app.clone(),
            account_id.clone(),
            folder.id.clone(),
            forward_tx,
        );
        watcher_count += 1;
    }
    for account_id in &jmap_accounts {
        let forward_tx = spawn_forwarder(account_id.clone(), tx.clone());
        let _handle = jmap_push::spawn_watcher(app.clone(), account_id.clone(), forward_tx);
        watcher_count += 1;
    }

    // Drop the engine's copy of `tx` so `rx.recv()` returns `None`
    // when every watcher exits — otherwise the loop would hang
    // forever.
    drop(tx);
    info!(
        watchers = watcher_count,
        imap_folders = imap_targets.len(),
        jmap_accounts = jmap_accounts.len(),
        "sync engine: live watchers spawned"
    );

    // Spawn the outbox drain on a periodic timer. Decoupled from
    // the watcher reactive loop so a wedged STORE doesn't stall
    // sync events. 5 seconds is short enough that a "mark read"
    // click visibly propagates ("within seconds" per the spec
    // exit criterion) and long enough that an idle queue costs
    // nothing.
    {
        let app_for_drain = app.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(e) = drain_outbox_once(&app_for_drain).await {
                    warn!("outbox drain: {e}");
                }
            }
        });
    }

    let folder_by_id: HashMap<(AccountId, FolderId), Folder> = imap_targets
        .into_iter()
        .map(|(acct, folder)| ((acct, folder.id.clone()), folder))
        .collect();

    // Two debounce maps: per-folder for IMAP FolderChanged, per-account
    // for JMAP AccountChanged. Sharing one map would force a
    // sentinel "all folders" key which makes the dispatch logic
    // muddier than it has to be.
    let mut folder_pending: HashMap<(AccountId, FolderId), tokio::time::Instant> = HashMap::new();
    let mut account_pending: HashMap<AccountId, tokio::time::Instant> = HashMap::new();
    let blobs = {
        let state: tauri::State<'_, AppState> = app.state();
        BlobStore::new(state.data_dir.join("blobs"))
    };

    loop {
        let next_deadline = folder_pending
            .values()
            .chain(account_pending.values())
            .min()
            .copied();
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
                        folder_pending.insert(key, tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Some((account_id, BackendEvent::AccountChanged)) => {
                        account_pending.insert(account_id, tokio::time::Instant::now() + DEBOUNCE);
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

                let due_folders: Vec<_> = folder_pending
                    .iter()
                    .filter(|(_, t)| **t <= now)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in due_folders {
                    folder_pending.remove(&key);
                    let (account_id, _folder_id) = &key;
                    let Some(folder) = folder_by_id.get(&key) else { continue; };
                    sync_one_folder(app, &blobs, account_id, folder).await;
                }

                let due_accounts: Vec<_> = account_pending
                    .iter()
                    .filter(|(_, t)| **t <= now)
                    .map(|(k, _)| k.clone())
                    .collect();
                for account_id in due_accounts {
                    account_pending.remove(&account_id);
                    sync_one_account(app, &blobs, &account_id).await;
                }
            }
        }
    }
}

/// Spawn a small forwarder task that re-broadcasts `BackendEvent`
/// from a single watcher onto the engine's shared `(AccountId,
/// BackendEvent)` channel. Returns the per-watcher sender end.
fn spawn_forwarder(
    account: AccountId,
    engine_tx: mpsc::Sender<(AccountId, BackendEvent)>,
) -> mpsc::Sender<BackendEvent> {
    let (forward_tx, mut forward_rx) = mpsc::channel::<BackendEvent>(8);
    tokio::spawn(async move {
        while let Some(ev) = forward_rx.recv().await {
            if engine_tx.send((account.clone(), ev)).await.is_err() {
                return;
            }
        }
    });
    forward_tx
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

/// Re-sync every folder of one account in response to a debounced
/// `AccountChanged` event (JMAP path). Walks `sync_account` and
/// emits one `SyncEvent::FolderSynced` per outcome — same shape the
/// IMAP per-folder path produces, so the UI doesn't have to care
/// which adapter pushed.
async fn sync_one_account(app: &AppHandle, blobs: &BlobStore, account_id: &AccountId) {
    let state: tauri::State<'_, AppState> = app.state();
    let backend = match backend_factory::get_or_open(&state, account_id).await {
        Ok(b) => b,
        Err(e) => {
            warn!(account = %account_id.0, "open backend for account refresh: {e}");
            return;
        }
    };

    let db = state.db.lock().await;
    let outcomes =
        match capytain_sync::sync_account(&*db, backend.as_ref(), Some(blobs), Some(200)).await {
            Ok(o) => o,
            Err(e) => {
                warn!(account = %account_id.0, "live sync_account: {e}");
                return;
            }
        };
    drop(db);

    for outcome in outcomes {
        let event = match outcome.result {
            Ok(report) => {
                debug!(
                    account = %account_id.0,
                    folder = %outcome.folder_id.0,
                    added = report.added,
                    flag_updates = report.flag_updates,
                    "live sync_account folder"
                );
                SyncEvent::FolderSynced {
                    account: account_id.clone(),
                    folder: outcome.folder_id,
                    added: report.added as u32,
                    updated: report.updated as u32,
                    flag_updates: report.flag_updates as u32,
                    removed: report.removed as u32,
                }
            }
            Err(e) => SyncEvent::FolderError {
                account: account_id.clone(),
                folder: outcome.folder_id,
                error: format!("{e}"),
            },
        };
        if let Err(e) = app.emit(SYNC_EVENT, &event) {
            warn!("emit sync_event: {e}");
        }
    }
}

/// Run one outbox-drain pass. Drains up to 32 entries per call;
/// any DLQ transitions get echoed to the UI as `SyncEvent::FolderError`
/// so the user sees a "failed to sync" banner.
async fn drain_outbox_once(app: &AppHandle) -> Result<(), capytain_core::StorageError> {
    let state: tauri::State<'_, AppState> = app.state();
    let resolver = AppHandleResolver { app: app.clone() };
    let db = state.db.lock().await;
    let outcomes = capytain_sync::outbox_drain::drain(&*db, &resolver, 32).await?;
    drop(db);

    for outcome in outcomes {
        match outcome {
            capytain_sync::outbox_drain::DrainOutcome::Sent { id, op_kind } => {
                debug!(id, op_kind, "outbox: sent");
            }
            capytain_sync::outbox_drain::DrainOutcome::Retrying {
                id,
                op_kind,
                attempts_after,
                error,
            } => {
                debug!(
                    id,
                    op_kind, attempts_after, error, "outbox: scheduled retry"
                );
            }
            capytain_sync::outbox_drain::DrainOutcome::DeadLettered { id, op_kind, error } => {
                warn!(id, op_kind, error, "outbox: dead-lettered");
                // Surface as a synthetic FolderError so the UI's
                // existing sync_event listener picks it up. We
                // don't have a folder context here — use a sentinel
                // so the UI banner can still render with the
                // operator-visible error.
                let event = capytain_ipc::SyncEvent::FolderError {
                    account: capytain_core::AccountId(String::new()),
                    folder: capytain_core::FolderId(format!("outbox:{op_kind}")),
                    error: format!("queued mutation failed: {error}"),
                };
                if let Err(e) = app.emit(SYNC_EVENT, &event) {
                    warn!("emit sync_event for DLQ: {e}");
                }
            }
        }
    }
    Ok(())
}

/// `BackendResolver` impl that walks back through the `AppHandle`
/// to reach the cached backend factory. Lets the outbox drain stay
/// in `capytain-sync` (which is backend-agnostic) without that
/// crate having to depend on `capytain-imap-client` /
/// `capytain-jmap-client`.
struct AppHandleResolver {
    app: AppHandle,
}

#[async_trait::async_trait]
impl capytain_sync::outbox_drain::BackendResolver for AppHandleResolver {
    async fn open(
        &self,
        account: &AccountId,
    ) -> Result<std::sync::Arc<dyn capytain_core::MailBackend>, capytain_core::MailError> {
        let state: tauri::State<'_, AppState> = self.app.state();
        backend_factory::get_or_open(&state, account).await
    }
}
