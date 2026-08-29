//! The Transaction Manager — the "Protect" guarantee.
//!
//! Every mutation FileMind ever makes to the file system goes through here:
//!
//! * A [`Manifest`] lists the steps **before** anything happens. It is
//!   persisted through the [`Journal`] (durably, see `filemind_storage`) and
//!   only then executed.
//! * Each step is journaled `running` → `done`. If the process dies between
//!   those two writes, [`recover`] looks at the disk on the next start and
//!   works out what happened — a rename either took effect or it did not.
//! * There is no delete. The only way a file leaves its folder is
//!   [`OsAdapter::move_to_trash`] or [`OsAdapter::rename_no_clobber`], both of
//!   which refuse to overwrite anything.
//! * [`undo`] replays the manifest backwards, verifying the BLAKE3 recorded
//!   at planning time so a file edited after the move is never clobbered.
//!
//! The [`CrashPoint`] hook exists for the chaos test; production passes
//! [`CrashPoint::Never`].

use crate::adapter::OsAdapter;
use crate::mode::{Mode, RiskTier};
use crate::scanner::hash_file;
use crate::{CoreError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxnState {
    Planned,
    Running,
    Done,
    Undone,
    Failed,
}

impl TxnState {
    pub fn as_str(self) -> &'static str {
        match self {
            TxnState::Planned => "planned",
            TxnState::Running => "running",
            TxnState::Done => "done",
            TxnState::Undone => "undone",
            TxnState::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => TxnState::Running,
            "done" => TxnState::Done,
            "undone" => TxnState::Undone,
            "failed" => TxnState::Failed,
            _ => TxnState::Planned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    Planned,
    Running,
    Done,
    Undone,
    Failed,
    /// Recovery found the step half-applied in a way it could not settle.
    Conflict,
}

impl StepState {
    pub fn as_str(self) -> &'static str {
        match self {
            StepState::Planned => "planned",
            StepState::Running => "running",
            StepState::Done => "done",
            StepState::Undone => "undone",
            StepState::Failed => "failed",
            StepState::Conflict => "conflict",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => StepState::Running,
            "done" => StepState::Done,
            "undone" => StepState::Undone,
            "failed" => StepState::Failed,
            "conflict" => StepState::Conflict,
            _ => StepState::Planned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Initiator {
    User,
    Rule,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Step {
    /// Rename within a volume. Fails if `to` exists. `to`'s parent is created.
    Move {
        from: PathBuf,
        to: PathBuf,
        hash_before: Option<String>,
    },
    /// Move to the platform Trash. `trashed_to` is filled in by execution and
    /// is what undo restores from.
    Trash {
        path: PathBuf,
        hash_before: Option<String>,
        #[serde(default)]
        trashed_to: Option<PathBuf>,
    },
}

impl Step {
    pub fn source(&self) -> &Path {
        match self {
            Step::Move { from, .. } => from,
            Step::Trash { path, .. } => path,
        }
    }
    pub fn hash_before(&self) -> Option<&str> {
        match self {
            Step::Move { hash_before, .. } | Step::Trash { hash_before, .. } => {
                hash_before.as_deref()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub txn_id: String,
    pub mode: Mode,
    pub initiator: Initiator,
    pub rule_id: Option<String>,
    pub risk_tier: RiskTier,
    pub created: DateTime<Utc>,
    pub steps: Vec<Step>,
    pub rationale: String,
    /// Files this transaction deliberately leaves in place (e.g. the kept
    /// copy of a duplicate group). Shown in the preview so the user can judge
    /// the choice; never touched by execute/undo.
    #[serde(default)]
    pub keeps: Vec<PathBuf>,
}

impl Manifest {
    pub fn new(
        mode: Mode,
        initiator: Initiator,
        risk_tier: RiskTier,
        rationale: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            txn_id: format!(
                "txn_{}_{:04x}",
                now.format("%Y%m%dT%H%M%S"),
                (now.timestamp_subsec_nanos() >> 8) & 0xffff
            ),
            mode,
            initiator,
            rule_id: None,
            risk_tier,
            created: now,
            steps: Vec::new(),
            rationale: rationale.into(),
            keeps: Vec::new(),
        }
    }

    /// Human-readable dry-run diff shown in the approval queue.
    pub fn diff(&self) -> String {
        let mut out = String::new();
        for k in &self.keeps {
            out.push_str(&format!("     KEEP   {}\n", k.display()));
        }
        for (i, s) in self.steps.iter().enumerate() {
            let line = match s {
                Step::Move { from, to, .. } => format!(
                    "{i:>3}  MOVE   {}\n       →      {}",
                    from.display(),
                    to.display()
                ),
                Step::Trash { path, .. } => format!("{i:>3}  TRASH  {}", path.display()),
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// Persistence for manifests and step states. Implemented by `filemind_storage`.
pub trait Journal {
    /// Persist the manifest with state `Planned`. Must be durable before returning.
    fn write_manifest(&self, m: &Manifest) -> Result<()>;
    fn set_txn_state(&self, txn_id: &str, state: TxnState) -> Result<()>;
    /// Persist a step's state (and, for Trash, where it went). Must be durable.
    fn set_step_state(
        &self,
        txn_id: &str,
        step_no: usize,
        state: StepState,
        trashed_to: Option<&Path>,
    ) -> Result<()>;
    fn load(&self, txn_id: &str) -> Result<Option<(Manifest, TxnState, Vec<StepState>)>>;
    /// Transactions left in `Planned` or `Running` by a previous process.
    fn unfinished(&self) -> Result<Vec<String>>;
}

/// Where to simulate a crash (chaos testing only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    Never,
    /// Die after the step's `running` mark, before touching the disk.
    BeforeOp(usize),
    /// Die after the disk operation, before the `done` mark.
    AfterOp(usize),
}

#[derive(Debug, thiserror::Error)]
#[error("simulated crash")]
pub struct SimulatedCrash;

// ----- validation ----------------------------------------------------------

/// Check every step against the disk and the safety rules. Fills in
/// `hash_before` for steps that lack it. Returns problems; empty means safe.
pub fn validate(
    adapter: &dyn OsAdapter,
    approved_roots: &[PathBuf],
    m: &mut Manifest,
) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let protected = adapter.protected_roots();
    let inside = |p: &Path| approved_roots.iter().any(|r| p.starts_with(r));
    let is_protected = |p: &Path| protected.iter().any(|r| p.starts_with(r));
    let mut seen_targets: Vec<PathBuf> = Vec::new();

    for (i, s) in m.steps.iter_mut().enumerate() {
        let src = s.source().to_path_buf();
        if !inside(&src) {
            problems.push(format!(
                "step {i}: {} is outside every approved root",
                src.display()
            ));
        }
        if is_protected(&src) {
            problems.push(format!(
                "step {i}: {} is in a protected location",
                src.display()
            ));
        }
        match adapter.stat(&src)? {
            None => problems.push(format!("step {i}: {} no longer exists", src.display())),
            Some(e) if e.kind == crate::model::EntryKind::Link => problems.push(format!(
                "step {i}: {} is a link; links are never moved",
                src.display()
            )),
            Some(e) => {
                if e.kind == crate::model::EntryKind::File {
                    match hash_file(&src) {
                        Ok(h) => match s.hash_before() {
                            Some(prev) if prev != h => problems.push(format!(
                                "step {i}: {} changed since it was planned",
                                src.display()
                            )),
                            Some(_) => {}
                            None => match s {
                                Step::Move { hash_before, .. }
                                | Step::Trash { hash_before, .. } => *hash_before = Some(h),
                            },
                        },
                        Err(e) => {
                            problems.push(format!("step {i}: cannot read {}: {e}", src.display()))
                        }
                    }
                }
            }
        }
        if let Step::Move { to, .. } = s {
            if !inside(to) {
                problems.push(format!(
                    "step {i}: destination {} is outside every approved root",
                    to.display()
                ));
            }
            if is_protected(to) {
                problems.push(format!(
                    "step {i}: destination {} is protected",
                    to.display()
                ));
            }
            if to.exists() {
                problems.push(format!(
                    "step {i}: destination {} already exists",
                    to.display()
                ));
            }
            if seen_targets.contains(to) {
                problems.push(format!("step {i}: two steps target {}", to.display()));
            }
            if to.starts_with(&src) {
                problems.push(format!(
                    "step {i}: cannot move {} into itself",
                    src.display()
                ));
            }
            seen_targets.push(to.clone());
        }
    }
    Ok(problems)
}

/// Mode gate: may this manifest run now, given how it was initiated?
pub fn permitted(mode: Mode, m: &Manifest, approved: bool) -> std::result::Result<(), String> {
    match (mode, m.initiator) {
        (Mode::Observe, _) => Err("mode is observe: FileMind proposes but never acts. `filemind mode assist` to allow approved actions.".into()),
        (Mode::Assist, _) | (Mode::Automate, Initiator::User) => {
            if approved {
                Ok(())
            } else {
                Err("this action needs your approval".into())
            }
        }
        (Mode::Automate, Initiator::Rule) => {
            if mode.allows_unattended(m.risk_tier) || approved {
                Ok(())
            } else {
                Err(format!("rule actions of tier {:?} need approval even in automate mode", m.risk_tier))
            }
        }
    }
}

// ----- execution -----------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ExecReport {
    pub done: usize,
    pub failed: usize,
    pub conflicts: usize,
}

fn do_step(adapter: &dyn OsAdapter, s: &Step) -> Result<Option<PathBuf>> {
    match s {
        Step::Move { from, to, .. } => {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            adapter.rename_no_clobber(from, to)?;
            Ok(None)
        }
        Step::Trash {
            path, trashed_to, ..
        } => {
            let target = match trashed_to {
                Some(t) => t.clone(),
                None => adapter.trash_target(path)?,
            };
            let receipt = adapter.move_to_trash_at(path, &target)?;
            Ok(receipt.trashed_to)
        }
    }
}

/// Execute a manifest that has already been validated and permitted.
/// The manifest is persisted first; each step is journaled around its
/// disk operation. Stops at the first failure (later steps stay `planned`,
/// earlier ones stay done and remain undoable).
pub fn execute(
    adapter: &dyn OsAdapter,
    journal: &dyn Journal,
    m: &mut Manifest,
    crash: CrashPoint,
) -> Result<ExecReport> {
    journal.write_manifest(m)?;
    journal.set_txn_state(&m.txn_id, TxnState::Running)?;
    let mut rep = ExecReport::default();
    for i in 0..m.steps.len() {
        // For a Trash step the destination is decided and journaled *before*
        // the move, so recovery can tell "moved" from "never started".
        let planned_target = match &mut m.steps[i] {
            Step::Trash {
                path, trashed_to, ..
            } => {
                let t = adapter.trash_target(path)?;
                *trashed_to = Some(t.clone());
                Some(t)
            }
            Step::Move { .. } => None,
        };
        journal.set_step_state(&m.txn_id, i, StepState::Running, planned_target.as_deref())?;
        if crash == CrashPoint::BeforeOp(i) {
            return Err(CoreError::Other(SimulatedCrash.into()));
        }
        match do_step(adapter, &m.steps[i]) {
            Ok(trashed_to) => {
                if let Step::Trash {
                    trashed_to: slot, ..
                } = &mut m.steps[i]
                {
                    *slot = trashed_to.clone();
                }
                if crash == CrashPoint::AfterOp(i) {
                    return Err(CoreError::Other(SimulatedCrash.into()));
                }
                journal.set_step_state(&m.txn_id, i, StepState::Done, trashed_to.as_deref())?;
                rep.done += 1;
            }
            Err(e) => {
                journal.set_step_state(&m.txn_id, i, StepState::Failed, None)?;
                journal.set_txn_state(&m.txn_id, TxnState::Failed)?;
                rep.failed += 1;
                tracing::warn!(txn = %m.txn_id, step = i, error = %e, "step failed; stopping");
                return Ok(rep);
            }
        }
    }
    journal.set_txn_state(&m.txn_id, TxnState::Done)?;
    Ok(rep)
}

// ----- recovery ------------------------------------------------------------

/// Settle a transaction left unfinished by a crash. Steps marked `running`
/// are resolved by looking at the disk; `planned` steps are **not** resumed
/// (the user did not get to see a partial result — they get a clear state
/// and can re-approve). Returns the report for what was settled.
pub fn recover(adapter: &dyn OsAdapter, journal: &dyn Journal, txn_id: &str) -> Result<ExecReport> {
    let Some((m, state, steps)) = journal.load(txn_id)? else {
        return Ok(ExecReport::default());
    };
    let mut rep = ExecReport::default();
    if !matches!(state, TxnState::Planned | TxnState::Running) {
        return Ok(rep);
    }
    for (i, st) in steps.iter().enumerate() {
        if *st != StepState::Running {
            if *st == StepState::Done {
                rep.done += 1;
            }
            continue;
        }
        let settled = match &m.steps[i] {
            Step::Move { from, to, .. } => match (adapter.stat(from)?, adapter.stat(to)?) {
                (None, Some(_)) => StepState::Done,        // rename happened
                (Some(_), None) => StepState::Planned,     // never happened; leave for re-approval
                (Some(_), Some(_)) => StepState::Conflict, // both exist: never touch either
                (None, None) => StepState::Conflict,       // vanished: investigate, never guess
            },
            Step::Trash {
                path, trashed_to, ..
            } => {
                let at_target = match trashed_to {
                    Some(t) => adapter.stat(t)?.is_some(),
                    None => false,
                };
                match (adapter.stat(path)?, at_target) {
                    (None, true) => StepState::Done,
                    (Some(_), false) => StepState::Planned,
                    (Some(_), true) => StepState::Conflict,
                    (None, false) => StepState::Conflict, // left, but not where we said
                }
            }
        };
        journal.set_step_state(txn_id, i, settled, None)?;
        match settled {
            StepState::Done => rep.done += 1,
            StepState::Conflict => rep.conflicts += 1,
            _ => {}
        }
    }
    let final_state = if rep.conflicts > 0 {
        TxnState::Failed
    } else if rep.done == m.steps.len() {
        TxnState::Done
    } else {
        TxnState::Failed // partial: earlier done steps remain undoable
    };
    journal.set_txn_state(txn_id, final_state)?;
    Ok(rep)
}

// ----- undo ----------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct UndoReport {
    pub restored: usize,
    pub skipped: Vec<String>,
}

/// Reverse every `done` step, last first. A step whose result was modified
/// after the transaction (hash mismatch) or whose original location is now
/// occupied is skipped and reported — never overwritten.
pub fn undo(adapter: &dyn OsAdapter, journal: &dyn Journal, txn_id: &str) -> Result<UndoReport> {
    let Some((m, state, steps)) = journal.load(txn_id)? else {
        return Err(CoreError::Other(anyhow::anyhow!(
            "unknown transaction {txn_id}"
        )));
    };
    if matches!(state, TxnState::Running | TxnState::Planned) {
        return Err(CoreError::Other(anyhow::anyhow!(
            "{txn_id} is still in progress; recover it first"
        )));
    }
    let mut rep = UndoReport::default();
    for i in (0..m.steps.len()).rev() {
        if steps[i] != StepState::Done {
            continue;
        }
        let (current, original, hash) = match &m.steps[i] {
            Step::Move {
                from,
                to,
                hash_before,
            } => (to.clone(), from.clone(), hash_before.clone()),
            Step::Trash {
                path,
                hash_before,
                trashed_to,
            } => match trashed_to {
                Some(t) => (t.clone(), path.clone(), hash_before.clone()),
                None => {
                    rep.skipped.push(format!(
                        "step {i}: {} — no Trash location recorded",
                        path.display()
                    ));
                    continue;
                }
            },
        };
        if adapter.stat(&current)?.is_none() {
            rep.skipped.push(format!(
                "step {i}: {} is no longer there",
                current.display()
            ));
            continue;
        }
        if let Some(h) = &hash {
            if current.is_file() {
                match hash_file(&current) {
                    Ok(now) if now != *h => {
                        rep.skipped.push(format!(
                            "step {i}: {} was modified after the move; left in place",
                            current.display()
                        ));
                        continue;
                    }
                    Err(e) => {
                        rep.skipped
                            .push(format!("step {i}: cannot read {}: {e}", current.display()));
                        continue;
                    }
                    _ => {}
                }
            }
        }
        if original.exists() {
            rep.skipped.push(format!(
                "step {i}: {} is occupied now; left in place",
                original.display()
            ));
            continue;
        }
        if let Some(parent) = original.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match adapter.rename_no_clobber(&current, &original) {
            Ok(()) => {
                journal.set_step_state(txn_id, i, StepState::Undone, None)?;
                rep.restored += 1;
            }
            Err(e) => rep.skipped.push(format!("step {i}: {e}")),
        }
    }
    if rep.skipped.is_empty() {
        journal.set_txn_state(txn_id, TxnState::Undone)?;
    }
    Ok(rep)
}
