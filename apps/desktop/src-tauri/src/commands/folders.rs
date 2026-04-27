// SPDX-License-Identifier: Apache-2.0

//! `folders_*` Tauri commands.
//!
//! See `COMMANDS.md §Folders`. This module currently implements
//! `folders_list`; `folders_list_unified` and `folders_refresh` arrive
//! in Phase 1 once the unified-inbox UX and background sync engine
//! land.

use qsl_ipc::{AccountId, Folder, IpcResult};
use qsl_storage::repos::{folders as folders_repo, messages as messages_repo};
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
    let mut folders = folders_repo::list_by_account(&*db, &input.account).await?;
    // The persisted `unread_count` / `total_count` columns aren't
    // updated on every message insert (the sync engine writes
    // headers without round-tripping the parent folder row), so
    // recompute live from the messages table for the sidebar
    // badges. <100 folders × one indexed COUNT(*) each is cheap;
    // measurable cost only emerges in the >1k folder regime, which
    // we're nowhere near.
    for f in folders.iter_mut() {
        f.unread_count = messages_repo::count_unread_by_folder(&*db, &f.id)
            .await
            .unwrap_or(0);
        f.total_count = messages_repo::count_by_folder(&*db, &f.id)
            .await
            .unwrap_or(0);
    }
    // PR-R1 diagnostic: log every folder's id + role so we can see
    // exactly what crosses the IPC boundary. The screenshot
    // 2026-04-27 showed only INBOX + All Mail in MAILBOXES even
    // though the DB has 7 [Gmail]/* folders with valid roles. This
    // line will tell us whether roles are dropping at the IPC
    // boundary (serde issue) or surviving fine (UI splitter issue).
    // Remove once the regression is root-caused and fixed.
    for f in folders.iter() {
        tracing::info!(
            id = %f.id.0,
            name = %f.name,
            role = ?f.role,
            "folders_list: row"
        );
    }
    tracing::info!(account = %input.account.0, count = folders.len(), "folders_list");
    Ok(folders)
}
