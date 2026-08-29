//! Debouncing of raw watcher events into coalesced change sets.
//!
//! Platform watchers are noisy: a single save can produce create + modify +
//! rename events within milliseconds. The [`Debouncer`] collects raw
//! [`FsEvent`]s and, once a path has been quiet for `window`, emits one
//! [`Change`] per path. Rename pairs are kept intact so identity tracking in
//! the index can record a rename instead of delete + create.

use crate::adapter::FsEvent;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Path exists (or existed at the time) and should be re-stat'ed and upserted.
    Upsert(PathBuf),
    /// Path is gone; mark it (and any children) missing.
    Removed(PathBuf),
    /// Path moved; stat `to`, mark `from` missing if `to` is absent.
    Renamed { from: PathBuf, to: PathBuf },
    /// Watcher overflowed; the whole root needs a rescan.
    Rescan(PathBuf),
}

#[derive(Debug)]
struct Pending {
    change: Change,
    last: Instant,
}

pub struct Debouncer {
    window: Duration,
    pending: HashMap<PathBuf, Pending>,
    rescans: Vec<PathBuf>,
}

impl Debouncer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending: HashMap::new(),
            rescans: Vec::new(),
        }
    }

    pub fn push(&mut self, ev: FsEvent) {
        self.push_at(ev, Instant::now());
    }

    fn push_at(&mut self, ev: FsEvent, now: Instant) {
        match ev {
            FsEvent::Created(p) | FsEvent::Modified(p) => {
                let e = self.pending.entry(p.clone()).or_insert(Pending {
                    change: Change::Upsert(p.clone()),
                    last: now,
                });
                // A rename that is then modified still counts as a rename.
                if matches!(e.change, Change::Removed(_)) {
                    e.change = Change::Upsert(p);
                }
                e.last = now;
            }
            FsEvent::Removed(p) => {
                self.pending.insert(
                    p.clone(),
                    Pending {
                        change: Change::Removed(p),
                        last: now,
                    },
                );
            }
            FsEvent::Renamed { from, to } => {
                self.pending.remove(&from);
                self.pending.insert(
                    to.clone(),
                    Pending {
                        change: Change::Renamed { from, to },
                        last: now,
                    },
                );
            }
            FsEvent::Overflow { root } => {
                self.pending.clear();
                if !self.rescans.contains(&root) {
                    self.rescans.push(root);
                }
            }
        }
    }

    /// Drain every change whose path has been quiet for at least `window`.
    pub fn ready(&mut self) -> Vec<Change> {
        self.ready_at(Instant::now())
    }

    fn ready_at(&mut self, now: Instant) -> Vec<Change> {
        let mut out: Vec<Change> = self.rescans.drain(..).map(Change::Rescan).collect();
        let due: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.last) >= self.window)
            .map(|(k, _)| k.clone())
            .collect();
        for k in due {
            if let Some(p) = self.pending.remove(&k) {
                out.push(p.change);
            }
        }
        // Parents before children so directory creates are seen first.
        out.sort_by_key(|c| match c {
            Change::Rescan(p) | Change::Upsert(p) | Change::Removed(p) => p.components().count(),
            Change::Renamed { to, .. } => to.components().count(),
        });
        out
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.rescans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_bursts_and_keeps_renames() {
        let mut d = Debouncer::new(Duration::from_millis(100));
        let t0 = Instant::now();
        let a = PathBuf::from("/r/a.txt");
        d.push_at(FsEvent::Created(a.clone()), t0);
        d.push_at(FsEvent::Modified(a.clone()), t0 + Duration::from_millis(10));
        d.push_at(FsEvent::Modified(a.clone()), t0 + Duration::from_millis(20));
        d.push_at(
            FsEvent::Renamed {
                from: PathBuf::from("/r/b.txt"),
                to: PathBuf::from("/r/c.txt"),
            },
            t0 + Duration::from_millis(30),
        );
        assert!(
            d.ready_at(t0 + Duration::from_millis(50)).is_empty(),
            "still inside window"
        );
        let mut got = d.ready_at(t0 + Duration::from_millis(200));
        got.sort_by_key(|c| format!("{c:?}"));
        assert_eq!(
            got,
            vec![
                Change::Renamed {
                    from: PathBuf::from("/r/b.txt"),
                    to: PathBuf::from("/r/c.txt")
                },
                Change::Upsert(a),
            ]
        );
        assert!(d.is_empty());
    }

    #[test]
    fn overflow_replaces_everything() {
        let mut d = Debouncer::new(Duration::from_millis(1));
        d.push(FsEvent::Created("/r/x".into()));
        d.push(FsEvent::Overflow { root: "/r".into() });
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(d.ready(), vec![Change::Rescan("/r".into())]);
    }
}
