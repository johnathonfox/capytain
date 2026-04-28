// SPDX-License-Identifier: Apache-2.0

//! `accounts_*` Tauri commands.
//!
//! See `COMMANDS.md §Accounts` for the catalogue. `accounts_list` is
//! the read path the sidebar consumes; the per-row mutators
//! (`accounts_set_display_name`, `accounts_set_signature`,
//! `accounts_set_notify_enabled`, `accounts_remove`) back the
//! Settings → Accounts tab. `accounts_add_oauth` is still stubbed
//! pending the first-run OAuth flow (PR-O1).

use qsl_core::AccountId;
use qsl_ipc::{Account, IpcResult};
use qsl_storage::repos::accounts as accounts_repo;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// `accounts_list` — return every configured account, ordered by
/// `created_at`. Called by the sidebar on window open and by the
/// Settings → Accounts tab.
#[tauri::command]
pub async fn accounts_list(state: State<'_, AppState>) -> IpcResult<Vec<Account>> {
    let db = state.db.lock().await;
    let accounts = accounts_repo::list(&*db).await?;
    tracing::debug!(count = accounts.len(), "accounts_list");
    Ok(accounts)
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetDisplayNameInput {
    pub id: AccountId,
    pub display_name: String,
}

/// `accounts_set_display_name` — rename an account. The display name
/// is the user-facing label in the sidebar; renaming is rename-safe
/// (id stays put, no folder churn).
#[tauri::command]
pub async fn accounts_set_display_name(
    state: State<'_, AppState>,
    input: AccountsSetDisplayNameInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_display_name(&*db, &input.id, &input.display_name).await?;
    tracing::info!(account = %input.id.0, "accounts_set_display_name");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetSignatureInput {
    pub id: AccountId,
    /// `None` / empty string clears the signature.
    pub signature: Option<String>,
}

/// `accounts_set_signature` — patch the per-account signature
/// appended to outbound mail. The compose pane reads this on draft
/// open; existing drafts are not retroactively rewritten.
#[tauri::command]
pub async fn accounts_set_signature(
    state: State<'_, AppState>,
    input: AccountsSetSignatureInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_signature(&*db, &input.id, input.signature.as_deref()).await?;
    tracing::info!(account = %input.id.0, "accounts_set_signature");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsSetNotifyEnabledInput {
    pub id: AccountId,
    pub enabled: bool,
}

/// `accounts_set_notify_enabled` — toggle whether new-mail
/// notifications fire for this account. Sync continues either way;
/// only the desktop notification bridge consults the flag.
#[tauri::command]
pub async fn accounts_set_notify_enabled(
    state: State<'_, AppState>,
    input: AccountsSetNotifyEnabledInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::set_notify_enabled(&*db, &input.id, input.enabled).await?;
    tracing::info!(account = %input.id.0, enabled = input.enabled, "accounts_set_notify_enabled");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AccountsRemoveInput {
    pub id: AccountId,
}

/// `accounts_remove` — delete an account row. Schema-level cascades
/// take care of folders / messages / threads / outbox / contacts;
/// the in-memory `state.backends` cache is also purged so a
/// re-added-immediately account doesn't hit the stale handle.
#[tauri::command]
pub async fn accounts_remove(
    state: State<'_, AppState>,
    input: AccountsRemoveInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    accounts_repo::delete(&*db, &input.id).await?;
    drop(db);
    state.backends.lock().await.remove(&input.id);
    tracing::info!(account = %input.id.0, "accounts_remove");
    Ok(())
}
