//! FileMind core: domain logic shared by every platform.
//!
//! Nothing in this crate touches the OS directly. All file-system access goes
//! through the [`OsAdapter`] trait so the same scanner, index, classifier and
//! transaction manager run unchanged on macOS and Windows.
//!
//! Core safety rules (see docs/ARCHITECTURE.md §7) are enforced here by
//! construction: there is no delete primitive other than
//! [`OsAdapter::move_to_trash`], and no move primitive outside the
//! transaction manager.

pub mod adapter;
pub mod classify;
pub mod extract;
pub mod health;
pub mod mode;
pub mod model;
pub mod projects;
pub mod query;
pub mod rank;
pub mod rules;
pub mod scanner;
pub mod shrink;
pub mod txn;
pub mod vectors;
pub mod versions;
pub mod watch;

pub use adapter::{Entry, FsEvent, LinkKind, NativeMeta, OsAdapter, TrashReceipt, WatchHandle};
pub use mode::{Mode, RiskTier};
pub use model::*;
pub use scanner::{ScanOpts, ScanReport, Scanner};

/// Errors raised by core operations.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("path is inside a protected root: {0}")]
    ProtectedPath(std::path::PathBuf),
    #[error("path is outside every approved root: {0}")]
    OutsideRoots(std::path::PathBuf),
    #[error("destination already exists: {0}")]
    DestinationExists(std::path::PathBuf),
    #[error("operation not permitted in {mode:?} mode")]
    ModeForbids { mode: Mode },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// Replace the default panic message (which dumps the whole payload — for a
/// malformed PDF that is the font dictionary) with one short line. Panics
/// from third-party parsers are caught at the call site; this only changes
/// what gets printed on the way.
pub fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        let msg: String = msg.chars().take(160).collect();
        let loc = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        eprintln!("warning: internal panic caught{loc}: {msg}");
    }));
}
