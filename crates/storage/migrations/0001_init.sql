-- FileMind schema v1. Mirrors docs/ARCHITECTURE.md §5.
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS roots (
  root_id     INTEGER PRIMARY KEY,
  path        TEXT NOT NULL UNIQUE,
  mode        TEXT,                       -- per-root override: observe|assist|automate|NULL
  added_ts    INTEGER NOT NULL,
  last_scan   INTEGER
);

CREATE TABLE IF NOT EXISTS files (
  file_id     TEXT PRIMARY KEY,           -- "<device>:<index>"
  root_id     INTEGER NOT NULL REFERENCES roots(root_id),
  path        TEXT NOT NULL,
  name        TEXT NOT NULL,
  ext         TEXT,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,
  ctime       INTEGER,
  birthtime   INTEGER,
  blob_id     INTEGER REFERENCES blobs(blob_id),
  kind        TEXT NOT NULL,              -- file|dir|link|other
  is_link     INTEGER NOT NULL DEFAULT 0,
  status      TEXT NOT NULL DEFAULT 'present',
  sensitive   INTEGER NOT NULL DEFAULT 0,
  first_seen  INTEGER NOT NULL,
  last_seen   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS files_path ON files(path);
CREATE INDEX IF NOT EXISTS files_blob ON files(blob_id);
CREATE INDEX IF NOT EXISTS files_root_status ON files(root_id, status);

CREATE TABLE IF NOT EXISTS blobs (
  blob_id         INTEGER PRIMARY KEY,
  blake3          TEXT NOT NULL UNIQUE,
  size            INTEGER NOT NULL,
  text_extracted  INTEGER NOT NULL DEFAULT 0,
  embedding_id    TEXT
);

CREATE TABLE IF NOT EXISTS file_events (
  event_id    INTEGER PRIMARY KEY,
  file_id     TEXT NOT NULL,
  ts          INTEGER NOT NULL,
  type        TEXT NOT NULL,              -- created|modified|renamed|moved|deleted|restored
  from_path   TEXT,
  to_path     TEXT,
  source      TEXT NOT NULL               -- watcher|scan|txn
);
CREATE INDEX IF NOT EXISTS file_events_file ON file_events(file_id, ts);

CREATE TABLE IF NOT EXISTS classifications (
  file_id     TEXT NOT NULL REFERENCES files(file_id),
  category    TEXT NOT NULL,
  confidence  REAL NOT NULL,
  signals     TEXT NOT NULL,              -- JSON array
  source      TEXT NOT NULL,              -- rule|ml|llm|user
  ts          INTEGER NOT NULL,
  PRIMARY KEY (file_id, source)
);

CREATE TABLE IF NOT EXISTS projects (
  project_id      INTEGER PRIMARY KEY,
  name            TEXT,
  suggested_name  TEXT,
  start_ts        INTEGER,
  end_ts          INTEGER,
  activity_score  REAL NOT NULL DEFAULT 0,
  status          TEXT NOT NULL DEFAULT 'active'
);

CREATE TABLE IF NOT EXISTS project_files (
  project_id  INTEGER NOT NULL REFERENCES projects(project_id),
  file_id     TEXT NOT NULL REFERENCES files(file_id),
  role        TEXT,
  confidence  REAL NOT NULL DEFAULT 1,
  PRIMARY KEY (project_id, file_id)
);

CREATE TABLE IF NOT EXISTS version_chains (
  chain_id           INTEGER PRIMARY KEY,
  canonical_file_id  TEXT REFERENCES files(file_id)
);

CREATE TABLE IF NOT EXISTS version_members (
  chain_id  INTEGER NOT NULL REFERENCES version_chains(chain_id),
  file_id   TEXT NOT NULL REFERENCES files(file_id),
  ordinal   INTEGER NOT NULL,
  mtime     INTEGER NOT NULL,
  PRIMARY KEY (chain_id, file_id)
);

CREATE TABLE IF NOT EXISTS duplicate_groups (
  group_id        INTEGER PRIMARY KEY,
  blob_id         INTEGER NOT NULL REFERENCES blobs(blob_id),
  keeper_file_id  TEXT REFERENCES files(file_id)
);

CREATE TABLE IF NOT EXISTS transactions (
  txn_id       TEXT PRIMARY KEY,
  mode         TEXT NOT NULL,
  initiator    TEXT NOT NULL,
  rule_id      TEXT,
  state        TEXT NOT NULL,             -- planned|running|done|undone|failed
  created_ts   INTEGER NOT NULL,
  executed_ts  INTEGER,
  manifest     TEXT NOT NULL              -- JSON, written before execution
);

CREATE TABLE IF NOT EXISTS txn_steps (
  txn_id       TEXT NOT NULL REFERENCES transactions(txn_id),
  step_no      INTEGER NOT NULL,
  file_id      TEXT,
  from_path    TEXT,
  to_path      TEXT,
  hash_before  TEXT,
  state        TEXT NOT NULL DEFAULT 'planned',
  PRIMARY KEY (txn_id, step_no)
);

CREATE TABLE IF NOT EXISTS memory_notes (
  note_id       INTEGER PRIMARY KEY,
  subject_type  TEXT NOT NULL,            -- file|project
  subject_id    TEXT NOT NULL,
  text          TEXT NOT NULL,
  source        TEXT NOT NULL,            -- user|system|llm
  ts            INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS health_snapshots (
  ts          INTEGER NOT NULL,
  root_id     INTEGER REFERENCES roots(root_id),
  score       REAL NOT NULL,
  components  TEXT NOT NULL               -- JSON
);

CREATE TABLE IF NOT EXISTS ai_audit (
  id          INTEGER PRIMARY KEY,
  ts          INTEGER NOT NULL,
  adapter     TEXT NOT NULL,
  purpose     TEXT NOT NULL,
  bytes_sent  INTEGER NOT NULL,
  file_id     TEXT
);

CREATE TABLE IF NOT EXISTS settings (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL                    -- JSON
);

-- Lexical search. External-content table so the inventory stays the source of truth.
CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
  name, path_tokens, extracted_text,
  content='', contentless_delete=1, tokenize='unicode61'
);

INSERT OR IGNORE INTO settings(key, value) VALUES ('mode', '"observe"');
