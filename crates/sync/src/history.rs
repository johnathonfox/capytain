// SPDX-License-Identifier: Apache-2.0

//! Full-archive history backfill driver.
//!
//! `pull_history` walks one (account, folder) pair from where the
//! bootstrap sync left off and pages every older message in via the
//! backend's [`MailBackend::pull_history_chunk`] method. State is
//! persisted to `history_sync_state` after every chunk so the work
//! is resumable across app restarts.
//!
//! Wired up by `apps/desktop/src-tauri/src/commands/history_sync.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{info, warn};

use qsl_core::{AccountId, FolderId, HistoryChunk, MailBackend, MessageHeaders};
use qsl_storage::{
    repos::{
        history_sync as history_repo, history_sync::HistorySyncStatus as RepoStatus, messages,
    },
    DbConn, TursoConn,
};

use crate::SyncError;

/// How many headers to ask the backend for per chunk.
///
/// 500 balances throughput against responsiveness — large enough to
/// amortize the per-FETCH SELECT + round-trip overhead, small enough
/// that the user sees a progress tick within ~10s of clicking Start
/// and Cancel takes effect within one chunk. Going higher (we tried
/// 1000) made the first chunk take 30+ seconds on a fresh Gmail
/// account, which reads to the user as "it's not pulling at all".
pub const HISTORY_CHUNK_SIZE: u32 = 500;

/// Sleep between chunks. Kept small so the network connection stays
/// hot; Gmail's "Too many simultaneous connections" gate trips on
/// concurrent IMAP sessions, not on a single session's request rate.
/// Errors-with-backoff still use the longer [`THROTTLE_RECOVERY_MS`].
pub const INTER_CHUNK_DELAY_MS: u64 = 50;

/// Wait this long after a chunk fails before retrying. Compounds with
/// the [`MAX_CHUNK_RETRIES`] cap below.
pub const THROTTLE_RECOVERY_MS: u64 = 30_000;

/// Stop trying after this many consecutive chunk failures and move
/// the row to `error` so the user can decide to restart. Fewer means
/// less wasted bandwidth on a flat-out broken connection; more means
/// we tolerate longer transient outages without the user noticing.
/// 5 covers a multi-minute Wi-Fi flap with the 30-second recovery
/// delay.
pub const MAX_CHUNK_RETRIES: u32 = 5;

/// Reported back to the caller after every chunk. The Tauri shell
/// turns these into `SyncEvent::HistorySyncProgress` events for the
/// UI.
#[derive(Debug, Clone)]
pub struct HistoryProgress {
    pub fetched_total: u32,
    pub anchor_uid: i64,
    pub total_estimate: Option<i64>,
    pub finished: bool,
}

/// Drive a full-history pull for `(account, folder)` against
/// `backend`, persisting headers as they arrive.
///
/// `start_anchor` is the UID strictly above which to fetch. The
/// caller is responsible for picking it: usually `min(local UIDs in
/// folder)` for a fresh start, or the persisted `anchor_uid` for a
/// resume. `total_estimate` is the upper bound for progress display
/// (usually `uidnext - 1` captured at start).
///
/// `db` is locked **per-chunk**, not for the whole pull, so the live
/// sync engine and other commands can keep using the same connection
/// while the IMAP fetch is in flight (which can take seconds per
/// chunk). The IMAP backend has its own per-session mutex so
/// concurrent commands queue cleanly there.
///
/// `cancel` flips when the user clicks cancel from the UI; checked
/// between chunks. `progress` is invoked at start and on every chunk
/// so the UI can render percent-complete; the start invocation gives
/// the user immediate "it's running" feedback before the first chunk
/// returns.
///
/// Returns when the historical tail is exhausted, the cancel flag
/// flips, or an unrecoverable error trips the retry budget. The
/// `history_sync_state` row reflects the terminal status either way.
#[allow(clippy::too_many_arguments)]
pub async fn pull_history<F>(
    db: Arc<Mutex<TursoConn>>,
    backend: &dyn MailBackend,
    account: &AccountId,
    folder: &FolderId,
    start_anchor: u64,
    total_estimate: Option<u64>,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<(), SyncError>
where
    F: FnMut(HistoryProgress),
{
    // Persist the running row up front. Brief lock; release so the
    // sync engine can use the connection while we wait on IMAP.
    {
        let conn = db.lock().await;
        history_repo::start(
            &*conn,
            account,
            folder,
            start_anchor as i64,
            total_estimate.map(|v| v as i64),
        )
        .await?;
    }

    // Emit a starting progress event so the UI shows "0 fetched"
    // immediately on click rather than waiting tens of seconds for
    // the first chunk to land.
    progress(HistoryProgress {
        fetched_total: 0,
        anchor_uid: start_anchor as i64,
        total_estimate: total_estimate.map(|v| v as i64),
        finished: false,
    });

    let mut anchor = start_anchor;
    let mut fetched_total: u32 = 0;
    let mut consecutive_failures: u32 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            info!(
                account = %account.0,
                folder = %folder.0,
                fetched = fetched_total,
                "history sync canceled"
            );
            let conn = db.lock().await;
            history_repo::set_status(&*conn, account, folder, RepoStatus::Canceled, None).await?;
            drop(conn);
            progress(HistoryProgress {
                fetched_total,
                anchor_uid: anchor as i64,
                total_estimate: total_estimate.map(|v| v as i64),
                finished: true,
            });
            return Ok(());
        }

        if anchor <= 1 {
            info!(
                account = %account.0,
                folder = %folder.0,
                fetched = fetched_total,
                "history sync exhausted tail"
            );
            let conn = db.lock().await;
            history_repo::set_status(&*conn, account, folder, RepoStatus::Completed, None).await?;
            drop(conn);
            progress(HistoryProgress {
                fetched_total,
                anchor_uid: 0,
                total_estimate: total_estimate.map(|v| v as i64),
                finished: true,
            });
            return Ok(());
        }

        // Network fetch — explicitly NOT holding the DB lock. This
        // is where the wall-clock seconds go on a big mailbox.
        let chunk = match backend
            .pull_history_chunk(folder, anchor, HISTORY_CHUNK_SIZE)
            .await
        {
            Ok(c) => {
                consecutive_failures = 0;
                c
            }
            Err(e) => {
                consecutive_failures += 1;
                warn!(
                    account = %account.0,
                    folder = %folder.0,
                    anchor,
                    failures = consecutive_failures,
                    "history sync chunk failed: {e}"
                );
                if consecutive_failures >= MAX_CHUNK_RETRIES {
                    let msg = format!("{consecutive_failures} consecutive failures: {e}");
                    let conn = db.lock().await;
                    history_repo::set_status(
                        &*conn,
                        account,
                        folder,
                        RepoStatus::Error,
                        Some(&msg),
                    )
                    .await?;
                    drop(conn);
                    progress(HistoryProgress {
                        fetched_total,
                        anchor_uid: anchor as i64,
                        total_estimate: total_estimate.map(|v| v as i64),
                        finished: true,
                    });
                    return Err(e.into());
                }
                tokio::time::sleep(Duration::from_millis(THROTTLE_RECOVERY_MS)).await;
                continue;
            }
        };

        let HistoryChunk {
            headers,
            next_anchor,
        } = chunk;

        // Persist + update progress in a single locked window.
        let inserted = {
            let conn = db.lock().await;
            let inserted = persist_chunk(&*conn, &headers).await?;
            history_repo::update_progress(&*conn, account, folder, next_anchor as i64, inserted)
                .await?;
            inserted
        };
        fetched_total = fetched_total.saturating_add(inserted);

        let advanced = next_anchor < anchor;
        anchor = next_anchor;

        progress(HistoryProgress {
            fetched_total,
            anchor_uid: anchor as i64,
            total_estimate: total_estimate.map(|v| v as i64),
            finished: false,
        });

        info!(
            account = %account.0,
            folder = %folder.0,
            anchor,
            inserted,
            fetched_total,
            "history chunk persisted"
        );

        // If the backend returned an anchor that didn't move forward
        // (towards 1) and the chunk was empty, we're stuck. Treat as
        // exhausted to avoid an infinite loop. A non-empty chunk
        // with a stuck anchor is also possible if the lowest UID in
        // the chunk equals the previous anchor minus 1 — that's
        // legitimate progress because subsequent loop reads anchor.
        if !advanced && headers.is_empty() {
            let conn = db.lock().await;
            history_repo::set_status(&*conn, account, folder, RepoStatus::Completed, None).await?;
            drop(conn);
            progress(HistoryProgress {
                fetched_total,
                anchor_uid: 0,
                total_estimate: total_estimate.map(|v| v as i64),
                finished: true,
            });
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(INTER_CHUNK_DELAY_MS)).await;
    }
}

/// Persist one chunk of headers — insert if new, skip silently if
/// already known. Returns the count of newly-inserted rows.
///
/// Wraps every chunk's inserts in a single transaction via
/// [`messages::batch_insert_skip_existing`]. Without that, Turso's
/// experimental FTS index (`messages_fts_idx`) rebuilds at every
/// implicit commit, which made a 500-row chunk take 3-6 minutes
/// against a real Gmail account during v0.1 history pulls. Batched
/// commits drop that to seconds, matching what the IMAP-side fetch
/// budget actually deserves.
///
/// Threading and contacts upserts are deliberately skipped on the
/// history-pull hot path: each `attach_to_thread` runs ~3 SQL
/// queries per message, which makes a 100k-message pull spend most
/// of its wall-clock time in serial round-trips for archival mail
/// nobody reads as a thread anyway. The recent-mail threading the
/// user actually sees comes from the bootstrap + live-sync paths,
/// both of which still run threading inline. Pulled-history
/// messages will lack `thread_id` until either a re-sync of the
/// folder triggers `sync_folder`'s heal-on-update path, or a
/// future "rethread historical mail" action runs.
async fn persist_chunk(conn: &dyn DbConn, headers: &[MessageHeaders]) -> Result<u32, SyncError> {
    Ok(messages::batch_insert_skip_existing(conn, headers).await?)
}
