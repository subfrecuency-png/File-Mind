//! SQLite system of record (WAL + FTS5) and, later, the LanceDB vector store.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Embedded migrations, applied in order. Add new files to the end; never edit old ones.
const MIGRATIONS: &[(&str, &str)] = &[("0001_init", include_str!("../migrations/0001_init.sql"))];

/// Default per-user database location:
/// macOS `~/Library/Application Support/FileMind/filemind.db`,
/// Windows `%LOCALAPPDATA%\FileMind\filemind.db`.
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("ai", "FileMind", "FileMind")
        .context("could not resolve a per-user data directory")?;
    let dir = dirs.data_local_dir();
    std::fs::create_dir_all(dir)?;
    Ok(dir.join("filemind.db"))
}

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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
            files: q("SELECT COUNT(*) FROM files WHERE kind = 'file'")?,
            transactions: q("SELECT COUNT(*) FROM transactions")?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Counts {
    pub roots: i64,
    pub files: i64,
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
            assert_eq!(db.schema_version().unwrap(), 1);
            assert_eq!(
                db.get_setting("mode").unwrap(),
                Some(serde_json::json!("observe"))
            );
        }
        let db = Db::open(&p).unwrap();
        assert_eq!(db.schema_version().unwrap(), 1);
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
