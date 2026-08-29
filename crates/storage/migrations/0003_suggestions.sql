-- Observe-mode suggestions. Nothing here is ever executed automatically;
-- a suggestion becomes a transaction only when a user approves it (Phase 6).
CREATE TABLE IF NOT EXISTS suggestions (
  suggestion_id  INTEGER PRIMARY KEY,
  kind           TEXT NOT NULL,             -- trash_duplicates | collapse_versions | stale_downloads
  key            TEXT NOT NULL,             -- stable identity (e.g. blob_id, chain_id, root_id) so regeneration updates in place
  subject        TEXT NOT NULL,             -- JSON: paths and roles
  rationale      TEXT NOT NULL,
  est_bytes      INTEGER NOT NULL DEFAULT 0,
  risk_tier      INTEGER NOT NULL,          -- 0 reversible+low, 1 reversible, 2 trashes
  state          TEXT NOT NULL DEFAULT 'proposed', -- proposed | dismissed | accepted | stale
  created_ts     INTEGER NOT NULL,
  updated_ts     INTEGER NOT NULL,
  UNIQUE(kind, key)
);
CREATE INDEX IF NOT EXISTS suggestions_state ON suggestions(state, kind);

-- duplicate_groups gets a unique blob so rebuilds can upsert
CREATE UNIQUE INDEX IF NOT EXISTS duplicate_groups_blob ON duplicate_groups(blob_id);
