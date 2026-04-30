-- SPDX-FileCopyrightText: 2026 QSL Contributors
-- SPDX-License-Identifier: Apache-2.0

-- Outbox dedup key. Lets a producer coalesce repeated enqueues for the
-- same logical mutation: e.g. the compose pane's 5-second auto-save
-- tick would otherwise enqueue a fresh `save_draft` row on every
-- keystroke burst, spamming the server with a flurry of `APPEND`s.
-- With a dedup key, the producer either updates the existing pending
-- row's payload or no-ops if the row is already in flight.
--
-- `dedup_key` is nullable so existing op kinds (`update_flags`,
-- `move_messages`, `delete_messages`, `submit_message`) keep their
-- old "every enqueue is a new row" semantics — only `save_draft`
-- needs the coalesce today, and any future op can opt in by passing
-- a non-NULL key. The unique index is partial so the NULL rows can
-- coexist freely.

ALTER TABLE outbox ADD COLUMN dedup_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS outbox_dedup
    ON outbox(account_id, op_kind, dedup_key)
    WHERE dedup_key IS NOT NULL;
