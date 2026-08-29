//! Project persistence. Detection is pure (`filemind_core::projects`); this
//! module loads the inventory, runs it per root, and writes the result while
//! keeping user edits (name, status) keyed by the project's stable key.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::projects::{self, FileIn, Project, ProjectKind};
use rusqlite::{params, OptionalExtension};
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectRow {
    pub project_id: i64,
    pub key: String,
    pub kind: String,
    pub name: Option<String>,
    pub suggested_name: String,
    pub root_path: Option<String>,
    pub file_count: i64,
    pub bytes: i64,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub activity_score: f64,
    pub status: String,
}

impl ProjectRow {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.suggested_name)
    }
}

fn status_for(activity: f32, end_ts: i64, now: i64) -> &'static str {
    if activity >= 0.25 || now - end_ts <= projects::ACTIVE_DAYS * 86_400 {
        "active"
    } else if now - end_ts <= projects::DORMANT_DAYS * 86_400 {
        "dormant"
    } else {
        "cold"
    }
}

impl Db {
    /// Re-run detection for every root and persist. Returns the projects
    /// (most active first) with their assigned ids.
    pub fn rebuild_projects(&self) -> Result<Vec<ProjectRow>> {
        let now = Utc::now().timestamp();
        let mut detected: Vec<Project> = Vec::new();
        for root in self.list_roots()? {
            let mut st = self.conn.prepare(
                "SELECT file_id, path, mtime, size FROM files WHERE root_id = ?1 AND kind = 'file' AND status = 'present'",
            )?;
            let files: Vec<FileIn> = st
                .query_map([root.root_id], |r| {
                    Ok(FileIn {
                        file_id: r.get(0)?,
                        path: PathBuf::from(r.get::<_, String>(1)?),
                        mtime: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            detected.extend(projects::detect(&root.path, &files, now));
        }

        let tx = self.conn.unchecked_transaction()?;
        // Mark everything stale; detected keys get refreshed below. Projects
        // the user renamed or pinned keep their row even if not re-detected.
        tx.execute(
            "UPDATE projects SET status = 'gone', updated_ts = ?1 WHERE key IS NOT NULL",
            [now],
        )?;
        tx.execute("DELETE FROM project_files", [])?;
        {
            let mut up = tx.prepare_cached(
                "INSERT INTO projects(key, kind, suggested_name, root_path, file_count, bytes, start_ts, end_ts, activity_score, status, updated_ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(key) DO UPDATE SET kind = excluded.kind, suggested_name = excluded.suggested_name,
                   root_path = excluded.root_path, file_count = excluded.file_count, bytes = excluded.bytes,
                   start_ts = excluded.start_ts, end_ts = excluded.end_ts, activity_score = excluded.activity_score,
                   status = excluded.status, updated_ts = excluded.updated_ts",
            )?;
            let mut id_of = tx.prepare_cached("SELECT project_id FROM projects WHERE key = ?1")?;
            let mut member = tx.prepare_cached(
                "INSERT OR IGNORE INTO project_files(project_id, file_id, role, confidence) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in &detected {
                let status = status_for(p.activity_score, p.end_ts, now);
                up.execute(params![
                    p.key,
                    p.kind.as_str(),
                    p.suggested_name,
                    p.root_path.to_string_lossy(),
                    p.files.len() as i64,
                    p.bytes as i64,
                    p.start_ts,
                    p.end_ts,
                    p.activity_score as f64,
                    status,
                    now
                ])?;
                let pid: i64 = id_of.query_row([&p.key], |r| r.get(0))?;
                let conf = match p.kind {
                    ProjectKind::Marker => 0.95,
                    ProjectKind::Folder => 0.85,
                    ProjectKind::Topic => 0.6,
                    ProjectKind::Session => 0.5,
                };
                for fid in &p.files {
                    member.execute(params![pid, fid, "member", conf])?;
                }
            }
        }
        // Drop gone projects the user never touched.
        tx.execute(
            "DELETE FROM projects WHERE status = 'gone' AND name IS NULL",
            [],
        )?;
        tx.commit()?;
        self.list_projects(usize::MAX)
    }

    pub fn list_projects(&self, limit: usize) -> Result<Vec<ProjectRow>> {
        let mut st = self.conn.prepare(
            "SELECT project_id, key, kind, name, suggested_name, root_path, file_count, bytes, start_ts, end_ts, activity_score, status
             FROM projects WHERE key IS NOT NULL AND status <> 'gone'
             ORDER BY activity_score DESC, file_count DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit.min(i64::MAX as usize) as i64], |r| {
            Ok(ProjectRow {
                project_id: r.get(0)?,
                key: r.get(1)?,
                kind: r.get(2)?,
                name: r.get(3)?,
                suggested_name: r.get(4)?,
                root_path: r.get(5)?,
                file_count: r.get(6)?,
                bytes: r.get(7)?,
                start_ts: r.get(8)?,
                end_ts: r.get(9)?,
                activity_score: r.get(10)?,
                status: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn project(&self, project_id: i64) -> Result<Option<ProjectRow>> {
        Ok(self
            .list_projects(usize::MAX)?
            .into_iter()
            .find(|p| p.project_id == project_id))
    }

    /// Member paths, newest first.
    pub fn project_files(&self, project_id: i64, limit: usize) -> Result<Vec<(PathBuf, i64, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT f.path, f.mtime, f.size FROM project_files pf JOIN files f ON f.file_id = pf.file_id
             WHERE pf.project_id = ?1 AND f.status = 'present' ORDER BY f.mtime DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![project_id, limit as i64], |r| {
            Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn project_of_path(&self, path: &std::path::Path) -> Result<Option<ProjectRow>> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT pf.project_id FROM project_files pf JOIN files f ON f.file_id = pf.file_id
                 WHERE f.path = ?1 AND f.status = 'present' LIMIT 1",
                [path.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.project(id),
            None => Ok(None),
        }
    }

    pub fn rename_project(&self, project_id: i64, name: Option<&str>) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE projects SET name = ?2, updated_ts = ?3 WHERE project_id = ?1",
            params![project_id, name, Utc::now().timestamp()],
        )? == 1)
    }

    /// Categories inside a project: (category, files).
    pub fn project_categories(&self, project_id: i64) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT COALESCE(
                 (SELECT category FROM classifications WHERE file_id = f.file_id AND source = 'user'),
                 (SELECT category FROM classifications WHERE file_id = f.file_id ORDER BY ts DESC LIMIT 1),
                 'unclassified') AS c, COUNT(*)
             FROM project_files pf JOIN files f ON f.file_id = pf.file_id
             WHERE pf.project_id = ?1 AND f.status = 'present' GROUP BY c ORDER BY 2 DESC",
        )?;
        let rows = st.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}
