<!--
SPDX-FileCopyrightText: 2026 Capytain Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Phase 1 — Read Path, Gmail + Fastmail

**Status:** draft for review. Timeframe per `DESIGN.md §11`: weeks 7–15 (9 weeks).

Phase 1 turns Capytain from "the OAuth + IMAP + Servo scaffolding is proven" into "opens real email from two providers and renders it safely." The hard work is no longer proving a stack is viable (Phase 0 did that); it's building the correctness + polish layers that make a mail client trustable. Every week below is read-side — write path is Phase 2.

---

## Entry state (what Phase 0 ships on main)

Everything below is live on main at the start of Phase 1. Phase 1 tasks never reprove these:

- **Workspace + tooling:** Cargo workspace, Tauri 2 shell, Dioxus UI built via `build.rs`, `cargo-deny`, `reuse lint`, GitHub Actions matrix (ubuntu-latest + windows-latest).
- **Storage:** Turso behind `DbConn`, schema v1 with migrations, blob store for raw `.eml` payloads.
- **Auth:** OAuth2 + PKCE via `capytain-auth`; Gmail + Fastmail profiles; confidential-client (`client_secret`) support; keyring-backed refresh tokens; `.env` build-time credential loading.
- **Protocols:** `MailBackend` trait with two implementations — `capytain-imap-client` (IMAP with CONDSTORE detection, XOAUTH2 login, envelope fetching, minimal delta sync) and `capytain-jmap-client` (JMAP session + Mailbox/Email/get/changes skeleton). MIME parsing via `capytain-mime` with RFC 2047 header decoding.
- **Servo reader pane:** embedded as a GTK child widget on Linux (via XWayland routing + Mesa llvmpipe fallback for NVIDIA + KWin Wayland boxes). macOS / Windows code shipped UNVERIFIED; runtime validation hardware-gated. Reader pane renders HTML supplied by the UI via the `reader_render` Tauri command.
- **mailcli:** `auth add/list/remove`, `list-folders`, `list-messages`, `sync` — end-to-end-validated against real Gmail.

**Gmail smoke test evidence (real account):** ~58 messages synced to Turso in ~800ms; `list-folders` shows Gmail-specific roles (`[Gmail]/All Mail`, `[Gmail]/Sent Mail`, etc.) correctly tagged; RFC 2047-encoded subjects decoded; Servo reader pane renders composed-from-body HTML when a message is selected.

**Open deferred from Phase 0** (will be closed during Phase 1 as their prerequisites come due):

- Fastmail OAuth + JMAP smoke test — prereq for Week 11.
- macOS / Windows runtime validation — hardware-gated; slots in opportunistically.
- Ubuntu CI corpus test re-enablement — revisit if the week-7 sanitization work naturally surfaces a working headless rendering path.

---

## Objective

By end of Phase 1, the app:

1. Renders an arbitrary real email from **either** Gmail or Fastmail with HTML sanitization, remote-content blocking, link-click cleaning, and a reader pane that composes cleanly into the Tauri window.
2. Syncs folder state (UIDVALIDITY / HIGHESTMODSEQ on IMAP, state strings on JMAP) across process restarts and network drops — no duplicate messages, no stale flags.
3. Surfaces the system-defined mailboxes (INBOX, Sent, Drafts, Trash, Spam, Archive) with their roles regardless of backend, plus a unified inbox view across both accounts.
4. Shows conversation threading (References/In-Reply-To + subject fallback) with per-thread unread counts.
5. Applies local optimistic mutations (mark-read, flag, move, delete) that survive server round-trip.
6. Fires desktop notifications on new mail and shows unread counts in the tray / dock badge.

Write path (compose / send) is explicitly **Phase 2**.

---

## Week-by-week

Each week has a primary deliverable and a "done when" that's a concrete, observable behavior (same shape as `PHASE_0.md`'s weekly tables).

| Week | Task | Done when |
|---|---|---|
| 7 | **HTML sanitization pipeline.** Add `ammonia` to the workspace. Extend `capytain-mime` to expose `sanitize_email_html(raw_html: &str) -> String` with a conservative allowlist (presentational tags only — no scripts, forms, iframes, objects, event handlers, `javascript:` URLs). `capytain-storage::repos::messages` learns to extract `text/html` parts alongside `text/plain` when a body blob is cached. `messages_get` populates `RenderedMessage.sanitized_html` from that path; the Dioxus UI's `compose_reader_html` prefers `sanitized_html` over the plaintext fallback when present. | Selecting a real HTML email in the UI renders its HTML content (styled, no scripts) in the Servo pane, not its plaintext fallback. Manual XSS probes (`<script>`, `<img onerror>`, `javascript:` links) are neutralized. |
| 8 | **Remote-content blocking + sender opt-in.** Add `adblock` (brave's) to the workspace. Load EasyList + EasyPrivacy + uBlock Origin at startup into a shared `Arc<adblock::Engine>`. Servo's `WindowRenderingContext` gets a resource-load hook (or pre-render URL rewriter) that checks each external URL against the engine — matches get blocked unconditionally; non-matches are replaced with an inline placeholder unless the sender's address is on a per-sender opt-in list (new `remote_content_opt_ins` table + repo). Implement link-click URL cleaning (`utm_*`, `fbclid`, `gclid`, `mc_cid`, Mailchimp/SendGrid unwrapping) via `adblock`'s `$removeparam` rules plus a small handwritten redirect-unwrap list. The existing `on_link_click` callback in `ServoRenderer` is the seam. | A test marketing email with Mailchimp pixels renders with the pixels blocked (reader pane shows placeholders where tracking images lived); clicking a wrapped `http://click.list-manage.com/?u=...&id=...` link opens the destination with trackers stripped and no Mailchimp redirect hop. |
| 9 | **IMAP body fetching + delta sync completeness.** `capytain-sync` (new crate) owns the top-level sync loop. Per-folder sync: on first SELECT, cache `(UIDVALIDITY, HIGHESTMODSEQ, UIDNEXT)` in `folders`; on reconnect, UIDVALIDITY mismatch → full refetch, modseq delta → `UID FETCH … CHANGEDSINCE modseq` for flag changes. Message bodies lazy-fetched on demand via `fetch_message` and written to the blob store; `messages_get` triggers the fetch if `body_path` is null. CONDSTORE flag sync updates `flags_bitmap` in-place. | `mailcli sync` against Gmail picks up the last 500 INBOX messages with bodies on disk under `<data_dir>/blobs/...`. Toggle a flag server-side (mark-unread in Gmail web) → next `mailcli sync` reflects the change locally without a full refetch. |
| 10 | **IMAP IDLE push + reconnect.** Spawn one `tokio::task` per connected IMAP folder that issues `IDLE`, reads untagged updates (`EXISTS`, `EXPUNGE`, `FETCH`), and forwards events over a `tokio::sync::mpsc::Sender<SyncEvent>` into the sync engine. On socket drop: exponential backoff reconnect, then QRESYNC-style resume. IDLE refresh every 25 min per RFC 2177. New `capytain-ipc` event type `folder_updated` reaches the Dioxus UI via `tauri::Window::emit`; `MessageListPane` refetches when the event matches the current selection. | New message arriving in Gmail web appears in the Capytain inbox within ~5 seconds without any user action. Kill-9 the network + restore → connection reestablished + missing delta applied without duplicates. |
| 11 | **Fastmail OAuth + JMAP end-to-end.** Closes the Phase 0 deferred item. Register a Fastmail OAuth client, drop into `.env`, run `mailcli auth add fastmail` + `mailcli sync` + debug whatever surfaces. Then: implement `Email/changes` delta sync in `capytain-jmap-client` (state string round-trip through `SyncState.backend_state` column), implement EventSource push via `reqwest-eventsource` (or `eventsource-stream`) with the same `SyncEvent` channel as IMAP IDLE. The UI becomes backend-agnostic — both adapters push events through the same pipe. | Same-shape test as Week 10: new message arriving in Fastmail web lands in Capytain inbox via EventSource within ~5s. Flag changes round-trip. JMAP state strings survive process restarts. |
| 12 | **Folder navigation + unified inbox.** Full SPECIAL-USE role handling on IMAP (`\Inbox`, `\Sent`, `\Drafts`, `\Trash`, `\Junk`, `\Archive`, `\Flagged`) plus Gmail label awareness via `X-GM-LABELS` (labels as non-exclusive tags alongside the folder). JMAP natively returns roles on `Mailbox/get`. Sidebar shows per-account folder tree with roles badged; new top-level "Unified Inbox" node queries both accounts' INBOX-role folders and merges (sorted by date desc, paginated via a new `folders::query_unified_inbox` repo fn). | Sidebar renders two account trees with correct role icons for INBOX/Sent/Trash/etc. on both providers. Unified Inbox shows Gmail + Fastmail messages interleaved by date. Switching between folders doesn't trigger re-syncs. |
| 13 | **Threading.** New `threads` table (already sketched in `DESIGN.md §4.4`). Thread assembly pipeline runs after each message insert: look up thread by Message-ID of `In-Reply-To` → if hit, attach to that thread; else walk `References` in reverse, first hit wins; else subject-normalize (`"Re: "` / `"Fwd: "` strip + Unicode case fold + collapse whitespace) and attempt a subject+participants match within a 30-day window; else new thread. `messages_list` gains a `group_by_thread: bool` option; middle pane collapses threads by default with expand-on-click. | An email reply arriving to an existing Gmail thread shows up attached to that thread in Capytain (thread count increments, not a new row). Subject-renamed replies in a conversation ("Re: → (no subject)") still attach via References chain. |
| 14 | **Optimistic mutations.** New `outbox` table + repo. `messages_mark_read`, `messages_flag`, `messages_move`, `messages_delete` commands: apply locally immediately (update flags_bitmap / folder_id in Turso), insert an outbox row, return. A sync-engine worker drains the outbox: `STORE \Seen` on IMAP, `Email/set` on JMAP, retries with exponential backoff (max 5 attempts, then move to a dead-letter state and emit an event the UI shows as "Failed to sync"). Reconciliation: on conflict, server state wins for moves; last-write-wins per flag. | Marking a message read in Capytain updates Gmail within seconds; killing the app mid-flight leaves a pending outbox row that drains on next launch. Intentional 500 from a mock server produces a DLQ entry and a UI error banner, not silent data loss. |
| 15 | **Notifications + unread + final polish.** `tauri::notification` for new-mail from IDLE / EventSource events (respect per-account mute; sender not-in-contacts gets a muted badge only). Unread counts per folder (already tracked via `repos::messages::count_unread_by_folder`) surface in sidebar badges + tray icon count. Regression pass on the Phase 0 deferred macOS / Windows runtime targets if hardware is available — Phase 1 exit criteria don't require it, but a green pass here is the cleanest path to the 0.1 prerelease. | macOS notification drawer shows a new-mail banner when Gmail web sends a new message. Tray icon reads `Capytain (3)` when three unread messages are in the unified inbox. One full release-check pass of `cargo clippy --workspace --all-targets` + `cargo test --workspace` on main, no `#[ignore]` tests that weren't `#[ignore]`d before Phase 1 started. |

---

## Phase 1 Done

By end of week 15:

- Unified inbox across Gmail + Fastmail with live push updates from both.
- Threading, remote-content blocking, link-click cleaning, optimistic read-path mutations, notifications — all working end-to-end on real accounts.
- Phase 0 deferred items closed: Fastmail smoke validated; macOS + Windows runtime if hardware permits (otherwise still tracked in `PHASE_0.md`'s Deferred section).
- `DESIGN.md §1`'s "respects the user by default" promise is observable: no network traffic to third-party servers (verified via a quick tcpdump during a reader-pane session), no tracking pixels loading, no link trackers surviving the click cleaner.

At this point Phase 2 (write path: Compose, SMTP, `EmailSubmission/set`, drafts, outbox-of-sent) becomes a feature-build rather than a platform-build.

---

## Phase 1 Deliverables Summary

| Deliverable | Path |
|---|---|
| HTML sanitization pipeline | `crates/mime/src/sanitize.rs` |
| Remote-content blocking engine | `crates/renderer/src/adblock.rs` |
| Link-click cleaner | `crates/renderer/src/link_cleaner.rs` |
| Sync engine (IMAP + JMAP, shared event channel) | `crates/sync/` |
| Threading pipeline | `crates/storage/src/repos/threads.rs` |
| Optimistic mutation outbox | `crates/storage/src/repos/outbox.rs` |
| Notifications bridge | `apps/desktop/src-tauri/src/commands/notifications.rs` |
| Fastmail smoke log | `docs/dependencies/fastmail.md` (mirror of `docs/dependencies/turso.md`) |

---

## Phase 1 Non-Goals

Explicitly **not** in Phase 1 (these belong to Phase 2 or later):

- Compose / reply / forward / send — Phase 2.
- SMTP or JMAP `EmailSubmission/set` — Phase 2.
- Drafts sync — Phase 2.
- Full-text search (Tantivy) — Phase 3.
- Keyboard shortcuts + remapping UI — Phase 3.
- Rules / filters / Sieve — Phase 3 / Phase 7.
- PGP/MIME or S/MIME — Phase 7+.
- Attachment download UI — Phase 2 (infra lands here; surfacing it in the UI waits on compose).
- Multiple windows (pop-out reader) — never; design decision per `PHASE_0.md` Non-Goals.

---

## Open questions for during Phase 1

Decisions that are safe to defer into the weekly work but should be named explicitly:

- **Remote-content opt-in granularity.** Per-sender address (simple) vs per-domain (fewer entries, but coarser) vs a list + regex override table. Default proposal: per-address, with a "trust domain" button in the per-sender placeholder for one-click upgrade.
- **Thread subject-normalization locale.** Unicode case-fold is trivially correct but slow-ish on a per-insert path; a simple ASCII lowercase + whitespace collapse is probably fine for 99% of senders. Revisit if CJK subject threading breaks.
- **IDLE scheduling.** One task per folder per account scales to ~3 accounts × 5 folders = 15 long-lived sockets, which is plenty for v1. Post-v1: connection pooling if heavy users complain.
- **Notification mute granularity.** Per-account / per-folder / per-thread? Proposal: per-account + a "don't notify for this thread" thread-level override. Per-folder adds UI surface for limited value.
- **Corpus CI.** If week-7's sanitization work produces a reliably-renderable stub, revisit un-ignoring the corpus test on Ubuntu CI. Not a Phase 1 exit criterion.
