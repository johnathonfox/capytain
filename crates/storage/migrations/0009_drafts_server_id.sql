-- SPDX-FileCopyrightText: 2026 QSL Contributors
-- SPDX-License-Identifier: Apache-2.0

-- Track the canonical server-side draft id so the next upstream sync
-- can destroy the prior copy after the new APPEND / Email/import
-- lands. Without this column, every auto-save tick produces a fresh
-- server-side draft and a 30-minute compose session leaves N stale
-- copies in gmail.com → Drafts.
--
-- Nullable: a draft that's never made it upstream (no recipients yet,
-- the outbox row is still pending, or upstream sync DLQ'd) doesn't
-- have a server id to remember. The next save_draft drain-cycle
-- treats `NULL` as "no prior copy to destroy."

ALTER TABLE drafts ADD COLUMN server_id TEXT;
