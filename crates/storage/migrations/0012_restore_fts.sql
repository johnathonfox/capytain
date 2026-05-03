-- SPDX-License-Identifier: Apache-2.0

-- Restore the messages full-text index.
--
-- Migration 0005 (`search_fts.sql`) was temporarily emptied during
-- the bulk-INSERT perf overhaul that landed in PR #141, because
-- Turso 0.5.3's experimental `USING fts` index commits Tantivy on
-- every per-row INSERT at ~250ms/row and dominated the bulk path.
-- The fix landed alongside this migration: `pull_history` now
-- drops the index for the duration of the pull and rebuilds it
-- once at the end (see `crates/sync/src/history.rs`).
--
-- This migration is for DBs that ran the empty 0005 (i.e. anyone
-- who pulled main between PR #141 and this PR). Fresh DBs hit the
-- restored 0005 directly and skip this. `IF NOT EXISTS` makes
-- both paths idempotent.

CREATE INDEX IF NOT EXISTS messages_fts_idx ON messages
USING fts (subject, from_json, to_json, snippet);
