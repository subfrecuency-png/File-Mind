//! The portability seam. Each OS implements this once; everything else is shared.

use crate::model::{EntryKind, FileId};
use crate::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// One item yielded by [`OsAdapter::enumerate`].
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub file_id: FileId,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: DateTime<Utc>,
    pub ctime: Option<DateTime<Utc>>,
    pub birthtime: Option<DateTime<Utc>>,
    /// Depth relative to the scan root (root itself is 0).
    pub depth: usize,
}

/// A file-system change reported by the platform watcher.
#[derive(Debug, Clone)]
pub enum FsEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    Removed(PathBuf),
    /// The watcher lost events (buffer overflow); a rescan of `root` is needed.
    Overflow {
        root: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    NotALink,
    Symlink,
    /// Windows junction / reparse point, macOS alias or firmlink.
    Reparse,
}

/// Platform metadata beyond the POSIX basics (Spotlight kMDItem*, Windows properties).
#[derive(Debug, Clone, Default)]
pub struct NativeMeta {
    pub content_type: Option<String>,
    pub where_from: Vec<String>,
    pub tags: Vec<String>,
    pub extra: Vec<(String, String)>,
}

/// Receipt returned by a successful trash operation, sufficient to restore the item.
#[derive(Debug, Clone)]
pub struct TrashReceipt {
    pub original: PathBuf,
    pub trashed_to: Option<PathBuf>,
    pub at: DateTime<Utc>,
}

/// What an in-place rewrite (transparent compression) did to a file's
/// on-disk footprint. The bytes an application reads never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewriteReceipt {
    pub on_disk_before: u64,
    pub on_disk_after: u64,
}

/// Whether a file currently carries a rewrite, as seen on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteState {
    /// Plain file; nothing of ours on it.
    Original,
    /// The rewrite is complete and the kernel serves the original bytes.
    Rewritten,
    /// A rewrite was interrupted between its last two steps (the compressed
    /// payload is on the file but not yet activated). `rewrite` finishes it.
    HalfDone,
    /// This platform / volume has no such mechanism.
    Unsupported,
}

/// Handle that stops the watcher when dropped.
pub trait WatchHandle: Send {}

/// Options for a scan pass.
#[derive(Debug, Clone)]
pub struct ScanOptsNative {
    pub max_depth: usize,
    pub follow_links: bool,
}

pub trait OsAdapter: Send + Sync {
    /// Human-readable platform name, e.g. "macos" / "windows".
    fn platform(&self) -> &'static str;

    /// Start watching `roots`; events are sent on `tx`.
    fn watch(&self, roots: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>>;

    /// Enumerate everything under `root`, breadth-first, without following links.
    fn enumerate(
        &self,
        root: &Path,
        opts: &ScanOptsNative,
    ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>>;

    /// Stat a single path without following links. `Ok(None)` if it does not exist.
    /// `depth` is left at 0; callers that need it compute it from the root.
    fn stat(&self, path: &Path) -> Result<Option<Entry>>;

    /// Platform metadata for a single path.
    fn native_metadata(&self, path: &Path) -> Result<NativeMeta>;

    /// Delegate to mdfind / Windows Search. Used as a bootstrap and fallback only.
    fn native_search(&self, query: &str) -> Result<Vec<PathBuf>>;

    /// Move to Trash / Recycle Bin. **This is the only deletion primitive in FileMind.**
    fn move_to_trash(&self, path: &Path) -> Result<TrashReceipt>;

    /// Where [`OsAdapter::move_to_trash`] would put `path` right now, so the
    /// transaction journal can record the destination *before* the move.
    fn trash_target(&self, path: &Path) -> Result<PathBuf>;

    /// Move `path` to the exact `target` (from [`OsAdapter::trash_target`]).
    /// Must fail if `target` exists.
    fn move_to_trash_at(&self, path: &Path, target: &Path) -> Result<TrashReceipt>;

    /// Rename/move within the same volume. Must fail if `to` exists.
    /// Only the transaction manager may call this.
    fn rename_no_clobber(&self, from: &Path, to: &Path) -> Result<()>;

    /// Roots that must never be scanned or modified.
    fn protected_roots(&self) -> Vec<PathBuf>;

    /// Register / unregister the agent with launchd or Task Scheduler.
    fn register_autostart(&self, enable: bool) -> Result<()>;

    /// Classify a path as a symlink, reparse point, or regular entry.
    fn link_kind(&self, path: &Path) -> Result<LinkKind>;

    /// Default roots offered during onboarding (Desktop, Documents, Downloads, Pictures).
    fn default_roots(&self) -> Vec<PathBuf>;

    /// Rewrite `path` in place with `method` (today: `"apfs"`, transparent
    /// compression). Same inode, same bytes on read, smaller on disk; must
    /// refuse links, files open by another process and anything already
    /// rewritten; must be idempotent on a `HalfDone` file. Only the
    /// transaction manager may call this.
    fn rewrite(&self, path: &Path, method: &str) -> Result<RewriteReceipt> {
        let _ = path;
        Err(crate::CoreError::Other(anyhow::anyhow!(
            "rewrite method {method} is not supported on {}",
            self.platform()
        )))
    }

    /// Undo [`OsAdapter::rewrite`]: put the plain representation back, in
    /// place, same inode.
    fn rewrite_restore(&self, path: &Path, method: &str) -> Result<()> {
        let _ = path;
        Err(crate::CoreError::Other(anyhow::anyhow!(
            "rewrite method {method} is not supported on {}",
            self.platform()
        )))
    }

    /// What the disk says about `path` with respect to `method`.
    fn rewrite_state(&self, path: &Path, method: &str) -> Result<RewriteState> {
        let _ = (path, method);
        Ok(RewriteState::Unsupported)
    }

    /// Bytes `path` occupies on disk (blocks, not length). Defaults to the length.
    fn on_disk_bytes(&self, path: &Path) -> Result<u64> {
        Ok(std::fs::symlink_metadata(path)?.len())
    }

    /// Vault master key (32 bytes). Default: `FILEMIND_VAULT_MK` or a 0600
    /// file under the FileMind data dir. macOS overrides this with Keychain
    /// (`vault-mk`, WhenUnlockedThisDeviceOnly). Never log the return value.
    fn vault_master_key(&self) -> Result<[u8; 32]> {
        if let Some(k) = crate::vault::mk_from_env()? {
            return Ok(k);
        }
        let dir = crate::vault::default_objects_dir()?
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        crate::vault::file_mk(&dir)
    }

    /// Directory for `*.fmseal` objects. Overridable with `FILEMIND_VAULT_DIR`.
    fn vault_objects_dir(&self) -> Result<PathBuf> {
        crate::vault::default_objects_dir()
    }
}

/// True if `path` is `root` or lies beneath it (lexically; callers canonicalise first).
pub fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}
