// SPDX-License-Identifier: Apache-2.0

//! IMAP sync cursor serialization.
//!
//! `SyncState.backend_state` is an opaque string from the core's point
//! of view. This module is the one place that interprets that string
//! for IMAP: it serializes the RFC 7162 tuple
//! `(uidvalidity, highestmodseq, uidnext)` to JSON and parses it back.

use qsl_core::{MailError, MessageId, SyncState};
use serde::{Deserialize, Serialize};

/// Serializable IMAP-specific backend state.
///
/// `uidvalidity` changes whenever the server renumbers UIDs — if it
/// differs from the last seen value the client must discard its cache
/// for that folder and do a full refetch.
///
/// `highestmodseq` (CONDSTORE) lets us fetch only messages whose flags
/// or metadata changed since the last sync.
///
/// `uidnext` bounds newly-arrived messages: anything with UID >= the
/// previous `uidnext` is new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendState {
    pub uidvalidity: u32,
    pub highestmodseq: u64,
    pub uidnext: u32,
}

impl BackendState {
    /// Serialize to the opaque string the core persists verbatim.
    pub fn encode(&self) -> String {
        // `to_string` is infallible for this fully-owned struct.
        serde_json::to_string(self).expect("BackendState serialization is infallible")
    }

    /// Decode a previously-persisted sync state. Returns a
    /// [`MailError::Protocol`] on malformed input — which should only
    /// happen if someone hand-edited the database.
    pub fn decode(raw: &str) -> Result<Self, MailError> {
        serde_json::from_str(raw)
            .map_err(|e| MailError::Protocol(format!("corrupt IMAP sync cursor: {e}")))
    }

    /// Convenience: pull the state out of a [`SyncState`] wrapper.
    pub fn from_sync(state: &SyncState) -> Result<Self, MailError> {
        Self::decode(&state.backend_state)
    }
}

/// Identity of an IMAP message, packed into QSL's opaque
/// [`MessageId`] wrapper.
///
/// `MessageId`s are strings to the core. We use a `|`-delimited shape so
/// the encoder/decoder is trivially inspectable in logs:
///
/// ```text
/// imap|<uidvalidity>|<uid>|<folder_path>
/// ```
///
/// Splitting with `splitn(4, '|')` means folder paths that contain `|`
/// round-trip correctly (they live in the final segment). Paths in the
/// wild use `/` or `.` as separators; `|` is rare but allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    pub uidvalidity: u32,
    pub uid: u32,
    pub folder: String,
}

impl MessageRef {
    pub fn encode(&self) -> MessageId {
        MessageId(format!(
            "imap|{}|{}|{}",
            self.uidvalidity, self.uid, self.folder
        ))
    }

    pub fn decode(id: &MessageId) -> Result<Self, MailError> {
        let s = &id.0;
        let mut parts = s.splitn(4, '|');
        let scheme = parts.next();
        let uidvalidity = parts.next();
        let uid = parts.next();
        let folder = parts.next();
        match (scheme, uidvalidity, uid, folder) {
            (Some("imap"), Some(uv), Some(u), Some(f)) => Ok(MessageRef {
                uidvalidity: uv
                    .parse()
                    .map_err(|e| MailError::Protocol(format!("bad uidvalidity in {s:?}: {e}")))?,
                uid: u
                    .parse()
                    .map_err(|e| MailError::Protocol(format!("bad uid in {s:?}: {e}")))?,
                folder: f.to_string(),
            }),
            _ => Err(MailError::Protocol(format!(
                "not an IMAP message id: {s:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qsl_core::FolderId;

    #[test]
    fn round_trip() {
        let s = BackendState {
            uidvalidity: 1_712_345,
            highestmodseq: 987_654_321,
            uidnext: 4242,
        };
        let encoded = s.encode();
        let back = BackendState::decode(&encoded).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn from_sync_state_wrapper() {
        let inner = BackendState {
            uidvalidity: 1,
            highestmodseq: 2,
            uidnext: 3,
        };
        let wrapped = SyncState {
            folder_id: FolderId("INBOX".into()),
            backend_state: inner.encode(),
        };
        assert_eq!(BackendState::from_sync(&wrapped).unwrap(), inner);
    }

    #[test]
    fn rejects_garbage() {
        let err = BackendState::decode("not json").unwrap_err();
        assert!(err.to_string().contains("corrupt IMAP sync cursor"));
    }

    #[test]
    fn message_ref_round_trip() {
        let r = MessageRef {
            uidvalidity: 1_712_345,
            uid: 42,
            folder: "[Gmail]/Sent Mail".into(),
        };
        let back = MessageRef::decode(&r.encode()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn message_ref_handles_folder_with_pipe() {
        // splitn(4) keeps everything after the third `|` in the last
        // segment, so a folder with `|` round-trips correctly.
        let r = MessageRef {
            uidvalidity: 1,
            uid: 2,
            folder: "odd|name".into(),
        };
        let back = MessageRef::decode(&r.encode()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn message_ref_rejects_non_imap() {
        let err = MessageRef::decode(&qsl_core::MessageId("M0000001".into())).unwrap_err();
        assert!(err.to_string().contains("not an IMAP message id"));
    }
}
