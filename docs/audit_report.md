# QSL Codebase Targeted Criticality and Performance Audit

> **Verification status (2026-04-28):** every claim below has been
> checked against the actual source. Annotations under each item
> mark the verdict — `[FIXED]`, `[FIXED — overstated]`, `[WRONG]`,
> or `[UNVERIFIABLE]` — with the relevant call site and rationale.
> The original audit text is kept verbatim for traceability.

## Executive Summary
This audit evaluated the QSL codebase against critical requirements, focusing on Priority 1 (Critical Bugs) and Priority 2 (Performance Bottlenecks). Several significant issues were identified that pose immediate risks to application stability, data integrity, and UI responsiveness.

---

## Priority 1: Critical Bugs

### 1. Data Loss (IMAP / SQLite Sync Errors)
- **Unbounded IMAP Queries:** In `crates/imap-client/src/backend.rs`, methods like `update_flags` and `move_messages` group messages by folder and concatenate their UIDs using `.join(",")`. There is no batch size limit for the constructed `UID STORE` or `UID MOVE` commands. Synchronizing a folder with thousands of flag changes or moving thousands of messages at once will exceed IMAP command length limits, resulting in silent protocol failures and dropped synchronizations (i.e., data loss).
  - **`[FIXED — severity overstated]`** Real concern, addressed by the `uid_chunks` helper (`backend.rs`) — `update_flags`, `move_messages`, and `delete_messages` all chunk at 1000 UIDs / 4 KB joined-bytes, whichever cap fires first. The "silent data loss" framing was wrong: `async-imap` propagates protocol errors visibly, so an oversized command would have failed with a server error, not a silent drop. Bulk action would have surfaced an error to the user; chunking eliminates that failure mode entirely.
- **Missing ROLLBACK on Transaction Drop:** In `crates/storage/src/turso_conn.rs`, `TursoTx` is implemented with a manual `BEGIN` because Turso 0.5.3 lacks native transaction handles. The `Drop` implementation logs a warning but **does not issue a `ROLLBACK`**. If an operation fails and returns early (via `?`), the transaction drops, leaving `BEGIN` in flight. This completely wedges the database connection, preventing further syncs and ultimately causing data loss.
  - **`[FIXED]`** Real and confirmed. Fix is a poison flag (`Arc<AtomicBool>`) shared between `TursoConn` and live `TursoTx`. `Drop` sets the flag; the next `begin()` observes it, issues a recovery `ROLLBACK` to clear the orphaned transaction, then `BEGIN`s fresh. Two `#[tokio::test]`s lock the contract: a dropped tx doesn't wedge subsequent operations, a clean commit/rollback doesn't trigger spurious recovery.
- **UIDNEXT Fallback Masking Server State:** In `list_messages` (`backend.rs`), the IMAP `uid_next` fallback is `mbox.uid_next.unwrap_or(1)`. If the server omits `UIDNEXT` on `SELECT`, defaulting to `1` causes an incorrect state boundary calculation, which could lead to missed messages or completely redundant fetches.
  - **`[DISREGARD — partly wrong]`** "Could lead to missed messages" is incorrect — defaulting to `1` means every UID ≥ 1 is included, so nothing is missed; only over-fetches happen. "Redundant fetches" is correct but rare in practice: every CONDSTORE-advertising server (which `connect_tls` requires) MUST return `UIDNEXT` per RFC 7162, so the fallback is essentially dead code. Not fixing — would require turning the silent fallback into a hard error, which risks breaking imperfect/stub IMAP servers without observable benefit on production providers (Gmail, Fastmail, iCloud, Outlook all return UIDNEXT).

### 2. Security Leaks (Credential Exposure)
- **OAuth / Auth Flow Logging:** In `crates/auth/src/flow.rs` (line 106), `debug!(%auth_url);` is logged during the browser authorization step. Depending on the OAuth2 provider, this URL can contain sensitive authorization codes, CSRF states (`state`), or PKCE verifiers that are emitted in plaintext logs.
  - **`[FIXED — severity overstated]`** "Credential exposure" is wrong. The auth URL contains: client_id (public), redirect_uri (public loopback), scopes (public), CSRF state (one-time nonce), PKCE code_*challenge* (the public half — the *verifier* never leaves this process). It does NOT contain authorization codes or tokens — those come back over the redirect, not in the outbound URL. PKCE specifically exists so leaked auth URLs are useless without the verifier. Still, logging cryptographic nonces is poor hygiene; replaced with structured fields (`provider`, `scopes` count, `host`, `has_email_hint`) that keep debugging useful without splatting nonces into logs.

### 3. Hard Crashes (Segmentation Faults / Thread Panics)
- **Servo Bridge Thread Panics:** `crates/renderer/src/servo.rs` heavily relies on `.expect()` and `.unwrap()` in critical paths.
  - Lines 186 & 312: `*self.cursor_cb.lock().expect("cursor_cb poisoned") = Some(cb);`. If the mutex is poisoned by an earlier panic on another thread, this will systematically panic the bridge.
  - Lines 251 & 334: Includes `expect("SERVO_RUNTIME initialized just above")` and `expect("about:blank is a valid URL")`.
  - **`[DISREGARD — mostly pedantic]`** Two of the four citations are unconditionally safe: `Url::parse("about:blank")` is deterministic, and the SERVO_RUNTIME expect is locally-invariant (the line just above sets it, with no early return between). The two mutex `.lock().expect("...poisoned")` calls only panic if a thread holding the lock previously panicked — that's the standard Rust idiom and the canonical "fail loud after a poison" behavior. Migrating to `parking_lot::Mutex` would eliminate the construct stylistically, but it's not a correctness bug. Not fixing.
- **Link Cleaner Parsing Panics:** In `crates/renderer/src/link_cleaner.rs`, the test helper function `clean(s: &str)` uses `.unwrap()`. More critically, if any production routing uses this pattern for `clean_outbound_url(Url::parse(s).unwrap()).into()`, clicking an invalid outbound URL inside the reader pane will immediately panic and hard-crash the desktop application. 
  - **`[DISREGARD — wrong]`** The cited `unwrap()` at `link_cleaner.rs:210` is inside `#[cfg(test)] mod tests { fn clean() { … } }` — a test helper, gated out of release builds. Production never parses URLs there: `servo/delegate.rs:108` receives an already-parsed `Url` from Servo's `NavigationRequest` and passes it directly to `clean_outbound_url(url: Url)`, which takes an owned `Url`, not a `&str`. There is no production parse-then-unwrap path. No bug, no fix.

---

## Priority 2: Performance Bottlenecks

### 1. Blocking Calls & Threading Issues
- **Tokio Mutex Lock Contention:** In `apps/desktop/src-tauri/src/sync_engine.rs`, `sync_one_folder` locks the `sync_db` using an asynchronous mutex (`let db = state.sync_db.lock().await;`) and holds the lock for the entirety of the `qsl_sync::sync_folder` call. This blocks any other concurrent task trying to read from or write to the database (including UI interaction) until the potentially long-running folder sync completes.
  - **`[DISREGARD — wrong]`** The audit's claim is that holding `sync_db.lock()` blocks "any other concurrent task ... including UI interaction." That's exactly what the **two-handle split** prevents. `AppState` holds two distinct `TursoConn`s on two distinct Mutexes: `state.db` for IPC (UI-side reads) and `state.sync_db` for the sync engine. WAL mode lets both connections operate on the same file concurrently. UI interaction goes through `state.db`, which is never touched by `sync_one_folder`. The lock does serialize sync tasks among themselves (one folder syncing at a time per account), which is intentional — concurrent writers on a single Turso connection would conflict — but it's not blocking UI. No fix needed; the project memory `project_db_split.md` documents this design.

### 2. Inefficient SQLite Queries
- **Missing Essential PRAGMAs:** In `crates/storage/src/turso_conn.rs`, `PRAGMA journal_mode=WAL` is enabled, but it lacks **`PRAGMA synchronous=NORMAL;`**. Under WAL mode, default `synchronous=FULL` forces `fsync()` on every single commit. This significantly degrades bulk insert performance during syncs. Additionally, `PRAGMA busy_timeout` is missing, which can cause `SQLITE_BUSY` exceptions under concurrent UI and Sync access.
  - **`[FIXED — Turso-implementation-dependent]`** `synchronous=NORMAL` and `busy_timeout=5000` added to `TursoConn::open`. Whether Turso's libSQL fork honors these is engine-version-dependent (libSQL has partial PRAGMA coverage); the calls tolerate failure with `let _ = ...`, so a non-supporting engine simply no-ops. SQLite-spec semantics: `synchronous=NORMAL` drops to one fsync per checkpoint instead of per commit (significant for bulk inserts under WAL); `busy_timeout=5000` makes concurrent writers wait 5 s instead of returning `SQLITE_BUSY` immediately. Either it speeds bulk inserts or it's a no-op — either way no regression risk.
- **Missing UID Index:** The `messages` table in `crates/storage/migrations/0001_initial.sql` has indices for `folder_id` and `thread_id`, but lacks a covering index on `uid` (or `(folder_id, uid)`). Sync operations that apply flag deltas or process deletions must scan the entire folder's messages to locate specific `uid`s, resulting in linear $O(N)$ query degradation as mailboxes grow.
  - **`[DISREGARD — wrong]`** The `messages` table has no `uid` column. `MessageId`s are encoded strings of the form `imap|<uidvalidity>|<uid>|<folder>` and stored in the `id` column — which is the table's `PRIMARY KEY`. All flag-delta and deletion lookups go by `MessageId` (i.e. by primary key), which is `O(log N)` on the B-tree. There is nothing to index, and no scan to avoid. No fix needed.

### 3. Tantivy Indexing Bottlenecks
- **Inline FTS Commits (Micro-Stutters):** Turso's `experimental_index_method(true)` dynamically builds a Tantivy-backed full-text search index (`USING fts(...)` per `0005_search_fts.sql`). However, Turso 0.5.3 applies Tantivy FTS commits synchronously alongside SQLite inserts. Because `qsl_sync` commits individual messages (or small batches) over the `TursoTx` transaction, the Tantivy index undergoes extremely frequent merges and commits, leading to perceptible micro-stutters and main-thread blocking when inserting new downloaded emails.
  - **`[UNVERIFIABLE — partly wrong on framing]`** The internal claim ("Turso 0.5.3 applies Tantivy FTS commits synchronously alongside SQLite inserts") is an engine-internals assertion I can't confirm without reading Turso source. Plausible — Turso's FTS is documented as experimental and lacks SQLite FTS5's tunables — but unverified from QSL alone. The "main-thread blocking" framing is wrong: sync runs on tokio workers, never on the GTK main thread, so any cost lands on background threads. If Turso's FTS commits genuinely are slow, the impact is slower sync rather than UI freezes. Not actioning until either Turso source is reviewed or a real-account profile shows the regression.

---

> [!WARNING] 
> The `TursoTx` missing `ROLLBACK` bug and the unbounded IMAP `UID STORE`/`UID MOVE` commands pose the greatest immediate threat to long-term synchronization reliability. These should be hot-fixed immediately to prevent persistent wedges in the database connection.
> 
> **`[ADDRESSED]`** Both flagged WARNING-level items shipped in the audit-cleanup branch — see annotations above.

## Verification summary (2026-04-28)

| # | Item | Verdict | Action |
|---|---|---|---|
| 1.1 | Unbounded IMAP UID set | Real, severity overstated | Fixed (`uid_chunks` + 5 tests) |
| 1.2 | TursoTx Drop ROLLBACK | Real | Fixed (poison flag + 2 tests) |
| 1.3 | UIDNEXT fallback | Partly wrong ("missed messages") | Disregard |
| 2.1 | OAuth auth_url logging | Real, severity overstated | Fixed (structured fields, no URL) |
| 3.1 | Servo `.expect()` | Mostly pedantic | Disregard |
| 3.2 | Link cleaner panic | Wrong (test-only path) | Disregard |
| 4.1 | sync_db lock blocks UI | Wrong (two-handle split prevents this) | Disregard |
| 4.2a | Missing PRAGMAs | Plausible, Turso-dependent | Fixed (best-effort) |
| 4.2b | Missing `(folder_id, uid)` index | Wrong (no `uid` column) | Disregard |
| 4.3 | Tantivy commit cadence | Unverifiable, framing wrong | Defer |
