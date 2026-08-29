//! Minimal periodic scheduler for the daemon. Phase 2 replaces the timer
//! with the file-system watcher; the periodic full scan stays as a safety net.

use crate::pipeline::{self, HashOpts};
use anyhow::Result;
use filemind_core::OsAdapter;
use filemind_storage::Db;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    pub scan_every: Duration,
    /// Time budget for the hashing pass after each scan.
    pub hash_budget: Duration,
    pub hash_duty_cycle: f32,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            scan_every: Duration::from_secs(30 * 60),
            hash_budget: Duration::from_secs(10 * 60),
            hash_duty_cycle: 0.2,
        }
    }
}

pub fn schedule_from_settings(db: &Db) -> Schedule {
    let mut s = Schedule::default();
    if let Ok(Some(v)) = db.get_setting("scan_interval_min") {
        if let Some(m) = v.as_u64() {
            s.scan_every = Duration::from_secs(m.max(1) * 60);
        }
    }
    if let Ok(Some(v)) = db.get_setting("hash_duty_cycle") {
        if let Some(d) = v.as_f64() {
            s.hash_duty_cycle = d as f32;
        }
    }
    s
}

/// One tick: scan every root, then hash within budget. Returns a one-line summary.
pub fn tick(adapter: &dyn OsAdapter, db: &Db, sched: Schedule) -> Result<String> {
    let scans = pipeline::scan_all(adapter, db)?;
    let mut files = 0;
    let mut changed = 0;
    for (_, o) in &scans {
        files += o.report.files;
        changed +=
            o.upsert.inserted + o.upsert.updated + o.upsert.renamed + o.upsert.moved + o.missing;
    }
    let h = pipeline::hash_pending(
        db,
        HashOpts {
            duty_cycle: sched.hash_duty_cycle,
            max_files: 0,
            max_wall: Some(sched.hash_budget),
        },
    )?;
    let c = crate::classifier::classify_pending(
        db,
        crate::classifier::ClassifyOpts {
            duty_cycle: sched.hash_duty_cycle,
            max_wall: Some(sched.hash_budget),
            names_only: false,
        },
    )?;
    let a = crate::analysis::run(db)?;
    Ok(format!(
        "roots {}  files {}  changes {}  hashed {} (+{} pending)  classified {} (+{} pending, {} sensitive)  health {}  dup groups {} ({})  version chains {}  suggestions {}",
        scans.len(),
        files,
        changed,
        h.hashed,
        h.remaining,
        c.classified,
        c.remaining,
        c.sensitive,
        a.health.score,
        a.duplicate_groups,
        filemind_core::health::human(a.duplicate_bytes),
        a.version_chains,
        a.suggestions
    ))
}

/// Run forever. `once = true` runs a single tick and returns.
pub fn run(adapter: &dyn OsAdapter, db: &Db, once: bool) -> Result<()> {
    loop {
        let sched = schedule_from_settings(db);
        match tick(adapter, db, sched) {
            Ok(summary) => tracing::info!(%summary, "tick"),
            Err(e) => tracing::error!(error = %e, "tick failed"),
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(sched.scan_every);
    }
}
