//! Automate mode (Phase 9): rules that run unattended, after a preview week.
//!
//! Lifecycle of a rule:
//!
//! * `preview` — created. Every scheduler tick plans the rule against the
//!   current index and records a **dry run** (the manifest it would have
//!   executed). Nothing moves. The Automate screen shows "in the last 7 days
//!   this rule would have moved N files".
//! * `armed` — the user pressed Arm, which is only accepted once the rule
//!   has been previewing for `automate.preview_days` (default 7) *and* at
//!   least one dry run exists. From now on a tick executes the plan when the
//!   mode is Automate: a normal journaled transaction with
//!   `Initiator::Rule`, capped by `max_items_per_run`, visible in History,
//!   undoable like everything else.
//! * `paused` — any conflict, failed step or a plan bigger than
//!   `pause_above` pauses the rule with a reason. Arming again is a
//!   deliberate act by the user.
//!
//! Only tier-0 kinds exist (`core::rules`), so `txn::permitted` in Automate
//! mode is the last gate, not the only one.

use crate::actions;
use anyhow::{bail, Context, Result};
use chrono::{TimeZone, Utc};
use filemind_core::rules::RuleKind;
use filemind_core::txn::{self, CrashPoint, Initiator, Manifest, Step};
use filemind_core::versions::{normalize_stem_marker, Marker};
use filemind_core::{Mode, OsAdapter, RiskTier};
use filemind_storage::{Db, Rule};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const DEFAULT_PREVIEW_DAYS: i64 = 7;
/// Dry runs older than this are pruned from the log.
const KEEP_DRY_RUNS_SECS: i64 = 30 * 86_400;

pub fn preview_days(db: &Db) -> i64 {
    db.get_setting("automate.preview_days")
        .ok()
        .flatten()
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_PREVIEW_DAYS)
        .max(0)
}

pub fn current_mode(db: &Db) -> Mode {
    db.get_setting("mode")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn in_downloads(p: &Path) -> bool {
    p.to_string_lossy().to_lowercase().contains("/downloads/")
}

/// The part of `p` under its (last) `Downloads` folder: `~/Downloads/a/b.pdf` → `a/b.pdf`.
fn below_downloads(p: &Path) -> Option<PathBuf> {
    let comps: Vec<_> = p.components().collect();
    let idx = comps.iter().rposition(|c| {
        c.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("downloads")
    })?;
    let rel: PathBuf = comps[idx + 1..].iter().collect();
    (!rel.as_os_str().is_empty()).then_some(rel)
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .context("no home directory")
}

/// What one evaluation of a rule found, before capping.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Evaluation {
    pub rule_id: i64,
    pub kind: String,
    /// Files that qualified in total (before the per-run cap).
    pub candidates: u64,
    pub candidate_bytes: u64,
    /// Steps in the manifest (≤ max_items_per_run).
    pub steps: usize,
    pub bytes: u64,
    pub capped: bool,
    pub problems: Vec<String>,
    pub diff: String,
}

/// Plan a rule against the current index. Nothing is touched; the manifest
/// is validated against the disk so `problems` is meaningful.
pub fn plan(adapter: &dyn OsAdapter, db: &Db, rule: &Rule) -> Result<(Manifest, Evaluation)> {
    let kind = RuleKind::parse(&rule.kind, &rule.params).map_err(|e| anyhow::anyhow!(e))?;
    let now = Utc::now().timestamp();
    let mut m = Manifest::new(
        current_mode(db),
        Initiator::Rule,
        RiskTier::Tier0,
        format!("rule #{}: {}", rule.rule_id, kind.describe()),
    );
    m.rule_id = Some(format!("rule:{}", rule.rule_id));
    let home = home_dir()?;
    let roots: Vec<PathBuf> = db.list_roots()?.into_iter().map(|r| r.path).collect();

    // Groups of steps that must stay together (a version chain), each with
    // its size and the file the group keeps in place. Oldest first.
    let mut groups: Vec<(Vec<Step>, u64, Option<PathBuf>)> = Vec::new();
    let mut candidate_bytes = 0u64;
    match &kind {
        RuleKind::ArchiveStaleDownloads(p) => {
            let cut = now - i64::from(p.older_than_days) * 86_400;
            let archive = home.join("FileMind Archive").join("Downloads");
            for root in &roots {
                for (path, mtime, size) in db.stale_downloads(root, cut)? {
                    // loose files and shallow folders only; deep trees are
                    // projects, and a project is never archived by a rule
                    let Some(rel) = below_downloads(&path) else {
                        continue;
                    };
                    if rel.components().count() > 2 || filemind_core::scanner::in_noise_dir(&path) {
                        continue;
                    }
                    let _ = root;
                    if path.starts_with(&archive) {
                        continue;
                    }
                    let month = Utc
                        .timestamp_opt(mtime, 0)
                        .single()
                        .map(|t| t.format("%Y/%Y-%m").to_string())
                        .unwrap_or_else(|| "undated".into());
                    let to = archive.join(month).join(rel);
                    candidate_bytes += size;
                    groups.push((
                        vec![Step::Move {
                            from: path,
                            to,
                            hash_before: None,
                        }],
                        size,
                        None,
                    ));
                }
            }
        }
        RuleKind::CollapseVersions(p) => {
            let cut = now - i64::from(p.older_than_days) * 86_400;
            for chain in db.version_chain_members()? {
                if chain.members.len() < 2 {
                    continue;
                }
                let (canonical, _) = chain.members.last().unwrap().clone();
                if filemind_core::scanner::in_noise_dir(&canonical)
                    || !roots.iter().any(|r| canonical.starts_with(r))
                {
                    continue;
                }
                if p.strong_markers_only {
                    let strong = chain.members.iter().any(|(path, _)| {
                        path.file_stem()
                            .map(|s| {
                                normalize_stem_marker(&s.to_string_lossy()).1 == Marker::Strong
                            })
                            .unwrap_or(false)
                    });
                    if !strong {
                        continue;
                    }
                }
                let stem = canonical
                    .file_stem()
                    .map(|x| x.to_string_lossy().to_string())
                    .unwrap_or_else(|| "versions".into());
                let dir = canonical
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .join(format!("{stem} versions"));
                let older = &chain.members[..chain.members.len() - 1];
                // the whole chain must be cold; a chain someone is still
                // working on is not a rule's business
                if older.iter().any(|(_, mtime)| *mtime >= cut) {
                    continue;
                }
                let mut steps = Vec::new();
                let mut size = 0u64;
                for (path, _) in older {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    size += std::fs::metadata(path).map(|md| md.len()).unwrap_or(0);
                    steps.push(Step::Move {
                        from: path.clone(),
                        to: dir.join(name),
                        hash_before: None,
                    });
                }
                candidate_bytes += size;
                groups.push((steps, size, Some(canonical.clone())));
            }
        }
        RuleKind::TrashExactDuplicates(p) => {
            let cut = now - i64::from(p.older_than_days) * 86_400;
            for d in db.dup_copies(p.min_bytes)? {
                if d.copy_mtime >= cut {
                    continue;
                }
                if p.keeper_must_be_outside_downloads && in_downloads(&d.keeper) {
                    continue;
                }
                if p.copies_in_downloads_only && !in_downloads(&d.copy) {
                    continue;
                }
                if filemind_core::scanner::in_noise_dir(&d.copy)
                    || filemind_core::scanner::in_noise_dir(&d.keeper)
                    || !roots.iter().any(|r| d.copy.starts_with(r))
                {
                    continue;
                }
                if !d.keeper.is_file() {
                    continue; // the keeper must be there right now
                }
                candidate_bytes += d.size;
                groups.push((
                    vec![Step::Trash {
                        path: d.copy,
                        hash_before: None,
                        trashed_to: None,
                    }],
                    d.size,
                    Some(d.keeper.clone()),
                ));
            }
        }
    }

    let total: u64 = groups.iter().map(|g| g.0.len() as u64).sum();
    let cap = kind.max_items_per_run() as usize;
    let mut bytes = 0u64;
    // groups stay whole: a version chain is collapsed in one run or not at all
    for (steps, size, keep) in groups {
        if m.steps.len() + steps.len() > cap {
            if m.steps.is_empty() {
                continue; // a single group larger than the cap waits for a bigger cap
            }
            break;
        }
        bytes += size;
        m.steps.extend(steps);
        if let Some(k) = keep {
            if !m.keeps.contains(&k) {
                m.keeps.push(k);
            }
        }
    }
    let capped = total > m.steps.len() as u64;

    let mut allowed = roots.clone();
    allowed.push(home.join("FileMind Archive"));
    let mut problems = if m.steps.is_empty() {
        Vec::new()
    } else {
        txn::validate(adapter, &allowed, &mut m)?
    };
    if let RuleKind::TrashExactDuplicates(_) = &kind {
        // the keeper of every trashed copy must still be present and intact
        // at execution: `keeps` is checked here, hashes below at execute time
        for k in &m.keeps {
            if !k.is_file() {
                problems.push(format!("keeper {} is missing", k.display()));
            }
        }
    }
    if total > kind.pause_above() {
        problems.push(format!(
            "{total} files qualify, above the rule's pause_above of {} — review before letting it run",
            kind.pause_above()
        ));
    }
    let eval = Evaluation {
        rule_id: rule.rule_id,
        kind: rule.kind.clone(),
        candidates: total,
        candidate_bytes,
        steps: m.steps.len(),
        bytes,
        capped,
        problems,
        diff: m.diff(),
    };
    Ok((m, eval))
}

fn summary_json(e: &Evaluation, executed: Option<&actions::Applied>) -> Value {
    let mut v = json!({
        "candidates": e.candidates, "candidate_bytes": e.candidate_bytes,
        "files": e.steps, "bytes": e.bytes, "capped": e.capped, "problems": e.problems,
    });
    if let Some(a) = executed {
        v["done"] = json!(a.done);
        v["failed"] = json!(a.failed);
        v["state"] = json!(a.state);
    }
    v
}

/// For the Automate screen: what the rule would have done over the preview window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WouldHave {
    pub rule_id: i64,
    pub since_ts: i64,
    pub dry_runs: usize,
    pub real_runs: usize,
    /// Distinct source paths across all dry runs in the window.
    pub files: Vec<String>,
    pub bytes: u64,
    pub last_eval_ts: Option<i64>,
    pub last_problems: Vec<String>,
    pub armable: bool,
    pub armable_in_secs: i64,
    pub armable_reason: String,
}

pub fn would_have(db: &Db, rule: &Rule, window_days: i64) -> Result<WouldHave> {
    let now = Utc::now().timestamp();
    let since = now - window_days * 86_400;
    let runs = db.list_automation_runs(rule.rule_id, since, 10_000)?;
    let mut files: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    let mut dry = 0;
    let mut real = 0;
    for r in &runs {
        if r.dry_run {
            dry += 1;
        } else {
            real += 1;
            continue;
        }
        if let Some(steps) = r.manifest.get("steps").and_then(Value::as_array) {
            for s in steps {
                let src = s
                    .get("from")
                    .or_else(|| s.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !src.is_empty() && !files.iter().any(|f| f == src) {
                    files.push(src.to_string());
                }
            }
        }
        bytes = bytes.max(r.summary["bytes"].as_u64().unwrap_or(0));
    }
    let last = runs.first();
    let (armable, wait, reason) = arm_check(db, rule)?;
    Ok(WouldHave {
        rule_id: rule.rule_id,
        since_ts: since,
        dry_runs: dry,
        real_runs: real,
        files,
        bytes,
        last_eval_ts: last.map(|r| r.ts),
        last_problems: last
            .and_then(|r| r.summary["problems"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|p| p.as_str().map(String::from))
            .collect(),
        armable,
        armable_in_secs: wait,
        armable_reason: reason,
    })
}

/// May this rule be armed now? (ok, seconds to wait, reason)
pub fn arm_check(db: &Db, rule: &Rule) -> Result<(bool, i64, String)> {
    let now = Utc::now().timestamp();
    let need = preview_days(db) * 86_400;
    let ready_at = rule.created_ts + need;
    if now < ready_at {
        return Ok((
            false,
            ready_at - now,
            format!(
                "previewing: a rule can be armed {} days after it was created",
                preview_days(db)
            ),
        ));
    }
    if db.first_automation_run_ts(rule.rule_id)?.is_none() {
        return Ok((
            false,
            0,
            "no dry run recorded yet — the agent has not evaluated this rule".into(),
        ));
    }
    Ok((true, 0, String::new()))
}

/// Arm: preview/paused → armed, if the preview period has passed.
pub fn arm(db: &Db, rule_id: i64) -> Result<Rule> {
    let rule = db
        .get_automation(rule_id)?
        .with_context(|| format!("no rule #{rule_id}"))?;
    let (ok, _, why) = arm_check(db, &rule)?;
    if !ok {
        bail!("cannot arm rule #{rule_id}: {why}");
    }
    db.arm_automation(rule_id)?;
    Ok(db.get_automation(rule_id)?.expect("exists"))
}

/// Create a rule in preview state. Params are validated and normalised.
pub fn add(db: &Db, kind: &str, params: &Value) -> Result<Rule> {
    let k = RuleKind::parse(kind, params).map_err(|e| anyhow::anyhow!(e))?;
    db.add_automation(k.kind(), &k.params(), RiskTier::Tier0.as_u8())
}

/// Outcome of one tick for one rule.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TickOutcome {
    pub rule_id: i64,
    pub state: String,
    pub dry_run: bool,
    pub files: usize,
    pub txn_id: Option<String>,
    pub paused: Option<String>,
}

/// Evaluate every rule; execute the armed ones in Automate mode. Called
/// from the scheduler after analysis so duplicates/versions are fresh.
pub fn tick(adapter: &dyn OsAdapter, db: &Db) -> Result<Vec<TickOutcome>> {
    let mut out = Vec::new();
    let mode = current_mode(db);
    for rule in db.list_automations()? {
        if rule.state == "paused" {
            continue;
        }
        let (mut m, eval) = match plan(adapter, db, &rule) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(rule = rule.rule_id, error = %e, "rule plan failed");
                db.record_automation_run(
                    rule.rule_id,
                    true,
                    None,
                    &json!({"steps": []}),
                    &json!({"files": 0, "bytes": 0, "problems": [format!("plan failed: {e:#}")]}),
                )?;
                continue;
            }
        };
        let manifest_json = serde_json::to_value(&m)?;
        let live = rule.state == "armed" && mode == Mode::Automate;
        if !live || m.steps.is_empty() || !eval.problems.is_empty() {
            db.record_automation_run(
                rule.rule_id,
                true,
                None,
                &manifest_json,
                &summary_json(&eval, None),
            )?;
            let mut paused = None;
            if live && !eval.problems.is_empty() {
                let why = eval.problems.join("; ");
                db.pause_automation(rule.rule_id, &why)?;
                tracing::warn!(rule = rule.rule_id, %why, "rule paused");
                paused = Some(why);
            }
            out.push(TickOutcome {
                rule_id: rule.rule_id,
                state: if paused.is_some() {
                    "paused".into()
                } else {
                    rule.state.clone()
                },
                dry_run: true,
                files: eval.steps,
                txn_id: None,
                paused,
            });
            continue;
        }
        // armed, automate, clean plan: run it as a normal transaction
        if let Err(why) = txn::permitted(mode, &m, false) {
            db.pause_automation(rule.rule_id, &why)?;
            out.push(TickOutcome {
                rule_id: rule.rule_id,
                state: "paused".into(),
                dry_run: true,
                files: eval.steps,
                txn_id: None,
                paused: Some(why),
            });
            continue;
        }
        let rep = txn::execute(adapter, db, &mut m, CrashPoint::Never)?;
        let (_, state, states) = db.load_txn(&m.txn_id)?.context("transaction vanished")?;
        db.note_txn_effects(&m, &states)?;
        let applied = actions::Applied {
            txn_id: m.txn_id.clone(),
            done: rep.done,
            failed: rep.failed,
            state: state.as_str().to_string(),
        };
        db.record_automation_run(
            rule.rule_id,
            false,
            Some(&m.txn_id),
            &serde_json::to_value(&m)?,
            &summary_json(&eval, Some(&applied)),
        )?;
        let mut paused = None;
        if rep.failed > 0 {
            let why = format!(
                "{} of {} steps failed in {} — see History",
                rep.failed,
                m.steps.len(),
                m.txn_id
            );
            db.pause_automation(rule.rule_id, &why)?;
            tracing::warn!(rule = rule.rule_id, %why, "rule paused after failed steps");
            paused = Some(why);
        }
        tracing::info!(rule = rule.rule_id, txn = %m.txn_id, done = rep.done, failed = rep.failed, "rule executed");
        out.push(TickOutcome {
            rule_id: rule.rule_id,
            state: if paused.is_some() {
                "paused".into()
            } else {
                "armed".into()
            },
            dry_run: false,
            files: rep.done,
            txn_id: Some(m.txn_id.clone()),
            paused,
        });
    }
    db.prune_automation_runs(KEEP_DRY_RUNS_SECS)?;
    Ok(out)
}
