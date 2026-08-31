//! Database side of cold-project archives: which chunks exist where, which
//! members make up which archive, and the inventory flips that keep
//! archived files searchable.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::shrink::archive as fmt;
use rusqlite::{params, OptionalExtension};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveRow {
    pub archive_id: String,
    pub project_id: Option<i64>,
    pub name: String,
    pub folder: String,
    pub pack_path: String,
    pub created_ts: i64,
    pub bytes_raw: u64,
    pub bytes_stored: u64,
    pub members: usize,
    pub state: String,
    pub txn_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MemberRow {
    pub rel: String,
    pub kind: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
}

/// Where one chunk physically lives.
#[derive(Debug, Clone)]
pub struct ChunkLoc {
    pub archive_id: String,
    pub off: u64,
    pub clen: usize,
    pub ulen: usize,
    pub dict_id: String,
}

impl Db {
    pub fn archive_new_id(&self) -> String {
        format!(
            "arc_{}_{}",
            Utc::now().format("%Y%m%dT%H%M%S"),
            &blake3::hash(&std::process::id().to_le_bytes()).to_hex()[..4]
        )
    }

    pub fn archive_insert(&self, a: &ArchiveRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archives(archive_id, project_id, name, folder, pack_path, created_ts,
                                  bytes_raw, bytes_stored, members, state, txn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                a.archive_id,
                a.project_id,
                a.name,
                a.folder,
                a.pack_path,
                a.created_ts,
                a.bytes_raw as i64,
                a.bytes_stored as i64,
                a.members as i64,
                a.state,
                a.txn_id
            ],
        )?;
        Ok(())
    }

    pub fn archive_set_state(&self, archive_id: &str, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE archives SET state = ?2 WHERE archive_id = ?1",
            params![archive_id, state],
        )?;
        Ok(())
    }

    pub fn archive_set_txn(&self, archive_id: &str, txn_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE archives SET txn_id = ?2 WHERE archive_id = ?1",
            params![archive_id, txn_id],
        )?;
        Ok(())
    }

    pub fn archive_add_member(&self, archive_id: &str, m: &MemberRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_members(archive_id, rel, kind, size, mtime, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![archive_id, m.rel, m.kind, m.size as i64, m.mtime, m.hash],
        )?;
        Ok(())
    }

    pub fn archive_add_member_chunk(
        &self,
        archive_id: &str,
        rel: &str,
        seq: usize,
        hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_member_chunks(archive_id, rel, seq, hash) VALUES (?1, ?2, ?3, ?4)",
            params![archive_id, rel, seq as i64, hash],
        )?;
        Ok(())
    }

    pub fn archive_add_chunk(&self, hash: &str, loc: &ChunkLoc) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_chunks(hash, archive_id, off, clen, ulen, dict_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                hash,
                loc.archive_id,
                loc.off as i64,
                loc.clen as i64,
                loc.ulen as i64,
                loc.dict_id
            ],
        )?;
        Ok(())
    }

    pub fn archive_chunk(&self, hash: &str) -> Result<Option<ChunkLoc>> {
        Ok(self
            .conn
            .query_row(
                "SELECT archive_id, off, clen, ulen, dict_id FROM archive_chunks WHERE hash = ?1",
                [hash],
                |r| {
                    Ok(ChunkLoc {
                        archive_id: r.get(0)?,
                        off: r.get::<_, i64>(1)? as u64,
                        clen: r.get::<_, i64>(2)? as usize,
                        ulen: r.get::<_, i64>(3)? as usize,
                        dict_id: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn archive_add_dict(
        &self,
        dict_id: &str,
        archive_id: &str,
        category: &str,
        data: &[u8],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_dicts(dict_id, archive_id, category, data) VALUES (?1, ?2, ?3, ?4)",
            params![dict_id, archive_id, category, data],
        )?;
        Ok(())
    }

    pub fn archive_dict(&self, dict_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT data FROM archive_dicts WHERE dict_id = ?1",
                [dict_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn archive(&self, archive_id: &str) -> Result<Option<ArchiveRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT archive_id, project_id, name, folder, pack_path, created_ts,
                        bytes_raw, bytes_stored, members, state, txn_id
                 FROM archives WHERE archive_id = ?1",
                [archive_id],
                row_archive,
            )
            .optional()?)
    }

    pub fn archives(&self) -> Result<Vec<ArchiveRow>> {
        let mut st = self.conn.prepare(
            "SELECT archive_id, project_id, name, folder, pack_path, created_ts,
                    bytes_raw, bytes_stored, members, state, txn_id
             FROM archives ORDER BY created_ts DESC",
        )?;
        let rows = st.query_map([], row_archive)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn archive_members(&self, archive_id: &str) -> Result<Vec<MemberRow>> {
        let mut st = self.conn.prepare(
            "SELECT rel, kind, size, mtime, hash FROM archive_members
             WHERE archive_id = ?1 ORDER BY rel",
        )?;
        let rows = st.query_map([archive_id], |r| {
            Ok(MemberRow {
                rel: r.get(0)?,
                kind: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                mtime: r.get(3)?,
                hash: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn archive_member(&self, archive_id: &str, rel: &str) -> Result<Option<MemberRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT rel, kind, size, mtime, hash FROM archive_members
                 WHERE archive_id = ?1 AND rel = ?2",
                params![archive_id, rel],
                |r| {
                    Ok(MemberRow {
                        rel: r.get(0)?,
                        kind: r.get(1)?,
                        size: r.get::<_, i64>(2)? as u64,
                        mtime: r.get(3)?,
                        hash: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Chunk hashes of one member, in order.
    pub fn archive_member_chunks(&self, archive_id: &str, rel: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT hash FROM archive_member_chunks
             WHERE archive_id = ?1 AND rel = ?2 ORDER BY seq",
        )?;
        let rows = st.query_map(params![archive_id, rel], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// After the original folder is trashed: flip its inventory rows from
    /// `trashed` to `archived` and point them into the pack, so search
    /// still finds them.
    pub fn mark_archived(&self, archive_id: &str, folder: &Path) -> Result<usize> {
        let folder = folder.to_string_lossy();
        let prefix = format!("{folder}/");
        let mut st = self.conn.prepare(
            "SELECT file_id, path FROM files WHERE (path = ?1 OR path LIKE ?2) AND status = 'trashed'",
        )?;
        let rows: Vec<(String, String)> = st
            .query_map(params![folder, format!("{prefix}%")], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut n = 0;
        for (file_id, path) in rows {
            let rel = path.strip_prefix(prefix.as_str()).unwrap_or(&path);
            self.conn.execute(
                "UPDATE files SET status = 'archived', location = ?2 WHERE file_id = ?1",
                params![file_id, fmt::location(archive_id, rel)],
            )?;
            n += 1;
        }
        Ok(n)
    }

    /// After a restore (or an undo of the archive transaction): archived
    /// rows for this archive become plain present rows again.
    pub fn mark_unarchived(&self, archive_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE files SET status = 'present', location = NULL
             WHERE location LIKE ?1 AND status = 'archived'",
            [format!("archive:{archive_id}#%")],
        )?)
    }
}

fn row_archive(r: &rusqlite::Row) -> rusqlite::Result<ArchiveRow> {
    Ok(ArchiveRow {
        archive_id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        folder: r.get(3)?,
        pack_path: r.get(4)?,
        created_ts: r.get(5)?,
        bytes_raw: r.get::<_, i64>(6)? as u64,
        bytes_stored: r.get::<_, i64>(7)? as u64,
        members: r.get::<_, i64>(8)? as usize,
        state: r.get(9)?,
        txn_id: r.get(10)?,
    })
}
