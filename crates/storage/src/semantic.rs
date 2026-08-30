//! Phase 7 persistence: text heads, embeddings, memory notes, AI audit,
//! and the candidate/filter queries hybrid search runs on.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::query::Parsed;
use filemind_core::vectors::VecIndex;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::path::PathBuf;

/// How much extracted text is kept per file for embedding.
pub const TEXT_HEAD_BYTES: usize = 2048;

/// A subject that needs (re)embedding.
#[derive(Debug, Clone)]
pub struct EmbedCandidate {
    pub subject: String,
    pub path: PathBuf,
    pub name: String,
    pub category: Option<String>,
    pub head: Option<String>,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Note {
    pub note_id: i64,
    pub subject_type: String,
    pub subject_id: String,
    pub text: String,
    pub source: String,
    pub ts: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub ts: i64,
    pub adapter: String,
    pub purpose: String,
    pub bytes_sent: u64,
    pub file_id: Option<String>,
    pub local: bool,
    pub ok: bool,
    pub latency_ms: Option<i64>,
}

/// One row of a search candidate list, enough to filter and display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileCard {
    pub file_id: String,
    pub path: PathBuf,
    pub name: String,
    pub ext: Option<String>,
    pub size: u64,
    pub mtime: i64,
    pub category: Option<String>,
    pub sensitive: bool,
    pub status: String,
}

impl Db {
    // ---- embeddings ----------------------------------------------------

    /// Files that have no vector under `model`, or changed since it was made.
    /// Sensitive files are never embedded (their text never leaves the file).
    pub fn embed_candidates(&self, model: &str, limit: usize) -> Result<Vec<EmbedCandidate>> {
        let mut st = self.conn.prepare(
            "SELECT f.file_id, f.path, f.name, f.size, f.mtime, c.category, t.head
             FROM files f
             LEFT JOIN embeddings e ON e.subject = f.file_id AND e.model = ?1
             LEFT JOIN classifications c ON c.file_id = f.file_id AND c.source = 'rule'
             LEFT JOIN file_text t ON t.file_id = f.file_id
             WHERE f.status = 'present' AND f.kind = 'file' AND f.sensitive = 0
               AND f.classified_mtime IS NOT NULL
               AND (e.subject IS NULL OR e.ts < f.mtime)
             ORDER BY f.mtime DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![model, limit as i64], |r| {
            Ok(EmbedCandidate {
                subject: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                name: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime: r.get(4)?,
                category: r.get(5)?,
                head: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            let c = r?;
            if filemind_core::scanner::in_noise_dir(&c.path) {
                continue;
            }
            out.push(c);
        }
        Ok(out)
    }

    pub fn count_embed_pending(&self, model: &str) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files f
             LEFT JOIN embeddings e ON e.subject = f.file_id AND e.model = ?1
             WHERE f.status = 'present' AND f.kind = 'file' AND f.sensitive = 0
               AND f.classified_mtime IS NOT NULL AND (e.subject IS NULL OR e.ts < f.mtime)",
            [model],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    pub fn put_text_head(&self, file_id: &str, mtime: i64, head: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_text(file_id, mtime, head) VALUES (?1, ?2, ?3)
             ON CONFLICT(file_id) DO UPDATE SET mtime = excluded.mtime, head = excluded.head",
            params![file_id, mtime, head],
        )?;
        Ok(())
    }

    /// Store vectors. `rows`: (subject, quantised vector, input hash).
    pub fn put_embeddings(&self, model: &str, rows: &[(String, Vec<i8>, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO embeddings(subject, model, dim, vec, input_hash, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(subject) DO UPDATE SET model = excluded.model, dim = excluded.dim,
                   vec = excluded.vec, input_hash = excluded.input_hash, ts = excluded.ts",
            )?;
            for (subject, vec, hash) in rows {
                let bytes: Vec<u8> = vec.iter().map(|&x| x as u8).collect();
                ins.execute(params![subject, model, vec.len() as i64, bytes, hash, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Marks an unchanged subject as fresh without re-embedding.
    pub fn touch_embedding(&self, subject: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE embeddings SET ts = ?2 WHERE subject = ?1",
            params![subject, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn embedding_input_hash(&self, subject: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT input_hash FROM embeddings WHERE subject = ?1",
                [subject],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Load every vector for `model` into memory. Subjects whose file is
    /// gone are skipped (and left for `prune_embeddings`).
    pub fn load_vec_index(&self, model: &str, dim: usize) -> Result<VecIndex> {
        let mut ix = VecIndex::new(dim);
        let mut st = self.conn.prepare(
            "SELECT e.subject, e.vec FROM embeddings e
             LEFT JOIN files f ON f.file_id = e.subject
             WHERE e.model = ?1 AND e.dim = ?2
               AND (e.subject LIKE 'note:%' OR f.status = 'present')",
        )?;
        let rows = st.query_map(params![model, dim as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for r in rows {
            let (subject, bytes) = r?;
            if bytes.len() != dim {
                continue;
            }
            let q: Vec<i8> = bytes.iter().map(|&b| b as i8).collect();
            ix.upsert(&subject, &q);
        }
        Ok(ix)
    }

    /// Remove vectors for files that are no longer present or notes that are gone.
    pub fn prune_embeddings(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM embeddings WHERE subject IN (
               SELECT e.subject FROM embeddings e LEFT JOIN files f ON f.file_id = e.subject
               WHERE e.subject NOT LIKE 'note:%' AND (f.file_id IS NULL OR f.status <> 'present'))
             OR subject IN (
               SELECT e.subject FROM embeddings e
               WHERE e.subject LIKE 'note:%'
                 AND CAST(substr(e.subject, 6) AS INTEGER) NOT IN (SELECT note_id FROM memory_notes))",
            [],
        )?;
        Ok(n)
    }

    pub fn embedding_stats(&self, model: &str) -> Result<(u64, u64)> {
        let have: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM embeddings WHERE model = ?1 AND subject NOT LIKE 'note:%'",
            [model],
            |r| r.get(0),
        )?;
        Ok((have as u64, self.count_embed_pending(model)?))
    }

    // ---- lexical search with filters ------------------------------------

    /// FTS5 hits as file ids in bm25 order, restricted to present files.
    pub fn search_lexical_ids(&self, fts_expr: &str, limit: usize) -> Result<Vec<String>> {
        if fts_expr.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare(
            "SELECT f.file_id FROM files_fts
             JOIN files f ON f.rowid = files_fts.rowid
             WHERE files_fts MATCH ?1 AND f.status = 'present'
             ORDER BY bm25(files_fts, 8.0, 3.0, 1.0) LIMIT ?2",
        )?;
        let rows = st.query_map(params![fts_expr, limit as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Files matching only the structured part of a query (no text), newest first.
    pub fn filter_only_ids(&self, p: &Parsed, limit: usize) -> Result<Vec<String>> {
        let mut sql = String::from(
            "SELECT f.file_id FROM files f WHERE f.status = 'present' AND f.kind = 'file'",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        push_filters(p, &mut sql, &mut args);
        sql.push_str(" ORDER BY f.mtime DESC LIMIT ?");
        args.push((limit as i64).into());
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Cards for a set of ids, in the order given, dropping ids that fail the
    /// structured filters of `p`.
    pub fn cards_filtered(&self, ids: &[String], p: &Parsed) -> Result<Vec<FileCard>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(ids.len());
        let mut sql = String::from(
            "SELECT f.file_id, f.path, f.name, f.ext, f.size, f.mtime, c.category, f.sensitive, f.status
             FROM files f LEFT JOIN classifications c ON c.file_id = f.file_id AND c.source = 'rule'
             WHERE f.file_id = ? AND f.status = 'present'",
        );
        let mut extra: Vec<rusqlite::types::Value> = Vec::new();
        push_filters(p, &mut sql, &mut extra);
        let mut st = self.conn.prepare(&sql)?;
        for id in ids {
            let mut args: Vec<rusqlite::types::Value> = vec![id.clone().into()];
            args.extend(extra.iter().cloned());
            let row = st
                .query_row(rusqlite::params_from_iter(args), |r| {
                    Ok(FileCard {
                        file_id: r.get(0)?,
                        path: PathBuf::from(r.get::<_, String>(1)?),
                        name: r.get(2)?,
                        ext: r.get(3)?,
                        size: r.get::<_, i64>(4)? as u64,
                        mtime: r.get(5)?,
                        category: r.get(6)?,
                        sensitive: r.get::<_, i64>(7)? != 0,
                        status: r.get(8)?,
                    })
                })
                .optional()?;
            if let Some(c) = row {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Ids of files in projects whose name matches `hint` (case-insensitive substring).
    pub fn project_member_ids(&self, hint: &str) -> Result<HashSet<String>> {
        let mut st = self.conn.prepare(
            "SELECT pf.file_id FROM project_files pf JOIN projects p ON p.project_id = pf.project_id
             WHERE p.status <> 'gone' AND (lower(COALESCE(p.name, '')) LIKE ?1 OR lower(COALESCE(p.suggested_name, '')) LIKE ?1
                    OR lower(COALESCE(p.root_path, '')) LIKE ?1)",
        )?;
        let pat = format!("%{}%", hint.to_lowercase());
        let rows = st.query_map([pat], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn duplicate_member_ids(&self) -> Result<HashSet<String>> {
        let mut st = self.conn.prepare(
            "SELECT f.file_id FROM files f JOIN duplicate_groups g ON g.blob_id = f.blob_id
             WHERE f.status = 'present'",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- memory notes -----------------------------------------------------

    pub fn add_note(
        &self,
        subject_type: &str,
        subject_id: &str,
        text: &str,
        source: &str,
    ) -> Result<Note> {
        let ts = Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memory_notes(subject_type, subject_id, text, source, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![subject_type, subject_id, text, source, ts],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO notes_fts(rowid, text) VALUES (?1, ?2)",
            params![id, text],
        )?;
        tx.commit()?;
        Ok(Note {
            note_id: id,
            subject_type: subject_type.into(),
            subject_id: subject_id.into(),
            text: text.into(),
            source: source.into(),
            ts,
        })
    }

    pub fn remove_note(&self, note_id: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", [note_id])?;
        tx.execute(
            "DELETE FROM embeddings WHERE subject = ?1",
            [format!("note:{note_id}")],
        )?;
        let n = tx.execute("DELETE FROM memory_notes WHERE note_id = ?1", [note_id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn note(&self, note_id: i64) -> Result<Option<Note>> {
        Ok(self
            .conn
            .query_row(
                "SELECT note_id, subject_type, subject_id, text, source, ts FROM memory_notes WHERE note_id = ?1",
                [note_id],
                note_row,
            )
            .optional()?)
    }

    /// Notes for one subject (a path or a project key), newest first; or all
    /// notes when `subject_id` is `None`.
    pub fn list_notes(&self, subject_id: Option<&str>, limit: usize) -> Result<Vec<Note>> {
        let mut out = Vec::new();
        match subject_id {
            Some(s) => {
                let mut st = self.conn.prepare(
                    "SELECT note_id, subject_type, subject_id, text, source, ts FROM memory_notes
                     WHERE subject_id = ?1 ORDER BY ts DESC LIMIT ?2",
                )?;
                for r in st.query_map(params![s, limit as i64], note_row)? {
                    out.push(r?);
                }
            }
            None => {
                let mut st = self.conn.prepare(
                    "SELECT note_id, subject_type, subject_id, text, source, ts FROM memory_notes
                     ORDER BY ts DESC LIMIT ?1",
                )?;
                for r in st.query_map([limit as i64], note_row)? {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    /// Notes with no vector yet (or edited since).
    pub fn notes_to_embed(&self, model: &str) -> Result<Vec<Note>> {
        let mut st = self.conn.prepare(
            "SELECT n.note_id, n.subject_type, n.subject_id, n.text, n.source, n.ts FROM memory_notes n
             LEFT JOIN embeddings e ON e.subject = 'note:' || n.note_id AND e.model = ?1
             WHERE e.subject IS NULL OR e.ts < n.ts",
        )?;
        let rows = st.query_map([model], note_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn search_notes_lexical(&self, fts_expr: &str, limit: usize) -> Result<Vec<i64>> {
        if fts_expr.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare(
            "SELECT rowid FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY bm25(notes_fts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![fts_expr, limit as i64], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- AI audit ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn log_ai(
        &self,
        adapter: &str,
        purpose: &str,
        bytes_sent: u64,
        file_id: Option<&str>,
        snippet_hash: &str,
        local: bool,
        ok: bool,
        latency_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ai_audit(ts, adapter, purpose, bytes_sent, file_id, snippet_hash, local, ok, latency_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Utc::now().timestamp(),
                adapter,
                purpose,
                bytes_sent as i64,
                file_id,
                snippet_hash,
                local as i64,
                ok as i64,
                latency_ms
            ],
        )?;
        Ok(())
    }

    pub fn list_ai_audit(&self, limit: usize) -> Result<Vec<AuditRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, ts, adapter, purpose, bytes_sent, file_id, local, ok, latency_ms
             FROM ai_audit ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                adapter: r.get(2)?,
                purpose: r.get(3)?,
                bytes_sent: r.get::<_, i64>(4)? as u64,
                file_id: r.get(5)?,
                local: r.get::<_, i64>(6)? != 0,
                ok: r.get::<_, i64>(7)? != 0,
                latency_ms: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Text head for a file, if extraction kept one.
    pub fn text_head(&self, file_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT head FROM file_text WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn file_id_of_path(&self, path: &std::path::Path) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT file_id FROM files WHERE path = ?1 AND status = 'present'",
                [path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?)
    }
}

fn note_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        note_id: r.get(0)?,
        subject_type: r.get(1)?,
        subject_id: r.get(2)?,
        text: r.get(3)?,
        source: r.get(4)?,
        ts: r.get(5)?,
    })
}

/// Append `AND …` clauses for the structured parts of a parsed query.
fn push_filters(p: &Parsed, sql: &mut String, args: &mut Vec<rusqlite::types::Value>) {
    if let Some(a) = p.after {
        sql.push_str(" AND f.mtime >= ?");
        args.push(a.into());
    }
    if let Some(b) = p.before {
        sql.push_str(" AND f.mtime < ?");
        args.push(b.into());
    }
    if let Some(s) = p.min_size {
        sql.push_str(" AND f.size >= ?");
        args.push((s as i64).into());
    }
    if let Some(s) = p.max_size {
        sql.push_str(" AND f.size <= ?");
        args.push((s as i64).into());
    }
    if p.sensitive_only {
        sql.push_str(" AND f.sensitive = 1");
    }
    if let Some(folder) = &p.folder {
        sql.push_str(" AND lower(f.path) LIKE ?");
        args.push(format!("%/{}/%", folder.to_lowercase()).into());
    }
    // extension OR category: a kind word means "files like this", and the
    // classifier may know better than the extension list.
    if !p.exts.is_empty() || !p.categories.is_empty() {
        let mut parts = Vec::new();
        if !p.exts.is_empty() {
            let marks = vec!["?"; p.exts.len()].join(",");
            parts.push(format!("lower(f.ext) IN ({marks})"));
            for e in &p.exts {
                args.push(e.to_lowercase().into());
            }
        }
        if !p.categories.is_empty() {
            let marks = vec!["?"; p.categories.len()].join(",");
            parts.push(format!(
                "f.file_id IN (SELECT file_id FROM classifications WHERE source = 'rule' AND category IN ({marks}))"
            ));
            for c in &p.categories {
                args.push(c.as_str().to_string().into());
            }
        }
        sql.push_str(&format!(" AND ({})", parts.join(" OR ")));
    }
}
