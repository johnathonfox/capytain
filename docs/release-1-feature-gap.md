# qsl ↔ Proton Mail desktop: feature gap analysis

> **Scope.** This document compares qsl (in its current Phase 0 / early-UI state)
> against Proton Mail's desktop client to identify general email-client features
> qsl is missing. Features that are intrinsically tied to Proton's
> infrastructure or proprietary protocols are listed at the end as **out of
> scope** — qsl deliberately doesn't target Proton (see [`README.md`](../../README.md)).
>
> The intent is to provide a working punch-list for v0.1 and beyond, not a
> claim that qsl should mirror Proton Mail.

## Snapshot of qsl as of writing

- Three-pane layout (folder rail / message list / reader pane), dark theme
- Compose entry point present (compose UI depth unverified)
- System folders + Gmail labels with native label colors in the rail
- Reader pane renders HTML email via Servo, with Reply / Reply All / Forward
- Sync status indicator in the footer ("Synced INBOX · 1 updated")
- Native desktop notifications (currently generic "new message" content)
- Headless `mailcli` for protocol work; OAuth2-only auth pipeline

---

## High-priority gaps

These are the items that most distinguish a "real" email client from a tech
demo. Roughly ordered by user-visible impact.

1. **Conversation threading.** The biggest single gap. Without thread
   collapsing, the inbox feels like a 2005 client. Gmail provides
   `X-GM-THRID`; JMAP has native thread support; IMAP CONDSTORE/QRESYNC give
   the underlying primitives. Needs both a data model (threads as a
   first-class entity over the message store) and UI (collapsed thread row,
   expanded multi-message reader).
2. **Search.** Currently absent. Both UI (search input in the header) and
   storage (an FTS index over the synced cache) are missing. Turso/SQLite
   ships FTS5; the storage side is mostly schema work. The harder questions
   are product decisions — see "Search design notes" below.
3. **Standard message-action toolbar.** Archive, Delete, Mark read/unread,
   Move, Label, Snooze, Spam. Reader-pane and message-list both need these,
   plus right-click context menus and keyboard shortcuts.
4. **Multi-select for bulk operations.** Shift/cmd-click ranges, checkbox
   column, "select all in folder," and bulk-apply of any toolbar action.
   Required to make the toolbar useful at scale.
5. **Compose editor depth.** Beyond the Compose button visible today:
   rich-text formatting (bold/italic/lists/links), attachments
   (drag-drop, multiple, size limits), inline images, draft auto-save,
   signatures (multiple, per-identity), Cc/Bcc reveal, address autocomplete
   from a contact store, spellcheck, schedule send, undo send.
6. **Notification content.** Notifications exist but are generic. Upgrade to
   include sender, subject, and a short body snippet, with action buttons
   (Archive / Mark read / Reply) — the typical pattern Proton, Apple Mail,
   and Thunderbird all converge on.
7. **Multiple accounts + account switcher.** Header shows a single account.
   Even if multi-account is implemented in the backend, the switcher UI and
   per-account identity/signature/notification settings are gaps.
8. **Settings panel + keyboard shortcuts.** No visible settings entry point.
   Keyboard shortcuts (j/k, e, #, r, a, /, ?) are table stakes for power
   users; pair with a `?` cheatsheet overlay.

---

## Medium-priority gaps

Useful to have, but each is independently scopeable and none block a 0.1.

### Message actions & organization

- Star/flag toggle on individual messages (sidebar entry exists, per-message
  affordance unclear)
- Mark read / unread toggle
- Move-to-folder picker
- Apply / remove labels from a message
- Block sender
- Snooze (client-side scheduled re-surface, or via Gmail's snooze API)
- Mute conversation
- Drag-drop into folders/labels in the rail

### Threading & conversation view

- Collapse / expand individual messages within a thread
- Toggle threading on/off (some users prefer flat)
- Inline quoted-text collapse ("show trimmed content")

### Search

- Operators: `from:`, `to:`, `subject:`, `has:attachment`, `before:`,
  `after:`, `in:label`, `is:unread`
- Search-in-folder vs. search-all-mail toggle
- Saved searches
- Server-side search fallback for messages outside the local cache
  (Gmail `X-GM-RAW` via IMAP; `Email/query` via JMAP)

### View / UI customization

- Light / dark / system theme toggle
- Density options (compact vs. comfortable)
- Reading-pane position (right / bottom / off)
- Sort options (date / sender / size / unread first)
- Conversation list customization (which fields shown)

### Notifications & system integration

- Per-account notification settings
- Quiet hours / DND
- System tray icon with unread count
- Dock / taskbar badge with unread count

### Accounts & sync

- Account switcher UI
- Manual refresh button / pull-to-refresh
- Online/offline indicator (the README emphasizes offline-first; the indicator
  itself is a small but important UX cue)
- Per-account identity (display name, signature, reply-to)

### Filters, rules, automation

- Filter / rule management UI. For Gmail and Fastmail, this can lean on
  server-side filters via Gmail API and Sieve (Fastmail) — qsl provides the
  management surface, the provider executes
- Vacation / auto-reply (server-side via Sieve)

### Privacy & security UX

- Remote image load-on-demand (per-message and per-sender)
- Link cleaning UI on click — README mentions this is planned; not yet
  shipped
- SPF / DKIM / DMARC indicators on incoming mail
- Phishing / suspicious-sender warnings
- Block sender / report phishing

### Calendar & contacts

- Contacts / address book. CardDAV is supported by Gmail and Fastmail; JMAP
  has native Contacts. Useful even just for compose autocomplete
- Inline invitation responses (RSVP from within an `.ics` invite email)
- Calendar tab — open scope question (see below)

### Settings & onboarding

- First-run OAuth flow in the desktop UI (currently CLI-driven via
  `mailcli auth add`)
- Settings panel with at least: accounts, signatures, notifications,
  appearance, keyboard shortcuts, privacy

---

## Search design notes

Search is the largest single architectural gap, so it warrants more detail.

**Storage.** Turso ships SQLite FTS5; you get a virtual table mirroring
subject / from / to / body / labels, populated on message insert/update,
queried on search input. Pure-Rust, no extra dependency.

**Local vs. server.** A reasonable v1 approach:

- **Local FTS5 over the synced cache** for the default case — instant,
  works offline, scales to whatever is on disk
- **"Search server" affordance** that falls through to provider-side search
  (Gmail `X-GM-RAW`, JMAP `Email/query`) for messages outside the local
  cache, surfaced as an explicit "Search all mail on server" action

This sidesteps the "I only see results from messages I already synced"
footgun without forcing eager full-mailbox download.

**Operators.** Gmail-style operators are the de-facto user expectation. The
parser maps to FTS5 MATCH expressions for local search and to provider
filter syntax for server search.

**Indexing.** Eager indexing on sync is the cleaner UX (first search is
snappy); lazy is simpler to ship. Either is defensible for v1.

---

## Out of scope (Proton-specific)

These features depend on Proton's infrastructure or proprietary protocols
and don't translate to a multi-provider IMAP/JMAP client. They are listed
here so the analysis is complete, not as todo items.

- End-to-end encryption between Proton users (relies on Proton's PGP key
  directory)
- Encrypted-to-outside-recipient via password link (Proton-hosted endpoint)
- Self-destructing messages (Proton-server-enforced expiry; `Expires:`
  header is a partial, non-equivalent client-side analog)
- PhishGuard / proprietary sender verification
- Proton Bridge integration (qsl deliberately doesn't talk to Proton)
- Proton account ecosystem features (VPN, Drive, Pass, SimpleLogin alias
  management)
- Custom domain management UI for Proton-hosted domains
- Encrypted contact / calendar sync via Proton's proprietary protocols
  (CardDAV / CalDAV / JMAP-Calendars are the standards-based equivalents
  for qsl's target providers)

A standalone calendar tab is not Proton-specific per se — Apple Mail,
Outlook, and Thunderbird all bundle one — but it is a deliberate scope
question for qsl rather than a feature gap. "qsl is mail, full stop" is a
defensible product position; integrating CalDAV / JMAP-Calendars is also
defensible. Worth deciding explicitly rather than drifting either way.

---

## Suggested v0.1 cut

Picking the floor for "feels like a real email client":

- Conversation threading
- Search (local FTS5, basic operators, current folder default)
- Message-actions toolbar + right-click menu + keyboard shortcuts
- Multi-select with bulk apply
- Compose: rich text, attachments, drafts auto-save, signature,
  address autocomplete (from a minimal contact store), Cc/Bcc
- Richer notification content (sender + subject + actions)
- Multi-account + switcher
- Settings panel covering accounts / appearance / notifications /
  shortcuts
- First-run OAuth flow in the desktop UI

Snooze, schedule send, undo send, filters/rules UI, contacts manager,
calendar, and server-side search fallback are reasonable v0.2 candidates.
