// SPDX-License-Identifier: Apache-2.0

//! Outbox drain — Phase 1 Week 14.
//!
//! UI mutations (`messages_mark_read`, `messages_flag`,
//! `messages_move`, `messages_delete`) follow the optimistic
//! pattern: apply locally, enqueue an [`outbox_repo`] row, return
//! immediately. This module owns the worker that walks the queue
//! and dispatches each row to the appropriate `MailBackend`
//! method.
//!
//! The worker is invoked by the desktop sync engine on a timer (and
//! eagerly after each mutation; it's a noop when there's nothing
//! due). Per-row failures bump `attempts` and reschedule with
//! exponential backoff; after `MAX_ATTEMPTS` the row enters the
//! dead-letter state and the engine emits a `SyncEvent` the UI
//! shows as a "failed to sync" banner.

use base64::engine::general_purpose::STANDARD as base64_engine;
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use qsl_core::{DraftId, FolderId, MailError, MessageFlags, MessageId, StorageError};
use qsl_storage::{
    repos::{drafts as drafts_repo, outbox as outbox_repo},
    DbConn,
};

/// Op-kind tag used for `update_flags` payloads.
pub const OP_UPDATE_FLAGS: &str = "update_flags";
/// Op-kind tag for `move_messages` payloads.
pub const OP_MOVE: &str = "move_messages";
/// Op-kind tag for `delete_messages` payloads.
pub const OP_DELETE: &str = "delete_messages";
/// Op-kind tag for `submit_message` payloads — sends an outgoing
/// message via the account's submission backend (SMTP for IMAP
/// accounts, JMAP `EmailSubmission/set` for JMAP accounts).
pub const OP_SUBMIT_MESSAGE: &str = "submit_message";
/// Op-kind tag for `save_draft` payloads — uploads the draft RFC 5322
/// bytes to the account's Drafts mailbox (IMAP APPEND with `\Draft`,
/// JMAP `Email/import` with `$draft`). Producers enqueue with a
/// dedup key set to the local draft id so auto-save bursts coalesce
/// to a single pending row instead of stacking one per keystroke.
pub const OP_SAVE_DRAFT: &str = "save_draft";

/// One drained row's outcome — the engine uses this to decide
/// whether to emit a UI event for a failure-to-DLQ transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Row succeeded and was deleted.
    Sent { id: String, op_kind: String },
    /// Row failed but will retry; backoff was scheduled.
    Retrying {
        id: String,
        op_kind: String,
        attempts_after: u32,
        error: String,
    },
    /// Row exceeded `MAX_ATTEMPTS` and entered the DLQ.
    DeadLettered {
        id: String,
        op_kind: String,
        error: String,
    },
}

/// JSON payload for `update_flags` rows. Keep field names stable —
/// they cross the storage/process boundary as a row in `outbox`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateFlagsPayload {
    pub ids: Vec<MessageId>,
    pub add: MessageFlags,
    pub remove: MessageFlags,
}

/// JSON payload for `move_messages` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovePayload {
    pub ids: Vec<MessageId>,
    pub target: FolderId,
}

/// JSON payload for `delete_messages` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletePayload {
    pub ids: Vec<MessageId>,
}

/// JSON payload for `save_draft` rows. Same base64 encoding logic as
/// [`SubmitMessagePayload`] for the same reason — JSON arrays of
/// integers triple the byte count. `draft_id` is the *local* drafts
/// row id (see `qsl-storage::repos::drafts`); it doubles as the
/// outbox row's `dedup_key` so auto-save bursts don't queue up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveDraftPayload {
    pub draft_id: String,
    /// Base64-encoded RFC 5322 bytes.
    pub raw_b64: String,
}

/// JSON payload for `submit_message` rows. The full RFC 5322 byte
/// stream rides along — drains are dispatched against an
/// `Arc<dyn MailBackend>` whose `submit_message(raw_rfc822)` does
/// both SMTP submission and the post-send `APPEND` to Sent. The
/// `message_id` is the angle-bracket-wrapped Message-ID header
/// minted by `qsl_mime::compose::build_rfc5322`; the sync engine
/// uses it to reconcile the eventual server-side row in Sent
/// without double-rendering the same conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitMessagePayload {
    pub message_id: String,
    /// Base64-encoded RFC 5322 bytes. Encoded so the JSON row in
    /// SQLite stays tight — a raw `Vec<u8>` would round-trip as a
    /// JSON array of integers (~3.5x bloat).
    pub raw_b64: String,
}

/// Trait the engine implements to hand the worker a backend on
/// demand. Decoupled from `MailBackend` so the engine can do its
/// per-account dispatch (the outbox row carries the account_id;
/// the resolver hands back a backend for that account).
#[async_trait::async_trait]
pub trait BackendResolver: Send + Sync {
    async fn open(
        &self,
        account: &qsl_core::AccountId,
    ) -> Result<std::sync::Arc<dyn qsl_core::MailBackend>, MailError>;
}

/// Process every row whose `next_attempt_at <= now`. Returns one
/// outcome per row visited so the caller can fan UI events out.
/// `limit` caps work per call so a hung backend can't starve the
/// rest of the engine; 32 is plenty of headroom for the typical
/// burst from a "mark all as read" click.
#[instrument(skip_all)]
pub async fn drain(
    conn: &dyn DbConn,
    resolver: &dyn BackendResolver,
    limit: u32,
) -> Result<Vec<DrainOutcome>, StorageError> {
    let now = Utc::now();
    let due = outbox_repo::list_due(conn, now, limit).await?;
    if due.is_empty() {
        return Ok(Vec::new());
    }
    debug!(count = due.len(), "outbox drain: processing due rows");

    let mut outcomes = Vec::with_capacity(due.len());
    for entry in due {
        let outcome = process_one(conn, resolver, &entry).await;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

async fn process_one(
    conn: &dyn DbConn,
    resolver: &dyn BackendResolver,
    entry: &outbox_repo::OutboxEntry,
) -> DrainOutcome {
    let result = dispatch(conn, resolver, entry).await;
    match result {
        Ok(()) => match outbox_repo::delete(conn, &entry.id).await {
            Ok(()) => DrainOutcome::Sent {
                id: entry.id.clone(),
                op_kind: entry.op_kind.clone(),
            },
            Err(e) => {
                // The send succeeded but we couldn't delete the
                // row. Treat as a soft failure — next drain will
                // retry the send (which is idempotent for STORE
                // and Email/set keyword updates).
                warn!(id = %entry.id, "outbox: send ok but delete failed: {e}");
                DrainOutcome::Retrying {
                    id: entry.id.clone(),
                    op_kind: entry.op_kind.clone(),
                    attempts_after: entry.attempts,
                    error: format!("delete after send: {e}"),
                }
            }
        },
        Err(e) => {
            let err_str = format!("{e}");
            let now = Utc::now();
            if let Err(record_err) =
                outbox_repo::record_failure(conn, &entry.id, entry.attempts, &err_str, now).await
            {
                warn!(id = %entry.id, "outbox: failed to record failure: {record_err}");
            }
            if entry.attempts + 1 >= outbox_repo::MAX_ATTEMPTS {
                DrainOutcome::DeadLettered {
                    id: entry.id.clone(),
                    op_kind: entry.op_kind.clone(),
                    error: err_str,
                }
            } else {
                DrainOutcome::Retrying {
                    id: entry.id.clone(),
                    op_kind: entry.op_kind.clone(),
                    attempts_after: entry.attempts + 1,
                    error: err_str,
                }
            }
        }
    }
}

async fn dispatch(
    conn: &dyn DbConn,
    resolver: &dyn BackendResolver,
    entry: &outbox_repo::OutboxEntry,
) -> Result<(), MailError> {
    let backend = resolver.open(&entry.account_id).await?;
    match entry.op_kind.as_str() {
        OP_UPDATE_FLAGS => {
            let payload: UpdateFlagsPayload = serde_json::from_str(&entry.payload_json)
                .map_err(|e| MailError::Parse(format!("outbox.update_flags payload: {e}")))?;
            backend
                .update_flags(&payload.ids, payload.add, payload.remove)
                .await
        }
        OP_MOVE => {
            let payload: MovePayload = serde_json::from_str(&entry.payload_json)
                .map_err(|e| MailError::Parse(format!("outbox.move_messages payload: {e}")))?;
            backend.move_messages(&payload.ids, &payload.target).await
        }
        OP_DELETE => {
            let payload: DeletePayload = serde_json::from_str(&entry.payload_json)
                .map_err(|e| MailError::Parse(format!("outbox.delete_messages payload: {e}")))?;
            backend.delete_messages(&payload.ids).await
        }
        OP_SUBMIT_MESSAGE => {
            let payload: SubmitMessagePayload = serde_json::from_str(&entry.payload_json)
                .map_err(|e| MailError::Parse(format!("outbox.submit_message payload: {e}")))?;
            let raw = base64_engine
                .decode(&payload.raw_b64)
                .map_err(|e| MailError::Parse(format!("outbox.submit_message base64: {e}")))?;
            backend.submit_message(&raw).await.map(|_| ())
        }
        OP_SAVE_DRAFT => {
            let payload: SaveDraftPayload = serde_json::from_str(&entry.payload_json)
                .map_err(|e| MailError::Parse(format!("outbox.save_draft payload: {e}")))?;
            let raw = base64_engine
                .decode(&payload.raw_b64)
                .map_err(|e| MailError::Parse(format!("outbox.save_draft base64: {e}")))?;
            // Read the prior server id at execution time, not enqueue
            // time, so a flurry of auto-saves coalesced under
            // `enqueue_dedup` always destroys the latest known prior
            // copy rather than something stale that an earlier
            // overwritten payload was carrying.
            let draft_id = DraftId(payload.draft_id);
            let prior = drafts_repo::get_server_id(conn, &draft_id)
                .await
                .map_err(|e| MailError::Other(format!("drafts.get_server_id: {e}")))?;
            let new_id = backend.save_draft(&raw, prior.as_ref()).await?;
            // Persist the new server id so the next save_draft cycle
            // can destroy this copy. NotFound (the user discarded the
            // draft while the row was in flight) is silently OK; the
            // server-side draft is now an orphan that the user (or a
            // future orphan sweep) cleans up. Other errors surface as
            // dispatch failures so the outbox row sticks around for
            // retry.
            drafts_repo::set_server_id(conn, &draft_id, &new_id)
                .await
                .map_err(|e| MailError::Other(format!("drafts.set_server_id: {e}")))?;
            Ok(())
        }
        other => Err(MailError::Other(format!("outbox: unknown op_kind {other}"))),
    }
}
