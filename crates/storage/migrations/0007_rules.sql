-- Phase 9: Automate mode. A rule is an allow-listed, tier-0 action the
-- agent may run unattended — but only after it has shown its work: every
-- scheduler tick records a dry run, and a rule can be armed only once it has
-- been previewing for the configured number of days (default 7).
CREATE TABLE IF NOT EXISTS rules (
  rule_id      INTEGER PRIMARY KEY,
  kind         TEXT NOT NULL,              -- archive_stale_downloads | collapse_versions | trash_exact_duplicates
  params       TEXT NOT NULL,              -- JSON, validated by core::rules
  tier         INTEGER NOT NULL DEFAULT 0, -- always 0: only tier-0 kinds exist
  state        TEXT NOT NULL DEFAULT 'preview', -- preview | armed | paused
  created_ts   INTEGER NOT NULL,
  armed_ts     INTEGER,
  paused_ts    INTEGER,
  paused_reason TEXT
);

-- What each tick would have done (dry_run = 1) or did (dry_run = 0, txn_id set).
CREATE TABLE IF NOT EXISTS rule_runs (
  run_id    INTEGER PRIMARY KEY,
  rule_id   INTEGER NOT NULL REFERENCES rules(rule_id) ON DELETE CASCADE,
  ts        INTEGER NOT NULL,
  dry_run   INTEGER NOT NULL,
  txn_id    TEXT,
  manifest  TEXT NOT NULL,                 -- JSON manifest the run evaluated
  summary   TEXT NOT NULL                  -- JSON: {files, bytes, problems, capped, ...}
);
CREATE INDEX IF NOT EXISTS rule_runs_rule_ts ON rule_runs(rule_id, ts);

INSERT OR IGNORE INTO settings(key, value) VALUES ('automate.preview_days', '7');
