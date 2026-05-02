// SPDX-License-Identifier: Apache-2.0

//! Folder — a container of messages within an account.
//!
//! IMAP calls these "mailboxes"; JMAP calls them "mailboxes" too. Gmail
//! surfaces labels as pseudo-folders. QSL normalizes all three under
//! the `Folder` name.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, FolderId};

/// A folder or mailbox in a given account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Backend-assigned identifier.
    pub id: FolderId,

    /// Owning account.
    pub account_id: AccountId,

    /// Display name (leaf name, no path separators).
    pub name: String,

    /// Full path as understood by the server (e.g. `[Gmail]/All Mail`).
    pub path: String,

    /// Well-known role if the server tags one, via IMAP SPECIAL-USE
    /// (RFC 6154) or JMAP `role`.
    pub role: Option<FolderRole>,

    /// Unread message count reported by the server or computed locally.
    pub unread_count: u32,

    /// Total message count.
    pub total_count: u32,

    /// Parent folder in the hierarchy, if any.
    pub parent: Option<FolderId>,
}

/// Standardized roles for well-known mailboxes, per IMAP SPECIAL-USE (RFC
/// 6154) and JMAP role attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FolderRole {
    /// Incoming mail.
    Inbox,

    /// Outgoing mail that has been submitted.
    Sent,

    /// Work-in-progress messages.
    Drafts,

    /// Soft-deleted messages (still recoverable).
    Trash,

    /// Junk / spam bucket.
    Spam,

    /// Long-term archive.
    Archive,

    /// Gmail's "Important" marker.
    Important,

    /// Gmail's "All Mail" view.
    All,

    /// Server-side flagged / starred view.
    Flagged,
}

impl FolderRole {
    /// Canonical sentence-case display name for the role, suitable for
    /// the sidebar / message-list header. Used as a fallback when the
    /// server-provided folder name looks unfriendly (e.g. all-caps
    /// `DRAFTS` from a self-hosted IMAP server, or the IMAP-mandated
    /// `INBOX`). Gmail and Fastmail return already-presentable names
    /// like `Sent Mail` and `All Mail`, so the UI helpers prefer the
    /// server name when it looks human-friendly and only fall back
    /// here when it doesn't.
    pub fn canonical_display_name(&self) -> &'static str {
        match self {
            FolderRole::Inbox => "Inbox",
            FolderRole::Sent => "Sent",
            FolderRole::Drafts => "Drafts",
            FolderRole::Trash => "Trash",
            FolderRole::Spam => "Spam",
            FolderRole::Archive => "Archive",
            FolderRole::Important => "Important",
            FolderRole::All => "All Mail",
            FolderRole::Flagged => "Starred",
        }
    }
}
