//! Read-only scanner (Phase 1 skeleton).
//!
//! Walks approved roots through the [`OsAdapter`], applies the safety rules
//! (protected roots, link handling, depth cap, ignore file) and yields
//! [`Entry`] items for the indexer. It never writes to the file system.

use crate::adapter::{is_within, Entry, OsAdapter, ScanOptsNative};
use crate::model::EntryKind;
use crate::{CoreError, Result};
use std::path::{Path, PathBuf};

pub const IGNORE_FILE: &str = ".filemindignore";
pub const DEFAULT_MAX_DEPTH: usize = 64;

/// Directories that are machine-generated or dependency caches. They are still
/// indexed (so search and recovery see them) but never become projects, never
/// produce duplicate/version suggestions, and never count against health.
pub const NOISE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "target",
    "build",
    "dist",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".cache",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    "Pods",
    "DerivedData",
    "vendor",
    "bower_components",
    ".Trash",
    "Library",
    ".idea",
    ".vscode",
    "coverage",
    ".tox",
    ".terraform",
    ".serverless",
];

/// True if any path component is a noise directory.
pub fn in_noise_dir(path: &std::path::Path) -> bool {
    path.components()
        .any(|c| NOISE_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

#[derive(Debug, Clone)]
pub struct ScanOpts {
    pub max_depth: usize,
    /// Hash file contents (pass B). Off for the fast metadata walk.
    pub hash_contents: bool,
}

impl Default for ScanOpts {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            hash_contents: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub files: u64,
    pub dirs: u64,
    pub links_skipped: u64,
    pub protected_skipped: u64,
    pub ignored: u64,
    pub errors: u64,
    pub bytes: u64,
}

pub struct Scanner<'a> {
    adapter: &'a dyn OsAdapter,
    approved_roots: Vec<PathBuf>,
    protected: Vec<PathBuf>,
}

impl<'a> Scanner<'a> {
    pub fn new(adapter: &'a dyn OsAdapter, approved_roots: Vec<PathBuf>) -> Self {
        let protected = adapter.protected_roots();
        Self {
            adapter,
            approved_roots,
            protected,
        }
    }

    /// Reject roots that are protected or are a filesystem root (`/`, `C:\`).
    pub fn validate_root(&self, root: &Path) -> Result<()> {
        if root.parent().is_none() {
            return Err(CoreError::OutsideRoots(root.to_path_buf()));
        }
        if self.is_protected(root) {
            return Err(CoreError::ProtectedPath(root.to_path_buf()));
        }
        Ok(())
    }

    pub fn is_protected(&self, path: &Path) -> bool {
        self.protected.iter().any(|p| is_within(path, p))
    }

    pub fn is_approved(&self, path: &Path) -> bool {
        self.approved_roots.iter().any(|r| is_within(path, r))
    }

    /// Scan one root, calling `sink` for every entry that passes the safety rules.
    pub fn scan_root<F>(&self, root: &Path, opts: &ScanOpts, mut sink: F) -> Result<ScanReport>
    where
        F: FnMut(&Entry),
    {
        self.validate_root(root)?;
        let mut report = ScanReport::default();
        let native = ScanOptsNative {
            max_depth: opts.max_depth,
            follow_links: false,
        };
        let mut ignored_dirs: Vec<PathBuf> = Vec::new();

        for item in self.adapter.enumerate(root, &native)? {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "scan error");
                    report.errors += 1;
                    continue;
                }
            };

            if ignored_dirs.iter().any(|d| is_within(&entry.path, d)) {
                report.ignored += 1;
                continue;
            }
            if self.is_protected(&entry.path) {
                report.protected_skipped += 1;
                continue;
            }
            if entry.depth > opts.max_depth {
                continue;
            }
            match entry.kind {
                EntryKind::Link | EntryKind::Other => {
                    // Rule: never follow unknown symlinks/junctions. Record, don't traverse.
                    report.links_skipped += 1;
                    sink(&entry);
                    continue;
                }
                EntryKind::Dir => {
                    if entry.path.join(IGNORE_FILE).exists() {
                        ignored_dirs.push(entry.path.clone());
                        report.ignored += 1;
                        continue;
                    }
                    report.dirs += 1;
                }
                EntryKind::File => {
                    report.files += 1;
                    report.bytes += entry.size;
                }
            }
            sink(&entry);
        }
        Ok(report)
    }
}

/// BLAKE3 of a file's contents, streamed. Used by pass B of the scanner.
pub fn hash_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::*;
    use crate::model::FileId;
    use chrono::Utc;
    use std::sync::mpsc::Sender;

    /// Test adapter backed by `walkdir`; stands in for the platform adapters.
    struct WalkAdapter {
        protected: Vec<PathBuf>,
    }

    impl OsAdapter for WalkAdapter {
        fn platform(&self) -> &'static str {
            "test"
        }
        fn watch(&self, _: &[PathBuf], _: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
            unimplemented!()
        }
        fn enumerate(
            &self,
            root: &Path,
            opts: &ScanOptsNative,
        ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>> {
            let it = walkdir::WalkDir::new(root)
                .min_depth(1)
                .max_depth(opts.max_depth)
                .follow_links(false)
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
                    let md = d.metadata().map_err(|e| CoreError::Other(e.into()))?;
                    Ok(Entry {
                        path: d.path().to_path_buf(),
                        file_id: FileId {
                            device: 0,
                            index: 0,
                        },
                        kind,
                        size: md.len(),
                        mtime: Utc::now(),
                        ctime: None,
                        birthtime: None,
                        depth: d.depth(),
                    })
                });
            Ok(Box::new(it))
        }
        fn stat(&self, _: &Path) -> Result<Option<Entry>> {
            Ok(None)
        }
        fn native_metadata(&self, _: &Path) -> Result<NativeMeta> {
            Ok(NativeMeta::default())
        }
        fn native_search(&self, _: &str) -> Result<Vec<PathBuf>> {
            Ok(vec![])
        }
        fn move_to_trash(&self, _: &Path) -> Result<TrashReceipt> {
            unimplemented!()
        }
        fn trash_target(&self, _: &Path) -> Result<PathBuf> {
            unimplemented!()
        }
        fn move_to_trash_at(&self, _: &Path, _: &Path) -> Result<TrashReceipt> {
            unimplemented!()
        }
        fn rename_no_clobber(&self, _: &Path, _: &Path) -> Result<()> {
            unimplemented!()
        }
        fn protected_roots(&self) -> Vec<PathBuf> {
            self.protected.clone()
        }
        fn register_autostart(&self, _: bool) -> Result<()> {
            Ok(())
        }
        fn link_kind(&self, _: &Path) -> Result<LinkKind> {
            Ok(LinkKind::NotALink)
        }
        fn default_roots(&self) -> Vec<PathBuf> {
            vec![]
        }
    }

    #[test]
    fn skips_protected_ignored_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::create_dir_all(root.join("skip")).unwrap();
        std::fs::write(root.join("docs/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("secret/b.txt"), b"nope").unwrap();
        std::fs::write(root.join("skip/.filemindignore"), b"").unwrap();
        std::fs::write(root.join("skip/c.txt"), b"ignored").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path(), root.join("loop")).unwrap();

        let adapter = WalkAdapter {
            protected: vec![root.join("secret")],
        };
        let scanner = Scanner::new(&adapter, vec![root.clone()]);
        let mut seen = Vec::new();
        let report = scanner
            .scan_root(&root, &ScanOpts::default(), |e| seen.push(e.path.clone()))
            .unwrap();

        assert_eq!(report.files, 1, "only docs/a.txt is a scannable file");
        assert!(seen.contains(&root.join("docs/a.txt")));
        assert!(!seen.iter().any(|p| p.starts_with(root.join("secret"))));
        assert!(!seen.contains(&root.join("skip/c.txt")));
        assert!(report.protected_skipped >= 1);
        #[cfg(unix)]
        assert_eq!(report.links_skipped, 1);
    }

    #[test]
    fn refuses_filesystem_root() {
        let adapter = WalkAdapter { protected: vec![] };
        let scanner = Scanner::new(&adapter, vec![]);
        let fs_root = if cfg!(windows) { "C:\\" } else { "/" };
        assert!(scanner.validate_root(Path::new(fs_root)).is_err());
    }

    #[test]
    fn hashes_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x");
        std::fs::write(&p, b"filemind").unwrap();
        assert_eq!(
            hash_file(&p).unwrap(),
            blake3::hash(b"filemind").to_hex().to_string()
        );
    }
}
