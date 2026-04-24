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
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::client::TlsStream;
use tracing::{debug, info, warn};

use capytain_core::{
    AccountId, AttachmentRef, BackendKind, EmailAddress, Folder, FolderId, FolderRole, MailBackend,
    MailError, MessageBody, MessageFlags, MessageHeaders, MessageId, MessageList, SyncState,
    ThreadId,
};

use crate::auth::XOAuth2;
use crate::capabilities::require as require_caps;
use crate::sync_state::{BackendState, MessageRef};

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
        // `async_imap::types::Capability` is an enum (`Imap4rev1`,
        // `Auth(String)`, `Atom(String)`). Debug-formatting it — what
        // this code used to do — yielded strings like `Atom("IDLE")`
        // that never matched the uppercase atom names the capabilities
        // check expects. Pattern-match explicitly instead.
        let cap_strings: Vec<String> = caps
            .iter()
            .map(|c| match c {
                async_imap::types::Capability::Imap4rev1 => "IMAP4REV1".to_string(),
                async_imap::types::Capability::Auth(s) => format!("AUTH={s}"),
                async_imap::types::Capability::Atom(s) => s.clone(),
            })
            .collect();
        tracing::debug!(capabilities = ?cap_strings, "IMAP server capabilities");
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
        let mut session = self.session.lock().await;

        let mbox = session
            .select(&folder.0)
            .await
            .map_err(|e| MailError::Protocol(format!("SELECT {}: {e}", folder.0)))?;
        let uidvalidity = mbox
            .uid_validity
            .ok_or_else(|| MailError::Protocol("SELECT missing UIDVALIDITY".into()))?;
        let uidnext = mbox.uid_next.unwrap_or(1);

        // Build the new backend state from what SELECT told us. Any
        // call to list_messages ends with this persisted regardless of
        // how the fetch side goes.
        let new_state = BackendState {
            uidvalidity,
            // `highestmodseq` comes in via Mailbox.extensions on CONDSTORE
            // servers; async-imap 0.11 surfaces it differently across
            // response shapes, so we track 0 as a best-effort seed and let
            // Phase 1's smarter delta logic refine.
            highestmodseq: 0,
            uidnext,
        };

        // Decide the UID set to FETCH. If `since` is present *and*
        // UIDVALIDITY matches, we only need messages the server added
        // after the previous uidnext. Otherwise we do a bounded initial
        // sync.
        let uid_set = match since {
            Some(state) => {
                let cached = BackendState::from_sync(state)?;
                if cached.uidvalidity != uidvalidity {
                    warn!(
                        cached = cached.uidvalidity,
                        observed = uidvalidity,
                        "UIDVALIDITY changed; caller must refetch from scratch"
                    );
                    return Err(MailError::Protocol(format!(
                        "UIDVALIDITY changed for {} ({} → {})",
                        folder.0, cached.uidvalidity, uidvalidity
                    )));
                }
                // New messages have UID >= cached.uidnext.
                format!("{}:*", cached.uidnext)
            }
            None => match limit {
                // A bare `1:*` would pull the whole folder. For initial
                // sync we lean on the limit if the caller supplied one.
                Some(n) if n > 0 => {
                    // Fetch the `n` highest UIDs — the most recent messages
                    // are usually what the UI wants first.
                    let lo = uidnext.saturating_sub(n);
                    format!("{}:*", lo.max(1))
                }
                _ => "1:*".to_string(),
            },
        };

        let query = "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE)";
        let mut fetches = session
            .uid_fetch(&uid_set, query)
            .await
            .map_err(|e| MailError::Protocol(format!("UID FETCH {uid_set} {query}: {e}")))?;

        let mut messages = Vec::new();
        while let Some(item) = fetches.next().await {
            let fetch = item.map_err(|e| MailError::Protocol(format!("FETCH entry: {e}")))?;
            match fetch_to_headers(&fetch, folder, uidvalidity, &self.account) {
                Ok(Some(h)) => messages.push(h),
                Ok(None) => {
                    debug!(message = ?fetch.message, "FETCH response missing UID — skipping");
                }
                Err(e) => {
                    warn!(error = %e, message = ?fetch.message, "failed to translate FETCH");
                }
            }
        }
        drop(fetches);
        debug!(
            folder = %folder.0,
            count = messages.len(),
            "IMAP list_messages"
        );

        Ok(MessageList {
            messages,
            new_state: SyncState {
                folder_id: folder.clone(),
                backend_state: new_state.encode(),
            },
            // QRESYNC's VANISHED response is the standard route for
            // server-side deletion detection since the last sync. The
            // full VANISHED parser lands alongside Phase 1's delta
            // work; for now we surface an empty `removed` list and
            // rely on the next full rescan to catch deletions.
            removed: Vec::new(),
        })
    }

    async fn fetch_message(&self, id: &MessageId) -> Result<MessageBody, MailError> {
        let r = MessageRef::decode(id)?;
        let mut session = self.session.lock().await;

        let mbox = session
            .select(&r.folder)
            .await
            .map_err(|e| MailError::Protocol(format!("SELECT {}: {e}", r.folder)))?;
        let current_uv = mbox
            .uid_validity
            .ok_or_else(|| MailError::Protocol("SELECT missing UIDVALIDITY".into()))?;
        if current_uv != r.uidvalidity {
            return Err(MailError::Protocol(format!(
                "UIDVALIDITY changed for {} ({} → {})",
                r.folder, r.uidvalidity, current_uv
            )));
        }

        let query = "(UID RFC822)";
        let mut fetches = session
            .uid_fetch(&r.uid.to_string(), query)
            .await
            .map_err(|e| MailError::Protocol(format!("UID FETCH {} {query}: {e}", r.uid)))?;

        let fetch = fetches
            .next()
            .await
            .ok_or(MailError::NotFound(format!(
                "message UID {} in {}",
                r.uid, r.folder
            )))?
            .map_err(|e| MailError::Protocol(format!("FETCH entry: {e}")))?;
        drop(fetches);

        let raw = fetch
            .body()
            .ok_or_else(|| MailError::Protocol("FETCH returned no RFC822 body".into()))?
            .to_vec();

        let folder_id = FolderId(r.folder.clone());
        let flags = extract_flags(&fetch);
        let identity = capytain_mime::MessageIdentity {
            id,
            account_id: &self.account,
            folder_id: &folder_id,
            thread_id: None,
            size: raw.len() as u32,
            flags: &flags,
            labels: &[],
        };
        capytain_mime::parse_rfc822(&raw, identity)
            .ok_or_else(|| MailError::Parse(format!("mail-parser could not parse UID {}", r.uid)))
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

fn fetch_to_headers(
    fetch: &Fetch,
    folder: &FolderId,
    uidvalidity: u32,
    account: &AccountId,
) -> Result<Option<MessageHeaders>, MailError> {
    let Some(uid) = fetch.uid else {
        return Ok(None);
    };
    let envelope = fetch.envelope();
    let flags = extract_flags(fetch);
    let size = fetch.size.unwrap_or(0);
    let rfc822_message_id = envelope
        .and_then(|e| e.message_id.as_deref())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.to_string());
    let subject = envelope
        .and_then(|e| e.subject.as_deref())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(capytain_mime::decode_header_value)
        .unwrap_or_default();
    let from = addr_vec(envelope.and_then(|e| e.from.as_ref()));
    let reply_to = addr_vec(envelope.and_then(|e| e.reply_to.as_ref()));
    let to = addr_vec(envelope.and_then(|e| e.to.as_ref()));
    let cc = addr_vec(envelope.and_then(|e| e.cc.as_ref()));
    let bcc = addr_vec(envelope.and_then(|e| e.bcc.as_ref()));

    // Prefer INTERNALDATE (always present on servers). Fall back to the
    // envelope date if the server skips it for some reason.
    let date = fetch
        .internal_date()
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| {
            envelope
                .and_then(|e| e.date.as_deref())
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(parse_rfc2822_to_utc)
        })
        .unwrap_or_else(Utc::now);

    let r = MessageRef {
        uidvalidity,
        uid,
        folder: folder.0.clone(),
    };

    Ok(Some(MessageHeaders {
        id: r.encode(),
        account_id: account.clone(),
        folder_id: folder.clone(),
        thread_id: None::<ThreadId>,
        rfc822_message_id,
        subject,
        from,
        reply_to,
        to,
        cc,
        bcc,
        date,
        flags,
        labels: Vec::new(),
        snippet: String::new(),
        size,
        has_attachments: false,
    }))
}

fn addr_vec(addrs: Option<&Vec<imap_proto::Address<'_>>>) -> Vec<EmailAddress> {
    let Some(list) = addrs else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|a| {
            let mailbox = a
                .mailbox
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())?;
            let host = a
                .host
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())?;
            let name = a
                .name
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(capytain_mime::decode_header_value);
            Some(EmailAddress {
                address: format!("{mailbox}@{host}"),
                display_name: name,
            })
        })
        .collect()
}

fn extract_flags(fetch: &Fetch) -> MessageFlags {
    use async_imap::types::Flag;
    let mut flags = MessageFlags::default();
    for f in fetch.flags() {
        match f {
            Flag::Seen => flags.seen = true,
            Flag::Flagged => flags.flagged = true,
            Flag::Answered => flags.answered = true,
            Flag::Draft => flags.draft = true,
            Flag::Custom(s) if s.eq_ignore_ascii_case("$forwarded") => flags.forwarded = true,
            _ => {}
        }
    }
    flags
}

fn parse_rfc2822_to_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

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
