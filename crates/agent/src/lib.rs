//! Agent library: the scan/hash pipeline and platform adapter selection.
//! The `filemind-agent` binary and the `filemind` CLI both build on this.

pub mod incremental;
pub mod pipeline;
pub mod platform;
pub mod rpc;
pub mod scheduler;
pub mod watcher;
