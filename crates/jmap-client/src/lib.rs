// SPDX-License-Identifier: Apache-2.0

//! Capytain JMAP adapter — [`MailBackend`] implementation over
//! `jmap-client` v0.4.
//!
//! The backend is constructed with a session URL and a bearer access
//! token minted by `capytain-auth`:
//!
//! 1. `jmap_client::client::Client::new().credentials(Credentials::bearer(token)).connect(session_url)`.
//! 2. `mailbox_get(None, None)` for discovery → `Vec<Folder>`.
//! 3. `list_messages` / `fetch_message` use `Email/query`, `Email/get`,
//!    and `Email/changes` for delta sync. The server's state token
//!    round-trips through `SyncState.backend_state`.
//!
//! Phase 0 Week 4 ships the **read path**. Write methods return
//! `MailError::Other("not yet implemented")` until Phase 1 Week 2, and
//! `watch()` stays at the trait default empty stream until Phase 1 Week
//! 1 when EventSource lands.

use async_trait::async_trait;
use jmap_client::client::Client;
use tokio::sync::Mutex;
use tracing::{debug, info};

use capytain_core::{
    AccountId, AttachmentRef, BackendKind, Folder, FolderId, FolderRole, MailBackend, MailError,
    MessageBody, MessageFlags, MessageId, MessageList, SyncState,
};

/// JMAP-backed [`MailBackend`].
pub struct JmapBackend {
    client: Mutex<Client>,
    account_id: AccountId,
    session_url: String,
}

impl JmapBackend {
    /// Connect to a JMAP session URL with the supplied bearer access
    /// token. The session URL typically lives at
    /// `https://<host>/.well-known/jmap` or a provider-specific path.
    pub async fn connect(
        session_url: &str,
        access_token: &str,
        account_id: AccountId,
    ) -> Result<Self, MailError> {
        let client = Client::new()
            .credentials(jmap_client::client::Credentials::bearer(access_token))
            .connect(session_url)
            .await
            .map_err(|e| MailError::Network(format!("JMAP connect {session_url}: {e}")))?;
        info!(session_url, "JMAP connected");
        Ok(Self {
            client: Mutex::new(client),
            account_id,
            session_url: session_url.to_string(),
        })
    }

    /// Session URL this backend connected to — exposed for logs and
    /// diagnostics.
    pub fn session_url(&self) -> &str {
        &self.session_url
    }
}

// ---------- MailBackend impl ----------

const NOT_YET: &str =
    "JMAP adapter read-path arrives in Phase 0 Week 4 (Mailbox/get landed here); \
    Email/query + Email/get + Email/changes wiring is staged for Phase 1 Week 1";

#[async_trait]
impl MailBackend for JmapBackend {
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError> {
        use jmap_client::core::query::Comparator;
        use jmap_client::mailbox::query::Filter as MailboxFilter;

        let client = self.client.lock().await;

        // JMAP's `mailbox_get(id, …)` takes a single id; for "all
        // mailboxes" we first Mailbox/query to get the id set, then
        // Mailbox/get each. One extra roundtrip — acceptable at the
        // mailbox count we expect (<100).
        let ids = client
            .mailbox_query(
                None::<jmap_client::core::query::Filter<MailboxFilter>>,
                None::<Vec<Comparator<jmap_client::mailbox::query::Comparator>>>,
            )
            .await
            .map_err(|e| MailError::Protocol(format!("Mailbox/query: {e}")))?
            .take_ids();

        let mut folders = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(mb) = client
                .mailbox_get(&id, None::<Vec<jmap_client::mailbox::Property>>)
                .await
                .map_err(|e| MailError::Protocol(format!("Mailbox/get {id}: {e}")))?
            {
                folders.push(mailbox_to_folder(mb, &self.account_id));
            }
        }
        drop(client);

        debug!(count = folders.len(), "JMAP Mailbox/get returned folders");
        Ok(folders)
    }

    async fn list_messages(
        &self,
        folder: &FolderId,
        since: Option<&SyncState>,
        limit: Option<u32>,
    ) -> Result<MessageList, MailError> {
        let _ = (folder, since, limit);
        Err(MailError::Other(NOT_YET.into()))
    }

    async fn fetch_message(&self, id: &MessageId) -> Result<MessageBody, MailError> {
        let _ = id;
        Err(MailError::Other(NOT_YET.into()))
    }

    async fn fetch_attachment(
        &self,
        message: &MessageId,
        attachment: &AttachmentRef,
    ) -> Result<Vec<u8>, MailError> {
        let _ = (message, attachment);
        Err(MailError::Other(NOT_YET.into()))
    }

    async fn update_flags(
        &self,
        messages: &[MessageId],
        _add: MessageFlags,
        _remove: MessageFlags,
    ) -> Result<(), MailError> {
        let _ = messages;
        Err(MailError::Other(
            "JMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn move_messages(
        &self,
        messages: &[MessageId],
        target: &FolderId,
    ) -> Result<(), MailError> {
        let _ = (messages, target);
        Err(MailError::Other(
            "JMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn delete_messages(&self, messages: &[MessageId]) -> Result<(), MailError> {
        let _ = messages;
        Err(MailError::Other(
            "JMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn save_draft(&self, raw_rfc822: &[u8]) -> Result<MessageId, MailError> {
        let _ = raw_rfc822;
        Err(MailError::Other(
            "JMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn submit_message(&self, raw_rfc822: &[u8]) -> Result<Option<MessageId>, MailError> {
        let _ = raw_rfc822;
        Err(MailError::Other(
            "JMAP EmailSubmission/set arrives in Phase 2 Week 2".into(),
        ))
    }
}

// ---------- helpers ----------

fn mailbox_to_folder(mb: jmap_client::mailbox::Mailbox, account: &AccountId) -> Folder {
    let id = mb.id().map(str::to_string).unwrap_or_default();
    let name = mb.name().map(str::to_string).unwrap_or_default();
    let role = jmap_role_to_folder_role(&mb.role());
    let parent = mb.parent_id().map(|p| FolderId(p.to_string()));
    // `total_emails()` / `unread_emails()` already default to 0 on
    // missing fields; they return `usize`.
    let unread = mb.unread_emails() as u32;
    let total = mb.total_emails() as u32;

    Folder {
        id: FolderId(id.clone()),
        account_id: account.clone(),
        name: name.clone(),
        // JMAP mailboxes are flat at the wire level; parent info is a
        // reference. For display, we leave `path` == `name` — the UI can
        // reconstruct a tree from `parent` if it wants hierarchy.
        path: name,
        role,
        unread_count: unread,
        total_count: total,
        parent,
    }
}

/// Translate JMAP's built-in mailbox roles (RFC 8621 §2) into Capytain's
/// `FolderRole`. `All` and `Flagged` don't exist as standard JMAP roles
/// — Gmail's "All Mail" and "Starred" are IMAP-specific concepts that
/// show up in JMAP as custom labels, which we surface as the None
/// default here and leave for client-side classification.
fn jmap_role_to_folder_role(role: &jmap_client::mailbox::Role) -> Option<FolderRole> {
    use jmap_client::mailbox::Role as R;
    match role {
        R::Inbox => Some(FolderRole::Inbox),
        R::Sent => Some(FolderRole::Sent),
        R::Drafts => Some(FolderRole::Drafts),
        R::Trash => Some(FolderRole::Trash),
        R::Junk => Some(FolderRole::Spam),
        R::Archive => Some(FolderRole::Archive),
        R::Important => Some(FolderRole::Important),
        R::Other(_) | R::None => None,
    }
}

/// True if the given account kind is backed by this adapter.
pub fn handles(kind: &BackendKind) -> bool {
    matches!(kind, BackendKind::Jmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jmap_client::mailbox::Role as R;

    #[test]
    fn role_translation_covers_known_jmap_roles() {
        assert_eq!(jmap_role_to_folder_role(&R::Inbox), Some(FolderRole::Inbox));
        assert_eq!(jmap_role_to_folder_role(&R::Sent), Some(FolderRole::Sent));
        assert_eq!(
            jmap_role_to_folder_role(&R::Drafts),
            Some(FolderRole::Drafts)
        );
        assert_eq!(jmap_role_to_folder_role(&R::Trash), Some(FolderRole::Trash));
        // Fastmail / JMAP uses "Junk"; Capytain normalizes to Spam.
        assert_eq!(jmap_role_to_folder_role(&R::Junk), Some(FolderRole::Spam));
        assert_eq!(
            jmap_role_to_folder_role(&R::Archive),
            Some(FolderRole::Archive)
        );
        assert_eq!(jmap_role_to_folder_role(&R::None), None);
        assert_eq!(jmap_role_to_folder_role(&R::Other("X".into())), None);
    }

    #[test]
    fn handles_jmap_only() {
        assert!(handles(&BackendKind::Jmap));
        assert!(!handles(&BackendKind::ImapSmtp));
    }
}
