// SPDX-License-Identifier: Apache-2.0

//! Account persistence. One row per configured account.

use chrono::{DateTime, TimeZone, Utc};

use qsl_core::{Account, AccountId, BackendKind, StorageError};

use crate::conn::{DbConn, Params, Row, Value};

const INSERT: &str = "
    INSERT INTO accounts (id, kind, display_name, email_address, created_at)
    VALUES (?1, ?2, ?3, ?4, ?5)
";

const UPDATE: &str = "
    UPDATE accounts
       SET kind = ?2,
           display_name = ?3,
           email_address = ?4,
           created_at = ?5
     WHERE id = ?1
";

const SELECT_BY_ID: &str = "
    SELECT id, kind, display_name, email_address, created_at
      FROM accounts
     WHERE id = ?1
";

const SELECT_ALL: &str = "
    SELECT id, kind, display_name, email_address, created_at
      FROM accounts
     ORDER BY created_at ASC
";

const DELETE_BY_ID: &str = "DELETE FROM accounts WHERE id = ?1";

/// Insert a new account. Returns [`StorageError::Conflict`] if the id or
/// email address is already taken.
pub async fn insert(conn: &dyn DbConn, account: &Account) -> Result<(), StorageError> {
    conn.execute(INSERT, to_params(account)).await.map(|_| ())
}

/// Overwrite an existing account by id. Returns [`StorageError::NotFound`]
/// if no row matched.
pub async fn update(conn: &dyn DbConn, account: &Account) -> Result<(), StorageError> {
    let affected = conn.execute(UPDATE, to_params(account)).await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

/// Fetch by id; [`StorageError::NotFound`] if missing.
pub async fn get(conn: &dyn DbConn, id: &AccountId) -> Result<Account, StorageError> {
    let row = conn
        .query_one(SELECT_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await?;
    row_to_account(&row)
}

/// Fetch by id, or `None` if missing.
pub async fn find(conn: &dyn DbConn, id: &AccountId) -> Result<Option<Account>, StorageError> {
    let maybe = conn
        .query_opt(SELECT_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await?;
    maybe.map(|r| row_to_account(&r)).transpose()
}

/// Return all accounts ordered by `created_at`.
pub async fn list(conn: &dyn DbConn) -> Result<Vec<Account>, StorageError> {
    let rows = conn.query(SELECT_ALL, Params::empty()).await?;
    rows.iter().map(row_to_account).collect()
}

/// Delete by id. A missing id is treated as success.
pub async fn delete(conn: &dyn DbConn, id: &AccountId) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await
        .map(|_| ())
}

fn to_params(a: &Account) -> Params<'_> {
    Params(vec![
        Value::Text(&a.id.0),
        Value::OwnedText(backend_kind_str(&a.kind).into()),
        Value::Text(&a.display_name),
        Value::Text(&a.email_address),
        Value::Integer(a.created_at.timestamp()),
    ])
}

fn row_to_account(row: &Row) -> Result<Account, StorageError> {
    Ok(Account {
        id: AccountId(row.get_str("id")?.to_string()),
        kind: backend_kind_from_str(row.get_str("kind")?)?,
        display_name: row.get_str("display_name")?.to_string(),
        email_address: row.get_str("email_address")?.to_string(),
        created_at: timestamp_to_utc(row.get_i64("created_at")?)?,
    })
}

fn backend_kind_str(kind: &BackendKind) -> &'static str {
    // `BackendKind` is `#[non_exhaustive]`; the wildcard exists to keep the
    // storage crate compiling if a future variant is added without a migration.
    match kind {
        BackendKind::ImapSmtp => "imap_smtp",
        BackendKind::Jmap => "jmap",
        _ => "unknown",
    }
}

fn backend_kind_from_str(s: &str) -> Result<BackendKind, StorageError> {
    match s {
        "imap_smtp" => Ok(BackendKind::ImapSmtp),
        "jmap" => Ok(BackendKind::Jmap),
        other => Err(StorageError::Db(format!("unknown backend kind: {other}"))),
    }
}

fn timestamp_to_utc(secs: i64) -> Result<DateTime<Utc>, StorageError> {
    Utc.timestamp_opt(secs, 0)
        .single()
        .ok_or_else(|| StorageError::Db(format!("invalid timestamp: {secs}")))
}
