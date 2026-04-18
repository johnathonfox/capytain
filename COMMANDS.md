# IPC Commands

This document enumerates every Tauri command the Dioxus UI invokes on the Rust core. It defines the contract between `apps/desktop/ui/` (UI) and `apps/desktop/src-tauri/` (shell) + `crates/*` (core).

Conventions:

- All commands are `async fn` on the Rust side and `await invoke("name", args)` on the UI side.
- Inputs and outputs are `serde`-serializable types defined in `crates/ipc`.
- Every command returns `Result<T, IpcError>`. `IpcError` is a display-safe wrapper over `MailError` / `StorageError` that does not leak backend-specific detail or credentials.
- Events flow the other direction (core → UI) via Tauri's event system, listed at the bottom.
- All command names use `snake_case` with a domain prefix: `accounts_*`, `folders_*`, `messages_*`, etc. This groups related commands in autocomplete and logs.

## Error Shape

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcError {
    pub kind: IpcErrorKind,
    pub message: String,
    /// Optional context for the UI to route the error (e.g. which account failed).
    pub account_id: Option<AccountId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcErrorKind {
    Network,
    Auth,           // UI should prompt re-auth
    NotFound,
    Permission,
    Protocol,
    Storage,
    Cancelled,
    Internal,
}
```

## Accounts

| Command | Input | Output |
|---|---|---|
| `accounts_list` | — | `Vec<Account>` |
| `accounts_add_oauth` | `{ provider: OAuthProvider, email_hint: Option<String> }` | `Account` (new, stored, authorized) |
| `accounts_remove` | `{ id: AccountId }` | `()` |
| `accounts_get_status` | `{ id: AccountId }` | `AccountStatus` |
| `accounts_set_display_name` | `{ id: AccountId, display_name: String }` | `()` |

```rust
#[derive(Serialize, Deserialize)]
pub enum OAuthProvider {
    Gmail,
    Fastmail,
    // Microsoft365 — Phase 5
    // CustomOAuth { authorize_url: String, token_url: String, ... } — Phase 6
}

#[derive(Serialize, Deserialize)]
pub struct AccountStatus {
    pub online: bool,
    pub last_sync: Option<DateTime<Utc>>,
    pub last_error: Option<IpcError>,
    pub is_syncing: bool,
}
```

**`accounts_add_oauth` behavior:** the backend spawns a loopback HTTP server on an ephemeral port, opens the provider's authorization URL in the default browser, waits for the redirect with the authorization code, exchanges it for tokens, persists the refresh token in the OS keychain, and returns the newly created `Account`. The UI shows a spinner during this and listens for the `auth_complete` or `auth_failed` event.

## Folders

| Command | Input | Output |
|---|---|---|
| `folders_list` | `{ account: AccountId }` | `Vec<Folder>` |
| `folders_list_unified` | — | `Vec<UnifiedFolder>` |
| `folders_refresh` | `{ account: AccountId, folder: Option<FolderId> }` | `()` (fires `folder_updated` events) |

```rust
#[derive(Serialize, Deserialize)]
pub struct UnifiedFolder {
    pub role: FolderRole,
    pub name: String,
    pub unread_count: u32,
    pub constituent_folders: Vec<FolderId>,
}
```

## Messages

| Command | Input | Output |
|---|---|---|
| `messages_list` | `{ folder: FolderId, limit: u32, offset: u32, sort: SortOrder }` | `MessagePage` |
| `messages_list_unified` | `{ role: FolderRole, limit: u32, offset: u32 }` | `MessagePage` |
| `messages_get` | `{ id: MessageId }` | `RenderedMessage` |
| `messages_mark_read` | `{ ids: Vec<MessageId>, read: bool }` | `()` |
| `messages_flag` | `{ ids: Vec<MessageId>, flagged: bool }` | `()` |
| `messages_move` | `{ ids: Vec<MessageId>, target: FolderId }` | `()` |
| `messages_archive` | `{ ids: Vec<MessageId> }` | `()` (moves to each account's Archive role folder) |
| `messages_delete` | `{ ids: Vec<MessageId> }` | `()` (moves to Trash; permanent delete happens in Trash only) |
| `messages_download_attachment` | `{ message: MessageId, attachment: AttachmentRef, target_dir: Option<PathBuf> }` | `PathBuf` |

```rust
#[derive(Serialize, Deserialize)]
pub enum SortOrder {
    DateDesc,
    DateAsc,
    UnreadFirst,
}

#[derive(Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<MessageHeaders>,
    pub total_count: u32,
    pub unread_count: u32,
}

/// What the UI gets back when a user opens a message. Contains sanitized HTML
/// (already through ammonia + filter lists), ready to hand to the Servo renderer.
#[derive(Serialize, Deserialize)]
pub struct RenderedMessage {
    pub headers: MessageHeaders,
    pub sanitized_html: Option<String>,
    pub body_text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub sender_is_trusted: bool,
    pub remote_content_blocked: bool,
}
```

## Threads

| Command | Input | Output |
|---|---|---|
| `threads_get` | `{ id: ThreadId }` | `Vec<MessageHeaders>` |
| `threads_mark_read` | `{ id: ThreadId, read: bool }` | `()` |
| `threads_archive` | `{ id: ThreadId }` | `()` |

## Compose

| Command | Input | Output |
|---|---|---|
| `compose_new` | `{ account: AccountId }` | `DraftId` |
| `compose_reply` | `{ message: MessageId, reply_all: bool }` | `DraftId` |
| `compose_forward` | `{ message: MessageId }` | `DraftId` |
| `compose_load` | `{ draft: DraftId }` | `DraftData` |
| `compose_save` | `{ draft: DraftId, data: DraftData }` | `()` |
| `compose_add_attachment` | `{ draft: DraftId, path: PathBuf }` | `AttachmentRef` |
| `compose_remove_attachment` | `{ draft: DraftId, attachment: AttachmentRef }` | `()` |
| `compose_send` | `{ draft: DraftId }` | `()` (queues to outbox; fires `outbox_updated`) |
| `compose_discard` | `{ draft: DraftId }` | `()` |

```rust
#[derive(Serialize, Deserialize)]
pub struct DraftData {
    pub account_id: AccountId,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub in_reply_to: Option<MessageId>,
    pub attachments: Vec<AttachmentRef>,
}
```

## Search

| Command | Input | Output |
|---|---|---|
| `search_execute` | `{ query: String, scope: SearchScope, limit: u32 }` | `Vec<SearchResult>` |
| `search_saved_list` | — | `Vec<SavedSearch>` |
| `search_saved_add` | `{ name: String, query: String, scope: SearchScope }` | `SavedSearchId` |
| `search_saved_remove` | `{ id: SavedSearchId }` | `()` |

```rust
#[derive(Serialize, Deserialize)]
pub enum SearchScope {
    AllAccounts,
    Account(AccountId),
    Folder(FolderId),
    Unread,
}

#[derive(Serialize, Deserialize)]
pub struct SearchResult {
    pub headers: MessageHeaders,
    pub matched_snippet: String,  // text fragment with match highlighted
}
```

## Contacts

| Command | Input | Output |
|---|---|---|
| `contacts_autocomplete` | `{ prefix: String, limit: u32 }` | `Vec<Contact>` |
| `contacts_set_trusted` | `{ address: String, trusted: bool }` | `()` |
| `contacts_get_trusted_list` | — | `Vec<String>` (addresses) |

```rust
#[derive(Serialize, Deserialize)]
pub struct Contact {
    pub address: String,
    pub display_name: Option<String>,
    pub frequency: u32,
    pub trusted_for_remote_content: bool,
}
```

## Outbox

| Command | Input | Output |
|---|---|---|
| `outbox_list` | — | `Vec<OutboxItem>` |
| `outbox_retry` | `{ id: OutboxId }` | `()` |
| `outbox_cancel` | `{ id: OutboxId }` | `()` |

```rust
#[derive(Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: OutboxId,
    pub account_id: AccountId,
    pub kind: OutboxKind,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub enum OutboxKind {
    Send,
    FlagUpdate,
    Move,
    Delete,
}
```

## Sync

| Command | Input | Output |
|---|---|---|
| `sync_now` | `{ account: Option<AccountId> }` (None = all) | `()` (fires `sync_progress` events) |
| `sync_status` | — | `SyncStatus` |

```rust
#[derive(Serialize, Deserialize)]
pub struct SyncStatus {
    pub per_account: Vec<(AccountId, AccountStatus)>,
    pub global_is_syncing: bool,
}
```

## Settings

| Command | Input | Output |
|---|---|---|
| `settings_get` | — | `Settings` |
| `settings_update` | `{ patch: SettingsPatch }` | `()` |
| `settings_export_json` | — | `String` (JSON) |
| `settings_import_json` | `{ json: String }` | `()` |

```rust
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub remote_content_mode: RemoteContentMode,
    pub adblock_enabled: bool,
    pub adblock_filter_lists: Vec<FilterList>,
    pub link_cleaning_enabled: bool,
    pub notifications_enabled: bool,
    pub theme: Theme,
    pub keyboard_shortcuts: ShortcutSet,
    pub body_cache_size_mb: u32,
}

#[derive(Serialize, Deserialize)]
pub enum RemoteContentMode {
    AlwaysBlock,
    TrustedOnly, // default
    AlwaysAllow, // filter list still applies
}

#[derive(Serialize, Deserialize)]
pub enum Theme {
    System,
    Light,
    Dark,
}
```

## App Lifecycle

| Command | Input | Output |
|---|---|---|
| `app_get_version` | — | `AppVersion` |
| `app_check_updates` | — | `Option<UpdateInfo>` |
| `app_open_data_dir` | — | `()` (opens in OS file manager; for debug/forensics) |

```rust
#[derive(Serialize, Deserialize)]
pub struct AppVersion {
    pub version: String,        // semver
    pub build: String,          // git commit + dirty flag
    pub channel: String,        // stable | beta | nightly
    pub platform: String,       // e.g. "macos-aarch64"
}
```

## Events (core → UI)

Fired on Tauri's event system. UI subscribes via `listen("event_name", |e| ...)`. Payloads are the listed types, serialized as JSON.

| Event | Payload | Fired when |
|---|---|---|
| `message_added` | `{ folder: FolderId, id: MessageId }` | New message arrives via IDLE / EventSource |
| `message_changed` | `{ folder: FolderId, id: MessageId }` | Flags, labels, or other metadata changed server-side |
| `message_removed` | `{ folder: FolderId, id: MessageId }` | Message deleted or moved out server-side |
| `folder_updated` | `{ folder: FolderId, unread_count: u32 }` | Folder counts changed |
| `sync_progress` | `{ account: AccountId, fraction: f32, phase: String }` | Periodic during `sync_now` |
| `sync_complete` | `{ account: AccountId, added: u32, removed: u32, duration_ms: u64 }` | End of a sync pass |
| `auth_required` | `{ account: AccountId, reason: String }` | Refresh token expired or revoked; UI should prompt re-auth |
| `outbox_updated` | `{ id: OutboxId, status: OutboxStatus }` | Outbox item state changed (sent, failed, retrying) |
| `notification` | `{ account: AccountId, kind: NotificationKind, title: String, body: String }` | New mail worth notifying, error worth surfacing |
| `app_update_available` | `{ version: String, release_notes_url: String }` | Auto-updater found a new version |

## Commands Intentionally Not Here (Deferred)

- **Snooze / send-later** — Phase 7+.
- **Rules and filters** — Phase 3 will add these; see `DESIGN.md` §3.2.
- **Per-account identities and aliases** — Phase 7+.
- **CardDAV contact operations** — Phase 7+.
- **Plugin system** — Phase 8+; will add its own command namespace (`plugin_*`).

## Naming Anti-Patterns to Avoid

- Don't overload verbs: `messages_update` is vague; split into `messages_mark_read`, `messages_flag`, `messages_move`.
- Don't expose backend-specific concepts: no `imap_*`, no `jmap_*` commands at the IPC layer.
- Don't return `Option<T>` for "not found" — return `Err(IpcError { kind: NotFound, .. })` so the UI error path is consistent.
- Don't nest command payloads more than one level. If a command needs a big object, define a struct and use it — don't inline nested structs.
