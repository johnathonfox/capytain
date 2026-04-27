// SPDX-License-Identifier: Apache-2.0

//! Errors produced by the auth flow and token storage.

use thiserror::Error;

/// Error enum for OAuth2 flows, keyring I/O, and token exchange.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The provider isn't registered (unknown name) or its client ID is
    /// not configured at build time.
    #[error("provider not configured: {0}")]
    ProviderNotConfigured(String),

    /// Failed to bind the loopback HTTP listener on 127.0.0.1:0.
    #[error("could not start loopback listener: {0}")]
    Loopback(String),

    /// Could not open the system browser for the authorization URL.
    #[error("could not open browser: {0}")]
    Browser(String),

    /// The authorization response from the provider was malformed or the
    /// `state` parameter didn't match what we sent.
    #[error("authorization response invalid: {0}")]
    AuthResponse(String),

    /// The token endpoint rejected our code or refresh token.
    #[error("token exchange failed: {0}")]
    TokenExchange(String),

    /// The OS keychain refused a read, write, or delete.
    #[error("keyring error: {0}")]
    Keyring(String),

    /// The user cancelled the flow (closed the browser tab before
    /// redirecting back).
    #[error("authentication cancelled by user")]
    Cancelled,

    /// Anything else that falls outside the above.
    #[error("{0}")]
    Other(String),
}

impl From<keyring::Error> for AuthError {
    fn from(e: keyring::Error) -> Self {
        AuthError::Keyring(e.to_string())
    }
}

impl From<std::io::Error> for AuthError {
    fn from(e: std::io::Error) -> Self {
        AuthError::Loopback(e.to_string())
    }
}

impl From<url::ParseError> for AuthError {
    fn from(e: url::ParseError) -> Self {
        AuthError::AuthResponse(e.to_string())
    }
}

/// Route `AuthError` variants to their closest `IpcErrorKind`. The UI
/// primarily cares about `Auth` (prompt re-auth) and `Cancelled` (user
/// aborted); everything else collapses to `Internal` or `Network`.
///
/// Lives here rather than in `qsl-ipc` because `qsl-ipc`
/// compiles to wasm32 for the Dioxus UI and can't pull in this crate's
/// tokio/keyring/reqwest dependencies.
impl From<AuthError> for qsl_ipc::IpcError {
    fn from(e: AuthError) -> Self {
        use qsl_ipc::IpcErrorKind as K;
        use AuthError as A;
        let kind = match &e {
            A::ProviderNotConfigured(_) => K::Internal,
            A::Loopback(_) | A::Browser(_) => K::Network,
            A::AuthResponse(_) | A::TokenExchange(_) => K::Auth,
            A::Keyring(_) => K::Storage,
            A::Cancelled => K::Cancelled,
            _ => K::Internal,
        };
        qsl_ipc::IpcError::new(kind, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_surfaces_inner_detail() {
        let err = AuthError::TokenExchange("invalid_grant".into());
        assert!(err.to_string().contains("invalid_grant"));
    }
}
