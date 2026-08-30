//! Persistence for classifications, user rules and extracted text.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::classify::UserRule;
use filemind_core::model::{Category, Classification, ClassificationSource};
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

/// (file_id, mtime, classification, sensitive kind, extracted text)
pub type ClassifiedRow = (String, i64, Classification, Option<String>, Option<String>);

/// A file that still needs classifying (never done, or edited since).
#[derive(Debug, Clone)]
pub struct Pending {
    pub file_id: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
}

impl Db {
    pub fn classify_pending(&self, limit: usize) -> Result<Vec<Pending>> {
        let mut st = self.conn.prepare(
            "SELECT file_id, path, size, mtime FROM files
             WHERE kind = 'file' AND status = 'present'
               AND (classified_mtime IS NULL OR classified_mtime <> mtime)
             ORDER BY size ASC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(Pending {
                file_id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                size: r.get::<_, i64>(2)? as u64,
                mtime: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn count_classify_pending(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE kind = 'file' AND status = 'present'
             AND (classified_mtime IS NULL OR classified_mtime <> mtime)",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Write one classification result plus (optionally) extracted text into the
    /// FTS mirror. `text` is ignored for sensitive files.
    pub fn record_classification(
        &self,
        file_id: &str,
        mtime: i64,
        c: &Classification,
        sensitive: Option<&str>,
        text: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        write_classification(&tx, file_id, mtime, c, sensitive, text)?;
        tx.commit()?;
        Ok(())
    }

    /// Batch form of [`Db::record_classification`].
    pub fn record_classifications(&self, rows: &[ClassifiedRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (id, mtime, c, sens, text) in rows {
            write_classification(&tx, id, *mtime, c, sens.as_deref(), text.as_deref())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn classification_of(
        &self,
        path: &Path,
    ) -> Result<Option<(Classification, Option<String>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT c.category, c.confidence, c.signals, c.source, f.sensitive_kind
                 FROM files f JOIN classifications c ON c.file_id = f.file_id
                 WHERE f.path = ?1 AND f.status = 'present'
                 ORDER BY CASE c.source WHEN 'user' THEN 0 ELSE 1 END LIMIT 1",
                [path.to_string_lossy()],
                |r| {
                    let cat: String = r.get(0)?;
                    let src: String = r.get(3)?;
                    Ok((
                        Classification {
                            category: Category::parse(&cat).unwrap_or(Category::Other),
                            confidence: r.get::<_, f64>(1)? as f32,
                            signals: serde_json::from_str(&r.get::<_, String>(2)?)
                                .unwrap_or_default(),
                            source: match src.as_str() {
                                "user" => ClassificationSource::User,
                                "ml" => ClassificationSource::Ml,
                                "llm" => ClassificationSource::Llm,
                                _ => ClassificationSource::Rule,
                            },
                        },
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?)
    }

    /// Category → (files, bytes) over present files, user classifications winning.
    pub fn category_counts(&self) -> Result<Vec<(String, i64, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT category, COUNT(*), COALESCE(SUM(size),0) FROM (
               SELECT f.size,
                      COALESCE(
                        (SELECT category FROM classifications WHERE file_id = f.file_id AND source = 'user'),
                        (SELECT category FROM classifications WHERE file_id = f.file_id AND source <> 'user' ORDER BY ts DESC LIMIT 1),
                        'unclassified') AS category
               FROM files f WHERE f.kind = 'file' AND f.status = 'present')
             GROUP BY category ORDER BY 2 DESC",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn sensitive_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE status = 'present' AND sensitive = 1",
            [],
            |r| r.get(0),
        )?)
    }

    // ----- user rules -----------------------------------------------------

    pub fn add_rule(&self, rule: &UserRule) -> Result<i64> {
        let (kind, pattern, cat) = match rule {
            UserRule::PathPrefix { prefix, category } => ("path_prefix", prefix.clone(), *category),
            UserRule::NameContains { token, category } => {
                ("name_contains", token.clone(), *category)
            }
            UserRule::Ext { ext, category } => ("ext", ext.clone(), *category),
        };
        self.conn.execute(
            "INSERT INTO user_rules(kind, pattern, category, created_ts) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kind, pattern) DO UPDATE SET category = excluded.category, created_ts = excluded.created_ts",
            params![kind, pattern, cat.as_str(), Utc::now().timestamp()],
        )?;
        // Files the rule now covers must be reclassified.
        match kind {
            "path_prefix" => self.conn.execute(
                "UPDATE files SET classified_mtime = NULL WHERE path = ?1 OR path LIKE ?2",
                params![pattern, format!("{}/%", pattern.trim_end_matches('/'))],
            )?,
            "name_contains" => self.conn.execute(
                "UPDATE files SET classified_mtime = NULL WHERE lower(name) LIKE ?1",
                [format!("%{}%", pattern.to_lowercase())],
            )?,
            _ => self.conn.execute(
                "UPDATE files SET classified_mtime = NULL WHERE ext = ?1",
                [&pattern],
            )?,
        };
        Ok(self.conn.query_row(
            "SELECT rule_id FROM user_rules WHERE kind = ?1 AND pattern = ?2",
            params![kind, pattern],
            |r| r.get(0),
        )?)
    }

    pub fn list_rules(&self) -> Result<Vec<(i64, UserRule)>> {
        let mut st = self
            .conn
            .prepare("SELECT rule_id, kind, pattern, category FROM user_rules ORDER BY rule_id")?;
        let rows = st.query_map([], |r| {
            let kind: String = r.get(1)?;
            let pattern: String = r.get(2)?;
            let cat: String = r.get(3)?;
            let category = Category::parse(&cat).unwrap_or(Category::Other);
            let rule = match kind.as_str() {
                "path_prefix" => UserRule::PathPrefix {
                    prefix: pattern,
                    category,
                },
                "name_contains" => UserRule::NameContains {
                    token: pattern,
                    category,
                },
                _ => UserRule::Ext {
                    ext: pattern,
                    category,
                },
            };
            Ok((r.get::<_, i64>(0)?, rule))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_rule(&self, rule_id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM user_rules WHERE rule_id = ?1", [rule_id])?
            == 1)
    }

    /// A direct user correction for one file (no rule).
    pub fn set_user_category(&self, path: &Path, category: Category) -> Result<bool> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT file_id FROM files WHERE path = ?1 AND status = 'present'",
                [path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { return Ok(false) };
        self.conn.execute(
            "INSERT INTO classifications(file_id, category, confidence, signals, source, ts)
             VALUES (?1, ?2, 1.0, '[\"set by you\"]', 'user', ?3)
             ON CONFLICT(file_id, source) DO UPDATE SET category = excluded.category, ts = excluded.ts",
            params![id, category.as_str(), Utc::now().timestamp()],
        )?;
        Ok(true)
    }
}

fn write_classification(
    tx: &rusqlite::Transaction<'_>,
    file_id: &str,
    mtime: i64,
    c: &Classification,
    sensitive: Option<&str>,
    text: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO classifications(file_id, category, confidence, signals, source, ts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(file_id, source) DO UPDATE SET category = excluded.category,
           confidence = excluded.confidence, signals = excluded.signals, ts = excluded.ts",
        params![
            file_id,
            c.category.as_str(),
            c.confidence as f64,
            serde_json::to_string(&c.signals)?,
            c.source.as_str(),
            Utc::now().timestamp()
        ],
    )?;
    tx.execute(
        "UPDATE files SET classified_mtime = ?2, sensitive = ?3, sensitive_kind = ?4 WHERE file_id = ?1",
        params![file_id, mtime, sensitive.is_some() as i64, sensitive],
    )?;
    if sensitive.is_none() {
        if let Some(t) = text {
            let row: Option<(i64, String, String)> = tx
                .query_row(
                    "SELECT rowid, name, path FROM files WHERE file_id = ?1",
                    [file_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((rowid, name, path)) = row {
                tx.execute(
                    "INSERT INTO file_text(file_id, mtime, head) VALUES (?1, ?2, ?3)
                     ON CONFLICT(file_id) DO UPDATE SET mtime = excluded.mtime, head = excluded.head",
                    params![file_id, mtime, filemind_core::classify::head(t, crate::semantic::TEXT_HEAD_BYTES)],
                )?;
                tx.execute("DELETE FROM files_fts WHERE rowid = ?1", [rowid])?;
                tx.execute(
                    "INSERT INTO files_fts(rowid, name, path_tokens, extracted_text) VALUES (?1, ?2, ?3, ?4)",
                    params![rowid, name, crate::inventory::path_tokens(Path::new(&path)), t],
                )?;
                tx.execute(
                    "UPDATE blobs SET text_extracted = 1 WHERE blob_id = (SELECT blob_id FROM files WHERE file_id = ?1)",
                    [file_id],
                )?;
            }
        }
    }
    Ok(())
}
