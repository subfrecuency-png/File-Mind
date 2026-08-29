//! Watcher thread: platform events → debouncer → incremental index updates.
//! Re-watches when the set of registered roots changes.

use crate::incremental;
use anyhow::Result;
use filemind_core::adapter::{FsEvent, WatchHandle};
use filemind_core::watch::Debouncer;
use filemind_core::OsAdapter;
use filemind_storage::Db;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(500);
const ROOT_REFRESH: Duration = Duration::from_secs(10);

/// Shared counters the RPC server reports.
#[derive(Default)]
pub struct WatchStats {
    pub raw_events: AtomicU64,
    pub changes_applied: AtomicU64,
    pub last_change_unix: AtomicU64,
    pub watching_roots: AtomicU64,
}

pub fn run(
    adapter: &dyn OsAdapter,
    db: &Db,
    stats: Arc<WatchStats>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut debouncer = Debouncer::new(DEBOUNCE);
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut handle: Option<Box<dyn WatchHandle>> = None;
    let (tx, rx) = channel::<FsEvent>();
    let mut last_root_check = Instant::now() - ROOT_REFRESH;

    while !stop.load(Ordering::Relaxed) {
        if last_root_check.elapsed() >= ROOT_REFRESH {
            last_root_check = Instant::now();
            let current: Vec<PathBuf> = db.list_roots()?.into_iter().map(|r| r.path).collect();
            if current != roots {
                handle = None; // drop stops the old watcher
                if current.is_empty() {
                    tracing::info!("no roots registered; watcher idle");
                } else {
                    match adapter.watch(&current, tx.clone()) {
                        Ok(h) => {
                            tracing::info!(roots = ?current, "watching");
                            handle = Some(h);
                        }
                        Err(e) => tracing::warn!(error = %e, "could not start watcher"),
                    }
                }
                roots = current;
                stats
                    .watching_roots
                    .store(roots.len() as u64, Ordering::Relaxed);
            }
        }

        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => {
                stats.raw_events.fetch_add(1, Ordering::Relaxed);
                debouncer.push(ev);
                // drain the burst
                while let Ok(ev) = rx.try_recv() {
                    stats.raw_events.fetch_add(1, Ordering::Relaxed);
                    debouncer.push(ev);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Changes wait in the debouncer while a scan/classify/analyze job
        // owns the database, rather than racing it into a lock timeout.
        let Some(_slot) = crate::jobs::try_heavy() else {
            continue;
        };
        let ready = debouncer.ready();
        if !ready.is_empty() {
            match incremental::apply_changes(adapter, db, &ready) {
                Ok(st) => {
                    let n = st.upserted + st.missing + st.rescans;
                    stats.changes_applied.fetch_add(n, Ordering::Relaxed);
                    stats
                        .last_change_unix
                        .store(chrono::Utc::now().timestamp() as u64, Ordering::Relaxed);
                    tracing::debug!(?st, "applied changes");
                }
                Err(e) => tracing::warn!(error = %e, "applying changes failed"),
            }
        }
    }
    drop(handle);
    Ok(())
}
