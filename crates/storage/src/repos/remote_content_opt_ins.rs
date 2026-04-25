// SPDX-License-Identifier: Apache-2.0

//! Per-sender remote-content opt-in.
//!
//! Phase 1 Week 8. One row per `(account_id, email_address)` pair the
//! user has explicitly chosen to trust for remote content (images,
//! stylesheets, fonts). Lookup happens inside `messages_get` against
//! the message's `From` header — when an entry exists, the
//! sanitizer skips the adblock URL filter for that one render and
//! the `RenderedMessage.sender_is_trusted` flag goes true.
//!
//! Email addresses are normalized to lowercase before storage to
//! make lookups case-insensitive without needing a functional
//! index. `account_id` and `email_address` are the composite
//! primary key — the same sender can have different trust state on
//! a work account vs a personal account.

use chrono::Utc;

use capytain_core::{AccountId, StorageError};

use crate::conn::{DbConn, Params, Value};

const INSERT_OR_REPLACE: &str = "
    INSERT INTO remote_content_opt_ins (account_id, email_address, created_at)
    VALUES (?1, ?2, ?3)
    ON CONFLICT(account_id, email_address) DO UPDATE
       SET created_at = excluded.created_at
";

const DELETE_BY_KEY: &str = "
    DELETE FROM remote_content_opt_ins
    WHERE account_id = ?1 AND email_address = ?2
";

const SELECT_EXISTS: &str = "
    SELECT 1 FROM remote_content_opt_ins
    WHERE account_id = ?1 AND email_address = ?2
";

const SELECT_BY_ACCOUNT: &str = "
    SELECT email_address FROM remote_content_opt_ins
    WHERE account_id = ?1
    ORDER BY email_address ASC
";

/// Mark a sender as trusted. If an entry already exists, the
/// `created_at` is refreshed; storage is otherwise idempotent.
pub async fn add(conn: &dyn DbConn, account: &AccountId, email: &str) -> Result<(), StorageError> {
    let normalized = email.to_ascii_lowercase();
    let now = Utc::now().timestamp();
    conn.execute(
        INSERT_OR_REPLACE,
        Params(vec![
            Value::Text(&account.0),
            Value::Text(&normalized),
            Value::Integer(now),
        ]),
    )
    .await
    .map(|_| ())
}

/// Untrust a sender. Missing entries are treated as success.
pub async fn remove(
    conn: &dyn DbConn,
    account: &AccountId,
    email: &str,
) -> Result<(), StorageError> {
    let normalized = email.to_ascii_lowercase();
    conn.execute(
        DELETE_BY_KEY,
        Params(vec![Value::Text(&account.0), Value::Text(&normalized)]),
    )
    .await
    .map(|_| ())
}

/// Cheap "is this sender trusted" check. Used on every reader-pane
/// render, so the lookup is a single indexed point query against
/// the composite primary key.
pub async fn is_trusted(
    conn: &dyn DbConn,
    account: &AccountId,
    email: &str,
) -> Result<bool, StorageError> {
    let normalized = email.to_ascii_lowercase();
    let row = conn
        .query_opt(
            SELECT_EXISTS,
            Params(vec![Value::Text(&account.0), Value::Text(&normalized)]),
        )
        .await?;
    Ok(row.is_some())
}

/// All trusted senders for an account, sorted by address.
/// Useful for a settings-pane "review trusted senders" list.
pub async fn list_for_account(
    conn: &dyn DbConn,
    account: &AccountId,
) -> Result<Vec<String>, StorageError> {
    let rows = conn
        .query(SELECT_BY_ACCOUNT, Params(vec![Value::Text(&account.0)]))
        .await?;
    rows.iter()
        .map(|r| r.get_str("email_address").map(str::to_string))
        .collect()
}
