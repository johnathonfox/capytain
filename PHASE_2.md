<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Phase 2 — Write Path, Gmail + Fastmail

**Status:** draft for review. Timeframe: weeks 16–21 (6 weeks). Extended from `DESIGN.md §11`'s original 5-week budget by one to absorb the UI-polish work that surfaced when the read path went end-to-end against a real Gmail account on real hardware (1500+ messages, 30 folders).

Phase 2 turns QSL from "renders mail safely" into "answers it." Compose, draft, attach, send — across both providers, through the same `MailBackend` trait surface that Phase 1 stress-tested for the read path. Most weeks below are write-side; Week 16 is a focused UI polish pass that lands first so subsequent compose / reply / attachments work ships into a coherent visual shell instead of layered on top of the Phase 0 placeholder stylesheet.

The shape of this phase is different from Phase 1. Phase 1 was a platform build: nine weeks of correctness layers stacked on top of an already-proven trait. Phase 2 is feature work — every deliverable is a vertical slice of "user clicks a button, mail leaves the building." That means tight UX scope per week, and most of the integration risk is concentrated in Weeks 18–19 (SMTP / JMAP submission against real servers).

---

## Entry state (what Phase 1 ships on main)

Everything below is live on main at the start of Phase 2. Phase 2 tasks never reprove these:

- **Read path is complete:** Gmail IMAP + Fastmail JMAP via `MailBackend`, sanitized HTML rendering, remote-content blocking, link-click cleaning, threading, unified inbox, live IDLE / EventSource push, optimistic mark-read / flag / move / delete, OS-native notifications, sidebar unread badges.
- **Sync engine + outbox + drain:** `qsl-sync` owns the per-folder + per-account sync loop, the outbox drain (5s tick, exponential backoff, DLQ on `MAX_ATTEMPTS = 5`), and the reactive event channel. The outbox is op-kind-keyed JSON payloads, pluggable for new mutations.
- **Storage:** schema v3 (initial + remote_content_opt_ins + threading_columns) with migration runner. Tables already in place but unused by Phase 1: `outbox` is in active use; `attachments` and `drafts`-shaped storage exist in `DESIGN.md §4.4` as schema sketches but no rows are written yet.
- **MailBackend trait surface:** `save_draft(raw_rfc822) -> MessageId` and `submit_message(raw_rfc822) -> Option<MessageId>` are part of the trait, both currently returning `MailError::Other("not yet implemented")`. Phase 2 fills these in.
- **Auth:** OAuth2 + PKCE for Gmail and Fastmail via `qsl-auth`; access tokens minted on demand and cached in the desktop's backend factory. SMTP XOAUTH2 reuses the same token mint.
- **mailcli + desktop:** all read-path commands. The desktop has a three-pane reader (sidebar / message list / Servo reader pane) with Compose explicitly absent.

**Open deferred from earlier phases** (will be closed during Phase 2 as their prerequisites come due):

- Fastmail OAuth + JMAP smoke test — the live JMAP path is already wired through Phase 1, but the maintainer hasn't yet registered the OAuth client in production. Closes naturally during Week 18 testing.
- Tray icon — Phase 1's explicit deferral. Out of Phase 2 scope; lives in Phase 3 or whenever a maintainer picks it up.
- macOS / Windows runtime validation — still hardware-gated. Compose UI on macOS / Windows is the first Phase-2 piece that genuinely depends on those targets working, so verification is opportunistic across Weeks 16–20.

---

## Objective

By end of Phase 2, the app:

1. Composes a new mail with To/Cc/Bcc/Subject/Body and (optional) attachments, against either Gmail or Fastmail accounts.
2. Sends via SMTP (Gmail) or `EmailSubmission/set` (Fastmail), durably enough that pulling the network mid-send doesn't lose the message.
3. Persists drafts locally and syncs them to the server's Drafts mailbox via IMAP `APPEND` (Gmail) or JMAP `Email/set` with `$draft` keyword (Fastmail).
4. Replies, replies-all, and forwards with correct quoting + threading headers (`In-Reply-To`, `References`).
5. Handles attachments end-to-end — file picker → MIME assembly → server upload → recipient receives intact.
6. Inserts per-account signatures on compose creation; lets the user edit them in account settings.

Search (Tantivy), keyboard shortcuts, rich-text compose, send-later, snooze, and PGP/S-MIME are explicitly **Phase 3+**.

---

## Week-by-week

Each week has a primary deliverable and a "done when" that's a concrete, observable behavior (same shape as `PHASE_0.md` and `PHASE_1.md`'s weekly tables).

| Week | Task | Done when |
|---|---|---|
| 16 | **UI polish.** Replace the Phase 0 placeholder stylesheet with a real visual shell so the rest of Phase 2 ships into a coherent surface instead of bolting fields onto an unstyled scaffold. **Design tokens:** CSS custom properties for surface / text / accent / border / muted / danger colors, spacing scale, type scale; light + dark via `prefers-color-scheme` with a body-class override. **Three-pane layout:** CSS grid `240px / 360px / 1fr`; minimum window width enforced. **Sidebar:** account header w/ initials avatar; folder list grouped (well-known roles above user folders, in the same priority order the watcher pool uses); role icons (`✉ ✓ ✏ 🗑 ⚠ ★`); unread count badge with restrained styling; selected/hover states. **Message list:** sender display name + subject + preview snippet (first 80 chars of `body_text`); date column with relative formatting (`14:32` today, `Mon` this week, `Apr 3` this year, else `2024-12-08`); unread bold + accent dot; selected fill; hover. **Reader pane:** header card (subject H1, from line w/ initials avatar, date right-aligned, To/Cc collapsible), body container with sensible `max-width` and `line-height`; attachment list placeholder (no download yet — that's Week 21). **Topbar:** app title + a disabled "Compose" button placeholder (enabled in Week 17). **Hide the Servo render-test button** behind a `#[cfg(debug_assertions)]` toggle — useful for bring-up, noise in production. New helper `format_relative_date(DateTime<Utc>) -> String` in `apps/desktop/ui/src/format.rs` with unit tests. **No backend / IPC changes.** | Launch the app. Sidebar shows accounts grouped logically with role icons. Message list is scannable at a glance; unread rows visually distinct; dates make sense. Selecting a message renders headers in a card and body in the reader pane. Light/dark theme follows the OS. Resize the window — three-pane layout holds together down to the configured min-width and reflows above it. No "mailcli" hints surface anywhere in normal use. |
| 17 | **Compose window + local drafts.** New `drafts` table (id, account_id, in_reply_to, references_json, to/cc/bcc_json, subject, body, body_kind ('plain'/'markdown'), attachments_json, created_at, updated_at). New repo `crates/storage/src/repos/drafts.rs` + migration `0004_drafts.sql`. Dioxus compose pane (modal-style, replaces middle pane when active) with To/Cc/Bcc/Subject/Body fields plus an account selector. Tauri commands `drafts_save / drafts_load / drafts_list / drafts_delete`. Body is plain text only this week — markdown-to-HTML lands Week 20. Drafts auto-save every 5 seconds while typing; manual "Save" + "Discard" buttons. **No send button yet.** | Click the now-enabled "Compose" button in the topbar; compose pane opens. Type a To address, a subject, and a body. Close the pane. Reopen it from the new `Drafts` sidebar entry — content round-trips intact. Process restart preserves the draft. |
| 18 | **SMTP submission against Gmail (XOAUTH2).** Fill in `crates/smtp-client/src/lib.rs` with `lettre`-backed XOAUTH2 submission on port 587 (STARTTLS, never downgraded) and 465 (implicit TLS) for hosts where 587 isn't available. New `qsl-mime::compose::build_rfc5322(parts) -> Vec<u8>` assembles a sendable byte stream from a `Draft` (Date / Message-ID / From / To / Subject / RFC 2047 encoded headers / `text/plain` body). Implement `MailBackend::submit_message` on `ImapBackend` — it dials `smtp.gmail.com:587` with the same access-token-mint path the IMAP side uses (account_id → token vault → `XOAuth2`). The compose pane gets a "Send" button; on click, the submission goes through a new `outbox` op_kind `submit_message` (so a network blip during send becomes a DLQ entry, not a lost message). After successful send, the draft is deleted locally and an IMAP `APPEND` lands a copy in `[Gmail]/Sent Mail`. | Compose a message to a second mailbox you own. Click Send. The message arrives at the destination with correct headers (no DKIM mangling — Gmail signs on submission for us). The Sent folder in the QSL sidebar grows by one row within ~5s. Pull the network between click and SMTP completion → message lands in the outbox DLQ; reconnecting drains it without duplicating. |
| 19 | **JMAP `EmailSubmission/set` against Fastmail.** Implement `MailBackend::submit_message` on `JmapBackend` via `jmap-client`'s `email_submission_set` (the same `Email/set { create: { ... } }` patch flow PR-A and PR-B used for keyword updates). The compose pane is now backend-agnostic — same Send button drives Gmail or Fastmail depending on the draft's `account_id`. Fastmail handles its own Sent-folder copy server-side via `EmailSubmission`'s `onSuccessUpdateEmail` patch; we don't double-copy. Closes the Phase 0 deferred Fastmail OAuth smoke test. **Reconciliation:** when the server-side Sent-folder copy lands via the existing IDLE / EventSource push, the local Sent row replaces the optimistic one (matched by `Message-ID`). | Compose to a Gmail address from a Fastmail account in QSL. Recipient receives, headers correct. Sent folder in QSL grows; the row matches the server-issued canonical Message-ID. Same network-drop test as Week 18. |
| 20 | **Drafts sync + reply / forward + markdown → HTML body.** Drafts sync upstream: on every local draft save (Week 17), enqueue a `save_draft` outbox op that writes to the server's Drafts folder (IMAP `APPEND` with `\Draft` flag for Gmail; JMAP `Email/set { create: { keywords: { $draft: true }, mailboxIds: { <drafts>: true } } }` for Fastmail). Conflict policy: server-wins on update; local row gets the server-issued id back. **Reply / Reply-All / Forward** seed compose with the right pre-filled headers — `In-Reply-To` + `References` chain — and a quoted body (top-of-cursor, blockquoted prior text). The body field gains a `body_kind: 'plain' | 'markdown'` toggle; markdown bodies pass through `pulldown-cmark` to produce a `multipart/alternative` with both `text/plain` and `text/html` parts. The HTML pass through Phase 1's `sanitize_email_html` before assembly so we don't accidentally weaponize our own outbound mail. | Open a received message, click Reply. Compose pre-fills To, Subject (`Re: …`), and a quoted body. Toggle to markdown, write `**hello**`. Send. Recipient sees both a plain and an HTML alternative; the HTML alt renders bold "hello." Drafts you save in QSL show up in Gmail web's Drafts folder within ~5s. |
| 21 | **Attachments + signatures + final polish.** **Attachments:** file picker (`tauri-plugin-dialog`), inline images via Markdown image syntax (rewritten to `cid:` refs in `multipart/related`), file attachments via `multipart/mixed`. Per-message size cap surfaced from the account's `EmailSubmission` quota (Fastmail) or a hardcoded 25 MB (Gmail). On the receive side: `messages_get` already returns an `attachments: Vec<Attachment>` list from Phase 0; Phase 2 lets the reader pane render filenames + a download button that pulls bytes via `MailBackend::fetch_attachment` (implemented this week — currently stubbed). **Signatures:** new `accounts.signature_text` column (plain text v1; HTML signatures Phase 3). Compose-pane creation appends a `\n-- \n<signature>` separator if non-empty. Settings UI adds a per-account text-area editor. **Final regression:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, manual end-to-end against both providers. Document any new known issues. | Compose a message with a 2 MB PNG attachment to a Gmail address. Recipient receives the attachment intact; the file's SHA-256 matches what was attached. A new draft created in QSL has the configured signature appended. Edit the signature in settings; the next compose picks up the new value. The full release-check pass is green at HEAD. |

---

## Phase 2 Done

By end of week 21:

- Compose / Reply / Reply-All / Forward all work end-to-end against both Gmail and Fastmail.
- Drafts persist locally **and** in the server's Drafts mailbox; deleting a draft anywhere syncs across.
- Attachments round-trip intact in both directions (sending and receiving).
- Per-account signatures auto-insert.
- The outbox pattern from Phase 1 cleanly absorbed three new op_kinds (`save_draft`, `submit_message`, `update_draft`) with no architectural changes — proves the abstraction is the right shape.
- One full release-check pass on main: `cargo clippy --workspace --all-targets` + `cargo test --workspace` green; no `#[ignore]` tests added since Phase 2 began.

At this point Phase 3 (Tantivy search, keyboard shortcuts, rules, themes, onboarding) becomes the next platform-build slice.

---

## Phase 2 Deliverables Summary

| Deliverable | Path |
|---|---|
| Visual shell (design tokens, three-pane grid, typography) | `apps/desktop/ui/assets/tailwind.css`, `apps/desktop/ui/src/app.rs` |
| Relative-date formatter | `apps/desktop/ui/src/format.rs` |
| Drafts table + repo | `crates/storage/migrations/0004_drafts.sql`, `crates/storage/src/repos/drafts.rs` |
| RFC 5322 message assembly | `crates/mime/src/compose.rs` |
| SMTP XOAUTH2 client | `crates/smtp-client/src/lib.rs` |
| `submit_message` on IMAP backend | `crates/imap-client/src/backend.rs` |
| `submit_message` on JMAP backend | `crates/jmap-client/src/lib.rs` |
| `save_draft` on both backends | same files as above |
| `fetch_attachment` on both backends | same files as above |
| New outbox op_kinds | `crates/sync/src/outbox_drain.rs` |
| Compose / Reply / Forward UI | `apps/desktop/ui/src/compose.rs` (new module under `app.rs`) |
| Markdown → HTML body | `crates/mime/src/compose.rs` (uses `pulldown-cmark`) |
| Attachment storage + download | `crates/storage/src/repos/attachments.rs` (already-sketched table comes alive) |
| Per-account signatures | `crates/storage/migrations/0005_signatures.sql`, settings UI |

---

## Phase 2 Non-Goals

Explicitly **not** in Phase 2 (these belong to Phase 3 or later):

- Full-text search (Tantivy) — Phase 3.
- Keyboard shortcuts + remapping UI — Phase 3.
- Themes / preferences UI beyond signatures + per-account toggles — Phase 3.
- Onboarding flow — Phase 3.
- Send-later / scheduled send — Phase 7.
- Snooze — Phase 7.
- Rich-text WYSIWYG editor — never; Markdown is the v1 ceiling. WYSIWYG can be a Phase-3-or-later plugin.
- HTML signatures — Phase 3 (plain text in v1).
- Templates / canned responses — Phase 3 or later.
- Mailing-list helpers (List-Unsubscribe one-click, etc.) — Phase 3 or later.
- PGP/MIME or S/MIME signing — Phase 7+.
- Multiple windows (pop-out compose) — never; design decision per `PHASE_0.md` Non-Goals. Compose is in-pane.

---

## Open questions for during Phase 2

Decisions that are safe to defer into the weekly work but should be named explicitly:

- **Compose body format.** Plain text vs Markdown vs both-at-once. Default proposal: per-message toggle defaulting to plain; markdown emits `multipart/alternative`. Revisit if the toggle ergonomics are bad.
- **Reply quoting style.** Top-post (cursor above quote, quote at bottom) vs bottom-post (cursor below quote) vs interleaved. Default proposal: top-post with `> `-prefixed plain-text quote, `<blockquote>` for HTML. Configurable in settings is a Phase-3 nice-to-have, not a Phase-2 requirement.
- **Drafts conflict policy.** What happens if the same draft was edited in two clients? Default proposal: last-write-wins on save, with the local copy keeping a `dirty: bool` until the upstream save round-trips. JMAP's `Email/set` already returns a `notUpdated` map for conflicts; the engine treats those as a forced re-pull.
- **Attachment storage location.** Inline in the draft row's `attachments_json` (small files) vs spilled to the blob store (>1 MB)? Default proposal: spill threshold of 256 KB so the JSON-serialize round-trip stays cheap; large files live as file paths under `<data_dir>/drafts/`.
- **Signature scope.** Per-account only (simplest) vs per-account + per-identity (when From-aliases land later) vs per-thread overrides. Default proposal: per-account in v1; per-identity falls out naturally when aliases ship in Phase 3+.
- **Bounce handling.** SMTP rejects (5xx, mailbox-not-found, over-quota) need surfacing. Default proposal: outbox DLQ catches them via the existing pattern; UI banner reads the `last_error` directly. A polished bounce-detail UI is Phase 3.
- **`Sent` folder reconciliation.** Gmail auto-copies via SMTP's hidden BCC-to-self trick, Fastmail uses `EmailSubmission`'s `onSuccessUpdateEmail`. The optimistic local Sent row needs to merge with the server-issued canonical row when it arrives via IDLE/EventSource. Default proposal: match by RFC 5322 `Message-ID` (we control the value at compose time); duplicates merge into the canonical row. Edge case: provider rewrites the Message-ID. Punt to a Phase 3 known-issue if observed.
- **Markdown safety.** `pulldown-cmark` doesn't permit raw HTML by default; we keep that posture. If a user wants `<details>` or similar, they'd need to switch to plain. Worth surfacing in the compose-mode toggle help text.
