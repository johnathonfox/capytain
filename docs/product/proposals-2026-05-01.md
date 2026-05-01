<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Backlog Proposals — 2026-05-01

> Sidecar to `BACKLOG.md`. Independent research pass produced on
> 2026-05-01. Where this overlaps with `BACKLOG.md`, `BACKLOG.md` is
> the canonical P0/P1 cut list; this document is the longer-form
> proposal text behind several of those items.

## Context

QSL is a cross-platform desktop email client written end-to-end in Rust (Tauri 2 shell,
Dioxus/WASM UI, webkit2gtk sandboxed iframe for email rendering, Turso/SQLite storage, tantivy
FTS). It currently supports Gmail (IMAP+SMTP+OAuth2) and Fastmail (JMAP+OAuth2). The project
has completed Phases 0–2 and the post-Phase-2 v0.1 feature plan: threading, full-text search,
compose with attachments/signatures/undo-send, settings panel, first-run OAuth flow, keyboard
shortcuts, multi-select bulk operations, notifications, and a warm-dark monospace UI overhaul are
all shipped. The project is now at version 0.0.1 and building toward a 0.1.0 tag. The clearest
next milestone is the MCP server (spec written at `docs/QSL_MCP_SERVER_SPEC.md`, not yet
implemented), preceded by closing three security audit findings from `docs/security/audit-2026-05-01.md`.
Everything beyond that maps to the DESIGN.md Phase 5–7 roadmap.

## Themes

- **Release-gate** — security and quality items that should land before the 0.1.0 tag
- **MCP surface** — expose the local mail cache to AI agents via the Model Context Protocol
- **Provider breadth** — Microsoft 365 and self-hosted OAuth2 servers
- **Power-user depth** — search power, rules/filters, scheduling, PGP/S-MIME
- **Platform integrity** — runtime validation, renderer fidelity, idle resource use

---

## Proposals

---

### P0 — Remediate three security audit findings before 0.1.0 tag

**Problem:** The 2026-05-01 security audit (`docs/security/audit-2026-05-01.md`) flagged two
High and one Medium finding that are straightforward to fix but should not ship in a 0.1.0 release
aimed at real users:

1. **Token zeroization** (SEC-001 High): `AccessToken` and `RefreshToken` in
   `crates/auth/src/tokens.rs` wrap a plain `String`. When dropped, the heap allocation is freed
   but not overwritten — a memory dump, crash report, or page-file scrape can recover the token
   bytes. The fix is adding the `zeroize` crate (already in the Rust ecosystem; Apache-2.0) and
   implementing `Zeroize`/`ZeroizeOnDrop` on both types.

2. **Token revocation on logout** (SEC-002 High): `accounts_remove`
   (`apps/desktop/src-tauri/src/commands/accounts.rs:275`) deletes the keychain entry but does not
   call the provider's revocation endpoint. Google's endpoint is
   `https://oauth2.googleapis.com/revoke`; Fastmail does not publish a revocation endpoint
   (PKCE-only clients rely on token expiry). A removed account's refresh token remains valid at
   Google until it naturally expires or Google detects unusual use. The fix is calling the
   revocation endpoint before deleting the keychain entry; Fastmail can log a `warn!` and
   continue since there is no revocation API.

3. **Missing Tauri CSP** (SEC-004 Medium): `apps/desktop/src-tauri/tauri.conf.json` has
   `"csp": null`. With no Content Security Policy, the Tauri-managed webview that hosts the
   Dioxus UI has no protection against content injection via a compromised IPC message. A strict
   `default-src 'self'; script-src 'self' 'unsafe-inline'` (Dioxus/WASM currently needs
   `unsafe-inline` for its inline script bootstrap) is a meaningful improvement with near-zero
   integration risk.

**Hypothesis:** None of these is hypothetical — all three are audit-confirmed. Zeroization
protects against crash-dump credential extraction. Revocation ensures account removal has the
intended security effect. CSP provides defence-in-depth against a class of attacks the project
explicitly cares about (email is a hostile-HTML surface, and the renderer is one IPC message away
from the UI webview).

**Scope:**
- Add `zeroize = "1"` to `crates/auth/Cargo.toml`; derive `ZeroizeOnDrop` on `AccessToken` and
  `RefreshToken`; add a test confirming the backing bytes are zeroed on drop.
- Add a `revoke_token(account_id, provider)` async helper in `crates/auth/src/refresh.rs` that
  calls the provider revocation endpoint; call it at the top of `accounts_remove` before the
  keychain delete; skip gracefully for Fastmail (no endpoint) with a `warn!`.
- Set `"csp": "default-src 'self'; script-src 'self' 'unsafe-inline'; img-src * blob: data:; connect-src 'self' ipc: https://ipc.localhost"` in `tauri.conf.json`; verify the Dioxus WASM app still loads.

**Success signal:** Audit SEC-001, SEC-002, SEC-004 are marked resolved. No new test failures.
Token zeroization test passes. Manual test: remove a Gmail account → `curl` the revocation
endpoint returns 200 (token invalid).

**Effort:** S (three independently mergeable changes; zeroize is 5 lines; revocation is ~30 lines;
CSP is a config string)

**Priority:** P0 — should not tag 0.1.0 with known High-severity unmitigated findings.

**Pointers:**
- `crates/auth/src/tokens.rs` — zeroize
- `crates/auth/src/refresh.rs` — revocation helper
- `apps/desktop/src-tauri/src/commands/accounts.rs:275` — call site
- `apps/desktop/src-tauri/tauri.conf.json` — CSP config
- `docs/security/audit-2026-05-01.md` — full audit context

**Architect needed:** No — three independent, bounded fixes with no design choices required.

---

### P0 — Implement the MCP server (`qsl-mcp` binary)

**Problem:** The full MCP server design is specified at `docs/QSL_MCP_SERVER_SPEC.md` but nothing
has been implemented. The only way to use QSL's local mail cache from an AI agent today is through
the Tauri IPC (not accessible to external tools). Claude Desktop, Claude Code, and any other MCP
client cannot read the user's mail through QSL, which defeats the stated goal of "QSL owns the
IMAP/OAuth code, the local cache, and the policy layer — the MCP server is a façade over it."

**Hypothesis:** A read-only stdio MCP server is the single feature with the highest leverage for
the target user (a developer who uses AI agents daily). Every future agentic capability — mail
search, thread summarization, draft generation, action suggestions — depends on this surface
existing. The spec is complete; the implementation is the only thing missing.

**Scope:**
- New `apps/mcp/` binary crate (`qsl-mcp`) in the workspace; reads from the same data directory
  as the Tauri app (`~/.local/share/qsl/` etc.), uses the same `TursoConn` and repo layer.
- v0 tools per spec: `list_accounts`, `get_account_info`, `list_folders`, `search_messages`,
  `get_message`, `get_thread`.
- `rmcp` crate for the MCP protocol; stdio transport only; log to stderr.
- File/SQLite advisory lock: if the Tauri app is running and holds the sync loop, `qsl-mcp` is
  read-only against the cache; if not, `qsl-mcp` runs a light sync poll.
- HTML sanitization (`ammonia`) applied to `body_html` returns before sending over MCP — agents
  must not trigger remote-image loads or tracking pixels by reading mail.
- Out of scope for v0: write tools, HTTP/SSE transport, streaming push.

**Success signal:** `claude` (or any MCP client) can be configured to use `qsl-mcp` and
successfully answer "what are my most recent unread emails?" without touching the provider API
directly.

**Effort:** L (the spec itself estimates the refactor step as half the total work; the MCP wiring
is M on top of that)

**Priority:** P0 — the MCP surface is the next stated milestone and blocks all agentic use-cases
downstream.

**Pointers:**
- `docs/QSL_MCP_SERVER_SPEC.md` — authoritative spec, read fully before writing any code
- `crates/storage/src/repos/` — existing repo layer to call from `qsl-mcp`
- `crates/storage/src/turso_conn.rs` — connection to open against the shared data dir
- `apps/mailcli/` — existing headless binary, good structural reference for a non-Tauri binary

**Architect needed:** Yes — the spec describes an optional `MailStore` trait extraction that would
let `qsl-mcp` and `qsl-app` share a common data access layer. The current workspace has
`crates/storage` but the MCP binary will need to decide whether to call the repo layer directly
(simpler, potentially duplicates some Tauri-command business logic) or extract a shared trait
first (cleaner, more maintenance). This is the key early-session decision; the spec recommends the
extraction but it adds scope. The implementer should pick one before writing any code.

---

### P0 — Fastmail real-account end-to-end validation

**Problem:** The JMAP send path (`qsl-jmap-client`, `EmailSubmission/set`) was implemented in
Phase 2 (PR #71) and CI tests pass against cassette fixtures, but it has never been exercised
against a live Fastmail account. The Fastmail OAuth client has not been registered (tracked in
`docs/USER_TODO.md` and `docs/dependencies/fastmail.md`). This means the second provider — a
first-class v1 target — has an unvalidated send path that could fail in surprising ways when a
real user tries to use it.

**Hypothesis:** Shipping 0.1.0 with an unvalidated JMAP send path would be a credibility problem.
Fastmail is not an afterthought — it's the reference JMAP target. Validating it catches real
integration issues (EventSource reconnect behavior, `EmailSubmission/set` edge cases, server-issued
`cannotCalculateChanges` errors) before they affect real users.

**Scope:**
- Maintainer action: register Fastmail OAuth client per `docs/dependencies/fastmail.md` runbook;
  add `QSL_FASTMAIL_CLIENT_ID` to workspace `.env`.
- Run `mailcli auth add fastmail <address>` → `mailcli sync <address>` → verify correct folder list
  and message count.
- Compose a test message in the desktop UI to a second mailbox; verify receipt, correct headers,
  Sent folder entry in QSL, and JMAP-side deduplication.
- Network-drop test: pull connection mid-send → outbox DLQ → reconnect drains without duplicate.
- Update `docs/dependencies/fastmail.md` and `PHASE_0.md` deferred table with results.
- Out of scope: new code (this is validation of existing code, not feature work).

**Success signal:** `PHASE_0.md` "Deferred from Phase 0" Fastmail row is deleted or marked
verified. Send path smoke test passes on a live account.

**Effort:** XS (mostly maintainer keypresses and reading logs; the code is already there)

**Priority:** P0 — blocks trusting the second listed provider; should complete before the 0.1.0 tag.

**Pointers:**
- `docs/USER_TODO.md` — "Fastmail OAuth client" checkbox
- `docs/dependencies/fastmail.md` — OAuth client setup runbook
- `apps/mailcli/src/main.rs` — `auth add` + `sync` commands
- `crates/jmap-client/src/backend.rs` — `submit_message` to watch

**Architect needed:** No — this is a validation exercise. If it surfaces bugs, those are separate
proposals.

---

### P1 — Enable branch protection on `main`

**Problem:** The `main` branch has no protection rules. Force-pushes and deletions are not blocked
by GitHub. While the project convention is "merge on green CI," nothing enforces it. This is a
consciously-tracked known issue in `docs/KNOWN_ISSUES.md` since the project started.

**Hypothesis:** Branch protection is a one-time admin action that prevents accidental history
destruction and enforces the CI gate the project already runs. At the current stage — heading
toward a public 0.1.0 release and potential outside contributors — the absence of this is a
credibility gap.

**Scope:**
- GitHub UI: Settings → Branches → Add rule targeting `main`.
- Require the four existing CI checks: `Check (ubuntu-latest)`, `Check (windows-latest)`,
  `cargo-deny`, `reuse lint`. No required reviews (solo-maintainer).
- Block force-pushes and deletions. `enforce_admins: false` so maintainer can override in
  genuine emergencies.
- Delete the entry from `docs/KNOWN_ISSUES.md` once done.
- Out of scope: adding required reviews (solo project).

**Success signal:** Pushing a commit that fails any CI check to `main` is blocked. Force-push to
`main` from a dev branch is rejected. `docs/KNOWN_ISSUES.md` has no entries.

**Effort:** XS (five minutes in the GitHub UI)

**Priority:** P1 — important for project health but not a user-visible feature.

**Pointers:**
- `docs/KNOWN_ISSUES.md` — entry to delete after completion
- `docs/USER_TODO.md` — checkbox to tick
- `.github/workflows/` — CI job names to reference in the protection rule

**Architect needed:** No.

---

### P1 — Investigate and fix webkit2gtk high CPU at idle

**Problem:** The maintainer observed high CPU usage from the webkit2gtk renderer at idle
(documented in `docs/USER_TODO.md` investigations section). Suspects include a Dioxus reactive
cycle churning on a self-triggering signal, a JavaScript-side ResizeObserver stuck in a
layout-thrash loop, or the compose draft auto-save (5s debounce) failing to gate on "compose is
open." No profiling has been done yet.

**Hypothesis:** An email client that burns CPU at idle is unsuitable as a daily-driver. Given the
project just completed a large UI overhaul (PR #92) and added several new reactive signals and
event listeners, the timing suggests something introduced in that window. Fixing this before 0.1.0
is a quality-of-life gate.

**Scope:**
- Profile: open Chrome/WebKit devtools (`Ctrl+Shift+I` in Tauri dev mode) → Performance tab →
  record 10s at idle → identify the hot stack.
- Fix whatever the profile reveals: most likely a Dioxus `use_reactive!` with a signal in its
  deps that it also writes, or a JS timer that isn't gated on component visibility.
- Add a note to `docs/KNOWN_ISSUES.md` if the fix is incomplete (e.g., identified but requires
  upstream Dioxus change).
- Out of scope: changes to sync engine, IMAP IDLE cadence, or anything that doesn't show in the
  profiler.

**Success signal:** CPU usage at idle (main window open, no compose, no sync in progress) drops to
<5% on a modern CPU. The change is validated by the maintainer in a real session.

**Effort:** S (profiling + targeted fix; if the fix is upstream-only, the effort is documenting)

**Priority:** P1 — user-visible quality gate; affects whether QSL is usable as a daily-driver.

**Pointers:**
- `apps/desktop/ui/src/app.rs` — Dioxus reactive signals and `use_reactive!` deps
- `docs/USER_TODO.md` — "webkit2gtk CPU usage" investigation item

**Architect needed:** No.

---

### P1 — macOS and Windows runtime validation

**Problem:** `crates/renderer/src/servo/macos.rs` and `windows.rs` were marked `UNVERIFIED` in
Phase 0 because the Servo renderer was written to a target shape without Mac or Windows hardware.
Servo was subsequently removed (PR #101, 2026-04-28) and replaced with a sandboxed webkit2gtk
`<iframe srcdoc>`, but the Tauri app chrome itself — window creation, OAuth loopback server,
keyring integration, tray/notification plugin — has never been validated on macOS or Windows. CI
builds on `macos-latest` and `windows-latest` but the runtime (actual app launch, OAuth flow,
mail render) has not been confirmed.

**Hypothesis:** The project's README claims macOS 12+ and Windows 10 22H2+ support. Shipping
0.1.0 without verifying those claims is a credibility risk. The Tauri 2 platform abstractions
should handle most of this, but keyring behavior (macOS Keychain vs Windows Credential Manager),
window chrome, and notification plugin behavior vary.

**Scope:**
- macOS: run `cargo tauri dev` on Apple Silicon or Intel Mac; confirm window opens, OAuth flow
  completes in Safari, keychain stores token, sync runs, reader pane renders HTML email.
- Windows: same sequence on Windows 10 or 11 x86_64.
- Document findings in a new `docs/platform-validation.md` (pass/fail per feature per platform).
- File upstream issues for any Tauri/keyring/notification failures; work around in-tree if
  unblocking within 14 days.
- Out of scope: ARM64 Windows (explicitly deferred in DESIGN.md §12).

**Success signal:** `docs/platform-validation.md` exists with green results for both platforms.
Any known issues documented with workarounds or upstream links.

**Effort:** S (running the app and taking notes; any bugs found may add M work)

**Priority:** P1 — without this, the multi-platform claim in README is unverified.

**Pointers:**
- `docs/PHASE_0.md` "Deferred from Phase 0" section — macOS and Windows runtime rows
- `apps/desktop/src-tauri/src/main.rs` — entry point for platform-specific paths

**Architect needed:** No.

---

### P1 — Microsoft 365 / Outlook.com provider

**Problem:** Microsoft 365 and Outlook.com are the second-most-used email provider behind Gmail
among developers and knowledge workers. The DESIGN.md Phase 5 plan calls for adding M365 as a
second IMAP+OAuth2 provider after Gmail, using the existing IMAP adapter with a new OAuth2
provider profile. Users on M365 are currently unserved.

**Hypothesis:** Adding M365 would significantly expand the addressable user base without requiring
new protocol work — the IMAP adapter already handles CONDSTORE/QRESYNC/IDLE, and SMTP submission
already handles XOAUTH2. The effort is primarily: register an Azure app, add the OAuth2 provider
profile, test against an Outlook.com account, and handle any M365-specific quirks (folder naming
conventions differ from Gmail; M365's IMAP has its own capability set).

**Scope:**
- Add `Microsoft365` variant to `OAuthProvider` enum in `crates/ipc/` and `crates/auth/src/providers/`.
- Register an Azure AD application for QSL; document client ID and required scopes in a new
  `docs/dependencies/microsoft365.md`.
- Handle M365 IMAP folder naming (`Inbox` not `INBOX`, `Sent Items` not `Sent Mail`, etc.) in the
  display name mapper (`apps/desktop/ui/src/format.rs:display_name_for_folder`).
- Validate OAuth flow, folder sync, read, compose, and send against a live M365/Outlook.com
  account.
- Out of scope: Exchange EWS/EAS (explicitly excluded in DESIGN.md §2); M365 calendar/contacts.

**Success signal:** A user can add a Microsoft 365 or Outlook.com account via the first-run OAuth
flow, see their inbox, and send a reply. Provider appears in the account-add picker.

**Effort:** M (OAuth profile is small; IMAP adapter reuse is the point; M365 quirks testing is the
unknown)

**Priority:** P1 — high user-base impact; should land in v0.2.

**Pointers:**
- `crates/auth/src/providers/gmail.rs` — reference implementation for an IMAP+SMTP OAuth2 profile
- `crates/imap-client/src/backend.rs` — shared IMAP adapter to reuse
- `COMMANDS.md` `OAuthProvider` enum — IPC surface to extend
- `DESIGN.md §11 Phase 5` — roadmap context

**Architect needed:** No — the adapter is shared; this is a new provider profile + config. The
OAuth profile pattern is established.

---

### P1 — Server-side search fallback (Gmail `X-GM-RAW`, JMAP `Email/query`)

**Problem:** QSL's full-text search (`crates/search/`, Turso experimental FTS) searches only the
local cache. A user who has synced only the last 90 days of email cannot search messages from
2022. The `docs/plans/post-phase-2.md` plan explicitly deferred server-side search to v0.2 with
the note "local-only ships as one PR; adding the server-side fallback path doubles the surface."
The local-only search has shipped; the fallback is now the open item.

**Hypothesis:** Email search that silently returns no results for old messages — without telling
the user "there may be more on the server" — is a user-trust problem. A simple "Search all mail
on server" affordance, surfaced when local results are sparse or the user explicitly requests it,
resolves this without requiring eager full-mailbox download.

**Scope:**
- Add `search_server_fallback(query, account_id) -> Vec<MessageHeaders>` to `MailBackend` trait
  (optional method with a default returning `Ok(vec![])`).
- Implement for `ImapBackend` via `UID SEARCH X-GM-RAW "<query>"` (Gmail) or generic IMAP
  `SEARCH` for non-Gmail.
- Implement for `JmapBackend` via `Email/query` with `filter: { text: "..." }`.
- Add "Search server" link below local results; clicking replaces the result list with server
  results (fetches and locally caches them as they arrive).
- Out of scope: result merging/deduplication between local and server (v0.3); real-time streaming
  of results.

**Success signal:** User types "invoice 2023" → local results show. Clicking "Search all mail on
server" triggers a provider search and shows older matches.

**Effort:** M

**Priority:** P1 — important for users with large mailboxes; directly affects daily-driver
suitability.

**Pointers:**
- `crates/imap-client/src/backend.rs` — IMAP search command
- `crates/jmap-client/src/backend.rs` — JMAP `Email/query`
- `TRAITS.md` `MailBackend` — trait to extend
- `apps/desktop/ui/src/app.rs` — search UI components to extend
- `docs/plans/post-phase-2.md` "Out of v0.1" section

**Architect needed:** Yes — extending `MailBackend` with an optional server-search method is a
trait-surface decision. The default-impl pattern (return empty vec) keeps existing impls compiling,
but the method signature needs to be right the first time since `TRAITS.md` is the contract
document. Worth a 10-minute ADR to confirm the signature before implementation.

---

### P2 — SPF/DKIM/DMARC indicators in the reader pane

**Problem:** The reader pane shows no authentication signals for incoming mail. Users have no
in-app way to verify that an email claiming to be from their bank actually passed SPF, DKIM, and
DMARC checks. The `mail-auth` crate is already listed in `DESIGN.md §5.1` as a dependency for
"inbound verification."

**Hypothesis:** Authentication indicators are a privacy/security affordance that differentiates
QSL from standard mail clients, directly supporting the "private by default" principle. Most
phishing emails fail DMARC; surfacing this prominently but unobtrusively (a small lock/shield icon
in the reader header with a tooltip, amber/red for fail, green for pass) gives users a real signal
without overwhelming the UI.

**Scope:**
- Add `mail-auth` crate to `crates/mime` or a new `crates/auth-verify` crate for DNS-based
  verification.
- Perform SPF/DKIM/DMARC verification on message fetch (or read from existing `Authentication-Results`
  headers where present — most providers already stamp these, avoiding a second DNS round-trip).
- Add `auth_result: AuthResult` field to `RenderedMessage` (IPC type in `crates/ipc`).
- Display in reader header: a small indicator icon (green = pass, amber = none, red = fail) with
  a tooltip explaining what passed/failed.
- Out of scope: active DNS verification for every message (prefer reading `Authentication-Results`
  headers); key management (that's PGP).

**Success signal:** Opening a legitimate email from a known domain shows a green authentication
indicator. Opening a forged test email (crafted to fail DMARC) shows red.

**Effort:** M

**Priority:** P2 — meaningful security UX but not blocking daily use; good for v0.2.

**Pointers:**
- `crates/mime/src/lib.rs` — HTML sanitization pipeline; authentication check slots here
- `crates/ipc/src/lib.rs` — `RenderedMessage` to extend
- `DESIGN.md §5.1` — `mail-auth` listed as planned dependency
- `apps/desktop/ui/src/app.rs` — reader pane component

**Architect needed:** Yes — choosing between "read `Authentication-Results` headers" vs. "perform
active DNS verification" is a meaningful design decision with security and performance tradeoffs.
Reading existing headers is fast and correct for messages from Gmail/Fastmail (both stamp them);
active DNS verification is needed for self-hosted or unusual providers but adds latency and
complexity.

---

### P2 — Client-side rules and filters

**Problem:** Users with high email volume need automation — "move newsletters to Promotions,"
"label GitHub notifications," "auto-archive Dependabot PRs." QSL has no rules engine. The
`DESIGN.md §3.2` post-MVP list includes "Rules and filters (client-side plus Sieve where
supported)."

**Hypothesis:** Rules are a daily-driver requirement for power users managing 50+ emails/day. A
client-side implementation (evaluate rules against incoming messages from the sync engine, apply
flag/move/label operations via the existing outbox) works independently of provider support and
avoids the complexity of Sieve where it's not available.

**Scope:**
- Data model: `rules(id, account_id, name, conditions_json, actions_json, enabled, priority)` —
  new migration.
- Condition types: `from:`, `to:`, `subject:`, `has_attachment`, `label`, `is_unread`.
- Action types: `move_to(folder_id)`, `add_label(label)`, `mark_read`, `archive`, `delete`,
  `flag`.
- Rules engine runs in `crates/sync` after each message insert; applies matching rules via the
  outbox (same optimistic path as manual operations).
- Settings UI: Rules tab showing rule list with create/edit/delete; simple form with
  condition + action builder.
- Out of scope: Sieve integration (v0.3+); server-side rule push to Gmail/Fastmail API.

**Success signal:** User creates a rule "from: github.com → move to GitHub folder." New GitHub
notifications arriving via IDLE are automatically moved to the GitHub folder without user action.

**Effort:** L

**Priority:** P2 — valuable for power users; not blocking basic daily use.

**Pointers:**
- `crates/sync/src/lib.rs` — sync loop to extend with rules evaluation
- `crates/storage/src/repos/` — new `rules_repo.rs`
- `apps/desktop/ui/src/settings.rs` — settings window to extend
- `DESIGN.md §3.2` — post-MVP feature list

**Architect needed:** Yes — the rules engine runs in `crates/sync`, which must not depend on
`crates/storage` directly today (verify the current dependency graph). If sync already depends on
storage (it does, via the `DbConn` trait), the rules table is straightforward. The action
evaluation must feed into the outbox rather than calling `MailBackend` directly, which requires a
brief design check to confirm the outbox drain handles rule-generated ops correctly (no infinite
loops if a rule moves a message that triggers another rule).

---

### P2 — Snooze and send-later (client-side)

**Problem:** Snooze ("resurface this message on Monday morning") and send-later ("send this at
9am tomorrow") are table-stakes features for a power-user email client. Both are deferred to
Phase 7+ in `DESIGN.md §3.2` and the post-Phase-2 plan explicitly calls them out-of-scope for
v0.1. They belong in the backlog for v0.3.

**Hypothesis:** Snooze reduces inbox cognitive load by letting users defer messages they can't
act on now. Send-later respects recipients' work hours without requiring the sender to stay up.
Both are fully implementable client-side — snooze via a `snoozed_until` timestamp on messages
(hide in inbox, resurface via a background timer); send-later via the existing outbox with a
`not_before` constraint.

**Scope:**
- Snooze: add `snoozed_until` column to `messages` table; a background timer in `AppState`
  polls every minute and un-snoozes via `messages_move` back to Inbox when the time passes; a
  dedicated Snoozed folder surfaces currently-snoozed messages.
- Send-later: add `send_after: Option<DateTime<Utc>>` to `OutboxItem` for `OutboxKind::Send`;
  the outbox drain skips items where `send_after > Utc::now()`; compose pane gets a
  "Schedule send" option.
- UI: snooze button in reader toolbar with time-picker (today at 5pm, tomorrow morning, next
  week, custom); compose send-later option.
- Out of scope: server-side snooze via Gmail API (no server-side guarantee).

**Success signal:** User right-clicks a message and selects "Snooze until tomorrow morning" →
message disappears from inbox → reappears at 8am the next day. User schedules a compose at
"9am tomorrow" → message sits in Drafts with a clock icon → sends automatically at that time.

**Effort:** M

**Priority:** P2 — quality-of-life addition; not blocking v0.1 or v0.2; good for v0.3.

**Pointers:**
- `crates/storage/src/repos/messages.rs` — `snoozed_until` column
- `apps/desktop/src-tauri/src/sync_engine.rs` — background timer for snooze check
- `crates/storage/src/repos/outbox.rs` — `send_after` constraint
- `DESIGN.md §3.2` — post-MVP list
- `COMMANDS.md` — outbox and compose IPC to extend

**Architect needed:** No — both features are additive to existing outbox and storage patterns.
Snooze is a column + a background check; send-later is a constraint on an existing queue.

---

### P2 — PGP/MIME signing and encryption

**Problem:** QSL has no support for reading or sending PGP/MIME encrypted or signed messages.
Users who exchange encrypted mail with colleagues cannot use QSL for that use-case. DESIGN.md
§3.2 lists "PGP/MIME and S/MIME" as post-MVP.

**Hypothesis:** End-to-end encryption support is a meaningful differentiator for a
privacy-respecting email client. PGP/MIME (RFC 3156) is the open standard used by Thunderbird,
Fastmail, and most privacy-focused users. The pure-Rust `pgp` crate (sequoia-pgp is Apache-2.0)
provides the cryptographic foundation.

**Scope:**
- Inbound: detect `multipart/encrypted` (PGP) or `application/pgp-signature` in MIME tree;
  attempt decryption/verification using the user's local key store; display verification badge in
  reader pane; show decrypted body.
- Outbound: compose UI exposes "Encrypt" and "Sign" toggles; `crates/mime` builds the correct
  MIME structure; key lookup for recipients from the local keystore or WKD (Web Key Directory).
- Key management: import/export PGP keys; a minimal keystore in `<data_dir>/pgp/`; no
  SKS/keyserver integration in v0 (WKD-only is sufficient for Fastmail and most modern providers).
- Out of scope: S/MIME (separate standard, separate crate, different trust model); key generation
  UI (import only in v0 of this feature).

**Success signal:** User imports their PGP private key. Receiving an encrypted email from a
colleague decrypts and displays correctly. Sending a signed email to a WKD-published recipient
sends with correct `multipart/signed` structure.

**Effort:** XL (cryptography, MIME complexity, key management UX — this is the largest item in
the backlog)

**Priority:** P2 — important for a subset of users; not blocking general use. Validate demand
before investing.

**Pointers:**
- `crates/mime/src/lib.rs` — MIME parse/build pipeline to extend
- `DESIGN.md §3.2` — post-MVP feature list
- `crates/smtp-client/src/lib.rs` — send path

**Architect needed:** Yes — PGP/MIME requires a new `crates/crypto` or `crates/pgp` crate, a
choice of cryptography library (sequoia-pgp vs. rpgp — both Apache-2.0, different API shapes),
and a key storage design. The dependency choice (sequoia vs rpgp) has build-time and API
implications; a short ADR is warranted before writing any code.

---

### P2 — Self-hosted provider support via custom OAuth2 configuration

**Problem:** Users running Stalwart Mail Server, Dovecot with OAUTHBEARER, or Cyrus with
OAUTHBEARER cannot use QSL because the provider picker only offers Gmail and Fastmail. DESIGN.md
Phase 6 describes adding a "custom OAuth2" profile with user-supplied authorization URL, token
URL, scopes, and IMAP/JMAP server config.

**Hypothesis:** The self-hosted market is small but high-signal for a privacy-focused client — it
is exactly the user who cares about "no third party between me and my mail." Since both IMAP and
JMAP backends are already implemented, adding a self-hosted provider is primarily configuration
UI plus OAuth2 endpoint parameterization.

**Scope:**
- Add `CustomOAuth { authorize_url, token_url, scopes, server_host, server_port, protocol }` to
  `OAuthProvider` enum in `COMMANDS.md`; implement the flow in `crates/auth`.
- Settings → Accounts → "Add custom account" form with the above fields; validation that the
  auth endpoint responds before saving.
- Document the expected server configuration for Stalwart and Dovecot+OAUTHBEARER in
  `docs/providers/self-hosted.md`.
- Out of scope: password-only IMAP (explicitly excluded in DESIGN.md §2); Exchange EWS.

**Success signal:** A developer running Stalwart Mail Server with OAuth2 enabled can add their
account via the custom OAuth form and use QSL normally.

**Effort:** M

**Priority:** P2 — niche but enthusiast-aligned; good for v0.3 or as a community contribution.

**Pointers:**
- `crates/auth/src/providers/` — `gmail.rs` and `fastmail.rs` as reference profiles
- `COMMANDS.md` `OAuthProvider` enum — IPC surface to extend
- `DESIGN.md §3.3` — provider table and Phase 6 context
- `apps/desktop/ui/src/oauth_add.rs` — provider picker UI

**Architect needed:** No — the pattern is established; this is parameterizing an existing flow.
The only question is validation UX (how to test a custom endpoint without a real server handy),
which can be solved with a "Test connection" button that calls `list_folders` before saving.

---

### P2 — Evaluate Servo re-introduction for cross-platform rendering consistency

**Problem:** Servo was removed in PR #101 (2026-04-28) after blank rendering on an AMD+NVIDIA
hybrid laptop running llvmpipe forced by an over-broad GPU workaround. The removal was the right
pragmatic call, but it gave up the core architectural benefit: consistent email rendering across
macOS, Windows, and Linux (the webkit2gtk iframe renders identically on Linux but the system
WebKit on macOS and WebView2 on Windows will diverge). The `docs/servo-tombstone.md` documents
the removal and the revival path.

**Hypothesis:** Servo 0.1.x is actively developed and the surfman GPU issue filed at
servo/surfman#354 may be resolved or have a cleaner workaround within 6 months. Re-evaluating
Servo as an opt-in rendering path (or as a platform-specific default once the GPU issue has a
real fix) preserves the architectural vision from DESIGN.md §4.5 without requiring it for 0.1.0.

**Scope:**
- This is a research/evaluation proposal, not a build-it-now proposal.
- At or after 0.1.0 tag: check servo/surfman#354 status; test whether Servo 0.1.x (or 0.2.x if
  released) resolves the AMD blend issue with proper GPU gating (only apply llvmpipe override
  when NVIDIA is detected, not on pure AMD/Mesa).
- If the rendering issue is resolved on the available hardware: re-introduce `crates/renderer`
  behind an opt-in feature flag (`--features servo`); validate against the corpus at
  `tests/fixtures/emails/`.
- If the issue is not resolved: update `docs/servo-tombstone.md` with current status; defer to
  the Dioxus Native (Blitz) path described in DESIGN.md §5.2 as the long-term cross-platform
  renderer once that matures.
- Out of scope: shipping Servo as default in 0.1.0; any new compositor work.

**Success signal:** Either (a) `cargo tauri dev --features servo` works correctly on all three
platforms including AMD hardware, documented in `docs/servo-composition.md`; or (b) a clear
"not ready until X" verdict is documented in `docs/servo-tombstone.md` with a specific version
or upstream issue to watch.

**Effort:** S (evaluation + documentation only; the re-implementation is M if evaluation passes)

**Priority:** P2 — architectural vision item; not blocking any user-facing feature. Needs
validation before committing to.

**Pointers:**
- `docs/servo-tombstone.md` — full context of the removal and revival criteria
- `docs/servo-composition.md` — platform-specific embedding findings
- `docs/upstream/surfman-explicit-sync.md` + servo/surfman#354 — upstream bug to track
- `DESIGN.md §4.5` and `§5.2` — architectural rationale

**Architect needed:** Yes — re-introducing Servo involves a dependency choice (which version,
which GPU gating strategy) and affects the `EmailRenderer` trait contract in `TRAITS.md`. Worth a
brief spike doc before any re-implementation.

---

## Open product questions

These are decisions that no existing doc resolves. Each one will affect scope in at least one
proposal above. Resolving them before starting the corresponding work saves significant rework.

1. **Password-only IMAP escape hatch** (`DESIGN.md §12`): The project currently rejects any server
   that requires a password (no OAuth2 support). Do we add a clearly-labelled "I understand the
   risks" escape hatch for homelab users, or hold the OAuth2-only line? This gates the scope of
   Proposal 12 (self-hosted providers) and determines whether Dovecot without OAUTHBEARER is ever
   supported. The Phase 6 roadmap says "decide before Phase 6 to prevent scope creep."

2. **Calendar integration**: `docs/release-1-feature-gap.md` flags the calendar tab as "a
   deliberate scope question for qsl rather than a feature gap." Is QSL mail-only, or will it
   eventually expose CalDAV / JMAP-Calendars as a first-class feature? This determines whether
   the `contacts` table (currently write-only from mail) should also grow a calendar data model,
   and whether the settings window should reserve a Calendar tab.

3. **MCP write tools timing**: The MCP spec defers write tools (`send_message`, `archive`,
   `mark_read`) to after the read-only surface has been used for a few weeks. When is the right
   moment to spec and ship write tools? This depends on whether there are active agent workflows
   on the read path to learn from, or whether write tools should ship concurrently. Decide before
   starting Proposal 2 (MCP server) so the v0 scope boundary is clear to implementers.

4. **Servo GPU-gating strategy for the re-evaluation**: Before Proposal 15 (Servo
   re-introduction) can proceed, a concrete gating strategy is needed. Option A: detect NVIDIA
   via `lspci` at runtime and only apply llvmpipe on confirmed NVIDIA+hybrid setups. Option B:
   don't re-introduce Servo until surfman#354 is fixed upstream. Option C: skip Servo and
   invest in Blitz (Dioxus Native) as the long-term cross-platform renderer instead. Each option
   has different timelines and dependency implications; pick one before spending engineering time.

5. **MCP server sync-loop ownership**: The spec says "use a file lock or SQLite advisory lock
   to coordinate; if QSL UI is running, the MCP binary skips its own sync loop." The
   implementation must pick a specific mechanism (SQLite advisory lock vs. OS file lock vs.
   a running-pid file). This is a small decision but it affects concurrent access safety and needs
   to be made at the start of Proposal 2 implementation, not mid-PR.

