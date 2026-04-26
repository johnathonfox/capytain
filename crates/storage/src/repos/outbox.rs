// SPDX-License-Identifier: Apache-2.0

//! Outbox persistence — Phase 1 Week 14 optimistic mutations.
//!
//! Every UI-driven write (`messages_mark_read`, `messages_flag`,
//! `messages_move`, `messages_delete`) follows the optimistic
//! pattern: apply the change locally, insert a row here, return.
//! A sync-engine worker drains the queue, dispatching each row to
//! the appropriate `MailBackend` method. On success the row is
//! deleted; on failure we bump `attempts` + push out
//! `next_attempt_at` per an exponential schedule, with the row
//! moving to a dead-letter state after `MAX_ATTEMPTS` so the UI
//! can surface "failed to sync."
//!
//! The table itself shipped in `0001_initial.sql` per
//! `DESIGN.md` §4.4. This module is the typed access layer.

use chrono::{DateTime, TimeZone, Utc};

use qsl_core::{AccountId, StorageError};

use crate::conn::{DbConn, Params, Row, Value};

/// Caller-side handle for one outbox row. The `payload_json` is
/// kept opaque here — each `op_kind` chooses its own shape, and the
/// drain worker in `qsl-sync` deserializes per kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub id: String,
    pub account_id: AccountId,
    pub op_kind: String,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    /// `None` for entries that have failed past `MAX_ATTEMPTS` —
    /// the dead-letter state. The drain worker skips these; the
    /// UI surfaces them as "failed to sync" banners.
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Stop retrying after this many failures. The 6th failure transitions
/// the row to the DLQ state (`next_attempt_at = NULL`).
pub const MAX_ATTEMPTS: u32 = 5;

const INSERT: &str = "
    INSERT INTO outbox (id, account_id, op_kind, payload_json, created_at,
                        attempts, next_attempt_at, last_error)
    VALUES (?1, ?2, ?3, ?4, ?5, 0, ?5, NULL)
";

const SELECT_DUE: &str = "
    SELECT id, account_id, op_kind, payload_json, created_at,
           attempts, next_attempt_at, last_error
      FROM outbox
     WHERE next_attempt_at IS NOT NULL
       AND next_attempt_at <= ?1
     ORDER BY next_attempt_at ASC
     LIMIT ?2
";

const SELECT_DLQ: &str = "
    SELECT id, account_id, op_kind, payload_json, created_at,
           attempts, next_attempt_at, last_error
      FROM outbox
     WHERE next_attempt_at IS NULL
     ORDER BY created_at DESC
";

const DELETE_BY_ID: &str = "DELETE FROM outbox WHERE id = ?1";

const TOUCH_FAILURE: &str = "
    UPDATE outbox
       SET attempts        = attempts + 1,
           next_attempt_at = ?2,
           last_error      = ?3
     WHERE id = ?1
";

const TOUCH_DLQ: &str = "
    UPDATE outbox
       SET attempts        = attempts + 1,
           next_attempt_at = NULL,
           last_error      = ?2
     WHERE id = ?1
";

/// Append a new outbox row. Returns the row's id so the caller can
/// log / display it. Caller is responsible for serializing
/// `payload_json` against the op kind's contract.
pub async fn enqueue(
    conn: &dyn DbConn,
    account: &AccountId,
    op_kind: &str,
    payload_json: &str,
) -> Result<String, StorageError> {
    let id = new_id();
    let now = Utc::now();
    conn.execute(
        INSERT,
        Params(vec![
            Value::Text(&id),
            Value::Text(&account.0),
            Value::OwnedText(op_kind.to_string()),
            Value::OwnedText(payload_json.to_string()),
            Value::Integer(now.timestamp()),
        ]),
    )
    .await?;
    Ok(id)
}

/// Pull every row whose `next_attempt_at` is at or before `now`,
/// in oldest-first order, capped at `limit`. The drain worker
/// calls this on a timer and processes each row in turn.
pub async fn list_due(
    conn: &dyn DbConn,
    now: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<OutboxEntry>, StorageError> {
    let rows = conn
        .query(
            SELECT_DUE,
            Params(vec![
                Value::Integer(now.timestamp()),
                Value::Integer(limit.into()),
            ]),
        )
        .await?;
    rows.iter().map(row_to_entry).collect()
}

/// Pull the dead-letter set — rows that exceeded `MAX_ATTEMPTS` and
/// won't retry without manual intervention. Surfaced to the UI as
/// "failed to sync" banners.
pub async fn list_dlq(conn: &dyn DbConn) -> Result<Vec<OutboxEntry>, StorageError> {
    let rows = conn.query(SELECT_DLQ, Params::empty()).await?;
    rows.iter().map(row_to_entry).collect()
}

/// Delete an entry — drain-worker calls this on success.
pub async fn delete(conn: &dyn DbConn, id: &str) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_ID, Params(vec![Value::Text(id)]))
        .await
        .map(|_| ())
}

/// Mark an entry as failed. Computes the next backoff and either
/// reschedules or transitions the row to the DLQ if `MAX_ATTEMPTS`
/// was already reached. The caller hands us the *previous*
/// `attempts` count so the schedule is computed without a re-read.
pub async fn record_failure(
    conn: &dyn DbConn,
    id: &str,
    prev_attempts: u32,
    error: &str,
    now: DateTime<Utc>,
) -> Result<(), StorageError> {
    if prev_attempts + 1 >= MAX_ATTEMPTS {
        conn.execute(
            TOUCH_DLQ,
            Params(vec![
                Value::Text(id),
                Value::OwnedText(truncate(error, 500)),
            ]),
        )
        .await?;
        return Ok(());
    }
    let backoff_secs = backoff_seconds(prev_attempts + 1);
    let next = now + chrono::Duration::seconds(backoff_secs);
    conn.execute(
        TOUCH_FAILURE,
        Params(vec![
            Value::Text(id),
            Value::Integer(next.timestamp()),
            Value::OwnedText(truncate(error, 500)),
        ]),
    )
    .await?;
    Ok(())
}

/// Exponential backoff with light jitter. Cadence per attempt:
/// 1 → ~30 s, 2 → ~2 min, 3 → ~8 min, 4 → ~30 min. Past attempt 5
/// the row is in the DLQ; this function is only called for live
/// retries.
fn backoff_seconds(attempt: u32) -> i64 {
    let base: i64 = 30; // attempt 1 → 30 s
    let mult: i64 = 4;
    let exp = base.saturating_mul(mult.saturating_pow(attempt.saturating_sub(1)));
    // ±20% jitter so a thundering-herd of mutations doesn't all
    // re-fire at the same moment.
    let jitter_pct: i64 = (rand::random::<u8>() as i64 % 41) - 20;
    exp.saturating_add(exp * jitter_pct / 100).max(1)
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

fn new_id() -> String {
    let n: u64 = rand::random();
    format!("ob-{n:016x}")
}

fn row_to_entry(row: &Row) -> Result<OutboxEntry, StorageError> {
    Ok(OutboxEntry {
        id: row.get_str("id")?.to_string(),
        account_id: AccountId(row.get_str("account_id")?.to_string()),
        op_kind: row.get_str("op_kind")?.to_string(),
        payload_json: row.get_str("payload_json")?.to_string(),
        created_at: Utc
            .timestamp_opt(row.get_i64("created_at")?, 0)
            .single()
            .ok_or_else(|| StorageError::Db("invalid outbox.created_at".into()))?,
        attempts: u32::try_from(row.get_i64("attempts")?)
            .map_err(|e| StorageError::Db(format!("attempts out of range: {e}")))?,
        next_attempt_at: match row.get_optional_i64("next_attempt_at")? {
            Some(t) => Some(
                Utc.timestamp_opt(t, 0)
                    .single()
                    .ok_or_else(|| StorageError::Db("invalid outbox.next_attempt_at".into()))?,
            ),
            None => None,
        },
        last_error: row.get_optional_str("last_error")?.map(str::to_string),
    })
}
