-- Phase 10: beta instrumentation. Counters only — no paths, names or content
-- are ever stored here, so the daily telemetry document (docs/TELEMETRY.md)
-- can be built from this table alone.
CREATE TABLE IF NOT EXISTS metrics (
  day   TEXT NOT NULL,        -- YYYY-MM-DD (UTC)
  key   TEXT NOT NULL,
  value INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, key)
);

-- One row per process run of the agent or the app. A session that never
-- ended cleanly is counted as a crash by the next session of the same kind.
CREATE TABLE IF NOT EXISTS sessions (
  session_id INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,   -- agent | desktop | cli
  started_ts INTEGER NOT NULL,
  ended_ts   INTEGER,
  clean      INTEGER          -- 1 clean exit, 0 crashed/killed, NULL still open
);
CREATE INDEX IF NOT EXISTS sessions_kind_open ON sessions(kind, ended_ts);

INSERT OR IGNORE INTO settings(key, value) VALUES ('telemetry.enabled', 'false');
INSERT OR IGNORE INTO settings(key, value) VALUES ('telemetry.endpoint', '""');
