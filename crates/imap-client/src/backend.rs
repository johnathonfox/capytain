// SPDX-License-Identifier: Apache-2.0

//! [`ImapBackend`] — the concrete [`MailBackend`] implementation.
//!
//! The session is held behind a tokio `Mutex` because IMAP is a
//! stateful, serialized protocol: there's exactly one in-flight command
//! at a time per connection. Every [`MailBackend`] method locks the
//! mutex, runs its command sequence, unlocks.

use std::sync::Arc;

use async_imap::types::Fetch;
use async_imap::{Client, Session};
use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::client::TlsStream;
use tracing::{debug, info, warn};

use capytain_core::{
    AccountId, AttachmentRef, BackendKind, Folder, FolderId, FolderRole, MailBackend, MailError,
    MessageBody, MessageFlags, MessageId, MessageList, SyncState,
};

use crate::auth::XOAuth2;
use crate::capabilities::require as require_caps;
use crate::sync_state::BackendState;

type StreamT = TlsStream<TcpStream>;

/// Production-grade IMAP backend: TLS, XOAUTH2, CONDSTORE + QRESYNC +
/// IDLE required at connect.
pub struct ImapBackend {
    session: Mutex<Session<StreamT>>,
    account: AccountId,
    host: Arc<str>,
}

impl ImapBackend {
    /// Wrap an already-authenticated [`Session`]. Used by tests that
    /// supply a pre-scripted stream; the production `connect_tls`
    /// constructor is the normal entry point.
    pub(crate) fn from_session(
        session: Session<StreamT>,
        account: AccountId,
        host: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            session: Mutex::new(session),
            account,
            host: host.into(),
        }
    }

    /// Connect to `host:993` over TLS, authenticate via SASL XOAUTH2
    /// with the supplied `access_token`, verify required capabilities,
    /// and return a ready-to-use backend.
    pub async fn connect_tls(
        host: &str,
        port: u16,
        email: &str,
        access_token: &str,
        account: AccountId,
    ) -> Result<Self, MailError> {
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| MailError::Network(format!("tcp connect {host}:{port}: {e}")))?;
        let tls = tls_connect(host, tcp).await?;

        let mut client = Client::new(tls);
        // Read the server greeting. async-imap requires this before any
        // commands run — without it the first command returns UNTAGGED.
        let _greeting = client
            .read_response()
            .await
            .map_err(|e| MailError::Protocol(format!("greeting: {e}")))?;

        let authenticator = XOAuth2::new(email, access_token);
        let mut session = client
            .authenticate("XOAUTH2", &authenticator)
            .await
            .map_err(|(e, _client)| MailError::Auth(format!("XOAUTH2: {e}")))?;

        // Force a CAPABILITY roundtrip; some servers only advertise the
        // post-auth set after login, not in the greeting.
        let caps = session
            .capabilities()
            .await
            .map_err(|e| MailError::Protocol(format!("CAPABILITY: {e}")))?;
        let cap_strings: Vec<String> = caps
            .iter()
            .map(|c| format!("{c:?}").trim_matches('"').to_string())
            .collect();
        require_caps(&cap_strings)?;

        info!(host, email, "IMAP connected and authenticated");
        Ok(Self::from_session(session, account, host))
    }

    /// The host this backend connected to — exposed for logs/diagnostics.
    pub fn host(&self) -> &str {
        &self.host
    }
}

async fn tls_connect(host: &str, tcp: TcpStream) -> Result<StreamT, MailError> {
    use tokio_rustls::rustls::{pki_types::ServerName, ClientConfig, RootCertStore};

    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| MailError::Network(format!("invalid SNI hostname {host}: {e}")))?;

    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| MailError::Network(format!("TLS handshake with {host}: {e}")))
}

// ---------- MailBackend impl ----------

const NOT_YET: &str = "IMAP adapter read-path arrives in Phase 0 Week 4 part 2a (this PR); \
    concrete command machinery for list_folders / list_messages / fetch_message is the \
    next commit in this branch";

#[async_trait]
impl MailBackend for ImapBackend {
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError> {
        let mut session = self.session.lock().await;
        let mut stream = session
            .list(None, Some("*"))
            .await
            .map_err(|e| MailError::Protocol(format!("LIST: {e}")))?;

        let mut folders = Vec::new();
        while let Some(item) = stream.next().await {
            let name = item.map_err(|e| MailError::Protocol(format!("LIST entry: {e}")))?;
            folders.push(name_to_folder(&name, &self.account));
        }
        drop(stream);
        debug!(count = folders.len(), "IMAP LIST returned folders");
        Ok(folders)
    }

    async fn list_messages(
        &self,
        folder: &FolderId,
        since: Option<&SyncState>,
        limit: Option<u32>,
    ) -> Result<MessageList, MailError> {
        // Placeholder: the wire command shape — SELECT folder (checks
        // UIDVALIDITY against `since`), UID FETCH (or CHANGEDSINCE for
        // CONDSTORE delta), ENVELOPE + RFC822.SIZE + FLAGS — is the next
        // increment on this branch. The method exists now so the trait
        // is fully populated and callers' plumbing compiles.
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
            "IMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn move_messages(
        &self,
        messages: &[MessageId],
        target: &FolderId,
    ) -> Result<(), MailError> {
        let _ = (messages, target);
        Err(MailError::Other(
            "IMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn delete_messages(&self, messages: &[MessageId]) -> Result<(), MailError> {
        let _ = messages;
        Err(MailError::Other(
            "IMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn save_draft(&self, raw_rfc822: &[u8]) -> Result<MessageId, MailError> {
        let _ = raw_rfc822;
        Err(MailError::Other(
            "IMAP write path arrives in Phase 1 Week 2".into(),
        ))
    }

    async fn submit_message(&self, raw_rfc822: &[u8]) -> Result<Option<MessageId>, MailError> {
        let _ = raw_rfc822;
        Err(MailError::Other(
            "IMAP submission arrives in Phase 0 Week 2 (Phase 2) via capytain-smtp-client".into(),
        ))
    }
}

// ---------- helpers ----------

fn name_to_folder(name: &async_imap::types::Name, account: &AccountId) -> Folder {
    // IMAP's LIST response hands us a hierarchical path delimited by
    // some character (usually '/' or '.'). We store the full path as-is
    // and derive the leaf as the display name.
    let path = name.name().to_string();
    let delimiter = name.delimiter().unwrap_or("/");
    let display = path
        .rsplit_once(delimiter)
        .map(|(_, leaf)| leaf.to_string())
        .unwrap_or_else(|| path.clone());
    let role = role_from_attributes(
        &name
            .attributes()
            .iter()
            .map(|a| format!("{a:?}"))
            .collect::<Vec<_>>(),
    );

    Folder {
        id: FolderId(path.clone()),
        account_id: account.clone(),
        name: display,
        path,
        role,
        unread_count: 0,
        total_count: 0,
        parent: None,
    }
}

fn role_from_attributes(attributes: &[String]) -> Option<FolderRole> {
    // IMAP SPECIAL-USE (RFC 6154) attributes are prefixed with `\`. The
    // Debug formatting of async-imap's `NameAttribute::Custom` yields
    // something like `Custom("\\Inbox")`, so we look for the
    // well-known names anywhere in the formatted string.
    let joined = attributes.join(" ").to_ascii_lowercase();
    if joined.contains("inbox") {
        Some(FolderRole::Inbox)
    } else if joined.contains("sent") {
        Some(FolderRole::Sent)
    } else if joined.contains("drafts") {
        Some(FolderRole::Drafts)
    } else if joined.contains("trash") {
        Some(FolderRole::Trash)
    } else if joined.contains("junk") || joined.contains("spam") {
        Some(FolderRole::Spam)
    } else if joined.contains("archive") {
        Some(FolderRole::Archive)
    } else if joined.contains("important") {
        Some(FolderRole::Important)
    } else if joined.contains("all") {
        Some(FolderRole::All)
    } else if joined.contains("flagged") {
        Some(FolderRole::Flagged)
    } else {
        None
    }
}

// Kept around because callers will eventually need it when the fetch
// path lands in the next increment on this branch.
#[allow(dead_code)]
fn ensure_uidvalidity_matches(
    cached: &BackendState,
    observed_uidvalidity: u32,
) -> Result<(), MailError> {
    if cached.uidvalidity != observed_uidvalidity {
        warn!(
            cached = cached.uidvalidity,
            observed = observed_uidvalidity,
            "UIDVALIDITY changed — cached state is stale"
        );
        return Err(MailError::Protocol(
            "UIDVALIDITY changed; refetch the folder from scratch".into(),
        ));
    }
    Ok(())
}

// Silence unused-fetch warning until list_messages/fetch_message land.
#[allow(dead_code)]
fn _keep_types_alive(_f: Fetch) {}

// ---------- BackendKind helper ----------

/// True if the given account kind is backed by this adapter.
pub fn handles(kind: &BackendKind) -> bool {
    matches!(kind, BackendKind::ImapSmtp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_from_attributes_recognizes_special_use() {
        assert_eq!(
            role_from_attributes(&["\\Inbox".into()]),
            Some(FolderRole::Inbox)
        );
        assert_eq!(
            role_from_attributes(&["Custom(\"\\\\Sent\")".into()]),
            Some(FolderRole::Sent)
        );
        assert_eq!(
            role_from_attributes(&["\\Drafts".into(), "\\HasNoChildren".into()]),
            Some(FolderRole::Drafts)
        );
        assert_eq!(role_from_attributes(&["\\HasChildren".into()]), None);
    }

    #[test]
    fn ensure_uidvalidity_flags_change() {
        let cached = BackendState {
            uidvalidity: 10,
            highestmodseq: 1,
            uidnext: 1,
        };
        assert!(ensure_uidvalidity_matches(&cached, 10).is_ok());
        let err = ensure_uidvalidity_matches(&cached, 11).unwrap_err();
        assert!(err.to_string().contains("UIDVALIDITY changed"));
    }

    #[test]
    fn handles_imapsmtp_only() {
        assert!(handles(&BackendKind::ImapSmtp));
        assert!(!handles(&BackendKind::Jmap));
    }
}
