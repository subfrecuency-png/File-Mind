//! Turning suggestions into transactions, and running them safely.
//!
//! plan → (user sees diff) → apply → (later) undo. Recovery runs on agent
//! start for anything a previous process left unfinished.

use anyhow::{bail, Context, Result};
use chrono::{TimeZone, Utc};
use filemind_core::txn::{self, CrashPoint, Initiator, Manifest, Step, TxnState};
use filemind_core::{Mode, OsAdapter, RiskTier};
use filemind_storage::{Db, Suggestion};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn archive_root(home: &Path) -> PathBuf {
    home.join("FileMind Archive")
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .context("no home directory")
}

fn paths(v: &Value) -> Vec<PathBuf> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Build a manifest for one suggestion. Nothing is touched.
pub fn plan_suggestion(db: &Db, s: &Suggestion) -> Result<Manifest> {
    let mode: Mode = serde_json::from_value(
        db.get_setting("mode")?
            .unwrap_or(serde_json::json!("observe")),
    )
    .unwrap_or_default();
    let mut m = Manifest::new(
        mode,
        Initiator::User,
        RiskTier::from_u8(s.risk_tier),
        s.rationale.clone(),
    );
    m.rule_id = Some(format!("suggestion:{}", s.id));
    match s.kind.as_str() {
        "trash_duplicates" => {
            for p in paths(&s.subject["trash"]) {
                m.steps.push(Step::Trash {
                    path: p,
                    hash_before: None,
                    trashed_to: None,
                });
            }
        }
        "collapse_versions" => {
            let keep = PathBuf::from(s.subject["keep"].as_str().unwrap_or_default());
            let stem = keep
                .file_stem()
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_else(|| "versions".into());
            let dir = keep
                .parent()
                .unwrap_or(Path::new("/"))
                .join(format!("{stem} versions"));
            for p in paths(&s.subject["older"]) {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                m.steps.push(Step::Move {
                    from: p,
                    to: dir.join(name),
                    hash_before: None,
                });
            }
        }
        "stale_downloads" => {
            let root = PathBuf::from(s.subject["root"].as_str().unwrap_or_default());
            let days = s.subject["older_than_days"].as_i64().unwrap_or(90);
            let cut = Utc::now().timestamp() - days * 86_400;
            let home = home_dir()?;
            let mut st = db.conn.prepare(
                "SELECT path, mtime FROM files WHERE kind='file' AND status='present'
                 AND path LIKE ?1 AND lower(path) LIKE '%/downloads/%' AND mtime < ?2
                 AND path NOT LIKE '%/node_modules/%' AND path NOT LIKE '%/.git/%' ORDER BY mtime",
            )?;
            let rows: Vec<(String, i64)> = st
                .query_map(
                    rusqlite::params![format!("{}/%", root.display()), cut],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<std::result::Result<_, _>>()?;
            for (p, mtime) in rows {
                let p = PathBuf::from(p);
                // only loose files and shallow folders go to the archive; deep project
                // trees are left alone (a project-level archive is a separate suggestion)
                let rel = p.strip_prefix(&root).unwrap_or(&p);
                if rel.components().count() > 2 {
                    continue;
                }
                let month = Utc
                    .timestamp_opt(mtime, 0)
                    .single()
                    .map(|t| t.format("%Y/%Y-%m").to_string())
                    .unwrap_or_else(|| "undated".into());
                let to = archive_root(&home).join("Downloads").join(month).join(rel);
                m.steps.push(Step::Move {
                    from: p,
                    to,
                    hash_before: None,
                });
            }
        }
        other => bail!("cannot plan suggestion kind {other}"),
    }
    if m.steps.is_empty() {
        bail!("nothing to do for suggestion #{}", s.id);
    }
    Ok(m)
}

#[derive(Debug, serde::Serialize)]
pub struct Plan {
    pub txn_id: String,
    pub steps: usize,
    pub diff: String,
    pub problems: Vec<String>,
    pub risk_tier: u8,
    pub mode: Mode,
}

pub fn plan(adapter: &dyn OsAdapter, db: &Db, suggestion_id: i64) -> Result<(Manifest, Plan)> {
    let s = db
        .list_suggestions("proposed", usize::MAX)?
        .into_iter()
        .find(|s| s.id == suggestion_id)
        .with_context(|| format!("no proposed suggestion #{suggestion_id}"))?;
    let mut m = plan_suggestion(db, &s)?;
    let roots: Vec<PathBuf> = db.list_roots()?.into_iter().map(|r| r.path).collect();
    // the archive lives in the home folder; allow it as a destination
    let mut allowed = roots.clone();
    allowed.push(archive_root(&home_dir()?));
    let problems = txn::validate(adapter, &allowed, &mut m)?;
    let plan = Plan {
        txn_id: m.txn_id.clone(),
        steps: m.steps.len(),
        diff: m.diff(),
        problems,
        risk_tier: m.risk_tier.as_u8(),
        mode: m.mode,
    };
    Ok((m, plan))
}

#[derive(Debug, serde::Serialize)]
pub struct Applied {
    pub txn_id: String,
    pub done: usize,
    pub failed: usize,
    pub state: String,
}

/// Validate, gate on mode/approval, execute, and record effects.
pub fn apply(
    adapter: &dyn OsAdapter,
    db: &Db,
    suggestion_id: i64,
    approved: bool,
) -> Result<Applied> {
    let (mut m, plan) = plan(adapter, db, suggestion_id)?;
    if !plan.problems.is_empty() {
        bail!("refusing to run:\n  {}", plan.problems.join("\n  "));
    }
    if let Err(why) = txn::permitted(m.mode, &m, approved) {
        bail!("{why}");
    }
    let rep = txn::execute(adapter, db, &mut m, CrashPoint::Never)?;
    let (m2, state, states) = db.load_txn(&m.txn_id)?.context("transaction vanished")?;
    // executed manifest carries trashed_to; prefer it for effects
    db.note_txn_effects(&m, &states)?;
    let _ = m2;
    if rep.failed == 0 {
        db.set_suggestion_state(suggestion_id, "accepted")?;
    }
    Ok(Applied {
        txn_id: m.txn_id,
        done: rep.done,
        failed: rep.failed,
        state: state.as_str().to_string(),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct Undone {
    pub txn_id: String,
    pub restored: usize,
    pub skipped: Vec<String>,
}

pub fn undo(adapter: &dyn OsAdapter, db: &Db, txn_id: &str) -> Result<Undone> {
    let rep = txn::undo(adapter, db, txn_id)?;
    // reverse the inventory effects for restored steps
    if let Some((m, _, states)) = db.load_txn(txn_id)? {
        let now = Utc::now().timestamp();
        for (i, s) in m.steps.iter().enumerate() {
            if states[i] != filemind_core::txn::StepState::Undone {
                continue;
            }
            match s {
                Step::Move { from, to, .. } => {
                    db.conn.execute(
                        "UPDATE files SET path = ?2, name = ?3 WHERE path = ?1",
                        rusqlite::params![
                            to.to_string_lossy(),
                            from.to_string_lossy(),
                            from.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default()
                        ],
                    )?;
                    db.conn.execute(
                        "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                         SELECT file_id, ?3, 'moved', ?1, ?2, 'txn' FROM files WHERE path = ?2",
                        rusqlite::params![to.to_string_lossy(), from.to_string_lossy(), now],
                    )?;
                }
                Step::Trash { path, .. } => {
                    db.conn.execute(
                        "UPDATE files SET status = 'present' WHERE path = ?1 AND status = 'trashed'",
                        [path.to_string_lossy()],
                    )?;
                    db.conn.execute(
                        "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                         SELECT file_id, ?2, 'restored', NULL, path, 'txn' FROM files WHERE path = ?1",
                        rusqlite::params![path.to_string_lossy(), now],
                    )?;
                }
            }
        }
    }
    Ok(Undone {
        txn_id: txn_id.to_string(),
        restored: rep.restored,
        skipped: rep.skipped,
    })
}

/// Settle anything a previous process left running. Called on agent start
/// and before any new transaction.
pub fn recover_all(adapter: &dyn OsAdapter, db: &Db) -> Result<Vec<(String, TxnState)>> {
    let mut out = Vec::new();
    for id in filemind_core::txn::Journal::unfinished(db)? {
        let rep = txn::recover(adapter, db, &id)?;
        if let Some((m, state, states)) = db.load_txn(&id)? {
            db.note_txn_effects(&m, &states)?;
            tracing::warn!(txn = %id, done = rep.done, conflicts = rep.conflicts, state = ?state, "recovered unfinished transaction");
            out.push((id, state));
        }
    }
    Ok(out)
}
