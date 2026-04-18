// SPDX-License-Identifier: Apache-2.0

//! Sync cursor shared between the core and each backend adapter.

use serde::{Deserialize, Serialize};

use crate::ids::FolderId;

/// Opaque per-folder sync cursor.
///
/// The core persists the `backend_state` string verbatim and hands it back to
/// the adapter on the next delta fetch. Interpretation is entirely the
/// adapter's concern:
///
/// - **IMAP** serializes `(uidvalidity, highestmodseq, uidnext)` into the
///   string (CONDSTORE + QRESYNC territory).
/// - **JMAP** uses the server-issued state token as-is; on the next request
///   it feeds the same token back through `Email/changes`.
///
/// The core never parses, compares, or mutates the inner string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// The folder this cursor refers to.
    pub folder_id: FolderId,

    /// Opaque backend payload. Treat as bytes-on-the-wire.
    pub backend_state: String,
}
