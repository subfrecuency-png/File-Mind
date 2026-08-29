//! Incremental index updates driven by watcher events.
//!
//! Each [`Change`] is resolved against the disk (never trusted blindly):
//! we stat the path and upsert what is actually there, or mark it missing.
//! New or renamed directories are re-enumerated through the same `Scanner`
//! rules as a full scan, so protected roots and links are handled identically.

use anyhow::Result;
use chrono::Utc;
use filemind_core::adapter::Entry;
use filemind_core::model::EntryKind;
use filemind_core::watch::Change;
use filemind_core::{OsAdapter, ScanOpts, Scanner};
use filemind_storage::Db;
use std::path::Path;

pub const SOURCE: &str = "watcher";

#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyStats {
    pub upserted: u64,
    pub missing: u64,
    pub rescans: u64,
    pub ignored: u64,
}

fn upsert_one(db: &Db, entry: &Entry, ts: i64) -> Result<bool> {
    let Some(root) = db.root_for_path(&entry.path)? else {
        return Ok(false);
    };
    let seq = db.current_seq(root.root_id)?;
    db.upsert_entries_from(root.root_id, seq, std::slice::from_ref(entry), ts, SOURCE)?;
    Ok(true)
}

fn upsert_subtree(adapter: &dyn OsAdapter, db: &Db, dir: &Path, ts: i64) -> Result<u64> {
    let Some(root) = db.root_for_path(dir)? else {
        return Ok(0);
    };
    let seq = db.current_seq(root.root_id)?;
    let roots: Vec<_> = db.list_roots()?.into_iter().map(|r| r.path).collect();
    let scanner = Scanner::new(adapter, roots);
    let mut batch: Vec<Entry> = Vec::new();
    let mut n = 0u64;
    let mut err: Option<anyhow::Error> = None;
    scanner.scan_root(dir, &ScanOpts::default(), |e| {
        batch.push(e.clone());
        if batch.len() >= 1000 && err.is_none() {
            if let Err(e) = db.upsert_entries_from(root.root_id, seq, &batch, ts, SOURCE) {
                err = Some(e);
            }
            n += batch.len() as u64;
            batch.clear();
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    if !batch.is_empty() {
        db.upsert_entries_from(root.root_id, seq, &batch, ts, SOURCE)?;
        n += batch.len() as u64;
    }
    Ok(n)
}

/// Resolve one path against disk: upsert what exists (recursing into new
/// directories) or mark it missing. Returns true if the path is inside a root
/// and passes the scanner's safety rules.
fn reconcile(
    adapter: &dyn OsAdapter,
    db: &Db,
    path: &Path,
    ts: i64,
    st: &mut ApplyStats,
) -> Result<()> {
    let roots: Vec<_> = db.list_roots()?.into_iter().map(|r| r.path).collect();
    let scanner = Scanner::new(adapter, roots);
    // Some watchers report paths through a symlinked directory. Resolve to the
    // real path (which is what a scan stores); if it cannot be resolved or
    // lands outside every root, ignore it.
    let resolved;
    let path: &Path = if traverses_link(adapter, path) {
        match std::fs::canonicalize(path) {
            Ok(p) => {
                resolved = p;
                &resolved
            }
            Err(_) => {
                st.ignored += 1;
                return Ok(());
            }
        }
    } else {
        path
    };
    if !scanner.is_approved(path) || scanner.is_protected(path) || traverses_link(adapter, path) {
        st.ignored += 1;
        return Ok(());
    }
    match adapter.stat(path)? {
        None => {
            st.missing += db.mark_missing_path(path, ts, SOURCE)?;
        }
        Some(entry) => {
            let known = db.file_at_path(path)?.is_some();
            if upsert_one(db, &entry, ts)? {
                st.upserted += 1;
            }
            if entry.kind == EntryKind::Dir && !known {
                st.upserted += upsert_subtree(adapter, db, path, ts)?;
            }
        }
    }
    Ok(())
}

/// True if any ancestor of `path` (excluding the path itself) is a symlink or
/// reparse point. Watchers on some platforms report paths through links; the
/// index only ever stores the real path, exactly as a scan would.
fn traverses_link(adapter: &dyn OsAdapter, path: &Path) -> bool {
    let mut cur = std::path::PathBuf::new();
    let comps: Vec<_> = path.components().collect();
    for c in &comps[..comps.len().saturating_sub(1)] {
        cur.push(c);
        if cur.parent().is_none() {
            continue;
        }
        if let Ok(kind) = adapter.link_kind(&cur) {
            if kind != filemind_core::LinkKind::NotALink {
                return true;
            }
        }
    }
    false
}

pub fn apply_changes(adapter: &dyn OsAdapter, db: &Db, changes: &[Change]) -> Result<ApplyStats> {
    let ts = Utc::now().timestamp();
    let mut st = ApplyStats::default();
    for c in changes {
        match c {
            Change::Rescan(root) => {
                crate::pipeline::scan_root(adapter, db, root)?;
                st.rescans += 1;
            }
            Change::Upsert(p) | Change::Removed(p) => reconcile(adapter, db, p, ts, &mut st)?,
            Change::Renamed { from, to } => {
                // The destination is reconciled like any other path (identity
                // tracking records the move; a renamed directory is re-enumerated
                // because it is unknown at its new path). Whatever is still
                // recorded under the old path did not come along.
                reconcile(adapter, db, to, ts, &mut st)?;
                if !traverses_link(adapter, from) {
                    st.missing += db.mark_missing_path(from, ts, SOURCE)?;
                }
            }
        }
    }
    Ok(st)
}
