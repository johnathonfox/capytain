// SPDX-License-Identifier: Apache-2.0

//! Provider profiles — authorization URL, token URL, scopes, redirect
//! model — plus the [`OAuthProvider`] trait they implement.

use crate::error::AuthError;

/// Broad classification of a provider's backend protocol. Consumers in
/// `qsl-sync` decide which `MailBackend` implementation to spin up
/// based on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderKind {
    /// IMAP for reads, SMTP for submission, OAuth2 XOAUTH2 for both.
    ImapSmtp,
    /// JMAP end-to-end.
    Jmap,
}

/// Static description of how to authenticate against a provider.
///
/// Everything is `&'static str` — provider profiles are compile-time
/// constants in [`crate::providers`]. Fork maintainers override at the
/// build boundary via `QSL_{PROVIDER}_CLIENT_ID`.
#[derive(Debug, Clone, Copy)]
pub struct ProviderProfile {
    /// Display-friendly name (`"Gmail"`, `"Fastmail"`).
    pub name: &'static str,
    /// Stable shortname used on the command line (`"gmail"`, `"fastmail"`).
    pub slug: &'static str,
    /// OAuth2 client ID. Empty string means "not configured at build time".
    pub client_id: &'static str,
    /// OAuth2 client secret, if the provider's client is configured as a
    /// confidential client (Web application type on Google, some Fastmail
    /// registrations, Microsoft 365 ADAL, etc.). Empty string means "no
    /// secret, PKCE-only flow". Desktop-app-type Google clients don't
    /// issue one; Web-application-type clients require it on the token
    /// exchange even when PKCE is also in use.
    pub client_secret: &'static str,
    /// RFC 6749 authorization endpoint.
    pub authorization_url: &'static str,
    /// RFC 6749 token endpoint (used for both code exchange and refresh).
    pub token_url: &'static str,
    /// RFC 7009 revocation endpoint, or empty if the provider doesn't
    /// publish one we can call statically. Hit best-effort by
    /// `accounts_remove` when the user removes an account, so a
    /// stolen refresh token is invalidated server-side and not just
    /// dropped from the keychain. Fastmail's revocation endpoint is
    /// JMAP-session-discoverable rather than static; leaving the
    /// field empty there falls back to local-only cleanup.
    pub revocation_url: &'static str,
    /// Scopes to request. Joined with `' '` when building the authorization
    /// URL.
    pub scopes: &'static [&'static str],
    /// Underlying protocol family. Drives which adapter handles this
    /// account once it's been provisioned.
    pub kind: ProviderKind,
}

impl ProviderProfile {
    /// Return [`AuthError::ProviderNotConfigured`] if the client ID is
    /// empty — used by the flow before we open the browser.
    pub fn require_client_id(&self) -> Result<&'static str, AuthError> {
        if self.client_id.is_empty() {
            Err(AuthError::ProviderNotConfigured(format!(
                "{slug}: set QSL_{upper}_CLIENT_ID at build time",
                slug = self.slug,
                upper = self.slug.to_ascii_uppercase()
            )))
        } else {
            Ok(self.client_id)
        }
    }
}

/// The one behavior a provider is required to supply. Trait-shaped even
/// though we only have `const` profiles today, so later additions
/// (Microsoft 365 in Phase 5, custom OAuth2 in Phase 6) can carry their
/// own construction logic without reshaping the consumer side.
pub trait OAuthProvider: Send + Sync {
    fn profile(&self) -> &'static ProviderProfile;
}

/// Look up a built-in provider by slug. Returns `None` for unknown names.
pub fn lookup(slug: &str) -> Option<&'static dyn OAuthProvider> {
    match slug {
        "gmail" => Some(&crate::providers::gmail::GMAIL),
        "fastmail" => Some(&crate::providers::fastmail::FASTMAIL),
        _ => None,
    }
}

/// Every built-in provider, in display order. Used by `mailcli auth list`
/// and the onboarding flow.
pub fn builtin() -> &'static [&'static dyn OAuthProvider] {
    BUILTIN
}

static BUILTIN: &[&dyn OAuthProvider] = &[
    &crate::providers::gmail::GMAIL,
    &crate::providers::fastmail::FASTMAIL,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_returns_known_providers() {
        assert!(lookup("gmail").is_some());
        assert!(lookup("fastmail").is_some());
        assert!(lookup("yahoo").is_none());
    }

    #[test]
    fn builtin_covers_phase_0_providers() {
        let slugs: Vec<_> = builtin().iter().map(|p| p.profile().slug).collect();
        assert!(slugs.contains(&"gmail"));
        assert!(slugs.contains(&"fastmail"));
    }
}
