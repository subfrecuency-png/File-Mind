//! `filemind-agent`: the headless daemon.
//!
//! Phase 0: opens the database, reports platform + mode, and exits.
//! Phase 1+: scheduler, watcher, indexer, JSON-RPC over a local socket/pipe.

mod platform;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

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
        schema = db.schema_version()?,
        "filemind-agent ready (Phase 0: nothing scheduled yet)"
    );
    Ok(())
}
