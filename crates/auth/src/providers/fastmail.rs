// SPDX-License-Identifier: Apache-2.0

//! Fastmail OAuth2 profile (JMAP backend).
//!
//! Fastmail's OAuth2 endpoints live under `api.fastmail.com/oauth`; the
//! JMAP session + core scopes cover read, write, and submission.
//! Reference: <https://www.fastmail.com/dev/oauth/>.

use crate::provider::{OAuthProvider, ProviderKind, ProviderProfile};

pub static FASTMAIL: FastmailProvider = FastmailProvider;

pub struct FastmailProvider;

impl OAuthProvider for FastmailProvider {
    fn profile(&self) -> &'static ProviderProfile {
        &PROFILE
    }
}

static PROFILE: ProviderProfile = ProviderProfile {
    name: "Fastmail",
    slug: "fastmail",
    client_id: env!("QSL_FASTMAIL_CLIENT_ID"),
    // Fastmail's native-app OAuth is PKCE-only (no secret). Env-var
    // hook kept for future-proofing in case they add a confidential
    // mode.
    client_secret: env!("QSL_FASTMAIL_CLIENT_SECRET"),
    authorization_url: "https://api.fastmail.com/oauth/authorize",
    token_url: "https://api.fastmail.com/oauth/refresh",
    scopes: &[
        "https://www.fastmail.com/dev/protocol-imap",
        "https://www.fastmail.com/dev/protocol-smtp",
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
        "urn:ietf:params:jmap:submission",
    ],
    kind: ProviderKind::Jmap,
};
