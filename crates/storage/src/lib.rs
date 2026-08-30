//! SQLite system of record (WAL + FTS5) and, later, the LanceDB vector store.

pub mod analysis;
pub mod classify;
pub mod folderdups;
pub mod inventory;
pub mod journal;
pub mod keyring;
pub mod metrics;
pub mod projects;
pub mod rules;
pub mod semantic;

pub use analysis::{DupGroup, Suggestion, VersionChain};
pub use folderdups::FolderDup;
pub use inventory::{Root, UpsertStats};
pub use journal::TxnSummary;
pub use projects::ProjectRow;
pub use rules::{Rule, RuleRun};

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
    (
        "0005_projects",
        include_str!("../migrations/0005_projects.sql"),
    ),
    (
        "0006_semantic",
        include_str!("../migrations/0006_semantic.sql"),
    ),
    ("0007_rules", include_str!("../migrations/0007_rules.sql")),
    ("0008_beta", include_str!("../migrations/0008_beta.sql")),
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
    /// Open (creating if needed) the database at `path`. With the `encrypt`
    /// feature the file is SQLCipher-encrypted with the key from
    /// `keyring`; a plaintext database from an earlier build is converted
    /// in place on first open (the plaintext copy is kept as
    /// `filemind.db.pre-sqlcipher` until the next successful open, when
    /// the agent moves it to the Trash).
    pub fn open(path: &Path) -> Result<Self> {
        #[cfg(feature = "encrypt")]
        {
            if std::env::var_os("FILEMIND_PLAINTEXT_DB").is_none() {
                let dir = path.parent().unwrap_or(Path::new("."));
                // the Keychain guards the real database only; anything else
                // (tests, copies) gets a key file beside it
                let is_real = default_db_path().map(|p| p == path).unwrap_or(false);
                let key = if is_real {
                    keyring::db_key(dir)?
                } else {
                    keyring::file_key(dir)?
                };
                return Self::open_with_key(path, Some(&key));
            }
        }
        Self::open_with_key(path, None)
    }

    /// `key = None` opens plaintext. Public for tests and tooling.
    pub fn open_with_key(path: &Path, key: Option<&[u8; 32]>) -> Result<Self> {
        let mut converted = false;
        if let Some(k) = key {
            if Self::needs_encryption(path, k)? {
                Self::encrypt_in_place(path, k)?;
                converted = true;
            }
        }
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        if let Some(k) = key {
            conn.execute_batch(&format!("PRAGMA key = {};", keyring::pragma_value(k)))?;
            // fail here, with a clear message, rather than at the first query
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
                .with_context(|| format!("{} cannot be opened with the stored key (wrong key, or a corrupt file). Set FILEMIND_DB_KEY to the right key, or move the file aside to start fresh.", path.display()))?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Generous: background jobs would rather wait a minute than fail; the
        // agent additionally serialises its own heavy jobs (agent::jobs).
        conn.busy_timeout(std::time::Duration::from_secs(60))?;
        // Writers start with BEGIN IMMEDIATE so a second writer waits on the
        // busy timeout instead of failing with SQLITE_BUSY when it tries to
        // upgrade a deferred read transaction (the classic WAL "database is
        // locked" with several connections).
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        let db = Self { conn };
        db.migrate()?;
        if converted {
            // remembered so the plaintext copy is only trashed by a *later*
            // process that opened the encrypted file successfully
            db.set_setting(
                "encrypt.converted_by_pid",
                &serde_json::json!(std::process::id()),
            )?;
        }
        Ok(db)
    }

    /// The plaintext copy left by the conversion may go once a process other
    /// than the converting one has opened the encrypted file successfully.
    pub fn pre_cipher_backup_ready_to_trash(&self) -> Result<Option<PathBuf>> {
        let Some(path) = self.path() else {
            return Ok(None);
        };
        let backup = Self::pre_cipher_backup(&path);
        if !backup.is_file() {
            return Ok(None);
        }
        let by = self
            .get_setting("encrypt.converted_by_pid")?
            .and_then(|v| v.as_u64());
        Ok((by != Some(u64::from(std::process::id()))).then_some(backup))
    }

    /// True when `path` exists and is a plaintext SQLite file (header
    /// "SQLite format 3\0"); an encrypted file has a random-looking header.
    fn needs_encryption(path: &Path, _key: &[u8; 32]) -> Result<bool> {
        let mut head = [0u8; 16];
        match std::fs::File::open(path) {
            Ok(mut f) => {
                let n = std::io::Read::read(&mut f, &mut head)?;
                Ok(n == 16 && &head == b"SQLite format 3\0")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The plaintext copy kept beside an encrypted database after conversion.
    pub fn pre_cipher_backup(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "filemind.db".into());
        path.with_file_name(format!("{name}.pre-sqlcipher"))
    }

    /// One-time conversion: export the plaintext database into a new
    /// encrypted file, then swap them. The plaintext file is renamed, never
    /// deleted.
    fn encrypt_in_place(path: &Path, key: &[u8; 32]) -> Result<()> {
        // An agent (of any build) still holding the plaintext file open would
        // keep writing to the renamed copy after the swap. Refuse until it is
        // stopped; the socket lives next to the database.
        #[cfg(unix)]
        {
            let sock = path.with_file_name("agent.sock");
            if sock.exists() && std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                anyhow::bail!(
                    "the database needs a one-time encryption, but an agent is running. Stop it first (`filemind agent stop`, or Settings → Background agent → Stop) and try again."
                );
            }
        }
        let enc = path.with_extension("db.enc-tmp");
        let backup = Self::pre_cipher_backup(path);
        if enc.exists() {
            // an earlier attempt died mid-export; start the export over
            std::fs::rename(&enc, enc.with_extension("enc-tmp.stale"))?;
        }
        tracing::info!(db = %path.display(), "encrypting the database (one-time)");
        let plain = Connection::open(path)?;
        // fold the WAL into the main file so the backup is self-contained
        plain.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")?;
        plain
            .execute_batch(&format!(
                "ATTACH DATABASE '{}' AS enc KEY {};
             SELECT sqlcipher_export('enc');
             DETACH DATABASE enc;",
                enc.display().to_string().replace('\'', "''"),
                keyring::pragma_value(key)
            ))
            .context("sqlcipher_export")?;
        drop(plain);
        // verify before swapping: the new file must open with the key
        {
            let check = Connection::open(&enc)?;
            check.execute_batch(&format!("PRAGMA key = {};", keyring::pragma_value(key)))?;
            let n: i64 = check.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            anyhow::ensure!(n > 0, "encrypted copy is empty");
        }
        if backup.exists() {
            std::fs::rename(&backup, backup.with_extension("pre-sqlcipher.older"))?;
        }
        std::fs::rename(path, &backup)?;
        std::fs::rename(&enc, path)?;
        tracing::info!(backup = %backup.display(), "database encrypted; plaintext copy kept until the next successful open");
        Ok(())
    }

    /// Where this connection's file lives (None for in-memory).
    pub fn path(&self) -> Option<PathBuf> {
        self.conn
            .path()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
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
        // keep the test off the real Keychain / key file
        std::env::set_var("FILEMIND_DB_KEY", "00".repeat(32));
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("t.db");
        {
            let db = Db::open(&p).unwrap();
            assert_eq!(db.schema_version().unwrap(), 8);
            assert_eq!(
                db.get_setting("mode").unwrap(),
                Some(serde_json::json!("observe"))
            );
        }
        let db = Db::open(&p).unwrap();
        assert_eq!(db.schema_version().unwrap(), 8);
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

#[cfg(all(test, feature = "encrypt"))]
mod encrypt_tests {
    use super::*;

    #[test]
    fn plaintext_db_is_converted_once_and_keeps_a_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("filemind.db");
        // a plaintext database from an older build, with some content
        {
            let db = Db::open_with_key(&path, None).unwrap();
            db.set_setting("mode", &serde_json::json!("assist"))
                .unwrap();
        }
        assert!(Db::needs_encryption(&path, &[0; 32]).unwrap());
        let key = [7u8; 32];
        {
            let db = Db::open_with_key(&path, Some(&key)).unwrap();
            assert_eq!(
                db.get_setting("mode").unwrap(),
                Some(serde_json::json!("assist"))
            );
            let v: String = db
                .conn
                .query_row("PRAGMA cipher_version", [], |r| r.get(0))
                .unwrap();
            assert!(v.contains("4."), "{v}");
        }
        assert!(!Db::needs_encryption(&path, &key).unwrap(), "now encrypted");
        assert!(Db::pre_cipher_backup(&path).exists(), "plaintext copy kept");
        // wrong key is refused clearly; right key keeps working; FTS still there
        let err = match Db::open_with_key(&path, Some(&[9u8; 32])) {
            Ok(_) => panic!("wrong key accepted"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("stored key"), "{err}");
        let db = Db::open_with_key(&path, Some(&key)).unwrap();
        db.conn
            .execute_batch("INSERT INTO notes_fts(rowid, text) VALUES (1, 'hello world')")
            .unwrap();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM notes_fts WHERE notes_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }
}
