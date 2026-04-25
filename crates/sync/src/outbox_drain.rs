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

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use capytain_core::{FolderId, MailError, MessageFlags, MessageId, StorageError};
use capytain_storage::{repos::outbox as outbox_repo, DbConn};

/// Op-kind tag used for `update_flags` payloads.
pub const OP_UPDATE_FLAGS: &str = "update_flags";
/// Op-kind tag for `move_messages` payloads.
pub const OP_MOVE: &str = "move_messages";
/// Op-kind tag for `delete_messages` payloads.
pub const OP_DELETE: &str = "delete_messages";

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

/// Trait the engine implements to hand the worker a backend on
/// demand. Decoupled from `MailBackend` so the engine can do its
/// per-account dispatch (the outbox row carries the account_id;
/// the resolver hands back a backend for that account).
#[async_trait::async_trait]
pub trait BackendResolver: Send + Sync {
    async fn open(
        &self,
        account: &capytain_core::AccountId,
    ) -> Result<std::sync::Arc<dyn capytain_core::MailBackend>, MailError>;
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
    let result = dispatch(resolver, entry).await;
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
        other => Err(MailError::Other(format!("outbox: unknown op_kind {other}"))),
    }
}
