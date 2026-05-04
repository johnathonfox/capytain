-- Real `unread` column + composite index for the per-folder
-- "unread" COUNT(*).
--
-- The query is:
--
--   SELECT COUNT(*) FROM messages
--    WHERE folder_id = ?1
--      AND unread = 1
--
-- Without an index on the seen-flag axis, SQLite has to scan every
-- message in the folder (via `messages_folder_date`) and json-decode
-- `flags_json` per row to evaluate the `$.seen` predicate. On a
-- 30k-row Gmail folder that's ~1.4s — measured on the maintainer's
-- NVIDIA + KWin Wayland box, 2026-05-04. The query fires on every
-- IDLE poll of every folder; PR #144 already gates most of that
-- storm at the consumer side, but the FIRST sync that does see a
-- real change still pays the full scan, so this index makes that
-- case fast too.
--
-- Why a real column and not a partial index or generated column:
-- Turso 0.5.3 has incomplete planner support for both. A partial
-- index (`CREATE INDEX … WHERE …`) is accepted but the planner
-- prefers `messages_folder_date` and falls back to the full-folder
-- scan; `INDEXED BY` to override is rejected as "not supported
-- yet"; `ALTER TABLE … ADD COLUMN … GENERATED ALWAYS AS …` is
-- rejected as "Alter table does not support adding generated
-- columns". A regular column with a regular composite index is
-- the only option that the planner picks unambiguously here.
--
-- App-side maintenance: every code path that writes `flags_json`
-- also writes the matching `unread` value (1 = unseen, 0 = seen)
-- through the same INSERT/UPDATE/`update_flags` statements. The
-- two stay in lockstep at the SQL level — see
-- `crates/storage/src/repos/messages.rs::to_params` and
-- `update_flags`.
--
-- Backfill: the `UPDATE` below classifies every existing row from
-- its current `flags_json`, so populated databases get correct
-- counts on the very first launch after this migration applies
-- without needing a re-sync.

ALTER TABLE messages ADD COLUMN unread INTEGER NOT NULL DEFAULT 1;

UPDATE messages
   SET unread = CASE
       WHEN COALESCE(json_extract(flags_json, '$.seen'), 0) = 0 THEN 1
       ELSE 0
   END;

CREATE INDEX IF NOT EXISTS messages_folder_unread
    ON messages(folder_id, unread);
