// SPDX-License-Identifier: Apache-2.0

//! Message-header persistence. Bodies live on disk via `crate::blobs`;
//! this repo persists only the `MessageHeaders` view plus a `body_path`
//! pointer.

use chrono::{TimeZone, Utc};

use capytain_core::{
    AccountId, FolderId, MessageFlags, MessageHeaders, MessageId, StorageError, ThreadId,
};

use super::json;
use crate::conn::{DbConn, Params, Row, Value};

const INSERT: &str = "
    INSERT INTO messages
        (id, account_id, folder_id, thread_id, rfc822_message_id,
         subject, from_json, reply_to_json, to_json, cc_json, bcc_json,
         date, flags_json, labels_json, snippet, size, has_attachments,
         body_path, in_reply_to, references_json)
    VALUES
        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
";

const UPDATE: &str = "
    UPDATE messages
       SET account_id = ?2,
           folder_id = ?3,
           thread_id = ?4,
           rfc822_message_id = ?5,
           subject = ?6,
           from_json = ?7,
           reply_to_json = ?8,
           to_json = ?9,
           cc_json = ?10,
           bcc_json = ?11,
           date = ?12,
           flags_json = ?13,
           labels_json = ?14,
           snippet = ?15,
           size = ?16,
           has_attachments = ?17,
           body_path = ?18,
           in_reply_to = ?19,
           references_json = ?20
     WHERE id = ?1
";

const COLS: &str = "id, account_id, folder_id, thread_id, rfc822_message_id, \
     subject, from_json, reply_to_json, to_json, cc_json, bcc_json, \
     date, flags_json, labels_json, snippet, size, has_attachments, body_path, \
     in_reply_to, references_json";

const DELETE_BY_ID: &str = "DELETE FROM messages WHERE id = ?1";

pub async fn insert(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
    body_path: Option<&str>,
) -> Result<(), StorageError> {
    conn.execute(INSERT, to_params(headers, body_path)?)
        .await
        .map(|_| ())
}

pub async fn update(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
    body_path: Option<&str>,
) -> Result<(), StorageError> {
    let affected = conn.execute(UPDATE, to_params(headers, body_path)?).await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn get(conn: &dyn DbConn, id: &MessageId) -> Result<MessageHeaders, StorageError> {
    let sql = format!("SELECT {COLS} FROM messages WHERE id = ?1");
    let row = conn
        .query_one(&sql, Params(vec![Value::Text(&id.0)]))
        .await?;
    row_to_headers(&row)
}

pub async fn find(
    conn: &dyn DbConn,
    id: &MessageId,
) -> Result<Option<MessageHeaders>, StorageError> {
    let sql = format!("SELECT {COLS} FROM messages WHERE id = ?1");
    conn.query_opt(&sql, Params(vec![Value::Text(&id.0)]))
        .await?
        .map(|r| row_to_headers(&r))
        .transpose()
}

/// Look up a message by its RFC 5322 `Message-ID` header within a
/// single account. Used by the threading assembly pipeline to find
/// the local row that an incoming message's `In-Reply-To` /
/// `References` chain points at — the row's `thread_id` is then the
/// thread to attach the new message to. Returns the most recent
/// match by date if (rare) duplicates exist.
pub async fn find_by_rfc822_id(
    conn: &dyn DbConn,
    account: &AccountId,
    rfc822_message_id: &str,
) -> Result<Option<MessageHeaders>, StorageError> {
    let sql = format!(
        "SELECT {COLS} FROM messages \
         WHERE account_id = ?1 AND rfc822_message_id = ?2 \
         ORDER BY date DESC LIMIT 1"
    );
    conn.query_opt(
        &sql,
        Params(vec![
            Value::Text(&account.0),
            Value::OwnedText(rfc822_message_id.to_string()),
        ]),
    )
    .await?
    .map(|r| row_to_headers(&r))
    .transpose()
}

pub async fn list_by_folder(
    conn: &dyn DbConn,
    folder: &FolderId,
    limit: u32,
    offset: u32,
) -> Result<Vec<MessageHeaders>, StorageError> {
    let sql = format!(
        "SELECT {COLS} FROM messages \
         WHERE folder_id = ?1 \
         ORDER BY date DESC LIMIT ?2 OFFSET ?3"
    );
    let rows = conn
        .query(
            &sql,
            Params(vec![
                Value::Text(&folder.0),
                Value::Integer(limit.into()),
                Value::Integer(offset.into()),
            ]),
        )
        .await?;
    rows.iter().map(row_to_headers).collect()
}

/// Cross-folder version of [`list_by_folder`]: all messages in any
/// of the given folders, sorted by date desc, paginated. Used by
/// the unified-inbox UI to merge every account's INBOX-role folder
/// in one query.
///
/// Empty `folders` returns `Ok(vec![])` without round-tripping. The
/// `IN (?, ?, …)` clause is built with one placeholder per folder
/// id; we expect <100 entries in practice (one INBOX per account).
pub async fn list_by_folders(
    conn: &dyn DbConn,
    folders: &[FolderId],
    limit: u32,
    offset: u32,
) -> Result<Vec<MessageHeaders>, StorageError> {
    if folders.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = (1..=folders.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let limit_param = folders.len() + 1;
    let offset_param = folders.len() + 2;
    let sql = format!(
        "SELECT {COLS} FROM messages \
         WHERE folder_id IN ({placeholders}) \
         ORDER BY date DESC LIMIT ?{limit_param} OFFSET ?{offset_param}"
    );
    let mut params: Vec<Value> = folders.iter().map(|f| Value::Text(&f.0)).collect();
    params.push(Value::Integer(limit.into()));
    params.push(Value::Integer(offset.into()));
    let rows = conn.query(&sql, Params(params)).await?;
    rows.iter().map(row_to_headers).collect()
}

/// Cross-folder count for the unified inbox. Mirrors
/// [`count_by_folder`].
pub async fn count_by_folders(
    conn: &dyn DbConn,
    folders: &[FolderId],
) -> Result<u32, StorageError> {
    if folders.is_empty() {
        return Ok(0);
    }
    let placeholders: String = (1..=folders.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT COUNT(*) AS c FROM messages WHERE folder_id IN ({placeholders})");
    let params: Vec<Value> = folders.iter().map(|f| Value::Text(&f.0)).collect();
    let row = conn.query_one(&sql, Params(params)).await?;
    let c = row.get_i64("c")?;
    Ok(c.max(0) as u32)
}

/// Cross-folder unread count for the unified inbox. Mirrors
/// [`count_unread_by_folder`].
pub async fn count_unread_by_folders(
    conn: &dyn DbConn,
    folders: &[FolderId],
) -> Result<u32, StorageError> {
    if folders.is_empty() {
        return Ok(0);
    }
    let placeholders: String = (1..=folders.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT COUNT(*) AS c \
           FROM messages \
          WHERE folder_id IN ({placeholders}) \
            AND COALESCE(json_extract(flags_json, '$.seen'), 0) = 0"
    );
    let params: Vec<Value> = folders.iter().map(|f| Value::Text(&f.0)).collect();
    let row = conn.query_one(&sql, Params(params)).await?;
    let c = row.get_i64("c")?;
    Ok(c.max(0) as u32)
}

/// Total number of messages persisted for a folder.
///
/// Used by `MessagePage::total_count` so the UI can render pagination
/// hints without streaming the whole folder.
pub async fn count_by_folder(conn: &dyn DbConn, folder: &FolderId) -> Result<u32, StorageError> {
    let row = conn
        .query_one(
            "SELECT COUNT(*) AS c FROM messages WHERE folder_id = ?1",
            Params(vec![Value::Text(&folder.0)]),
        )
        .await?;
    let c = row.get_i64("c")?;
    Ok(c.max(0) as u32)
}

/// Number of unread (`!seen`) messages in a folder.
///
/// We persist `flags_json` as an opaque JSON blob, so the query uses
/// SQLite's `json_extract` to look inside it. Messages whose flag blob
/// can't be parsed (e.g. written by a future schema version) are
/// treated as read rather than crashing the count.
pub async fn count_unread_by_folder(
    conn: &dyn DbConn,
    folder: &FolderId,
) -> Result<u32, StorageError> {
    let row = conn
        .query_one(
            "SELECT COUNT(*) AS c \
               FROM messages \
              WHERE folder_id = ?1 \
                AND COALESCE(json_extract(flags_json, '$.seen'), 0) = 0",
            Params(vec![Value::Text(&folder.0)]),
        )
        .await?;
    let c = row.get_i64("c")?;
    Ok(c.max(0) as u32)
}

pub async fn update_flags(
    conn: &dyn DbConn,
    id: &MessageId,
    flags: &MessageFlags,
) -> Result<(), StorageError> {
    let affected = conn
        .execute(
            "UPDATE messages SET flags_json = ?2 WHERE id = ?1",
            Params(vec![
                Value::Text(&id.0),
                Value::OwnedText(json::encode(flags)?),
            ]),
        )
        .await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn body_path(conn: &dyn DbConn, id: &MessageId) -> Result<Option<String>, StorageError> {
    let row = conn
        .query_one(
            "SELECT body_path FROM messages WHERE id = ?1",
            Params(vec![Value::Text(&id.0)]),
        )
        .await?;
    Ok(row.get_optional_str("body_path")?.map(str::to_string))
}

/// Move a message between folders by patching its `folder_id` in
/// place. Used by `messages_move`'s optimistic local update — the
/// background drain reconciles to the server.
pub async fn set_folder(
    conn: &dyn DbConn,
    id: &MessageId,
    folder: &FolderId,
) -> Result<(), StorageError> {
    let affected = conn
        .execute(
            "UPDATE messages SET folder_id = ?2 WHERE id = ?1",
            Params(vec![Value::Text(&id.0), Value::Text(&folder.0)]),
        )
        .await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn set_body_path(
    conn: &dyn DbConn,
    id: &MessageId,
    path: Option<&str>,
) -> Result<(), StorageError> {
    let affected = conn
        .execute(
            "UPDATE messages SET body_path = ?2 WHERE id = ?1",
            Params(vec![
                Value::Text(&id.0),
                path.map(Value::Text).unwrap_or(Value::Null),
            ]),
        )
        .await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn delete(conn: &dyn DbConn, id: &MessageId) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await
        .map(|_| ())
}

fn to_params<'a>(
    h: &'a MessageHeaders,
    body_path: Option<&'a str>,
) -> Result<Params<'a>, StorageError> {
    Ok(Params(vec![
        Value::Text(&h.id.0),
        Value::Text(&h.account_id.0),
        Value::Text(&h.folder_id.0),
        h.thread_id
            .as_ref()
            .map(|t| Value::Text(&t.0))
            .unwrap_or(Value::Null),
        h.rfc822_message_id
            .as_deref()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        Value::Text(&h.subject),
        Value::OwnedText(json::encode(&h.from)?),
        Value::OwnedText(json::encode(&h.reply_to)?),
        Value::OwnedText(json::encode(&h.to)?),
        Value::OwnedText(json::encode(&h.cc)?),
        Value::OwnedText(json::encode(&h.bcc)?),
        Value::Integer(h.date.timestamp()),
        Value::OwnedText(json::encode(&h.flags)?),
        Value::OwnedText(json::encode(&h.labels)?),
        Value::Text(&h.snippet),
        Value::Integer(h.size.into()),
        Value::Integer(h.has_attachments.into()),
        body_path.map(Value::Text).unwrap_or(Value::Null),
        h.in_reply_to
            .as_deref()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        Value::OwnedText(json::encode(&h.references)?),
    ]))
}

fn row_to_headers(row: &Row) -> Result<MessageHeaders, StorageError> {
    Ok(MessageHeaders {
        id: MessageId(row.get_str("id")?.to_string()),
        account_id: AccountId(row.get_str("account_id")?.to_string()),
        folder_id: FolderId(row.get_str("folder_id")?.to_string()),
        thread_id: row
            .get_optional_str("thread_id")?
            .map(|s| ThreadId(s.to_string())),
        rfc822_message_id: row
            .get_optional_str("rfc822_message_id")?
            .map(str::to_string),
        subject: row.get_str("subject")?.to_string(),
        from: json::decode(row.get_str("from_json")?)?,
        reply_to: json::decode(row.get_str("reply_to_json")?)?,
        to: json::decode(row.get_str("to_json")?)?,
        cc: json::decode(row.get_str("cc_json")?)?,
        bcc: json::decode(row.get_str("bcc_json")?)?,
        date: Utc
            .timestamp_opt(row.get_i64("date")?, 0)
            .single()
            .ok_or_else(|| StorageError::Db("invalid message date".into()))?,
        flags: json::decode(row.get_str("flags_json")?)?,
        labels: json::decode(row.get_str("labels_json")?)?,
        snippet: row.get_str("snippet")?.to_string(),
        size: u32::try_from(row.get_i64("size")?)
            .map_err(|e| StorageError::Db(format!("size out of range: {e}")))?,
        has_attachments: row.get_i64("has_attachments")? != 0,
        in_reply_to: row.get_optional_str("in_reply_to")?.map(str::to_string),
        references: json::decode(row.get_str("references_json")?)?,
    })
}
