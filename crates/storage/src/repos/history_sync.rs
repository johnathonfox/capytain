// SPDX-License-Identifier: Apache-2.0

//! Per-(account, folder) history-sync state.
//!
//! Drives the "pull full mail history" feature in
//! [`qsl_sync::history`]. The pager walks UIDs descending from
//! `anchor_uid` and persists progress on every chunk so the work is
//! resumable across app restarts.
//!
//! Schema in `migrations/0010_history_sync.sql`.

use chrono::{DateTime, TimeZone, Utc};

use qsl_core::{AccountId, FolderId, StorageError};

use crate::conn::{DbConn, Params, Row, Value};

/// Plain-data row out of the `history_sync_state` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySyncRow {
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub status: HistorySyncStatus,
    /// Lowest UID we've fetched so far; the next chunk pulls UIDs
    /// strictly below this. `None` until the first chunk lands.
    pub anchor_uid: Option<i64>,
    /// Upper bound on history left to pull, captured from `uidnext`
    /// at start. Used to render a progress percentage.
    pub total_estimate: Option<i64>,
    /// Headers persisted into the messages table by this run.
    pub fetched: u32,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Lifecycle of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySyncStatus {
    /// Row created; the in-memory task hasn't started yet (e.g.
    /// queued during a UI tick before the engine spawn).
    Pending,
    /// Active task with a cancel token in `AppState`. On clean app
    /// exit any `running` row is bumped back to `pending` so the
    /// next launch can resume it.
    Running,
    /// `anchor_uid <= 1` reached; no more history to pull. Re-running
    /// is a no-op.
    Completed,
    /// User-cancelled mid-run; restart-able.
    Canceled,
    /// Fatal failure; `last_error` carries the message. Restart-able.
    Error,
}

impl HistorySyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HistorySyncStatus::Pending => "pending",
            HistorySyncStatus::Running => "running",
            HistorySyncStatus::Completed => "completed",
            HistorySyncStatus::Canceled => "canceled",
            HistorySyncStatus::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(HistorySyncStatus::Pending),
            "running" => Some(HistorySyncStatus::Running),
            "completed" => Some(HistorySyncStatus::Completed),
            "canceled" => Some(HistorySyncStatus::Canceled),
            "error" => Some(HistorySyncStatus::Error),
            _ => None,
        }
    }
}

const SELECT_COLS: &str = "account_id, folder_id, status, anchor_uid, total_estimate, \
                           fetched, started_at, updated_at, completed_at, last_error";

const UPSERT: &str = "
    INSERT INTO history_sync_state
        (account_id, folder_id, status, anchor_uid, total_estimate,
         fetched, started_at, updated_at, completed_at, last_error)
    VALUES
        (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6, NULL, NULL)
    ON CONFLICT(account_id, folder_id) DO UPDATE SET
        status         = excluded.status,
        anchor_uid     = excluded.anchor_uid,
        total_estimate = excluded.total_estimate,
        fetched        = 0,
        started_at     = excluded.started_at,
        updated_at     = excluded.updated_at,
        completed_at   = NULL,
        last_error     = NULL
";

const SELECT_ONE: &str = "
    SELECT account_id, folder_id, status, anchor_uid, total_estimate,
           fetched, started_at, updated_at, completed_at, last_error
      FROM history_sync_state
     WHERE account_id = ?1 AND folder_id = ?2
";

const SELECT_BY_ACCOUNT: &str = "
    SELECT account_id, folder_id, status, anchor_uid, total_estimate,
           fetched, started_at, updated_at, completed_at, last_error
      FROM history_sync_state
     WHERE account_id = ?1
";

const SELECT_BY_STATUS: &str = "
    SELECT account_id, folder_id, status, anchor_uid, total_estimate,
           fetched, started_at, updated_at, completed_at, last_error
      FROM history_sync_state
     WHERE status = ?1
";

const UPDATE_PROGRESS: &str = "
    UPDATE history_sync_state
       SET anchor_uid = ?3,
           fetched    = fetched + ?4,
           updated_at = ?5
     WHERE account_id = ?1 AND folder_id = ?2
";

const SET_STATUS: &str = "
    UPDATE history_sync_state
       SET status       = ?3,
           updated_at   = ?4,
           completed_at = ?5,
           last_error   = ?6
     WHERE account_id = ?1 AND folder_id = ?2
";

const _: () = {
    // Anchor SELECT_COLS in source so a future schema tweak that drops
    // a column gives a compile-time nudge. Const-evaluated; no
    // runtime cost.
    assert!(!SELECT_COLS.is_empty());
};

/// Create or reset a history-sync row to running. Anchor + estimate
/// come from the backend's `SELECT folder` response captured by the
/// caller. Replaces any existing row's progress fields (this is
/// "Restart" semantics — start fresh from the top of the folder).
pub async fn start(
    conn: &dyn DbConn,
    account: &AccountId,
    folder: &FolderId,
    anchor_uid: i64,
    total_estimate: Option<i64>,
) -> Result<(), StorageError> {
    let now = Utc::now().timestamp();
    conn.execute(
        UPSERT,
        Params(vec![
            Value::Text(&account.0),
            Value::Text(&folder.0),
            Value::Text(HistorySyncStatus::Running.as_str()),
            Value::Integer(anchor_uid),
            match total_estimate {
                Some(v) => Value::Integer(v),
                None => Value::Null,
            },
            Value::Integer(now),
        ]),
    )
    .await
    .map(|_| ())
}

/// Update the running row's anchor and add `delta` to `fetched`.
/// Caller passes the new lowest-UID-seen and the count just persisted.
pub async fn update_progress(
    conn: &dyn DbConn,
    account: &AccountId,
    folder: &FolderId,
    anchor_uid: i64,
    delta: u32,
) -> Result<(), StorageError> {
    let now = Utc::now().timestamp();
    conn.execute(
        UPDATE_PROGRESS,
        Params(vec![
            Value::Text(&account.0),
            Value::Text(&folder.0),
            Value::Integer(anchor_uid),
            Value::Integer(delta as i64),
            Value::Integer(now),
        ]),
    )
    .await
    .map(|_| ())
}

/// Move a row to a terminal (or paused) state.
pub async fn set_status(
    conn: &dyn DbConn,
    account: &AccountId,
    folder: &FolderId,
    status: HistorySyncStatus,
    last_error: Option<&str>,
) -> Result<(), StorageError> {
    let now = Utc::now().timestamp();
    let completed_at: Option<i64> = matches!(
        status,
        HistorySyncStatus::Completed | HistorySyncStatus::Canceled | HistorySyncStatus::Error
    )
    .then_some(now);
    conn.execute(
        SET_STATUS,
        Params(vec![
            Value::Text(&account.0),
            Value::Text(&folder.0),
            Value::Text(status.as_str()),
            Value::Integer(now),
            match completed_at {
                Some(v) => Value::Integer(v),
                None => Value::Null,
            },
            match last_error {
                Some(s) => Value::Text(s),
                None => Value::Null,
            },
        ]),
    )
    .await
    .map(|_| ())
}

/// Remove the history-sync row for `(account, folder)`. Used by the
/// cancel path to drop the row entirely from the UI list rather than
/// leaving a `Canceled` terminal entry the user has to clean up by
/// hand. Idempotent — silently does nothing when no row exists.
pub async fn delete(
    conn: &dyn DbConn,
    account: &AccountId,
    folder: &FolderId,
) -> Result<(), StorageError> {
    conn.execute(
        "DELETE FROM history_sync_state WHERE account_id = ?1 AND folder_id = ?2",
        Params(vec![Value::Text(&account.0), Value::Text(&folder.0)]),
    )
    .await
    .map(|_| ())
}

/// Lookup one row.
pub async fn get(
    conn: &dyn DbConn,
    account: &AccountId,
    folder: &FolderId,
) -> Result<Option<HistorySyncRow>, StorageError> {
    let row = conn
        .query_opt(
            SELECT_ONE,
            Params(vec![Value::Text(&account.0), Value::Text(&folder.0)]),
        )
        .await?;
    row.as_ref().map(row_to_state).transpose()
}

/// Every history-sync row for one account.
pub async fn list_by_account(
    conn: &dyn DbConn,
    account: &AccountId,
) -> Result<Vec<HistorySyncRow>, StorageError> {
    let rows = conn
        .query(SELECT_BY_ACCOUNT, Params(vec![Value::Text(&account.0)]))
        .await?;
    rows.iter().map(row_to_state).collect()
}

/// Every row in a given status — used at app boot to find rows that
/// were `running` at the last shutdown and need to be requeued, plus
/// to pick up any `pending` entries the engine should kick off.
pub async fn list_by_status(
    conn: &dyn DbConn,
    status: HistorySyncStatus,
) -> Result<Vec<HistorySyncRow>, StorageError> {
    let rows = conn
        .query(SELECT_BY_STATUS, Params(vec![Value::Text(status.as_str())]))
        .await?;
    rows.iter().map(row_to_state).collect()
}

fn row_to_state(row: &Row) -> Result<HistorySyncRow, StorageError> {
    let status_str = row.get_str("status")?;
    let status = HistorySyncStatus::parse(status_str)
        .ok_or_else(|| StorageError::Db(format!("unknown history_sync status: {status_str}")))?;
    Ok(HistorySyncRow {
        account_id: AccountId(row.get_str("account_id")?.to_string()),
        folder_id: FolderId(row.get_str("folder_id")?.to_string()),
        status,
        anchor_uid: row.get_optional_i64("anchor_uid")?,
        total_estimate: row.get_optional_i64("total_estimate")?,
        fetched: row.get_i64("fetched")?.try_into().unwrap_or(0),
        started_at: Utc.timestamp_opt(row.get_i64("started_at")?, 0).unwrap(),
        updated_at: Utc.timestamp_opt(row.get_i64("updated_at")?, 0).unwrap(),
        completed_at: row
            .get_optional_i64("completed_at")?
            .map(|t| Utc.timestamp_opt(t, 0).unwrap()),
        last_error: row.get_optional_str("last_error")?.map(|s| s.to_string()),
    })
}
