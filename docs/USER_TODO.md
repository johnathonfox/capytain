<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# User-action backlog

Items that need you (the maintainer) at the keyboard or the GitHub UI — things an agent can't do or can't verify alone. Tracked here so they don't get lost between sessions. Delete an item when it's done.

---

## GitHub admin

- [ ] **Branch protection on `main`.** `Settings → Branches → Add rule` requiring the four CI checks (`Check (ubuntu-latest)`, `Check (windows-latest)`, `cargo-deny`, `reuse lint`). No required reviews (solo-maintainer). Force-pushes + deletions blocked. `enforce_admins: false` so you can override in genuine emergencies. Long-tracked in `KNOWN_ISSUES.md`.

## Provider registration

- [ ] **Fastmail OAuth client.** Per `docs/dependencies/fastmail.md`. Prerequisite for the W19 real-account JMAP submit smoke. Without it, W19 ships green in CI but can't be exercised against a live mailbox.

## Runtime verification (need you sitting in front of the app)

- [ ] **PR-M1 multi-select bulk actions.** Backlog §0 — UI shipped, action paths not exercised end-to-end. Tests:
  - Bulk **Archive** with mixed singletons + threads → all rows leave folder, selection clears, sidebar refreshes.
  - Bulk **Mark read / unread** → unread dots flip, sidebar counts update.
  - Bulk **Delete** → rows go to Trash (Gmail) / disappear (JMAP).
  - Thread-head checkbox toggles every member atomically.
  - Folder switch with rows checked → checks persist; Clear drops them.

- [ ] **W19 JMAP submit real-account smoke.** After the Fastmail OAuth client lands and W19 ships:
  - Compose to a second mailbox you own.
  - Verify recipient receives, headers correct, QSL Sent folder grows with the canonical server-issued Message-ID.
  - Network-drop test (yank cable mid-send) → outbox DLQ → reconnect drains without duplicate send.

- [ ] **⌘K palette ↔ Servo overlay coordination.** Verify: open palette while a message is rendered → email body should stop bleeding through; close palette → body re-appears in correct slot. Already shipped; needs eyeball confirmation on next build.

- [ ] **Reader-render multi-fire observation** (`KNOWN_ISSUES.md`). Click the "Render test page in Servo" button once during a normal interactive session; confirm exactly one `reader_render` log line per click. If verified, delete the entry from `KNOWN_ISSUES.md`.

- [ ] **Compose bundle (PR #100).** Five features need eyeball confirmation:
  - **Cc/Bcc reveal**: open compose, confirm Bcc is hidden behind `+bcc` link; clicking reveals the row; hydrating a draft with Bcc auto-reveals.
  - **Per-identity signatures**: set distinct signatures on two accounts in `Settings → Compose`; switching the From dropdown lands the right signature on a fresh draft (only when body is empty).
  - **Undo-send window**: set `Settings → Compose → Undo send` to 5/10/30s; click Send → countdown banner replaces Send button; Esc anywhere cancels and pane stays open; let it expire → message goes out and pane closes.
  - **Right-click context menu**: right-click a message row → menu appears at cursor with Reply / Reply-all / Forward / Mark read|unread / Archive / Delete / Open-in-window; backdrop or Esc dismisses.
  - **File attachments**: `+ Attach` opens the OS file picker (`rfd`); chips show filename + humanized size; × removes; send → recipient sees the file with correct MIME type.

## Investigations (you've raised, I haven't dug)

- [ ] **webkit2gtk CPU usage.** You noted high CPU at idle. Suspects:
  - Dioxus reactive cycle churning on a self-triggering signal (the recent `last_query` peek bug had this shape).
  - JS-side ResizeObserver stuck in a layout-thrash loop.
  - Compose draft auto-save (5 s debounce) or a settings resource refresh stuck in a loop.
  - Profiling path: open devtools (`Ctrl+Shift+I`) → Performance tab → record 5 s at idle → flame chart will show the hot stack.

- [x] **Reader rendering blank on AMD laptop (2026-04-28).** Resolved by removal — the Servo-backed reader was swapped for a sandboxed webkit2gtk `<iframe srcdoc>` and the Servo code path was removed entirely (see `docs/servo-tombstone.md`).

## Deferred (not blocking)

(no current Servo-blocked items — all of them resolved by the 2026-04-28 removal.)
