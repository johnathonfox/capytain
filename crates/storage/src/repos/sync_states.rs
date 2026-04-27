// SPDX-License-Identifier: Apache-2.0

//! Persistence for [`SyncState`] — the opaque per-folder sync cursor.
//!
//! Stored inline on the `folders.sync_state` TEXT column. The core never
//! inspects the payload; it's whatever the backend adapter wrote.

use qsl_core::{FolderId, StorageError, SyncState};

use crate::conn::{DbConn, Params, Value};

const SELECT_SYNC_STATE: &str = "SELECT sync_state FROM folders WHERE id = ?1";
const UPDATE_SYNC_STATE: &str = "UPDATE folders SET sync_state = ?2 WHERE id = ?1";

/// Load the sync cursor for a folder. Returns `Ok(None)` when the column is
/// NULL (never persisted) and [`StorageError::NotFound`] when the folder
/// itself does not exist.
pub async fn get(conn: &dyn DbConn, folder: &FolderId) -> Result<Option<SyncState>, StorageError> {
    let row = conn
        .query_opt(SELECT_SYNC_STATE, Params(vec![Value::Text(&folder.0)]))
        .await?
        .ok_or(StorageError::NotFound)?;
    let opt = row.get_optional_str("sync_state")?;
    Ok(opt.map(|backend_state| SyncState {
        folder_id: folder.clone(),
        backend_state: backend_state.to_string(),
    }))
}

/// Persist a sync cursor, replacing any previous value. Returns
/// [`StorageError::NotFound`] if the folder does not exist.
pub async fn put(conn: &dyn DbConn, state: &SyncState) -> Result<(), StorageError> {
    let affected = conn
        .execute(
            UPDATE_SYNC_STATE,
            Params(vec![
                Value::Text(&state.folder_id.0),
                Value::Text(&state.backend_state),
            ]),
        )
        .await?;
    if affected == 0 {
        Err(StorageError::NotFound)
    } else {
        Ok(())
    }
}

/// Clear the stored cursor for a folder. No-op if already clear or folder
/// missing.
pub async fn clear(conn: &dyn DbConn, folder: &FolderId) -> Result<(), StorageError> {
    conn.execute(
        UPDATE_SYNC_STATE,
        Params(vec![Value::Text(&folder.0), Value::Null]),
    )
    .await
    .map(|_| ())
}
