// SPDX-License-Identifier: Apache-2.0

//! History-sync ("Pull full mail history") commands.
//!
//! - `history_sync_start({ account, folder })` — kick off a backfill
//!   for one folder. The driver runs in the background; the UI tracks
//!   progress via `SyncEvent::HistorySyncProgress`.
//! - `history_sync_cancel({ account, folder })` — flip the in-memory
//!   cancel token. The driver bails between chunks, persisting a
//!   `canceled` row.
//! - `history_sync_list({ account })` — current state of every
//!   tracked folder for one account, used to render the Settings
//!   pane.
//!
//! Resumption across app restarts is handled by `sync_engine` which
//! re-spawns any row left in a non-terminal state (`running`,
//! `pending`, or `error`) at boot. See [`resume_running_jobs`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// RAII guard that bumps `AppState::pull_in_progress` on construct
/// and decrements it on drop. Decrements on every exit path
/// (success, `?` error, panic) so `messages_search` never wedges in
/// the "indexing in progress" state if a pull task crashes.
struct PullGuard {
    counter: Arc<AtomicU32>,
}

impl PullGuard {
    fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

use qsl_core::{AccountId, FolderId, MailBackend};
use qsl_ipc::{HistorySyncStatus, IpcResult, SyncEvent};
use qsl_storage::repos::{
    folders as folders_repo, history_sync as history_repo,
    history_sync::{HistorySyncRow, HistorySyncStatus as RepoStatus},
    messages as messages_repo,
};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::backend_factory;
use crate::state::AppState;
use crate::sync_engine::SYNC_EVENT;

#[derive(Debug, Deserialize)]
pub struct HistorySyncStartInput {
    pub account: AccountId,
    pub folder: FolderId,
}

#[derive(Debug, Deserialize)]
pub struct HistorySyncCancelInput {
    pub account: AccountId,
    pub folder: FolderId,
}

#[derive(Debug, Deserialize)]
pub struct HistorySyncListInput {
    pub account: AccountId,
}

/// `history_sync_start` — begin pulling every older message for one
/// folder. Idempotent against an already-running job for the same
/// (account, folder): the call is rejected with a clear error.
///
/// On success the row is in `running` and the driver task is
/// spawned. The UI receives a stream of `SyncEvent::HistorySyncProgress`
/// updates terminating in `completed`, `canceled`, or `error`.
#[tauri::command]
pub async fn history_sync_start(
    app: AppHandle,
    state: State<'_, AppState>,
    input: HistorySyncStartInput,
) -> IpcResult<()> {
    let HistorySyncStartInput { account, folder } = input;

    // Reject double-start. We can't usefully run two concurrent
    // pullers against the same folder — they'd race on anchor
    // updates and double-fetch every chunk.
    {
        let cancellers = state.history_cancellers.lock().await;
        if cancellers.contains_key(&(account.clone(), folder.clone())) {
            return Err(qsl_ipc::IpcError::new(
                qsl_ipc::IpcErrorKind::Internal,
                "history sync already running for this folder",
            ));
        }
    }

    // Resolve start anchor + total estimate. The anchor is the
    // lowest UID we've already locally synced; the estimate is the
    // backend's current uidnext (or its IMAP equivalent surfaced
    // through `list_known_ids`).
    let (start_anchor, total_estimate) = compute_start_metrics(&state, &account, &folder).await?;

    if start_anchor <= 1 {
        // Nothing left to backfill. Persist a completed row so the
        // UI sees a clean state and don't bother spawning a task.
        let db = state.db.lock().await;
        history_repo::start(
            &*db,
            &account,
            &folder,
            start_anchor as i64,
            total_estimate.map(|v| v as i64),
        )
        .await?;
        history_repo::set_status(&*db, &account, &folder, RepoStatus::Completed, None).await?;
        drop(db);
        emit_progress_for_account(&app, &state, &account).await;
        return Ok(());
    }

    // Persist the row in `pending`, register the canceller, spawn
    // the driver. The driver flips to `running` once it actually
    // acquires the per-account lock and starts fetching, so a job
    // queued behind another pull on the same account renders as
    // "Queued" in the UI instead of misleadingly showing "Running ·
    // 0 fetched".
    {
        let db = state.db.lock().await;
        history_repo::start(
            &*db,
            &account,
            &folder,
            start_anchor as i64,
            total_estimate.map(|v| v as i64),
        )
        .await?;
        history_repo::set_status(&*db, &account, &folder, RepoStatus::Pending, None).await?;
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut cancellers = state.history_cancellers.lock().await;
        cancellers.insert((account.clone(), folder.clone()), cancel.clone());
    }

    spawn_driver(
        app.clone(),
        account.clone(),
        folder.clone(),
        start_anchor,
        total_estimate,
        cancel,
    );

    Ok(())
}

/// `history_sync_cancel` — stop the pull AND remove the row from the
/// table so the Settings list and the bottom status bar both clear
/// immediately. The driver checks the AtomicBool between chunks and
/// exits cleanly; its own post-cancel `set_status` write becomes a
/// no-op against the now-empty row.
///
/// Why a delete instead of leaving a `canceled` terminal row: the
/// user reported that a Canceled row sticks around as visual debt
/// they have to dismiss by hand. Treating Cancel as "remove the
/// task" matches the mental model of the button — Resume is still
/// available from the start dropdown if they change their mind.
///
/// We emit a synthetic `canceled` `HistorySyncProgress` event after
/// the delete so the UI's status-bar overlay (which keys off
/// terminal status strings) drops the "Pulling history" line right
/// away. Without this synthetic event the bar would stay stuck —
/// the driver's own final emit would find no row and skip.
///
/// No-op for unknown (account, folder) pairs.
#[tauri::command]
pub async fn history_sync_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
    input: HistorySyncCancelInput,
) -> IpcResult<()> {
    let HistorySyncCancelInput { account, folder } = input;
    let token_present = {
        let mut cancellers = state.history_cancellers.lock().await;
        match cancellers.get(&(account.clone(), folder.clone())) {
            Some(token) => {
                token.store(true, Ordering::Relaxed);
                // Drop the canceller now too. The driver's own
                // cleanup (`drop_canceller`) is idempotent so this
                // double-remove is safe and means a brand-new
                // Start on the same folder right after Cancel
                // doesn't trip the "already running" rejection.
                cancellers.remove(&(account.clone(), folder.clone()));
                true
            }
            None => false,
        }
    };
    if !token_present {
        tracing::debug!(
            account = %account.0,
            folder = %folder.0,
            "history sync cancel for non-running folder — ignored"
        );
        return Ok(());
    }

    // Snapshot the row's last-known progress before the delete so
    // the synthetic terminal event below carries truthful counts
    // (rather than zeros that could briefly flash in the UI).
    let snapshot = {
        let db = state.db.lock().await;
        history_repo::get(&*db, &account, &folder)
            .await
            .ok()
            .flatten()
    };

    {
        let db = state.db.lock().await;
        if let Err(e) = history_repo::delete(&*db, &account, &folder).await {
            tracing::warn!(
                account = %account.0,
                folder = %folder.0,
                "history sync cancel: delete row failed: {e}"
            );
        }
    }

    // Synthetic terminal event — clears the status-bar overlay and
    // bumps the Settings tick so the panel refetches and drops the
    // row from the list. Carries `status="canceled"` because that's
    // the variant `HistoryActivity::from_event` recognises as a
    // clear-the-bar terminal state.
    let event = SyncEvent::HistorySyncProgress {
        account: account.clone(),
        folder: folder.clone(),
        status: RepoStatus::Canceled.as_str().to_string(),
        fetched: snapshot.as_ref().map(|r| r.fetched).unwrap_or(0),
        total_estimate: snapshot
            .as_ref()
            .and_then(|r| r.total_estimate.map(|v| v as u32)),
        last_error: None,
    };
    if let Err(e) = app.emit(SYNC_EVENT, &event) {
        tracing::warn!("emit history_sync cancel terminal event: {e}");
    }

    tracing::info!(
        account = %account.0,
        folder = %folder.0,
        "history sync cancel requested — row deleted"
    );
    Ok(())
}

/// `history_sync_list` — every history-sync row for one account.
/// Rendered in Settings as a per-folder progress / start-button list.
#[tauri::command]
pub async fn history_sync_list(
    state: State<'_, AppState>,
    input: HistorySyncListInput,
) -> IpcResult<Vec<HistorySyncStatus>> {
    let HistorySyncListInput { account } = input;
    let db = state.db.lock().await;
    let rows = history_repo::list_by_account(&*db, &account).await?;
    let mut folder_label_cache: HashMap<FolderId, String> = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let label = match folder_label_cache.get(&row.folder_id) {
            Some(s) => s.clone(),
            None => {
                let label = match folders_repo::get(&*db, &row.folder_id).await {
                    Ok(f) => f.name,
                    Err(_) => row.folder_id.0.clone(),
                };
                folder_label_cache.insert(row.folder_id.clone(), label.clone());
                label
            }
        };
        out.push(history_row_to_ipc(row, label));
    }
    Ok(out)
}

/// Spawn the per-folder driver on Tauri's async runtime. The
/// closure owns its own DB handle (via `state.sync_db`) so it
/// doesn't hold the IPC mutex while paging chunks. Acquires the
/// per-account history-sync mutex before opening the backend so
/// concurrent pulls on the same account queue cleanly instead of
/// racing on the cached IMAP session.
fn spawn_driver(
    app: AppHandle,
    account: AccountId,
    folder: FolderId,
    start_anchor: u64,
    total_estimate: Option<u64>,
    cancel: Arc<AtomicBool>,
) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();

        // Mark pending so a row that's queued behind another pull
        // on the same account renders as "Queued" in the UI rather
        // than "Running · 0 fetched". Idempotent if already
        // pending (e.g. from `history_sync_start`), and idempotent
        // for resumed rows that came in as `running`.
        {
            let db = state.sync_db.lock().await;
            if let Err(e) =
                history_repo::set_status(&*db, &account, &folder, RepoStatus::Pending, None).await
            {
                tracing::warn!(
                    account = %account.0,
                    folder = %folder.0,
                    "history sync: persist pending status failed: {e}"
                );
            }
        }
        emit_progress_for_account(&app, &state, &account).await;

        // Per-account serialization. Each account's cached IMAP
        // session can only run one command at a time; without this
        // gate, two parallel pulls on the same account would
        // interleave their chunk fetches, halving each pull's
        // throughput. Held for the entire driver run; released on
        // exit (terminal status, cancel, or error).
        let account_lock = {
            let mut map = state.history_account_locks.lock().await;
            map.entry(account.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _account_guard = account_lock.lock().await;

        // We've acquired the per-account lock — flip status from
        // `pending` to `running` so the UI shows "Running" instead
        // of "Queued". `pull_history` will set this again at its
        // top, but doing it here means the transition is visible
        // before the first DB call inside the driver fires its
        // own progress event.
        {
            let db = state.sync_db.lock().await;
            if let Err(e) =
                history_repo::set_status(&*db, &account, &folder, RepoStatus::Running, None).await
            {
                tracing::warn!(
                    account = %account.0,
                    folder = %folder.0,
                    "history sync: persist running status failed: {e}"
                );
            }
        }
        emit_progress_for_account(&app, &state, &account).await;

        // Re-check cancel after the queue wait — the user may have
        // canceled this folder while we were waiting for an earlier
        // pull on the same account to finish. No point opening the
        // backend just to bail in the first loop iteration.
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(
                account = %account.0,
                folder = %folder.0,
                "history sync canceled while queued"
            );
            let db = state.sync_db.lock().await;
            let _ =
                history_repo::set_status(&*db, &account, &folder, RepoStatus::Canceled, None).await;
            drop(db);
            drop_canceller(&state, &account, &folder).await;
            emit_progress_for_account(&app, &state, &account).await;
            return;
        }

        let backend = match backend_factory::get_or_open(&state, &account).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    account = %account.0,
                    folder = %folder.0,
                    "history sync: open backend failed: {e}"
                );
                let db = state.sync_db.lock().await;
                let _ = history_repo::set_status(
                    &*db,
                    &account,
                    &folder,
                    RepoStatus::Error,
                    Some(&e.to_string()),
                )
                .await;
                drop(db);
                drop_canceller(&state, &account, &folder).await;
                emit_progress_for_account(&app, &state, &account).await;
                return;
            }
        };

        let outcome = run_driver(
            &app,
            backend.as_ref(),
            &state,
            &account,
            &folder,
            start_anchor,
            total_estimate,
            cancel,
        )
        .await;

        if let Err(e) = outcome {
            tracing::warn!(
                account = %account.0,
                folder = %folder.0,
                "history sync driver error: {e}"
            );
        }
        drop_canceller(&state, &account, &folder).await;
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_driver(
    app: &AppHandle,
    backend: &dyn MailBackend,
    state: &AppState,
    account: &AccountId,
    folder: &FolderId,
    start_anchor: u64,
    total_estimate: Option<u64>,
    cancel: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Pass the Mutex<TursoConn> handle through; `pull_history` locks
    // per-chunk so the live sync engine and other tasks can use the
    // same connection while we wait on IMAP fetches (which dominate
    // the wall-clock time per chunk).
    let db_arc = state.sync_db.clone();

    // Mark a pull as in-flight for the duration of this driver. The
    // guard's `Drop` decrements on every exit path (Ok, Err, panic),
    // which is what `messages_search` reads to short-circuit a
    // doomed `fts_match()` against the dropped FTS index. The
    // refcount supports concurrent pulls on different accounts.
    let _pull_guard = PullGuard::new(state.pull_in_progress.clone());
    let app_for_progress = app.clone();
    let account_for_progress = account.clone();
    let folder_for_progress = folder.clone();
    let estimate_for_progress = total_estimate;

    let mut last_emitted_chunk: i64 = -1;
    let progress_cb = move |p: qsl_sync::history::HistoryProgress| {
        // Coalesce progress updates: emit roughly every chunk
        // (already chunked at the driver level) but skip duplicates
        // when a retry produces an identical state.
        let cur = p.fetched_total as i64;
        if !p.finished && cur == last_emitted_chunk {
            return;
        }
        last_emitted_chunk = cur;

        let status = if p.finished {
            // The driver has already persisted a terminal row. Look
            // it up so the UI sees the right label.
            "in-flight"
        } else {
            "running"
        };

        let event = SyncEvent::HistorySyncProgress {
            account: account_for_progress.clone(),
            folder: folder_for_progress.clone(),
            status: status.to_string(),
            fetched: p.fetched_total,
            total_estimate: estimate_for_progress.map(|v| v as u32),
            last_error: None,
        };
        if let Err(e) = app_for_progress.emit(SYNC_EVENT, &event) {
            tracing::warn!("emit history_sync progress: {e}");
        }
    };

    let result = qsl_sync::history::pull_history(
        db_arc,
        backend,
        account,
        folder,
        start_anchor,
        total_estimate,
        cancel,
        progress_cb,
    )
    .await;

    // After the driver returns, emit one final event carrying the
    // *persisted* terminal status so the UI has a clean handoff
    // (the in-flight progress callback uses placeholder "running"
    // labels, not the final canceled / completed / error).
    let final_row = {
        let db = state.db.lock().await;
        history_repo::get(&*db, account, folder)
            .await
            .ok()
            .flatten()
    };
    if let Some(row) = final_row {
        let event = SyncEvent::HistorySyncProgress {
            account: account.clone(),
            folder: folder.clone(),
            status: row.status.as_str().to_string(),
            fetched: row.fetched,
            total_estimate: row.total_estimate.map(|v| v as u32),
            last_error: row.last_error.clone(),
        };
        if let Err(e) = app.emit(SYNC_EVENT, &event) {
            tracing::warn!("emit history_sync terminal: {e}");
        }
    }

    if let Err(e) = result {
        return Err(Box::new(e));
    }
    Ok(())
}

async fn drop_canceller(state: &AppState, account: &AccountId, folder: &FolderId) {
    let mut cancellers = state.history_cancellers.lock().await;
    cancellers.remove(&(account.clone(), folder.clone()));
}

/// Re-emit current state for every history-sync row in `account`.
/// Used after a synchronous start that completes immediately, and
/// after a backend-open failure that doesn't go through the live
/// driver loop.
async fn emit_progress_for_account(app: &AppHandle, state: &AppState, account: &AccountId) {
    let db = state.db.lock().await;
    let rows = match history_repo::list_by_account(&*db, account).await {
        Ok(r) => r,
        Err(_) => return,
    };
    drop(db);
    for row in rows {
        let event = SyncEvent::HistorySyncProgress {
            account: row.account_id.clone(),
            folder: row.folder_id.clone(),
            status: row.status.as_str().to_string(),
            fetched: row.fetched,
            total_estimate: row.total_estimate.map(|v| v as u32),
            last_error: row.last_error.clone(),
        };
        if let Err(e) = app.emit(SYNC_EVENT, &event) {
            tracing::warn!("emit history_sync rebroadcast: {e}");
        }
    }
}

/// Compute the start anchor + an upper bound on remaining history.
/// Returns (anchor, estimate). Anchor is the lowest UID currently
/// in storage for `folder`; estimate is `anchor - 1` (the count of
/// unsynced UIDs below it).
async fn compute_start_metrics(
    state: &AppState,
    account: &AccountId,
    folder: &FolderId,
) -> IpcResult<(u64, Option<u64>)> {
    // Prefer the persisted history-sync row's anchor when it's a
    // resume — restart-from-where-we-left-off is the right semantics.
    let db = state.db.lock().await;
    if let Some(row) = history_repo::get(&*db, account, folder).await? {
        if let Some(anchor) = row.anchor_uid {
            if matches!(
                row.status,
                RepoStatus::Pending
                    | RepoStatus::Running
                    | RepoStatus::Canceled
                    | RepoStatus::Error
            ) && anchor > 1
            {
                return Ok((anchor as u64, row.total_estimate.map(|v| v as u64)));
            }
        }
    }

    let ids = messages_repo::list_ids_by_folder(&*db, folder).await?;
    drop(db);
    let lowest_uid = ids
        .iter()
        .filter_map(|m| qsl_imap_client::MessageRef::decode(m).ok())
        .map(|r| u64::from(r.uid))
        .min();
    let Some(anchor) = lowest_uid else {
        // Empty folder locally — treat as "nothing to backfill yet";
        // user should let the bootstrap sync land first.
        return Err(qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Internal,
            "no local messages in this folder yet — wait for the bootstrap sync to finish",
        ));
    };
    let estimate = anchor.saturating_sub(1);
    Ok((anchor, Some(estimate)))
}

/// Re-spawn every history-sync row left in a non-terminal state at
/// app boot. Called from `sync_engine` after the bootstrap pass so any
/// pull that was in flight when the previous app process exited resumes
/// from its last persisted anchor.
///
/// Picks up `running` (clean shutdown mid-pull), `pending` (app exited
/// after the row was created but before the driver started fetching),
/// and `error` (prior fatal failure that the user may want auto-retried;
/// they can cancel from the UI if they don't). `canceled` rows are
/// deleted at cancel time so they never reach this path; `completed`
/// rows are no-ops.
///
/// Errors fetching the row list or re-opening backends are logged
/// but never propagated — partial resume is better than no resume.
pub async fn resume_running_jobs(app: &AppHandle) {
    let state = app.state::<AppState>();
    let rows = {
        let db = state.sync_db.lock().await;
        let mut acc: Vec<HistorySyncRow> = Vec::new();
        for status in [RepoStatus::Running, RepoStatus::Pending, RepoStatus::Error] {
            match history_repo::list_by_status(&*db, status).await {
                Ok(rs) => acc.extend(rs),
                Err(e) => {
                    tracing::warn!(
                        status = status.as_str(),
                        "history sync resume: list_by_status: {e}"
                    );
                }
            }
        }
        acc
    };
    if rows.is_empty() {
        return;
    }
    tracing::info!(count = rows.len(), "resuming history sync jobs");
    for row in rows {
        let Some(anchor) = row.anchor_uid else {
            continue;
        };
        if anchor <= 1 {
            // Nothing left — flip to completed.
            let db = state.sync_db.lock().await;
            let _ = history_repo::set_status(
                &*db,
                &row.account_id,
                &row.folder_id,
                RepoStatus::Completed,
                None,
            )
            .await;
            continue;
        }
        let already_running = {
            let cancellers = state.history_cancellers.lock().await;
            cancellers.contains_key(&(row.account_id.clone(), row.folder_id.clone()))
        };
        if already_running {
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut cancellers = state.history_cancellers.lock().await;
            cancellers.insert(
                (row.account_id.clone(), row.folder_id.clone()),
                cancel.clone(),
            );
        }
        spawn_driver(
            app.clone(),
            row.account_id.clone(),
            row.folder_id.clone(),
            anchor as u64,
            row.total_estimate.map(|v| v as u64),
            cancel,
        );
    }
}

fn history_row_to_ipc(row: HistorySyncRow, folder_label: String) -> HistorySyncStatus {
    HistorySyncStatus {
        account: row.account_id,
        folder: row.folder_id,
        folder_label,
        status: row.status.as_str().to_string(),
        fetched: row.fetched,
        total_estimate: row.total_estimate.map(|v| v as u32),
        last_error: row.last_error,
        started_at: row.started_at.timestamp(),
        updated_at: row.updated_at.timestamp(),
        completed_at: row.completed_at.map(|t| t.timestamp()),
    }
}
