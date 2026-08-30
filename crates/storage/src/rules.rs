//! Automate-mode rules ("automations" in method names, since `*_rule` is
//! taken by the classification rules) and their run log (Phase 9).

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::Db;

#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub rule_id: i64,
    pub kind: String,
    pub params: Value,
    pub tier: u8,
    /// preview | armed | paused
    pub state: String,
    pub created_ts: i64,
    pub armed_ts: Option<i64>,
    pub paused_ts: Option<i64>,
    pub paused_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuleRun {
    pub run_id: i64,
    pub rule_id: i64,
    pub ts: i64,
    pub dry_run: bool,
    pub txn_id: Option<String>,
    pub manifest: Value,
    pub summary: Value,
}

/// One exact-duplicate copy the rule planner may consider: the kept file
/// and one of its copies, both present, neither sensitive.
#[derive(Debug, Clone)]
pub struct DupCopy {
    pub blob_id: i64,
    pub size: u64,
    pub keeper: PathBuf,
    pub copy: PathBuf,
    pub copy_mtime: i64,
}

/// A version chain as stored by the last analysis: newest last.
#[derive(Debug, Clone)]
pub struct ChainMembers {
    pub chain_id: i64,
    /// (path, mtime) in ordinal order; the last one is the canonical file.
    pub members: Vec<(PathBuf, i64)>,
}

fn row_rule(r: &rusqlite::Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        rule_id: r.get(0)?,
        kind: r.get(1)?,
        params: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
        tier: r.get::<_, i64>(3)? as u8,
        state: r.get(4)?,
        created_ts: r.get(5)?,
        armed_ts: r.get(6)?,
        paused_ts: r.get(7)?,
        paused_reason: r.get(8)?,
    })
}

const RULE_COLS: &str =
    "rule_id, kind, params, tier, state, created_ts, armed_ts, paused_ts, paused_reason";

impl Db {
    pub fn add_automation(&self, kind: &str, params: &Value, tier: u8) -> Result<Rule> {
        let now = Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO rules(kind, params, tier, state, created_ts) VALUES (?1, ?2, ?3, 'preview', ?4)",
            params![kind, params.to_string(), tier as i64, now],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(self.get_automation(id)?.expect("just inserted"))
    }

    pub fn get_automation(&self, id: i64) -> Result<Option<Rule>> {
        let mut st = self
            .conn
            .prepare_cached(&format!("SELECT {RULE_COLS} FROM rules WHERE rule_id = ?1"))?;
        let mut rows = st.query([id])?;
        Ok(match rows.next()? {
            Some(r) => Some(row_rule(r)?),
            None => None,
        })
    }

    pub fn list_automations(&self) -> Result<Vec<Rule>> {
        let mut st = self
            .conn
            .prepare_cached(&format!("SELECT {RULE_COLS} FROM rules ORDER BY rule_id"))?;
        let rows = st.query_map([], row_rule)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_automation_params(&self, id: i64, params: &Value) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE rules SET params = ?2 WHERE rule_id = ?1",
            params![id, params.to_string()],
        )? == 1)
    }

    pub fn arm_automation(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE rules SET state = 'armed', armed_ts = ?2, paused_ts = NULL, paused_reason = NULL WHERE rule_id = ?1",
            params![id, Utc::now().timestamp()],
        )? == 1)
    }

    pub fn pause_automation(&self, id: i64, reason: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE rules SET state = 'paused', paused_ts = ?2, paused_reason = ?3 WHERE rule_id = ?1",
            params![id, Utc::now().timestamp(), reason],
        )? == 1)
    }

    /// Back to preview (keeps its history, so the 7-day clock does not restart).
    pub fn unarm_automation(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE rules SET state = 'preview', armed_ts = NULL, paused_ts = NULL, paused_reason = NULL WHERE rule_id = ?1",
            [id],
        )? == 1)
    }

    /// Remove a rule and its run log. Rows only — no file is touched.
    pub fn remove_automation(&self, id: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM rule_runs WHERE rule_id = ?1", [id])?;
        let n = tx.execute("DELETE FROM rules WHERE rule_id = ?1", [id])?;
        tx.commit()?;
        Ok(n == 1)
    }

    pub fn record_automation_run(
        &self,
        rule_id: i64,
        dry_run: bool,
        txn_id: Option<&str>,
        manifest: &Value,
        summary: &Value,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO rule_runs(rule_id, ts, dry_run, txn_id, manifest, summary) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rule_id,
                Utc::now().timestamp(),
                dry_run as i64,
                txn_id,
                manifest.to_string(),
                summary.to_string()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Runs for one rule since `since` (unix seconds), newest first.
    pub fn list_automation_runs(
        &self,
        rule_id: i64,
        since: i64,
        limit: usize,
    ) -> Result<Vec<RuleRun>> {
        let mut st = self.conn.prepare_cached(
            "SELECT run_id, rule_id, ts, dry_run, txn_id, manifest, summary FROM rule_runs
             WHERE rule_id = ?1 AND ts >= ?2 ORDER BY ts DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![rule_id, since, limit as i64], |r| {
            Ok(RuleRun {
                run_id: r.get(0)?,
                rule_id: r.get(1)?,
                ts: r.get(2)?,
                dry_run: r.get::<_, i64>(3)? != 0,
                txn_id: r.get(4)?,
                manifest: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                summary: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Timestamp of the first run recorded for a rule (the preview clock
    /// starts at creation, but this proves the rule has actually been evaluated).
    pub fn first_automation_run_ts(&self, rule_id: i64) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(ts) FROM rule_runs WHERE rule_id = ?1",
            [rule_id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    /// Keep the log bounded: drop dry runs older than `keep_secs`. Real runs
    /// (with a transaction) are kept — they are history.
    pub fn prune_automation_runs(&self, keep_secs: i64) -> Result<usize> {
        let cut = Utc::now().timestamp() - keep_secs;
        Ok(self
            .conn
            .execute("DELETE FROM rule_runs WHERE dry_run = 1 AND ts < ?1", [cut])?)
    }

    /// Exact-duplicate copies from the last analysis, largest first.
    pub fn dup_copies(&self, min_bytes: u64) -> Result<Vec<DupCopy>> {
        let mut st = self.conn.prepare(
            "SELECT g.blob_id, k.size, k.path, f.path, f.mtime
             FROM duplicate_groups g
             JOIN files k ON k.file_id = g.keeper_file_id AND k.status = 'present' AND k.sensitive_kind IS NULL
             JOIN files f ON f.blob_id = g.blob_id AND f.file_id != k.file_id
                          AND f.status = 'present' AND f.kind = 'file' AND f.sensitive_kind IS NULL
             WHERE k.size >= ?1
             ORDER BY k.size DESC, f.path",
        )?;
        let rows = st.query_map([min_bytes as i64], |r| {
            Ok(DupCopy {
                blob_id: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
                keeper: PathBuf::from(r.get::<_, String>(2)?),
                copy: PathBuf::from(r.get::<_, String>(3)?),
                copy_mtime: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Version chains from the last analysis with their members in order.
    pub fn version_chain_members(&self) -> Result<Vec<ChainMembers>> {
        let mut st = self.conn.prepare(
            "SELECT m.chain_id, f.path, f.mtime FROM version_members m
             JOIN files f ON f.file_id = m.file_id AND f.status = 'present' AND f.sensitive_kind IS NULL
             ORDER BY m.chain_id, m.ordinal",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                PathBuf::from(r.get::<_, String>(1)?),
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out: Vec<ChainMembers> = Vec::new();
        for row in rows {
            let (chain_id, path, mtime) = row?;
            match out.last_mut() {
                Some(c) if c.chain_id == chain_id => c.members.push((path, mtime)),
                _ => out.push(ChainMembers {
                    chain_id,
                    members: vec![(path, mtime)],
                }),
            }
        }
        Ok(out)
    }

    /// Present, non-sensitive files under a Downloads folder inside `root`
    /// untouched since `cut`, oldest first.
    pub fn stale_downloads(
        &self,
        root: &std::path::Path,
        cut: i64,
    ) -> Result<Vec<(PathBuf, i64, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT path, mtime, size FROM files WHERE kind='file' AND status='present' AND sensitive_kind IS NULL
             AND path LIKE ?1 AND lower(path) LIKE '%/downloads/%' AND mtime < ?2
             AND path NOT LIKE '%/node_modules/%' AND path NOT LIKE '%/.git/%' ORDER BY mtime",
        )?;
        let rows = st.query_map(params![format!("{}/%", root.display()), cut], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}
