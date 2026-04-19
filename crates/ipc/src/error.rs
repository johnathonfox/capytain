// SPDX-License-Identifier: Apache-2.0

//! IPC error shape per `COMMANDS.md` §Error Shape.
//!
//! Every Tauri command returns `Result<T, IpcError>`. This type is a
//! display-safe wrapper over `MailError` / `StorageError` /
//! `AuthError` that never leaks credentials or backend-specific detail
//! the UI has no business seeing.

use capytain_core::{AccountId, MailError, StorageError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The categories the UI routes on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IpcErrorKind {
    Network,
    /// UI should prompt for re-auth (refresh token expired / revoked).
    Auth,
    NotFound,
    Permission,
    Protocol,
    Storage,
    Cancelled,
    Internal,
}

/// Error the UI sees. Clone-able, serde-friendly, no secrets.
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
#[error("{kind:?}: {message}")]
pub struct IpcError {
    pub kind: IpcErrorKind,
    pub message: String,
    /// Which account failed, if the UI should route the error to one.
    pub account_id: Option<AccountId>,
}

/// Convenience alias for tauri::command returns.
pub type IpcResult<T> = Result<T, IpcError>;

impl IpcError {
    pub fn new(kind: IpcErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            account_id: None,
        }
    }

    pub fn for_account(mut self, account: AccountId) -> Self {
        self.account_id = Some(account);
        self
    }
}

impl From<MailError> for IpcError {
    fn from(e: MailError) -> Self {
        let kind = match &e {
            MailError::Network(_) => IpcErrorKind::Network,
            MailError::Auth(_) => IpcErrorKind::Auth,
            MailError::Protocol(_) => IpcErrorKind::Protocol,
            MailError::NotFound(_) => IpcErrorKind::NotFound,
            MailError::ServerRejected(_) => IpcErrorKind::Permission,
            MailError::Storage(_) => IpcErrorKind::Storage,
            MailError::Parse(_) => IpcErrorKind::Protocol,
            MailError::Cancelled => IpcErrorKind::Cancelled,
            MailError::Other(_) => IpcErrorKind::Internal,
            _ => IpcErrorKind::Internal,
        };
        IpcError::new(kind, e.to_string())
    }
}

impl From<StorageError> for IpcError {
    fn from(e: StorageError) -> Self {
        let kind = match &e {
            StorageError::NotFound => IpcErrorKind::NotFound,
            _ => IpcErrorKind::Storage,
        };
        IpcError::new(kind, e.to_string())
    }
}

// `From<capytain_auth::AuthError>` deliberately lives in `capytain-auth`
// instead of here: the UI crate compiles to wasm32 and can't pull in
// tokio / mio / keyring transitively through `capytain-auth`. Desktop
// command handlers that surface auth errors use the impl on the
// `capytain-auth` side to convert.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_error_categories_route() {
        let ipc: IpcError = MailError::Network("conn refused".into()).into();
        assert_eq!(ipc.kind, IpcErrorKind::Network);
        assert!(ipc.message.contains("conn refused"));

        let ipc: IpcError = MailError::Auth("token expired".into()).into();
        assert_eq!(ipc.kind, IpcErrorKind::Auth);
    }

    #[test]
    fn storage_not_found_routes_to_not_found() {
        let ipc: IpcError = StorageError::NotFound.into();
        assert_eq!(ipc.kind, IpcErrorKind::NotFound);
    }

    #[test]
    fn for_account_threads_account_id() {
        let ipc =
            IpcError::new(IpcErrorKind::Auth, "nope").for_account(AccountId("gmail:me@x".into()));
        assert_eq!(
            ipc.account_id.as_ref().map(|a| a.0.as_str()),
            Some("gmail:me@x")
        );
    }

    #[test]
    fn serde_round_trip() {
        let ipc = IpcError::new(IpcErrorKind::Protocol, "QRESYNC required");
        let json = serde_json::to_string(&ipc).unwrap();
        let back: IpcError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, ipc.kind);
        assert_eq!(back.message, ipc.message);
    }
}
