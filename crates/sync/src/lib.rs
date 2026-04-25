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
    /// Headers updated in place (already-known UIDs whose flags or
    /// labels moved).
    pub updated: usize,
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
/// 5. If `blobs` is `Some`, fetch raw bytes for each newly-inserted
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
                report.added += 1;
                new_headers.push(h.clone());
            }
        }
    }
    report.removed = result.removed.len();

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
        removed = report.removed,
        bodies_fetched = report.bodies_fetched,
        bodies_failed = report.bodies_failed,
        "sync_folder cycle complete"
    );
    Ok(report)
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
