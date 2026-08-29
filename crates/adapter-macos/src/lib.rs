//! macOS adapter. The enumeration/metadata parts are plain POSIX and also
//! compile on Linux so the workspace can be tested on any Unix CI runner;
//! FSEvents, Spotlight, Trash and launchd integration are macOS-only and
//! land in Phases 1–2 and 6.

#![cfg(unix)]

use chrono::{DateTime, TimeZone, Utc};
use filemind_core::adapter::*;
use filemind_core::model::{EntryKind, FileId};
use filemind_core::{CoreError, Result};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Default)]
pub struct MacAdapter;

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

impl OsAdapter for MacAdapter {
    fn platform(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "posix"
        }
    }

    fn watch(&self, _roots: &[PathBuf], _tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
        // Phase 2: FSEvents via the `notify` crate.
        Err(CoreError::Other(anyhow::anyhow!(
            "watcher not implemented yet (Phase 2)"
        )))
    }

    fn enumerate(
        &self,
        root: &Path,
        opts: &ScanOptsNative,
    ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>> {
        let it = walkdir::WalkDir::new(root)
            .min_depth(1)
            .max_depth(opts.max_depth)
            .follow_links(opts.follow_links)
            .into_iter()
            .map(|r| {
                let d = r.map_err(|e| CoreError::Other(e.into()))?;
                let ft = d.file_type();
                let kind = if ft.is_symlink() {
                    EntryKind::Link
                } else if ft.is_dir() {
                    EntryKind::Dir
                } else if ft.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                let md = std::fs::symlink_metadata(d.path())?;
                Ok(Entry {
                    path: d.path().to_path_buf(),
                    file_id: FileId {
                        device: md.dev(),
                        index: md.ino(),
                    },
                    kind,
                    size: md.len(),
                    mtime: ts(md.mtime()),
                    ctime: Some(ts(md.ctime())),
                    birthtime: md.created().ok().map(DateTime::<Utc>::from),
                    depth: d.depth(),
                })
            });
        Ok(Box::new(it))
    }

    fn native_metadata(&self, _path: &Path) -> Result<NativeMeta> {
        // Phase 2: Spotlight kMDItem* attributes via `mdls`.
        Ok(NativeMeta::default())
    }

    fn native_search(&self, _query: &str) -> Result<Vec<PathBuf>> {
        // Phase 2: `mdfind`.
        Ok(Vec::new())
    }

    fn move_to_trash(&self, _path: &Path) -> Result<TrashReceipt> {
        // Phase 6: NSFileManager.trashItem via osascript or the `trash` crate.
        Err(CoreError::Other(anyhow::anyhow!(
            "trash not implemented yet (Phase 6)"
        )))
    }

    fn rename_no_clobber(&self, from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(CoreError::DestinationExists(to.to_path_buf()));
        }
        // Phase 6: use renameat2/RENAME_NOREPLACE semantics where available.
        std::fs::rename(from, to)?;
        Ok(())
    }

    fn protected_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = [
            "/System",
            "/Library",
            "/usr",
            "/bin",
            "/sbin",
            "/private",
            "/Applications",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            v.push(home.join("Library"));
            v.push(home.join(".Trash"));
        }
        v
    }

    fn register_autostart(&self, _enable: bool) -> Result<()> {
        // Phase 0/1: write ~/Library/LaunchAgents/ai.filemind.agent.plist and `launchctl bootstrap`.
        Ok(())
    }

    fn link_kind(&self, path: &Path) -> Result<LinkKind> {
        let md = std::fs::symlink_metadata(path)?;
        Ok(if md.file_type().is_symlink() {
            LinkKind::Symlink
        } else {
            LinkKind::NotALink
        })
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        let Some(u) = directories::UserDirs::new() else {
            return vec![];
        };
        [
            u.desktop_dir(),
            u.document_dir(),
            u.download_dir(),
            u.picture_dir(),
        ]
        .into_iter()
        .flatten()
        .map(Path::to_path_buf)
        .collect()
    }
}
