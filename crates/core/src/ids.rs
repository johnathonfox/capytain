// SPDX-License-Identifier: Apache-2.0

//! Opaque identifier newtypes used throughout the core.
//!
//! Each type wraps a `String` chosen by the backend that minted it. The core
//! never parses or interprets the inner string — for IMAP, a [`MessageId`]
//! encodes `<folder_uid_validity>:<uid>`; for JMAP it's the opaque email id
//! issued by the server. Callers should treat these values as bytes-on-the-
//! wire and compare them only for equality.

use serde::{Deserialize, Serialize};

/// Identifies an `Account` inside QSL's local storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

/// Identifies a folder or mailbox inside an account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FolderId(pub String);

/// Identifies a single message. Backend-specific encoding; see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub String);

/// Identifies a thread (conversation). Either the backend's native thread id
/// (Gmail `X-GM-THRID`, JMAP thread id) or a synthetic id minted by
/// `crates/sync` when the backend doesn't expose one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(pub String);

/// Identifies a draft under construction. Distinct from [`MessageId`] because
/// drafts may not yet have been persisted server-side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DraftId(pub String);

/// Identifies an attachment within its parent message. Not unique across
/// messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentRef(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_serializes_transparently() {
        let id = AccountId("gmail:foo@example.com".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"gmail:foo@example.com\"");
        let round: AccountId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, round);
    }

    #[test]
    fn distinct_types_with_identical_strings_are_not_interchangeable() {
        let m = MessageId("abc".into());
        let f = FolderId("abc".into());
        // This is a compile-time guarantee — the assertion is only here to
        // document intent.
        assert_eq!(m.0, f.0);
    }
}
