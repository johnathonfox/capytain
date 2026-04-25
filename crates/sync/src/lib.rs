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
use tracing::{debug, instrument};

use capytain_core::{Folder, MailBackend, MailError, StorageError};
use capytain_storage::{
    repos::{folders, messages, sync_states},
    DbConn,
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
/// the headers and the new sync cursor through `conn`.
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
///
/// Body fetching is intentionally **not** in this function — it
/// lands in a follow-up PR alongside a `fetch_raw_message` addition
/// to `MailBackend` that returns the bytes the existing
/// `fetch_message` parses and discards.
#[instrument(skip_all, fields(folder = %folder.id.0))]
pub async fn sync_folder(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
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
    for h in &result.messages {
        match messages::find(conn, &h.id).await? {
            Some(_) => {
                messages::update(conn, h, None).await?;
                report.updated += 1;
            }
            None => {
                messages::insert(conn, h, None).await?;
                report.added += 1;
            }
        }
    }
    report.removed = result.removed.len();

    sync_states::put(conn, &result.new_state).await?;

    debug!(
        added = report.added,
        updated = report.updated,
        removed = report.removed,
        "sync_folder cycle complete"
    );
    Ok(report)
}
