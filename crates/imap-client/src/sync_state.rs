// SPDX-License-Identifier: Apache-2.0

//! IMAP sync cursor serialization.
//!
//! `SyncState.backend_state` is an opaque string from the core's point
//! of view. This module is the one place that interprets that string
//! for IMAP: it serializes the RFC 7162 tuple
//! `(uidvalidity, highestmodseq, uidnext)` to JSON and parses it back.

use capytain_core::{MailError, SyncState};
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

#[cfg(test)]
mod tests {
    use super::*;
    use capytain_core::FolderId;

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
}
