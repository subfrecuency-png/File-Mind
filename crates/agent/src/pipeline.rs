//! Scan pipeline: OsAdapter → Scanner → batched upserts → missing sweep,
//! and the throttled content-hashing pass.

use anyhow::{Context, Result};
use chrono::Utc;
use filemind_core::adapter::Entry;
use filemind_core::scanner::hash_file;
use filemind_core::{OsAdapter, ScanOpts, ScanReport, Scanner};
use filemind_storage::{Db, UpsertStats};
use std::path::Path;
use std::time::{Duration, Instant};

const BATCH: usize = 2_000;

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanOutcome {
    pub report: ScanReportCopy,
    pub upsert: UpsertStats,
    pub missing: u64,
    pub elapsed_ms: u128,
}

/// `ScanReport` is not `Copy`; mirror the fields we display.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanReportCopy {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub links_skipped: u64,
    pub protected_skipped: u64,
    pub ignored: u64,
    pub errors: u64,
}

impl From<&ScanReport> for ScanReportCopy {
    fn from(r: &ScanReport) -> Self {
        Self {
            files: r.files,
            dirs: r.dirs,
            bytes: r.bytes,
            links_skipped: r.links_skipped,
            protected_skipped: r.protected_skipped,
            ignored: r.ignored,
            errors: r.errors,
        }
    }
}

fn merge(a: &mut UpsertStats, b: UpsertStats) {
    a.inserted += b.inserted;
    a.updated += b.updated;
    a.renamed += b.renamed;
    a.moved += b.moved;
    a.unchanged += b.unchanged;
}

/// Full metadata scan of one approved root, persisted. Read-only on the file system.
pub fn scan_root(adapter: &dyn OsAdapter, db: &Db, root: &Path) -> Result<ScanOutcome> {
    let root = root
        .canonicalize()
        .with_context(|| format!("root {}", root.display()))?;
    let started = Instant::now();
    let scan_ts = Utc::now().timestamp();
    let root_row = db.add_root(&root)?;
    let seq = db.begin_scan(root_row.root_id)?;
    let roots: Vec<_> = db.list_roots()?.into_iter().map(|r| r.path).collect();
    let scanner = Scanner::new(adapter, roots);

    let mut batch: Vec<Entry> = Vec::with_capacity(BATCH);
    let mut upsert = UpsertStats::default();
    let mut flush_err: Option<anyhow::Error> = None;

    let report = scanner.scan_root(&root, &ScanOpts::default(), |e| {
        batch.push(e.clone());
        if batch.len() >= BATCH && flush_err.is_none() {
            match db.upsert_entries(root_row.root_id, seq, &batch, scan_ts) {
                Ok(s) => merge(&mut upsert, s),
                Err(e) => flush_err = Some(e),
            }
            batch.clear();
        }
    })?;
    if let Some(e) = flush_err {
        return Err(e);
    }
    if !batch.is_empty() {
        merge(
            &mut upsert,
            db.upsert_entries(root_row.root_id, seq, &batch, scan_ts)?,
        );
    }
    let missing = db.mark_missing(root_row.root_id, seq, scan_ts)?;

    Ok(ScanOutcome {
        report: (&report).into(),
        upsert,
        missing,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Scan every registered root.
pub fn scan_all(
    adapter: &dyn OsAdapter,
    db: &Db,
) -> Result<Vec<(std::path::PathBuf, ScanOutcome)>> {
    let mut out = Vec::new();
    for r in db.list_roots()? {
        match scan_root(adapter, db, &r.path) {
            Ok(o) => out.push((r.path, o)),
            Err(e) => tracing::warn!(root = %r.path.display(), error = %e, "scan failed"),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
pub struct HashOpts {
    /// Fraction of wall time the hasher may keep busy (0.0–1.0). 0.2 = "≤ 20 % of one core".
    pub duty_cycle: f32,
    /// Stop after this many files (0 = no limit).
    pub max_files: usize,
    /// Stop after this much wall time.
    pub max_wall: Option<Duration>,
}

impl Default for HashOpts {
    fn default() -> Self {
        Self {
            duty_cycle: 0.2,
            max_files: 0,
            max_wall: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HashOutcome {
    pub hashed: u64,
    pub bytes: u64,
    pub errors: u64,
    pub remaining: u64,
    pub elapsed_ms: u128,
}

/// Hash files that have no content hash yet, smallest first. Results are
/// written in batches (one SQLite transaction per batch) and the loop sleeps
/// between batches so busy time stays around `duty_cycle` of wall time.
pub fn hash_pending(db: &Db, opts: HashOpts) -> Result<HashOutcome> {
    let started = Instant::now();
    let mut out = HashOutcome::default();
    let duty = opts.duty_cycle.clamp(0.05, 1.0);
    let mut busy = Duration::ZERO;
    // A batch ends after this many files or this many bytes, whichever first,
    // so big files still get throttled between them.
    const BATCH_FILES: usize = 1_000;
    const BATCH_BYTES: u64 = 256 << 20;

    'outer: loop {
        let pending = db.files_without_hash(BATCH_FILES)?;
        if pending.is_empty() {
            break;
        }
        let t = Instant::now();
        let mut results: Vec<(String, String, u64)> = Vec::with_capacity(pending.len());
        let mut batch_bytes = 0u64;
        let mut batch_ok = 0u64;
        for (file_id, path, size) in pending {
            if opts.max_files > 0 && out.hashed as usize + results.len() >= opts.max_files {
                break;
            }
            if let Some(w) = opts.max_wall {
                if started.elapsed() >= w {
                    break;
                }
            }
            match hash_file(&path) {
                Ok(h) => {
                    out.bytes += size;
                    batch_ok += 1;
                    results.push((file_id, h, size));
                }
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "hash failed");
                    // Sentinel so the file is not retried every pass; a proper
                    // hash_error column replaces this later.
                    results.push((file_id.clone(), format!("err:{file_id}"), size));
                    out.errors += 1;
                }
            }
            batch_bytes += size;
            if batch_bytes >= BATCH_BYTES {
                break;
            }
        }
        let n = results.len();
        let stop = n == 0
            || (opts.max_files > 0 && out.hashed as usize + n >= opts.max_files)
            || opts.max_wall.is_some_and(|w| started.elapsed() >= w);
        db.set_file_hashes(&results)?;
        out.hashed += batch_ok;
        busy += t.elapsed();
        if stop {
            break 'outer;
        }
        // Sleep so that busy / (busy + idle) ≈ duty.
        let target_wall = busy.mul_f32(1.0 / duty);
        let wall = started.elapsed();
        if target_wall > wall {
            std::thread::sleep((target_wall - wall).min(Duration::from_secs(5)));
        }
    }
    out.remaining = db.count_without_hash()?;
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}
