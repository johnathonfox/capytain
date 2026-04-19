// SPDX-License-Identifier: Apache-2.0

//! `folders_*` Tauri commands.
//!
//! See `COMMANDS.md §Folders`. This module currently implements
//! `folders_list`; `folders_list_unified` and `folders_refresh` arrive
//! in Phase 1 once the unified-inbox UX and background sync engine
//! land.

use capytain_ipc::{AccountId, Folder, IpcResult};
use capytain_storage::repos::folders as folders_repo;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// Inputs for `folders_list`. Deserialized from Tauri's JSON payload.
#[derive(Debug, Deserialize)]
pub struct FoldersListInput {
    pub account: AccountId,
}

/// `folders_list` — return the persisted folders for `account`.
///
/// The sidebar calls this on account selection. The list comes out of
/// the local Turso cache, not a live IMAP/JMAP call; the sync engine
/// (Phase 1) is responsible for keeping the cache fresh.
#[tauri::command]
pub async fn folders_list(
    state: State<'_, AppState>,
    input: FoldersListInput,
) -> IpcResult<Vec<Folder>> {
    let db = state.db.lock().await;
    let folders = folders_repo::list_by_account(&*db, &input.account).await?;
    tracing::debug!(account = %input.account.0, count = folders.len(), "folders_list");
    Ok(folders)
}
