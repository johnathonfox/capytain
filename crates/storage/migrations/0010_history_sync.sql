-- SPDX-License-Identifier: Apache-2.0

-- Phase 2 Week 22: history sync (full-archive pull) state.
--
-- One row per (account, folder) the user has asked to backfill.
-- The pager walks UIDs descending from `anchor_uid` toward 1, and
-- this row is updated on every chunk so the work is resumable
-- across app restarts.
--
-- `status`:
--   pending   — created, not yet running
--   running   — task active (in-memory cancel token attached)
--   completed — anchor_uid <= 1 reached, no more history to pull
--   canceled  — user-cancelled mid-run; can be re-started
--   error     — fatal failure; `last_error` carries the message
--
-- `anchor_uid` is the lowest IMAP UID we've successfully processed.
-- The pager fetches `[anchor_uid - chunk .. anchor_uid - 1]` on the
-- next pass. NULL until the first chunk lands.
--
-- `total_estimate` is uidnext - 1 captured on start; it's an upper
-- bound (some UIDs in the range may have been EXPUNGEd) but close
-- enough for a progress percentage. NULL if SELECT didn't surface
-- uidnext (rare).
--
-- `fetched` counts headers actually persisted, not chunks issued —
-- the difference matters when the pager skips already-known rows.

CREATE TABLE IF NOT EXISTS history_sync_state (
    account_id      TEXT NOT NULL,
    folder_id       TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pending','running','completed','canceled','error')),
    anchor_uid      INTEGER,
    total_estimate  INTEGER,
    fetched         INTEGER NOT NULL DEFAULT 0,
    started_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    completed_at    INTEGER,
    last_error      TEXT,
    PRIMARY KEY (account_id, folder_id)
);

CREATE INDEX IF NOT EXISTS history_sync_state_status_idx
    ON history_sync_state(status);
