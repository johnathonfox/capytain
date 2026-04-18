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

    fn entry(&self, account: &AccountId) -> Result<Entry, AuthError> {
        Ok(Entry::new(&self.service, &account.0)?)
    }

    /// Store (or overwrite) the refresh token for an account.
    pub fn put(&self, account: &AccountId, token: &RefreshToken) -> Result<(), AuthError> {
        debug!(account = %account.0, "storing refresh token in keychain");
        self.entry(account)?.set_password(token.expose())?;
        Ok(())
    }

    /// Retrieve a previously-stored refresh token. Returns
    /// [`AuthError::Keyring`] if the entry doesn't exist.
    pub fn get(&self, account: &AccountId) -> Result<RefreshToken, AuthError> {
        let raw = self.entry(account)?.get_password()?;
        Ok(RefreshToken(raw))
    }

    /// Remove the stored refresh token for an account. Missing entries
    /// are treated as success.
    pub fn delete(&self, account: &AccountId) -> Result<(), AuthError> {
        debug!(account = %account.0, "deleting refresh token from keychain");
        match self.entry(account)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Best-effort existence check. Returns `Ok(false)` when the entry
    /// is missing; other keyring errors bubble up.
    pub fn contains(&self, account: &AccountId) -> Result<bool, AuthError> {
        match self.entry(account)?.get_password() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.into()),
        }
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
