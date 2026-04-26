// SPDX-License-Identifier: Apache-2.0

//! Local draft message — the canonical "in-flight outgoing mail"
//! shape shared by storage, the desktop's Tauri commands, and the
//! Dioxus compose pane.
//!
//! Phase 2 Week 17 ships only local persistence; upstream sync to
//! the server's Drafts mailbox arrives in Week 20 via a `save_draft`
//! outbox op. Submission (`Send`) lands Week 18 (SMTP) and Week 19
//! (JMAP).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, DraftId};
use crate::message::EmailAddress;

/// Plain-text vs markdown distinction for the body field. Phase 2
/// Week 17 only writes `Plain`; the `Markdown` variant is wired
/// here so the storage round-trip is forward-compatible with Week 20.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftBodyKind {
    #[default]
    Plain,
    Markdown,
}

impl DraftBodyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DraftBodyKind::Plain => "plain",
            DraftBodyKind::Markdown => "markdown",
        }
    }
}

/// One attachment on a draft. Phase 2 Week 21 wires the file picker
/// (`tauri-plugin-dialog`) to write rows of this shape; Week 17
/// only the empty-vec form is exercised. `path` is an absolute
/// filesystem path the desktop binary can read at submission time;
/// `inline` toggles whether the part is intended as `Content-
/// Disposition: inline` (embedded image) vs. `attachment` (standard
/// file attachment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub path: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub inline: bool,
}

/// Persistent draft. Round-trips through the `drafts` table introduced
/// in storage migration `0004`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: DraftId,
    pub account_id: AccountId,

    /// `Message-ID` of the message this draft is a reply to.
    /// Populated by Reply / Reply-All in Week 20; `None` for a
    /// fresh compose.
    pub in_reply_to: Option<String>,

    /// `References` chain. Empty for a fresh compose.
    pub references: Vec<String>,

    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,

    pub subject: String,
    pub body: String,
    pub body_kind: DraftBodyKind,

    /// Inline-or-spilled attachment list. Phase 2 Week 17 always
    /// stores an empty `Vec`; the file picker (Week 21) populates
    /// it via [`DraftAttachment`].
    pub attachments: Vec<DraftAttachment>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
