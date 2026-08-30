-- Phase 11 (Shrink): which in-place rewrite a file currently carries
-- ('apfs' = transparent compression), set by a finished Rewrite step and
-- cleared by undo or by the scanner when the file's size/mtime change
-- (any write to a compressed file makes macOS decompress it).
ALTER TABLE files ADD COLUMN rewrite TEXT;
