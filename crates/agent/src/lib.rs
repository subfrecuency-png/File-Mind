//! Agent library: the scan/hash pipeline and platform adapter selection.
//! The `filemind-agent` binary and the `filemind` CLI both build on this.

/// Changes on every compile of this crate; the CLI compares it with a running
/// agent's to detect a stale daemon after a rebuild.
pub const BUILD_ID: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("FILEMIND_BUILD"));

pub mod actions;
pub mod analysis;
pub mod archive;
pub mod classifier;
pub mod crash;
pub mod fixture;
pub mod incremental;
pub mod jobs;
pub mod pipeline;
pub mod platform;
pub mod rpc;
pub mod rules;
pub mod scheduler;
pub mod semantic;
pub mod shrink;
pub mod telemetry;
pub mod watcher;
