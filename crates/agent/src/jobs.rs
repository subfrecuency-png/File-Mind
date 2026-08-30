//! Heavy jobs: one at a time, and (since Phase 9) optionally in the background.
//!
//! The agent has three threads with their own SQLite connections (scheduler,
//! watcher, RPC). Each writes in short transactions, but a full scan, a
//! classification pass and an analysis rebuild all contend for the write
//! lock, and a waiter that sits at the busy timeout fails with "database is
//! locked". Serialising the heavy jobs in-process means they queue instead.
//!
//! `jobs.start` runs a scan / analysis / embedding pass / model download on
//! its own thread with its own database connection and reports progress
//! through `jobs.status`, so the desktop window is never blocked by a long
//! RPC. The registry is process-wide: whichever process handles the RPC
//! (the daemon, or the desktop app's in-process context) owns its jobs.

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

static HEAVY: Mutex<()> = Mutex::new(());

/// Block until no other heavy job is running.
pub fn heavy() -> MutexGuard<'static, ()> {
    match HEAVY.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Take the heavy-job slot only if it is free right now.
pub fn try_heavy() -> Option<MutexGuard<'static, ()>> {
    match HEAVY.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

// ---------------------------------------------------------------------------
// Background job registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub id: u64,
    pub kind: String,
    pub state: JobState,
    /// What the job is doing right now ("scanning ~/Downloads", "model.onnx").
    pub label: String,
    /// Progress in whatever unit fits (files, bytes); `total` 0 = unknown.
    pub done: u64,
    pub total: u64,
    pub started_ms: u128,
    pub elapsed_ms: u128,
    /// The RPC-shaped result once finished, or the error message.
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Registry {
    jobs: BTreeMap<u64, JobInfo>,
    starts: BTreeMap<u64, Instant>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = match REGISTRY.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let r = g.get_or_insert_with(Registry::default);
    f(r)
}

/// Handle a running job uses to report progress.
#[derive(Clone)]
pub struct Progress {
    id: u64,
}

impl Progress {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn set(&self, label: &str, done: u64, total: u64) {
        with_registry(|r| {
            if let Some(j) = r.jobs.get_mut(&self.id) {
                if !label.is_empty() {
                    j.label = label.to_string();
                }
                j.done = done;
                j.total = total;
            }
        });
    }
}

/// The job currently running, if any.
pub fn running() -> Option<JobInfo> {
    list().into_iter().find(|j| j.state == JobState::Running)
}

/// Every job this process has run, oldest first, with live elapsed times.
pub fn list() -> Vec<JobInfo> {
    with_registry(|r| {
        r.jobs
            .values()
            .map(|j| {
                let mut j = j.clone();
                if j.state == JobState::Running {
                    if let Some(s) = r.starts.get(&j.id) {
                        j.elapsed_ms = s.elapsed().as_millis();
                    }
                }
                j
            })
            .collect()
    })
}

pub fn get(id: u64) -> Option<JobInfo> {
    list().into_iter().find(|j| j.id == id)
}

/// Forget finished jobs older than `keep` so the registry stays small.
fn prune(keep: Duration) {
    with_registry(|r| {
        let old: Vec<u64> = r
            .jobs
            .iter()
            .filter(|(id, j)| {
                j.state != JobState::Running
                    && r.starts.get(id).map(|s| s.elapsed() > keep).unwrap_or(true)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in old {
            r.jobs.remove(&id);
            r.starts.remove(&id);
        }
    });
}

/// Start `work` on a background thread. Only one job runs at a time: if the
/// heavy slot is taken (by another job, a scheduler tick or a synchronous
/// RPC) this returns an error rather than queueing, so the UI can say so.
pub fn spawn<F>(kind: &str, label: &str, work: F) -> Result<JobInfo>
where
    F: FnOnce(&Progress) -> Result<Value> + Send + 'static,
{
    prune(Duration::from_secs(3600));
    // A MutexGuard cannot cross threads, so the check happens here and the
    // job thread takes the real slot first thing. A synchronous RPC that
    // sneaks in between simply makes the job wait a little.
    if running().is_some() || try_heavy().is_none() {
        let what = running()
            .map(|j| format!("{} ({})", j.kind, j.label))
            .unwrap_or_else(|| "another job".into());
        anyhow::bail!("busy: {what} is running");
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let info = JobInfo {
        id,
        kind: kind.to_string(),
        state: JobState::Running,
        label: label.to_string(),
        done: 0,
        total: 0,
        started_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        elapsed_ms: 0,
        result: None,
        error: None,
    };
    with_registry(|r| {
        r.jobs.insert(id, info.clone());
        r.starts.insert(id, Instant::now());
    });
    let progress = Progress { id };
    std::thread::Builder::new()
        .name(format!("job-{kind}"))
        .spawn(move || {
            let _slot = heavy();
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&progress)));
            let (state, result, error) = match outcome {
                Ok(Ok(v)) => (JobState::Done, Some(v), None),
                Ok(Err(e)) => (JobState::Failed, None, Some(format!("{e:#}"))),
                Err(_) => (JobState::Failed, None, Some("job panicked".to_string())),
            };
            with_registry(|r| {
                let elapsed = r
                    .starts
                    .get(&id)
                    .map(|s| s.elapsed().as_millis())
                    .unwrap_or(0);
                if let Some(j) = r.jobs.get_mut(&id) {
                    j.state = state;
                    j.result = result;
                    j.error = error;
                    j.elapsed_ms = elapsed;
                    if j.total > 0 && state == JobState::Done {
                        j.done = j.total;
                    }
                }
            });
        })?;
    Ok(info)
}

/// JSON for the `jobs.*` RPCs.
pub fn info_json(j: &JobInfo) -> Value {
    json!(j)
}
