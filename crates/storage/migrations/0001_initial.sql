-- SPDX-License-Identifier: Apache-2.0
--
-- Capytain schema v1. Matches DESIGN.md §4.4 with minor column-naming
-- adjustments for SQLite idiomatics (snake_case, explicit NOT NULL, no
-- backticks). Addresses, labels, and anything with a variable shape are
-- stored as TEXT holding JSON; numbers are INTEGER; bodies live on disk
-- under <data_dir>/blobs/ and are referenced here by path.
--
-- All timestamps are UNIX seconds (INTEGER). Offset-naïve UTC — we never
-- store a local-tz value.

CREATE TABLE IF NOT EXISTS accounts (
    id                   TEXT    PRIMARY KEY,
    kind                 TEXT    NOT NULL CHECK (kind IN ('imap_smtp', 'jmap')),
    display_name         TEXT    NOT NULL,
    email_address        TEXT    NOT NULL,
    auth_ref             TEXT,
    server_config_json   TEXT,
    created_at           INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS accounts_email_address ON accounts(email_address);

CREATE TABLE IF NOT EXISTS folders (
    id                   TEXT    PRIMARY KEY,
    account_id           TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name                 TEXT    NOT NULL,
    path                 TEXT    NOT NULL,
    role                 TEXT,
    unread_count         INTEGER NOT NULL DEFAULT 0,
    total_count          INTEGER NOT NULL DEFAULT 0,
    parent_id            TEXT    REFERENCES folders(id) ON DELETE SET NULL,
    -- Opaque per-folder sync cursor (see capytain-core::SyncState). For IMAP
    -- this carries the serialized (uidvalidity, highestmodseq, uidnext)
    -- tuple; for JMAP it carries the server's state token verbatim.
    sync_state           TEXT
);

CREATE INDEX IF NOT EXISTS folders_account_id ON folders(account_id);
CREATE UNIQUE INDEX IF NOT EXISTS folders_account_path ON folders(account_id, path);

CREATE TABLE IF NOT EXISTS threads (
    id                   TEXT    PRIMARY KEY,
    account_id           TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    root_message_id      TEXT,
    subject_normalized   TEXT,
    last_date            INTEGER,
    message_count        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS threads_account_id ON threads(account_id);
CREATE INDEX IF NOT EXISTS threads_last_date ON threads(account_id, last_date DESC);

CREATE TABLE IF NOT EXISTS messages (
    id                   TEXT    PRIMARY KEY,
    account_id           TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder_id            TEXT    NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    thread_id            TEXT    REFERENCES threads(id) ON DELETE SET NULL,
    rfc822_message_id    TEXT,
    subject              TEXT    NOT NULL DEFAULT '',
    from_json            TEXT    NOT NULL DEFAULT '[]',
    reply_to_json        TEXT    NOT NULL DEFAULT '[]',
    to_json              TEXT    NOT NULL DEFAULT '[]',
    cc_json              TEXT    NOT NULL DEFAULT '[]',
    bcc_json             TEXT    NOT NULL DEFAULT '[]',
    date                 INTEGER NOT NULL,
    flags_json           TEXT    NOT NULL DEFAULT '{}',
    labels_json          TEXT    NOT NULL DEFAULT '[]',
    snippet              TEXT    NOT NULL DEFAULT '',
    size                 INTEGER NOT NULL DEFAULT 0,
    has_attachments      INTEGER NOT NULL DEFAULT 0,
    body_path            TEXT,
    indexed_at           INTEGER
);

CREATE INDEX IF NOT EXISTS messages_folder_date ON messages(folder_id, date DESC);
CREATE INDEX IF NOT EXISTS messages_account_date ON messages(account_id, date DESC);
CREATE INDEX IF NOT EXISTS messages_thread ON messages(thread_id, date);
CREATE INDEX IF NOT EXISTS messages_rfc822 ON messages(rfc822_message_id);

CREATE TABLE IF NOT EXISTS attachments (
    id                   TEXT    PRIMARY KEY,
    message_id           TEXT    NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename             TEXT    NOT NULL,
    mime_type            TEXT    NOT NULL,
    size                 INTEGER NOT NULL,
    inline               INTEGER NOT NULL DEFAULT 0,
    content_id           TEXT,
    path                 TEXT
);

CREATE INDEX IF NOT EXISTS attachments_message ON attachments(message_id);

CREATE TABLE IF NOT EXISTS outbox (
    id                   TEXT    PRIMARY KEY,
    account_id           TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    op_kind              TEXT    NOT NULL,
    payload_json         TEXT    NOT NULL,
    created_at           INTEGER NOT NULL,
    attempts             INTEGER NOT NULL DEFAULT 0,
    next_attempt_at      INTEGER,
    last_error           TEXT
);

CREATE INDEX IF NOT EXISTS outbox_pending ON outbox(next_attempt_at) WHERE next_attempt_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS outbox_account ON outbox(account_id);

CREATE TABLE IF NOT EXISTS contacts (
    id                   TEXT    PRIMARY KEY,
    account_id           TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    address              TEXT    NOT NULL,
    display_name         TEXT,
    frequency            INTEGER NOT NULL DEFAULT 0,
    last_seen            INTEGER,
    trusted_for_remote   INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS contacts_account_address ON contacts(account_id, address);
CREATE INDEX IF NOT EXISTS contacts_frequency ON contacts(account_id, frequency DESC);
