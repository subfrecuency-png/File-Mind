# FileMind AI

**AI Memory for Your Computer.** A local-first file organizer, recovery system and semantic "File Memory" for macOS and Windows 10/11.

> Organize · Remember · Recover · Protect

Status: **Phase 0 — foundations.** See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full architecture and the phased plan.

## Layout

```
crates/core           domain logic — scanner, modes, transaction manifests, the OsAdapter seam
crates/storage        SQLite (WAL + FTS5) schema, migrations; LanceDB later
crates/ai             AiAdapter trait, payload caps, ONNX embedder/classifier later
crates/adapter-macos  FSEvents / Spotlight / Trash / launchd
crates/adapter-win    USN Journal / Windows Search / Recycle Bin / Task Scheduler
crates/agent          filemind-agent daemon
crates/cli            filemind CLI
apps/desktop          Tauri GUI (Phase 8)
research/             Python notebooks and eval sets (never shipped)
```

## Build

```sh
cargo build
cargo test --workspace
cargo run -p filemind-cli -- status
cargo run -p filemind-cli -- scan ~/Downloads --dry-run
```

## Core safety rules

These are enforced in code and CI, not just documented:

1. Never permanently delete user files automatically — `OsAdapter::move_to_trash` is the only deletion primitive; CI fails if `fs::remove_*` appears in core/agent/cli.
2. Never mass-move without a transaction manifest — the transaction manager is the only caller of `rename_no_clobber`.
3. Never overwrite existing files — `rename_no_clobber` refuses if the destination exists.
4. Never silently rename conflicts — every rename is a manifest step shown in the approval diff.
5. Never follow unknown symlinks/junctions recursively — the scanner records links and does not traverse them.
6. Never modify protected OS folders or app internals — `protected_roots()` is applied at scanner, planner and adapter level.

## Modes

`observe` (default, read-only) → `assist` (per-transaction approval) → `automate` (tier-0 allow-listed rules only).
