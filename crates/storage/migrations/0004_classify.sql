-- Rules learned from user corrections, applied before any automatic layer.
CREATE TABLE IF NOT EXISTS user_rules (
  rule_id     INTEGER PRIMARY KEY,
  kind        TEXT NOT NULL,        -- path_prefix | name_contains | ext
  pattern     TEXT NOT NULL,
  category    TEXT NOT NULL,
  created_ts  INTEGER NOT NULL,
  UNIQUE(kind, pattern)
);

-- Which mtime a file was classified/extracted at, so edits trigger a redo.
ALTER TABLE files ADD COLUMN classified_mtime INTEGER;
ALTER TABLE files ADD COLUMN sensitive_kind TEXT;
CREATE INDEX IF NOT EXISTS files_classify_pending ON files(status, kind, classified_mtime);
