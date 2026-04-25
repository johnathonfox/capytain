// SPDX-License-Identifier: Apache-2.0

//! Capytain sync engine.
//!
//! Owns the top-level sync loop. Phase 1 Week 9 lands the per-folder
//! header sync orchestrator extracted from `mailcli`; subsequent weeks
//! grow it into a multi-folder daemon (one task per folder, one mpsc
//! event channel) plus the lazy-body-fetch path that `messages_get`
//! triggers when a reader-pane request arrives for a header-only row.
//!
//! The crate depends on `capytain-storage` and the `MailBackend` trait
//! from `capytain-core`. It deliberately knows nothing about IMAP- or
//! JMAP-specific quirks: a backend either returns the right shape or
//! it raises a `MailError` the caller can act on.

pub mod outbox_drain;
pub mod threading;

use thiserror::Error;
use tracing::{debug, instrument, warn};

use capytain_core::{Folder, MailBackend, MailError, MessageHeaders, StorageError};
use capytain_storage::{
    repos::{folders, messages, sync_states},
    BlobStore, DbConn,
};

/// Outcome of a single [`sync_folder`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Headers newly inserted this cycle.
    pub added: usize,
    /// Already-known headers re-fetched and updated in place
    /// (full re-fetch path on a non-CONDSTORE response, or a header
    /// whose envelope changed).
    pub updated: usize,
    /// Already-known messages whose flags moved per the CONDSTORE
    /// `CHANGEDSINCE` pass. Distinct from `updated` because a flag
    /// update only touches the `flags_json` column — no full
    /// header re-fetch is required, so the engine applies them via
    /// `messages::update_flags`.
    pub flag_updates: usize,
    /// Server-side deletions the backend reported via `removed`.
    pub removed: usize,
    /// Bodies successfully fetched + persisted to the blob store this
    /// cycle. Always `<= added` since the body-fetch pass only runs
    /// for newly-inserted headers.
    pub bodies_fetched: usize,
    /// Body fetches that failed and were logged + skipped (transient
    /// network blip, UIDVALIDITY mismatch, parse failure on this
    /// specific message). Failed bodies are retried on the next
    /// `sync_folder` cycle that sees the message again, so this is
    /// non-fatal.
    pub bodies_failed: usize,
}

/// Errors from the sync engine. `MailError` covers protocol /
/// transport failures from the backend; `StorageError` covers local
/// persistence failures. The engine doesn't introduce new error
/// variants of its own — every failure mode already has a home in
/// one of the two layers it sits between.
#[derive(Debug, Error)]
pub enum SyncError {
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Run one delta-sync cycle for `folder` against `backend`, persisting
/// the headers and the new sync cursor through `conn`. When `blobs`
/// is `Some`, also runs a body-fetch pass for newly-inserted
/// messages: `MailBackend::fetch_raw_message` → `BlobStore::put` →
/// `messages::set_body_path`.
///
/// The flow:
/// 1. Upsert the folder row so `sync_states` has somewhere to hang
///    its FK.
/// 2. Read the prior sync cursor (`None` on first run → full fetch
///    bounded by `limit`).
/// 3. Ask the backend for the delta. The IMAP adapter raises a
///    `MailError::Protocol` on UIDVALIDITY mismatch; that surfaces
///    here as `SyncError::Mail` and the caller decides whether to
///    clear the cursor and retry.
/// 4. Upsert each returned header and persist the new cursor.
/// 5. Apply the CONDSTORE `flag_updates` deltas via
///    `messages::update_flags`. Updates targeting an unknown ID
///    (cache out of sync) are logged and skipped rather than
///    failing the cycle.
/// 6. If `blobs` is `Some`, fetch raw bytes for each newly-inserted
///    header and stash them in the blob store. Per-message failures
///    here are logged + counted (`bodies_failed`) but don't fail the
///    cycle — the next `sync_folder` call retries any header without
///    a `body_path`.
#[instrument(skip_all, fields(folder = %folder.id.0))]
pub async fn sync_folder(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
    blobs: Option<&BlobStore>,
    folder: &Folder,
    limit: Option<u32>,
) -> Result<SyncReport, SyncError> {
    match folders::find(conn, &folder.id).await? {
        Some(_) => folders::update(conn, folder).await?,
        None => folders::insert(conn, folder).await?,
    }

    let prior = sync_states::get(conn, &folder.id).await?;

    let result = backend
        .list_messages(&folder.id, prior.as_ref(), limit)
        .await?;

    let mut report = SyncReport::default();
    let mut new_headers: Vec<MessageHeaders> = Vec::new();
    for h in &result.messages {
        match messages::find(conn, &h.id).await? {
            Some(_) => {
                messages::update(conn, h, None).await?;
                report.updated += 1;
            }
            None => {
                messages::insert(conn, h, None).await?;
                // Thread assembly runs immediately after the row
                // lands so subsequent inserts in this same cycle
                // see the thread_id we just minted via the
                // `find_by_rfc822_id` chain. Failures are logged
                // and skipped — a missing thread_id is recoverable
                // (the message just won't group), unlike a missing
                // header row which is a hard cache bug.
                if let Err(e) = threading::attach_to_thread(conn, h).await {
                    warn!(message = %h.id.0, "thread assembly failed: {e}");
                }
                report.added += 1;
                new_headers.push(h.clone());
            }
        }
    }
    report.removed = result.removed.len();

    for (id, flags) in &result.flag_updates {
        match messages::update_flags(conn, id, flags).await {
            Ok(()) => report.flag_updates += 1,
            Err(StorageError::NotFound) => {
                // The CONDSTORE pass covers UIDs 1..uidnext-1, but the
                // local cache may not have every one of them — earlier
                // bounded syncs only pulled the most recent N. Log
                // and skip; it's not a sync failure.
                debug!(
                    message = %id.0,
                    "flag update for unknown message — skipping"
                );
            }
            Err(e) => return Err(e.into()),
        }
    }

    sync_states::put(conn, &result.new_state).await?;

    if let Some(blobs) = blobs {
        for h in &new_headers {
            match fetch_and_store_body(conn, backend, blobs, h).await {
                Ok(()) => report.bodies_fetched += 1,
                Err(e) => {
                    warn!(
                        message = %h.id.0,
                        "body fetch failed: {e}"
                    );
                    report.bodies_failed += 1;
                }
            }
        }
    }

    debug!(
        added = report.added,
        updated = report.updated,
        flag_updates = report.flag_updates,
        removed = report.removed,
        bodies_fetched = report.bodies_fetched,
        bodies_failed = report.bodies_failed,
        "sync_folder cycle complete"
    );
    Ok(report)
}

/// One folder's slice of a [`sync_account`] cycle.
#[derive(Debug)]
pub struct FolderSyncOutcome {
    pub folder_id: capytain_core::FolderId,
    pub result: Result<SyncReport, SyncError>,
}

/// Run [`sync_folder`] across every folder the backend advertises.
///
/// One-shot multi-folder cycle: discovers folders via
/// `list_folders`, then drives `sync_folder` on each in sequence
/// (the backends serialize on a single connection anyway). Per-
/// folder failures are captured in [`FolderSyncOutcome::result`]
/// rather than aborting the whole cycle — a UIDVALIDITY mismatch on
/// one folder shouldn't take down sync for the rest.
///
/// Returns one outcome per folder in the order the backend returned
/// them. The desktop app's bootstrap calls this on launch; the live
/// sync engine (Week 10) drives it again on each `BackendEvent`
/// (or on a polling timer for backends without push).
#[instrument(skip_all)]
pub async fn sync_account(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
    blobs: Option<&BlobStore>,
    limit_per_folder: Option<u32>,
) -> Result<Vec<FolderSyncOutcome>, SyncError> {
    let folders = backend.list_folders().await?;
    let mut outcomes = Vec::with_capacity(folders.len());
    for folder in folders {
        let folder_id = folder.id.clone();
        let result = sync_folder(conn, backend, blobs, &folder, limit_per_folder).await;
        if let Err(e) = &result {
            warn!(folder = %folder_id.0, "sync_folder failed: {e}");
        }
        outcomes.push(FolderSyncOutcome { folder_id, result });
    }
    Ok(outcomes)
}

/// Fetch the raw bytes of a single message and persist them via the
/// blob store + `body_path` column. Pulled out of [`sync_folder`] so
/// the per-message error path is isolated and the engine can keep
/// going after a single bad fetch.
async fn fetch_and_store_body(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
    blobs: &BlobStore,
    header: &MessageHeaders,
) -> Result<(), SyncError> {
    let raw = backend.fetch_raw_message(&header.id).await?;
    let path = blobs
        .put(&header.account_id, &header.folder_id, &header.id, &raw)
        .await?;
    messages::set_body_path(conn, &header.id, Some(&path.to_string_lossy())).await?;
    Ok(())
}
