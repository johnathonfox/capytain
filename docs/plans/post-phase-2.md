# Post-Phase 2 — v0.1 Cut Plan

> **Status:** draft, ready to execute. Authored 2026-04-27 against the
> `release-1-feature-gap.md` "Suggested v0.1 cut" list.
>
> **Scope.** Take QSL from "Phase 2 functional" to "feels like a real
> email client" — the 9 items at the bottom of the gap analysis.
> Sequenced as independently mergeable PRs against `main`.
>
> **Authority.** This plan makes assumptions where the gap analysis,
> `DESIGN.md`, and existing phase docs don't specify. Each assumption
> is documented inline. Load-bearing assumptions (expensive to reverse)
> are flagged with **🔒 LOAD-BEARING**.

---

## Open product questions

These are decisions that aren't dictated by the existing docs and that I
can't reasonably infer from convention. Resolve before starting the
related PRs; everything else proceeds with the default proposed below.

1. **Calendar / contacts apps in v0.1?** The gap analysis flags this as
   a deliberate scope question rather than a gap. Default proposed
   below: **out of v0.1**, both calendar UI and CardDAV/JMAP-Contacts
   sync. Address autocomplete uses a write-only contact store seeded
   from incoming/outgoing mail (Item 5 below). Confirm.

2. **Address autocomplete source.** Default proposed: **write-only
   contact collection** — every `From:` we render and every `To:` we
   send seeds a `contacts_v1` table; autocomplete queries that. CardDAV
   / JMAP Contacts is v0.2. The trade-off: write-only means the user
   only sees suggestions for addresses they've already encountered
   (hostile to first-day usage); CardDAV is "real" contacts but ~2
   weeks of work. **🔒 LOAD-BEARING** — schema decision propagates to
   every compose interaction.

3. **Search indexing strategy.** Default proposed: **lazy on first
   search**. Hits a one-time "indexing your mail…" wait (~10s on
   1500 messages, measured proxy: existing storage scan rates) but
   ships as one PR. Eager-on-sync is cleaner UX but threads through
   the sync engine and adds a checkpoint table. **🔒 LOAD-BEARING** —
   choice changes whether the sync engine writes FTS rows.

4. **Settings panel placement.** Default proposed: **dedicated Tauri
   window** opened from a gear icon in the topbar. Rationale: matches
   Phase 2's pop-out reader window pattern (Tauri runtime is now
   multi-window-capable per `feat/reader-popup-window`); the main
   three-pane UI stays simple; settings can be tabbed internally
   (Accounts / Appearance / Notifications / Shortcuts) without
   competing for grid space. Alternative: in-pane route. The
   in-pane route is cheaper but makes "open settings while reading"
   require leaving the message.

---

## Status of the v0.1 cut

For each gap-analysis item, what's already on `main` and what remains.

| # | v0.1 item | Already shipped | Remaining work |
|---|---|---|---|
| 1 | Conversation threading | Data model: P1 W13. `MessageHeaders.thread_id` flows to UI. | UI grouping: thread row (collapsed), expanded thread reader, toggle |
| 2 | Search (local FTS5) | Nothing. Turso ships SQLite FTS5. | Schema, indexer, parser, search UI |
| 3 | Message-actions toolbar | Backend: `messages_mark_read / messages_flag / messages_move / messages_delete` (P1 W14). | Toolbar component, right-click menu, keyboard shortcuts |
| 4 | Multi-select | Nothing. | Selection set state, UI affordance, bulk-apply path |
| 5 | Compose depth | P2 W17–W21: drafts, attachments, signatures, reply/forward, markdown body. | Address autocomplete + minimal contact store; Cc/Bcc reveal (already in W17 per docs — verify) |
| 6 | Notification content | OS-native via `tauri-plugin-notification` (P1 W15). Body is `"{account} · {folder}"`. | Sender/subject in body, action buttons |
| 7 | Multi-account + switcher | Backend: P1 W11. `accounts_list` works. Sidebar groups by account. | Account switcher UI (compact mode for desktop dropdown; doesn't apply when sidebar already shows all accounts — see assumption A1) |
| 8 | Settings panel | Nothing visible. `accounts_add_oauth` etc. stubs in `commands/accounts.rs`. | Window + tabs, account add/remove, signature edit, notification toggles, shortcuts viewer |
| 9 | First-run OAuth in UI | Nothing. CLI-only via `mailcli auth add`. | Empty-state CTA, OAuth-flow window/popup, token persist, kick first sync |

---

## Assumptions

Listed once at the top so each PR doesn't re-litigate them.

- **A1 — Multi-account "switcher" means filter, not swap.** The
  sidebar already shows every account's folders side-by-side
  (Phase 1 W12 unified inbox). A "switcher" in this context is a
  filter chip in the topbar that scopes the message list / unified
  inbox to one account, not a swap of which account is "active."
  **Why:** swap-style switchers fit single-account-at-a-time clients
  (Apple Mail). QSL's existing UI is panoptic — switching to swap
  would be a regression. *Not load-bearing — easy to flip later.*

- **A2 — Threading toggle defaults ON.** Modern users expect Gmail-
  style thread collapsing. Provide a Settings toggle for users who
  want flat. *Not load-bearing.*

- **A3 — Keyboard shortcuts follow Gmail conventions.** `j`/`k`
  navigate, `e` archive, `#` delete, `r` reply, `a` reply-all, `f`
  forward, `/` focus search, `?` help overlay, `c` compose, `Esc`
  cancel. **Why:** widely known; collisions with Servo are limited
  (Servo only consumes input on hover, focus-in-Dioxus dominates).
  *Not load-bearing — remappable in Settings (Phase 3).*

- **A4 — Multi-select pattern is checkbox-on-hover.** Hovering a
  message row reveals a checkbox; clicking it (or shift-clicking
  a range) selects. The toolbar shifts to bulk mode when ≥1 row is
  checked. **Why:** Gmail / Fastmail / Outlook web all do this;
  it's lower visual noise than always-visible checkboxes and
  carries a known mental model. *Not load-bearing.*

- **A5 — Notification actions: Linux gets two (`Mark read`,
  `Archive`); macOS / Windows get the title + body only.**
  `tauri-plugin-notification` exposes actions on Linux via
  libnotify capabilities; macOS UNUserNotificationCenter and
  Windows toast support exist but require platform-specific
  scaffolding the plugin doesn't expose uniformly. v0.1 takes the
  Linux win and defers cross-platform parity. *Not load-bearing.*

- **A6 — Search starts as local-only.** The "Search all mail on
  server" affordance from the gap doc's design notes is **deferred
  to v0.2**. Reason: local-only ships as one PR; adding the
  server-side fallback path doubles the surface (Gmail `X-GM-RAW`
  and JMAP `Email/query` integrations, results merging, "did you
  mean" UX). *Not load-bearing — additive later.*

- **A7 — First-run OAuth lives in a dedicated window** opened from
  Settings → Accounts → "Add account." For the empty-app case
  (no accounts configured yet), the main window shows an empty
  state with an "Add an account" button that opens the same
  window. **Why:** matches Phase 2's multi-window posture; the
  OAuth callback already runs a localhost loopback (per
  `qsl-auth`) so the OS browser handles the redirect — the QSL
  window just shows a "waiting for browser approval…" state.
  *Not load-bearing.*

- **A8 — Compose Cc/Bcc reveal already works.** W17 docs say the
  compose pane has Cc/Bcc fields. Verify in the audit step of
  PR-5a; if missing, fold a one-line fix into PR-5a.
  *Not load-bearing.*

---

## Pre-existing regressions (must land before v0.1 work)

Two screenshots (current `qsl` build vs older `Capytain` build, from the
same Gmail account) surfaced regressions that aren't features in the
gap analysis — they're things that worked before and don't now. Land a
**PR-0 regression sweep** in front of every other PR below.

### PR-R1 — Folder role classification (M)

**Symptom.** Sidebar `MAILBOXES` section shows only `INBOX` and
`All Mail`. The other Gmail system folders (`[Gmail]/Drafts`,
`[Gmail]/Sent Mail`, `[Gmail]/Spam`, `[Gmail]/Starred`,
`[Gmail]/Trash`, `[Gmail]/Important`) all appear in `LABELS`
instead. Old Capytain build had all 7 in `MAILBOXES`.

**Investigation result (static analysis, ~2026-04-27).**
- `~/.local/share/qsl/qsl.db` has the correct rows: `[Gmail]/Drafts`
  with `role='drafts'`, `[Gmail]/Sent Mail` with `role='sent'`,
  etc. All 7 are present and correctly tagged.
- `folders_repo::list_by_account` returns every row, no filter.
- `split_mailboxes_labels` correctly maps `Some(FolderRole::Drafts)`
  through `Sent / Spam / Trash / Flagged` to mailboxes.
- `MailboxRoleIcon` has SVG glyphs for every relevant role.
- `Folder` and `FolderRole` are vanilla `serde`-derived (PascalCase
  variant names on the wire).

The DB is right and the code path looks right under static reading.
The bug is therefore runtime: most likely either (a) a serde
mismatch I'm not seeing — `FolderRole` lives in `qsl-core` but
`qsl-ipc` re-exports it; if some consumer in the IPC layer
deserializes against a stale shape, roles arrive as `None` — or
(b) a stale `dist/` bundle for the Dioxus UI baking in old code.

**Fix shape (to be confirmed during implementation).**
1. Add `tracing::info!` in `folders_list` printing
   `(id, role)` for every folder returned. Re-run; compare to DB.
2. If roles arrive at the IPC boundary correctly but lose their
   variant in the UI, fix the serde shape (likely needs an
   explicit `#[serde(rename_all = "snake_case")]` on
   `FolderRole` so wire format matches what the UI deserializer
   expects, or a corresponding update on the deserializer side).
3. If roles are `None` at the IPC boundary, the bug is upstream —
   investigate `row_to_folder` in `folders_repo`.

- Files (anticipated):
  - `apps/desktop/src-tauri/src/commands/folders.rs` — diagnostic
    tracing first; remove after fix verified
  - Either `crates/core/src/folder.rs` (serde rename) or `crates/
    storage/src/repos/folders.rs` (round-trip fix), depending
    on diagnosis
- Verification:
  - DB query `SELECT id, role FROM folders WHERE account_id = ?`
    returns the same `(id, role)` set as the UI sidebar's
    `MAILBOXES` section
  - All 7 Gmail system folders + INBOX visible in `MAILBOXES`
  - Labels section contains only user-defined labels
- Est. size: M (mostly investigation; code change likely small)

### PR-R2 — Reader pane stuck on "Loading…" (S–M)

**Symptom.** Selecting a message in the main window sometimes
leaves the reader pane stuck on the "Loading…" text indefinitely.
Visible in the same screenshot that surfaced PR-R1.

**Investigation needed.** Almost certainly downstream of recent
work on this branch. Suspects in priority order:

1. **Per-folder token map (PR #75):** if the message-list refetch
   storm fix accidentally also gates the reader's `messages_get`
   call, the resource can stay in `None` (Loading) until a token
   bump that never comes. Read `MessageListV2`'s `use_reactive!`
   key list.
2. **Popup-window branch leftover state:** the App root now branches
   on `__QSL_READER_ID__` to mount `ReaderOnlyApp` (popup) vs
   `full_app_shell` (main). If the main window's reader code path
   has accidentally been kept on the popup-only branch's load
   state, the resource never resolves. Read `ReaderV2` and check
   what populates the reader against what the popup populates.
3. **Servo install for popup main thread:** unlikely to affect the
   main window (its install completed at app boot), but check the
   logs for `reader_render: no renderer installed` warnings.

**Fix shape.** Ship a focused regression test (Dioxus mounting
`ReaderV2` against a fixed message id, asserting `messages_get`
resolves). Whatever the fix, make sure it's covered.

- Files (anticipated):
  - `apps/desktop/ui/src/app.rs` — `ReaderV2` + dependencies
  - Possibly `apps/desktop/src-tauri/src/commands/messages.rs`
    if `messages_get` has a state-machine bug
- Verification:
  - Click any message in the main window → body renders within
    ~500 ms (or shows a real error)
  - Switching messages always lands a render; never stuck on
    Loading
- Est. size: S–M

### PR-R3 — Reader header action buttons (S)

**Symptom.** Reader header card has no Reply / Archive buttons.
Old Capytain build had them inline. The reader-header CSS
(`.reader-actions`, `.reader-action`) is in the stylesheet; the
component just doesn't render them.

**Note.** This PR effectively *is* the smaller half of PR-T1 below
(the action toolbar). Decision: ship the inline header buttons
here as a regression fix using the existing CSS classes, and let
PR-T1 expand to the full 6-button toolbar (Move popover, Label
popover, etc.). The reader header will keep Reply / Archive plus
gain the others in PR-T1.

- Files:
  - `apps/desktop/ui/src/app.rs` — `ReaderV2` header rsx adds
    `.reader-actions` block with two buttons; wire to the
    existing `messages_move` (archive) and reply-open path
- Verification:
  - Reader header shows Reply + Archive buttons on the right
  - Click Reply → compose pane opens with message pre-filled
  - Click Archive → message moves to Archive folder, reader
    clears, message-list refreshes
- Est. size: S

---

## Sequencing rationale

Rough order of dependencies:

```
PR-R1  (folder roles)  ─┬── ship FIRST. blocks all sidebar work.
PR-R2  (reader Loading) ┤── independent of R1; same urgency
PR-R3  (reader buttons) ┤── trivial; bundle with R2 if convenient
                        │
PR-Tx  (toolbar + shortcut scaffold)  ─┐
PR-Mx  (multi-select + bulk-apply)     ├── independent of search/threading
PR-Nx  (notification content)          │
                                       │
PR-Hx  (threading UI)                  │── reads thread_id from existing data
                                       │
PR-Sx  (search FTS5)                   │── new schema, isolated subsystem
                                       │
PR-Cx  (compose autocomplete)          │── new contacts_v1 table, isolated
                                       │
PR-Ax  (account switcher chip)         │── pure UI on existing account state
                                       │
PR-Px  (settings panel)               ─┴── consumes most of the above
                                          (signature edit hits W21 work,
                                          shortcuts viewer needs PR-Tx,
                                          notification toggles need PR-Nx)
PR-Ox  (first-run OAuth)              ─── builds on settings → accounts tab
```

The settings panel (PR-P) and first-run OAuth (PR-O) are **last**
because they re-export functionality the earlier PRs add. Building
them first would mean stubs and re-work.

Everything else can happen in any order; pick by appetite. Suggested
order roughly tracks user-visible impact:

1. **Threading UI (PR-H1, PR-H2)** — biggest "doesn't feel like 2005"
   improvement and reads from data already on disk.
2. **Toolbar + shortcuts (PR-T1, PR-T2)** — unblocks daily power-user
   workflows; the actions already work.
3. **Multi-select (PR-M1)** — pairs with toolbar to make bulk ops
   scale.
4. **Search (PR-S1, PR-S2, PR-S3)** — biggest absent feature; benefits
   from threading shipping first (search results group by thread).
5. **Notification content (PR-N1)** — small, high-value polish.
6. **Compose autocomplete (PR-C1, PR-C2)** — needs contacts table; one
   focused PR-pair.
7. **Account switcher chip (PR-A1)** — pure UI; quick win.
8. **Settings panel (PR-P1, PR-P2)** — gates everything that needs a
   settings surface.
9. **First-run OAuth (PR-O1, PR-O2)** — last; depends on PR-P1.

---

## PR-by-PR plan

Each PR lists: **scope**, **files**, **verification**, **est. size**.
Size is meaningful-line-count of new/changed code, excluding tests
(rough order: S = ≤200 LOC, M = 200–600, L = 600+).

### Threading UI

**PR-H1 — Thread grouping in the message list (M)**

Group adjacent same-thread messages in `MessageListV2` into a single
collapsed row that shows the latest message's metadata plus a count
badge (`3`). Clicking expands inline; clicking again collapses. Keep
the per-message `MessageRowV2` shape — the thread row is a wrapper.

A new `Tauri` command isn't needed: `messages_list` already returns
`MessageHeaders` with `thread_id`. Group client-side by walking the
already-paginated list.

- Files:
  - `apps/desktop/ui/src/app.rs` — new `ThreadRow` component, `group_by_thread()` helper, modify `MessageListV2` body
  - `apps/desktop/ui/assets/tailwind.css` — `.thread-row`, `.thread-count`, `.thread-expanded` styles
- Verification:
  - Inbox with 3 reply-chain messages renders as one row with a `3` badge
  - Clicking the row expands to show all 3 messages indented
  - Selection still works on individual messages within the expanded thread
  - Unread state on any message in the thread → thread row shows unread
- Est. size: M

**PR-H2 — Thread reader (M)**

When the selected message's `thread_id` resolves to ≥2 messages, the
reader pane shows a stacked-card view: each message in the thread
gets its own collapsed header card, with the most recent expanded by
default. Clicking a card toggles its expansion.

Backend addition: a `messages_list_thread(thread_id) -> Vec<MessageHeaders>`
IPC command that returns every message sharing a thread id, ordered
by date.

- Files:
  - `apps/desktop/src-tauri/src/commands/messages.rs` — new `messages_list_thread`
  - `crates/storage/src/repos/messages.rs` — `list_by_thread(thread_id)` query (already-existing `messages_thread` index supports this)
  - `apps/desktop/src-tauri/src/main.rs` — register the new command
  - `apps/desktop/ui/src/app.rs` — `ReaderV2` branches on thread length; new `ThreadReader` component
- Verification:
  - Open a 3-message thread → reader shows 3 cards, top one expanded
  - Click a collapsed card → it expands and pushes the others up
  - Reply from any card seeds the right `In-Reply-To`
- Est. size: M

### Toolbar + shortcuts

**PR-T1 — Action toolbar in the reader header (S)**

Add a horizontal toolbar above the reader header card with 6 buttons:
Archive, Delete, Mark unread/read (state-toggling), Flag, Move,
Label. Each calls the corresponding existing IPC command. Tooltips
include the keyboard shortcut (added in PR-T2).

Move and Label open small popovers anchored to their button:
- Move: lists folders for the active account, click to move
- Label: lists labels (Gmail) or flags (`\Flagged`) for the account, multi-toggle

- Files:
  - `apps/desktop/ui/src/app.rs` — `ActionToolbar`, `MovePopover`, `LabelPopover`
  - `apps/desktop/ui/assets/tailwind.css` — toolbar styles
- Verification:
  - Each button calls the right IPC and the message-list reflects the change after the next sync tick
  - Popovers close on outside click
  - Toolbar disappears when no message is selected
- Est. size: S

**PR-T2 — Keyboard shortcut layer (M)**

Global `keydown` handler dispatching the Gmail-style shortcuts (per
**A3**). A `?` overlay shows a cheatsheet when pressed.

- Files:
  - `apps/desktop/ui/src/keyboard.rs` (new) — `KeyboardCommand` enum, dispatcher
  - `apps/desktop/ui/src/app.rs` — wire the dispatcher at the App root via Dioxus's `onkeydown`
  - New: `ShortcutsOverlay` component
  - `apps/desktop/ui/assets/tailwind.css` — overlay styles
- Verification:
  - `j`/`k` navigates next/prev row (selection wraps)
  - `e` archives, `#` deletes, `r` opens reply, `a` opens reply-all, `f` forwards
  - `c` opens compose, `Esc` cancels
  - `?` shows the overlay; pressing again hides it
  - Shortcuts don't fire while the user is typing in any `<input>` or `[contenteditable]`
- Est. size: M

### Multi-select

**PR-M1 — Multi-select with bulk apply (M)**

Hovering a message row reveals a checkbox in the avatar slot
(checkbox replaces avatar on hover, comes back when not hovered and
not checked). Selected set persists across hover. Shift-click
selects a range. When ≥1 message is checked, the action toolbar
shifts into bulk mode (its actions apply to all checked messages).

- Files:
  - `apps/desktop/ui/src/app.rs` — `selected_set: Signal<HashSet<MessageId>>`, `MessageRowV2` checkbox slot, toolbar branches on `selected_set.read().len() > 0`
  - `apps/desktop/ui/assets/tailwind.css` — `.msg-row-checkbox`, hover transitions
- Verification:
  - Click 3 checkboxes → toolbar shows "3 selected" + bulk Archive
  - Click bulk Archive → all 3 archive
  - Shift-click between two rows → range selects
  - `Esc` clears the selection
- Est. size: M

### Search

**PR-S1 — FTS5 schema + indexer (M)**

New migration `0007_search_fts.sql` (next free number — 0006 is
signatures per W21):

```sql
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject,
    from_addr,
    to_addr,
    body,
    content='',
    tokenize='unicode61 remove_diacritics 2'
);
```

Indexing path: lazy. First search invocation triggers a one-time
backfill that walks the messages table and inserts FTS rows. A
`search_index_state` table tracks high-watermark. After backfill,
ongoing inserts/updates from the sync engine maintain the FTS rows
via repo write-side hooks.

Per **assumption A6**, no server-side fallback in v0.1.

- Files:
  - `crates/storage/migrations/0007_search_fts.sql`
  - `crates/storage/src/repos/search.rs` (new) — `index_message`, `delete_indexed`, `query`
  - `crates/storage/src/repos/messages.rs` — call `search::index_message` on insert
  - `crates/sync/src/lib.rs` — same on the sync-engine path
- Verification:
  - Cold run: first search blocks ~5s with "Indexing…" toast then returns results
  - Warm run: subsequent searches return in <100 ms
  - Edit a message (mark read) → FTS row stays in sync
- Est. size: M

**PR-S2 — Search query parser (S)**

Parse Gmail-style operators (`from:`, `to:`, `subject:`,
`has:attachment`, `before:`, `after:`, `is:unread`, `in:label`)
into FTS5 MATCH expressions plus a structured filter for
non-FTS predicates (date, labels, unread state).

- Files:
  - `crates/search/src/lib.rs` (new) — `Query` AST, `parse(input: &str) -> Query`, `to_sql(query) -> (String, Vec<Param>)`
  - `Cargo.toml` workspace member entry
  - `crates/storage/src/repos/search.rs` — `query` accepts `Query` instead of raw string
- Verification:
  - `from:alice subject:invoice` → SQL with FTS MATCH + structured filter
  - `is:unread before:2026-01-01` → no FTS, just structured filter
  - Bare term `invoice` → FTS MATCH on all columns
- Est. size: S

**PR-S3 — Search UI (M)**

Add a search input in the topbar (`/` focuses it per A3). Results
view replaces the message list while the input is non-empty; `Esc`
or empty input restores the folder view. Result rows render the
same `MessageRowV2` shape with the matched query terms highlighted.

- Files:
  - `apps/desktop/ui/src/app.rs` — `SearchBar`, `SearchResultsList` components, `search_query: Signal<String>` at App root
  - `apps/desktop/src-tauri/src/commands/search.rs` (new) — `search_messages(query: &str, limit: u32, offset: u32) -> Vec<MessageHeaders>`
  - `apps/desktop/src-tauri/src/main.rs` — register
- Verification:
  - Type "invoice" → results list shows matching messages from any folder
  - `Esc` clears search and restores folder view
  - Click a result → reader pane opens that message normally
- Est. size: M

### Notifications

**PR-N1 — Richer notification content + Linux actions (S)**

Replace `fire_new_mail_notification` body with `"{from} — {subject}"`.
Add Linux action buttons: `Mark read`, `Archive` (per A5). Each
action invokes `messages_mark_read` or `messages_move` on the
notified message id. macOS / Windows continue with title + body
only.

For multi-message bursts (count > 1), revert to the current
"{count} new messages" body without actions.

- Files:
  - `apps/desktop/src-tauri/src/sync_engine.rs` — `fire_new_mail_notification` rewrite, take a `MessageHeaders` ref instead of just count
  - `apps/desktop/src-tauri/Cargo.toml` — `tauri-plugin-notification` features check
- Verification:
  - Receive a single message → notification reads "Alice Cohen — Project status"
  - Click "Mark read" action on Linux → message flips to seen
  - Receive 5 messages in one IDLE burst → "5 new messages" without actions
- Est. size: S

### Compose autocomplete

**PR-C1 — Contacts table + write-only collection (S)**

New migration `0008_contacts.sql`:

```sql
CREATE TABLE contacts_v1 (
    address       TEXT PRIMARY KEY COLLATE NOCASE,
    display_name  TEXT,
    last_seen_at  INTEGER NOT NULL,
    seen_count    INTEGER NOT NULL DEFAULT 1,
    source        TEXT NOT NULL  -- 'inbound' | 'outbound'
);
```

Hooks:
- Sync engine: every incoming message's `From:` upserts (`source='inbound'`)
- `messages_send`: every `To:`/`Cc:`/`Bcc:` upserts (`source='outbound'`)
- `display_name` keeps the most recent non-empty value seen

- Files:
  - `crates/storage/migrations/0008_contacts.sql`
  - `crates/storage/src/repos/contacts.rs` (new) — `upsert_seen`, `query_prefix`
  - `crates/sync/src/lib.rs` — call `upsert_seen` on every message insert
  - `apps/desktop/src-tauri/src/commands/messages.rs` — same on `messages_send`
- Verification:
  - Receive 3 messages → 3 rows in `contacts_v1` with `source='inbound'`
  - Send a message to a new address → row appears with `source='outbound'`
- Est. size: S

**PR-C2 — Compose autocomplete UI (M)**

In the compose pane's `AddressField`, typing ≥2 characters opens a
dropdown of matching contacts (prefix match on `address` and
`display_name`, ordered by `last_seen_at DESC, seen_count DESC`).
Up/Down navigates, Enter / Tab inserts. New IPC command
`contacts_query(prefix: &str, limit: u32)`.

- Files:
  - `apps/desktop/src-tauri/src/commands/contacts.rs` (new)
  - `apps/desktop/src-tauri/src/main.rs` — register
  - `apps/desktop/ui/src/app.rs` — `AddressField` gets `ContactsDropdown`
  - `apps/desktop/ui/assets/tailwind.css` — dropdown styles
- Verification:
  - Type "ali" in `To:` → dropdown shows alice@example.com, alistair@…
  - Up/Down navigates; Enter inserts the highlighted entry
  - Click outside closes the dropdown
- Est. size: M

### Account switcher chip

**PR-A1 — Topbar account filter chip (S)**

New chip in the topbar showing the currently-filtered account (or
"All Accounts"). Clicking opens a dropdown listing every configured
account; selecting one filters the message list and unified inbox to
that account. Selecting "All Accounts" restores the unfiltered view.

This filter is purely UI state — no backend changes; the existing
`messages_list_unified` already returns `account_id` per row.

- Files:
  - `apps/desktop/ui/src/app.rs` — `AccountFilterChip`, `account_filter: Signal<Option<AccountId>>` at App root, message-list components consume it
  - `apps/desktop/ui/assets/tailwind.css` — chip styles
- Verification:
  - With 2 accounts configured, chip shows "All Accounts"
  - Click chip, pick Account A → message list shows only Account A's messages
  - Sidebar still shows all accounts (assumption A1 — chip is filter, not swap)
- Est. size: S

### Settings panel

**PR-P1 — Settings window scaffold (M)**

New Tauri window `settings` opened by a gear icon in the topbar.
Window mounts a Dioxus tree with a tab strip: Accounts / Appearance
/ Notifications / Shortcuts / Privacy. Each tab is a stub for now
except Shortcuts (read-only viewer of the keymap from PR-T2).

The window opens via `messages_open_in_window`-style pattern (see
the popup-reader work) — a dedicated `settings_open` IPC command
that builds a `WebviewWindowBuilder` with `__QSL_VIEW__ = 'settings'`
in the init script. App root branches on it the same way the popup
reader does.

- Files:
  - `apps/desktop/src-tauri/src/commands/settings.rs` (new) — `settings_open`
  - `apps/desktop/src-tauri/src/main.rs` — register
  - `apps/desktop/ui/src/settings.rs` (new) — `SettingsApp` + tab components
  - `apps/desktop/ui/src/app.rs` — App root branches on `__QSL_VIEW__` (extends the existing popup-reader branch)
- Verification:
  - Click gear → settings window opens
  - Tabs visible; Shortcuts tab shows the full keymap
  - Other tabs render a "coming soon" placeholder
- Est. size: M

**PR-P2 — Settings tab content (M)**

Wire each tab to working state:

- **Accounts tab:** list configured accounts (`accounts_list`), each
  row shows display name + email + provider; per-row "Remove" and
  "Edit signature" buttons. Header has "Add account" → opens
  first-run OAuth window (PR-O1).
- **Appearance tab:** dark/light/system toggle (writes to
  `app_settings_v1` table — new migration). Density toggle
  (compact/comfortable) — adjusts row padding.
- **Notifications tab:** master enable/disable, per-account toggle
  (writes to `accounts.notify_enabled` — adds the column).
- **Shortcuts tab:** already done in PR-P1, no change.
- **Privacy tab:** "Always load remote images" master toggle (the
  existing per-sender opt-in stays available from the reader's
  banner).

Backend additions:
- `accounts_set_display_name`, `accounts_set_signature`,
  `accounts_set_notify_enabled`, `accounts_remove`
- New migration `0009_app_settings.sql` with `app_settings_v1
  (key TEXT PRIMARY KEY, value TEXT NOT NULL)` and an
  `accounts.notify_enabled INTEGER NOT NULL DEFAULT 1` column

- Files:
  - `crates/storage/migrations/0009_app_settings.sql`
  - `crates/storage/src/repos/app_settings.rs` (new)
  - `apps/desktop/src-tauri/src/commands/accounts.rs` — implement the deferred commands
  - `apps/desktop/src-tauri/src/commands/settings.rs` — `app_settings_get`, `app_settings_set`
  - `apps/desktop/src-tauri/src/main.rs` — register
  - `apps/desktop/ui/src/settings.rs` — fill in tab content
- Verification:
  - Toggle dark→light → app reflects within ~one frame (writes to settings, App reads on render)
  - Edit signature → next compose has the new value
  - Disable notifications for one account → no notifications fire for it
  - Remove an account → it disappears from sidebar after re-list
- Est. size: M

### First-run OAuth

**PR-O1 — OAuth window + provider picker (M)**

When `accounts_list` returns empty, the main window renders an
empty-state with "Add an account" button. Clicking opens a new
`oauth-add` window with a provider picker (Gmail, Fastmail). Click a
provider → window starts the existing `qsl-auth` loopback flow,
shows "Waiting for browser approval…", and on success creates the
account row + kicks the first sync.

`accounts_add_oauth` (already stubbed in `commands/accounts.rs`) is
the IPC seam — implement it to call the existing CLI-side flow logic
in `qsl-auth`.

- Files:
  - `apps/desktop/src-tauri/src/commands/accounts.rs` — implement
    `accounts_add_oauth`
  - `apps/desktop/ui/src/oauth_add.rs` (new) — `OAuthAddApp` component
  - `apps/desktop/ui/src/app.rs` — empty-state branch in main window;
    branch on `__QSL_VIEW__ = 'oauth-add'`
- Verification:
  - Fresh data dir → main window shows "Add an account"
  - Click → oauth-add window opens, provider picker visible
  - Pick Gmail → browser opens, approve → window closes, sidebar populates with the new account, first sync runs
- Est. size: M

**PR-O2 — Account-add flow inside Settings (S)**

Settings → Accounts → "Add account" button reuses the same
`oauth-add` window from PR-O1. No new functionality — wire the
button to `accounts_add_oauth` open call.

- Files:
  - `apps/desktop/ui/src/settings.rs` — wire the button
- Verification:
  - From Settings, click Add account → same oauth-add flow as
    first-run; new account appears in the list when done
- Est. size: S

### UI overhaul

**PR-U series — monospace, density-first, warm-dark chrome**

Direction-shift refresh of every chrome surface per
[`docs/ui-direction.md`](../ui-direction.md). Read that file before
opening any PR in this series. Acceptance criteria are mirrored in
`docs/QSL_BACKLOG_FIXES.md` §13.

Treated as **active work** alongside the v0.1 feature bundles, not
deferred. Every chrome surface needs to flip in tandem to avoid a
half-and-half look, so the sequencing puts the cross-cutting tokens
+ typography pass first; subsequent bundles can land in any order
because they touch disjoint files.

- **PR-U1 — design tokens + typography (M).**
  Rewrite `apps/desktop/ui/assets/tailwind.css` token block: warm
  palette (`#1a1817` primary, `#252321` raised, `#d4a05a` accent
  amber, `#7ba968` success green, `#e8e3d8` text-primary). Bundle
  JetBrains Mono via `asset!` (or wire a CDN/system fallback) and
  set it as the chrome font. Two weights only (400 / 500); never
  600/700. `font-variant-numeric: tabular-nums` on every numeric
  field. Drop drop-shadows / glows / gradients / blur globally. 4px
  outer radius cap, 0px on rows/dividers.

- **PR-U2 — top bar + sidebar (M).**
  Top bar: `qsl 0.1.0-dev` wordmark left, command-palette pill
  centered (no-op until PR-U7), account count right; remove the
  capybara icon and "QSL" uppercase. Sidebar: drop the blue Compose
  button, mailbox icons, and avatar circle; tighten to ~124px;
  active-mailbox 2px amber left rail; user-label color bullets stay,
  system-mailbox bullets removed.

- **PR-U3 — message list rebuild (M).**
  Tab strip (`all / unread / flagged`); 26–28px dense rows with the
  IMAP flag-glyph column (`!` unread amber, `·` read tertiary, `F`
  flagged amber, `R` replied green, `D` draft secondary); single-
  line truncate layout (`[flag][sender][subject · preview][time]`);
  selected-row 2px amber rail; no avatars; tabular-num timestamps
  in the formats from `ui-direction.md` (Today `14:23`, Yesterday
  `yest`, this week weekday short, this year `Apr 23`, older
  `Mar '25`).

- **PR-U4 — message view + toolbar (S–M).**
  Replace pill action buttons with the keyboard-hint toolbar
  (`[r] reply  [a] reply-all  [f] forward  [e] archive  [s] flag`).
  Header block: subject 13px/500 at top, then `From / To / Cc / Date`
  rows with tertiary 56px label column. Two collapsed-by-default
  rows below standard headers — raw header expansion + IMAP flags —
  click to expand. Body HTML rendering untouched.

- **PR-U5 — compose redesign (M).**
  Drop the formatting toolbar and the rendered Send button.
  `⌘↵ send` in the bottom status line **is** the send affordance.
  Recipients as small 2px-radius pills on `bg-secondary`. Mono cursor
  block. Thread-context strip at the top on replies. Auto-insert
  `-- ` (RFC sig delimiter) before the user's signature.

- **PR-U6 — status bar expansion (S–M).**
  Expand from "Synced INBOX · 1 updated" to the three-zone layout:
  `<account> · <folder> · <total> / <unread> unread` left;
  `CONDSTORE · QRESYNC · IDLE` capabilities center with IDLE in
  `success-green` when active; `synced 12s · ⌘? help` right.
  Sourcing the capability flags requires plumbing the negotiated
  `Capability` set from `ImapBackend` / `JmapBackend` through to
  `AppState` and emitting it on `sync_event` (or a new
  `connection_state` event); the storage layer doesn't know it.

- **PR-U7 — command palette (⌘K) (M).**
  Centered overlay, sharp corners, mono input, fuzzy-match against
  mailboxes (jump-to), labels, commands (compose, archive, flag,
  …), and recent searches. ESC closes, arrow keys navigate, Enter
  confirms. No animation. Defer if it grows past one bundle —
  the pill in PR-U2 stays a no-op until then.

**Out of scope for this series:** light mode (defer until dark ships
clean), custom wordmark typeface, HTML email body styling
(rendered author content stays untouched).

---

## Total scope

18 PRs (15 v0.1 features + 3 regression fixes), sized:

- 7 × S (≤200 LOC)
- 11 × M (200–600 LOC)
- 0 × L (intentional — anything that grew toward L got split)

That's roughly 4–6 weeks of solo work at the pace Phase 2 was
hitting (1–2 PRs per evening for S/M). The regression sweep
(PR-R1 / R2 / R3) lands first since it blocks several downstream
PRs from delivering on a working sidebar / reader.

The UI overhaul (PR-U1 through PR-U7) is sequenced after the
feature set above lands; it adds 6–7 PRs (mostly M, U7 may slip)
on top of the 18 above.

---

## Out of v0.1 (deferred to v0.2 or later)

Per the gap doc and assumptions above, the following are **not**
in this plan and are explicitly v0.2+:

- Server-side search fallback (Gmail `X-GM-RAW`, JMAP `Email/query`)
- CardDAV / JMAP-Contacts sync
- Calendar app
- Snooze, schedule send, undo send
- Filters / rules UI
- HTML signatures
- Per-thread / per-identity signatures
- Notification actions on macOS / Windows
- Quiet hours / DND
- System tray icon with unread count
- Saved searches
- Sort / density / reading-pane-position customization (only dark/light + compact/comfortable in v0.1)
- Drag-drop into folders
- SPF / DKIM / DMARC indicators
- Phishing warnings
- First-run onboarding tour (v0.1 ships with empty-state CTA only)

---

## Risk register

- **R1: Servo input handling for `j`/`k` shortcuts.** The reader
  pane has Servo's surface laid over the Dioxus webview. Keyboard
  focus may bounce between them. Mitigation: dispatch shortcuts on
  the Dioxus root, swallow on `keydown` before Servo sees it. Test
  in PR-T2 specifically.

- **R2: FTS5 indexer blocking on first search.** A 1500-message
  inbox at ~50 KB/message → 75 MB body insert. Lazy backfill takes
  ~5–10s on the user's hardware (extrapolated from existing storage
  scan rates). Mitigation: show a progress modal during the
  backfill; don't block subsequent searches once the watermark is
  reached.

- **R3: Notification actions plumbing.** `tauri-plugin-notification`
  exposes actions but the wire-up requires registering action ids
  with the OS at app start, then handling action invocations as
  events. Untested in QSL. Mitigation: feature-gate the action wiring
  behind `cfg(target_os = "linux")` in PR-N1; if it fails to wire,
  fall back to actionless notifications without blocking the rest of
  the PR.

- **R4: Multi-window settings + state sync.** Settings writes need
  to propagate to the main window mid-session (e.g., dark→light
  toggle). Mitigation: emit a `settings_changed` Tauri event from
  the writer; the main window subscribes and re-reads. Same pattern
  as the existing `sync_event` channel.

- **R5: First-run OAuth + token persistence.** `qsl-auth` already
  handles the loopback flow and keyring storage in CLI; mirroring
  that path through Tauri requires careful handling of the
  `tauri::Runtime` thread-affinity rules (`run_on_main_thread` for
  any GTK touch). Mitigation: model on the existing
  `messages_open_in_window` pattern that already does this dance.

---

## What "v0.1 ships" means

End state of this plan:

- Threading collapses by default in the message list and reader
- `/` opens search; basic operators work; results return in <100 ms
  warm
- Toolbar in the reader header; right-click menu; full Gmail-style
  keyboard scheme with `?` cheatsheet
- Hover-to-reveal checkboxes; bulk archive/delete/move/label
- Notifications carry sender + subject; Linux gets Mark Read /
  Archive actions
- Topbar account filter chip
- Compose autocompletes from contacts seeded by mail history
- Settings window with Accounts / Appearance / Notifications /
  Shortcuts / Privacy tabs, all functional
- Empty app state shows "Add an account" → in-app OAuth flow → first
  sync
- Same provider matrix as today (Gmail + Fastmail), no regressions

When all 15 PRs are merged: tag `v0.1.0`, write release notes,
move `release-1-feature-gap.md` items 1–9 to "Done."
