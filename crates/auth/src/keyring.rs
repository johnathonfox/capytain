// SPDX-License-Identifier: Apache-2.0

//! Refresh-token storage in the OS keychain.
//!
//! One entry per account, scoped to the keychain service
//! `com.capytain.app`. On macOS this shows up in Keychain Access; on
//! Windows in Credential Manager; on Linux in Secret Service (GNOME
//! Keyring / KWallet).
//!
//! Only refresh tokens live in the keychain. Access tokens stay in
//! process memory. Passwords are never persisted because they never
//! cross our code path in the first place.

use capytain_core::AccountId;
use keyring::Entry;
use tracing::debug;

use crate::error::AuthError;
use crate::tokens::RefreshToken;

/// Keychain service identifier used for every Capytain entry.
pub const KEYCHAIN_SERVICE: &str = "com.capytain.app";

/// Facade over the OS keychain for Capytain's refresh tokens.
#[derive(Debug)]
pub struct TokenVault {
    service: String,
}

impl TokenVault {
    /// Build a vault that uses the default `com.capytain.app` service.
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_string(),
        }
    }

    /// Build a vault with a custom service name. Tests use this to
    /// namespace parallel runs so they don't step on each other (and so
    /// a developer's real Capytain refresh tokens don't get clobbered
    /// by the test suite).
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    // All public methods are `async` and dispatch the keyring call
    // through `tokio::task::spawn_blocking`. The Linux
    // `async-secret-service` feature of the `keyring` crate wraps the
    // async `secret_service` crate in a **blocking** facade that
    // calls `block_on` internally — which panics if it's already
    // inside a tokio runtime (the mailcli `#[tokio::main]` entry,
    // the desktop bin's Tauri runtime). Moving the call to a
    // blocking thread pool sidesteps that cleanly and keeps the
    // public API honest about the I/O cost. `spawn_blocking` joins
    // via a `JoinError` which we flatten into `AuthError::Other`.

    /// Store (or overwrite) the refresh token for an account.
    pub async fn put(&self, account: &AccountId, token: &RefreshToken) -> Result<(), AuthError> {
        debug!(account = %account.0, "storing refresh token in keychain");
        let service = self.service.clone();
        let id = account.0.clone();
        let password = token.expose().to_owned();
        blocking(move || {
            Entry::new(&service, &id)?.set_password(&password)?;
            Ok(())
        })
        .await
    }

    /// Retrieve a previously-stored refresh token. Returns
    /// [`AuthError::Keyring`] if the entry doesn't exist.
    pub async fn get(&self, account: &AccountId) -> Result<RefreshToken, AuthError> {
        let service = self.service.clone();
        let id = account.0.clone();
        blocking(move || {
            let raw = Entry::new(&service, &id)?.get_password()?;
            Ok(RefreshToken(raw))
        })
        .await
    }

    /// Remove the stored refresh token for an account. Missing entries
    /// are treated as success.
    pub async fn delete(&self, account: &AccountId) -> Result<(), AuthError> {
        debug!(account = %account.0, "deleting refresh token from keychain");
        let service = self.service.clone();
        let id = account.0.clone();
        blocking(move || match Entry::new(&service, &id)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        })
        .await
    }

    /// Best-effort existence check. Returns `Ok(false)` when the entry
    /// is missing; other keyring errors bubble up.
    pub async fn contains(&self, account: &AccountId) -> Result<bool, AuthError> {
        let service = self.service.clone();
        let id = account.0.clone();
        blocking(move || match Entry::new(&service, &id)?.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.into()),
        })
        .await
    }

    /// Service name this vault writes under.
    pub fn service(&self) -> &str {
        &self.service
    }
}

impl Default for TokenVault {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a keyring-touching closure on a blocking tokio thread pool,
/// flattening the `JoinError` into `AuthError::Other`.
async fn blocking<T, F>(f: F) -> Result<T, AuthError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AuthError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AuthError::Other(format!("keyring join error: {e}")))?
}
