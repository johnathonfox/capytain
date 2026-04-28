-- SPDX-License-Identifier: Apache-2.0
--
-- Settings panel storage (post-Phase-2, PR-P1+P2).
--
-- Two pieces:
--
-- 1. `app_settings_v1` is the global k/v store backing the
--    Appearance / Privacy tabs (theme, density, "always load remote
--    images" master toggle). Plain TEXT values; the UI deserializes
--    per-key. Suffix `_v1` reserves room for a schema migration if a
--    future tab needs structured per-key types.
--
-- 2. Two new columns on `accounts`:
--    - `signature` — plain-text signature appended to outbound
--      messages by the compose pane. NULL means "no signature".
--    - `notify_enabled` — per-account notification gate consumed by
--      the desktop notification bridge. Defaults to 1 so existing
--      rows keep notifying after the migration runs.

CREATE TABLE IF NOT EXISTS app_settings_v1 (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

ALTER TABLE accounts ADD COLUMN signature TEXT;
ALTER TABLE accounts ADD COLUMN notify_enabled INTEGER NOT NULL DEFAULT 1;
