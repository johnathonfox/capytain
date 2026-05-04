# QSL Project Specification (corrected)

## Overview

**QSL** is an open-source desktop email client written in Rust. It targets Linux first (sole maintainer's daily-driver: CachyOS + NVIDIA + KWin Wayland); macOS and Windows runtime support are open v0.1 blockers. The project follows a local-first architecture: every protocol round-trip lands in a single SQLite-compatible file on disk, and the UI reads from the local store rather than waiting on the network.

The project was renamed from "Capytain" on 2026-04-26. The four-letter name is treated as opaque branding — there is no canonical expansion.

## Core Technology Stack

- **Language:** Rust (workspace, stable toolchain)
- **Frontend Framework:** **Dioxus 0.7** (signals + `use_resource`; assets registered with `asset!()` + `document::Stylesheet`, not `Dioxus.toml [web.resource]`)
- **Desktop Shell:** Tauri 2 — Rust host process, Dioxus UI compiled to wasm and loaded into the Tauri webview
- **Database:** Turso 0.5.3 (libSQL fork) — single local file at `~/.local/share/qsl/qsl.db`, no replication, no sync engine. The libsql `read_your_writes` toggle does not apply.
- **Search:** Turso 0.5.3's experimental `USING fts(...)` index method, which wraps Tantivy internally. Tantivy is **not** linked directly; the engine owns the index lifecycle. The FTS index is currently **disabled** in migration 0005 as a perf workaround (see Known Bottlenecks).
- **Mail Protocols:**
  - Gmail / Google Workspace → **IMAP** with X-GM-EXT-1 extensions (`X-GM-LABELS`, `X-GM-MSGID`); auth via OAuth2 + PKCE; no Gmail REST API in use
  - Fastmail → **JMAP**; auth via OAuth2 + PKCE; EventSource for push
- **Auth Storage:** OS keyring via `secret-service` (libsecret service `com.qsl.app`) for refresh tokens; access tokens are kept in memory only.

## Architectural Principles

1. **Local-first, single-file storage.** All metadata, headers, threads, contacts, drafts, and outbox state live in one Turso file. Two `TursoConn` handles open the same file: one for IPC reads (`state.db`), one for the sync engine's writes (`state.sync_db`). WAL mode keeps reads non-blocking.
2. **On-demand body hydration.** History sync persists headers only. Full RFC 5322 bodies are fetched lazily via `messages_get` + `fetch_raw_message` and cached as blobs under `~/.local/share/qsl/blobs/`.
3. **Reactive UI via Dioxus signals.** The sync engine emits `sync_event` and `history_sync_progress` events; UI components subscribe and re-fetch through Tauri IPC. Signal writes during render are forbidden (panics the wasm bundle silently); use `use_effect` for derived state.
4. **Forward-only schema migrations** at `crates/storage/migrations/NNNN_*.sql`, applied per-connection on open. Multi-statement files are split on `;` (Turso 0.5.3 quirk: multi-statement transactions split incorrectly without manual splitting).

## Performance & Optimization Rules

### Database (Turso 0.5.3)

- **Pragmas at open** (`TursoConn::open`): `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`, `foreign_keys=ON`. The FK pragma is per-connection and was missing for months; cascade deletes silently no-op'd until migration 0011 + the pragma landed together.
- **Bulk inserts:** use `messages::batch_insert_skip_existing` (one multi-row `INSERT ... VALUES (...), (...), ...` statement per group of up to 1500 rows, all groups inside a single transaction). This replaced a per-row `tx.execute` loop that paid Turso's per-statement dispatch cost N times per chunk.
- **Placeholder ceiling:** SQLite's `SQLITE_MAX_VARIABLE_NUMBER` is 32,766. At 20 columns per row that caps a single multi-row INSERT at 1638 rows; we use 1500 for headroom.
- **Quirks to know about:**
  - `VACUUM` panics the engine in 0.5.3; `VACUUM INTO` likewise. Reclaim leaked pages by `mailcli reset --yes` + re-add.
  - `foreign_key_check` pragma is unsupported.
  - `tx.execute` rejects statements that return rows (e.g. `SELECT 1`) — use `query` for those.
  - Search must use `fts_match(...)` / `fts_score(...)` table-level functions, **not** `WHERE table MATCH 'query'`.

### Search

- **Currently disabled.** The `messages_fts_idx` `CREATE INDEX` is commented out in migration 0005. While enabled, every `INSERT INTO messages` paid a ~250ms per-row Tantivy index update under SHM rendering — search throughput was net-negative for a write-heavy mailbox.
- **Restore plan:** drop/recreate the FTS index around bulk passes (`pull_history`, fresh-folder `sync_folder`), keep it live for incremental writes. `mailcli doctor --rebuild-fts` already does the drop/recreate.
- **No vector embeddings.** Hybrid / semantic search is not implemented and not in scope for v0.1.

### Sync Engine

- **Header-only history backfill.** `pull_history` issues IMAP UID FETCH for envelope + flags + INTERNALDATE, never bodies. Chunk size: 1500 messages per fetch (post-multi-row-INSERT change). Per-account serialization via a `tokio::Mutex<()>` keyed by `AccountId` — Gmail's IMAP session can only run one command at a time.
- **Live sync (`sync_folder`):** delta-based via `CONDSTORE` / `CHANGEDSINCE`. Falls back to full mailbox scan + reconcile prune for backends without modseq.
- **Throttle handling:** fixed 30-second backoff (`THROTTLE_RECOVERY_MS`) on chunk failure, capped retries (`MAX_CHUNK_RETRIES`). Not exponential.
- **IDLE / push:** IMAP IDLE watchers per account+folder, JMAP EventSource subscriptions for Fastmail. State transitions logged at info via `qsl_desktop::imap_idle` / `qsl_desktop::jmap_push`.

### UI

- **Dioxus signal hygiene.** Never `.set()` a signal inside `rsx!` — panics the wasm bundle, manifests as a blank webview with no error in DevTools. Use `use_effect` for "publish derived state up" patterns.
- **Webkit2gtk repaint cost.** Smooth CSS animations (e.g. `opacity` interpolation on `ease-in-out`) force webkit2gtk's compositor into per-frame SHM buffer allocation when `WEBKIT_DISABLE_DMABUF_RENDERER=1` (forced for the NVIDIA + Wayland blank-webview workaround). One small animated dot was driving the entire page at 60fps and burning 25% CPU. Use `steps()` timing functions for status indicators.
- **IPC payload shape:** `messages_list` currently returns full `MessageHeaders`. For very large folders this is a real serialize cost on every `sync_event`; refactoring to ID-only + lazy `messages_get_batch` is on the backlog.

## Current Project Status / Known Bottlenecks

- **History-sync throughput:** dominated by IMAP UID FETCH wall-clock (~30-80s per 500-msg chunk on Gmail; per-fetch latency varies by Gmail rate-limit state). Per-row INSERT cost was the previous bottleneck — fixed by multi-row VALUES + chunk-size bump from 500 → 1500. First-chunk latency increases proportionally (~30-45s before any progress shows) but total time drops 3x.
- **FTS search dead.** Disabled until we land drop/recreate-around-bulk-syncs.
- **Live-sync `apply_chunk` still per-row.** Suspected source of a "slow tx_execute every ~2s at 280-360ms" pattern observed during a recent history pull. Multi-row helper exists; not yet wired into the live-sync path.
- **macOS / Windows runtime untested.** v0.1 blocker.
- **No virtualization on the message list.** 100k-row folders will not scroll at 60fps until this lands; scrolling stalls the webview during sync_event-driven re-renders.

## Development Workflow

- **Logging:** `tracing` with structured fields throughout. Default filter: `warn,qsl_*=info,mailcli=info`. Telemetry slow-op watchdog (`qsl_telemetry::slow::time_op!`) emits warnings at thresholds keyed by subsystem (`qsl::slow::db`, `qsl::slow::imap`, `qsl::slow::sync`, `qsl::slow::blob`).
- **CLI surface:** `mailcli` mirrors the desktop's protocol layer. `mailcli sync <email>`, `mailcli history-sync <email> --all`, `mailcli doctor --rebuild-fts`, `mailcli reset --yes`. Useful for testing perf without the UI in the loop.
- **Linting:** `cargo fmt --all` is gated by CI; clippy clean is required. Clippy/test passing locally doesn't catch fmt drift — run fmt pre-push.
- **Profiling:** ad-hoc `strace -c -f -p <pid>` for syscall histograms; periodic `ps -p <pid> -o pcpu` snapshots for CPU baselines. `tokio-console` is **not** wired up; would be a useful add for sync-engine task starvation diagnosis.
- **Dev-runtime gotchas:** `WEBKIT_DISABLE_DMABUF_RENDERER=1` forced on Linux in `main.rs` (NVIDIA hybrid stack returns EINVAL from libgbm under DMA-BUF). `tauri-plugin-window-state` saves x=0, y=0 on Wayland — only `SIZE | MAXIMIZED | FULLSCREEN` flags are honored.

## What this spec deliberately doesn't claim

- No "embedded replicas" — we don't replicate.
- No Gmail REST API.
- No vector / semantic search.
- No 60fps guarantee on 100k+ message lists (not implemented).
- No Apple Silicon optimization target — the maintainer's primary box is Linux + NVIDIA, and macOS support hasn't been smoke-tested.
