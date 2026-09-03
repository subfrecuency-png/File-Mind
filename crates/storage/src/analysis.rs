//! Analysis over the inventory: duplicate groups, version chains, health
//! inputs and Observe-mode suggestions. Everything here is derived data that
//! can be rebuilt from `files` + `blobs` at any time.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::health::{self, Health, HealthInputs};
use filemind_core::versions::{
    looks_unnamed, natural_key, normalize_stem, normalize_stem_marker, Marker,
};
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const STALE_DAYS: i64 = 90;
/// An archive suggestion needs at least this much on-disk saving.
const ARCHIVE_MIN_SAVING: u64 = 10 << 20;
/// A folder needs this many files before an identical twin is worth a suggestion.
const FOLDER_DUP_MIN_FILES: u64 = 5;

#[derive(Debug, Clone)]
pub struct DupGroup {
    pub group_id: i64,
    pub blob_id: i64,
    pub size: u64,
    pub keeper: PathBuf,
    pub copies: Vec<PathBuf>,
}

/// (file_id, path, mtime, marker)
type Member = (String, PathBuf, i64, Marker);

#[derive(Debug, Clone)]
pub struct VersionChain {
    pub chain_id: i64,
    pub canonical: PathBuf,
    pub older: Vec<PathBuf>,
}

/// Prefer keeping the copy that lives in a deliberate place with a deliberate name.
fn keeper_rank(path: &Path) -> (u8, u8, usize) {
    let p = path.to_string_lossy().to_lowercase();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let in_downloads = p.contains("/downloads/") || p.contains("\\downloads\\");
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let (_, marked) = normalize_stem(&stem);
    (
        in_downloads as u8,                     // prefer not-Downloads
        (marked || looks_unnamed(&name)) as u8, // prefer clean names
        p.len(),                                // prefer shorter paths
    )
}

impl Db {
    /// Rebuild `duplicate_groups` from blobs shared by ≥ 2 present files.
    pub fn rebuild_duplicates(&self) -> Result<Vec<DupGroup>> {
        let mut st = self.conn.prepare(
            "SELECT f.blob_id, b.size, f.file_id, f.path FROM files f
             JOIN blobs b ON b.blob_id = f.blob_id
             WHERE f.status = 'present' AND f.kind = 'file' AND f.blob_id IS NOT NULL
               AND b.size > 0 AND b.blake3 NOT LIKE 'err:%'
               AND f.blob_id IN (SELECT blob_id FROM files WHERE status = 'present' AND kind = 'file'
                                 GROUP BY blob_id HAVING COUNT(*) > 1)
             ORDER BY f.blob_id",
        )?;
        let rows: Vec<(i64, i64, String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut by_blob: HashMap<i64, (u64, Vec<(String, PathBuf)>)> = HashMap::new();
        for (blob, size, id, path) in rows {
            by_blob
                .entry(blob)
                .or_insert((size as u64, Vec::new()))
                .1
                .push((id, PathBuf::from(path)));
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM duplicate_groups", [])?;
        let mut out = Vec::new();
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO duplicate_groups(blob_id, keeper_file_id) VALUES (?1, ?2)",
            )?;
            let mut blobs: Vec<_> = by_blob.into_iter().collect();
            blobs.sort_by_key(|(b, _)| *b);
            for (blob, (size, mut members)) in blobs {
                members.sort_by_key(|(_, p)| keeper_rank(p));
                let (keeper_id, keeper) = members[0].clone();
                ins.execute(params![blob, keeper_id])?;
                out.push(DupGroup {
                    group_id: tx.last_insert_rowid(),
                    blob_id: blob,
                    size,
                    keeper,
                    copies: members[1..].iter().map(|(_, p)| p.clone()).collect(),
                });
            }
        }
        tx.commit()?;
        out.sort_by_key(|g| std::cmp::Reverse(g.size * g.copies.len() as u64));
        Ok(out)
    }

    /// Rebuild version chains: same folder, same extension, same normalised
    /// stem, at least one member carrying a version marker.
    pub fn rebuild_versions(&self) -> Result<Vec<VersionChain>> {
        let mut st = self.conn.prepare(
            "SELECT file_id, path, mtime FROM files WHERE status = 'present' AND kind = 'file'",
        )?;
        let rows: Vec<(String, String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;

        // key: (parent, ext, normalised stem) → members (file_id, path, mtime, marker)
        let mut groups: HashMap<(String, String, String), Vec<Member>> = HashMap::new();
        for (id, path, mtime) in rows {
            let p = PathBuf::from(&path);
            let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let parent = p
                .parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default();
            let (norm, marker) = normalize_stem_marker(&stem);
            if norm.is_empty() {
                continue;
            }
            groups
                .entry((parent, ext, norm))
                .or_default()
                .push((id, p, mtime, marker));
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM version_members", [])?;
        tx.execute("DELETE FROM version_chains", [])?;
        let mut out = Vec::new();
        {
            let mut ins_chain =
                tx.prepare_cached("INSERT INTO version_chains(canonical_file_id) VALUES (?1)")?;
            let mut ins_member =
                tx.prepare_cached("INSERT INTO version_members(chain_id, file_id, ordinal, mtime) VALUES (?1, ?2, ?3, ?4)")?;
            let mut keys: Vec<_> = groups.into_iter().collect();
            keys.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, mut members) in keys {
                if members.len() < 2 {
                    continue;
                }
                // A chain needs an explicit marker, or a bare-number series
                // that also contains the unnumbered base name.
                let strong = members.iter().any(|m| m.3 == Marker::Strong);
                let weak_with_base = members.iter().any(|m| m.3 == Marker::Weak)
                    && members.iter().any(|m| m.3 == Marker::None);
                if !(strong || weak_with_base) {
                    continue;
                }
                // newest last; ties broken by "unmarked name wins", then natural
                // name order so v10 sorts after v9 when mtimes are equal
                members.sort_by_key(|m| {
                    (
                        m.2,
                        m.3 == Marker::None,
                        natural_key(
                            &m.1.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        ),
                    )
                });
                let canonical = members.last().unwrap().clone();
                ins_chain.execute([&canonical.0])?;
                let chain_id = tx.last_insert_rowid();
                for (ordinal, m) in members.iter().enumerate() {
                    ins_member.execute(params![chain_id, m.0, ordinal as i64, m.2])?;
                }
                out.push(VersionChain {
                    chain_id,
                    canonical: canonical.1,
                    older: members[..members.len() - 1]
                        .iter()
                        .map(|m| m.1.clone())
                        .collect(),
                });
            }
        }
        tx.commit()?;
        out.sort_by_key(|c| std::cmp::Reverse(c.older.len()));
        Ok(out)
    }

    /// Inputs for the health score, optionally restricted to one root.
    pub fn health_inputs(&self, root_id: Option<i64>) -> Result<HealthInputs> {
        let scope = match root_id {
            Some(id) => format!("AND root_id = {id}"),
            None => String::new(),
        };
        let q1 = |sql: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(&sql.replace("{scope}", &scope), [], |r| {
                    r.get::<_, Option<i64>>(0)
                })?
                .unwrap_or(0))
        };
        let files =
            q1("SELECT COUNT(*) FROM files WHERE kind='file' AND status='present' {scope}")? as u64;
        let bytes =
            q1("SELECT SUM(size) FROM files WHERE kind='file' AND status='present' {scope}")?
                as u64;
        let duplicate_bytes = q1(
            "SELECT SUM(b.size) FROM files f JOIN blobs b ON b.blob_id=f.blob_id
             JOIN duplicate_groups g ON g.blob_id=f.blob_id
             WHERE f.status='present' AND f.kind='file' AND f.file_id <> g.keeper_file_id
               AND f.path NOT LIKE '%/node_modules/%' AND f.path NOT LIKE '%/.git/%'
               AND f.path NOT LIKE '%/.next/%' AND f.path NOT LIKE '%/target/%'
               AND f.path NOT LIKE '%/.cache/%' AND f.path NOT LIKE '%/venv/%' {scope}",
        )? as u64;
        let now = Utc::now().timestamp();
        let stale_cut = now - STALE_DAYS * 86_400;
        let downloads_files = q1(
            "SELECT COUNT(*) FROM files WHERE kind='file' AND status='present' AND lower(path) LIKE '%/downloads/%' {scope}",
        )? as u64;
        let stale_downloads = q1(&format!(
            "SELECT COUNT(*) FROM files WHERE kind='file' AND status='present' AND lower(path) LIKE '%/downloads/%' AND mtime < {stale_cut} {{scope}}"
        ))? as u64;
        let orphan_versions = q1(
            "SELECT COUNT(*) FROM version_members vm JOIN version_chains vc ON vc.chain_id = vm.chain_id
             JOIN files f ON f.file_id = vm.file_id
             WHERE vm.file_id <> vc.canonical_file_id AND f.status='present' {scope}",
        )? as u64;

        // naming needs a per-file rule; one pass over names.
        let mut st = self.conn.prepare(&format!(
            "SELECT name FROM files WHERE kind='file' AND status='present' {scope}"
        ))?;
        let mut unnamed = 0u64;
        for row in st.query_map([], |r| r.get::<_, String>(0))? {
            if looks_unnamed(&row?) {
                unnamed += 1;
            }
        }
        // unclassified = classified as Other, or not classified yet but of an unknown extension
        let unclassified = q1(
            "SELECT COUNT(*) FROM files f WHERE f.kind='file' AND f.status='present' {scope}
             AND COALESCE(
                 (SELECT category FROM classifications WHERE file_id = f.file_id AND source = 'user'),
                 (SELECT category FROM classifications WHERE file_id = f.file_id ORDER BY ts DESC LIMIT 1),
                 'other') = 'other'",
        )? as u64;

        Ok(HealthInputs {
            files,
            bytes,
            duplicate_bytes,
            stale_downloads,
            downloads_files,
            unnamed,
            orphan_versions,
            unclassified,
            free_fraction: None,
        })
    }

    pub fn record_health(&self, root_id: Option<i64>, h: &Health) -> Result<()> {
        self.conn.execute(
            "INSERT INTO health_snapshots(ts, root_id, score, components) VALUES (?1, ?2, ?3, ?4)",
            params![
                Utc::now().timestamp(),
                root_id,
                h.score as f64,
                serde_json::to_string(&h.components)?
            ],
        )?;
        Ok(())
    }

    pub fn health_history(&self, root_id: Option<i64>, limit: usize) -> Result<Vec<(i64, u8)>> {
        let mut st = self.conn.prepare(
            "SELECT ts, score FROM health_snapshots WHERE root_id IS ?1 ORDER BY ts DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![root_id, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)? as u8))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Compute + record health for the whole index and every root.
    pub fn refresh_health(&self) -> Result<Health> {
        let all = health::score(&self.health_inputs(None)?);
        self.record_health(None, &all)?;
        for r in self.list_roots()? {
            let h = health::score(&self.health_inputs(Some(r.root_id))?);
            self.record_health(Some(r.root_id), &h)?;
        }
        Ok(all)
    }

    // ----- suggestions ------------------------------------------------

    /// Regenerate Observe-mode suggestions from the current groups and chains.
    /// Existing rows keep their state (a dismissed suggestion stays dismissed);
    /// rows whose key no longer exists are marked stale.
    pub fn refresh_suggestions(&self, dups: &[DupGroup], chains: &[VersionChain]) -> Result<usize> {
        let folders = self
            .folder_duplicates(FOLDER_DUP_MIN_FILES)
            .unwrap_or_default();
        self.refresh_suggestions_with(dups, chains, &folders)
    }

    pub fn refresh_suggestions_with(
        &self,
        dups: &[DupGroup],
        chains: &[VersionChain],
        folders: &[crate::FolderDup],
    ) -> Result<usize> {
        let now = Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE suggestions SET state = 'stale', updated_ts = ?1 WHERE state = 'proposed'",
            [now],
        )?;
        let mut n = 0usize;
        {
            let mut up = tx.prepare_cached(
                "INSERT INTO suggestions(kind, key, subject, rationale, est_bytes, risk_tier, state, created_ts, updated_ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', ?7, ?7)
                 ON CONFLICT(kind, key) DO UPDATE SET
                   subject = excluded.subject, rationale = excluded.rationale, est_bytes = excluded.est_bytes,
                   updated_ts = excluded.updated_ts,
                   state = CASE WHEN suggestions.state = 'dismissed' THEN 'dismissed' ELSE 'proposed' END",
            )?;
            // whole-folder copies first: one suggestion per group, and the
            // per-file duplicates inside them are folded into it
            for f in folders {
                let saved = f.bytes * f.copies.len() as u64;
                up.execute(params![
                    "trash_duplicate_folder",
                    f.keeper.to_string_lossy().to_string(),
                    json!({"keep": f.keeper, "trash": f.copies, "files": f.files, "bytes": f.bytes}).to_string(),
                    format!(
                        "{} is a byte-for-byte copy of {} ({} files, {}) — move the whole copy to Trash as one step (reversible).",
                        f.copies.iter().map(|c| c.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).collect::<Vec<_>>().join(", "),
                        f.keeper.display(),
                        f.files,
                        health::human(f.bytes)
                    ),
                    saved as i64,
                    2i64,
                    now
                ])?;
                n += 1;
            }
            let inside_folder_dup = |p: &Path| {
                folders
                    .iter()
                    .any(|f| p.starts_with(&f.keeper) || f.copies.iter().any(|c| p.starts_with(c)))
            };
            for g in dups {
                // Copies inside dependency/build trees are not the user's mess to
                // sort file-by-file; a duplicated project is handled as a whole (Phase 6).
                if filemind_core::scanner::in_noise_dir(&g.keeper)
                    || g.copies
                        .iter()
                        .any(|c| filemind_core::scanner::in_noise_dir(c))
                {
                    continue;
                }
                if inside_folder_dup(&g.keeper) || g.copies.iter().any(|c| inside_folder_dup(c)) {
                    continue;
                }
                let saved = g.size * g.copies.len() as u64;
                if saved < 64 * 1024 {
                    continue; // not worth a suggestion
                }
                up.execute(params![
                    "trash_duplicates",
                    g.blob_id.to_string(),
                    json!({"keep": g.keeper, "trash": g.copies}).to_string(),
                    format!(
                        "{} identical cop{} of {} — keep the one in {} and move the rest to Trash (reversible).",
                        g.copies.len(),
                        if g.copies.len() == 1 { "y" } else { "ies" },
                        g.keeper.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        g.keeper.parent().map(|p| p.display().to_string()).unwrap_or_default()
                    ),
                    saved as i64,
                    2i64,
                    now
                ])?;
                n += 1;
            }
            for c in chains {
                if filemind_core::scanner::in_noise_dir(&c.canonical) {
                    continue;
                }
                up.execute(params![
                    "collapse_versions",
                    c.canonical.to_string_lossy().to_string(),
                    json!({"keep": c.canonical, "older": c.older}).to_string(),
                    format!(
                        "{} older version{} of {} — collapse into a versions folder, newest stays in place (reversible).",
                        c.older.len(),
                        if c.older.len() == 1 { "" } else { "s" },
                        c.canonical.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                    ),
                    0i64,
                    1i64,
                    now
                ])?;
                n += 1;
            }
            // one stale-downloads suggestion per root that has a Downloads-like area
            let stale_cut = now - STALE_DAYS * 86_400;
            for r in self.list_roots()? {
                let (count, bytes): (i64, Option<i64>) = tx.query_row(
                    "SELECT COUNT(*), SUM(size) FROM files WHERE root_id = ?1 AND kind='file' AND status='present'
                     AND lower(path) LIKE '%/downloads/%' AND mtime < ?2",
                    params![r.root_id, stale_cut],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if count >= 20 {
                    up.execute(params![
                        "stale_downloads",
                        r.root_id.to_string(),
                        json!({"root": r.path, "count": count, "older_than_days": STALE_DAYS}).to_string(),
                        format!(
                            "{} files ({}) in Downloads have not been touched in {STALE_DAYS}+ days — archive them to a dated folder (reversible).",
                            count,
                            health::human(bytes.unwrap_or(0) as u64)
                        ),
                        bytes.unwrap_or(0),
                        1i64,
                        now
                    ])?;
                    n += 1;
                }
            }
            // Shrink tier 1: only where transparent compression exists (or
            // when a test asks for it), and only after an estimate measured
            // the ratios — "measure first, promise nothing".
            if cfg!(target_os = "macos") || std::env::var_os("FILEMIND_SHRINK_ANYWHERE").is_some() {
                for b in self.compress_batches(now)? {
                    let files: Vec<String> = b
                        .files
                        .iter()
                        .map(|(p, _)| p.to_string_lossy().to_string())
                        .collect();
                    up.execute(params![
                        "compress_cold_text",
                        format!("{}:{}", b.root_id, b.bucket),
                        json!({
                            "root": b.root, "bucket": b.bucket, "ratio": b.ratio, "method": "apfs",
                            "files": files, "bytes": b.bytes(),
                            "total_files": b.total_files, "total_bytes": b.total_bytes
                        })
                        .to_string(),
                        format!(
                            "{} {} files ({}) untouched for 30+ days{} could take about {} less space with APFS transparent compression. They stay exactly the same to every app; reversible in place.{}",
                            b.files.len(),
                            b.bucket,
                            health::human(b.bytes()),
                            b.root.file_name().map(|n| format!(" in {}", n.to_string_lossy())).unwrap_or_default(),
                            health::human(b.saving()),
                            if b.total_files > b.files.len() as u64 {
                                format!(" ({} more qualify; they come in the next batch.)", b.total_files - b.files.len() as u64)
                            } else {
                                String::new()
                            }
                        ),
                        b.saving() as i64,
                        1i64,
                        now
                    ])?;
                    n += 1;
                }
            }
            // Shrink tier 3: one archive suggestion per cold project the
            // estimate measured. Works on every platform (the pack is ours).
            if let Some(est) = self.shrink_estimate()? {
                let tiers = est["tiers"].as_array().cloned().unwrap_or_default();
                if let Some(t3) = tiers.iter().find(|t| t["kind"] == "cold_archive") {
                    for p in t3["projects"].as_array().cloned().unwrap_or_default() {
                        let Some(folder) = p["root_path"].as_str() else {
                            continue;
                        };
                        let saving = p["saving_bytes"].as_u64().unwrap_or(0);
                        let bytes = p["bytes"].as_u64().unwrap_or(0);
                        if saving < ARCHIVE_MIN_SAVING {
                            continue;
                        }
                        let name = p["name"].as_str().unwrap_or("project").to_string();
                        let end = p["end_ts"].as_i64().unwrap_or(0);
                        let months = ((now - end) / (30 * 86_400)).max(1);
                        up.execute(params![
                            "archive_cold_project",
                            format!("archive:{}", p["project_id"]),
                            json!({
                                "project_id": p["project_id"], "name": name, "folder": folder,
                                "files": p["files"], "bytes": bytes, "saving": saving,
                                "ratio": p["ratio"], "measured": p["measured"]
                            })
                            .to_string(),
                            format!(
                                "{name} has not been touched in {months} month{} — pack it ({} in {} files) into a verified compressed archive, reclaiming about {}{}. Search still finds every file inside; restore is one command. The original goes to Trash only after every file in the archive is decoded and checked.",
                                if months == 1 { "" } else { "s" },
                                health::human(bytes),
                                p["files"].as_u64().unwrap_or(0),
                                health::human(saving),
                                if p["measured"].as_bool() == Some(true) {
                                    " (measured on this project's own files)"
                                } else {
                                    ""
                                }
                            ),
                            saving as i64,
                            1i64,
                            now
                        ])?;
                        n += 1;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn list_suggestions(&self, state: &str, limit: usize) -> Result<Vec<Suggestion>> {
        let mut st = self.conn.prepare(
            "SELECT suggestion_id, kind, subject, rationale, est_bytes, risk_tier, state FROM suggestions
             WHERE state = ?1 ORDER BY est_bytes DESC, suggestion_id LIMIT ?2",
        )?;
        let rows = st.query_map(params![state, limit as i64], |r| {
            Ok(Suggestion {
                id: r.get(0)?,
                kind: r.get(1)?,
                subject: serde_json::from_str(&r.get::<_, String>(2)?)
                    .unwrap_or(serde_json::Value::Null),
                rationale: r.get(3)?,
                est_bytes: r.get::<_, i64>(4)? as u64,
                risk_tier: r.get::<_, i64>(5)? as u8,
                state: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_suggestion_state(&self, id: i64, state: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE suggestions SET state = ?2, updated_ts = ?3 WHERE suggestion_id = ?1",
            params![id, state, Utc::now().timestamp()],
        )?;
        Ok(n == 1)
    }

    pub fn suggestion_totals(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(est_bytes),0) FROM suggestions WHERE state = 'proposed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub id: i64,
    pub kind: String,
    pub subject: serde_json::Value,
    pub rationale: String,
    pub est_bytes: u64,
    pub risk_tier: u8,
    pub state: String,
}
