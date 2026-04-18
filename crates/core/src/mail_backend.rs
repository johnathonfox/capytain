// SPDX-License-Identifier: Apache-2.0

//! The `MailBackend` trait — the one seam the rest of the workspace sees
//! for any mail protocol adapter.
//!
//! Every read, write, and live-sync operation goes through this trait. The
//! IMAP and JMAP adapters each implement it; everything else depends on the
//! trait, not on either adapter.
//!
//! Trait shape is per `TRAITS.md`. Phase 0 Week 4 lands the read path
//! (`list_folders`, `list_messages`, `fetch_message`); the write-side
//! methods (`update_flags`, `move_messages`, `delete_messages`,
//! `save_draft`, `submit_message`) are part of the trait surface now but
//! their concrete implementations return `MailError::Other("not yet
//! implemented")` until Week 2 of Phase 1 lands the write path.

use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_core::stream::{BoxStream, Stream};

use crate::error::MailError;
use crate::ids::{AttachmentRef, FolderId, MessageId};
use crate::message::{MessageBody, MessageFlags, MessageHeaders};
use crate::sync_state::SyncState;
use crate::Folder;

/// The result of a `list_messages` call.
#[derive(Debug, Clone)]
pub struct MessageList {
    /// Headers for every message in the delta.
    pub messages: Vec<MessageHeaders>,
    /// The new sync cursor to persist. Hand it back on the next call.
    pub new_state: SyncState,
    /// IDs of messages the server says have disappeared since `since`.
    /// Empty when `since` was `None` (full fetch).
    pub removed: Vec<MessageId>,
}

/// Real-time change notifications from a backend's live-sync stream.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BackendEvent {
    MessageAdded { folder: FolderId, id: MessageId },
    MessageChanged { folder: FolderId, id: MessageId },
    MessageRemoved { folder: FolderId, id: MessageId },
    FolderChanged { folder: FolderId },
    ConnectionLost,
    ConnectionRestored,
}

/// The one protocol-agnostic mail abstraction.
///
/// Implementations live in `capytain-imap-client` and
/// `capytain-jmap-client`. The sync engine (`capytain-sync`) and the CLI
/// (`mailcli`) depend only on this trait.
#[async_trait]
pub trait MailBackend: Send + Sync {
    // ---------- Discovery ----------

    /// Return all folders / mailboxes for this account.
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError>;

    // ---------- Read ----------

    /// Fetch message headers for a folder. When `since` is `Some`, return
    /// only the delta against that cursor plus the list of removed ids.
    /// When `since` is `None`, return every message the adapter can
    /// reasonably surface (bounded by `limit` if set).
    async fn list_messages(
        &self,
        folder: &FolderId,
        since: Option<&SyncState>,
        limit: Option<u32>,
    ) -> Result<MessageList, MailError>;

    /// Fetch the full body of a single message.
    async fn fetch_message(&self, id: &MessageId) -> Result<MessageBody, MailError>;

    /// Fetch the bytes of a single attachment.
    async fn fetch_attachment(
        &self,
        message: &MessageId,
        attachment: &AttachmentRef,
    ) -> Result<Vec<u8>, MailError>;

    // ---------- Write (Phase 1) ----------

    async fn update_flags(
        &self,
        messages: &[MessageId],
        add: MessageFlags,
        remove: MessageFlags,
    ) -> Result<(), MailError>;

    async fn move_messages(
        &self,
        messages: &[MessageId],
        target: &FolderId,
    ) -> Result<(), MailError>;

    async fn delete_messages(&self, messages: &[MessageId]) -> Result<(), MailError>;

    // ---------- Compose / Send (Phase 2) ----------

    async fn save_draft(&self, raw_rfc822: &[u8]) -> Result<MessageId, MailError>;

    async fn submit_message(&self, raw_rfc822: &[u8]) -> Result<Option<MessageId>, MailError>;

    // ---------- Live sync (Phase 1) ----------

    /// Subscribe to real-time changes. Stream yields until the handle is
    /// dropped. Default implementation returns an empty stream — adapters
    /// override when they support a push mechanism (IDLE for IMAP,
    /// EventSource for JMAP).
    fn watch(&self) -> BoxStream<'static, BackendEvent> {
        Box::pin(EmptyStream)
    }
}

/// Trivial empty stream — kept local to this module so `capytain-core`
/// doesn't pull in `futures-util` just for one utility constructor.
struct EmptyStream;

impl Stream for EmptyStream {
    type Item = BackendEvent;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }
}
