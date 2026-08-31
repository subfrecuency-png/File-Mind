//! "Shrink": reclaim disk space losslessly.
//!
//! Lossless compression cannot beat the entropy of the data, and most disks
//! are full of things that were never compressed well in the first place.
//! FileMind already knows every file's type, hash, project and how cold it
//! is, so it can *measure* how much a given tier of shrink would reclaim
//! before touching anything.
//!
//! Tiers, safest first (see `docs/PHASE9_10_AND_SHRINK_PLAN.md` §3):
//!
//! 1. transparent APFS compression — the file stays a normal file, opens in
//!    every app, bit-identical on read; only the on-disk footprint shrinks;
//! 2. lossless media recompression — JPEG → JPEG XL (bit-exact
//!    reconstruction), PNG → optimised PNG; opt-in per category;
//! 3. cold-project archives — zstd, per-category dictionaries, searchable
//!    index, one-click restore.
//!
//! This module holds the estimator (`estimate`). The rewrite steps come in
//! their own modules, each as a journaled, verified, undoable transaction.

pub mod archive;
pub mod estimate;

pub use estimate::{Estimate, Estimator, FileIn, Probe, ProjectIn};
