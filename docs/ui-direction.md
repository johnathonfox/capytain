<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# qsl UI direction

Source of truth for the chrome aesthetic. Read this before changing anything visual.

## Goal

Move qsl's chrome — title bar, sidebar, message list, message view header, compose window, status line — to a monospace, density-first, single-accent aesthetic. Leave the rendered HTML message body alone; that's the email author's content, qsl just sanitizes and renders it.

The current UI defaulted to Spark/Hey conventions (avatar circles, large blue Compose button, sans-serif chrome, pill-shaped action buttons, generous row padding). Move to: mono everywhere in chrome, dense rows, a single amber accent, sharp corners, IMAP state surfaced via single-character flag glyphs, and a status line that shows real protocol state.

This is a direction shift, not a feature change. Functionality that already works keeps working. Changes are visual and informational density.

Dark mode ships first. Light mode is later.

## Design tokens

### Color (dark mode)

| Token              | Value     | Use                                              |
|--------------------|-----------|--------------------------------------------------|
| bg-primary         | `#1a1817` | Main app surface                                 |
| bg-secondary       | `#252321` | Raised/active surfaces, selected rows            |
| bg-tertiary        | `#131211` | Title bar, status bar, recessed surfaces         |
| bg-outer           | `#0e0d0c` | Outer window frame                               |
| border-tertiary    | `#2a2825` | Default dividers                                 |
| border-secondary   | `#38352f` | Window border, emphasis dividers                 |
| text-primary       | `#e8e3d8` | Primary text (warm off-white)                    |
| text-secondary     | `#9b958a` | Secondary text                                   |
| text-tertiary      | `#5e5950` | Hints, metadata, timestamps                      |
| accent-amber       | `#d4a05a` | Primary accent (active rail, send action)        |
| accent-strong      | `#f0c47e` | Bright amber for emphasis (unread counts)        |
| success-green      | `#7ba968` | IDLE indicator, replied flag                     |

The palette is deliberately warm. Most "developer dark" themes drift cool (blue-gray); qsl's identity is closer to phosphor-on-warm-dark.

### Typography

Single chrome font: a monospace face. Ship default is **JetBrains Mono** (free, packaged everywhere). Berkeley Mono and Iosevka are nicer-to-have but optional and out of scope for this refresh.

Rendered email body keeps whatever the email author sent — HTML inherits its own styling, plaintext renders in mono.

Chrome font sizes:
- 13px — wordmark, message subject in detail view
- 12px — primary list/sidebar text, compose body
- 11px — timestamps, status bar, message detail headers, command hints
- 10px — small right-aligned timestamps in dense rows

No size below 10px. No size below 11px outside the dense-row timestamp position.

Two weights only: 400 (regular) and 500 (bold for unread, active, primary action). Never 600/700 — they look heavy in mono.

`font-variant-numeric: tabular-nums` on every numeric field: timestamps, message counts, file sizes.

### Spacing

Density everywhere. The single biggest change vs. the current UI is row height in the message list — currently ~60px, target 26–28px.

- Sidebar item: ~24px tall, 4px vertical padding
- Message list row: 26–28px tall
- Compose header line: ~24px tall
- Status bar: 22–24px tall
- Top bar: 28–32px tall

### Radii

- Outer window: 4px
- Inputs (search box, contact pills): 2px
- Rows, dividers, tabs, selection highlights: 0px (sharp)
- Avatar circles, pill buttons: do not exist (removed — see below)

### Borders & effects

- Default divider: 0.5px solid `border-tertiary`
- Window/frame: 0.5px solid `border-secondary`
- Active item left rail: 2px solid `accent-amber`
- No drop shadows. No glows. No gradients. No blur effects.

## Layout structure

```
+-------------------------------------------------------------------+
| qsl  0.1.0-dev      [⌘K  search · jump · command]      2 accounts |  ← top bar (28–32px)
+-------------------------------------------------------------------+
|         |                       |                                 |
| sidebar | message list          | message view                    |
| 124px   | 270–300px             | flex                            |
|         |                       |                                 |
|         |                       |                                 |
+-------------------------------------------------------------------+
| fastmail · INBOX · 1,247 / 12 unread | CONDSTORE · QRESYNC · IDLE | synced 12s · ⌘? help |
+-------------------------------------------------------------------+
```

Three-pane horizontal grid, persistent top bar above and status bar below. Status bar is always visible.

## Component changes

### Top bar

**Remove**
- Capybara icon at left (lives in dock/tray only — chrome is wordmark-led, not logo-led)
- "QSL" centered uppercase text (duplicates the wordmark and isn't mono)

**Change**
- Left: `qsl` wordmark, mono, weight 500, 13px, `text-primary` — followed by `0.1.0-dev` (or current version) in 11px `text-tertiary`
- Center: command palette pill — `⌘K` in `text-secondary` + `search · jump · command` in `text-tertiary`. 0.5px border-tertiary, 2px radius, 11px. Click opens the command palette (see below).
- Right: `2 accounts` (or whatever count) in 11px `text-secondary`

Window controls (minimize/maximize/close) stay platform-default.

### Sidebar

**Remove**
- Large blue Compose button (compose is keyboard-driven via `n`; the message-list header has a small `+ new` affordance)
- Avatar circle in the account header
- Mailbox icons (Inbox, Star, Sent, Drafts, etc.) — replace with mono labels alone
- Generic colored bullets next to *system* mailboxes (Inbox, Sent, etc.)

**Keep**
- Colored bullets next to *user labels* (those carry user-meaningful data from IMAP/Gmail label colors). 6px round dots beside the label name.

**Change**
- Account header: single line, account name in 11px `text-secondary`, with a `▾`/`▸` collapse toggle. No avatar.
- Mailbox section header (`fastmail`, `gmail`): 11px `text-tertiary`
- Mailbox row: 12px mono label, right-aligned count
  - Active mailbox: `bg-secondary` background + 2px `accent-amber` border-left
  - Inbox unread count: `accent-strong` weight 500
  - Other counts (Drafts, Spam): 11px `text-tertiary`
- Labels section header: 11px `text-tertiary`, `labels`
- Label row: 12px mono label with 6px colored bullet, right-aligned count if present
- Separator before second account: 12px top margin, 0.5px top border

Sidebar width: 124px. Tighten if currently wider.

### Message list

**Remove**
- All avatar circles (JF, N, etc.)
- Two-line per-message layout if currently used
- Generous row padding

**Change**

Tab strip at top: `all 1,247  unread 12  flagged 3`. Active tab gets a 1px `accent-amber` border-bottom. Right-aligned `+ new` link with `n` keyboard hint in `text-tertiary`.

Search input (when active): mono, 2px radius, 0.5px border, focus ring is a 1px solid `accent-amber` border (no glow). Currently functional — restyle to match.

**Message row layout** (26–28px tall, single line):

```
[flag][sender         ][subject · preview                      ][time]
  8px   100px (trunc)   flex 1 (trunc with ellipsis)             32px
```

**Flag column glyphs (column 1, 8px wide, centered):**
- `!` `accent-amber` weight 500 — unread (no `\Seen` flag)
- `·` `text-tertiary` — read (`\Seen`)
- `F` `accent-amber` — flagged (`\Flagged`)
- `R` `success-green` — replied (`\Answered`)
- `D` `text-secondary` — draft (`\Draft`)

Combine when multiple apply (e.g. unread + flagged): show flagged precedence (`F` over `!`). Don't try to render two glyphs per row.

**Sender column** (~100px, truncate with ellipsis): 12px. Weight 500 + `text-primary` if unread, else weight 400 + `text-secondary`.

**Subject + preview** (flex 1, single-line truncate): 11px. Subject in same color as sender (matches unread/read state), then ` · ` separator, then preview in `text-tertiary`. All on one line. `white-space: nowrap; overflow: hidden; text-overflow: ellipsis`.

**Timestamp** (32px right-aligned, 10px tabular, `text-tertiary`):
- Today: `14:23`
- Yesterday: `yest`
- This week (older): `Sun`, `Sat`, `Fri`...
- This year (older): `Apr 23`
- Older: `Mar '25`

**Selected row**: `bg-secondary` background + 2px `accent-amber` border-left. No rounded corners. No row separators between unselected rows — selection highlighting is the only delineator.

### Message view

**Remove**
- "JF" avatar circle in message header
- Pill-shaped action buttons (Reply, Reply All, Forward, Archive, Mark unread, Star, Delete) at the top
- Generous padding around action buttons

**Change**

**Toolbar** (above message header, 6px vertical padding, 0.5px border-bottom):

```
[r] reply   [a] reply-all   [f] forward   [e] archive   [s] flag
```

Each letter in `text-primary` weight 500, label after in `text-secondary`, gap 16px. 11px font.

**Header block** (12px 14px padding, 0.5px border-bottom):

- Subject as 13px weight 500 line at top, no label
- `From:` value (label in `text-tertiary` 56px width column, value in `text-primary`)
- `To:` value (plain text, comma-separated for multiple recipients, truncate to one line — no pills)
- `Cc:` value (if present)
- `Date:` value with timezone, e.g. `Mon Apr 27 2026  14:23 -0700`

**Two collapsed-by-default lines below standard headers** (11px, `text-tertiary`):

- `▸ message-id, references, list-unsubscribe (4 more)` — click expands to show raw header values inline
- `flags: \Seen \Answered case-8842311` — IMAP system flags + custom flags visible

**Body**: keep current HTML rendering. 14px padding around. No further changes.

**Bottom of body** (if quoted history detected and folded): `▾ show N quoted messages` toggle in 11px `text-tertiary`, 10px top padding, 0.5px top border.

### Compose window

**Remove**
- Formatting toolbar (if present)
- Boxed input fields for headers
- Any rendered "Send" button

**Add / change**

**Title bar**: `qsl // compose · Re: <subject>` in mono 11px. `qsl` weight 500, `//` and `compose` in `text-secondary`/`text-tertiary`. Right side: `⌘W close`.

**Thread context strip** (only on reply, `bg-secondary`, 0.5px border-bottom):
```
▸ reply to Anova Support · 14:23 today · 2 prior in thread          expand
```

**Header fields** (12px 14px padding, 0.5px border-bottom, line-height 1.9):

- `From:` value with `▾` account selector
- `To:` recipients as small pills — 2px radius, 0.5px border, `bg-secondary` background
- `Cc:` recipients (or `—` if empty), with a `+bcc` link in `text-tertiary` to expand
- `Subject:` plain text

Labels in `text-tertiary` with `width: 70px` flex-shrink-0.

**Body editor** (14px padding, 12px font, line-height 1.65):

- Monospace
- Cursor visible at end of typing position as a 7×14px solid `accent-amber` block
- RFC sig delimiter `-- ` (with trailing space) auto-inserted before the signature
- No formatting toolbar

**Attachment row** (8px 14px padding, 0.5px border-top):

```
attached:  [apo-bake-logs.csv  44 KB  ×]                   + attach ⌘⇧A
```

Pill: 2px radius, 0.5px border, `bg-secondary`, sharp corners on the × delete affordance.

**Status bar at bottom of compose** (6px 12px padding, 0.5px border-top, `bg-tertiary`, grid `1fr auto 1fr`):

- Left: `plain · markdown · 312w / 1,847c` in `text-tertiary`
- Center: `● draft saved 2s ago` (6px round `success-green` dot + `success-green` text)
- Right: `⌘↵` in `text-tertiary` followed by `send` in `accent-amber` weight 500

The send action **is** the keyboard shortcut. Do not render a button-shaped element.

### Status bar (main window)

Currently shows minimal info ("Synced INBOX · 1 updated"). Expand to:

22–24px tall, 0.5px top border, `bg-tertiary` background, grid `1fr auto 1fr`, 5px 10px padding, 11px font.

- **Left**: `<account> · <folder> · <total> / <unread> unread` in `text-secondary`
- **Center**: protocol capability flags. Format: `CONDSTORE · QRESYNC · IDLE` — capability names in `text-tertiary`, IDLE in `success-green` when active. Show capabilities the current backend connection actually negotiated. If a capability isn't supported, omit it (don't grey it out — just don't show it).
- **Right**: `synced 12s ago · ⌘? help` in `text-tertiary`, right-aligned

### Command palette (⌘K)

If not already implemented, this is a separate task — defer if it's a multi-day implementation. The `⌘K` pill in the top bar can no-op or open existing search until the palette is built.

When implemented:
- Centered overlay, sharp corners, 0.5px `border-secondary`, `bg-primary` surface
- Mono input at top, fuzzy-matches against: mailboxes (jump to), labels, commands (compose, archive, flag, etc.), recent searches
- ESC closes, arrow keys navigate, Enter confirms
- No animation — appears and disappears instantly

## Things to remove (consolidated)

- All avatar circles (sidebar, message list, message view, compose)
- Big blue Compose button in the sidebar
- Pill-shaped action buttons at the top of message view
- Mailbox icons in the sidebar (keep label color bullets)
- "QSL" uppercase text in the top bar (replaced by `qsl` wordmark)
- Sans-serif fonts in chrome (replaced by mono — rendered HTML body unaffected)
- Any radius > 4px anywhere in chrome
- Drop shadows, glows, gradients, blur

## Out of scope (leave alone)

- HTML email body rendering (working — the GitHub Actions email rendering proves it)
- Account auth / OAuth flows
- IMAP / sync logic
- Search functionality (restyle the input only — search itself works)
- The capybara icon as the system tray / dock / launcher icon (only removed from in-app top bar)
- Light mode color values (defer until dark ships clean)
- Custom wordmark typeface (use JetBrains Mono ship-default)

## Acceptance criteria

The refresh is done when:

1. Chrome uses a single monospace face throughout. Sans-serif appears nowhere except inside rendered HTML email bodies.
2. The dark palette uses the warm tokens above. Cool blue-gray backgrounds and blue accents are gone.
3. The message list shows ≥10 rows in the same vertical space that currently shows ~5–6.
4. Every message row displays an IMAP flag glyph in column 1 reflecting actual IMAP state.
5. The status bar shows the connected account, folder, message counts, protocol capabilities (CONDSTORE/QRESYNC/IDLE) with IDLE in green when active, and last sync time.
6. The message view toolbar shows keyboard hints (`[r] reply` etc.) instead of pill buttons.
7. Compose has no formatting toolbar and no rendered Send button — `⌘↵ send` in the status line is the only send affordance.
8. No avatar circles appear anywhere in chrome.
9. Top bar: `qsl` wordmark top-left in mono weight 500, command palette pill centered, account count right-aligned.

## Implementation notes

- The renamed app is `qsl` (lowercase). Older "QSL" or "Capytain" references in chrome should be updated. Repo name and capybara dock icon are unaffected.
- This refresh is visual + informational only. Do not change backend/protocol behavior, IPC commands, or the trait architecture defined in TRAITS.md.
- If a token name above conflicts with what's already wired through the styling layer, keep the existing name and swap the value. The names in this doc are advisory.
- The terminal-native aesthetic depends on density. If a layout feels generous, it's wrong — tighten until it feels closer to an aerc/mutt window than to Apple Mail.
- The amber accent should only appear for: unread state, active rail, send action, active tab underline, and the cursor block in compose. If amber is showing up in five places on one screen, something is over-applied.
- When a design choice is ambiguous, prefer the muted/tertiary text color and the sharper-cornered option.
