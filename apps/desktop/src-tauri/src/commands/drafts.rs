// SPDX-License-Identifier: Apache-2.0

//! `drafts_*` Tauri commands. Phase 2 Week 17.
//!
//! Local-only persistence for outgoing messages-in-progress. The
//! upstream-sync side (writing to the server's Drafts mailbox) lands
//! in Week 20 via a `save_draft` outbox op; today these commands
//! never touch the network. The compose pane uses
//! `drafts_save` on every keystroke-debounced auto-save tick and on
//! manual Save / Discard, `drafts_load` to round-trip a draft after
//! window close + re-open or process restart, and `drafts_list` to
//! populate a "Drafts" sidebar entry.

use chrono::Utc;

use capytain_core::{Draft, DraftAttachment, DraftBodyKind, EmailAddress};
use capytain_ipc::{AccountId, DraftId, IpcResult};
use capytain_storage::repos::drafts as drafts_repo;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// Wire-shape draft sent up by the compose pane. Mirrors [`Draft`]
/// minus the timestamps (assigned server-side) and with `id` made
/// optional so a fresh compose can be saved without the UI inventing
/// an id first.
#[derive(Debug, Deserialize)]
pub struct DraftInput {
    #[serde(default)]
    pub id: Option<DraftId>,
    pub account_id: AccountId,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub to: Vec<EmailAddress>,
    #[serde(default)]
    pub cc: Vec<EmailAddress>,
    #[serde(default)]
    pub bcc: Vec<EmailAddress>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub body_kind: DraftBodyKind,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Debug, Deserialize)]
pub struct DraftsSaveInput {
    pub draft: DraftInput,
}

/// `drafts_save` — upsert a draft. Returns the draft's id (newly
/// minted on first save) so the UI can swap from "unsaved" state
/// into "saved-with-id" without a follow-up round-trip.
#[tauri::command]
pub async fn drafts_save(state: State<'_, AppState>, input: DraftsSaveInput) -> IpcResult<DraftId> {
    let DraftsSaveInput { draft } = input;
    let now = Utc::now();

    let db = state.db.lock().await;
    let (id, created_at): (DraftId, chrono::DateTime<chrono::Utc>) = match draft.id {
        Some(existing_id) => {
            // Preserve the original `created_at` if the draft was
            // already in the table, otherwise this is a UI-issued
            // id we've never seen — treat the save as create.
            match drafts_repo::find(&*db, &existing_id).await? {
                Some(existing) => (existing_id, existing.created_at),
                None => (existing_id, now),
            }
        }
        None => (drafts_repo::new_id(), now),
    };

    let row = Draft {
        id: id.clone(),
        account_id: draft.account_id,
        in_reply_to: draft.in_reply_to,
        references: draft.references,
        to: draft.to,
        cc: draft.cc,
        bcc: draft.bcc,
        subject: draft.subject,
        body: draft.body,
        body_kind: draft.body_kind,
        attachments: draft.attachments,
        created_at,
        updated_at: now,
    };
    drafts_repo::save(&*db, &row).await?;
    drop(db);

    tracing::debug!(id = %id.0, "drafts_save");
    Ok(id)
}

#[derive(Debug, Deserialize)]
pub struct DraftsLoadInput {
    pub id: DraftId,
}

/// `drafts_load` — fetch a single draft. Used when the user
/// re-opens compose from the sidebar's Drafts entry or after
/// process restart.
#[tauri::command]
pub async fn drafts_load(state: State<'_, AppState>, input: DraftsLoadInput) -> IpcResult<Draft> {
    let db = state.db.lock().await;
    Ok(drafts_repo::get(&*db, &input.id).await?)
}

#[derive(Debug, Deserialize)]
pub struct DraftsListInput {
    pub account_id: AccountId,
}

/// `drafts_list` — every draft for one account, newest-edited first.
/// Sized so the entire result set fits in a single IPC payload — no
/// pagination today; if the working set grows past hundreds of
/// drafts we'll add an offset/limit shape mirroring `messages_list`.
#[tauri::command]
pub async fn drafts_list(
    state: State<'_, AppState>,
    input: DraftsListInput,
) -> IpcResult<Vec<Draft>> {
    let db = state.db.lock().await;
    Ok(drafts_repo::list_by_account(&*db, &input.account_id).await?)
}

#[derive(Debug, Deserialize)]
pub struct DraftsDeleteInput {
    pub id: DraftId,
}

/// `drafts_delete` — discard a draft. Idempotent: deleting a
/// non-existent id returns `Ok(())`.
#[tauri::command]
pub async fn drafts_delete(
    state: State<'_, AppState>,
    input: DraftsDeleteInput,
) -> IpcResult<()> {
    let db = state.db.lock().await;
    drafts_repo::delete(&*db, &input.id).await?;
    Ok(())
}
