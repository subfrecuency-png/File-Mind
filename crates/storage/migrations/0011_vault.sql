-- Vault V0: custody, sealed objects, local audit (metadata only — never MK/DEK/plaintext).

ALTER TABLE files ADD COLUMN custody TEXT NOT NULL DEFAULT 'open';
ALTER TABLE files ADD COLUMN seal_id TEXT;
ALTER TABLE files ADD COLUMN sensitivity TEXT;
CREATE INDEX IF NOT EXISTS files_custody ON files(custody);
CREATE INDEX IF NOT EXISTS files_seal_id ON files(seal_id);

CREATE TABLE seal_objects (
    seal_id           TEXT PRIMARY KEY,
    file_id           TEXT,
    original_path     TEXT NOT NULL,
    original_name     TEXT NOT NULL,
    object_path       TEXT NOT NULL,
    sensitivity       TEXT NOT NULL,
    plaintext_blake3  TEXT NOT NULL,
    size              INTEGER NOT NULL,
    sealed_ts         INTEGER NOT NULL,
    policy_version    INTEGER NOT NULL DEFAULT 1,
    format            TEXT NOT NULL DEFAULT 'fmseal/1',
    txn_id            TEXT,
    unsealed_ts       INTEGER
);

CREATE TABLE vault_audit (
    id        INTEGER PRIMARY KEY,
    ts        INTEGER NOT NULL,
    action    TEXT NOT NULL,   -- seal | unseal | undo | list | status
    seal_id   TEXT,
    path      TEXT,
    detail    TEXT             -- metadata only
);
