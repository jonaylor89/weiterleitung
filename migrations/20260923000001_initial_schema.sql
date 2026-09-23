-- Weiterleitung initial schema (SQLite).
-- UUIDs and timestamps are stored as TEXT, booleans as INTEGER.

CREATE TABLE users (
    user_id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE mailboxes (
    mailbox_id TEXT PRIMARY KEY NOT NULL,
    email TEXT NOT NULL UNIQUE,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE aliases (
    alias_id TEXT PRIMARY KEY NOT NULL,
    address TEXT NOT NULL UNIQUE,
    mailbox_id TEXT NOT NULL REFERENCES mailboxes (mailbox_id) ON DELETE CASCADE,
    note TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    forward_count INTEGER NOT NULL DEFAULT 0,
    reply_count INTEGER NOT NULL DEFAULT 0,
    blocked_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_used_at TEXT
);

CREATE INDEX idx_aliases_mailbox ON aliases (mailbox_id);

-- A contact is a remote party that has written to (or been written to through)
-- an alias. Each one gets a stable reverse alias so replies from the mailbox
-- can be routed back out through the alias.
CREATE TABLE contacts (
    contact_id TEXT PRIMARY KEY NOT NULL,
    alias_id TEXT NOT NULL REFERENCES aliases (alias_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    name TEXT,
    reverse_alias TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    UNIQUE (alias_id, email)
);

CREATE INDEX idx_contacts_alias ON contacts (alias_id);

-- Outbound queue. The raw RFC 5322 message lives on disk at `raw_path`; the row
-- carries the envelope plus retry bookkeeping.
CREATE TABLE outbox (
    message_id TEXT PRIMARY KEY NOT NULL,
    direction TEXT NOT NULL,
    envelope_from TEXT NOT NULL,
    envelope_to TEXT NOT NULL,
    raw_path TEXT NOT NULL,
    alias_id TEXT REFERENCES aliases (alias_id) ON DELETE SET NULL,
    contact_id TEXT REFERENCES contacts (contact_id) ON DELETE SET NULL,
    subject TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL,
    delivered_at TEXT
);

CREATE INDEX idx_outbox_status ON outbox (status, next_attempt_at);
