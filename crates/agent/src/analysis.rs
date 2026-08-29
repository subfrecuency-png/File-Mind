//! Run the derived analyses (duplicates, versions, health, suggestions) as one step.

use anyhow::Result;
use filemind_core::health::Health;
use filemind_storage::Db;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct AnalysisOutcome {
    pub duplicate_groups: usize,
    pub duplicate_bytes: u64,
    pub version_chains: usize,
    pub suggestions: usize,
    pub health: Health,
    pub elapsed_ms: u128,
}

pub fn run(db: &Db) -> Result<AnalysisOutcome> {
    let started = Instant::now();
    let dups = db.rebuild_duplicates()?;
    let chains = db.rebuild_versions()?;
    let suggestions = db.refresh_suggestions(&dups, &chains)?;
    let health = db.refresh_health()?;
    Ok(AnalysisOutcome {
        duplicate_groups: dups.len(),
        duplicate_bytes: dups.iter().map(|g| g.size * g.copies.len() as u64).sum(),
        version_chains: chains.len(),
        suggestions,
        health,
        elapsed_ms: started.elapsed().as_millis(),
    })
}
