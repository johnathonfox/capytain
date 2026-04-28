-- SPDX-License-Identifier: Apache-2.0
--
-- QSL schema v6. Phase 2 post-Week-21: contacts table for compose
-- autocomplete (PR-C1).
--
-- Write-only collection: every inbound `From:` and every outbound
-- `To:` / `Cc:` / `Bcc:` address gets an upsert here. The repo
-- exposes a prefix query the compose pane's `AddressField` uses to
-- populate its dropdown (PR-C2 wires the UI; this PR is the data).
--
-- Schema notes:
--   - `address` is the primary key, COLLATE NOCASE so addresses
--     compare case-insensitively across the whole row. SMTP/IMAP
--     treat the local part as case-sensitive in theory but every
--     deployed mail server treats the whole address as
--     case-insensitive in practice — matching that lets the
--     dropdown deduplicate `Alice@Example.com` vs `alice@example.com`
--     without the repo doing it manually.
--   - `display_name` is nullable and updated to the most recent
--     non-empty value seen. Mailing-list senders frequently arrive
--     once with a real name and once without — keeping the
--     non-empty form prevents the dropdown from flipping to "" on
--     subsequent encounters.
--   - `seen_count` lets the autocomplete sort by familiarity
--     (popularity-bumped behind recency).
--   - `source` is `'inbound'` for `From:` collection and
--     `'outbound'` for `To:` / `Cc:` / `Bcc:` collection. We keep
--     this even though the autocomplete itself is source-agnostic
--     so a future privacy toggle ("don't suggest people I haven't
--     written to") has the data to filter on.

CREATE TABLE IF NOT EXISTS contacts_v1 (
    address       TEXT PRIMARY KEY COLLATE NOCASE,
    display_name  TEXT,
    last_seen_at  INTEGER NOT NULL,
    seen_count    INTEGER NOT NULL DEFAULT 1,
    source        TEXT NOT NULL
);

-- Prefix scans land on this index. Without it, autocomplete is a
-- full-table scan — fine for 100 contacts, painful at 10k.
CREATE INDEX IF NOT EXISTS contacts_v1_address_idx ON contacts_v1(address);
