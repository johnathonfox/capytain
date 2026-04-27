-- SPDX-License-Identifier: Apache-2.0
--
-- QSL schema v3. Phase 1 Week 13: threading.
--
-- The `threads` table itself shipped in 0001 (per `DESIGN.md` §4.4),
-- but message-level `In-Reply-To` and `References` weren't persisted.
-- The thread-assembly pipeline (see `qsl-sync::threading`) needs
-- both to walk the chain when an incoming message has no Message-ID
-- match against an existing thread's root.
--
-- `in_reply_to` is at most one Message-ID per RFC 5322; storing as a
-- TEXT (nullable) is enough. `references_json` is a JSON array of the
-- Message-IDs in the same shape `from_json`/`to_json` already use, so
-- existing `messages_repo::row_to_headers` patterns extend cleanly.
-- The 0001 messages table already includes `thread_id REFERENCES
-- threads(id) ON DELETE SET NULL`, so we don't touch the column itself
-- — only add the lookup keys threading uses to populate it.

ALTER TABLE messages ADD COLUMN in_reply_to TEXT;
ALTER TABLE messages ADD COLUMN references_json TEXT NOT NULL DEFAULT '[]';

CREATE INDEX IF NOT EXISTS messages_in_reply_to ON messages(in_reply_to);
