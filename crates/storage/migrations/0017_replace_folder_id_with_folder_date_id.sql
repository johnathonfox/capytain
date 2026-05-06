-- Replace `messages_folder_id_covering(folder_id, id)` from PR #148
-- with `messages_folder_date_id(folder_id, date DESC, id)` so the
-- planner has a single covering index that serves *both* the
-- reconciliation prune and the message-list date-ordered read.
--
-- After PR #148 shipped, the foreground `messages_list` IPC went
-- from a fast index-seek-by-date to consistent 3+ second freezes
-- (telemetry 2026-05-06 17:57): four back-to-back 3.1s slow events
-- on every folder click. Hypothesis: Turso 0.5.3's planner now
-- prefers the smaller `messages_folder_id_covering(folder_id, id)`
-- for the `WHERE folder_id = ? ORDER BY date DESC LIMIT 50` query
-- because folder_id is the leading column of both candidate indexes
-- and the new one is narrower. Picking it forces the planner to
-- fetch every row in the folder (30k for `[Gmail]/All Mail`) and
-- sort by date in memory — which lines up with the observed 3s
-- timing. The original `messages_folder_date(folder_id, date DESC)`
-- isn't being chosen even though it's the right plan for the read
-- path.
--
-- The cleanest fix is a single covering index that covers both
-- access patterns:
--
-- 1. Prune — `SELECT id FROM messages WHERE folder_id = ?1`
--    (`crates/sync/src/lib.rs::sync_folder` reconciliation pass)
--    — folder_id leading + id stored in the suffix means the
--    planner serves it index-only.
--
-- 2. List — `SELECT {COLS} FROM messages WHERE folder_id = ?1
--    ORDER BY date DESC LIMIT ?2 OFFSET ?3`
--    (`crates/storage/src/repos/messages.rs::list_by_folder` →
--    `messages_list` IPC) — date DESC is the second column so the
--    planner walks the index in date-desc order with no in-memory
--    sort.
--
-- Both queries collapse to one index. The redundant
-- `messages_folder_date(folder_id, date DESC)` from
-- `0001_initial.sql` is left in place — Turso may still pick the
-- smaller one for date-ordered queries that don't read id from the
-- index, and dropping it touches the initial schema, which we
-- prefer to avoid.

DROP INDEX IF EXISTS messages_folder_id_covering;

CREATE INDEX IF NOT EXISTS messages_folder_date_id
    ON messages(folder_id, date DESC, id);
