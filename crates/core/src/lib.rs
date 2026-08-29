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
pub mod scanner;
pub mod txn;
pub mod versions;
pub mod watch;

pub use adapter::{Entry, FsEvent, LinkKind, NativeMeta, OsAdapter, TrashReceipt, WatchHandle};
pub use mode::Mode;
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
