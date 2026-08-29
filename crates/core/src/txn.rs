//! Transaction manifests (Phase 6 will add execution, journaling and undo).
//!
//! A manifest is written and fsynced *before* any step executes. Every step
//! records the content hash at planning time so undo can verify nothing
//! changed underneath us.

use crate::mode::Mode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxnState {
    Planned,
    Running,
    Done,
    Undone,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Initiator {
    User,
    Rule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Step {
    Move {
        from: PathBuf,
        to: PathBuf,
        hash_before: Option<String>,
    },
    Trash {
        path: PathBuf,
        hash_before: Option<String>,
    },
    Restore {
        receipt_path: PathBuf,
        to: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub txn_id: String,
    pub mode: Mode,
    pub initiator: Initiator,
    pub rule_id: Option<String>,
    pub created: DateTime<Utc>,
    pub steps: Vec<Step>,
    pub rationale: String,
}

impl Manifest {
    pub fn new(mode: Mode, initiator: Initiator, rationale: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            txn_id: format!("txn_{}", now.format("%Y%m%dT%H%M%S%3f")),
            mode,
            initiator,
            rule_id: None,
            created: now,
            steps: Vec::new(),
            rationale: rationale.into(),
        }
    }

    /// Human-readable dry-run diff shown in the approval queue.
    pub fn diff(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.steps.iter().enumerate() {
            let line = match s {
                Step::Move { from, to, .. } => {
                    format!("{i:>3}  MOVE   {} -> {}", from.display(), to.display())
                }
                Step::Trash { path, .. } => format!("{i:>3}  TRASH  {}", path.display()),
                Step::Restore { to, .. } => format!("{i:>3}  RESTORE -> {}", to.display()),
            };
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}
