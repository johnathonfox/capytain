// SPDX-License-Identifier: Apache-2.0

//! Capytain IPC types.
//!
//! Serializable command inputs, outputs, and events exchanged between
//! the Tauri shell (`apps/desktop/src-tauri/`) and the Dioxus UI
//! (`apps/desktop/ui/`). See `COMMANDS.md` for the full command
//! catalogue.
//!
//! # Scope (Phase 0 Week 5 part 1)
//!
//! This file lands the error shape plus the proof-of-life types needed
//! for the first wired commands (`accounts_list`, `accounts_add_oauth`,
//! `folders_list`). The remainder of the `COMMANDS.md` surface —
//! message listing, compose, search, settings, events — arrives in
//! Week 5 part 2.

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
