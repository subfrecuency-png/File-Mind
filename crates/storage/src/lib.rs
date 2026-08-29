//! SQLite system of record (WAL + FTS5) and, later, the LanceDB vector store.

pub mod analysis;
pub mod classify;
pub mod inventory;

pub use analysis::{DupGroup, Suggestion, VersionChain};
pub use inventory::{Root, UpsertStats};

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Embedded migrations, applied in order. Add new files to the end; never edit old ones.
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../migrations/0001_init.sql")),
    (
        "0002_scan_seq",
        include_str!("../migrations/0002_scan_seq.sql"),
    ),
    (
        "0003_suggestions",
        include_str!("../migrations/0003_suggestions.sql"),
    ),
    (
        "0004_classify",
        include_str!("../migrations/0004_classify.sql"),
    ),
];

/// Default per-user database location:
/// macOS `~/Library/Application Support/FileMind/filemind.db`,
/// Windows `%LOCALAPPDATA%\FileMind\filemind.db`.
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "FileMind")
        .context("could not resolve a per-user data directory")?;
    let dir = dirs.data_local_dir().to_path_buf();
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("filemind.db");
    // Early builds wrote to a qualifier-prefixed folder on macOS; adopt it once.
    if !db.exists() {
        if let Some(legacy) = directories::ProjectDirs::from("ai", "FileMind", "FileMind") {
            let old = legacy.data_local_dir().join("filemind.db");
            if old != db && old.exists() {
                for suffix in ["", "-wal", "-shm"] {
                    let from = PathBuf::from(format!("{}{suffix}", old.display()));
                    if from.exists() {
                        std::fs::rename(&from, PathBuf::from(format!("{}{suffix}", db.display())))?;
                    }
                }
                tracing::info!(from = %old.display(), to = %db.display(), "moved database");
            }
        }
    }
    Ok(db)
}

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // Writers start with BEGIN IMMEDIATE so a second writer waits on the
        // busy timeout instead of failing with SQLITE_BUSY when it tries to
        // upgrade a deferred read transaction (the classic WAL "database is
        // locked" with several connections).
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_ts INTEGER NOT NULL);",
        )?;
        for (name, sql) in MIGRATIONS {
            let applied: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE name = ?1",
                    [name],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)?;
            if applied {
                continue;
            }
            tracing::info!(migration = name, "applying");
            self.conn
                .execute_batch(sql)
                .with_context(|| format!("migration {name}"))?;
            self.conn.execute(
                "INSERT INTO schema_migrations(name, applied_ts) VALUES (?1, strftime('%s','now'))",
                [name],
            )?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| {
                r.get::<_, i64>(0)
            })? as usize)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let v: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .ok();
        Ok(v.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, &value.to_string()],
        )?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let q = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(Counts {
            roots: q("SELECT COUNT(*) FROM roots")?,
            files: q("SELECT COUNT(*) FROM files WHERE kind = 'file' AND status = 'present'")?,
            dirs: q("SELECT COUNT(*) FROM files WHERE kind = 'dir' AND status = 'present'")?,
            missing: q("SELECT COUNT(*) FROM files WHERE status = 'missing'")?,
            hashed: q("SELECT COUNT(*) FROM files WHERE kind = 'file' AND status = 'present' AND blob_id IS NOT NULL")?,
            bytes: q("SELECT COALESCE(SUM(size),0) FROM files WHERE kind = 'file' AND status = 'present'")?,
            events: q("SELECT COUNT(*) FROM file_events")?,
            transactions: q("SELECT COUNT(*) FROM transactions")?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Counts {
    pub roots: i64,
    pub files: i64,
    pub dirs: i64,
    pub missing: i64,
    pub hashed: i64,
    pub bytes: i64,
    pub events: i64,
    pub transactions: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_fresh_db_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.db");
        {
            let db = Db::open(&p).unwrap();
            assert_eq!(db.schema_version().unwrap(), 4);
            assert_eq!(
                db.get_setting("mode").unwrap(),
                Some(serde_json::json!("observe"))
            );
        }
        let db = Db::open(&p).unwrap();
        assert_eq!(db.schema_version().unwrap(), 4);
        let c = db.counts().unwrap();
        assert_eq!((c.roots, c.files, c.transactions), (0, 0, 0));
    }

    #[test]
    fn fts_table_exists() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO files_fts(rowid, name, path_tokens, extracted_text) VALUES (1, 'budget.xlsx', 'documents taxes', 'schedule c')",
                [],
            )
            .unwrap();
        let hit: i64 = db
            .conn
            .query_row(
                "SELECT rowid FROM files_fts WHERE files_fts MATCH 'schedule'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1);
    }
}
