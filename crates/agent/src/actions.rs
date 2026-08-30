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
            if let Some(k) = s.subject["keep"].as_str() {
                m.keeps.push(PathBuf::from(k));
            }
            for p in paths(&s.subject["trash"]) {
                m.steps.push(Step::Trash {
                    path: p,
                    hash_before: None,
                    trashed_to: None,
                });
            }
        }
        "trash_duplicate_folder" => {
            if let Some(k) = s.subject["keep"].as_str() {
                m.keeps.push(PathBuf::from(k));
            }
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
            m.keeps.push(keep.clone());
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

fn fingerprint(m: &Manifest) -> String {
    let mut h = blake3::Hasher::new();
    for s in &m.steps {
        match s {
            Step::Move { from, to, .. } => {
                h.update(b"M");
                h.update(from.to_string_lossy().as_bytes());
                h.update(b"\0");
                h.update(to.to_string_lossy().as_bytes());
            }
            Step::Trash { path, .. } => {
                h.update(b"T");
                h.update(path.to_string_lossy().as_bytes());
            }
        }
        h.update(b"\n");
    }
    h.finalize().to_hex()[..16].to_string()
}

#[derive(Debug, serde::Serialize)]
pub struct Plan {
    pub txn_id: String,
    pub steps: usize,
    pub diff: String,
    /// Content hash of the steps; `apply` uses it to confirm the plan the
    /// user approved is the plan that runs.
    pub fingerprint: String,
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
        fingerprint: fingerprint(&m),
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
///
/// `previewed` is the (txn_id, fingerprint) the user saw from `plan`; the
/// re-planned steps must match it exactly, and the transaction then runs
/// under that id so the preview and the history agree.
pub fn apply(
    adapter: &dyn OsAdapter,
    db: &Db,
    suggestion_id: i64,
    approved: bool,
    previewed: Option<(&str, &str)>,
) -> Result<Applied> {
    let (mut m, plan) = plan(adapter, db, suggestion_id)?;
    if !plan.problems.is_empty() {
        bail!("refusing to run:\n  {}", plan.problems.join("\n  "));
    }
    if let Some((id, fp)) = previewed {
        if fp != plan.fingerprint {
            bail!("the plan changed since it was previewed — run `filemind suggest plan {suggestion_id}` again");
        }
        if !id.starts_with("txn_") || id.len() > 64 {
            bail!("bad transaction id");
        }
        if db.load_txn(id)?.is_some() {
            bail!("transaction {id} already exists");
        }
        m.txn_id = id.to_string();
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
                        "UPDATE files SET status = 'present' WHERE (path = ?1 OR path LIKE ?2) AND status = 'trashed'",
                        rusqlite::params![
                            path.to_string_lossy(),
                            format!("{}/%", path.to_string_lossy())
                        ],
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
    // The database opened fine: the plaintext copy kept from the SQLCipher
    // conversion (Phase 9.5) is no longer needed. Trash, never delete.
    if let Ok(Some(backup)) = db.pre_cipher_backup_ready_to_trash() {
        match adapter.move_to_trash(&backup) {
            Ok(_) => {
                tracing::info!(backup = %backup.display(), "pre-SQLCipher plaintext copy moved to Trash")
            }
            Err(e) => {
                tracing::warn!(backup = %backup.display(), error = %e, "could not trash the plaintext copy")
            }
        }
    }
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
