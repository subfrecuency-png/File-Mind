-- Cold-project archives: one pack file per archive, chunk-level dedup
-- across packs, members stay searchable via files.location.
ALTER TABLE files ADD COLUMN location TEXT;
CREATE INDEX idx_files_location ON files(location) WHERE location IS NOT NULL;

CREATE TABLE archives (
    archive_id   TEXT PRIMARY KEY,
    project_id   INTEGER,
    name         TEXT NOT NULL,
    folder       TEXT NOT NULL,          -- original tree that was packed
    pack_path    TEXT NOT NULL,          -- the .fmpack file
    created_ts   INTEGER NOT NULL,
    bytes_raw    INTEGER NOT NULL DEFAULT 0,
    bytes_stored INTEGER NOT NULL DEFAULT 0,  -- new chunk bytes in THIS pack
    members      INTEGER NOT NULL DEFAULT 0,
    state        TEXT NOT NULL DEFAULT 'building',  -- building | ready
    txn_id       TEXT
);

CREATE TABLE archive_members (
    archive_id TEXT NOT NULL,
    rel        TEXT NOT NULL,            -- path inside the archive
    kind       TEXT NOT NULL DEFAULT 'file',  -- file | dir | symlink
    size       INTEGER NOT NULL DEFAULT 0,
    mtime      INTEGER NOT NULL DEFAULT 0,
    hash       TEXT NOT NULL DEFAULT '', -- blake3 of the content (files), target (symlinks)
    PRIMARY KEY (archive_id, rel)
);

CREATE TABLE archive_member_chunks (
    archive_id TEXT NOT NULL,
    rel        TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    hash       TEXT NOT NULL,            -- chunk hash → archive_chunks
    PRIMARY KEY (archive_id, rel, seq)
);

-- One row per stored chunk, across every archive: dedup means a chunk is
-- stored in the first pack that needed it and referenced ever after.
CREATE TABLE archive_chunks (
    hash       TEXT PRIMARY KEY,         -- blake3 of the raw chunk
    archive_id TEXT NOT NULL,            -- pack that physically holds it
    off        INTEGER NOT NULL,
    clen       INTEGER NOT NULL,
    ulen       INTEGER NOT NULL,
    dict_id    TEXT NOT NULL DEFAULT ''
);

CREATE TABLE archive_dicts (
    dict_id    TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    category   TEXT NOT NULL,
    data       BLOB NOT NULL
);
