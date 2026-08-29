//! Windows 10/11 adapter. ReadDirectoryChangesW + USN Journal catch-up,
//! Windows Search, Recycle Bin (SHFileOperation / IFileOperation) and Task
//! Scheduler land in Phases 1–2 and 6.

#![cfg(windows)]

mod notify_bridge;

use chrono::{DateTime, Utc};
use filemind_core::adapter::*;
use filemind_core::model::{EntryKind, FileId};
use filemind_core::{CoreError, Result};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Default)]
pub struct WinAdapter;

impl OsAdapter for WinAdapter {
    fn platform(&self) -> &'static str {
        "windows"
    }

    fn watch(&self, roots: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
        notify_bridge::watch(roots, tx)
    }

    fn stat(&self, path: &Path) -> Result<Option<Entry>> {
        let md = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        const REPARSE: u32 = 0x400;
        let kind = if md.file_attributes() & REPARSE != 0 {
            EntryKind::Link
        } else if md.is_dir() {
            EntryKind::Dir
        } else if md.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(Some(Entry {
            path: path.to_path_buf(),
            file_id: FileId {
                device: 0,
                index: 0,
            },
            kind,
            size: md.len(),
            mtime: md
                .modified()
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(|_| Utc::now()),
            ctime: None,
            birthtime: md.created().ok().map(DateTime::<Utc>::from),
            depth: 0,
        }))
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
                let md = std::fs::symlink_metadata(d.path())?;
                const REPARSE: u32 = 0x400; // FILE_ATTRIBUTE_REPARSE_POINT
                let kind = if md.file_attributes() & REPARSE != 0 {
                    EntryKind::Link // junctions, symlinks, OneDrive placeholders
                } else if md.is_dir() {
                    EntryKind::Dir
                } else if md.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                // Phase 1: replace with GetFileInformationByHandle (volume serial + file index).
                let file_id = FileId {
                    device: 0,
                    index: 0,
                };
                Ok(Entry {
                    path: d.path().to_path_buf(),
                    file_id,
                    kind,
                    size: md.len(),
                    mtime: md
                        .modified()
                        .map(DateTime::<Utc>::from)
                        .unwrap_or_else(|_| Utc::now()),
                    ctime: None,
                    birthtime: md.created().ok().map(DateTime::<Utc>::from),
                    depth: d.depth(),
                })
            });
        Ok(Box::new(it))
    }

    fn native_metadata(&self, _path: &Path) -> Result<NativeMeta> {
        Ok(NativeMeta::default())
    }

    fn native_search(&self, _query: &str) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    fn move_to_trash(&self, _path: &Path) -> Result<TrashReceipt> {
        Err(CoreError::Other(anyhow::anyhow!(
            "recycle bin not implemented yet (Phase 6)"
        )))
    }

    fn rename_no_clobber(&self, from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(CoreError::DestinationExists(to.to_path_buf()));
        }
        std::fs::rename(from, to)?;
        Ok(())
    }

    fn protected_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = [
            "C:\\Windows",
            "C:\\Program Files",
            "C:\\Program Files (x86)",
            "C:\\ProgramData",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if let Some(b) = directories::BaseDirs::new() {
            v.push(b.data_dir().to_path_buf()); // %APPDATA%
            v.push(b.data_local_dir().to_path_buf()); // %LOCALAPPDATA%
        }
        v
    }

    fn register_autostart(&self, _enable: bool) -> Result<()> {
        // Phase 0/1: schtasks /Create /SC ONLOGON
        Ok(())
    }

    fn link_kind(&self, path: &Path) -> Result<LinkKind> {
        let md = std::fs::symlink_metadata(path)?;
        Ok(if md.file_type().is_symlink() {
            LinkKind::Symlink
        } else if md.file_attributes() & 0x400 != 0 {
            LinkKind::Reparse
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
