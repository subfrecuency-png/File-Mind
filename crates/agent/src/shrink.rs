//! Shrink: run the estimate over the index and cache the report.

use anyhow::Result;
use filemind_core::shrink::{estimate, Estimate, Estimator, FileIn, ProjectIn};
use filemind_storage::Db;

/// Seconds a cached estimate is considered fresh.
pub const FRESH_SECS: i64 = 24 * 3600;

/// Walk the index, probe the samples, store the report under
/// `settings.shrink.estimate` and return it. `progress(done, total)` is
/// called while probing.
pub fn run_estimate(db: &Db, mut progress: impl FnMut(u64, u64)) -> Result<Estimate> {
    let now = chrono::Utc::now().timestamp();
    let mut est = Estimator::new(now, now as u64);
    db.shrink_rows(|r| {
        est.add(&FileIn {
            path: &r.path,
            ext: r.ext.as_deref(),
            size: r.size,
            mtime: r.mtime,
            sensitive: r.sensitive,
            category: &r.category,
            cold_project: r.cold_project,
        })
    })?;
    let projects: Vec<ProjectIn> = db
        .cold_projects()?
        .into_iter()
        .map(|p| ProjectIn {
            project_id: p.project_id,
            name: p.name,
            root_path: p.folder,
            end_ts: p.end_ts,
        })
        .collect();
    let total = est.sample_count() as u64;
    let mut done = 0u64;
    progress(0, total);
    let report = est.finish(&projects, |path, size| {
        done += 1;
        if done.is_multiple_of(16) || done == total {
            progress(done, total);
        }
        match estimate::probe_file(path, size) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::debug!("shrink: skip {}: {e}", path.display());
                None
            }
        }
    });
    db.set_shrink_estimate(&serde_json::to_value(&report)?)?;
    Ok(report)
}

/// The cached report if it is younger than `FRESH_SECS`.
pub fn cached(db: &Db) -> Result<Option<serde_json::Value>> {
    let now = chrono::Utc::now().timestamp();
    Ok(db.shrink_estimate()?.filter(|v| {
        v.get("computed_ts")
            .and_then(serde_json::Value::as_i64)
            .map(|t| now - t < FRESH_SECS)
            .unwrap_or(false)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_runs_over_a_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let text = "fn main() { println!(\"hello, world\"); }\n".repeat(400);
        for i in 0..6 {
            std::fs::write(root.join(format!("src/f{i}.rs")), &text).unwrap();
        }
        let db = Db::open_in_memory().unwrap();
        let old = chrono::Utc::now().timestamp() - 90 * 86_400;
        db.conn
            .execute(
                "INSERT INTO roots(root_id, path, mode, added_ts) VALUES (1, ?1, 'observe', 1)",
                [root.to_string_lossy().to_string()],
            )
            .unwrap();
        for i in 0..6 {
            let p = root.join(format!("src/f{i}.rs"));
            db.conn
                .execute(
                    "INSERT INTO files(file_id, root_id, path, name, ext, size, mtime, kind, first_seen, last_seen)
                     VALUES (?1, 1, ?2, ?3, 'rs', ?4, ?5, 'file', 1, 1)",
                    rusqlite::params![format!("1:{i}"), p.to_string_lossy().to_string(), format!("f{i}.rs"), text.len() as i64, old],
                )
                .unwrap();
            db.conn
                .execute(
                    "INSERT INTO classifications(file_id, category, confidence, signals, source, ts) VALUES (?1, 'code', 0.9, '[]', 'auto', 1)",
                    [format!("1:{i}")],
                )
                .unwrap();
        }
        let mut ticks = 0;
        let est = run_estimate(&db, |_, _| ticks += 1).unwrap();
        assert!(ticks >= 1);
        assert_eq!(est.files_seen, 6);
        let t1 = est.tiers.iter().find(|t| t.kind == "apfs").unwrap();
        assert_eq!(t1.candidate_files, 6);
        assert!(t1.ratio < 0.2, "{t1:?}");
        assert!(est.saving_bytes > (6 * text.len() as u64) * 8 / 10);
        assert!(cached(&db).unwrap().is_some());
        assert_eq!(
            cached(&db).unwrap().unwrap()["saving_bytes"],
            est.saving_bytes
        );
    }
}
