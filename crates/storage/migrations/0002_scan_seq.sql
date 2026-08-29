-- Per-root scan generation counter so two scans within the same second
-- still distinguish "seen this scan" from "not seen".
ALTER TABLE roots ADD COLUMN scan_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE files ADD COLUMN last_scan_seq INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS files_root_seq ON files(root_id, last_scan_seq);
