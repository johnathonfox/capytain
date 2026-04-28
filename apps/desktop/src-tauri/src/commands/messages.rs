// SPDX-License-Identifier: Apache-2.0

//! `messages_*` Tauri commands.
//!
//! Implements the Phase 0 Week 5 read-path surface of
//! `COMMANDS.md §Messages`: `messages_list` and `messages_get`. The
//! write-path commands (`messages_mark_read`, `messages_flag`,
//! `messages_move`, `messages_archive`, `messages_delete`,
//! `messages_download_attachment`) land in Phase 1 alongside the
//! outbox / optimistic-mutation engine.

use base64::engine::general_purpose::STANDARD as base64_engine;
use base64::Engine as _;
use qsl_core::{EmailAddress, FolderRole, MessageFlags, MessageHeaders};
use qsl_ipc::{DraftId, FolderId, IpcResult, MessageId, MessagePage, RenderedMessage, SortOrder};
use qsl_mime::{
    compose::build_rfc5322, parse_rfc822, sanitize_email_html, sanitize_email_html_trusted,
    MessageIdentity,
};
use qsl_storage::{
    repos::accounts as accounts_repo, repos::drafts as drafts_repo, repos::folders as folders_repo,
    repos::messages as messages_repo, repos::outbox as outbox_repo, repos::remote_content_opt_ins,
    BlobStore,
};
use serde::Deserialize;
use tauri::State;

use crate::backend_factory;
use crate::state::AppState;

/// Phase 0 Week 5 caps a single page to 500 headers. Sync engine paging
/// (Phase 1) negotiates higher bounds directly with the backend.
const MAX_PAGE_LIMIT: u32 = 500;

#[derive(Debug, Deserialize)]
pub struct MessagesListInput {
    pub folder: FolderId,
    pub limit: u32,
    pub offset: u32,
    #[serde(default)]
    pub sort: SortOrder,
}

/// `messages_list` — return one page of a folder's message list.
///
/// Reads from the local cache; sorting beyond `DateDesc` is a Phase 1
/// concern, so for now non-`DateDesc` sorts fall back to `DateDesc` and
/// emit a `tracing` warning. The UI should still feel responsive: the
/// Phase 0 proof-of-life inbox is sorted newest-first anyway.
#[tauri::command]
pub async fn messages_list(
    state: State<'_, AppState>,
    input: MessagesListInput,
) -> IpcResult<MessagePage> {
    let MessagesListInput {
        folder,
        limit,
        offset,
        sort,
    } = input;

    let limit = limit.min(MAX_PAGE_LIMIT);
    if sort != SortOrder::DateDesc {
        tracing::warn!(
            requested = ?sort,
            "messages_list: Phase 0 Week 5 only implements date_desc — falling back"
        );
    }

    let db = state.db.lock().await;
    let messages = messages_repo::list_by_folder(&*db, &folder, limit, offset).await?;
    let total_count = messages_repo::count_by_folder(&*db, &folder).await?;
    let unread_count = messages_repo::count_unread_by_folder(&*db, &folder).await?;

    tracing::debug!(
        folder = %folder.0,
        page = messages.len(),
        total = total_count,
        unread = unread_count,
        "messages_list"
    );

    Ok(MessagePage {
        messages,
        total_count,
        unread_count,
    })
}

#[derive(Debug, Deserialize)]
pub struct MessagesListUnifiedInput {
    pub limit: u32,
    pub offset: u32,
    #[serde(default)]
    pub sort: SortOrder,
}

/// `messages_list_unified` — one page of the unified inbox: every
/// account's INBOX-role folder merged and sorted by date desc.
///
/// Resolves the set of INBOX folders by `folders::list_by_role` so
/// we don't depend on a hardcoded folder-name convention; works
/// for both IMAP (`\Inbox` SPECIAL-USE) and JMAP (`Mailbox.role =
/// inbox`) accounts because both adapters normalize to
/// `FolderRole::Inbox` at sync time.
#[tauri::command]
pub async fn messages_list_unified(
    state: State<'_, AppState>,
    input: MessagesListUnifiedInput,
) -> IpcResult<MessagePage> {
    let MessagesListUnifiedInput {
        limit,
        offset,
        sort,
    } = input;
    let limit = limit.min(MAX_PAGE_LIMIT);
    if sort != SortOrder::DateDesc {
        tracing::warn!(
            requested = ?sort,
            "messages_list_unified: Phase 1 Week 12 only implements date_desc — falling back"
        );
    }

    let db = state.db.lock().await;
    let inboxes = folders_repo::list_by_role(&*db, FolderRole::Inbox).await?;
    let folder_ids: Vec<FolderId> = inboxes.iter().map(|f| f.id.clone()).collect();
    let messages = messages_repo::list_by_folders(&*db, &folder_ids, limit, offset).await?;
    let total_count = messages_repo::count_by_folders(&*db, &folder_ids).await?;
    let unread_count = messages_repo::count_unread_by_folders(&*db, &folder_ids).await?;
    drop(db);

    tracing::debug!(
        inbox_folders = inboxes.len(),
        page = messages.len(),
        total = total_count,
        unread = unread_count,
        "messages_list_unified"
    );

    Ok(MessagePage {
        messages,
        total_count,
        unread_count,
    })
}

#[derive(Debug, Deserialize)]
pub struct MessagesGetInput {
    pub id: MessageId,
    /// One-shot override that forces the trusted sanitizer for this
    /// render. Used by the reader-pane "Load images" button so the
    /// user can let remote content through for a single message
    /// without persistently trusting the sender. Defaults to `false`.
    #[serde(default)]
    pub force_trusted: bool,
}

/// `messages_get` — hydrate a single message for the reader pane.
///
/// - Always returns headers from the local Turso cache.
/// - When the body blob is on disk (`body_path` non-null), parses it
///   and returns `body_text` / `sanitized_html` / `attachments`.
/// - When the body blob is missing — header-only row from a
///   pre-Week-9 sync, or one whose body fetch failed during the
///   sync cycle's body-fetch pass — falls back to a live
///   `fetch_raw_message` against the cached `MailBackend`,
///   persists the bytes to the blob store, and continues with the
///   parse path. A failed lazy fetch logs a warning and returns
///   headers-only rather than surfacing the error to the UI.
/// - `sender_is_trusted` is true when the message's first `From`
///   address is recorded in `remote_content_opt_ins` for this
///   account. Trusted senders skip the remote-content URL filter
///   inside `sanitize_email_html_trusted` so their image pixels,
///   stylesheets, and fonts render. `remote_content_blocked` is
///   the inverse — true when the sanitizer was the blocking
///   variant. The UI uses both to decide whether to show a "load
///   remote content" banner.
#[tauri::command]
pub async fn messages_get(
    state: State<'_, AppState>,
    input: MessagesGetInput,
) -> IpcResult<RenderedMessage> {
    tracing::debug!(id = %input.id.0, "messages_get");
    let db = state.db.lock().await;
    let headers = messages_repo::get(&*db, &input.id).await?;
    let body_path = messages_repo::body_path(&*db, &input.id).await?;
    // `input.force_trusted` is the per-render override used by the
    // reader's "Load images" banner button — it bypasses the opt-in
    // check for one render without writing anything to storage.
    let sender_is_trusted = if input.force_trusted {
        true
    } else {
        match headers.from.first() {
            Some(addr) if !addr.address.is_empty() => {
                remote_content_opt_ins::is_trusted(&*db, &headers.account_id, &addr.address).await?
            }
            _ => false,
        }
    };
    tracing::debug!(
        id = %input.id.0,
        sender_is_trusted,
        "messages_get sanitize path"
    );
    drop(db);

    let blobs = BlobStore::new(state.data_dir.join("blobs"));
    let bytes = if body_path.is_some() {
        load_cached_body(&blobs, &headers).await
    } else {
        lazy_fetch_body(&state, &blobs, &headers).await
    };

    let (body_text, sanitized_html, attachments) = match bytes {
        Some(bytes) => parse_and_sanitize(&bytes, &headers, sender_is_trusted),
        None => (None, None, Vec::new()),
    };

    Ok(RenderedMessage {
        headers,
        sanitized_html,
        body_text,
        attachments,
        sender_is_trusted,
        remote_content_blocked: !sender_is_trusted,
    })
}

#[derive(Debug, Deserialize)]
pub struct MessagesTrustSenderInput {
    pub account_id: qsl_core::AccountId,
    pub email_address: String,
}

/// Persist a per-sender remote-content opt-in. Subsequent
/// `messages_get` calls for any message whose `From` is `email_address`
/// on `account_id` will run the trusted sanitizer (images,
/// stylesheets, fonts not stripped). Existing rows are upserted.
///
/// Backs the reader-pane "Always load from this sender" banner button.
#[tauri::command]
pub async fn messages_trust_sender(
    state: State<'_, AppState>,
    input: MessagesTrustSenderInput,
) -> IpcResult<()> {
    let MessagesTrustSenderInput {
        account_id,
        email_address,
    } = input;
    let db = state.db.lock().await;
    remote_content_opt_ins::add(&*db, &account_id, &email_address).await?;
    drop(db);
    tracing::info!(account = %account_id.0, %email_address, "messages_trust_sender: added opt-in");
    Ok(())
}

/// Read a previously-fetched body blob from disk. A stale
/// `body_path` with no file on disk is a cache bug, not a
/// user-visible error: log and return `None`.
async fn load_cached_body(blobs: &BlobStore, headers: &MessageHeaders) -> Option<Vec<u8>> {
    match blobs
        .get(&headers.account_id, &headers.folder_id, &headers.id)
        .await
    {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!(id = %headers.id.0, "messages_get: body blob missing: {e}");
            None
        }
    }
}

/// Fetch the body live from the backend, persist to the blob store,
/// and update `body_path`. Any failure (no backend, network, parse,
/// storage write) logs a warning and returns `None` — the reader
/// pane still renders headers + plaintext-fallback.
async fn lazy_fetch_body(
    state: &AppState,
    blobs: &BlobStore,
    headers: &MessageHeaders,
) -> Option<Vec<u8>> {
    let backend = match backend_factory::get_or_open(state, &headers.account_id).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                account = %headers.account_id.0,
                "messages_get: cannot open backend for lazy fetch: {e}"
            );
            return None;
        }
    };

    let raw = match backend.fetch_raw_message(&headers.id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(id = %headers.id.0, "messages_get: lazy fetch failed: {e}");
            return None;
        }
    };

    let path = match blobs
        .put(&headers.account_id, &headers.folder_id, &headers.id, &raw)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(id = %headers.id.0, "messages_get: blob store write failed: {e}");
            // Bytes in hand are still useful even if persistence
            // failed — return them anyway so the user sees their
            // message.
            return Some(raw);
        }
    };

    let db = state.db.lock().await;
    if let Err(e) =
        messages_repo::set_body_path(&*db, &headers.id, Some(&path.to_string_lossy())).await
    {
        tracing::warn!(id = %headers.id.0, "messages_get: set_body_path failed: {e}");
    }

    Some(raw)
}

/// Run the parse + ammonia pass over a body blob and split the
/// returned `MessageBody` into the three fields the IPC contract
/// expects.
fn parse_and_sanitize(
    bytes: &[u8],
    headers: &MessageHeaders,
    sender_is_trusted: bool,
) -> (Option<String>, Option<String>, Vec<qsl_core::Attachment>) {
    let parsed = parse_rfc822(
        bytes,
        MessageIdentity {
            id: &headers.id,
            account_id: &headers.account_id,
            folder_id: &headers.folder_id,
            thread_id: headers.thread_id.as_ref(),
            size: headers.size,
            flags: &headers.flags,
            labels: &headers.labels,
        },
    );
    match parsed {
        Some(body) => {
            // The trusted-sender variant skips only the
            // remote-content URL filter — script/iframe/event-
            // handler/etc. stripping is unconditional regardless of
            // trust.
            let sanitize = if sender_is_trusted {
                sanitize_email_html_trusted
            } else {
                sanitize_email_html
            };
            let sanitized = body.body_html.as_deref().map(sanitize);
            (body.body_text, sanitized, body.attachments)
        }
        None => (None, None, Vec::new()),
    }
}

#[derive(Debug, Deserialize)]
pub struct MessagesMarkReadInput {
    pub ids: Vec<MessageId>,
    /// `true` marks read (sets `\Seen`); `false` marks unread.
    pub seen: bool,
}

/// `messages_mark_read` — flip the seen flag locally and queue an
/// outbox entry for the server.
///
/// Optimistic shape per `PHASE_1.md` Week 14: apply the local
/// update first, enqueue the outbox row second, return. The
/// background drain worker dispatches the row to
/// `MailBackend::update_flags` and on success deletes the row.
/// On failure it backs off; after `MAX_ATTEMPTS` the row enters
/// the dead-letter state and the UI surfaces a "failed to sync"
/// banner.
///
/// Per-account grouping: the outbox is keyed on `account_id`, so
/// mixed-account batches enqueue one row per account. The
/// `payload_json` shape is documented next to
/// `qsl_sync::outbox_drain::FlagsPayload`.
#[tauri::command]
pub async fn messages_mark_read(
    state: State<'_, AppState>,
    input: MessagesMarkReadInput,
) -> IpcResult<()> {
    let MessagesMarkReadInput { ids, seen } = input;
    if ids.is_empty() {
        return Ok(());
    }

    let db = state.db.lock().await;

    // Optimistic local update: read each row, flip the bit, write.
    // We don't pre-batch by account here because flag updates touch
    // a single row each — pulling the existing record gives us the
    // other flags so a future polish-pass `update_flags` against
    // multiple bits works cleanly.
    let mut by_account: std::collections::HashMap<qsl_core::AccountId, Vec<MessageId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let mut headers = match messages_repo::get(&*db, id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(id = %id.0, "messages_mark_read: skipping unknown id: {e}");
                continue;
            }
        };
        if headers.flags.seen == seen {
            continue;
        }
        headers.flags.seen = seen;
        if let Err(e) = messages_repo::update_flags(&*db, id, &headers.flags).await {
            tracing::warn!(id = %id.0, "messages_mark_read: local update failed: {e}");
            continue;
        }
        by_account
            .entry(headers.account_id)
            .or_default()
            .push(id.clone());
    }

    // Queue one outbox row per account. Payload is JSON of the
    // Vec<MessageId> + the desired flag delta — drain dispatches.
    for (account, ids) in by_account {
        let add = MessageFlags {
            seen,
            ..Default::default()
        };
        let remove = MessageFlags {
            seen: !seen,
            ..Default::default()
        };
        let payload = serde_json::json!({
            "ids": ids,
            "add": add,
            "remove": remove,
        });
        outbox_repo::enqueue(&*db, &account, "update_flags", &payload.to_string()).await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesFlagInput {
    pub ids: Vec<MessageId>,
    /// `true` sets `\Flagged` (the user's "starred" / "important"
    /// indicator); `false` clears it.
    pub flagged: bool,
}

/// `messages_flag` — flip the flagged ("starred") bit. Mirrors
/// [`messages_mark_read`]: optimistic local update + outbox row,
/// returns immediately.
#[tauri::command]
pub async fn messages_flag(state: State<'_, AppState>, input: MessagesFlagInput) -> IpcResult<()> {
    let MessagesFlagInput { ids, flagged } = input;
    if ids.is_empty() {
        return Ok(());
    }

    let db = state.db.lock().await;
    let mut by_account: std::collections::HashMap<qsl_core::AccountId, Vec<MessageId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let mut headers = match messages_repo::get(&*db, id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(id = %id.0, "messages_flag: skipping unknown id: {e}");
                continue;
            }
        };
        if headers.flags.flagged == flagged {
            continue;
        }
        headers.flags.flagged = flagged;
        if let Err(e) = messages_repo::update_flags(&*db, id, &headers.flags).await {
            tracing::warn!(id = %id.0, "messages_flag: local update failed: {e}");
            continue;
        }
        by_account
            .entry(headers.account_id)
            .or_default()
            .push(id.clone());
    }

    for (account, ids) in by_account {
        let add = MessageFlags {
            flagged,
            ..Default::default()
        };
        let remove = MessageFlags {
            flagged: !flagged,
            ..Default::default()
        };
        let payload = serde_json::json!({
            "ids": ids,
            "add": add,
            "remove": remove,
        });
        outbox_repo::enqueue(&*db, &account, "update_flags", &payload.to_string()).await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesMoveInput {
    pub ids: Vec<MessageId>,
    pub target: FolderId,
}

/// `messages_move` — relocate messages into `target`. Local
/// optimistic update flips `messages.folder_id`; the outbox row
/// drives the server-side move (IMAP MOVE / Email/set mailboxIds)
/// once the drain wakes up.
#[tauri::command]
pub async fn messages_move(state: State<'_, AppState>, input: MessagesMoveInput) -> IpcResult<()> {
    let MessagesMoveInput { ids, target } = input;
    if ids.is_empty() {
        return Ok(());
    }

    let db = state.db.lock().await;
    let mut by_account: std::collections::HashMap<qsl_core::AccountId, Vec<MessageId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let headers = match messages_repo::get(&*db, id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(id = %id.0, "messages_move: skipping unknown id: {e}");
                continue;
            }
        };
        if headers.folder_id == target {
            continue;
        }
        if let Err(e) = messages_repo::set_folder(&*db, id, &target).await {
            tracing::warn!(id = %id.0, "messages_move: local update failed: {e}");
            continue;
        }
        by_account
            .entry(headers.account_id)
            .or_default()
            .push(id.clone());
    }

    for (account, ids) in by_account {
        let payload = serde_json::json!({
            "ids": ids,
            "target": target,
        });
        outbox_repo::enqueue(&*db, &account, "move_messages", &payload.to_string()).await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesArchiveInput {
    pub ids: Vec<MessageId>,
}

/// `messages_archive` — archive messages by moving them to the
/// account's archive target. Resolution per account: prefer a folder
/// with `FolderRole::Archive` (Fastmail-style), then fall back to
/// `FolderRole::All` (Gmail's `[Gmail]/All Mail` — moving INBOX → All
/// Mail removes the INBOX label, which is exactly Gmail's archive
/// semantic).
///
/// Local update flips `messages.folder_id`; the existing
/// `OP_MOVE` outbox op carries the move to the wire. We do not
/// introduce a separate "archive" op — the move-to-correct-target
/// abstraction handles both Gmail and Fastmail correctly through the
/// backend's `move_messages` (UID MOVE on IMAP, `Email/set
/// mailboxIds` on JMAP).
#[tauri::command]
pub async fn messages_archive(
    state: State<'_, AppState>,
    input: MessagesArchiveInput,
) -> IpcResult<()> {
    let MessagesArchiveInput { ids } = input;
    if ids.is_empty() {
        return Ok(());
    }

    let db = state.db.lock().await;
    // Group ids by account so we resolve the archive target once per
    // account, not per message.
    let mut by_account: std::collections::HashMap<qsl_core::AccountId, Vec<MessageId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let headers = match messages_repo::get(&*db, id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(id = %id.0, "messages_archive: skipping unknown id: {e}");
                continue;
            }
        };
        by_account
            .entry(headers.account_id)
            .or_default()
            .push(id.clone());
    }

    for (account, account_ids) in by_account {
        let folders = folders_repo::list_by_account(&*db, &account).await?;
        let target = resolve_archive_target(&folders).ok_or_else(|| {
            qsl_ipc::IpcError::new(
                qsl_ipc::IpcErrorKind::NotFound,
                format!(
                    "messages_archive: no archive target for account {} (need Archive- or All-role folder)",
                    account.0
                ),
            )
        })?;

        let mut moved = Vec::with_capacity(account_ids.len());
        for id in account_ids {
            // Skip messages already in the archive target.
            let headers = match messages_repo::get(&*db, &id).await {
                Ok(h) => h,
                Err(_) => continue,
            };
            if headers.folder_id == target {
                continue;
            }
            if let Err(e) = messages_repo::set_folder(&*db, &id, &target).await {
                tracing::warn!(id = %id.0, "messages_archive: local update failed: {e}");
                continue;
            }
            moved.push(id);
        }

        if moved.is_empty() {
            continue;
        }

        let payload = serde_json::json!({
            "ids": moved,
            "target": target,
        });
        outbox_repo::enqueue(&*db, &account, "move_messages", &payload.to_string()).await?;
    }
    Ok(())
}

/// Pick the best archive target for an account given its folder list.
/// Prefers a folder with `FolderRole::Archive`; falls back to
/// `FolderRole::All`. Returns `None` if neither is present, in which
/// case the caller should surface an error rather than guess.
fn resolve_archive_target(folders: &[qsl_core::Folder]) -> Option<FolderId> {
    folders
        .iter()
        .find(|f| matches!(f.role, Some(FolderRole::Archive)))
        .or_else(|| {
            folders
                .iter()
                .find(|f| matches!(f.role, Some(FolderRole::All)))
        })
        .map(|f| f.id.clone())
}

#[derive(Debug, Deserialize)]
pub struct MessagesDeleteInput {
    pub ids: Vec<MessageId>,
}

/// `messages_delete` — remove messages locally and queue the
/// server-side delete (IMAP `+FLAGS (\Deleted) + UID EXPUNGE` /
/// JMAP `Email/destroy`). Note: Gmail interprets `\Deleted +
/// EXPUNGE` as "move to Trash" while Fastmail's `Email/destroy`
/// is permanent. The trait surface is the same; the visible
/// difference shows up in whether the message is recoverable.
#[tauri::command]
pub async fn messages_delete(
    state: State<'_, AppState>,
    input: MessagesDeleteInput,
) -> IpcResult<()> {
    let MessagesDeleteInput { ids } = input;
    if ids.is_empty() {
        return Ok(());
    }

    let db = state.db.lock().await;
    let mut by_account: std::collections::HashMap<qsl_core::AccountId, Vec<MessageId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let headers = match messages_repo::get(&*db, id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(id = %id.0, "messages_delete: skipping unknown id: {e}");
                continue;
            }
        };
        if let Err(e) = messages_repo::delete(&*db, id).await {
            tracing::warn!(id = %id.0, "messages_delete: local delete failed: {e}");
            continue;
        }
        by_account
            .entry(headers.account_id)
            .or_default()
            .push(id.clone());
    }

    for (account, ids) in by_account {
        let payload = serde_json::json!({ "ids": ids });
        outbox_repo::enqueue(&*db, &account, "delete_messages", &payload.to_string()).await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesSendInput {
    pub draft_id: DraftId,
}

/// `messages_send` — convert a saved draft into an outbox row and
/// drop it on the wire.
///
/// Reads the draft from storage, looks up the account's email
/// address for the From envelope, builds the RFC 5322 byte stream
/// via `qsl_mime::compose::build_rfc5322`, base64-encodes the
/// bytes into the outbox payload (so the JSON row stays a single
/// SQLite text column), and enqueues an `OP_SUBMIT_MESSAGE` row.
/// The drain worker picks it up and routes through the account's
/// `MailBackend::submit_message`.
///
/// The draft row is deleted on enqueue. Once the bytes are in the
/// outbox the draft is no longer the source of truth — the queue
/// row is. If submission DLQs, the user sees a "failed to send"
/// banner; the bytes are recoverable from the outbox row's
/// payload.
#[tauri::command]
pub async fn messages_send(state: State<'_, AppState>, input: MessagesSendInput) -> IpcResult<()> {
    let MessagesSendInput { draft_id } = input;

    let db = state.db.lock().await;
    let draft = drafts_repo::get(&*db, &draft_id).await?;
    let account = accounts_repo::get(&*db, &draft.account_id).await?;

    let from = EmailAddress {
        address: account.email_address.clone(),
        display_name: Some(account.display_name.clone()),
    };

    let built = build_rfc5322(&draft, &from).map_err(|e| {
        qsl_ipc::IpcError::new(
            qsl_ipc::IpcErrorKind::Internal,
            format!("messages_send: build RFC 5322: {e}"),
        )
    })?;

    let payload = serde_json::json!({
        "message_id": built.message_id,
        "raw_b64": base64_engine.encode(&built.bytes),
    });

    outbox_repo::enqueue(
        &*db,
        &draft.account_id,
        qsl_sync::outbox_drain::OP_SUBMIT_MESSAGE,
        &payload.to_string(),
    )
    .await?;
    drafts_repo::delete(&*db, &draft_id).await?;
    drop(db);

    tracing::info!(
        draft = %draft_id.0,
        account = %draft.account_id.0,
        message_id = %built.message_id,
        "messages_send: enqueued submit_message"
    );

    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesLoadOlderInput {
    pub folder: FolderId,
    pub limit: u32,
}

/// `messages_load_older` — pager backfill for the message-list pane.
///
/// Asks the account's [`MailBackend`] for headers strictly older than
/// the lowest currently-synced message in `folder`, persists the
/// returned rows (running the standard threading pass on each insert),
/// and returns the count added.
///
/// IMAP backends compute the anchor from the lowest local UID and
/// run a UID FETCH inverted-range request; JMAP and other backends
/// without an override get the trait's default empty implementation
/// for now (Phase 2 follow-up wires up `Email/query` paging).
///
/// Returns `0` when the historical tail is exhausted, when the
/// folder is empty (so the bootstrap sync is the right tool, not
/// the pager), or when the backend can't paginate older.
#[tauri::command]
pub async fn messages_load_older(
    state: State<'_, AppState>,
    input: MessagesLoadOlderInput,
) -> IpcResult<u32> {
    let MessagesLoadOlderInput { folder, limit } = input;
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);

    // Resolve the account id from the folder row so we can open the
    // right backend. `list_by_folder` with a generous cap pulls every
    // header so we can derive the IMAP UID floor below.
    let (account_id, anchor) = {
        let db = state.db.lock().await;
        let folder_row = folders_repo::get(&*db, &folder).await?;
        let local = messages_repo::list_by_folder(&*db, &folder, MAX_PAGE_LIMIT, 0).await?;
        let anchor = lowest_imap_uid(&local);
        (folder_row.account_id, anchor)
    };

    let Some(anchor) = anchor else {
        tracing::debug!(folder = %folder.0, "messages_load_older: empty folder, nothing to page");
        return Ok(0);
    };

    let backend = backend_factory::get_or_open(&state, &account_id).await?;

    let older = backend.fetch_older_headers(&folder, anchor, limit).await?;

    if older.is_empty() {
        tracing::debug!(
            folder = %folder.0,
            anchor,
            "messages_load_older: backend returned no older messages"
        );
        return Ok(0);
    }

    let mut added = 0u32;
    let db = state.db.lock().await;
    for h in &older {
        match messages_repo::find(&*db, &h.id).await? {
            Some(_) => {
                // Already in DB — skip insert; the bootstrap or live
                // sync got there first. Don't count toward `added`.
                continue;
            }
            None => {
                messages_repo::insert(&*db, h, None).await?;
                if let Err(e) = qsl_sync::threading::attach_to_thread(&*db, h).await {
                    tracing::warn!(message = %h.id.0, "thread assembly failed: {e}");
                }
                added += 1;
            }
        }
    }
    drop(db);

    tracing::info!(
        folder = %folder.0,
        anchor,
        returned = older.len(),
        inserted = added,
        "messages_load_older"
    );
    Ok(added)
}

/// Walk a slice of locally-cached headers, decode each id with the
/// IMAP `MessageRef` codec, and return the smallest UID. JMAP-shaped
/// ids fail the decode and are silently skipped — for those backends
/// the pager isn't usable until `Email/query` paging lands.
///
/// Returns `None` when the slice is empty or no id parses as IMAP.
fn lowest_imap_uid(messages: &[MessageHeaders]) -> Option<u64> {
    messages
        .iter()
        .filter_map(|m| qsl_imap_client::MessageRef::decode(&m.id).ok())
        .map(|r| u64::from(r.uid))
        .min()
}

#[derive(Debug, Deserialize)]
pub struct MessagesRefreshFolderInput {
    pub folder: FolderId,
}

/// `messages_refresh_folder` — kick a one-shot sync of one folder.
///
/// Triggered by the UI when the user opens a folder. The reactive
/// `sync_engine` already pushes new mail via IMAP IDLE for the top
/// 10 folders per account (and a 2-min poll for the rest), but this
/// command bridges the gap: clicking a folder always pulls server
/// state immediately rather than waiting on the next watcher cycle.
///
/// Returns `()`; sync_one_folder emits its own `sync_event` so the
/// caller doesn't need to bump `sync_tick` itself — the UI's
/// reactive resources refetch automatically when the event lands.
#[tauri::command]
pub async fn messages_refresh_folder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: MessagesRefreshFolderInput,
) -> IpcResult<()> {
    let folder_id = input.folder;
    let folder = {
        let db = state.db.lock().await;
        folders_repo::get(&*db, &folder_id).await?
    };
    let blobs = BlobStore::new(state.data_dir.join("blobs"));
    let account_id = folder.account_id.clone();
    crate::sync_engine::sync_one_folder(&app, &blobs, &account_id, &folder).await;
    tracing::info!(folder = %folder_id.0, "messages_refresh_folder");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct MessagesOpenInWindowInput {
    pub id: MessageId,
}

/// `messages_open_in_window` — pop a new Tauri window that mounts the
/// reader-only Dioxus route for the supplied message id.
///
/// The popup's window label is `reader-<sanitized_id>`; the message
/// id is injected into the popup's JS context as
/// `window.__QSL_READER_ID__` via `initialization_script`. The
/// Dioxus root component reads that global at boot and mounts the
/// `ReaderOnlyApp` instead of the three-pane shell.
///
/// `reader_render` for the popup's label lazy-installs a fresh
/// Servo instance on first call (see `commands::reader::reader_render`).
/// `WindowEvent::CloseRequested` drops the renderer entry and the
/// `linux_gtk` parent registry entry; the underlying GTK widgets
/// stay leaked (a few KB each) by design — see
/// `docs/superpowers/plans/2026-04-27-reader-popup-window.md`.
///
/// Calling this for an already-open popup focuses the existing
/// window instead of spawning a duplicate.
#[tauri::command]
pub async fn messages_open_in_window<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    input: MessagesOpenInWindowInput,
) -> IpcResult<()> {
    use tauri::Manager;

    let t_start = std::time::Instant::now();

    // Tauri labels accept only `[a-zA-Z0-9_-]` per the docs. IMAP ids
    // contain `|` and `:`; map any other char to `_` for the label.
    let safe_id: String = input
        .id
        .0
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let label = format!("reader-{safe_id}");

    if let Some(existing) = app.get_webview_window(&label) {
        if let Err(e) = existing.set_focus() {
            tracing::warn!(window = %label, error = %e, "messages_open_in_window: set_focus failed");
        }
        return Ok(());
    }

    // Pre-fetch the message before opening the popup so the Dioxus
    // bundle inside the popup can mount with the body already in
    // hand. Without this, the popup boots, runs `use_resource` to
    // call `messages_get`, waits for the round-trip, and only then
    // composes HTML — adding ~hundreds of ms of perceived latency
    // on top of the wasm boot. We embed the JSON serialization of
    // `RenderedMessage` as `window.__QSL_READER_PRELOAD__` and the
    // popup's reader-only app uses it directly. A fetch failure is
    // logged but doesn't abort the open: the popup falls back to
    // calling `messages_get` itself when `__QSL_READER_PRELOAD__`
    // is `null`.
    let t_preload_start = std::time::Instant::now();
    let preload_json: String = match messages_get(
        state,
        MessagesGetInput {
            id: input.id.clone(),
            force_trusted: false,
        },
    )
    .await
    {
        Ok(rendered) => serde_json::to_string(&rendered).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "messages_open_in_window: serialize preload failed");
            "null".to_string()
        }),
        Err(e) => {
            tracing::warn!(error = %e, "messages_open_in_window: preload fetch failed");
            "null".to_string()
        }
    };
    tracing::info!(
        ms = t_preload_start.elapsed().as_millis() as u64,
        bytes = preload_json.len(),
        "messages_open_in_window: preload fetched"
    );

    // initialization_script runs once in the new webview before the
    // wasm bundle boots. Setting `__QSL_READER_ID__` and
    // `__QSL_READER_PRELOAD__` here lets the Dioxus root component
    // branch on the id and skip a follow-up IPC round-trip when the
    // preload is present.
    let init_script = format!(
        "window.__QSL_READER_ID__ = {};\nwindow.__QSL_READER_PRELOAD__ = {};",
        serde_json::to_string(&input.id.0).expect("serializing message id"),
        preload_json,
    );

    let title = format!("QSL — {}", input.id.0);
    let t_window_start = std::time::Instant::now();
    let window =
        tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(720.0, 800.0)
            .initialization_script(&init_script)
            // Child windows need an explicit opt-in or right-click →
            // Inspect / Ctrl+Shift+I are silently disabled even though
            // the host has Tauri's `devtools` feature enabled. Effective
            // only when the underlying runtime supports it (Tauri's
            // `devtools` feature is on for this binary in `Cargo.toml`).
            .devtools(true)
            .build()
            .map_err(|e| {
                qsl_ipc::IpcError::new(
                    qsl_ipc::IpcErrorKind::Internal,
                    format!("create reader window: {e}"),
                )
            })?;
    tracing::info!(
        ms = t_window_start.elapsed().as_millis() as u64,
        "messages_open_in_window: WebviewWindow built"
    );

    {
        let app_for_close = app.clone();
        let label_for_close = label.clone();
        window.on_window_event(move |event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let app = app_for_close.clone();
                let label = label_for_close.clone();
                tauri::async_runtime::spawn(async move {
                    let state: tauri::State<AppState> = app.state();
                    state.servo_renderers.lock().await.remove(&label);
                    #[cfg(target_os = "linux")]
                    crate::linux_gtk::remove_parent(&label);
                    tracing::info!(window = %label, "popup reader window closed; renderer dropped");
                });
            }
        });
    }

    // Servo install must happen on the GTK main thread —
    // `gtk::Overlay::new()` panics with "GTK may only be used from
    // the main thread" anywhere else. We're currently on a tokio
    // worker (every `#[tauri::command] async fn` is), so dispatch
    // the install via `run_on_main_thread` and await the result so
    // the renderer is in the per-window registry before this
    // command returns. The popup's first `reader_render` then
    // succeeds without racing the install.
    #[cfg(feature = "servo")]
    {
        let t_install_start = std::time::Instant::now();
        let install_app = app.clone();
        let install_label = label.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        app.run_on_main_thread(move || {
            let result = (|| -> Result<(), String> {
                let webview = install_app
                    .get_webview_window(&install_label)
                    .ok_or_else(|| "popup window vanished before install".to_string())?;
                crate::renderer_bridge::install_servo_renderer_for_window(&install_app, &webview)
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| {
            qsl_ipc::IpcError::new(
                qsl_ipc::IpcErrorKind::Internal,
                format!("dispatch popup Servo install: {e}"),
            )
        })?;
        match rx.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(window = %label, error = %e, "messages_open_in_window: Servo install failed");
            }
            Err(e) => {
                tracing::warn!(window = %label, error = %e, "messages_open_in_window: install reply lost");
            }
        }
        tracing::info!(
            ms = t_install_start.elapsed().as_millis() as u64,
            "messages_open_in_window: Servo install completed"
        );
    }

    tracing::info!(
        window = %label,
        id = %input.id.0,
        total_ms = t_start.elapsed().as_millis() as u64,
        "messages_open_in_window"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qsl_core::{AccountId, Folder};

    fn folder(id: &str, role: Option<FolderRole>) -> Folder {
        Folder {
            id: FolderId(id.into()),
            account_id: AccountId("acct".into()),
            name: id.rsplit('/').next().unwrap_or(id).into(),
            path: id.into(),
            role,
            unread_count: 0,
            total_count: 0,
            parent: None,
        }
    }

    #[test]
    fn resolve_archive_target_prefers_archive_role() {
        let folders = vec![
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("Archive", Some(FolderRole::Archive)),
            folder("[Gmail]/All Mail", Some(FolderRole::All)),
        ];
        assert_eq!(
            resolve_archive_target(&folders),
            Some(FolderId("Archive".into())),
        );
    }

    #[test]
    fn resolve_archive_target_falls_back_to_all() {
        let folders = vec![
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("[Gmail]/All Mail", Some(FolderRole::All)),
            folder("[Gmail]/Sent Mail", Some(FolderRole::Sent)),
        ];
        assert_eq!(
            resolve_archive_target(&folders),
            Some(FolderId("[Gmail]/All Mail".into())),
        );
    }

    #[test]
    fn resolve_archive_target_returns_none_without_archive_or_all() {
        let folders = vec![
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("Trash", Some(FolderRole::Trash)),
        ];
        assert_eq!(resolve_archive_target(&folders), None);
    }
}
