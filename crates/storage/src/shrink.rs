//! Queries behind the Shrink estimate.

use crate::Db;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// One present file with everything the estimator needs.
#[derive(Debug, Clone)]
pub struct ShrinkRow {
    pub root_id: i64,
    pub path: PathBuf,
    pub ext: Option<String>,
    pub size: u64,
    pub mtime: i64,
    pub sensitive: bool,
    pub category: String,
    /// The largest *cold* folder-backed project this file belongs to.
    pub cold_project: Option<i64>,
    /// In-place rewrite the file already carries (`apfs`), if any.
    pub rewrite: Option<String>,
}

/// A cold, folder-backed project. `folder` is the project directory (the
/// project `key` for `marker`/`folder` kinds), not the scan root.
#[derive(Debug, Clone)]
pub struct ColdProject {
    pub project_id: i64,
    pub name: String,
    pub folder: Option<String>,
    pub end_ts: i64,
}

/// Key of the cached estimate in `settings`.
pub const ESTIMATE_KEY: &str = "shrink.estimate";
/// Files per `compress_cold_text` suggestion (largest first).
pub const COMPRESS_MAX_FILES: usize = 500;
/// A compress suggestion needs at least this much to reclaim.
pub const COMPRESS_MIN_SAVING: u64 = 1 << 20;
/// Buckets whose measured ratio is above this are not proposed.
pub const COMPRESS_MAX_RATIO: f64 = 0.9;

/// One proposed batch of tier-1 rewrites.
#[derive(Debug, Clone)]
pub struct CompressBatch {
    pub root_id: i64,
    pub root: PathBuf,
    pub bucket: String,
    pub ratio: f64,
    /// (path, size) of the files in this batch, largest first.
    pub files: Vec<(PathBuf, u64)>,
    /// Every qualifying file in the bucket, not just this batch.
    pub total_files: u64,
    pub total_bytes: u64,
}

impl CompressBatch {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }
    pub fn saving(&self) -> u64 {
        ((self.bytes() as f64) * (1.0 - self.ratio)).round() as u64
    }
}

impl Db {
    /// Every present file, user classifications winning, with its cold
    /// project (status `cold`, kind `marker`/`folder`, whose key is its folder).
    /// Streams through `f` so 200k rows never sit in memory at once.
    pub fn shrink_rows(&self, mut f: impl FnMut(ShrinkRow)) -> Result<u64> {
        let mut st = self.conn.prepare(
            "SELECT f.path, f.ext, f.size, f.mtime, f.sensitive, f.rewrite, f.root_id,
                    COALESCE(
                      (SELECT category FROM classifications WHERE file_id = f.file_id AND source = 'user'),
                      (SELECT category FROM classifications WHERE file_id = f.file_id AND source <> 'user' ORDER BY ts DESC LIMIT 1),
                      'unclassified'),
                    (SELECT p.project_id FROM project_files pf JOIN projects p ON p.project_id = pf.project_id
                      WHERE pf.file_id = f.file_id AND p.status = 'cold' AND p.kind IN ('marker', 'folder')
                        AND p.key IS NOT NULL
                      ORDER BY p.bytes DESC LIMIT 1)
             FROM files f WHERE f.kind = 'file' AND f.status = 'present'",
        )?;
        let mut rows = st.query([])?;
        let mut n = 0u64;
        while let Some(r) = rows.next()? {
            let path: String = r.get(0)?;
            let size: i64 = r.get(2)?;
            let sensitive: i64 = r.get(4)?;
            f(ShrinkRow {
                path: PathBuf::from(path),
                ext: r.get(1)?,
                size: size.max(0) as u64,
                mtime: r.get(3)?,
                sensitive: sensitive != 0,
                rewrite: r.get(5)?,
                root_id: r.get(6)?,
                category: r.get(7)?,
                cold_project: r.get(8)?,
            });
            n += 1;
        }
        Ok(n)
    }

    /// Cold folder-backed projects (the tier 3 candidates), largest first.
    pub fn cold_projects(&self) -> Result<Vec<ColdProject>> {
        let mut st = self.conn.prepare(
            "SELECT project_id, COALESCE(name, suggested_name, key), key, end_ts
             FROM projects WHERE status = 'cold' AND kind IN ('marker', 'folder') AND key IS NOT NULL
             ORDER BY bytes DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ColdProject {
                project_id: r.get(0)?,
                name: r.get(1)?,
                folder: r.get(2)?,
                end_ts: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Tier-1 candidates grouped per (root, bucket), using the ratios of the
    /// cached estimate — no estimate, no proposals ("measure first").
    pub fn compress_batches(&self, now: i64) -> Result<Vec<CompressBatch>> {
        use filemind_core::shrink::estimate::{bucket_for, FileIn};
        let Some(est) = self.shrink_estimate()? else {
            return Ok(Vec::new());
        };
        let mut ratios: HashMap<String, f64> = HashMap::new();
        if let Some(tiers) = est["tiers"].as_array() {
            for t in tiers.iter().filter(|t| t["kind"] == "apfs") {
                for b in t["buckets"].as_array().into_iter().flatten() {
                    if let (Some(name), Some(r)) = (b["name"].as_str(), b["ratio"].as_f64()) {
                        if r <= COMPRESS_MAX_RATIO {
                            ratios.insert(name.to_string(), r);
                        }
                    }
                }
            }
        }
        if ratios.is_empty() {
            return Ok(Vec::new());
        }
        let roots: HashMap<i64, PathBuf> = self
            .list_roots()?
            .into_iter()
            .map(|r| (r.root_id, r.path))
            .collect();
        let mut groups: HashMap<(i64, String), Vec<(PathBuf, u64)>> = HashMap::new();
        self.shrink_rows(|r| {
            let f = FileIn {
                path: &r.path,
                ext: r.ext.as_deref(),
                size: r.size,
                mtime: r.mtime,
                sensitive: r.sensitive,
                category: &r.category,
                cold_project: r.cold_project,
                rewritten: r.rewrite.is_some(),
            };
            if let Some((1, bucket)) = bucket_for(&f, now) {
                if ratios.contains_key(&bucket) {
                    groups
                        .entry((r.root_id, bucket))
                        .or_default()
                        .push((r.path.clone(), r.size));
                }
            }
        })?;
        let mut out = Vec::new();
        for ((root_id, bucket), mut files) in groups {
            files.sort_by_key(|f| std::cmp::Reverse(f.1));
            let total_files = files.len() as u64;
            let total_bytes = files.iter().map(|f| f.1).sum();
            files.truncate(COMPRESS_MAX_FILES);
            let b = CompressBatch {
                root_id,
                root: roots.get(&root_id).cloned().unwrap_or_default(),
                ratio: ratios[&bucket],
                bucket,
                files,
                total_files,
                total_bytes,
            };
            if b.saving() >= COMPRESS_MIN_SAVING {
                out.push(b);
            }
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.saving()));
        Ok(out)
    }

    /// The cached estimate, if one was computed.
    pub fn shrink_estimate(&self) -> Result<Option<serde_json::Value>> {
        self.get_setting(ESTIMATE_KEY)
    }

    pub fn set_shrink_estimate(&self, v: &serde_json::Value) -> Result<()> {
        self.set_setting(ESTIMATE_KEY, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_carry_category_and_cold_project() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO roots(root_id, path, mode, added_ts) VALUES (1, '/r', 'observe', 1);
                 INSERT INTO files(file_id, root_id, path, name, ext, size, mtime, kind, first_seen, last_seen)
                   VALUES ('1:1', 1, '/r/old/a.csv', 'a.csv', 'csv', 5000, 100, 'file', 1, 1),
                          ('1:2', 1, '/r/new/b.rs', 'b.rs', 'rs', 6000, 200, 'file', 1, 1),
                          ('1:3', 1, '/r/d', 'd', NULL, 0, 200, 'dir', 1, 1);
                 INSERT INTO classifications(file_id, category, confidence, signals, source, ts)
                   VALUES ('1:1', 'data', 0.7, '[]', 'auto', 5), ('1:1', 'document', 1.0, '[]', 'user', 6);
                 INSERT INTO projects(project_id, key, kind, name, root_path, status, bytes, end_ts, activity_score, file_count, start_ts)
                   VALUES (9, '/r/old', 'folder', 'Old thing', '/r', 'cold', 5000, 100, 0.0, 1, 100);
                 INSERT INTO project_files(project_id, file_id, role, confidence) VALUES (9, '1:1', 'member', 1.0);",
            )
            .unwrap();
        let mut rows = Vec::new();
        let n = db.shrink_rows(|r| rows.push(r)).unwrap();
        assert_eq!(n, 2);
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(rows[0].category, "unclassified");
        assert_eq!(rows[0].cold_project, None);
        assert_eq!(rows[1].category, "document");
        assert_eq!(rows[1].cold_project, Some(9));
        let cold = db.cold_projects().unwrap();
        assert_eq!(cold.len(), 1);
        assert_eq!(cold[0].name, "Old thing");
        assert_eq!(cold[0].folder.as_deref(), Some("/r/old"));
        assert!(db.shrink_estimate().unwrap().is_none());
        db.set_shrink_estimate(&serde_json::json!({"saving_bytes": 1}))
            .unwrap();
        assert_eq!(db.shrink_estimate().unwrap().unwrap()["saving_bytes"], 1);
    }
}
