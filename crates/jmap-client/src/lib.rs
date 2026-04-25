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

pub mod push;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use jmap_client::client::Client;
use tokio::sync::Mutex;
use tracing::{debug, info};

use capytain_core::{
    AccountId, Attachment, AttachmentRef, BackendKind, EmailAddress, Folder, FolderId, FolderRole,
    MailBackend, MailError, MessageBody, MessageFlags, MessageHeaders, MessageId, MessageList,
    SyncState, ThreadId,
};

pub use push::watch_account;

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
        let client = dial_client(session_url, access_token).await?;
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

/// Open a fresh JMAP `Client` against `session_url` with a bearer
/// access token. Both [`JmapBackend::connect`] and the
/// [`crate::push::watch_account`] watcher call through this so the
/// connect logic — bearer credentials, session resolution — lives
/// in one place. Mirrors `capytain_imap_client::dial_session`.
pub async fn dial_client(session_url: &str, access_token: &str) -> Result<Client, MailError> {
    Client::new()
        .credentials(jmap_client::client::Credentials::bearer(access_token))
        .connect(session_url)
        .await
        .map_err(|e| MailError::Network(format!("JMAP connect {session_url}: {e}")))
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
        use jmap_client::core::query::Comparator;
        use jmap_client::email::{self, query::Filter};

        let client = self.client.lock().await;

        // Discovery: what are the ids in this mailbox that changed
        // since our last sync? JMAP's Email/changes is the right answer
        // when `since` is present — it also tells us what was destroyed.
        // For the initial sync (no `since`) we fall back to Email/query
        // scoped to `inMailbox = <folder>`.
        let (ids_to_fetch, removed, new_state_token) = if let Some(state) = since {
            let changes = client
                .email_changes(state.backend_state.as_str(), None)
                .await
                .map_err(|e| MailError::Protocol(format!("Email/changes: {e}")))?;
            let created: Vec<String> = changes.created().iter().map(|s| s.to_string()).collect();
            let updated: Vec<String> = changes.updated().iter().map(|s| s.to_string()).collect();
            let destroyed: Vec<MessageId> = changes
                .destroyed()
                .iter()
                .map(|s| MessageId(s.to_string()))
                .collect();
            let mut all = created;
            all.extend(updated);
            let new_state = changes.new_state().to_string();
            (all, destroyed, new_state)
        } else {
            let filter = Filter::in_mailbox(folder.0.clone());
            let query = client
                .email_query(
                    Some(filter),
                    None::<Vec<Comparator<email::query::Comparator>>>,
                )
                .await
                .map_err(|e| MailError::Protocol(format!("Email/query: {e}")))?;
            let all_ids: Vec<String> = query.ids().iter().map(|s| s.to_string()).collect();
            let ids = match limit {
                Some(n) if (n as usize) < all_ids.len() => all_ids
                    .into_iter()
                    .rev()
                    .take(n as usize)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
                _ => all_ids,
            };
            // Email/query doesn't return a state-like cursor on its
            // own; the state we persist is Email/changes-ready —
            // clients must do one follow-up `email_changes` on next
            // sync. We seed with the query's `query_state` if present,
            // otherwise an empty string (which JMAP treats as "any
            // prior state").
            let new_state = query.query_state().to_string();
            (ids, Vec::new(), new_state)
        };

        let mut messages = Vec::with_capacity(ids_to_fetch.len());
        for id in &ids_to_fetch {
            let email = client
                .email_get(id.as_str(), None::<Vec<jmap_client::email::Property>>)
                .await
                .map_err(|e| MailError::Protocol(format!("Email/get {id}: {e}")))?;
            if let Some(email) = email {
                messages.push(email_to_headers(&email, folder, &self.account_id));
            }
        }
        drop(client);

        debug!(
            folder = %folder.0,
            fetched = messages.len(),
            destroyed = removed.len(),
            "JMAP list_messages"
        );

        Ok(MessageList {
            messages,
            // JMAP `Email/changes` returns ID-level created/updated/
            // destroyed sets but no per-message flag delta in the
            // shape `flag_updates` expects. Surfacing JMAP keyword
            // changes here lands alongside the `Email/changes` →
            // `Email/get` follow-up fetch in the JMAP polish pass.
            flag_updates: Vec::new(),
            new_state: SyncState {
                folder_id: folder.clone(),
                backend_state: new_state_token,
            },
            removed,
        })
    }

    async fn fetch_message(&self, id: &MessageId) -> Result<MessageBody, MailError> {
        let client = self.client.lock().await;
        let email = client
            .email_get(&id.0, None::<Vec<jmap_client::email::Property>>)
            .await
            .map_err(|e| MailError::Protocol(format!("Email/get {}: {e}", id.0)))?
            .ok_or_else(|| MailError::NotFound(format!("JMAP email id {}", id.0)))?;
        drop(client);

        // Folder assignment: JMAP emails belong to one-or-more mailboxes
        // via `mailboxIds`. For our MessageBody we pick the first
        // (there's no guaranteed canonical one in JMAP). Callers that
        // care about the "which folder" story can look at mailbox_ids
        // directly once we surface it.
        let folder_id = email
            .mailbox_ids()
            .first()
            .map(|s| FolderId(s.to_string()))
            .unwrap_or_else(|| FolderId(String::new()));

        let headers = email_to_headers(&email, &folder_id, &self.account_id);

        // Body content: JMAP returns textBody/htmlBody as references
        // into `bodyValues`. For Phase 0 read path, `preview()` + what
        // text_body/html_body point at is enough to surface plaintext.
        // Full bodyValues wiring lands in the Phase 1 polish pass.
        let body_text = email.preview().map(str::to_string);

        Ok(MessageBody {
            headers,
            body_html: None,
            body_text,
            attachments: Vec::<Attachment>::new(),
            in_reply_to: email
                .in_reply_to()
                .and_then(|list| list.first())
                .map(|s| format!("<{s}>")),
            references: email
                .references()
                .map(|list| list.iter().map(|s| format!("<{s}>")).collect())
                .unwrap_or_default(),
        })
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

fn email_to_headers(
    email: &jmap_client::email::Email,
    folder: &FolderId,
    account: &AccountId,
) -> MessageHeaders {
    let id = MessageId(email.id().unwrap_or_default().to_string());
    let subject = email.subject().unwrap_or_default().to_string();
    let rfc822_message_id = email
        .message_id()
        .and_then(|list| list.first())
        .map(|s| format!("<{s}>"));
    let date = email
        .received_at()
        .or_else(|| email.sent_at())
        .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
        .unwrap_or_else(Utc::now);

    let flags = keywords_to_flags(&email.keywords());
    let snippet = email.preview().unwrap_or_default().to_string();
    let size = u32::try_from(email.size()).unwrap_or(u32::MAX);

    MessageHeaders {
        id,
        account_id: account.clone(),
        folder_id: folder.clone(),
        thread_id: None::<ThreadId>,
        rfc822_message_id,
        subject,
        from: translate_addrs(email.from()),
        reply_to: translate_addrs(email.reply_to()),
        to: translate_addrs(email.to()),
        cc: translate_addrs(email.cc()),
        bcc: translate_addrs(email.bcc()),
        date,
        flags,
        // JMAP mailboxes serve as both folders and labels; a message in
        // multiple mailboxes shows up in each. For Capytain, the
        // secondary mailboxes become labels.
        labels: email.mailbox_ids().iter().map(|s| s.to_string()).collect(),
        snippet,
        size,
        has_attachments: email.has_attachment(),
    }
}

fn translate_addrs(addrs: Option<&[jmap_client::email::EmailAddress]>) -> Vec<EmailAddress> {
    let Some(list) = addrs else {
        return Vec::new();
    };
    list.iter()
        .map(|a| EmailAddress {
            address: a.email().to_string(),
            display_name: a.name().map(str::to_string),
        })
        .collect()
}

fn keywords_to_flags(keywords: &[&str]) -> MessageFlags {
    let mut flags = MessageFlags::default();
    for k in keywords {
        match k.to_ascii_lowercase().as_str() {
            "$seen" => flags.seen = true,
            "$flagged" => flags.flagged = true,
            "$answered" => flags.answered = true,
            "$draft" => flags.draft = true,
            "$forwarded" => flags.forwarded = true,
            _ => {}
        }
    }
    flags
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
