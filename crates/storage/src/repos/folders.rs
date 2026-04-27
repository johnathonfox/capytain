// SPDX-License-Identifier: Apache-2.0

//! Folder persistence.

use qsl_core::{AccountId, Folder, FolderId, FolderRole, StorageError};

use crate::conn::{DbConn, Params, Row, Value};

const INSERT: &str = "
    INSERT INTO folders
        (id, account_id, name, path, role, unread_count, total_count, parent_id)
    VALUES
        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
";

const UPDATE: &str = "
    UPDATE folders
       SET account_id = ?2,
           name = ?3,
           path = ?4,
           role = ?5,
           unread_count = ?6,
           total_count = ?7,
           parent_id = ?8
     WHERE id = ?1
";

const SELECT_BY_ID: &str = "
    SELECT id, account_id, name, path, role, unread_count, total_count, parent_id
      FROM folders
     WHERE id = ?1
";

const SELECT_BY_ACCOUNT: &str = "
    SELECT id, account_id, name, path, role, unread_count, total_count, parent_id
      FROM folders
     WHERE account_id = ?1
     ORDER BY path ASC
";

const SELECT_BY_ROLE: &str = "
    SELECT id, account_id, name, path, role, unread_count, total_count, parent_id
      FROM folders
     WHERE role = ?1
     ORDER BY account_id ASC, path ASC
";

const DELETE_BY_ID: &str = "DELETE FROM folders WHERE id = ?1";

pub async fn insert(conn: &dyn DbConn, folder: &Folder) -> Result<(), StorageError> {
    conn.execute(INSERT, to_params(folder)).await.map(|_| ())
}

pub async fn update(conn: &dyn DbConn, folder: &Folder) -> Result<(), StorageError> {
    let affected = conn.execute(UPDATE, to_params(folder)).await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn get(conn: &dyn DbConn, id: &FolderId) -> Result<Folder, StorageError> {
    let row = conn
        .query_one(SELECT_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await?;
    row_to_folder(&row)
}

pub async fn find(conn: &dyn DbConn, id: &FolderId) -> Result<Option<Folder>, StorageError> {
    conn.query_opt(SELECT_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await?
        .map(|r| row_to_folder(&r))
        .transpose()
}

pub async fn list_by_account(
    conn: &dyn DbConn,
    account: &AccountId,
) -> Result<Vec<Folder>, StorageError> {
    let rows = conn
        .query(SELECT_BY_ACCOUNT, Params(vec![Value::Text(&account.0)]))
        .await?;
    rows.iter().map(row_to_folder).collect()
}

/// Every folder with the given `role` across every account. Used by
/// the unified-inbox UI to find every account's INBOX-role folder
/// in one query — the engine then merges their messages by date.
pub async fn list_by_role(
    conn: &dyn DbConn,
    role: FolderRole,
) -> Result<Vec<Folder>, StorageError> {
    let role_text = role_str(&role);
    let rows = conn
        .query(
            SELECT_BY_ROLE,
            Params(vec![Value::OwnedText(role_text.to_string())]),
        )
        .await?;
    rows.iter().map(row_to_folder).collect()
}

pub async fn delete(conn: &dyn DbConn, id: &FolderId) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await
        .map(|_| ())
}

fn to_params(f: &Folder) -> Params<'_> {
    Params(vec![
        Value::Text(&f.id.0),
        Value::Text(&f.account_id.0),
        Value::Text(&f.name),
        Value::Text(&f.path),
        f.role
            .as_ref()
            .map(|r| Value::OwnedText(role_str(r).into()))
            .unwrap_or(Value::Null),
        Value::Integer(f.unread_count.into()),
        Value::Integer(f.total_count.into()),
        f.parent
            .as_ref()
            .map(|p| Value::Text(&p.0))
            .unwrap_or(Value::Null),
    ])
}

fn row_to_folder(row: &Row) -> Result<Folder, StorageError> {
    Ok(Folder {
        id: FolderId(row.get_str("id")?.to_string()),
        account_id: AccountId(row.get_str("account_id")?.to_string()),
        name: row.get_str("name")?.to_string(),
        path: row.get_str("path")?.to_string(),
        role: row
            .get_optional_str("role")?
            .map(role_from_str)
            .transpose()?,
        unread_count: u32::try_from(row.get_i64("unread_count")?).map_err(counter_err)?,
        total_count: u32::try_from(row.get_i64("total_count")?).map_err(counter_err)?,
        parent: row
            .get_optional_str("parent_id")?
            .map(|s| FolderId(s.to_string())),
    })
}

fn counter_err<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Db(format!("folder counter out of range: {e}"))
}

fn role_str(role: &FolderRole) -> &'static str {
    // `FolderRole` is `#[non_exhaustive]`. The wildcard keeps this crate
    // compiling if a new variant lands upstream without a migration; any
    // unknown role reads back as `FolderRole` parse error in `role_from_str`.
    match role {
        FolderRole::Inbox => "inbox",
        FolderRole::Sent => "sent",
        FolderRole::Drafts => "drafts",
        FolderRole::Trash => "trash",
        FolderRole::Spam => "spam",
        FolderRole::Archive => "archive",
        FolderRole::Important => "important",
        FolderRole::All => "all",
        FolderRole::Flagged => "flagged",
        _ => "unknown",
    }
}

fn role_from_str(s: &str) -> Result<FolderRole, StorageError> {
    match s {
        "inbox" => Ok(FolderRole::Inbox),
        "sent" => Ok(FolderRole::Sent),
        "drafts" => Ok(FolderRole::Drafts),
        "trash" => Ok(FolderRole::Trash),
        "spam" => Ok(FolderRole::Spam),
        "archive" => Ok(FolderRole::Archive),
        "important" => Ok(FolderRole::Important),
        "all" => Ok(FolderRole::All),
        "flagged" => Ok(FolderRole::Flagged),
        other => Err(StorageError::Db(format!("unknown folder role: {other}"))),
    }
}
