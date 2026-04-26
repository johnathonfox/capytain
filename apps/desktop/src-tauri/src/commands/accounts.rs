// SPDX-License-Identifier: Apache-2.0

//! `accounts_*` Tauri commands.
//!
//! See `COMMANDS.md §Accounts` for the catalogue. This file currently
//! implements only `accounts_list`; the remaining commands
//! (`accounts_add_oauth`, `accounts_remove`, `accounts_get_status`,
//! `accounts_set_display_name`) arrive in Week 5 part 2 once the
//! settings UI needs them.

use qsl_ipc::{Account, IpcResult};
use qsl_storage::repos::accounts as accounts_repo;
use tauri::State;

use crate::state::AppState;

/// `accounts_list` — return every configured account, ordered by
/// `created_at`. Called by the sidebar on window open.
///
/// This is the Phase 0 Week 5 proof-of-life command: it exercises
/// the full wiring chain (Dioxus → `invoke` → Tauri router → `State<AppState>` →
/// `qsl_storage::repos::accounts` → Turso → back across the IPC boundary
/// as serde JSON) without needing the backend cache, keyring, or OAuth flow.
#[tauri::command]
pub async fn accounts_list(state: State<'_, AppState>) -> IpcResult<Vec<Account>> {
    let db = state.db.lock().await;
    let accounts = accounts_repo::list(&*db).await?;
    tracing::debug!(count = accounts.len(), "accounts_list");
    Ok(accounts)
}
