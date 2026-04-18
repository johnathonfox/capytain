// SPDX-License-Identifier: Apache-2.0

//! Attachment metadata persistence. The bytes themselves are fetched on
//! demand and never stored by this repo.

use capytain_core::{Attachment, AttachmentRef, MessageId, StorageError};

use crate::conn::{DbConn, Params, Row, Value};

const INSERT: &str = "
    INSERT INTO attachments
        (id, message_id, filename, mime_type, size, inline, content_id, path)
    VALUES
        (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
";

const SELECT_BY_MESSAGE: &str = "
    SELECT id, message_id, filename, mime_type, size, inline, content_id, path
      FROM attachments
     WHERE message_id = ?1
     ORDER BY id ASC
";

const DELETE_BY_ID: &str = "DELETE FROM attachments WHERE id = ?1";

pub async fn insert(
    conn: &dyn DbConn,
    message: &MessageId,
    attachment: &Attachment,
) -> Result<(), StorageError> {
    let size_i64 = i64::try_from(attachment.size).map_err(|e| StorageError::Db(e.to_string()))?;
    conn.execute(
        INSERT,
        Params(vec![
            Value::Text(&attachment.id.0),
            Value::Text(&message.0),
            Value::Text(&attachment.filename),
            Value::Text(&attachment.mime_type),
            Value::Integer(size_i64),
            Value::Integer(attachment.inline.into()),
            attachment
                .content_id
                .as_deref()
                .map(Value::Text)
                .unwrap_or(Value::Null),
            Value::Null, // `path` populated later by the blob writer
        ]),
    )
    .await
    .map(|_| ())
}

pub async fn list_by_message(
    conn: &dyn DbConn,
    message: &MessageId,
) -> Result<Vec<Attachment>, StorageError> {
    let rows = conn
        .query(SELECT_BY_MESSAGE, Params(vec![Value::Text(&message.0)]))
        .await?;
    rows.iter().map(row_to_attachment).collect()
}

pub async fn delete(conn: &dyn DbConn, id: &AttachmentRef) -> Result<(), StorageError> {
    conn.execute(DELETE_BY_ID, Params(vec![Value::Text(&id.0)]))
        .await
        .map(|_| ())
}

fn row_to_attachment(row: &Row) -> Result<Attachment, StorageError> {
    let size_i64 = row.get_i64("size")?;
    Ok(Attachment {
        id: AttachmentRef(row.get_str("id")?.to_string()),
        filename: row.get_str("filename")?.to_string(),
        mime_type: row.get_str("mime_type")?.to_string(),
        size: u64::try_from(size_i64)
            .map_err(|e| StorageError::Db(format!("size out of range: {e}")))?,
        inline: row.get_i64("inline")? != 0,
        content_id: row.get_optional_str("content_id")?.map(str::to_string),
    })
}
