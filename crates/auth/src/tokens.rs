// SPDX-License-Identifier: Apache-2.0

//! Token value types.
//!
//! [`AccessToken`] and [`RefreshToken`] are opaque secret-bearing strings
//! that the crate takes care never to `Debug`-print or log at `info` level.
//! Both impl [`Drop`] via the `zeroize` crate so the inner allocation is
//! wiped before the `String` is freed — this protects against the secret
//! bytes lingering on the heap freelist after the value goes out of scope
//! (which would surface in core dumps or memory snapshots).
//!
//! Caveat: `AccessToken` propagates as `Arc<str>` into the IMAP backend
//! (`crates/imap-client/src/backend.rs::ImapBackend::access_token`) and
//! is `.clone()`d inside the JMAP client. Each `Arc<str>` keeps its own
//! allocation that is *not* zeroized on drop. The zeroize impl here
//! wipes the originally-allocated `String`; the residual `Arc<str>`
//! copies are out of scope for this layer. Closing that gap would
//! require migrating the whole token pipeline to `Zeroizing<String>`,
//! which isn't worth the API churn for the threat model (memory
//! snapshot, not remote attacker).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// An OAuth2 access token. Short-lived (minutes to an hour); passed to
/// IMAP/JMAP/SMTP on every request.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccessToken(pub String);

impl AccessToken {
    /// Expose the inner string. Callers are on the hook for not logging it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AccessToken").field(&"<redacted>").finish()
    }
}

impl Drop for AccessToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// An OAuth2 refresh token. Long-lived (weeks to forever); stored in the
/// OS keychain and rotated whenever the provider issues a new one.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RefreshToken(pub String);

impl RefreshToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefreshToken").field(&"<redacted>").finish()
    }
}

impl Drop for RefreshToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Full token envelope returned by the token endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenSet {
    pub access: AccessToken,
    pub refresh: Option<RefreshToken>,
    /// When `access` stops being valid. Computed client-side from the
    /// token endpoint's `expires_in` value. Some providers don't return
    /// `expires_in`; in that case callers may treat the token as valid
    /// until the next 401 and refresh.
    pub expires_at: Option<DateTime<Utc>>,
}

impl TokenSet {
    /// Returns true if we have an `expires_at` and it's in the past (or
    /// within the 30-second freshness window — tokens expiring that soon
    /// are treated as already stale so we don't race the wall clock).
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) => Utc::now() >= exp - chrono::Duration::seconds(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token_content() {
        let a = AccessToken("super-secret".into());
        let r = RefreshToken("also-secret".into());
        let dbg_a = format!("{a:?}");
        let dbg_r = format!("{r:?}");
        assert!(!dbg_a.contains("super-secret"));
        assert!(!dbg_r.contains("also-secret"));
        assert!(dbg_a.contains("redacted"));
        assert!(dbg_r.contains("redacted"));
    }

    #[test]
    fn is_expired_honors_30s_skew() {
        let past = Utc::now() - chrono::Duration::seconds(60);
        let t = TokenSet {
            access: AccessToken("x".into()),
            refresh: None,
            expires_at: Some(past),
        };
        assert!(t.is_expired());

        let future = Utc::now() + chrono::Duration::seconds(3600);
        let t2 = TokenSet {
            access: AccessToken("x".into()),
            refresh: None,
            expires_at: Some(future),
        };
        assert!(!t2.is_expired());
    }
}
