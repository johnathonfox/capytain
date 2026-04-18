// SPDX-License-Identifier: Apache-2.0

//! Error enums for the mail and storage layers.
//!
//! Libraries return [`Result<T, _>`] using these enums; binaries (Tauri
//! commands, `mailcli`) wrap them with [`anyhow::Error`] at the edges.
//!
//! Backend implementations (IMAP, JMAP) translate their internal errors into
//! [`MailError`] variants before returning — no `async_imap::Error` or
//! `jmap_client::Error` ever crosses the `MailBackend` trait boundary.

use thiserror::Error;

/// Errors produced by any mail-backend operation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MailError {
    /// Socket-level or TLS failure talking to the server.
    #[error("network error: {0}")]
    Network(String),

    /// OAuth2 token refresh failed, credentials were revoked, or the server
    /// rejected authentication for some other reason.
    #[error("authentication failed or token expired: {0}")]
    Auth(String),

    /// The remote server returned a malformed or unexpected response, or is
    /// missing a required capability (e.g. CONDSTORE, QRESYNC, IDLE).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The requested message or folder does not exist.
    #[error("message or folder not found: {0}")]
    NotFound(String),

    /// The server accepted the request but refused to carry it out (quota,
    /// permission, policy).
    #[error("server rejected operation: {0}")]
    ServerRejected(String),

    /// The local store returned an error while the backend was serving a
    /// request.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// A MIME parse or serialization failure.
    #[error("parse error: {0}")]
    Parse(String),

    /// The operation was cancelled (e.g. via a future being dropped).
    #[error("operation cancelled")]
    Cancelled,

    /// A bucket for errors that don't fit the other variants. Used sparingly;
    /// prefer adding a specific variant when a new failure mode recurs.
    #[error("{0}")]
    Other(String),
}

/// Errors produced by the storage layer (database, blob store, migrations).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The underlying database returned an error.
    #[error("database error: {0}")]
    Db(String),

    /// A migration step failed or the schema version is inconsistent.
    #[error("migration error: {0}")]
    Migration(String),

    /// The query completed but returned no row where one was expected.
    #[error("row not found")]
    NotFound,

    /// A `UNIQUE` constraint was violated.
    #[error("unique constraint violated: {0}")]
    Conflict(String),

    /// Serializing or deserializing a stored value failed.
    #[error("serialization error: {0}")]
    Serde(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_error_converts_into_mail_error() {
        let err: MailError = StorageError::NotFound.into();
        assert!(matches!(err, MailError::Storage(StorageError::NotFound)));
    }

    #[test]
    fn display_has_useful_text() {
        let err = MailError::Protocol("QRESYNC required".into());
        assert!(err.to_string().contains("QRESYNC"));
    }
}
