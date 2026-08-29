//! `filemind-agent`: the headless daemon.
//!
//! Three threads: the watcher (live incremental updates), the scheduler
//! (periodic full scan + throttled hashing as a safety net) and the local
//! JSON-RPC server the CLI/GUI talk to. `--once` runs a single scheduler
//! tick and exits.

use anyhow::Result;
use filemind_agent::{platform, rpc, scheduler, watcher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let once = std::env::args().any(|a| a == "--once");
    let adapter: Arc<dyn filemind_core::OsAdapter> = Arc::from(platform::adapter());
    let db_path = filemind_storage::default_db_path()?;
    let db = filemind_storage::Db::open(&db_path)?;
    let mode = db
        .get_setting("mode")?
        .unwrap_or(serde_json::json!("observe"));

    tracing::info!(
        platform = adapter.platform(),
        db = %db_path.display(),
        mode = %mode,
        roots = db.list_roots()?.len(),
        once,
        "filemind-agent starting"
    );

    // Settle anything a previous process left half-done before doing anything else.
    match filemind_agent::actions::recover_all(adapter.as_ref(), &db) {
        Ok(v) if !v.is_empty() => {
            tracing::warn!(recovered = v.len(), "unfinished transactions settled")
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "recovery failed"),
    }

    if once {
        return scheduler::run(adapter.as_ref(), &db, true);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(watcher::WatchStats::default());

    // Ctrl-C / SIGTERM → graceful stop.
    {
        let stop = stop.clone();
        ctrlc_handler(move || stop.store(true, Ordering::Relaxed));
    }

    let watcher_thread = {
        let (adapter, stats, stop, db_path) = (
            adapter.clone(),
            stats.clone(),
            stop.clone(),
            db_path.clone(),
        );
        std::thread::Builder::new()
            .name("watcher".into())
            .spawn(move || {
                let db = filemind_storage::Db::open(&db_path).expect("open db");
                if let Err(e) = watcher::run(adapter.as_ref(), &db, stats, stop) {
                    tracing::error!(error = %e, "watcher exited");
                }
            })?
    };

    let rpc_thread = {
        let ctx = Arc::new(rpc::Context {
            adapter: adapter.clone(),
            db_path: db_path.clone(),
            db: Mutex::new(filemind_storage::Db::open(&db_path)?),
            stats: stats.clone(),
            started: Instant::now(),
        });
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("rpc".into())
            .spawn(move || {
                if let Err(e) = rpc::serve(ctx, stop) {
                    tracing::error!(error = %e, "rpc exited");
                }
            })?
    };

    // Scheduler runs on the main thread.
    while !stop.load(Ordering::Relaxed) {
        let sched = scheduler::schedule_from_settings(&db);
        match scheduler::tick(adapter.as_ref(), &db, sched) {
            Ok(summary) => tracing::info!(%summary, "tick"),
            Err(e) => tracing::error!(error = %e, "tick failed"),
        }
        let deadline = Instant::now() + sched.scan_every;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    tracing::info!("stopping");
    let _ = watcher_thread.join();
    let _ = rpc_thread.join();
    Ok(())
}

#[cfg(unix)]
fn ctrlc_handler<F: Fn() + Send + Sync + 'static>(f: F) {
    // Minimal signal handling without a dependency: a thread that waits on a
    // self-pipe would be nicer; for now we rely on `signal` via libc-free
    // approach: std does not expose SIGTERM, so use the `ctrlc`-style trick
    // through `std::os::unix` is unavailable — fall back to a watcher on a
    // stop file next to the socket, written by `filemind agent stop`.
    let stop_file = filemind_storage::default_db_path()
        .map(|p| p.with_file_name("agent.stop"))
        .ok();
    std::thread::spawn(move || loop {
        if let Some(p) = &stop_file {
            if p.exists() {
                let _ = std::fs::remove_file(p); // filemind:own-file (control file, never user data)
                f();
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    });
}

#[cfg(not(unix))]
fn ctrlc_handler<F: Fn() + Send + Sync + 'static>(_f: F) {}
