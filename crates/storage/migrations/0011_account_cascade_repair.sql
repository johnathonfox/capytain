-- SPDX-License-Identifier: Apache-2.0

-- Account-cascade repair.
--
-- SQLite ships with `PRAGMA foreign_keys` OFF by default. Until the
-- companion change to `TursoConn::open` enabled the pragma, every
-- `ON DELETE CASCADE` clause in the schema (folders, threads,
-- messages, outbox, contacts, drafts, remote_content_opt_ins) was a
-- dead clause: removing an account dropped the `accounts` row but
-- left every child row orphaned, with no path back to a parent.
--
-- Two repairs in one migration:
--
--   1. Prune orphans accumulated under the old "FK pragma off"
--      regime — any row whose `account_id` no longer matches a
--      surviving `accounts.id`.
--
--   2. Backfill the missing FK on `history_sync_state` (added in
--      0010 without one, so it would have continued to leak rows
--      even after the pragma flip). SQLite has no
--      `ALTER TABLE ADD FOREIGN KEY`, so the standard 12-step
--      rename-recreate dance applies.
--
-- All of this runs inside the migration runner's transaction with
-- foreign_keys=ON. That's safe here because:
--   * orphan deletes don't violate any constraint (no parent is
--     being orphaned, only already-orphaned children removed),
--   * the recreate dance for `history_sync_state` is FK-safe (no
--     other table references it), and
--   * the `INSERT … SELECT` into the new table is checked against
--     `accounts`, which the orphan-prune step just guaranteed.

DELETE FROM folders                 WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM threads                 WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM messages                WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM outbox                  WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM contacts                WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM remote_content_opt_ins  WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM drafts                  WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM history_sync_state      WHERE account_id NOT IN (SELECT id FROM accounts);

CREATE TABLE history_sync_state_new (
    account_id      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
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

INSERT INTO history_sync_state_new
    (account_id, folder_id, status, anchor_uid, total_estimate,
     fetched, started_at, updated_at, completed_at, last_error)
SELECT account_id, folder_id, status, anchor_uid, total_estimate,
       fetched, started_at, updated_at, completed_at, last_error
  FROM history_sync_state;

DROP TABLE history_sync_state;

ALTER TABLE history_sync_state_new RENAME TO history_sync_state;

CREATE INDEX IF NOT EXISTS history_sync_state_status_idx
    ON history_sync_state(status);
