<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# QSL — Product Backlog

> **Produced:** 2026-05-01  
> **Ground truth:** HEAD at commit `7ee237c` (`v0.0.1`)  
> **Audience:** maintainer + any Claude Code / OpenCode session picking up work  
> **Method:** codebase reviewed end-to-end against all design docs, open PRs, existing backlogs, and git history. Every claim in this doc is grounded in specific file paths or confirmed absent from the codebase.

---

## Context

QSL is a cross-platform, Rust-native desktop email client (Tauri 2 shell + Dioxus UI + Turso SQLite storage) that talks directly to Gmail (IMAP+SMTP+OAuth2) and Fastmail (JMAP+OAuth2) — no intermediary servers, no telemetry, tracker blocking and link cleaning built in. As of v0.0.1 the app is a real, functional email client: it authenticates, syncs, renders HTML email in a sandboxed `<iframe srcdoc>` (Servo was removed 2026-04-28 due to GPU compositor issues on AMD/NVIDIA Linux), supports full compose/reply/forward with SMTP and JMAP submission, draft sync, signatures, undo-send, file attachments, full-text search with operator parsing, a settings window, OAuth first-run flow, keyboard shortcuts, bulk multi-select, contact autocomplete, and history sync. Gmail is verified end-to-end; Fastmail is wired but awaits live-account validation. The project is at the boundary between "tech demo that compiles" and "daily driver you'd recommend" — Phase 3 polish work is the next logical milestone before a public v0.1 release.

---

## Themes

- **Security hardening** — two HIGH audit findings are unresolved in code before release
- **Fastmail parity** — the second v1 provider is wired but unverified against a real account
- **Thread UX completeness** — adjacent-grouping ships; the full stacked-card thread reader does not
- **MCP / agent integration** — killer differentiator; spec is complete; binary does not exist yet
- **Privacy trust indicators** — a client that claims privacy leadership needs DKIM/DMARC/SPF UX
- **Roadmap progression** — Microsoft 365 (Phase 5), Servo revival (post-v1), rules/filters (Phase 3)

---

## Competitive Differentiation

QSL's strongest differentiators vs. Apple Mail / Thunderbird / Mailspring / Spark that the backlog should protect and extend:

| Feature | Apple Mail | Thunderbird | Mailspring | Spark | **QSL** |
|---|---|---|---|---|---|
| No Chromium/Electron | ✓ (Obj-C) | ✗ | ✗ | ✗ | **✓ pure Rust** |
| No vendor servers | ✓ | ✓ | ✗ (analytics) | ✗ (sync server) | **✓** |
| Tracker blocking (built-in) | Partial | ✗ | ✗ | ✗ | **✓ unconditional** |
| Link cleaning on click | ✗ | ✗ | ✗ | ✗ | **✓** |
| JMAP first-class | ✗ | WIP | ✗ | ✗ | **✓** |
| OAuth2-only auth | macOS only | ✗ (allows passwords) | ✗ | ✗ | **✓** |
| Offline-first with outbox | ✓ | Partial | ✓ | ✓ | **✓** |
| MCP / AI agent interface | ✗ | ✗ | ✗ | ✗ | **planned** |
| Cross-platform render consistency | ✗ | ✗ | ✗ | ✗ | **planned (Servo)** |
| Open source (Apache 2.0) | ✗ | ✓ (MPL) | ✗ | ✗ | **✓** |

---

## Now — P0

These three items must land before calling QSL "v0.1." Two are unresolved HIGH security findings confirmed in code; one is the validation gate for the second of QSL's two announced v1 providers.

---

### P0 — Add Tauri Content-Security Policy

**Problem:** `apps/desktop/src-tauri/tauri.conf.json:23` has `"csp": null`. The entire Tauri webview — which runs the Dioxus WASM frontend and also renders sandboxed email bodies via `<iframe srcdoc>` — operates without a Content Security Policy. Any XSS vector that reaches the parent webview (e.g., a crafted `postMessage` from the sandbox, a future Dioxus component bug, or a crafted OAuth redirect) has unrestricted script execution and access to the Tauri IPC bridge. This is finding QSL-SEC-004 (Medium, security audit 2026-05-01) but its severity is understated: the app's primary attack surface is hostile email HTML, and a missing CSP on the container webview is a defense-in-depth failure adjacent to it.

**Hypothesis:** A strict-origin CSP on the parent webview costs nothing in normal operation (Dioxus is compiled WASM, not inline scripts) and eliminates the entire class of "script injection against the IPC bridge" attacks. The email iframe is already sandboxed; this closes the parent side of that boundary.

**User-facing value:** Users who read mail from untrusted senders (which is everyone) get a hardened container that cannot be turned against the app's own IPC even if an email body escapes the sandbox.

**Scope:**
- Add `"csp": "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src ipc: http://ipc.localhost; img-src 'self' data:; style-src 'self' 'unsafe-inline'"` (or equivalent) to `tauri.conf.json`
- Verify Dioxus WASM eval still works (requires `wasm-unsafe-eval`)
- Verify the `installReaderLinkListener` and `tauriListen` JS bridges still function
- Out of scope: the sandboxed email `<iframe srcdoc>` — it already has `sandbox="allow-scripts"` and a null origin

**Success signal:** `Content-Security-Policy` header present in webview devtools; the existing reader iframe CSP regression test (`feat/reader-csp`, PR #107) remains green; no WASM boot failures.

**Effort:** S  
**Priority:** P0 — unresolved security finding; one-line config change but needs care around WASM eval  
**Phase dependency:** None — blocking pre-v0.1  
**QSL principle risks:** None. This strengthens privacy/security posture.  
**Pointers:** `apps/desktop/src-tauri/tauri.conf.json:22-24`, `apps/desktop/ui/src/app.rs` (Tauri bridge JS), `docs/security/audit-2026-05-01.md:QSL-SEC-004`

---

### P0 — OAuth Token Lifecycle Hardening

**Problem:** Two HIGH findings from the 2026-05-01 security audit are confirmed unresolved in code:

1. **No token zeroization** (`crates/auth/src/tokens.rs:13-46`): `AccessToken` and `RefreshToken` are plain `String` wrappers with no `Drop` impl. On process crash or OOM kill, live token bytes survive in process heap and can appear in crash dumps, core files, or `ptrace`-style memory reads. The `zeroize` crate would clear them on drop.

2. **No token revocation on account removal** (`apps/desktop/src-tauri/src/commands/accounts.rs:275-314`): `accounts_remove` deletes the refresh token from the OS keychain but does not call the provider's revocation endpoint (Google: `https://oauth2.googleapis.com/revoke`; Fastmail: `https://api.fastmail.com/oauth/revoke`). A token stolen from the keychain *before* removal remains live and usable indefinitely after the user believes they've disconnected QSL.

**Hypothesis:** Token zeroization + revocation together close both HIGH findings from the audit. Neither change involves new dependencies that violate QSL principles (`zeroize` is `no_std`-compatible, pure Rust, and already in the ecosystem for exactly this use). Revocation adds one HTTPS request on account remove, which is acceptable given the security benefit.

**User-facing value:** When a user removes an account, it is actually disconnected — the token is dead at the provider, not just removed from local storage. And token bytes in memory are wiped on drop, making crash-dump exfiltration dramatically harder.

**Scope:**
- Add `zeroize` to `crates/auth/Cargo.toml`; implement `Zeroize` + `ZeroizeOnDrop` on `AccessToken` and `RefreshToken`
- Add `revoke_token(refresh_token: &RefreshToken, provider: ProviderKind)` to `crates/auth/src/flow.rs`; wire it into `accounts_remove` before keychain deletion
- Handle revocation failure gracefully (log + continue delete; don't block account removal on a network error)
- Tests: confirm `RefreshToken` bytes are zeroed on drop; confirm revocation endpoint is called in `accounts_remove` (mock the HTTP call)

**Success signal:** `valgrind --leak-check=full` or equivalent shows no token bytes in heap after drop. A unit test asserts the revocation HTTP call was made. Audit findings QSL-SEC-001 and QSL-SEC-002 can be marked resolved.

**Effort:** S  
**Priority:** P0 — HIGH audit findings; straightforward fix; blocking pre-v0.1  
**Phase dependency:** None  
**QSL principle risks:** None — strengthens the "private by default" principle  
**Pointers:** `crates/auth/src/tokens.rs:13-46`, `apps/desktop/src-tauri/src/commands/accounts.rs:275-314`, `crates/auth/src/flow.rs`, `docs/security/audit-2026-05-01.md:QSL-SEC-001,002`

---

### P0 — Validate Fastmail Live-Account End-to-End

**Problem:** QSL's v0.1 release claim is "Gmail and Fastmail as two first-class providers." The Gmail path is verified end-to-end against a real account. Fastmail is not: the Phase 0 deliverables table in `PHASE_0.md:166` explicitly lists "Fastmail OAuth + JMAP smoke test" as deferred — "code shipped but not exercised against a real Fastmail account." This means the JMAP backend (`crates/jmap-client/src/`), the `EmailSubmission/set` send path, and the draft-sync path have had zero live-server integration testing.

**Hypothesis:** If the Fastmail path has bugs, they are cheapest to find now (before v0.1 marketing) rather than after users file issues. The JMAP backend code is mature and has offline fixtures; a live smoke run will either confirm it or surface 1–3 fixable issues that are invisible in offline tests.

**User-facing value:** Users who switch to QSL from Fastmail get a client that actually works, not one that compiles but silently fails on their provider.

**Scope:**
- Register a Fastmail OAuth client (Settings → Privacy & Security → Connected apps in Fastmail) and set `QSL_FASTMAIL_CLIENT_ID` in the dev environment
- Run `mailcli auth add fastmail <email>`, then `mailcli sync <email>` against a real Fastmail account
- Verify: folder list, message list, open a message (MIME parsing), send a test email (JMAP `EmailSubmission/set`), check draft sync (`$draft` keyword)
- Document results in `PHASE_0.md` "Deferred" section; file bugs for anything that fails; remove the deferred entry when green
- Out of scope: fixing deep JMAP issues that require protocol-level changes (those would be separate PRs)

**Success signal:** `PHASE_0.md` "Deferred from Phase 0" section has "Fastmail OAuth + JMAP smoke test" removed or checked off. A real Fastmail user can add their account and read mail without errors.

**Effort:** S (mostly operational; code changes only if bugs surface)  
**Priority:** P0 — v0.1 release claim is false without this  
**Phase dependency:** Requires a Fastmail account and OAuth client registration (maintainer action)  
**QSL principle risks:** None — this is validation, not new code  
**Pointers:** `PHASE_0.md:162-168`, `crates/jmap-client/src/lib.rs`, `apps/mailcli/src/main.rs`, `docs/USER_TODO.md` (provider registration item)

---

## Next — P1

These seven features should land after v0.1 ships. They are the biggest gaps between "functional client" and "daily driver anyone would recommend."

---

### P1 — Full Stacked-Card Thread Reader

**Problem:** The message list collapses adjacent same-thread messages into one row (shipped, `apps/desktop/ui/src/threading.rs`), but clicking that row does not show a full stacked-card thread view. The reader pane shows only the single selected message. The `threading.rs` module explicitly calls out "PR-H2's stacked-card thread reader shows the holistic view" as a future task — there is no `messages_list_thread` IPC command, no `threads_get` command, and no multi-card reader component in the codebase. The data model is complete: `thread_id` is stored on every message, and `list_by_thread` can be trivially added to the messages repo.

**Hypothesis:** Without a thread reader, QSL feels like a 2005 email client. Threads are the fundamental unit of email communication. Every competitor — Gmail web, Apple Mail, Thunderbird, Spark — shows the full thread when you open a conversation. This is the single highest-impact UX gap remaining after v0.0.1.

**User-facing value:** Opening a conversation shows the full back-and-forth in one scrollable view, not just the latest message. Users can follow a thread without clicking through individual messages.

**Scope:**
- Add `messages_list_thread(thread_id: ThreadId) -> Vec<MessageHeaders>` IPC command in `apps/desktop/src-tauri/src/commands/messages.rs`; back it with `messages_repo::list_by_thread` query (the `messages_thread` index already exists per `0003_threading_columns.sql`)
- Add `ThreadReader` Dioxus component in `apps/desktop/ui/src/app.rs`: stacked cards, latest message expanded by default, click-to-expand/collapse
- `ReaderPaneV2` branches on `thread_id` — if a message has a thread with ≥2 members, show `ThreadReader`; otherwise show single-message view
- Reply/forward from any card seeds `in_reply_to` + `References` correctly
- Out of scope: flat-mode toggle (can be a Settings option later), search-result threading

**Success signal:** Opening a 3-message thread shows 3 cards, latest expanded. Clicking a collapsed card expands it. Reply from any card pre-fills headers correctly. Keyboard `j/k` works in the thread list.

**Effort:** M  
**Priority:** P1 — biggest remaining UX gap; data model is done; just needs UI  
**Phase dependency:** Phase 1 (threading data model) is complete  
**QSL principle risks:** None  
**Pointers:** `apps/desktop/ui/src/threading.rs`, `apps/desktop/ui/src/app.rs` (`ReaderPaneV2`), `apps/desktop/src-tauri/src/commands/messages.rs`, `crates/storage/src/repos/messages.rs`, `docs/plans/post-phase-2.md:PR-H1+PR-H2`

---

### P1 — MCP Read-Only Server Binary

**Problem:** No MCP server binary exists in the workspace. The spec (`docs/QSL_MCP_SERVER_SPEC.md`) is detailed and production-ready, covering tools, schemas, multi-account design, process model, and concurrency. What does not exist is an `apps/mailmcp/` binary or equivalent. This is QSL's most distinctive competitive advantage — no other desktop email client exposes a standards-compliant MCP interface for AI agents to read and act on mail.

**Hypothesis:** An MCP server turns QSL into infrastructure for AI workflows: summarize mail, triage inbox, draft replies, research senders. Claude Desktop and Claude Code can both consume it immediately via `~/.config/claude/claude_desktop_config.json`. For the privacy-focused user this is the right model — the AI reads mail through QSL's sanitized, cached, policy-controlled layer rather than connecting directly to Gmail/Fastmail with raw OAuth tokens.

**User-facing value:** Power users can point Claude Desktop at QSL and ask "summarize my unread mail" or "what's the status of the invoice from Acme?" QSL acts as the privacy-preserving broker — the AI never touches the user's raw OAuth tokens.

**Scope:**
- New workspace member `apps/mailmcp/` — binary, not library
- Read-only tools per the spec: `list_accounts`, `list_folders`, `search_messages`, `get_message` (sanitized body, no tracking), `get_thread`, `list_contacts`
- Stdio transport; `rmcp` crate for MCP protocol
- Reads from the shared Turso SQLite cache; coordinates sync lock with the desktop app via a file advisory lock
- `ammonia` pass on bodies before returning (same pipeline as the desktop's `messages_get`)
- Out of scope v1: write tools (`archive`, `send`, etc.), HTTP/SSE transport, streaming

**Success signal:** `echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | ./target/debug/qsl-mcp` returns the tool list. Claude Desktop can list folders and read a sanitized message body via the MCP config.

**Effort:** L  
**Priority:** P1 — biggest differentiator; spec is complete; additive (no backend changes)  
**Phase dependency:** Depends on the existing storage layer and `messages_get` pipeline being stable (Phase 1+2 complete)  
**QSL principle risks:** The MCP server reads sanitized content through QSL's privacy layer — no principle violations. Must never pass raw OAuth tokens over the MCP wire.  
**Pointers:** `docs/QSL_MCP_SERVER_SPEC.md`, `crates/storage/`, `crates/mime/src/lib.rs` (`sanitize_email_html`), `apps/mailcli/` (reference pattern for a standalone binary)

---

### P1 — Folder Display-Name Normalization

**Problem:** The sidebar and message-list header render the raw IMAP folder name `INBOX` in all-caps. This is confirmed open in `docs/QSL_BACKLOG_FIXES.md:53-68` ("Status: Open"). The IMAP spec mandates `INBOX` as the canonical name; Gmail returns it as-is; the UI renders it as-is. `Sent Mail`, `Drafts`, etc. show correctly because Gmail provides display names. IMAP-only providers (including Microsoft 365 and any self-hosted server) return their own conventions and need normalization too.

**Hypothesis:** A 10-line display-name resolver in the UI is the difference between "looks like a toy" and "looks like a real client." It also unblocks the MCP server's `list_folders` tool (the spec requires a `display_name` field separate from the raw `id`).

**User-facing value:** The sidebar shows "Inbox" not "INBOX." When Microsoft 365 is added, its "Junk Email" folder shows as "Spam." Custom folder names are unaffected.

**Scope:**
- Add `display_name_for_folder(folder: &Folder) -> &str` function (pure, no I/O) in `apps/desktop/ui/src/format.rs` or a new `apps/desktop/ui/src/folders.rs`; maps `FolderRole::Inbox → "Inbox"`, `FolderRole::Sent → "Sent"`, `FolderRole::Trash → "Trash"`, `FolderRole::Spam → "Spam"`, `FolderRole::Drafts → "Drafts"`, `FolderRole::Archive → "Archive"`, `FolderRole::Flagged → "Starred"`, all others → raw name
- Apply consistently in `SidebarMailboxRow` and `folder_title_from_selection` in `apps/desktop/ui/src/app.rs`
- Move the same logic into `crates/ipc/` so the MCP server can reuse it
- Out of scope: full IMAP SPECIAL-USE display-name discovery (overkill for v1)

**Success signal:** Sidebar shows "Inbox"; message-list header shows "Inbox." No other folder names change. Tests in `format.rs` cover each canonical role.

**Effort:** XS  
**Priority:** P1 — open bug; tiny effort; high polish value; MCP server dependency  
**Phase dependency:** None  
**QSL principle risks:** None  
**Pointers:** `docs/QSL_BACKLOG_FIXES.md:53-68`, `apps/desktop/ui/src/app.rs` (`folder_title_from_selection`, `SidebarMailboxRow`), `apps/desktop/ui/src/format.rs`

---

### P1 — DKIM/DMARC/SPF Sender-Verification Indicators

**Problem:** QSL blocks tracking pixels, cleans links, and sanitizes HTML — but provides no UI signal about whether the sender is who they claim to be. Every incoming message already passes through `parse_rfc822` in `crates/mime/`; the `mail-auth` crate is listed in `DESIGN.md §5.1` and exists as a workspace dep. No indicator surfaces in the reader pane, in `RenderedMessage`, or in `MessageHeaders`. Users of a privacy-respecting client have no way to see that "billing@paypal.com" failed DMARC alignment or that a newsletter came from a misconfigured domain.

**Hypothesis:** Surfacing a simple green/yellow/red indicator per message — "sender authenticated via DKIM+DMARC," "DMARC pass, no DKIM," "DMARC fail (possible spoofing)" — is a meaningful, concrete privacy benefit that no competitor renders to regular users. It leverages infrastructure QSL already has (`mail-auth` dep) and costs nothing in server calls (the headers are already downloaded).

**User-facing value:** A subtle indicator in the reader header gives privacy-conscious users ground truth on sender identity. Phishing emails that spoof trusted domains become visibly suspicious. This is a differentiator — Apple Mail, Thunderbird, and Spark all hide this data.

**Scope:**
- Add `dkim_result: Option<DkimResult>` and `dmarc_result: Option<DmarcResult>` to `RenderedMessage` in `crates/ipc/src/lib.rs`
- In `messages_get` (`apps/desktop/src-tauri/src/commands/messages.rs`), after parsing the raw body, run `mail-auth`'s DKIM verify + DMARC alignment check against the cached headers; populate the new fields
- Add a small indicator in the reader header (green lock / yellow warning / red X) in `apps/desktop/ui/src/app.rs` (`ReaderV2` header section)
- DKIM verification requires DNS lookups for the public key — run these async, don't block the render. Show a "checking…" state initially.
- Out of scope: SPF (requires checking sending IP against SPF record — sender IP is not available post-delivery); phishing blocking (a separate, larger feature)

**Success signal:** A legitimate newsletter from Substack shows a green DKIM indicator. A test email with a stripped DKIM signature shows red. Performance: the indicator appears within 1–2 seconds of opening a message (async DNS + verify).

**Effort:** M  
**Priority:** P1 — differentiator; leverages existing dep; meaningful privacy signal  
**Phase dependency:** Phase 1 read path complete  
**QSL principle risks:** DNS lookups for DKIM key material are outbound network requests — acceptable because they don't reveal which message you're reading (only the signing domain), and they're necessary for verification. No data leaves without user intent.  
**Pointers:** `DESIGN.md §5.1` (`mail-auth` dep), `crates/mime/src/lib.rs`, `apps/desktop/src-tauri/src/commands/messages.rs` (`messages_get`), `crates/ipc/src/lib.rs` (`RenderedMessage`)

---

### P1 — System Tray Icon with Unread Count

**Problem:** QSL has no system tray icon and no dock badge with unread count. Every competitor — Apple Mail, Thunderbird, Mailspring, Spark — shows unread count in the dock/taskbar. The Phase 0 non-goals explicitly deferred tray integration, and the post-Phase-2 plan lists it as deferred to "Phase 3 or whenever a maintainer picks it up" (`PHASE_2.md:31`). No tray code exists anywhere in the codebase (`rg "tray" --type rust` returns nothing). Tauri 2 ships a mature tray plugin (`tauri-plugin-shell`-style API), and `tauri-plugin-notification` is already in use.

**Hypothesis:** A tray icon is table-stakes desktop integration. Users who minimize QSL to the background have no way to know new mail arrived without switching to the app. An unread count badge is the simplest possible notification surface that works even when the app is minimized or the OS notification is dismissed.

**User-facing value:** QSL shows a count badge in the macOS dock, Windows taskbar, and Linux notification area when there is unread mail. Clicking the tray icon brings the main window to front. The badge updates live via the existing sync event channel.

**Scope:**
- Add `tauri-plugin-tray` to `apps/desktop/src-tauri/Cargo.toml`
- Create a tray icon (reuse the existing `apps/desktop/src-tauri/icons/32x32.png`); show the total unread count across all accounts as a badge string
- Subscribe to `sync_event` in the Tauri shell; recompute unread total by calling `folders_list_all_accounts` (or a new lightweight `unread_total` query) and update the badge
- Context menu: "Open QSL" + "Quit"
- On Linux: use `AppIndicator`-style tray (already in `libayatana-appindicator3` dep from AGENTS.md)
- Out of scope: per-account tray counts (a future polish item), custom tray menu actions (Reply from tray)

**Success signal:** After receiving a new email, the macOS dock badge and tray icon show the correct unread count. Badge clears when all mail is marked read. App window raises to front when tray icon is clicked.

**Effort:** S  
**Priority:** P1 — table-stakes; conspicuous absence; small effort  
**Phase dependency:** None — additive Tauri plugin wiring  
**QSL principle risks:** None  
**Pointers:** `apps/desktop/src-tauri/src/main.rs`, `apps/desktop/src-tauri/Cargo.toml`, `PHASE_2.md:31`, `apps/desktop/src-tauri/src/sync_engine.rs` (existing `sync_event` emission)

---

### P1 — Server-Side Search Fallback for Unsynced Mail

**Problem:** QSL's search (`messages_search`, `crates/search/src/lib.rs`) runs only against the local Turso FTS5 index, which contains only messages that have been synced. The history-sync feature (`crates/sync/src/history.rs`) can pull full mailbox history, but on a large Gmail account this is impractical and slow. A user searching for a message from two years ago gets "no results" if that message hasn't been locally synced, with no fallback. The post-phase-2 plan explicitly notes this: "Server-side search fallback ... deferred to v0.2. Reason: local-only ships as one PR; adding the server-side fallback path doubles the surface" (`docs/plans/post-phase-2.md:A6`).

**Hypothesis:** The gap between "local search" and "server search" is invisible until users hit it — and then it's a trust-destroying confusing experience ("I know this email exists, why can't QSL find it?"). A simple "Search all mail on server" affordance — a button below the results list — resolves this without requiring full-mailbox sync. Gmail's `X-GM-RAW` IMAP search and JMAP's `Email/query` both support full-text server-side search.

**User-facing value:** Users can find any message, not just synced messages. The local search is fast by default; a "Search on server" button retrieves older results on demand.

**Scope:**
- Add `search_server(query: String, account_id: AccountId) -> Vec<MessageHeaders>` to `MailBackend` trait in `crates/core/src/mail_backend.rs` as an optional method (default: `MailError::Other("not supported")`)
- Implement for `ImapBackend`: translate the parsed `qsl_search::Query` into `UID SEARCH X-GM-RAW <query>` for Gmail; fall back to `UID SEARCH TEXT <term>` for non-Gmail IMAP
- Implement for `JmapBackend`: `Email/query` with `filter.text` clause
- New IPC command `messages_search_server(query, account_id) -> MessagePage` in `apps/desktop/src-tauri/src/commands/messages.rs`
- UI: add a "Search all mail on server…" button below local results when local results are fewer than expected; it triggers a spinner + the server search, then merges/appends results
- Out of scope: combining local and server results into one ranked list (dedup by Message-ID is enough for v1)

**Success signal:** Searching for a 2-year-old Gmail message returns it via the "Search all mail on server" path. Performance: server search responds in <3s for a 100k-message mailbox.

**Effort:** M  
**Priority:** P1 — significant UX gap for power users; additive feature; no privacy concerns  
**Phase dependency:** Phases 1+2 complete (backends exist); Fastmail live-account validation (P0 above) should precede the JMAP path  
**QSL principle risks:** Server search queries leave the device (the query goes to Gmail/Fastmail servers). This is equivalent to what every email client does; QSL's principle is "no third party between you and your provider" — the provider server IS the endpoint here, so there's no principle violation. Document this in Settings → Privacy to set expectations.  
**Pointers:** `crates/core/src/mail_backend.rs` (`MailBackend` trait), `crates/imap-client/src/backend.rs`, `crates/jmap-client/src/lib.rs`, `apps/desktop/src-tauri/src/commands/messages.rs`, `crates/search/src/lib.rs`, `docs/plans/post-phase-2.md:A6`

---

### P1 — Drag-and-Drop Messages into Folders

**Problem:** Users cannot drag a message row from the message list into a folder in the sidebar. Every major email client supports this. QSL has `messages_move` working (IPC command, outbox drain integration, confirmed end-to-end in `apps/desktop/src-tauri/src/commands/messages.rs:773-910`), but the drag-and-drop UI affordance does not exist. The only ways to move a message are the context menu "Move to…" item or the bulk-action bar. Neither is fast enough for the "quickly file this to Projects/" workflow.

**Hypothesis:** Drag-to-folder is the fastest message-organization gesture. Power users who file mail heavily use it constantly; its absence is immediately felt after switching from Apple Mail or Thunderbird.

**User-facing value:** Drag a message row onto a folder in the sidebar; it moves. Visual feedback (sidebar folder highlights on hover; row animates out on drop). Works for multi-select bulk drag too.

**Scope:**
- Make the `SidebarMailboxRow` component a Dioxus drop target using `ondragover` / `ondrop` handlers
- Make `MessageRowV2` (and `ThreadRow`) draggable: `draggable="true"` + `ondragstart` that encodes message ID(s) in `dataTransfer`
- On drop, invoke `messages_move` IPC
- Visual feedback: highlight the drop-target folder row on `ondragenter`; clear on `ondragleave` / `ondrop`
- Multi-message drag: if `bulk_selected` is non-empty and the dragged row is in the selection, drag the entire selection; otherwise drag only the dragged message
- Out of scope: drag between accounts (cross-account move is architecturally complex and a rare use case)

**Success signal:** Dragging a message onto "Projects" in the sidebar moves it there. The message disappears from the current folder view and appears in Projects on the next sync tick. Multi-drag works the same way.

**Effort:** S  
**Priority:** P1 — high-value power-user affordance; `messages_move` backend already works  
**Phase dependency:** Phase 2 write path complete  
**QSL principle risks:** None  
**Pointers:** `apps/desktop/ui/src/app.rs` (`MessageRowV2`, `SidebarMailboxRow`, `ThreadRow`), `apps/desktop/src-tauri/src/commands/messages.rs` (`messages_move`), `apps/desktop/ui/assets/tailwind.css`

---

## Later — P2

These five items are genuinely deferred — either they're on the DESIGN.md roadmap with a specific phase target, or they're blocked by upstream technical dependencies.

---

### P2 — Client-Side Rules and Filters UI

**Problem:** QSL has no rules engine and no UI for managing filters. `DESIGN.md §3.2` lists "Rules and filters (client-side plus Sieve where supported)" as post-MVP. `COMMANDS.md` "Commands Intentionally Not Here" explicitly defers rules. The Phase 3 milestone ("Polish") in `DESIGN.md §11` includes "rules and filters (client-side)" in its scope. No backend code exists for rules in any crate.

**Hypothesis:** Client-side rules are a major productivity feature. Auto-labeling newsletters, routing receipts, flagging mail from specific senders — these replace manual sorting. For Fastmail users who already have server-side Sieve rules, QSL could read their existing rules and apply them locally as a compatibility layer.

**User-facing value:** Users create conditions ("from: newsletter@substack.com → archive") that apply automatically to every new message as it syncs. Works offline.

**Scope:**
- Schema: `rules` table (condition_json, action_json, enabled, priority)
- Rule engine runs in `crates/sync` after `sync_folder` inserts messages; evaluates rules against new `MessageHeaders`
- Actions: move, label, mark read, flag, archive, delete
- Conditions: from, to, subject, has-attachment, is-unread (basic set; expandable)
- Settings UI: rules list with add/edit/delete/reorder
- Out of scope for v1: Sieve export, server-side rule sync, regex conditions

**Success signal:** A rule "from: github.com → mark read + move to GitHub" processes 50 GitHub notifications on next sync without user interaction.

**Effort:** L  
**Priority:** P2 — Phase 3 per DESIGN.md; needs a rules schema, engine, and Settings UI; no hard blockers but substantial new surface  
**Phase dependency:** Phase 3 per `DESIGN.md §11`  
**QSL principle risks:** None — client-side rules are private by construction  
**Pointers:** `DESIGN.md §3.2`, `COMMANDS.md` (deferred section), `crates/sync/src/lib.rs`, `crates/storage/migrations/`, `apps/desktop/ui/src/settings.rs`

---

### P2 — Per-Account Identities and Aliases

**Problem:** `COMMANDS.md` "Commands Intentionally Not Here (Deferred)" explicitly lists per-account identities and aliases. Currently each account has one email address and one display name. Users with multiple From aliases (e.g., `me@example.com` and `work@example.com` on the same Gmail account) cannot select which alias to send from in the compose pane. The `DraftData.account_id` field drives From selection, but there is no alias concept.

**Hypothesis:** Professionals managing multiple aliases — common for Google Workspace users with email aliases, or Fastmail users with custom domains — cannot use QSL as their primary client without this. It's a table-stakes feature for anyone who sends mail from more than one address.

**User-facing value:** Compose pane shows a From selector listing all aliases for the selected account. Per-alias signature (builds on existing per-account signature infrastructure).

**Scope:**
- Schema: `account_aliases` table (account_id, address, display_name, is_default, signature)
- `accounts_list_aliases` IPC command; `accounts_add_alias` / `accounts_remove_alias`
- Compose pane From dropdown expands to show aliases when >1 exists
- Per-alias signature selection at compose time
- Out of scope: auto-discovering aliases from the provider (manual entry only for v1)

**Success signal:** Add alias "work@example.com" in Settings. New compose starts with the default alias; switching to "work" changes From and inserts the work signature.

**Effort:** M  
**Priority:** P2 — COMMANDS.md deferred; Phase 7+ per roadmap; no hard blockers but needs schema work  
**Phase dependency:** Phase 7+ per `COMMANDS.md`  
**QSL principle risks:** None  
**Pointers:** `COMMANDS.md` (deferred section), `crates/storage/migrations/`, `apps/desktop/ui/src/app.rs` (`ComposePane`), `crates/storage/src/repos/accounts.rs`

---

### P2 — Snooze and Client-Side Send-Later

**Problem:** `COMMANDS.md` and `PHASE_2.md:Non-Goals` both explicitly defer snooze and send-later to Phase 7+. No code exists for either. These are standard features in Spark, Apple Mail, and Gmail. QSL's design principle explicitly prohibits send-later via a vendor server ("no third-party server for send-later"), but a client-side implementation — queue the message in the outbox with a `not_before` timestamp, drain when the timer fires — is fully aligned with QSL's offline-first, no-vendor-server principles.

**Hypothesis:** Snooze + send-later are table-stakes for knowledge workers. The outbox-with-retry infrastructure is already in place (`crates/sync/src/outbox_drain.rs`); adding a `not_before` column and a timer check is mechanically small. The differentiation is that QSL does this without a vendor server — the message never leaves the device until the timer fires.

**User-facing value:** Right-click a message → "Snooze until Monday 9am"; it disappears and re-appears as unread at the configured time. Compose → "Send at 9am tomorrow"; the message queues locally and is sent at the right time.

**Scope:**
- Add `not_before: Option<DateTime<Utc>>` to the outbox table (migration)
- Outbox drain skips rows where `now < not_before`; a background timer checks every minute
- Snooze: create a local "re-surface" entry; add snooze UI to the context menu and reader toolbar
- Send-later: add a "Send at…" date picker to compose, alongside the existing undo-send affordance
- Out of scope: snooze via Gmail/Fastmail server-side APIs (would require provider-specific code for a feature that works better client-side anyway)

**Success signal:** Snooze a message to Monday 9am; it disappears from the inbox and reappears as unread on Monday at 9am. Schedule a send; the outbox holds the message until the time fires.

**Effort:** M  
**Priority:** P2 — Phase 7+ per roadmap; outbox infrastructure is complete; no principle risks; just time  
**Phase dependency:** Phase 7+ per `DESIGN.md §3.2` and `COMMANDS.md`  
**QSL principle risks:** None — client-side implementation expressly aligned with "no vendor server"  
**Pointers:** `crates/sync/src/outbox_drain.rs`, `DESIGN.md §3.2`, `COMMANDS.md` (deferred section), `apps/desktop/ui/src/app.rs` (`ComposePane`)

---

### P2 — Microsoft 365 / Outlook.com Provider

**Problem:** `DESIGN.md §11` schedules Microsoft 365 as Phase 5. Currently `SmtpRoute::for_imap_host` in `crates/imap-client/src/backend.rs:1155-1166` only handles `imap.gmail.com` — any other IMAP host returns `None` and send fails. Adding M365 requires: an OAuth2 provider profile in `crates/auth/src/providers/`, the SMTP route for `smtp.office365.com`, and validation that the Gmail IMAP adapter works against Outlook's IMAP (which advertises CONDSTORE+QRESYNC+IDLE and should be compatible).

**Hypothesis:** Microsoft 365 is the world's most-used corporate email platform. Supporting it, post-Gmail+Fastmail, dramatically expands QSL's potential audience. The architecture is intentionally designed to make this a new provider profile + OAuth endpoints, not a new backend — the IMAP+SMTP infrastructure should just work.

**User-facing value:** Microsoft 365 and personal Outlook.com users can add their account to QSL with the same in-app OAuth flow as Gmail.

**Scope:**
- `crates/auth/src/providers/microsoft.rs` — OAuth2 profile (MSAL v2 endpoint, `Mail.ReadWrite` + `Mail.Send` scopes, PKCE)
- Add `imap.outlook.com` → `smtp.office365.com:587` to `SmtpRoute::for_imap_host`
- Add `Microsoft365` variant to `OAuthProvider` enum in `crates/ipc/src/lib.rs` and `COMMANDS.md`
- Live-account smoke test: list folders, read message, send message, draft sync
- Out of scope: EWS (explicitly non-goal), Exchange on-prem

**Success signal:** A Microsoft 365 user can add their account via the in-app OAuth flow and read/send mail from QSL without errors.

**Effort:** M  
**Priority:** P2 — Phase 5 per DESIGN.md; blocked on Phase 4 (0.1 release) by design; significant audience  
**Phase dependency:** Phase 5 per `DESIGN.md §11`; Phase 4 (0.1 release) must ship first  
**QSL principle risks:** None — same IMAP+SMTP+OAuth2 stack  
**Pointers:** `DESIGN.md §11`, `crates/imap-client/src/backend.rs:1155-1166`, `crates/auth/src/providers/`, `crates/ipc/src/lib.rs` (`OAuthProvider`), `COMMANDS.md`

---

### P2 — Servo Renderer Revival

**Problem:** `DESIGN.md §1` commits to "Rust-native throughout" and specifically to Servo as the email renderer for cross-platform rendering consistency. Servo was removed from the tree on 2026-04-28 (`docs/servo-tombstone.md`) after AMD+NVIDIA GPU compositor failures on Linux prevented the email body from painting. The current renderer is a sandboxed webkit2gtk `<iframe srcdoc>`, which means rendering is now platform-dependent (WebKit on macOS/Linux, WebView2 on Windows) — the exact problem Servo was meant to solve. The tombstone document contains a detailed revival path.

**Hypothesis:** Cross-platform rendering consistency is a genuine differentiator — emails look the same on all three platforms. The GPU compositor issue was specific to the `LIBGL_ALWAYS_SOFTWARE=1` workaround being applied unconditionally on Linux; gating it on NVIDIA detection (or upgrading to GTK4, which Tauri is planning) would likely unblock the AMD path. Servo's `0.1.x` embedding API is still young but maturing; re-integrating as an opt-in build feature (`--features servo`) preserves the pure-Rust story without forcing it on users until it's stable.

**User-facing value:** Email HTML renders identically on macOS, Windows, and Linux. Memory-safe Rust rendering for the app's most hostile input surface (email HTML from arbitrary senders).

**Scope:**
- Re-add `crates/renderer/` with the Servo implementation behind `--features servo` (opt-in, not default)
- Fix the GPU compositor issue: gate `LIBGL_ALWAYS_SOFTWARE=1` on `lspci | grep -i nvidia` detection, not unconditionally
- Re-run the Phase 0 Week 6 corpus (10 test emails) and fix any rendering regressions
- Promote the feature flag to default when the corpus passes on all three platforms
- Out of scope: migrating to GTK4 (a larger project tied to Tauri's GTK4 migration timeline)

**Success signal:** A test email (Stripe receipt, Substack newsletter, GitHub notification) renders visually identically on macOS, Linux (AMD GPU), and Windows. The `--features servo` build passes `cargo test --workspace`.

**Effort:** XL  
**Priority:** P2 — post-v1 per DESIGN.md; blocked by GPU compositor + Servo API churn; tombstone doc has the revival path  
**Phase dependency:** Post Phase 4 (0.1 release); depends on Servo `0.1.x` API stability  
**QSL principle risks:** Servo is the embodiment of the Rust-native principle. Deferring it is a temporary pragmatic compromise, not a change in direction. The webkit iframe is explicitly documented as a stopgap.  
**Pointers:** `docs/servo-tombstone.md`, `docs/servo-composition.md`, `DESIGN.md §1` + §4.5, `apps/desktop/src-tauri/Cargo.toml` (feature flags pattern)

---

## Won't

Proposals considered and rejected for QSL v1. These are listed explicitly so future sessions don't re-litigate them.

| Proposal | Reason rejected |
|---|---|
| **POP3 support** | Violates offline-first principle — POP3 has no server-side state, makes multi-device use impossible |
| **Exchange / EWS / EAS / MAPI** | Explicitly non-goal in `DESIGN.md §2`; users on Exchange connect via IMAP+OAuth2 |
| **App-password authentication** | Violates OAuth2-only principle; would require storing plaintext passwords; explicitly excluded in `DESIGN.md §2` |
| **iCloud Mail / Yahoo Mail** | App-password-only providers per `DESIGN.md §3.3`; no OAuth2 path from either provider as of 2026 |
| **Hosted sync/backend service** | Violates "no third-party servers between user and provider" principle; `DESIGN.md §2` explicit non-goal |
| **Calendar as a first-class module** | `DESIGN.md §2` explicit non-goal for v1; CalDAV/JMAP-Calendars can be post-v1 extension |
| **Mobile apps (v1)** | Out of scope for v1 per `DESIGN.md §2`; architecture doesn't preclude mobile later |
| **Tracking-pixel read receipts or vendor-server send-later** | `DESIGN.md §2` and §6 explicit non-goals; both require either tracking infrastructure or a vendor server |
| **ProtonMail / Tutanota** | Proprietary protocols; require their own Bridge/client — `README.md` explicit non-support |
| **WYSIWYG rich-text compose editor** | `PHASE_2.md:Non-Goals` explicit decision: "Markdown is the v1 ceiling. WYSIWYG can be a Phase-3-or-later plugin." |

---

## How to Use This Document

Each **P0** and **P1** proposal is sized to fit in a single Claude Code or OpenCode session (S or M effort). To start work on any proposal:

1. Quote the **Problem** and **Scope** bullets as the starting prompt
2. Reference the **Pointers** file paths as the reading list
3. Follow the **Success signal** as the acceptance criterion

For **P0** items — start there. They are blocking pre-v0.1. For **P1** items — pick by appetite after v0.1 ships. The suggested order (by user impact) is: thread reader → MCP server → folder names → DKIM/DMARC → tray → server search → drag-drop.

This document supersedes the v0.1 cut list in `docs/plans/post-phase-2.md` for items that have since shipped, but that doc remains authoritative for sequencing rationale and PR-by-PR implementation sketches.
