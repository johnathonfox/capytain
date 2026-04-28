// SPDX-License-Identifier: Apache-2.0

//! `contacts_*` Tauri commands.
//!
//! Phase 2 post-Week-21 PR-C2: surfaces the rows
//! `qsl_storage::repos::contacts` collects into the compose pane's
//! autocomplete dropdown. The collection itself ships in PR-C1; this
//! module is read-only.

use qsl_ipc::{Contact, IpcResult};
use qsl_storage::repos::contacts as contacts_repo;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// Maximum number of contacts the UI ever asks for in one call. The
/// dropdown only shows ~6-8 rows, so anything past this is wasted
/// IPC; we clamp here so a buggy UI can't accidentally page the
/// whole table over to wasm.
const MAX_QUERY_LIMIT: u32 = 32;

#[derive(Debug, Deserialize)]
pub struct ContactsQueryInput {
    pub prefix: String,
    pub limit: u32,
}

/// Prefix-search across `address` and `display_name`, returning up to
/// `limit` rows ordered most-recent-first then most-popular-first.
/// Empty prefix returns an empty list (the dropdown opens after the
/// user has typed >=2 chars; defending here costs nothing).
#[tauri::command]
pub async fn contacts_query(
    state: State<'_, AppState>,
    input: ContactsQueryInput,
) -> IpcResult<Vec<Contact>> {
    let ContactsQueryInput { prefix, limit } = input;
    let limit = limit.clamp(1, MAX_QUERY_LIMIT);

    let db = state.db.lock().await;
    let rows = contacts_repo::query_prefix(&*db, &prefix, limit).await?;
    drop(db);

    let out: Vec<Contact> = rows
        .into_iter()
        .map(|c| Contact {
            address: c.address,
            display_name: c.display_name,
            last_seen_at: c.last_seen_at,
            seen_count: c.seen_count,
        })
        .collect();

    tracing::debug!(
        prefix = %prefix,
        hits = out.len(),
        "contacts_query"
    );
    Ok(out)
}
