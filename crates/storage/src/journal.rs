//! Durable transaction journal on SQLite.
//!
//! Manifest and step writes run with `synchronous = FULL` so a power loss
//! right after a write cannot lose it (the rest of the database runs at
//! NORMAL, which already survives process crashes).

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use filemind_core::txn::{Journal, Manifest, Step, StepState, TxnState};
use filemind_core::{CoreError, Result as CoreResult};
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

fn core_err<E: std::fmt::Display>(e: E) -> CoreError {
    CoreError::Other(anyhow::anyhow!("{e}"))
}

impl Db {
    fn durable<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        let r = f();
        let _ = self.conn.pragma_update(None, "synchronous", "NORMAL");
        r
    }

    /// Manifest + step rows for one transaction, if it exists.
    pub fn load_txn(&self, txn_id: &str) -> Result<Option<(Manifest, TxnState, Vec<StepState>)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT manifest, state FROM transactions WHERE txn_id = ?1",
                [txn_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((manifest_json, state)) = row else {
            return Ok(None);
        };
        let mut m: Manifest = serde_json::from_str(&manifest_json)?;
        let mut st = self.conn.prepare(
            "SELECT step_no, state, to_path FROM txn_steps WHERE txn_id = ?1 ORDER BY step_no",
        )?;
        let mut states = vec![StepState::Planned; m.steps.len()];
        for row in st.query_map([txn_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (no, s, to) = row?;
            let i = no as usize;
            if i < states.len() {
                states[i] = StepState::parse(&s);
                // for Trash steps the journal knows where the file went
                if let (Step::Trash { trashed_to, .. }, Some(t)) = (&mut m.steps[i], to) {
                    if trashed_to.is_none() {
                        *trashed_to = Some(PathBuf::from(t));
                    }
                }
            }
        }
        Ok(Some((m, TxnState::parse(&state), states)))
    }

    pub fn list_txns(&self, limit: usize) -> Result<Vec<TxnSummary>> {
        let mut st = self.conn.prepare(
            "SELECT t.txn_id, t.state, t.created_ts, t.executed_ts, t.manifest,
                    (SELECT COUNT(*) FROM txn_steps s WHERE s.txn_id = t.txn_id AND s.state = 'done') AS done,
                    (SELECT COUNT(*) FROM txn_steps s WHERE s.txn_id = t.txn_id) AS total
             FROM transactions t ORDER BY t.created_ts DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            let manifest: String = r.get(4)?;
            let m: Manifest = serde_json::from_str(&manifest).unwrap_or_else(|_| {
                Manifest::new(
                    filemind_core::Mode::Observe,
                    filemind_core::txn::Initiator::User,
                    filemind_core::RiskTier::Tier2,
                    "",
                )
            });
            Ok(TxnSummary {
                txn_id: r.get(0)?,
                state: r.get(1)?,
                created_ts: r.get(2)?,
                executed_ts: r.get(3)?,
                rationale: m.rationale,
                steps: r.get::<_, i64>(6)? as usize,
                done: r.get::<_, i64>(5)? as usize,
                initiator: match m.initiator {
                    filemind_core::txn::Initiator::User => "user".into(),
                    filemind_core::txn::Initiator::Rule => "rule".into(),
                },
                rule_id: m.rule_id,
                trash_only: !m.steps.is_empty()
                    && m.steps
                        .iter()
                        .all(|s| matches!(s, filemind_core::txn::Step::Trash { .. })),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Mark inventory rows for a finished transaction: moved files get their
    /// new path reconciled by the watcher/scan; trashed ones are marked here.
    pub fn note_txn_effects(&self, m: &Manifest, states: &[StepState]) -> Result<()> {
        let now = Utc::now().timestamp();
        let tx = self.conn.unchecked_transaction()?;
        for (i, s) in m.steps.iter().enumerate() {
            if states.get(i) != Some(&StepState::Done) {
                continue;
            }
            match s {
                Step::Trash { path, .. } => {
                    tx.execute(
                        "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                         SELECT file_id, ?2, 'deleted', path, NULL, 'txn' FROM files WHERE path = ?1 AND status = 'present'",
                        params![path.to_string_lossy(), now],
                    )?;
                    tx.execute(
                        "UPDATE files SET status = 'trashed' WHERE path = ?1 AND status = 'present'",
                        [path.to_string_lossy()],
                    )?;
                    // a trashed folder takes everything under it along
                    let prefix = format!("{}/%", path.to_string_lossy());
                    tx.execute(
                        "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                         SELECT file_id, ?2, 'deleted', path, NULL, 'txn' FROM files WHERE path LIKE ?1 AND status = 'present'",
                        params![prefix, now],
                    )?;
                    tx.execute(
                        "UPDATE files SET status = 'trashed' WHERE path LIKE ?1 AND status = 'present'",
                        [prefix],
                    )?;
                }
                Step::Move { from, to, .. } => {
                    tx.execute(
                        "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                         SELECT file_id, ?3, 'moved', path, ?2, 'txn' FROM files WHERE path = ?1 AND status = 'present'",
                        params![from.to_string_lossy(), to.to_string_lossy(), now],
                    )?;
                    tx.execute(
                        "UPDATE files SET path = ?2, name = ?3 WHERE path = ?1 AND status = 'present'",
                        params![
                            from.to_string_lossy(),
                            to.to_string_lossy(),
                            to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TxnSummary {
    pub txn_id: String,
    pub state: String,
    pub created_ts: i64,
    pub executed_ts: Option<i64>,
    pub rationale: String,
    pub steps: usize,
    pub done: usize,
    /// "user" or "rule"; `rule_id` names the suggestion or rule behind it.
    pub initiator: String,
    pub rule_id: Option<String>,
    /// Every step is a Trash: undo is Finder's "Put Back".
    pub trash_only: bool,
}

impl Journal for Db {
    fn write_manifest(&self, m: &Manifest) -> CoreResult<()> {
        self.durable(|| {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO transactions(txn_id, mode, initiator, rule_id, state, created_ts, manifest)
                 VALUES (?1, ?2, ?3, ?4, 'planned', ?5, ?6)
                 ON CONFLICT(txn_id) DO UPDATE SET manifest = excluded.manifest",
                params![
                    m.txn_id,
                    serde_json::to_value(m.mode)?.as_str().unwrap_or("observe"),
                    serde_json::to_value(m.initiator)?.as_str().unwrap_or("user"),
                    m.rule_id,
                    m.created.timestamp(),
                    serde_json::to_string(m)?
                ],
            )?;
            tx.execute("DELETE FROM txn_steps WHERE txn_id = ?1", [&m.txn_id])?;
            let mut ins = tx.prepare_cached(
                "INSERT INTO txn_steps(txn_id, step_no, file_id, from_path, to_path, hash_before, state)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, 'planned')",
            )?;
            for (i, s) in m.steps.iter().enumerate() {
                let (from, to) = match s {
                    Step::Move { from, to, .. } => (from.to_string_lossy().to_string(), Some(to.to_string_lossy().to_string())),
                    Step::Trash { path, .. } => (path.to_string_lossy().to_string(), None),
                };
                ins.execute(params![m.txn_id, i as i64, from, to, s.hash_before()])?;
            }
            drop(ins);
            tx.commit()?;
            Ok(())
        })
        .map_err(core_err)
    }

    fn set_txn_state(&self, txn_id: &str, state: TxnState) -> CoreResult<()> {
        self.durable(|| {
            let executed = if matches!(state, TxnState::Done | TxnState::Failed) {
                Some(Utc::now().timestamp())
            } else {
                None
            };
            self.conn.execute(
                "UPDATE transactions SET state = ?2, executed_ts = COALESCE(?3, executed_ts) WHERE txn_id = ?1",
                params![txn_id, state.as_str(), executed],
            )?;
            Ok(())
        })
        .map_err(core_err)
    }

    fn set_step_state(
        &self,
        txn_id: &str,
        step_no: usize,
        state: StepState,
        trashed_to: Option<&Path>,
    ) -> CoreResult<()> {
        self.durable(|| {
            self.conn.execute(
                "UPDATE txn_steps SET state = ?3, to_path = COALESCE(?4, to_path) WHERE txn_id = ?1 AND step_no = ?2",
                params![txn_id, step_no as i64, state.as_str(), trashed_to.map(|p| p.to_string_lossy().to_string())],
            )?;
            Ok(())
        })
        .map_err(core_err)
    }

    fn load(&self, txn_id: &str) -> CoreResult<Option<(Manifest, TxnState, Vec<StepState>)>> {
        self.load_txn(txn_id).map_err(core_err)
    }

    fn unfinished(&self) -> CoreResult<Vec<String>> {
        (|| -> Result<Vec<String>> {
            let mut st = self.conn.prepare(
                "SELECT txn_id FROM transactions WHERE state IN ('planned','running') ORDER BY created_ts",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })()
        .map_err(core_err)
    }
}
