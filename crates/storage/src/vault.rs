//! Persistence for sealed objects and vault audit (metadata only).

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SealRow {
    pub seal_id: String,
    pub file_id: Option<String>,
    pub original_path: PathBuf,
    pub original_name: String,
    pub object_path: PathBuf,
    pub sensitivity: String,
    pub plaintext_blake3: String,
    pub size: u64,
    pub sealed_ts: i64,
    pub format: String,
    pub txn_id: Option<String>,
    pub unsealed_ts: Option<i64>,
}

impl Db {
    pub fn insert_seal_object(&self, row: &SealRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO seal_objects(
                seal_id, file_id, original_path, original_name, object_path,
                sensitivity, plaintext_blake3, size, sealed_ts, policy_version,
                format, txn_id, unsealed_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12)
             ON CONFLICT(seal_id) DO UPDATE SET
                file_id = excluded.file_id,
                object_path = excluded.object_path,
                txn_id = excluded.txn_id,
                unsealed_ts = excluded.unsealed_ts",
            params![
                row.seal_id,
                row.file_id,
                row.original_path.to_string_lossy(),
                row.original_name,
                row.object_path.to_string_lossy(),
                row.sensitivity,
                row.plaintext_blake3,
                row.size as i64,
                row.sealed_ts,
                row.format,
                row.txn_id,
                row.unsealed_ts
            ],
        )?;
        Ok(())
    }

    pub fn seal_object(&self, seal_id: &str) -> Result<Option<SealRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT seal_id, file_id, original_path, original_name, object_path,
                        sensitivity, plaintext_blake3, size, sealed_ts, format, txn_id, unsealed_ts
                 FROM seal_objects WHERE seal_id = ?1",
                [seal_id],
                row_from,
            )
            .optional()?)
    }

    pub fn seal_object_by_path(&self, path: &Path) -> Result<Option<SealRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT seal_id, file_id, original_path, original_name, object_path,
                        sensitivity, plaintext_blake3, size, sealed_ts, format, txn_id, unsealed_ts
                 FROM seal_objects
                 WHERE (original_path = ?1 OR object_path = ?1) AND unsealed_ts IS NULL
                 ORDER BY sealed_ts DESC LIMIT 1",
                [path.to_string_lossy()],
                row_from,
            )
            .optional()?)
    }

    pub fn list_sealed(&self) -> Result<Vec<SealRow>> {
        let mut st = self.conn.prepare(
            "SELECT seal_id, file_id, original_path, original_name, object_path,
                    sensitivity, plaintext_blake3, size, sealed_ts, format, txn_id, unsealed_ts
             FROM seal_objects WHERE unsealed_ts IS NULL ORDER BY sealed_ts DESC",
        )?;
        let rows = st.query_map([], row_from)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn mark_unsealed(&self, seal_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE seal_objects SET unsealed_ts = ?2 WHERE seal_id = ?1",
            params![seal_id, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn mark_resealed(&self, seal_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE seal_objects SET unsealed_ts = NULL WHERE seal_id = ?1",
            [seal_id],
        )?;
        Ok(())
    }

    pub fn vault_audit(
        &self,
        action: &str,
        seal_id: Option<&str>,
        path: Option<&Path>,
        detail: &str,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO vault_audit(ts, action, seal_id, path, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Utc::now().timestamp(),
                action,
                seal_id,
                path.map(|p| p.to_string_lossy().to_string()),
                detail
            ],
        );
    }

    pub fn vault_counts(&self) -> Result<(i64, i64)> {
        let sealed: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM seal_objects WHERE unsealed_ts IS NULL",
            [],
            |r| r.get(0),
        )?;
        let candidates: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files
             WHERE status = 'present' AND kind = 'file' AND sensitive = 1
               AND COALESCE(custody, 'open') <> 'sealed'",
            [],
            |r| r.get(0),
        )?;
        Ok((sealed, candidates))
    }

    /// After a successful Seal: custody + strip FTS body. Plaintext is gone.
    pub fn mark_sealed(
        &self,
        path: &Path,
        seal_id: &str,
        sensitivity: &str,
        object: &SealRow,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE files SET status = 'sealed', custody = 'sealed', seal_id = ?2,
                    sensitivity = ?3, sensitive = 1
             WHERE path = ?1 AND status IN ('present', 'trashed')",
            params![path.to_string_lossy(), seal_id, sensitivity],
        )?;
        let row: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT rowid, file_id, name, path FROM files WHERE path = ?1 AND status = 'sealed'",
                [path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((rowid, file_id, name, fpath)) = row {
            tx.execute("DELETE FROM file_text WHERE file_id = ?1", [&file_id])?;
            tx.execute("DELETE FROM files_fts WHERE rowid = ?1", [rowid])?;
            tx.execute(
                "INSERT INTO files_fts(rowid, name, path_tokens, extracted_text) VALUES (?1, ?2, ?3, '')",
                params![rowid, name, crate::inventory::path_tokens(Path::new(&fpath))],
            )?;
            tx.execute(
                "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                 VALUES (?1, ?2, 'sealed', ?3, NULL, 'txn')",
                params![file_id, now, path.to_string_lossy()],
            )?;
        }
        tx.commit()?;
        self.insert_seal_object(object)?;
        self.vault_audit(
            "seal",
            Some(seal_id),
            Some(path),
            &format!("sensitivity={sensitivity}"),
        );
        Ok(())
    }

    /// After a successful Unseal: file is present again; no FTS body until reclassify.
    pub fn mark_unsealed_file(&self, path: &Path, seal_id: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        self.conn.execute(
            "UPDATE files SET status = 'present', custody = 'open', classified_mtime = NULL
             WHERE (path = ?1 OR seal_id = ?2) AND status = 'sealed'",
            params![path.to_string_lossy(), seal_id],
        )?;
        self.conn.execute(
            "UPDATE files SET path = ?1, name = ?2, status = 'present', custody = 'open',
                    classified_mtime = NULL
             WHERE seal_id = ?3 AND status = 'sealed'",
            params![
                path.to_string_lossy(),
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                seal_id
            ],
        )?;
        self.mark_unsealed(seal_id)?;
        self.conn.execute(
            "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
             SELECT file_id, ?2, 'unsealed', NULL, path, 'txn' FROM files WHERE path = ?1",
            params![path.to_string_lossy(), now],
        )?;
        self.vault_audit("unseal", Some(seal_id), Some(path), "restored");
        Ok(())
    }

    /// Undo of Seal: file is present again, object row closed.
    pub fn mark_seal_undone(&self, path: &Path, seal_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET status = 'present', custody = 'open', seal_id = NULL,
                    classified_mtime = NULL
             WHERE path = ?1 OR seal_id = ?2",
            params![path.to_string_lossy(), seal_id],
        )?;
        self.mark_unsealed(seal_id)?;
        self.vault_audit("undo", Some(seal_id), Some(path), "seal undone");
        Ok(())
    }

    /// Undo of Unseal: back to sealed custody.
    pub fn mark_unseal_undone(&self, path: &Path, seal_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET status = 'sealed', custody = 'sealed', classified_mtime = NULL
             WHERE path = ?1 OR seal_id = ?2",
            params![path.to_string_lossy(), seal_id],
        )?;
        self.mark_resealed(seal_id)?;
        let row: Option<(i64, String, String)> = self
            .conn
            .query_row(
                "SELECT rowid, name, path FROM files WHERE seal_id = ?1",
                [seal_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((rowid, name, fpath)) = row {
            self.conn
                .execute("DELETE FROM files_fts WHERE rowid = ?1", [rowid])?;
            self.conn.execute(
                "INSERT INTO files_fts(rowid, name, path_tokens, extracted_text) VALUES (?1, ?2, ?3, '')",
                params![rowid, name, crate::inventory::path_tokens(Path::new(&fpath))],
            )?;
        }
        self.vault_audit("undo", Some(seal_id), Some(path), "unseal undone");
        Ok(())
    }
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<SealRow> {
    Ok(SealRow {
        seal_id: r.get(0)?,
        file_id: r.get(1)?,
        original_path: PathBuf::from(r.get::<_, String>(2)?),
        original_name: r.get(3)?,
        object_path: PathBuf::from(r.get::<_, String>(4)?),
        sensitivity: r.get(5)?,
        plaintext_blake3: r.get(6)?,
        size: r.get::<_, i64>(7)? as u64,
        sealed_ts: r.get(8)?,
        format: r.get(9)?,
        txn_id: r.get(10)?,
        unsealed_ts: r.get(11)?,
    })
}
