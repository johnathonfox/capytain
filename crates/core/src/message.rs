// SPDX-License-Identifier: Apache-2.0

//! Message — headers, body, flags, attachments, and addresses.
//!
//! The backend adapters populate these types from IMAP FETCH responses or
//! JMAP `Email/get` results. Downstream callers (sync engine, UI) consume
//! them without caring which protocol produced them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, AttachmentRef, FolderId, MessageId, ThreadId};

/// A parsed email address with optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    /// Addr-spec portion, e.g. `jane@example.com`.
    pub address: String,

    /// Display name, e.g. `Jane Doe` in `"Jane Doe" <jane@example.com>`.
    pub display_name: Option<String>,
}

/// IMAP-style message flags, generalized so JMAP keywords round-trip through
/// them too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFlags {
    /// IMAP `\Seen`, JMAP keyword `$seen`.
    pub seen: bool,

    /// IMAP `\Flagged`, JMAP keyword `$flagged`.
    pub flagged: bool,

    /// IMAP `\Answered`, JMAP keyword `$answered`.
    pub answered: bool,

    /// IMAP `\Draft`, JMAP keyword `$draft`.
    pub draft: bool,

    /// JMAP keyword `$forwarded`. IMAP has no standard flag for this; emu-
    /// lated via `$Forwarded` on servers that support custom keywords.
    pub forwarded: bool,
}

/// Metadata for a single message, without the body.
///
/// `MessageHeaders` is what populates list views. It's cheap to send over
/// IPC and cheap to persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHeaders {
    /// Backend-assigned identifier.
    pub id: MessageId,

    /// Owning account.
    pub account_id: AccountId,

    /// Folder containing this message. A message moved to another folder
    /// may surface a fresh `MessageId` and a fresh row.
    pub folder_id: FolderId,

    /// Thread id if one was assigned (Gmail, JMAP) or synthesized.
    pub thread_id: Option<ThreadId>,

    /// RFC 5322 `Message-ID` header, e.g. `<foo@host>`. Used for threading
    /// across backends that lack server-side threading.
    pub rfc822_message_id: Option<String>,

    /// `Subject`, as decoded from MIME-encoded words.
    pub subject: String,

    /// `From` addresses (usually one).
    pub from: Vec<EmailAddress>,

    /// `Reply-To` addresses if different from `From`.
    pub reply_to: Vec<EmailAddress>,

    /// `To` addresses.
    pub to: Vec<EmailAddress>,

    /// `Cc` addresses.
    pub cc: Vec<EmailAddress>,

    /// `Bcc` addresses. Usually empty on received mail.
    pub bcc: Vec<EmailAddress>,

    /// `Date` header, parsed.
    pub date: DateTime<Utc>,

    /// Current flags for this message.
    pub flags: MessageFlags,

    /// Server-side labels. Gmail uses these liberally; IMAP servers without
    /// labels produce an empty vec.
    pub labels: Vec<String>,

    /// Short preview text for the list view.
    pub snippet: String,

    /// Size on the wire (bytes) as reported by the server.
    pub size: u32,

    /// Whether the message carries any attachments.
    pub has_attachments: bool,
}

/// A message with its body and attachment descriptors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    /// Headers for this message.
    pub headers: MessageHeaders,

    /// Raw HTML body, unsanitized. Callers must pass this through the
    /// ammonia sanitizer and filter-list gate before rendering.
    pub body_html: Option<String>,

    /// Plain-text alternative, if the server provided one.
    pub body_text: Option<String>,

    /// Attachment descriptors (not the bytes).
    pub attachments: Vec<Attachment>,

    /// `In-Reply-To` header value, if any.
    pub in_reply_to: Option<String>,

    /// `References` header, parsed into its component `Message-ID`s.
    pub references: Vec<String>,
}

/// Attachment metadata. Bytes are fetched on demand via
/// `MailBackend::fetch_attachment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Backend-assigned identifier, unique within the parent message.
    pub id: AttachmentRef,

    /// Suggested filename.
    pub filename: String,

    /// MIME type as reported by the server (e.g. `application/pdf`).
    pub mime_type: String,

    /// Size in bytes.
    pub size: u64,

    /// True if the part is marked `inline` (typically an embedded image).
    pub inline: bool,

    /// `Content-ID` header for inline parts referenced via `cid:…` URLs.
    pub content_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_flags_default_is_all_false() {
        let f = MessageFlags::default();
        assert!(!f.seen);
        assert!(!f.flagged);
        assert!(!f.answered);
        assert!(!f.draft);
        assert!(!f.forwarded);
    }

    #[test]
    fn email_address_roundtrips_optional_display_name() {
        let with_name = EmailAddress {
            address: "jane@example.com".into(),
            display_name: Some("Jane Doe".into()),
        };
        let json = serde_json::to_string(&with_name).unwrap();
        let back: EmailAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(back.address, "jane@example.com");
        assert_eq!(back.display_name.as_deref(), Some("Jane Doe"));
    }
}
