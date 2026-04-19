// SPDX-License-Identifier: Apache-2.0

//! Capytain IPC types.
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
// serde in `capytain-core`, so they cross the IPC boundary as-is.
pub use capytain_core::{
    Account, AccountId, Attachment, AttachmentRef, BackendKind, DraftId, EmailAddress, Folder,
    FolderId, FolderRole, MessageBody, MessageFlags, MessageHeaders, MessageId, SyncState,
    ThreadId,
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
    /// Slug matching `capytain_auth::lookup` and `mailcli auth add`.
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
