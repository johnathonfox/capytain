// SPDX-License-Identifier: Apache-2.0

//! Account — one provider login owned by the local user.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AccountId;

/// A configured mail account. Credentials for the account live in the OS
/// keychain and are referenced via `auth_ref` in storage; they never appear
/// on this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Local identifier; stable across renames of `display_name`.
    pub id: AccountId,

    /// Which protocol adapter handles this account.
    pub kind: BackendKind,

    /// User-facing name ("Work", "Personal"). Rename-safe.
    pub display_name: String,

    /// Primary email address for this account.
    pub email_address: String,

    /// When the account was first added locally.
    pub created_at: DateTime<Utc>,
}

/// Which `MailBackend` implementation handles an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BackendKind {
    /// IMAP for the read path plus SMTP submission for sending. Used by
    /// Gmail, Microsoft 365, and most self-hosted servers.
    ImapSmtp,

    /// JMAP for both read and write. Used by Fastmail and Stalwart.
    Jmap,
}
