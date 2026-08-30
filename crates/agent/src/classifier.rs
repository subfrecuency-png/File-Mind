//! Throttled classification + text-extraction pass over pending files.

use anyhow::Result;
use filemind_core::classify::{self, UserRule};
use filemind_core::extract;
use filemind_core::model::Classification;
use filemind_storage::Db;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct ClassifyOpts {
    pub duty_cycle: f32,
    pub max_wall: Option<Duration>,
    /// Skip text extraction (classify from names only). Much faster.
    pub names_only: bool,
}

impl Default for ClassifyOpts {
    fn default() -> Self {
        Self {
            duty_cycle: 0.2,
            max_wall: None,
            names_only: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ClassifyOutcome {
    pub classified: u64,
    pub extracted: u64,
    pub sensitive: u64,
    pub remaining: u64,
    pub elapsed_ms: u128,
}

/// Classify one path on the spot (used by `filemind classify show` for
/// unindexed files and by tests).
pub fn classify_path(
    path: &Path,
    size: u64,
    rules: &[UserRule],
    names_only: bool,
) -> (Classification, Option<&'static str>, Option<String>) {
    let by_name = classify::sensitive_by_name(path);
    let text = if names_only || by_name.is_some() {
        None
    } else {
        extract::extract(path, size).ok().flatten()
    };
    let by_text = text.as_deref().and_then(classify::sensitive_by_text);
    let sensitive = by_name.or(by_text).map(|s| match s {
        classify::Sensitive::SecretsFile => "secrets_file",
        classify::Sensitive::PrivateKey => "private_key",
        classify::Sensitive::ApiKey => "api_key",
        classify::Sensitive::CardNumber => "card_number",
        classify::Sensitive::NationalId => "national_id",
    });
    let c = classify::classify(path, text.as_deref(), rules);
    (c, sensitive, text)
}

pub fn classify_pending(db: &Db, opts: ClassifyOpts) -> Result<ClassifyOutcome> {
    let started = Instant::now();
    let mut out = ClassifyOutcome::default();
    let duty = opts.duty_cycle.clamp(0.05, 1.0);
    let mut busy = Duration::ZERO;
    let rules: Vec<UserRule> = db.list_rules()?.into_iter().map(|(_, r)| r).collect();
    const BATCH: usize = 500;

    loop {
        let pending = db.classify_pending(BATCH)?;
        if pending.is_empty() {
            break;
        }
        let t = Instant::now();
        let mut rows = Vec::with_capacity(pending.len());
        for p in &pending {
            if opts.max_wall.is_some_and(|w| started.elapsed() >= w) {
                break;
            }
            // A parser bug on one odd file (a malformed PDF, an unexpected
            // encoding) must not take the agent down: fall back to name-only.
            let (c, sens, text) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || classify_path(&p.path, p.size, &rules, opts.names_only),
            )) {
                Ok(r) => r,
                Err(_) => {
                    tracing::warn!(path = %p.path.display(), "classifier panicked on file; using name only");
                    classify_path(&p.path, p.size, &rules, true)
                }
            };
            if text.is_some() {
                out.extracted += 1;
            }
            if sens.is_some() {
                out.sensitive += 1;
            }
            rows.push((p.file_id.clone(), p.mtime, c, sens.map(String::from), text));
        }
        let n = rows.len();
        db.record_classifications(&rows)?;
        out.classified += n as u64;
        busy += t.elapsed();
        if n < pending.len() || opts.max_wall.is_some_and(|w| started.elapsed() >= w) {
            break;
        }
        let target_wall = busy.mul_f32(1.0 / duty);
        let wall = started.elapsed();
        if target_wall > wall {
            std::thread::sleep((target_wall - wall).min(Duration::from_secs(5)));
        }
    }
    out.remaining = db.count_classify_pending()?;
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}
