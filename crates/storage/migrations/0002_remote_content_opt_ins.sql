-- SPDX-License-Identifier: Apache-2.0
--
-- Capytain schema v2. Phase 1 Week 8: per-sender remote-content opt-in.
--
-- One row per `(account_id, email_address)` pair the user has explicitly
-- chosen to trust for remote content (images, stylesheets, fonts that
-- ammonia + the adblock engine would otherwise block). Lookup happens
-- inside `messages_get` against the message's `from[0].address`.
--
-- The trust decision is per-account because the same person sending you
-- mail to two different accounts may be trusted on one and not the
-- other (work account vs personal account). Email addresses are stored
-- lowercase to make lookups case-insensitive without needing a
-- functional index.

CREATE TABLE IF NOT EXISTS remote_content_opt_ins (
    account_id     TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email_address  TEXT    NOT NULL,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (account_id, email_address)
);

CREATE INDEX IF NOT EXISTS remote_content_opt_ins_account
    ON remote_content_opt_ins(account_id);
