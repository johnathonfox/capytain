// SPDX-License-Identifier: Apache-2.0

//! QSL sync engine.
//!
//! Owns the top-level sync loop. Phase 1 Week 9 lands the per-folder
//! header sync orchestrator extracted from `mailcli`; subsequent weeks
//! grow it into a multi-folder daemon (one task per folder, one mpsc
//! event channel) plus the lazy-body-fetch path that `messages_get`
//! triggers when a reader-pane request arrives for a header-only row.
//!
//! The crate depends on `qsl-storage` and the `MailBackend` trait
//! from `qsl-core`. It deliberately knows nothing about IMAP- or
//! JMAP-specific quirks: a backend either returns the right shape or
//! it raises a `MailError` the caller can act on.

pub mod history;
pub mod outbox_drain;
pub mod threading;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{debug, instrument, warn, Level};

use qsl_core::{Folder, MailBackend, MailError, MessageHeaders, StorageError};
use qsl_storage::{
    repos::{contacts, folders, messages, sync_states},
    BlobStore, DbConn,
};

/// Process-local monotonic counter used to mint a `sync_run_id` per
/// [`sync_account`] invocation. Stable for the life of the process so
/// every event emitted under the same sync run can be grouped via
/// the `sync_run_id` tracing field.
static SYNC_RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Slowest single phase observed during the currently-running
/// [`sync_account`] call, by `(phase_label, elapsed)`. Reset at the
/// top of every `sync_account`. Diagnostic-only; not consulted by
/// any code path that affects behavior.
static SLOWEST_OP: Mutex<(String, Duration)> = Mutex::new((String::new(), Duration::ZERO));

/// Bump [`SLOWEST_OP`] if `elapsed` beats the current record. Inline
/// helper so phase timings stay one-line at call sites.
fn note_slowest(phase: &str, elapsed: Duration) {
    if let Ok(mut g) = SLOWEST_OP.lock() {
        if elapsed > g.1 {
            *g = (phase.to_string(), elapsed);
        }
    }
}

fn reset_slowest() {
    if let Ok(mut g) = SLOWEST_OP.lock() {
        *g = (String::new(), Duration::ZERO);
    }
}

/// Mint a fresh sync run id from the same counter `sync_account` uses.
/// Exposed for [`history::pull_history`] so history-pull events
/// share the same id space as live-sync runs.
pub(crate) fn next_history_sync_run_id() -> u64 {
    SYNC_RUN_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn read_slowest() -> (String, Duration) {
    SLOWEST_OP
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| (String::new(), Duration::ZERO))
}

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
#[instrument(skip_all, fields(folder = %folder.id.0, sync_run_id = tracing::field::Empty))]
pub async fn sync_folder(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
    blobs: Option<&BlobStore>,
    folder: &Folder,
    limit: Option<u32>,
) -> Result<SyncReport, SyncError> {
    let folder_wall = Instant::now();
    match folders::find(conn, &folder.id).await? {
        Some(_) => folders::update(conn, folder).await?,
        None => folders::insert(conn, folder).await?,
    }

    let prior = sync_states::get(conn, &folder.id).await?;

    // UIDVALIDITY-changed recovery: if the backend reports the cached
    // cursor's uidvalidity no longer matches what the server SELECT
    // returned, the cached UIDs are meaningless. Clear the cursor and
    // re-call list_messages with `prior = None` (the bounded
    // initial-sync path). The reconciliation pass below still runs
    // because `prior.is_some()` was true *before* recovery — exactly
    // what we want, since `list_known_ids` will return UIDs under the
    // *new* uidvalidity and the diff will prune every stale-UV row
    // that's still hanging around in the local cache. Without this
    // recovery the cursor stays poisoned and every subsequent sync
    // cycle re-errors on the same mismatch, leaving the folder
    // permanently stuck.
    let list_started = Instant::now();
    let result = match backend
        .list_messages(&folder.id, prior.as_ref(), limit)
        .await
    {
        Ok(r) => r,
        Err(MailError::UidValidityChanged {
            folder: changed_folder,
            cached,
            observed,
        }) => {
            warn!(
                folder = %changed_folder,
                cached_uidvalidity = cached,
                observed_uidvalidity = observed,
                "UIDVALIDITY changed; clearing sync cursor and refetching from scratch"
            );
            sync_states::clear(conn, &folder.id).await?;
            backend.list_messages(&folder.id, None, limit).await?
        }
        Err(e) => return Err(e.into()),
    };
    let list_elapsed = list_started.elapsed();
    note_slowest("sync.list_messages", list_elapsed);
    debug!(
        phase = "sync.list_messages",
        folder = %folder.id.0,
        count = result.messages.len(),
        flag_updates = result.flag_updates.len(),
        removed = result.removed.len(),
        elapsed_ms = list_elapsed.as_millis() as u64,
        "phase timing"
    );

    let mut report = SyncReport::default();

    // Phase 1 — classify: probe each message and bucket into new vs
    // updated.  This is the only read-heavy part of the loop; all
    // writes below run in batched transactions.
    let classify_started = Instant::now();
    let mut new_headers: Vec<MessageHeaders> = Vec::new();
    let mut updated_headers: Vec<MessageHeaders> = Vec::new();
    let mut orphaned_existing: Vec<MessageHeaders> = Vec::new();
    for h in &result.messages {
        match messages::find(conn, &h.id).await? {
            Some(existing) => {
                updated_headers.push(h.clone());
                if existing.thread_id.is_none() {
                    orphaned_existing.push(h.clone());
                }
            }
            None => {
                new_headers.push(h.clone());
            }
        }
    }
    let classify_elapsed = classify_started.elapsed();
    note_slowest("sync.classify", classify_elapsed);
    debug!(
        phase = "sync.classify",
        folder = %folder.id.0,
        count = result.messages.len(),
        new = new_headers.len(),
        updated = updated_headers.len(),
        orphaned = orphaned_existing.len(),
        elapsed_ms = classify_elapsed.as_millis() as u64,
        "phase timing"
    );

    // Phase 2 — batch insert new headers in a single transaction.
    let mut insert_elapsed = Duration::ZERO;
    if !new_headers.is_empty() {
        let started = Instant::now();
        let inserted = messages::batch_insert_skip_existing(conn, &new_headers).await?;
        insert_elapsed = started.elapsed();
        note_slowest("sync.batch_insert", insert_elapsed);
        debug!(
            phase = "sync.batch_insert",
            folder = %folder.id.0,
            count = new_headers.len(),
            inserted,
            elapsed_ms = insert_elapsed.as_millis() as u64,
            "phase timing (tx begin->commit observed at call site; storage layer logs inner breakdown)"
        );
        report.added = inserted as usize;
    }

    // Phase 3 — batch update existing headers in a single transaction.
    let mut update_elapsed = Duration::ZERO;
    if !updated_headers.is_empty() {
        let started = Instant::now();
        let updated = messages::batch_update(conn, &updated_headers).await?;
        update_elapsed = started.elapsed();
        note_slowest("sync.batch_update", update_elapsed);
        debug!(
            phase = "sync.batch_update",
            folder = %folder.id.0,
            count = updated_headers.len(),
            updated,
            elapsed_ms = update_elapsed.as_millis() as u64,
            "phase timing"
        );
        report.updated = updated as usize;
    }

    // Phase 4 — heal-on-update threading for orphaned existing rows.
    // Runs after the batch update so the freshly-updated row is visible
    // to the `find_by_rfc822_id` chain inside the resolver.
    let thread_started = Instant::now();
    for h in &orphaned_existing {
        if let Err(e) = threading::attach_to_thread(conn, h).await {
            warn!(message = %h.id.0, "thread heal-on-update failed: {e}");
        }
    }

    // Phase 5 — threading + contacts for newly-inserted headers.
    // Thread assembly runs immediately after the row lands so subsequent
    // inserts in this same cycle see the thread_id we just minted via
    // the `find_by_rfc822_id` chain.  Contacts collection seeds the
    // autocomplete dropdown from every `From:` address.  Both are
    // per-message because they depend on the row being visible to
    // cross-references, but they touch `threads` / `contacts_v1` (not
    // the FTS-indexed columns on `messages`), so they don't trigger
    // the Tantivy rebuild that dominates the hot path.
    let mut contacts_elapsed = Duration::ZERO;
    for h in &new_headers {
        let per_msg_thread = if tracing::enabled!(Level::TRACE) {
            Some(Instant::now())
        } else {
            None
        };
        if let Err(e) = threading::attach_to_thread(conn, h).await {
            warn!(message = %h.id.0, "thread assembly failed: {e}");
        }
        if let Some(start) = per_msg_thread {
            tracing::trace!(
                phase = "sync.attach_to_thread",
                message = %h.id.0,
                elapsed_us = start.elapsed().as_micros() as u64,
                "per-message"
            );
        }
        let contacts_start = Instant::now();
        let now = chrono::Utc::now().timestamp();
        for addr in &h.from {
            if let Err(e) = contacts::upsert_seen(
                conn,
                &addr.address,
                addr.display_name.as_deref(),
                contacts::Source::Inbound,
                now,
            )
            .await
            {
                warn!(message = %h.id.0, "contact upsert failed: {e}");
            }
        }
        contacts_elapsed += contacts_start.elapsed();
    }
    let thread_total = thread_started.elapsed();
    let thread_only = thread_total.saturating_sub(contacts_elapsed);
    note_slowest("sync.threading", thread_only);
    note_slowest("sync.contacts", contacts_elapsed);
    debug!(
        phase = "sync.threading",
        folder = %folder.id.0,
        count = new_headers.len() + orphaned_existing.len(),
        elapsed_ms = thread_only.as_millis() as u64,
        "phase timing"
    );
    debug!(
        phase = "sync.contacts",
        folder = %folder.id.0,
        count = new_headers.len(),
        elapsed_ms = contacts_elapsed.as_millis() as u64,
        "phase timing"
    );
    // Apply backend-reported deletions (JMAP's `Email/changes.destroyed`
    // is the only path that hits this today — IMAP's adapter can't
    // surface VANISHED responses through async-imap, so it falls
    // through to the reconciliation pass below).
    for id in &result.removed {
        match messages::delete(conn, id).await {
            Ok(()) => report.removed += 1,
            Err(StorageError::NotFound) => {
                debug!(message = %id.0, "delete for unknown message — skipping");
            }
            Err(e) => return Err(e.into()),
        }
    }

    // Reconciliation pass: ask the backend for the full live id set
    // for the folder and prune anything in storage that isn't in it.
    // Catches deletes IMAP-without-QRESYNC can't otherwise observe
    // (Gmail) and serves as a belt-and-braces correctness check
    // against JMAP's `Email/changes` (which can miss destroys past a
    // server's state-history retention window).
    //
    // Skipped on the very first sync (`prior` is `None`) — there's
    // nothing local to prune yet, and the bounded initial fetch
    // intentionally leaves history we don't want to misclassify as
    // server-side deletions.
    if prior.is_some() {
        match backend.list_known_ids(&folder.id).await {
            Ok(live_ids) if live_ids.is_empty() => {
                // Backend opted out (default impl returns empty) — we
                // can't safely diff against an "empty" set or we'd
                // wipe the whole folder. Skip silently.
            }
            Ok(live_ids) => {
                use std::collections::HashSet;
                let live: HashSet<&str> = live_ids.iter().map(|id| id.0.as_str()).collect();
                let local = messages::list_ids_by_folder(conn, &folder.id).await?;
                for id in local {
                    if live.contains(id.0.as_str()) {
                        continue;
                    }
                    match messages::delete(conn, &id).await {
                        Ok(()) => report.removed += 1,
                        Err(StorageError::NotFound) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Err(e) => {
                warn!(folder = %folder.id.0, "list_known_ids failed; skipping reconcile: {e}");
            }
        }
    }

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

    let bodies_started = Instant::now();
    if let Some(blobs) = blobs {
        for h in &new_headers {
            let per_msg = if tracing::enabled!(Level::TRACE) {
                Some(Instant::now())
            } else {
                None
            };
            let outcome = fetch_and_store_body(conn, backend, blobs, h).await;
            if let Some(start) = per_msg {
                tracing::trace!(
                    phase = "sync.fetch_and_store_body",
                    message = %h.id.0,
                    elapsed_us = start.elapsed().as_micros() as u64,
                    ok = outcome.is_ok(),
                    "per-message"
                );
            }
            match outcome {
                Ok(()) => report.bodies_fetched += 1,
                Err(e) => {
                    warn!(message = %h.id.0, "body fetch failed: {e}");
                    report.bodies_failed += 1;
                }
            }
        }
    }
    let bodies_elapsed = bodies_started.elapsed();
    note_slowest("sync.bodies", bodies_elapsed);

    let folder_elapsed = folder_wall.elapsed();
    debug!(
        phase = "sync.folder_done",
        folder = %folder.id.0,
        added = report.added,
        updated = report.updated,
        flag_updates = report.flag_updates,
        removed = report.removed,
        bodies_fetched = report.bodies_fetched,
        bodies_failed = report.bodies_failed,
        list_ms = list_elapsed.as_millis() as u64,
        classify_ms = classify_elapsed.as_millis() as u64,
        insert_ms = insert_elapsed.as_millis() as u64,
        update_ms = update_elapsed.as_millis() as u64,
        thread_ms = thread_only.as_millis() as u64,
        contacts_ms = contacts_elapsed.as_millis() as u64,
        bodies_ms = bodies_elapsed.as_millis() as u64,
        wall_ms = folder_elapsed.as_millis() as u64,
        "sync_folder cycle complete"
    );
    Ok(report)
}

/// One folder's slice of a [`sync_account`] cycle.
#[derive(Debug)]
pub struct FolderSyncOutcome {
    pub folder_id: qsl_core::FolderId,
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
#[instrument(skip_all, fields(sync_run_id = tracing::field::Empty))]
pub async fn sync_account(
    conn: &dyn DbConn,
    backend: &dyn MailBackend,
    blobs: Option<&BlobStore>,
    limit_per_folder: Option<u32>,
) -> Result<Vec<FolderSyncOutcome>, SyncError> {
    let sync_run_id = SYNC_RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    tracing::Span::current().record("sync_run_id", sync_run_id);
    reset_slowest();
    let wall = Instant::now();

    debug!(
        phase = "sync.account_start",
        sync_run_id,
        "sync run starting"
    );

    let list_folders_started = Instant::now();
    let folders = backend.list_folders().await?;
    let list_folders_elapsed = list_folders_started.elapsed();
    note_slowest("imap.list_folders", list_folders_elapsed);
    debug!(
        phase = "imap.list_folders",
        sync_run_id,
        count = folders.len(),
        elapsed_ms = list_folders_elapsed.as_millis() as u64,
        "phase timing"
    );

    let mut outcomes = Vec::with_capacity(folders.len());
    let mut total_added = 0usize;
    let mut total_updated = 0usize;
    let mut total_flag_updates = 0usize;
    let mut total_removed = 0usize;
    let mut total_bodies = 0usize;
    let folder_count = folders.len();
    for folder in folders {
        let folder_id = folder.id.clone();
        let folder_span = tracing::debug_span!(
            "folder",
            folder = %folder_id.0,
            sync_run_id = sync_run_id,
        );
        let result = {
            let _enter = folder_span.enter();
            sync_folder(conn, backend, blobs, &folder, limit_per_folder).await
        };
        if let Ok(r) = &result {
            total_added += r.added;
            total_updated += r.updated;
            total_flag_updates += r.flag_updates;
            total_removed += r.removed;
            total_bodies += r.bodies_fetched;
        }
        if let Err(e) = &result {
            warn!(folder = %folder_id.0, "sync_folder failed: {e}");
        }
        outcomes.push(FolderSyncOutcome { folder_id, result });
    }

    let wall_elapsed = wall.elapsed();
    let (slowest_phase, slowest_dur) = read_slowest();
    debug!(
        target: "SYNC_SUMMARY",
        sync_run_id,
        folders = folder_count,
        added = total_added,
        updated = total_updated,
        flag_updates = total_flag_updates,
        removed = total_removed,
        bodies_fetched = total_bodies,
        wall_ms = wall_elapsed.as_millis() as u64,
        list_folders_ms = list_folders_elapsed.as_millis() as u64,
        slowest_phase = %slowest_phase,
        slowest_ms = slowest_dur.as_millis() as u64,
        "SYNC_SUMMARY sync_run_id={} folders={} added={} updated={} flag_updates={} removed={} bodies_fetched={} wall_ms={} list_folders_ms={} slowest_phase={:?} slowest_ms={}",
        sync_run_id,
        folder_count,
        total_added,
        total_updated,
        total_flag_updates,
        total_removed,
        total_bodies,
        wall_elapsed.as_millis() as u64,
        list_folders_elapsed.as_millis() as u64,
        slowest_phase,
        slowest_dur.as_millis() as u64,
    );

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
    let fetch_start = Instant::now();
    let raw = backend.fetch_raw_message(&header.id).await?;
    let fetch_elapsed = fetch_start.elapsed();
    let put_start = Instant::now();
    let path = blobs
        .put(&header.account_id, &header.folder_id, &header.id, &raw)
        .await?;
    let put_elapsed = put_start.elapsed();
    let set_start = Instant::now();
    messages::set_body_path(conn, &header.id, Some(&path.to_string_lossy())).await?;
    let set_elapsed = set_start.elapsed();
    if tracing::enabled!(Level::TRACE) {
        tracing::trace!(
            phase = "sync.body_breakdown",
            message = %header.id.0,
            bytes = raw.len(),
            fetch_us = fetch_elapsed.as_micros() as u64,
            blob_put_us = put_elapsed.as_micros() as u64,
            set_path_us = set_elapsed.as_micros() as u64,
            "per-message body fetch breakdown"
        );
    }
    Ok(())
}
