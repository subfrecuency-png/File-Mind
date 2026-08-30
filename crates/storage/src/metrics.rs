//! Beta counters and process sessions (Phase 10). Numbers only.

use crate::Db;
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use std::collections::BTreeMap;

pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

impl Db {
    /// Add `n` to today's counter `key`.
    pub fn bump_metric(&self, key: &str, n: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO metrics(day, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(day, key) DO UPDATE SET value = value + excluded.value",
            params![today(), key, n],
        )?;
        Ok(())
    }

    pub fn metrics_for_day(&self, day: &str) -> Result<BTreeMap<String, i64>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT key, value FROM metrics WHERE day = ?1")?;
        let rows = st.query_map([day], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record a process start. Any still-open session of the same kind is
    /// from a process that did not end cleanly — a crash or a kill — and is
    /// closed as such now.
    pub fn session_start(&self, kind: &str) -> Result<i64> {
        let now = Utc::now().timestamp();
        let crashed = self.conn.execute(
            "UPDATE sessions SET ended_ts = ?2, clean = 0 WHERE kind = ?1 AND ended_ts IS NULL",
            params![kind, now],
        )?;
        if crashed > 0 {
            self.bump_metric("crashes", crashed as i64)?;
        }
        self.conn.execute(
            "INSERT INTO sessions(kind, started_ts) VALUES (?1, ?2)",
            params![kind, now],
        )?;
        self.bump_metric("sessions", 1)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn session_end(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET ended_ts = ?2, clean = 1 WHERE session_id = ?1 AND ended_ts IS NULL",
            params![id, Utc::now().timestamp()],
        )?;
        self.bump_metric("clean_exits", 1)?;
        Ok(())
    }

    /// Sessions started on `day` and how many of them ended cleanly (or are
    /// still running, which is not a crash).
    pub fn session_stats(&self, day: &str) -> Result<(i64, i64)> {
        let (start, end) = day_bounds(day);
        Ok(self.conn.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN clean = 0 THEN 0 ELSE 1 END) FROM sessions
             WHERE started_ts >= ?1 AND started_ts < ?2",
            params![start, end],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )?)
    }

    /// Transactions executed on `day`, by initiator.
    pub fn txn_counts(&self, day: &str) -> Result<(i64, i64, i64)> {
        let (start, end) = day_bounds(day);
        let q = |sql: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(sql, params![start, end], |r| r.get(0))?)
        };
        Ok((
            q("SELECT COUNT(*) FROM transactions WHERE initiator = 'user' AND created_ts >= ?1 AND created_ts < ?2")?,
            q("SELECT COUNT(*) FROM transactions WHERE initiator = 'rule' AND created_ts >= ?1 AND created_ts < ?2")?,
            q("SELECT COUNT(*) FROM transactions WHERE state = 'undone' AND created_ts >= ?1 AND created_ts < ?2")?,
        ))
    }
}

fn day_bounds(day: &str) -> (i64, i64) {
    let d = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .unwrap_or_else(|_| Utc::now().date_naive());
    let start = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    (start, start + 86_400)
}
