# Drag-and-drop messages into folders

**Date:** 2026-05-01
**Branch:** `ui-enhancements`
**Status:** Approved (pending user spec re-review)

## Goal

Let users move messages between folders by dragging rows from
`MessageListV2` onto folder rows in the sidebar. The IPC and storage
sides are already in place — `messages_move` accepts `{ ids: [MessageId],
target: FolderId }` and is exercised today by the row context menu and
the bulk-action bar. This spec covers the drag affordance, drop targets,
and selection semantics.

## Source: draggable rows

`MessageRowV2` becomes the drag source. `draggable: "true"` on the row
element. `ondragstart`:

- If the dragged message id is in `bulk_selected`, drag **all** ids in
  the set (Gmail / Apple Mail behaviour).
- Otherwise drag just that one row's id.
- Payload: `dataTransfer.setData("application/x-qsl-message-ids",
  JSON.stringify(ids))`. Custom MIME prevents the browser from
  interpreting the drag as text or a URL anywhere else.
- `effectAllowed: "move"`.

No custom drag image for v1; default browser ghost is fine.

## Target: sidebar folder rows

`SidebarMailboxRow` and `SidebarLabelRow` accept drops.

- `ondragover` reads the dataTransfer types; if it sees the QSL MIME and
  the row's role isn't blocked, calls `event.prevent_default()` so the
  drop becomes legal, sets `dropEffect: "move"`, and toggles a
  `sidebar-row-drop-target` class for the highlight.
- Blocked roles (drops rejected, no-drop cursor): `FolderRole::Important`,
  `FolderRole::Flagged`, `FolderRole::All`. These are Gmail label-views,
  not real folders — moving "into" them is meaningless or surprising.
  See backlog item for revisit.
- `ondragleave` removes the highlight class.
- `ondrop` parses the JSON id list, calls `messages_move` IPC, then bumps
  `sync_tick` so the source folder's `MessageListV2` refetches.
  `bulk_selected` is cleared.

## Visual feedback

Two CSS classes added to `apps/desktop/ui/styles/main.css`:

- `.sidebar-row-drop-target` — accent-tinted background + 2px inset
  outline. Applied during a valid `dragover`.
- `.sidebar-row-drop-blocked` — `cursor: not-allowed` for the duration
  of the drag. Applied to Important / Flagged / All rows when *any*
  drag is in progress (so the user sees they can't drop there).

A `dragging_active` Signal in `SidebarV2` tracks whether a drag is
currently in flight; flipped on the message-list's `ondragstart`,
flipped off on `ondragend` and `ondrop`. Used to apply the blocked
class.

## Selection edge cases

- Drag a row not in `bulk_selected` while `bulk_selected` is non-empty
  → drag just the dragged row, leave the bulk selection alone. (Most
  apps do this. Avoids the surprise of "I dragged this one but moved
  five.") If the row IS in `bulk_selected`, drag all of them.
- Drop-on-current-folder → no-op (the IPC happily processes it; we
  short-circuit client-side anyway because there's nothing visible to
  refresh).

## Out of scope (backlog)

- Drop-target semantics for `Important` / `Flagged` / `All` (currently
  blocked). Future work: either *add the label* on drop instead of
  *moving*, or hide these from the sidebar drop-target list entirely.
- Custom drag image with count badge.
- Drag-to-trash via reader-pane swipe / keyboard shortcut.
- Drag from message list into compose window as attachment.

## Verification

- `QSL_SKIP_UI_BUILD=1 cargo clippy --workspace --all-targets -- -D warnings`
- `QSL_SKIP_UI_BUILD=1 cargo test --workspace`
- Manual smoke: drag a single row to Trash, drag a multi-selected set
  to Archive, attempt a drag onto Important (should refuse with
  no-drop cursor), confirm sync_tick refresh updates source list.
