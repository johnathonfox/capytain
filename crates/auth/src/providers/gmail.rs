// SPDX-License-Identifier: Apache-2.0

//! Gmail / Google Workspace OAuth2 profile.
//!
//! Scopes: full read/write/send via `https://mail.google.com/`. That's
//! the umbrella Gmail scope for IMAP + SMTP with XOAUTH2 — narrower
//! `gmail.readonly` / `gmail.send` exist but don't cover our full range
//! of sync operations.
//!
//! Endpoints come from Google's OpenID Connect Discovery document
//! <https://accounts.google.com/.well-known/openid-configuration>; we
//! hardcode them rather than fetch at runtime because they've been stable
//! for years.

use crate::provider::{OAuthProvider, ProviderKind, ProviderProfile};

pub static GMAIL: GmailProvider = GmailProvider;

pub struct GmailProvider;

impl OAuthProvider for GmailProvider {
    fn profile(&self) -> &'static ProviderProfile {
        &PROFILE
    }
}

static PROFILE: ProviderProfile = ProviderProfile {
    name: "Gmail",
    slug: "gmail",
    client_id: env!("CAPYTAIN_GMAIL_CLIENT_ID"),
    authorization_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    scopes: &["https://mail.google.com/"],
    kind: ProviderKind::ImapSmtp,
};
