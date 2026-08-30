//! Opt-in, aggregate-only telemetry (Phase 10).
//!
//! Off by default. When on, once a day the agent builds one JSON document
//! for the previous (complete) UTC day from the `metrics` and `sessions`
//! tables and POSTs it to `telemetry.endpoint`. The document contains counts
//! and buckets only — never a path, a name, a hash or any content. Its
//! exact shape is `docs/TELEMETRY.md`, and a test pins the field list to
//! that document so it cannot drift silently.

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use filemind_storage::Db;
use serde_json::{json, Value};

pub const SCHEMA: u32 = 1;

/// Every top-level field the document may contain, in order. Kept in sync
/// with docs/TELEMETRY.md by `tests::fields_match_the_docs`.
pub const FIELDS: &[&str] = &[
    "schema",
    "install_id",
    "day",
    "version",
    "os",
    "arch",
    "health_bucket",
    "files_bucket",
    "suggestions_applied",
    "suggestions_undone",
    "rules_armed",
    "rule_runs",
    "sessions",
    "crash_free_sessions",
    "undo_failures",
    "adapter",
];

pub fn enabled(db: &Db) -> bool {
    db.get_setting("telemetry.enabled")
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn endpoint(db: &Db) -> String {
    std::env::var("FILEMIND_TELEMETRY_ENDPOINT")
        .ok()
        .unwrap_or_else(|| {
            db.get_setting("telemetry.endpoint")
                .ok()
                .flatten()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        })
}

/// Turn telemetry on or off. Turning it on mints a fresh random install id,
/// so switching off and on again never links the two periods.
pub fn set_enabled(db: &Db, on: bool) -> Result<()> {
    db.set_setting("telemetry.enabled", &json!(on))?;
    if on {
        db.set_setting("telemetry.install_id", &json!(random_id()))?;
    } else {
        db.set_setting("telemetry.install_id", &Value::Null)?;
    }
    Ok(())
}

fn random_id() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        b.copy_from_slice(&t.to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn bucket(n: i64, edges: &[i64], labels: &[&str]) -> String {
    for (i, e) in edges.iter().enumerate() {
        if n < *e {
            return labels[i].to_string();
        }
    }
    labels[labels.len() - 1].to_string()
}

/// The document for `day` (YYYY-MM-DD). Pure: reads the database, sends nothing.
pub fn document(db: &Db, day: &str) -> Result<Value> {
    let m = db.metrics_for_day(day)?;
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let (sessions, crash_free) = db.session_stats(day)?;
    let (applied, rule_runs, undone) = db.txn_counts(day)?;
    let counts = db.counts()?;
    let health = filemind_core::health::score(&db.health_inputs(None)?).score as i64;
    let adapter = db
        .get_setting("ai.adapter")?
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "none".into());
    let install_id = db
        .get_setting("telemetry.install_id")?
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    Ok(json!({
        "schema": SCHEMA,
        "install_id": install_id,
        "day": day,
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "health_bucket": bucket(health, &[50, 60, 70, 80, 90], &["<50", "50-59", "60-69", "70-79", "80-89", "90+"]),
        "files_bucket": bucket(counts.files, &[10_000, 50_000, 100_000, 250_000, 1_000_000], &["<10k", "10k-50k", "50k-100k", "100k-250k", "250k-1M", "1M+"]),
        "suggestions_applied": applied,
        "suggestions_undone": undone,
        "rules_armed": g("rules_armed"),
        "rule_runs": rule_runs,
        "sessions": sessions,
        "crash_free_sessions": crash_free,
        "undo_failures": g("undo_failures"),
        "adapter": adapter,
    }))
}

/// Send yesterday's document if telemetry is on, an endpoint is set, and it
/// has not been sent yet. Called from the scheduler tick; cheap when idle.
pub fn maybe_send(db: &Db) -> Result<Option<String>> {
    if !enabled(db) {
        return Ok(None);
    }
    let ep = endpoint(db);
    if ep.is_empty() {
        return Ok(None);
    }
    let yesterday = (Utc::now() - Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let last = db
        .get_setting("telemetry.last_sent_day")?
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    if last >= yesterday {
        return Ok(None);
    }
    let doc = document(db, &yesterday)?;
    send(&ep, &doc)?;
    db.set_setting("telemetry.last_sent_day", &json!(yesterday))?;
    db.bump_metric("telemetry_sent", 1)?;
    tracing::info!(day = %yesterday, "telemetry sent");
    Ok(Some(yesterday))
}

fn send(endpoint: &str, doc: &Value) -> Result<()> {
    let resp = ureq::post(endpoint)
        .header("content-type", "application/json")
        .send(doc.to_string().as_bytes())
        .with_context(|| format!("posting telemetry to {endpoint}"))?;
    anyhow::ensure!(
        resp.status().is_success(),
        "telemetry endpoint answered {}",
        resp.status()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_match_the_docs() {
        let doc = include_str!("../../../docs/TELEMETRY.md");
        let documented: Vec<&str> = doc
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                l.strip_prefix("| `").and_then(|r| r.split('`').next())
            })
            .collect();
        assert_eq!(
            documented, FIELDS,
            "docs/TELEMETRY.md and telemetry::FIELDS disagree"
        );

        let db = Db::open_in_memory().unwrap();
        let d = document(&db, "2026-08-29").unwrap();
        let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        let mut sorted = FIELDS.to_vec();
        sorted.sort();
        let mut got = keys.clone();
        got.sort();
        assert_eq!(got, sorted);
        // nothing that looks like a path or a name ever appears
        let s = d.to_string();
        assert!(!s.contains('/') && !s.contains('\\'), "{s}");
    }

    #[test]
    fn sessions_count_crashes_and_clean_exits() {
        let db = Db::open_in_memory().unwrap();
        let a = db.session_start("agent").unwrap();
        db.session_end(a).unwrap();
        let _b = db.session_start("agent").unwrap(); // left open: "crashed"
        let _c = db.session_start("agent").unwrap(); // closes b as a crash
        let today = filemind_storage::metrics::today();
        let (sessions, clean) = db.session_stats(&today).unwrap();
        assert_eq!(sessions, 3);
        assert_eq!(clean, 2, "a ended cleanly, c is still running, b crashed");
        assert_eq!(db.metrics_for_day(&today).unwrap()["crashes"], 1);
        assert!(!enabled(&db));
        set_enabled(&db, true).unwrap();
        assert!(enabled(&db));
        let id1 = db.get_setting("telemetry.install_id").unwrap().unwrap();
        set_enabled(&db, false).unwrap();
        set_enabled(&db, true).unwrap();
        assert_ne!(
            db.get_setting("telemetry.install_id").unwrap().unwrap(),
            id1,
            "a new id per opt-in"
        );
    }
}
