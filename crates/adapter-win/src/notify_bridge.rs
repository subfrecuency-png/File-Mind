//! notify → FsEvent bridge (FSEvents on macOS, inotify on Linux).

use filemind_core::adapter::{FsEvent, WatchHandle};
use filemind_core::{CoreError, Result};
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub struct NotifyHandle {
    _watcher: notify::RecommendedWatcher,
}
impl WatchHandle for NotifyHandle {}

pub fn map_event(ev: notify::Event, roots: &[PathBuf]) -> Vec<FsEvent> {
    if ev.need_rescan() {
        return roots
            .iter()
            .map(|r| FsEvent::Overflow { root: r.clone() })
            .collect();
    }
    let mut paths = ev.paths;
    match ev.kind {
        EventKind::Create(_) => paths.into_iter().map(FsEvent::Created).collect(),
        EventKind::Remove(_) => paths.into_iter().map(FsEvent::Removed).collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
            let to = paths.pop().unwrap();
            let from = paths.pop().unwrap();
            vec![FsEvent::Renamed { from, to }]
        }
        EventKind::Modify(ModifyKind::Name(_)) | EventKind::Any | EventKind::Other => paths
            .into_iter()
            .map(|p| {
                // FSEvents reports renames as a bare name change on one path:
                // decide by looking at the disk.
                if p.symlink_metadata().is_ok() {
                    FsEvent::Modified(p)
                } else {
                    FsEvent::Removed(p)
                }
            })
            .collect(),
        EventKind::Modify(_) => paths.into_iter().map(FsEvent::Modified).collect(),
        EventKind::Access(_) => Vec::new(),
    }
}

pub fn watch(roots: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
    let roots_owned = roots.to_vec();
    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(ev) => {
                for e in map_event(ev, &roots_owned) {
                    if tx.send(e).is_err() {
                        break;
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "watcher error"),
        })
        .map_err(|e| CoreError::Other(e.into()))?;
    for r in roots {
        watcher
            .watch(r, RecursiveMode::Recursive)
            .map_err(|e| CoreError::Other(anyhow::anyhow!("watch {}: {e}", r.display())))?;
    }
    Ok(Box::new(NotifyHandle { _watcher: watcher }))
}
