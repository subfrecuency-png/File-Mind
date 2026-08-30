-- Phase 7: semantic search + memory.

-- First ~2 KB of extracted text, kept so embeddings can be (re)built without
-- re-parsing the file. The FTS mirror is contentless and cannot give it back.
CREATE TABLE IF NOT EXISTS file_text (
  file_id   TEXT PRIMARY KEY REFERENCES files(file_id),
  mtime     INTEGER NOT NULL,
  head      TEXT NOT NULL
);

-- One vector per subject. `subject` is a file_id or "note:<note_id>".
-- Vectors are unit-length, stored as int8 (x * 127) — 384 bytes each.
CREATE TABLE IF NOT EXISTS embeddings (
  subject    TEXT PRIMARY KEY,
  model      TEXT NOT NULL,
  dim        INTEGER NOT NULL,
  vec        BLOB NOT NULL,
  input_hash TEXT NOT NULL,              -- blake3 of the embedded text
  ts         INTEGER NOT NULL
);

-- Memory notes get a lexical index too.
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
  text, content='', contentless_delete=1, tokenize='unicode61'
);

-- Audit log: what left the machine (or went to the local model), never the text itself.
ALTER TABLE ai_audit ADD COLUMN snippet_hash TEXT;
ALTER TABLE ai_audit ADD COLUMN local INTEGER NOT NULL DEFAULT 1;
ALTER TABLE ai_audit ADD COLUMN ok INTEGER NOT NULL DEFAULT 1;
ALTER TABLE ai_audit ADD COLUMN latency_ms INTEGER;

INSERT OR IGNORE INTO settings(key, value) VALUES ('ai.adapter', '"none"');
