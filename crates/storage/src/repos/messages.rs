// SPDX-License-Identifier: Apache-2.0

//! Message-header persistence. Bodies live on disk via `crate::blobs`;
//! this repo persists only the `MessageHeaders` view plus a `body_path`
//! pointer.

use chrono::{TimeZone, Utc};

use qsl_core::{
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

// `thread_id` (?4) and `body_path` (?18) are intentionally NOT in the
// SET clause: both are locally-computed and never sourced from the
// IMAP / JMAP wire. Including either would make every re-sync
// overwrite the previously-assigned value with the incoming-header
// `None`, leaving the row orphaned from its thread (or its body blob)
// even though the assignment had been correct on the prior insert.
// (`thread_id`: caught against a real Gmail account on 2026-04-27 —
// a reply kept landing with `thread_id = NULL` despite
// `threads.message_count` correctly counting it. `body_path`: same
// shape — a body fetched after the initial insert via
// `set_body_path` would be silently NULLed by the next sync cycle's
// `update`, forcing every reader-pane open to re-fetch from the
// server.) Mirrors the dropped slots: `to_params` still binds 20
// positional values; `?4` and `?18` are simply unreferenced here.
const UPDATE: &str = "
    UPDATE messages
       SET account_id = ?2,
           folder_id = ?3,
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

/// All messages attached to a thread, sorted ascending by date.
/// Drives the stacked-thread reader in the desktop UI: given the
/// currently-selected message, the reader pulls the whole
/// conversation in one round-trip and renders each entry as its own
/// card. Empty result for an unknown thread id (caller already
/// dereferenced through `messages.thread_id`, so this should be
/// non-empty in normal use).
pub async fn list_by_thread(
    conn: &dyn DbConn,
    thread: &ThreadId,
) -> Result<Vec<MessageHeaders>, StorageError> {
    let sql = format!(
        "SELECT {COLS} FROM messages \
         WHERE thread_id = ?1 \
         ORDER BY date ASC"
    );
    let rows = conn
        .query(&sql, Params(vec![Value::Text(&thread.0)]))
        .await?;
    rows.iter().map(row_to_headers).collect()
}

/// Every persisted message id in a folder. Drives the sync engine's
/// server-side-deletion reconciliation pass: after the regular sync
/// it diffs this against `MailBackend::list_known_ids` and deletes
/// the ones the server no longer carries.
pub async fn list_ids_by_folder(
    conn: &dyn DbConn,
    folder: &FolderId,
) -> Result<Vec<MessageId>, StorageError> {
    let rows = conn
        .query(
            "SELECT id FROM messages WHERE folder_id = ?1",
            Params(vec![Value::Text(&folder.0)]),
        )
        .await?;
    rows.iter()
        .map(|r| Ok(MessageId(r.get_str("id")?.to_string())))
        .collect()
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

/// Bulk-insert a slice of headers, skipping rows whose `id` already
/// exists. Returns the count of newly-inserted rows.
///
/// All inserts run inside a single transaction.
///
/// Dedup happens at the engine via `INSERT OR IGNORE` against the
/// `messages` PK. The previous SELECT-IN-then-filter pre-check was
/// the dominant cost (4–5s/chunk on a 1500-element IN list once the
/// table grew past ~25k rows); pushing it into the INSERT lets the
/// engine reject duplicates at the index lookup it was going to do
/// anyway.
///
/// **Multi-row VALUES.** Rows are flushed via
/// `INSERT OR IGNORE INTO messages (cols) VALUES (...), (...), ...`
/// — one statement per group of up to 1500 rows (SQLite's 32766-
/// placeholder limit divided by 20 columns, with headroom).
///
/// `body_path` is always `None` here — history-pull persists headers
/// only and lets the on-demand fetch path populate bodies later.
pub async fn batch_insert_skip_existing(
    conn: &dyn DbConn,
    headers: &[MessageHeaders],
) -> Result<u32, StorageError> {
    if headers.is_empty() {
        return Ok(0);
    }

    // Build all groups (SQL string + flattened param vec) BEFORE
    // opening the transaction. JSON encoding (with empty-vec fast
    // paths in `to_params`) and SQL string assembly run with no DB
    // lock held; the transaction window narrows to just the syscall
    // path inside Turso.
    let mut groups: Vec<(String, Vec<Value>)> =
        Vec::with_capacity(headers.len().div_ceil(BATCH_INSERT_GROUP_SIZE));
    for group in headers.chunks(BATCH_INSERT_GROUP_SIZE) {
        let mut params: Vec<Value> = Vec::with_capacity(group.len() * COLS_PER_ROW);
        for h in group {
            let mut p = to_params(h, None)?;
            params.append(&mut p.0);
        }
        let sql = build_multi_insert_sql(group.len());
        groups.push((sql, params));
    }

    let mut tx = conn.begin().await?;
    let mut inserted: u64 = 0;
    for (sql, params) in groups.drain(..) {
        inserted += tx.execute(&sql, Params(params)).await?;
    }
    tx.commit().await?;
    Ok(inserted as u32)
}

/// 20 placeholders per row — `INSERT` template above. Used to size
/// the placeholder budget for the multi-row INSERT path.
const COLS_PER_ROW: usize = 20;

/// Max rows per multi-row INSERT statement. SQLite's compile-time
/// `SQLITE_MAX_VARIABLE_NUMBER` is 32766; at 20 placeholders per row
/// that allows 1638. We round down to 1500 to leave headroom in case
/// a future migration grows the column count.
const BATCH_INSERT_GROUP_SIZE: usize = 1500;

/// Build the multi-row INSERT statement for `n` rows: one VALUES
/// tuple per row, with sequential `?N` placeholders.
fn build_multi_insert_sql(n: usize) -> String {
    debug_assert!(n > 0, "build_multi_insert_sql called with zero rows");
    let mut sql = String::with_capacity(256 + n * 80);
    sql.push_str(
        "INSERT OR IGNORE INTO messages \
         (id, account_id, folder_id, thread_id, rfc822_message_id, \
          subject, from_json, reply_to_json, to_json, cc_json, bcc_json, \
          date, flags_json, labels_json, snippet, size, has_attachments, \
          body_path, in_reply_to, references_json) \
         VALUES ",
    );
    for row in 0..n {
        if row > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for col in 0..COLS_PER_ROW {
            if col > 0 {
                sql.push_str(", ");
            }
            let idx = row * COLS_PER_ROW + col + 1;
            sql.push('?');
            sql.push_str(&idx.to_string());
        }
        sql.push(')');
    }
    sql
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
        encode_addr_vec(&h.from)?,
        encode_addr_vec(&h.reply_to)?,
        encode_addr_vec(&h.to)?,
        encode_addr_vec(&h.cc)?,
        encode_addr_vec(&h.bcc)?,
        Value::Integer(h.date.timestamp()),
        Value::OwnedText(json::encode(&h.flags)?),
        encode_str_vec(&h.labels)?,
        Value::Text(&h.snippet),
        Value::Integer(h.size.into()),
        Value::Integer(h.has_attachments.into()),
        body_path.map(Value::Text).unwrap_or(Value::Null),
        h.in_reply_to
            .as_deref()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        encode_str_vec(&h.references)?,
    ]))
}

/// JSON-encode a vec of email addresses, fast-pathing the empty case
/// to a borrowed `"[]"`. Most history-pull rows have empty
/// reply_to/cc/bcc; serde was responsible for ~7 string allocs per row
/// per chunk — now zero for empty, one allocation for non-empty.
fn encode_addr_vec(v: &[qsl_core::EmailAddress]) -> Result<Value<'static>, StorageError> {
    if v.is_empty() {
        Ok(Value::Text("[]"))
    } else {
        Ok(Value::OwnedText(json::encode(v)?))
    }
}

/// Same fast path for `Vec<String>` (labels, references). Most rows
/// have empty `references`; many have empty `labels`.
fn encode_str_vec(v: &[String]) -> Result<Value<'static>, StorageError> {
    if v.is_empty() {
        Ok(Value::Text("[]"))
    } else {
        Ok(Value::OwnedText(json::encode(v)?))
    }
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
