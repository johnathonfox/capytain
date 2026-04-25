// SPDX-License-Identifier: Apache-2.0

//! `messages_*` Tauri commands.
//!
//! Implements the Phase 0 Week 5 read-path surface of
//! `COMMANDS.md §Messages`: `messages_list` and `messages_get`. The
//! write-path commands (`messages_mark_read`, `messages_flag`,
//! `messages_move`, `messages_archive`, `messages_delete`,
//! `messages_download_attachment`) land in Phase 1 alongside the
//! outbox / optimistic-mutation engine.

use capytain_core::{FolderRole, MessageFlags, MessageHeaders};
use capytain_ipc::{FolderId, IpcResult, MessageId, MessagePage, RenderedMessage, SortOrder};
use capytain_mime::{
    parse_rfc822, sanitize_email_html, sanitize_email_html_trusted, MessageIdentity,
};
use capytain_storage::{
    repos::folders as folders_repo, repos::messages as messages_repo, repos::outbox as outbox_repo,
    repos::remote_content_opt_ins, BlobStore,
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
    let db = state.db.lock().await;
    let headers = messages_repo::get(&*db, &input.id).await?;
    let body_path = messages_repo::body_path(&*db, &input.id).await?;
    let sender_is_trusted = match headers.from.first() {
        Some(addr) if !addr.address.is_empty() => {
            remote_content_opt_ins::is_trusted(&*db, &headers.account_id, &addr.address).await?
        }
        _ => false,
    };
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
) -> (
    Option<String>,
    Option<String>,
    Vec<capytain_core::Attachment>,
) {
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
/// `capytain_sync::outbox_drain::FlagsPayload`.
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
    let mut by_account: std::collections::HashMap<capytain_core::AccountId, Vec<MessageId>> =
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
