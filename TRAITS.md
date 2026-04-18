# Traits and Core Types

This document pins the interfaces that the rest of the codebase depends on. Everything here lives in `crates/core` and `crates/storage`, and every other crate should depend on these abstractions rather than on concrete implementations.

The goal: any two engineers (or any two Claude Code sessions) working in parallel on different crates should agree on these types and methods without having to negotiate.

## Error Types

Two error enums, both in `crates/core/src/error.rs`. Libraries return `Result<T, _>` with these; binaries (tauri commands, `mailcli`) wrap them with `anyhow::Error` at the edges.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MailError {
    #[error("network error: {0}")]
    Network(String),

    #[error("authentication failed or token expired: {0}")]
    Auth(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("message or folder not found: {0}")]
    NotFound(String),

    #[error("server rejected operation: {0}")]
    ServerRejected(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Db(String),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("row not found")]
    NotFound,

    #[error("unique constraint violated: {0}")]
    Conflict(String),

    #[error("serialization error: {0}")]
    Serde(String),
}
```

**Rule:** any backend impl (IMAP, JMAP) translates its own internal errors into `MailError` variants before returning. No `async_imap::Error` or `jmap_client::Error` ever crosses the `MailBackend` trait boundary.

## ID Types

Opaque, serializable newtype wrappers. The core never parses or interprets these — they're backend-specific strings the backend itself understands.

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FolderId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DraftId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentRef(pub String);
```

For IMAP, a `MessageId` encodes `<folder_uid_validity>:<uid>` as a string. For JMAP, it's the opaque email id. The core never inspects the inner string.

## Domain Types

```rust
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub kind: BackendKind,
    pub display_name: String,
    pub email_address: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    ImapSmtp,
    Jmap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub path: String,
    pub role: Option<FolderRole>,
    pub unread_count: u32,
    pub total_count: u32,
    pub parent: Option<FolderId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Spam,
    Archive,
    Important,
    All,
    Flagged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddress {
    pub address: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub forwarded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeaders {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub thread_id: Option<ThreadId>,
    pub rfc822_message_id: Option<String>, // "<foo@host>" Message-ID header
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>, // usually empty on received mail
    pub date: DateTime<Utc>,
    pub flags: MessageFlags,
    pub labels: Vec<String>,
    pub snippet: String,
    pub size: u32,
    pub has_attachments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBody {
    pub headers: MessageHeaders,
    pub body_html: Option<String>,   // raw, unsanitized
    pub body_text: Option<String>,   // plaintext alternative, if present
    pub attachments: Vec<Attachment>,
    pub in_reply_to: Option<String>, // Message-ID header value
    pub references: Vec<String>,     // References header, parsed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentRef,
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub inline: bool,
    pub content_id: Option<String>,
}

/// Opaque sync state owned by the backend. The core persists it and hands it
/// back. IMAP serializes (uidvalidity, highestmodseq, uidnext); JMAP stores
/// the server's state string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    pub folder_id: FolderId,
    pub backend_state: String, // opaque to the core
}
```

## MailBackend Trait

The one abstraction that both the IMAP and JMAP adapters implement. Lives in `crates/core/src/mail_backend.rs`.

```rust
use async_trait::async_trait;
use futures::stream::BoxStream;

#[async_trait]
pub trait MailBackend: Send + Sync {
    // ---------- Discovery ----------

    /// Return all folders / mailboxes for this account.
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError>;

    // ---------- Read ----------

    /// Fetch message headers in a folder. If `since` is provided, return only
    /// changes since that state. If not, return all messages in the folder
    /// (bounded by `limit`).
    async fn list_messages(
        &self,
        folder: &FolderId,
        since: Option<&SyncState>,
        limit: Option<u32>,
    ) -> Result<MessageList, MailError>;

    /// Fetch a single message's full body.
    async fn fetch_message(&self, id: &MessageId) -> Result<MessageBody, MailError>;

    /// Fetch the bytes of one attachment.
    async fn fetch_attachment(
        &self,
        message: &MessageId,
        attachment: &AttachmentRef,
    ) -> Result<Vec<u8>, MailError>;

    // ---------- Write (flags, moves, deletes) ----------

    /// Add and remove flags on one or more messages atomically.
    async fn update_flags(
        &self,
        messages: &[MessageId],
        add: MessageFlags,
        remove: MessageFlags,
    ) -> Result<(), MailError>;

    /// Move messages to a different folder.
    async fn move_messages(
        &self,
        messages: &[MessageId],
        target: &FolderId,
    ) -> Result<(), MailError>;

    /// Permanently delete messages (expunge). Usually called only on Trash.
    async fn delete_messages(&self, messages: &[MessageId]) -> Result<(), MailError>;

    // ---------- Compose / Send ----------

    /// Save a draft. Returns the backend-assigned ID.
    async fn save_draft(&self, raw_rfc822: &[u8]) -> Result<MessageId, MailError>;

    /// Submit a message for delivery. IMAP backends send via SMTP+XOAUTH2;
    /// JMAP backends use `EmailSubmission/set`. Returns the ID of the sent
    /// message in the Sent folder, if the backend puts it there.
    async fn submit_message(
        &self,
        raw_rfc822: &[u8],
    ) -> Result<Option<MessageId>, MailError>;

    // ---------- Live sync ----------

    /// Subscribe to real-time change notifications. IMAP uses IDLE; JMAP uses
    /// EventSource. The stream yields until the returned handle is dropped.
    fn watch(&self) -> BoxStream<'static, BackendEvent>;
}

#[derive(Debug, Clone)]
pub struct MessageList {
    pub messages: Vec<MessageHeaders>,
    pub new_state: SyncState,
    /// IDs of messages that were removed since `since`, if a delta was requested.
    pub removed: Vec<MessageId>,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    MessageAdded { folder: FolderId, id: MessageId },
    MessageChanged { folder: FolderId, id: MessageId },
    MessageRemoved { folder: FolderId, id: MessageId },
    FolderChanged { folder: FolderId },
    ConnectionLost,
    ConnectionRestored,
}
```

### Design notes on MailBackend

1. **Everything is async.** Both IMAP and JMAP are network protocols; sync APIs would force a blocking runtime and leak implementation detail.
2. **Batch updates.** `update_flags`, `move_messages`, `delete_messages` take slices. IMAP can batch these in one command via UID ranges; JMAP batches in one `Email/set` call. A one-message-at-a-time API would perform badly on both.
3. **No threading here.** The backend returns flat messages with a `thread_id` hint where the server provides one (Gmail's X-GM-THRID, JMAP's thread id). Threading reconciliation across servers that don't expose thread ids happens in a separate `crates/sync` module.
4. **No search.** Backends implement what they can do natively, but unified search runs locally against Tantivy (§5.1). Server-side search can be added later as an optional method.
5. **`submit_message` returns `Option<MessageId>`.** IMAP-style SMTP doesn't reliably tell you where in the Sent folder the message ended up; JMAP does. The caller handles both.

## DbConn Trait

Lives in `crates/storage/src/conn.rs`. All repository code depends on `&dyn DbConn`, never on `turso::Connection` directly.

```rust
use async_trait::async_trait;

#[async_trait]
pub trait DbConn: Send + Sync {
    async fn execute(&self, sql: &str, params: Params<'_>)
        -> Result<u64, StorageError>;

    async fn query(&self, sql: &str, params: Params<'_>)
        -> Result<Vec<Row>, StorageError>;

    async fn query_one(&self, sql: &str, params: Params<'_>)
        -> Result<Row, StorageError>;

    async fn query_opt(&self, sql: &str, params: Params<'_>)
        -> Result<Option<Row>, StorageError>;

    async fn begin<'a>(&'a self) -> Result<Box<dyn Tx + 'a>, StorageError>;
}

#[async_trait]
pub trait Tx: Send {
    async fn execute(&mut self, sql: &str, params: Params<'_>)
        -> Result<u64, StorageError>;

    async fn query(&mut self, sql: &str, params: Params<'_>)
        -> Result<Vec<Row>, StorageError>;

    async fn commit(self: Box<Self>) -> Result<(), StorageError>;
    async fn rollback(self: Box<Self>) -> Result<(), StorageError>;
}

/// Parameters for prepared statements. Thin wrapper over a Vec of values
/// that avoids exposing Turso's parameter type.
pub struct Params<'a>(pub Vec<Value<'a>>);

#[derive(Debug, Clone)]
pub enum Value<'a> {
    Null,
    Integer(i64),
    Real(f64),
    Text(&'a str),
    OwnedText(String),
    Blob(&'a [u8]),
    OwnedBlob(Vec<u8>),
}

/// A row with typed column accessors. Keep this surface narrow.
pub struct Row { /* opaque */ }

impl Row {
    pub fn get_i64(&self, col: &str) -> Result<i64, StorageError> { todo!() }
    pub fn get_str(&self, col: &str) -> Result<&str, StorageError> { todo!() }
    pub fn get_blob(&self, col: &str) -> Result<&[u8], StorageError> { todo!() }
    pub fn get_optional_i64(&self, col: &str) -> Result<Option<i64>, StorageError> { todo!() }
    // ... etc.
}
```

### Design notes on DbConn

1. **No query-builder DSL.** SQL strings live in the repository modules as `const` string literals. This keeps the dependency surface small and the queries readable.
2. **Compile-time checked queries?** Turso doesn't have a sqlx-style compile-time check yet. For Phase 0 we write careful hand-rolled queries; if Turso gains compile-time check support we adopt it. If not, we grow our own with a test that runs every `const QUERY: &str` through `EXPLAIN` at build time.
3. **Transactions are explicit.** `begin → commit | rollback`. No closure-based auto-commit. The reason: async closures + transactions have lifetime issues that explicit scopes avoid.
4. **`Value` owns or borrows.** `Text(&str)` for static/temporary strings, `OwnedText(String)` for things built on the fly. Saves allocations in hot paths.

## EmailRenderer Trait

Lives in `crates/core/src/renderer.rs`. The Servo impl lives in `crates/renderer/`.

```rust
pub trait EmailRenderer: Send {
    /// Render sanitized HTML into the renderer's surface. The surface is
    /// assumed to already be attached to the host window by the constructor.
    /// Returns a handle used to identify the current render (for teardown /
    /// event routing).
    fn render(&mut self, sanitized_html: &str, policy: RenderPolicy) -> RenderHandle;

    /// Register a callback for link-click events. The URL passed to the
    /// callback has already been cleaned per §4.5 layer 4 (trackers stripped,
    /// redirects unwrapped).
    fn on_link_click(&mut self, cb: Box<dyn FnMut(url::Url) + Send + 'static>);

    /// Clear the current render. The next call to `render` creates a fresh
    /// surface state — nothing persists across render calls.
    fn clear(&mut self);

    /// Tear down the renderer and release OS resources. After this call, the
    /// renderer must not be used.
    fn destroy(&mut self);
}

#[derive(Debug, Clone)]
pub struct RenderPolicy {
    /// Whether to load external images. When false, placeholders are shown.
    pub allow_remote_images: bool,
    /// Used by the adblock pass to decide block vs. allow. Even when `true`,
    /// filter-list matches still block.
    pub sender_is_trusted: bool,
    /// Color scheme for CSS color-scheme media query.
    pub color_scheme: ColorScheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderHandle(pub u64);
```

### Design notes on EmailRenderer

1. **The renderer does not sanitize.** Sanitization (ammonia) and filter-list matching (adblock) happen in a separate pipeline stage before `render` is called. The renderer takes sanitized input and renders it as-is.
2. **The renderer does not clean links.** URL cleaning happens on the callback path before the system browser is invoked. The renderer just reports "user clicked this URL," and the surrounding code runs it through the cleaner.
3. **One renderer per reader pane.** Not one per message. `clear` + `render` is the per-message lifecycle. `destroy` is once at shutdown.
4. **Servo-specific details don't leak.** No `servo::WebView` in the signature. If we swap renderers in the future (Blitz post-v1), the trait doesn't change.

## Module Conventions

- Every crate exports its public surface from `lib.rs` with explicit `pub use`. No wildcard re-exports across crate boundaries.
- All async traits use `#[async_trait]`. No native-async-trait syntax until it stabilizes across all crates we use.
- `#[non_exhaustive]` on every public enum and struct unless there's a strong reason not to. Cheap future-proofing.
- Test-only types live in `#[cfg(test)]` modules or `tests/common/`. No "test helpers" in public APIs.

## What's Intentionally Not Here

- **The sync engine.** Lives in `crates/sync`. Depends on `MailBackend` + `DbConn` but isn't a trait itself — it's the concrete orchestrator.
- **The search index.** Tantivy has its own native API, wrapped in `crates/search` without a trait abstraction. Tantivy is the one true search path; a trait would be overhead with no swap-out story.
- **The adblock engine and URL cleaner.** These are pure functions over strings, not traits. `crates/privacy/src/{filter.rs,cleaner.rs}`.
- **OAuth flows.** Provider-specific data, not a shared trait. Each provider is a module in `crates/auth/src/providers/`.
