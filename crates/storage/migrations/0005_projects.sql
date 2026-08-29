-- Projects are derived, but a user-chosen name or a pin must survive rebuilds,
-- so each project has a stable key.
ALTER TABLE projects ADD COLUMN key TEXT;
ALTER TABLE projects ADD COLUMN kind TEXT NOT NULL DEFAULT 'folder';
ALTER TABLE projects ADD COLUMN root_path TEXT;
ALTER TABLE projects ADD COLUMN file_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN updated_ts INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX IF NOT EXISTS projects_key ON projects(key);
CREATE INDEX IF NOT EXISTS project_files_file ON project_files(file_id);
