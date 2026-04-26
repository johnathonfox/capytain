-- SPDX-License-Identifier: Apache-2.0
--
-- Capytain schema v4. Phase 2 Week 17: local drafts.
--
-- Drafts persist between app launches and survive process crashes.
-- Phase 2 Week 18 (SMTP) and Week 19 (JMAP) introduce a `save_draft`
-- outbox op_kind that mirrors local rows up to the server's Drafts
-- mailbox; today the table is local-only and the compose pane has no
-- Send button.
--
-- Body is stored as plain text; markdown / multipart bodies arrive in
-- Week 20 alongside `body_kind` taking values beyond `'plain'`.
-- `attachments_json` stores an array of `{ path, filename, mime_type,
-- size_bytes }` objects — the file picker (Week 21) writes the
-- spilled blob path; small attachments inline. For Week 17 the array
-- stays empty.
--
-- `in_reply_to` + `references_json` mirror the columns added on
-- `messages` in 0003, so reply / forward in Week 20 can seed them
-- straight from the source message without a re-shape.
--
-- `created_at` and `updated_at` are unix-epoch seconds matching the
-- shape used elsewhere in the schema.

CREATE TABLE IF NOT EXISTS drafts (
    id                  TEXT NOT NULL PRIMARY KEY,
    account_id          TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    in_reply_to         TEXT,
    references_json     TEXT NOT NULL DEFAULT '[]',
    to_json             TEXT NOT NULL DEFAULT '[]',
    cc_json             TEXT NOT NULL DEFAULT '[]',
    bcc_json            TEXT NOT NULL DEFAULT '[]',
    subject             TEXT NOT NULL DEFAULT '',
    body                TEXT NOT NULL DEFAULT '',
    body_kind           TEXT NOT NULL DEFAULT 'plain',
    attachments_json    TEXT NOT NULL DEFAULT '[]',
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS drafts_account_updated
    ON drafts (account_id, updated_at DESC);
