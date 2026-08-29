//! `filemind-agent`: the headless daemon. Scans registered roots on a timer
//! and hashes content within a throttled budget. Phase 2 adds the watcher
//! and the local JSON-RPC API.

use anyhow::Result;
use filemind_agent::{platform, scheduler};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let once = std::env::args().any(|a| a == "--once");
    let adapter = platform::adapter();
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
    scheduler::run(adapter.as_ref(), &db, once)
}
