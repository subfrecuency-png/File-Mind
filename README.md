# FileMind AI

**AI Memory for Your Computer.** A local-first file organizer, recovery system and semantic "File Memory" for macOS and Windows 10/11.

> Organize · Remember · Recover · Protect

Status: **Phase 5 — it knows what belongs together.** Project detection groups files into repos (marker files), project folders (descending through container folders like Clients/), and sessions/topics of loose files by when they were touched and what they are called; names you give projects survive re-analysis. Plus everything from Phase 4: Live-watched inventory, health score, duplicates, version chains, Observe-mode suggestions, and now a rule-based classifier with text extraction (PDF/DOCX/PPTX/XLSX/text/code) feeding full-content search, a sensitive-content detector, and corrections that become rules. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full architecture and the phased plan.

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

## Build and try it

```sh
cargo build --workspace
cargo test --workspace

filemind() { ./target/debug/filemind "$@"; }
filemind status                       # where the database is, mode, suggested roots
filemind roots add ~/Downloads        # register a folder (nothing scanned yet)
filemind scan                         # read-only metadata scan of every root → inventory
filemind hash --minutes 5             # content hashes, throttled to ~20 % of one core
filemind search "offer sheet"         # lexical search over names and paths
filemind analyze                      # rebuild duplicates, versions, health, suggestions
filemind health                       # 0–100 score and what is costing points
filemind dupes                        # exact-duplicate groups, keeper chosen
filemind versions                     # report_v1 / report_v2 … chains, newest marked
filemind suggest                      # what FileMind would do (observe mode: it never acts)
filemind classify --minutes 5         # categorise + extract text (throttled); search then finds contents
filemind categories                   # files per category, sensitive count, your rules
filemind classify show <file>         # category, confidence, and the signals behind it
filemind classify set <file> invoice --scope folder   # correct it; --scope folder|ext|name makes a rule
filemind projects                     # detected projects, most active first
filemind project show 3               # a project's files, dates and category mix
filemind project rename 3 "Calcium deal"
filemind project of ~/Downloads/x.pdf # which project a file belongs to
filemind agent start                  # run the agent in the foreground (watcher + scheduler + socket)
filemind agent install                # or: start filemind-agent at login (launchd)
filemind agent status                 # is it running? watcher counters
filemind agent stop                   # ask a running agent to stop
filemind info ~/Downloads/x.pdf       # what FileMind and Spotlight know about one file
filemind dev fixture /tmp/fx --entries 10000   # synthetic tree for tests/benchmarks
```

Every scan is read-only on the scanned folder; the only thing written is the
database under `~/Library/Application Support/FileMind/` (macOS) or
`%LOCALAPPDATA%\FileMind\` (Windows). `roots remove` forgets the index only.

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
