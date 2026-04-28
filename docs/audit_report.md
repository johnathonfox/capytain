# QSL Codebase Targeted Criticality and Performance Audit

## Executive Summary
This audit evaluated the QSL codebase against critical requirements, focusing on Priority 1 (Critical Bugs) and Priority 2 (Performance Bottlenecks). Several significant issues were identified that pose immediate risks to application stability, data integrity, and UI responsiveness.

---

## Priority 1: Critical Bugs

### 1. Data Loss (IMAP / SQLite Sync Errors)
- **Unbounded IMAP Queries:** In `crates/imap-client/src/backend.rs`, methods like `update_flags` and `move_messages` group messages by folder and concatenate their UIDs using `.join(",")`. There is no batch size limit for the constructed `UID STORE` or `UID MOVE` commands. Synchronizing a folder with thousands of flag changes or moving thousands of messages at once will exceed IMAP command length limits, resulting in silent protocol failures and dropped synchronizations (i.e., data loss).
- **Missing ROLLBACK on Transaction Drop:** In `crates/storage/src/turso_conn.rs`, `TursoTx` is implemented with a manual `BEGIN` because Turso 0.5.3 lacks native transaction handles. The `Drop` implementation logs a warning but **does not issue a `ROLLBACK`**. If an operation fails and returns early (via `?`), the transaction drops, leaving `BEGIN` in flight. This completely wedges the database connection, preventing further syncs and ultimately causing data loss.
- **UIDNEXT Fallback Masking Server State:** In `list_messages` (`backend.rs`), the IMAP `uid_next` fallback is `mbox.uid_next.unwrap_or(1)`. If the server omits `UIDNEXT` on `SELECT`, defaulting to `1` causes an incorrect state boundary calculation, which could lead to missed messages or completely redundant fetches.

### 2. Security Leaks (Credential Exposure)
- **OAuth / Auth Flow Logging:** In `crates/auth/src/flow.rs` (line 106), `debug!(%auth_url);` is logged during the browser authorization step. Depending on the OAuth2 provider, this URL can contain sensitive authorization codes, CSRF states (`state`), or PKCE verifiers that are emitted in plaintext logs.

### 3. Hard Crashes (Segmentation Faults / Thread Panics)
- **Servo Bridge Thread Panics:** `crates/renderer/src/servo.rs` heavily relies on `.expect()` and `.unwrap()` in critical paths.
  - Lines 186 & 312: `*self.cursor_cb.lock().expect("cursor_cb poisoned") = Some(cb);`. If the mutex is poisoned by an earlier panic on another thread, this will systematically panic the bridge.
  - Lines 251 & 334: Includes `expect("SERVO_RUNTIME initialized just above")` and `expect("about:blank is a valid URL")`.
- **Link Cleaner Parsing Panics:** In `crates/renderer/src/link_cleaner.rs`, the test helper function `clean(s: &str)` uses `.unwrap()`. More critically, if any production routing uses this pattern for `clean_outbound_url(Url::parse(s).unwrap()).into()`, clicking an invalid outbound URL inside the reader pane will immediately panic and hard-crash the desktop application. 

---

## Priority 2: Performance Bottlenecks

### 1. Blocking Calls & Threading Issues
- **Tokio Mutex Lock Contention:** In `apps/desktop/src-tauri/src/sync_engine.rs`, `sync_one_folder` locks the `sync_db` using an asynchronous mutex (`let db = state.sync_db.lock().await;`) and holds the lock for the entirety of the `qsl_sync::sync_folder` call. This blocks any other concurrent task trying to read from or write to the database (including UI interaction) until the potentially long-running folder sync completes.

### 2. Inefficient SQLite Queries
- **Missing Essential PRAGMAs:** In `crates/storage/src/turso_conn.rs`, `PRAGMA journal_mode=WAL` is enabled, but it lacks **`PRAGMA synchronous=NORMAL;`**. Under WAL mode, default `synchronous=FULL` forces `fsync()` on every single commit. This significantly degrades bulk insert performance during syncs. Additionally, `PRAGMA busy_timeout` is missing, which can cause `SQLITE_BUSY` exceptions under concurrent UI and Sync access.
- **Missing UID Index:** The `messages` table in `crates/storage/migrations/0001_initial.sql` has indices for `folder_id` and `thread_id`, but lacks a covering index on `uid` (or `(folder_id, uid)`). Sync operations that apply flag deltas or process deletions must scan the entire folder's messages to locate specific `uid`s, resulting in linear $O(N)$ query degradation as mailboxes grow.

### 3. Tantivy Indexing Bottlenecks
- **Inline FTS Commits (Micro-Stutters):** Turso's `experimental_index_method(true)` dynamically builds a Tantivy-backed full-text search index (`USING fts(...)` per `0005_search_fts.sql`). However, Turso 0.5.3 applies Tantivy FTS commits synchronously alongside SQLite inserts. Because `qsl_sync` commits individual messages (or small batches) over the `TursoTx` transaction, the Tantivy index undergoes extremely frequent merges and commits, leading to perceptible micro-stutters and main-thread blocking when inserting new downloaded emails.

---

> [!WARNING] 
> The `TursoTx` missing `ROLLBACK` bug and the unbounded IMAP `UID STORE`/`UID MOVE` commands pose the greatest immediate threat to long-term synchronization reliability. These should be hot-fixed immediately to prevent persistent wedges in the database connection.
