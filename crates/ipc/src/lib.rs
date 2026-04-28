// SPDX-License-Identifier: Apache-2.0

//! QSL IPC types.
//!
//! Serializable command inputs, outputs, and events exchanged between
//! the Tauri shell (`apps/desktop/src-tauri/`) and the Dioxus UI
//! (`apps/desktop/ui/`). See `COMMANDS.md` for the full command
//! catalogue.
//!
//! # Scope (Phase 0 Week 5)
//!
//! Part 1 lands the error shape plus proof-of-life types
//! (`OAuthProvider`, `AccountStatus`). Part 2 adds the read-path types
//! the UI needs to render the sidebar, message list, and reader pane:
//! `SortOrder`, `MessagePage`, `RenderedMessage`. The remainder of the
//! `COMMANDS.md` surface — compose, search, settings, threads, events
//! — arrives in Phase 1.

pub mod error;

pub use error::{IpcError, IpcErrorKind, IpcResult};

// Re-export the domain types the UI speaks in. These already derive
// serde in `qsl-core`, so they cross the IPC boundary as-is.
pub use qsl_core::{
    Account, AccountId, Attachment, AttachmentRef, BackendKind, Draft, DraftAttachment,
    DraftBodyKind, DraftId, EmailAddress, Folder, FolderId, FolderRole, MessageBody, MessageFlags,
    MessageHeaders, MessageId, SyncState, ThreadId,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which built-in OAuth2 provider an `accounts_add_oauth` call targets.
/// Custom OAuth2 / Microsoft 365 arrive in later phases (see
/// `COMMANDS.md` Accounts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    Gmail,
    Fastmail,
}

impl OAuthProvider {
    /// Slug matching `qsl_auth::lookup` and `mailcli auth add`.
    pub fn slug(&self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "gmail",
            OAuthProvider::Fastmail => "fastmail",
        }
    }
}

/// Per-account health snapshot for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatus {
    pub online: bool,
    pub last_sync: Option<DateTime<Utc>>,
    pub last_error: Option<IpcError>,
    pub is_syncing: bool,
}

/// How the UI asks `messages_list` to order its results. See
/// `COMMANDS.md §Messages`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Most recent first — the default mailbox view.
    #[default]
    DateDesc,
    /// Oldest first — archival / thread-sequence viewing.
    DateAsc,
    /// Unread messages before read ones, each group date-desc. Useful
    /// for a "triage unread" surface.
    UnreadFirst,
}

/// One page of a folder's message list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePage {
    /// The slice of headers for the current offset/limit.
    pub messages: Vec<MessageHeaders>,
    /// Total messages in the folder, independent of paging.
    pub total_count: u32,
    /// Total unread messages in the folder (i.e. `!seen`), independent
    /// of paging.
    pub unread_count: u32,
}

/// Events the sync engine emits to the UI as folders sync. The
/// Tauri shell forwards these via `Window::emit("sync_event", …)`;
/// the Dioxus UI listens and refetches the affected folder when its
/// id matches the user's current selection.
///
/// Phase 1 Week 10 introduces this type alongside the desktop's
/// startup-sync bootstrap. Live IDLE pushes (PR 7b) reuse the same
/// shape — only the trigger changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SyncEvent {
    /// A folder finished a sync cycle. Counts mirror `SyncReport`.
    /// `unread_count` is the post-sync `count_unread_by_folder`
    /// snapshot — the UI uses it to render sidebar badges without
    /// a follow-up `messages_list` round-trip.
    /// `live` is `false` during the engine's bootstrap pass and
    /// `true` for cycles triggered by a watcher event; the UI uses
    /// it to suppress new-mail notifications during initial load
    /// (otherwise opening the app fires hundreds of toasts).
    FolderSynced {
        account: AccountId,
        folder: FolderId,
        added: u32,
        updated: u32,
        flag_updates: u32,
        removed: u32,
        unread_count: u32,
        live: bool,
    },
    /// A folder's sync cycle failed. The error string is rendered as
    /// a UI banner; the engine retries on the next cycle.
    FolderError {
        account: AccountId,
        folder: FolderId,
        error: String,
    },
}

/// One row in the compose pane's autocomplete dropdown.
///
/// Wire shape mirrors `qsl_storage::repos::contacts::Contact` but
/// lives here so the wasm UI can deserialize without pulling the
/// storage crate. The desktop's `contacts_query` command maps
/// between the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub address: String,
    pub display_name: Option<String>,
    pub last_seen_at: i64,
    pub seen_count: i64,
}

/// What the UI gets back when a user opens a message.
///
/// Phase 0 Week 5 populates only `headers` and `body_text`. HTML
/// sanitization (via ammonia + filter lists) and HTML rendering (via
/// Servo) land in Phase 0 Week 6 and Phase 1; until then
/// `sanitized_html` is always `None` and `remote_content_blocked` is
/// always `false`. `sender_is_trusted` is likewise stubbed to `false`
/// until the contacts table ships.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedMessage {
    pub headers: MessageHeaders,
    pub sanitized_html: Option<String>,
    pub body_text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub sender_is_trusted: bool,
    pub remote_content_blocked: bool,
}
