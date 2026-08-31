# FileMind AI — Technical Architecture & Build Plan

*Working name: FileMind AI · Positioning: "AI Memory for Your Computer" · Philosophy: Organize, Remember, Recover, Protect*

Version 0.1 · August 29, 2026 · Derived from the FileMind AI brief

---

## 1. What we are building, in one paragraph

FileMind AI is a local-first desktop product (macOS, Windows 10/11) that continuously observes a user's file system, builds a durable index of what exists and what has happened to it, classifies files and groups them into projects, finds duplicates and versions, proposes safe organization moves, and can undo any move it made. On top of that index sits a semantic "File Memory": natural-language search ("the contract I edited the week before the Denver trip") that works even when the file has since moved, been renamed, or been archived. Every action passes through a transaction manifest and is reversible. Processing is local by default; cloud AI is an opt-in adapter.

---

## 2. Architecture decisions (with rationale)

### 2.1 Core language: Rust, with Python for R&D only

| Option | Verdict | Why |
|---|---|---|
| **Rust core + Tauri GUI** | **Chosen** | Single static binary per OS, no runtime to ship, safe concurrency for the scanner/watcher, native crates for SQLite (`rusqlite`), file watching (`notify`), embeddings (`ort` for ONNX Runtime), and an embedded vector store (`lancedb`). Tauri gives a small, native-feeling shell on both platforms. |
| Python core + PyInstaller | Rejected for shipping | Fastest to prototype but painful to package (200 MB+ bundles, AV false positives on Windows, slow cold start, GIL limits on the scanner). Kept as a **research sandbox** for classifier/prompt experiments; anything that graduates is ported to Rust. |
| Electron + Node | Rejected | Memory footprint is wrong for an always-on background agent. |

### 2.2 Storage

- **SQLite** (WAL mode) is the system of record: file inventory, events, classifications, projects, transactions, settings. One database per user at `~/Library/Application Support/FileMind/filemind.db` (macOS) or `%LOCALAPPDATA%\FileMind\filemind.db` (Windows).
- **Embeddings live in SQLite too** (`embeddings` table, int8-quantised unit vectors, 384 bytes each) and are scanned brute-force in memory (`core::vectors::VecIndex`, multi-threaded). *As built in Phase 7*: measured 42 ms average / 63 ms worst for top-300 over 500 k vectors on two cores, which clears the p95 < 150 ms target without a second storage engine. LanceDB (the original plan) remains the upgrade path if an index ever passes ~1 M subjects; it would slot in behind the same `search(query, k)` signature.
- **SQLite FTS5** provides the lexical search layer (filenames, paths, extracted text); semantic and lexical results are fused at query time.

### 2.3 AI stack

- **Embeddings**: `bge-small-en-v1.5` or `all-MiniLM-L6-v2` exported to ONNX, run via `ort`. ~30 MB, CPU-only, ~5 ms per chunk. Bundled with the app so day-one search needs no download.
- **Classification**: layered. (1) deterministic rules (extension, path patterns, MIME sniffing); (2) a small ONNX text classifier over extracted text; (3) optional LLM pass through the *AI adapter* for ambiguous files.
- **AI adapter trait**: `Ollama` (local, default if detected), `OpenAI`/`Anthropic`-compatible HTTP (opt-in cloud), `None`. Every adapter call carries only the minimum payload (filename, path fragments, extracted snippet ≤ 2 KB) and logs what was sent.

### 2.4 Process model

Two processes, one binary:

1. `filemind-agent` — headless daemon: watcher, scanner, indexer, scheduler, local API (Unix socket / named pipe, JSON-RPC). Registered with **launchd** (macOS) and **Task Scheduler / Windows Service** (Windows).
2. `filemind` (Tauri) — GUI that talks to the agent over the local API. Also exposes `filemind` as a **CLI** (`filemind scan`, `filemind search "…"`, `filemind undo <txn>`), which is how Phase 1 ships before the GUI exists.

---

## 3. System architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  Presentation      Tauri GUI            CLI            (future) MCP│
└───────────────┬──────────────────────┬──────────────────┬─────────┘
                │        local JSON-RPC over socket / pipe │
┌───────────────▼──────────────────────▼──────────────────▼─────────┐
│  Core (filemind-core crate)                                        │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌────────────┐ ┌──────────┐ │
│  │ Scanner  │ │Classifier│ │ Index  │ │ Duplicates │ │ Versions │ │
│  └──────────┘ └──────────┘ └────────┘ └────────────┘ └──────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌────────────┐ ┌──────────┐ │
│  │ Projects │ │ Archive  │ │Recovery│ │   Search   │ │  Memory  │ │
│  └──────────┘ └──────────┘ └────────┘ └────────────┘ └──────────┘ │
│  ┌──────────────────────┐ ┌──────────────────────────────────────┐ │
│  │ Policy / Mode engine │ │ Transaction Manager (manifest + undo)│ │
│  └──────────────────────┘ └──────────────────────────────────────┘ │
└───────────────┬──────────────────────────────┬─────────────────────┘
                │ OsAdapter trait               │ AiAdapter trait
┌───────────────▼─────────────┐  ┌─────────────▼────────────────────┐
│ macOS adapter  Windows adapter│  │ Ollama · Cloud HTTP · None        │
│ FSEvents/mdfind/Spotlight     │  │                                   │
│ USN Journal/Windows Search    │  └───────────────────────────────────┘
│ Trash/Recycle Bin · launchd/  │
│ Task Scheduler                │
└───────────────────────────────┘
          Storage: SQLite (WAL, FTS5) + LanceDB (vectors)
```

### 3.1 Crate layout

```
filemind/
  crates/
    core/          # domain logic, no OS-specific code
    adapter-macos/ # FSEvents, mdfind, Spotlight metadata, Trash, launchd
    adapter-win/   # ReadDirectoryChangesW / USN, Windows Search, Recycle Bin, Task Scheduler
    ai/            # AiAdapter trait + Ollama / cloud / none impls, ONNX embedder
    storage/       # SQLite schema + migrations, LanceDB wrapper
    agent/         # daemon binary, scheduler, JSON-RPC server
    cli/           # thin client over JSON-RPC
  apps/
    desktop/       # Tauri app (Svelte or React front end)
  research/        # Python notebooks — classifier experiments, eval sets
```

### 3.2 The OsAdapter trait (the portability seam)

```rust
pub trait OsAdapter: Send + Sync {
    fn watch(&self, roots: &[PathBuf], tx: Sender<FsEvent>) -> Result<WatchHandle>;
    fn enumerate(&self, root: &Path, opts: &ScanOpts) -> Result<Box<dyn Iterator<Item = Entry>>>;
    fn native_metadata(&self, path: &Path) -> Result<NativeMeta>;   // Spotlight kMDItem* / Windows props
    fn native_search(&self, query: &str) -> Result<Vec<PathBuf>>;   // mdfind / Windows Search (fallback + bootstrap)
    fn move_to_trash(&self, path: &Path) -> Result<TrashReceipt>;   // never a hard delete
    fn protected_roots(&self) -> Vec<PathBuf>;                      // /System, ~/Library/*, C:\Windows, AppData\*, etc.
    fn register_autostart(&self, enable: bool) -> Result<()>;
    fn is_reparse_or_symlink(&self, path: &Path) -> Result<LinkKind>;
}
```

Everything above this trait is identical on both platforms and is where the test suite lives.

---

## 4. Module specifications

### 4.1 Scanner
- **Inputs**: user-selected roots (default: Desktop, Documents, Downloads, Pictures; never `/` or `C:\` without an explicit opt-in).
- **Passes**: (a) fast metadata walk (path, size, mtime, ctime, inode/file-id); (b) content hashing (BLAKE3, chunked, streamed, throttled to ≤ 20 % of one core while the user is active); (c) text extraction for indexable types (PDF, DOCX, TXT/MD, code, email `.eml`, spreadsheets ≤ 5 MB).
- **Rules**: skip protected roots; never follow symlinks/junctions unless the target resolves inside an approved root; respect `.filemindignore`; hard caps on depth (64) and per-root file count warnings (> 2 M).
- **Watcher**: FSEvents (macOS) / `ReadDirectoryChangesW` with USN Journal catch-up (Windows) feed incremental updates. Debounced 500 ms, batched into the event log.

### 4.2 Index
- Inventory table keyed by a stable `file_id` (inode+device on macOS, NTFS file reference on Windows) so renames/moves preserve identity.
- FTS5 virtual table over `name`, `path_tokens`, `extracted_text`.
- Content hash → `blob_id` mapping enables duplicate and version detection without re-hashing.

### 4.3 Health scoring
A per-root and global 0–100 score composed of: duplicate bytes ratio, Downloads staleness (files > 90 days untouched), naming entropy (`final_v2_FINAL(3).docx`), orphan versions, unclassified ratio, free-space pressure. Score deltas are the product's "before/after" proof and drive the Assist queue.

### 4.4 Classifier
Outputs `category` (Document, Invoice, Contract, Photo, Screenshot, Design, Code, Archive, Installer, Media, Data, Other), `confidence`, `signals[]`. Rule layer runs on every file; ML layer on text-bearing files; LLM adapter only when confidence < 0.6 **and** the user has enabled an adapter. Human corrections are stored as labelled examples and re-applied as rules (path-prefix or name-pattern) — the system learns per user without any training loop.

### 4.5 Duplicates & versions
- **Exact duplicates**: identical BLAKE3 → group. Proposed action: keep the copy in the "best" location (project folder > Documents > Downloads), trash the rest.
- **Near-duplicates / versions**: same stem after normalization (`report`, `report_v2`, `report (1)`, `report copy`) + similar embedding + monotonic mtime → a `version_chain`. Proposed action: never delete; offer "collapse into `report/` with history".
- Image near-dupes (Phase 6+): perceptual hash (dHash) on JPEG/PNG/HEIC.

### 4.6 Projects
Clusters files into projects using path co-location, temporal co-editing (files touched in the same 30-minute windows), shared named entities in extracted text, and embedding proximity. Produces `project` rows with a name suggestion, date range, member files, and an "activity" signal. Projects are the primary navigation unit in the GUI.

### 4.7 Archive
Moves cold projects (no activity > 180 days, configurable) to `~/FileMind Archive/<Year>/<Project>/` as a transaction, leaving an optional `.webloc`/`.lnk` breadcrumb. Archived files stay fully searchable; Memory answers "it's in the archive, here's the link".

### 4.7b Shrink (Phase 11)
Reclaim disk space losslessly, measured before promised. *As built (estimate)*: `core::shrink::estimate` walks the index once (`storage::shrink_rows`, one streamed query with the effective category and the file's cold folder-backed project), sorts every present file into at most one disjoint bucket — tier 3 `cold_archive` (files of a `cold` marker/folder project), tier 1 `apfs` (allow-listed raw/text extensions, untouched ≥ 30 days, ≥ 4 KiB), tier 2 `media_lossless` (JPEG/PNG) — never sensitive files, noise dirs, `Library`, app bundles. Per bucket it keeps a size-weighted reservoir sample (A-Res, 48 files), reads three 64 KiB windows of each and compresses them in memory (zlib ≈ APFS, zstd -19 ≈ the archiver); the mean sampled ratio applied to the bucket's bytes is the estimate. Tier 2 is not probed — it reports documented typical ratios and says `measured: false`. Report cached as JSON in `settings.shrink.estimate` (fresh 24 h); RPC `shrink.estimate` (`refresh`, `cached_only`) and job kind `shrink.estimate`; CLI `filemind shrink estimate [--refresh] [--json]`; Overview "Shrinkable" tile. *As built (tier 1)*: `Step::Rewrite { path, method, hash_before, hash_after_decoded, bytes_before, bytes_after }` (`op: "rewrite"`, serialized with defaults so old journals load). `OsAdapter::{rewrite, rewrite_restore, rewrite_state, on_disk_bytes}` with `Unsupported` defaults; `adapter-macos::apfs` writes `com.apple.decmpfs` (+ `com.apple.ResourceFork`, zlib type 3/4 — encoder and decoder in `apfs::decmpfs`, portable and tested everywhere), then truncates, then sets `UF_COMPRESSED` (the order is load-bearing: flag-before-truncate destroys the data; verified on a real volume). The payload is decoded and compared before it is activated; `HalfDone` (payload written, flag missing) is finished by `rewrite`, never lost; files open by another process (`proc_listpidspath`), locked, linked, > 1 GiB, or with an existing resource fork are refused; a file that would not shrink is left untouched. Restore writes the file's own first byte back at offset 0 — the kernel materialises the plain data fork in place, same inode. Transaction manager: `validate` refuses already-rewritten / half-done files; `execute` re-hashes the file after the rewrite and restores on mismatch (step `failed`); `recover` settles a `running` rewrite from `rewrite_state` + hash; `undo` hash-checks then `rewrite_restore`s in place (an OS-decompressed file counts as restored). Journal gains `update_manifest` (real `bytes_after`, `hash_after_decoded`). Migration `0009_rewrite`: `files.rewrite` set by `note_txn_effects`, cleared by undo or by the scanner when size/mtime change. Suggestion kind `compress_cold_text` (tier 1, `storage::compress_batches`): per (root, bucket) from the cached estimate's ratios (≤ 0.9), ≤ 500 largest files, ≥ 1 MiB saving, macOS only (`FILEMIND_SHRINK_ANYWHERE` for tests). *As built (tier 3, cold archives)*: `core::shrink::archive` is the container — FastCDC content-defined chunks (16/64/256 KiB), each chunk compressed independently with zstd -19 (per-category dictionaries — code/data/text — trained with `zdict` on the project's own small files, stored as blobs in `archive_dicts`), appended to one pack file (`~/FileMind Archive/Projects/<name> (<id>).fmpack`, magic `FMPACK1\0`, all structure in the database). A chunk whose blake3 already exists in *any* archive is referenced, not stored, so a near-copy of an archived project costs almost nothing (`archive_chunks` is the global map; `archive_members` + `archive_member_chunks` rebuild any member). `agent::archive::build` walks the real tree (every file, `.git` and hidden files included; dirs and symlinks recorded; refuses sockets, > 1 GiB members, and — via `preflight`, surfaced as plan problems — any file the index marks sensitive), packs, then **verifies every member decodes to its recorded hash before anything else happens**; only then does the suggestion's single journaled `Trash` step move the original folder to the Trash, after which the members' inventory rows flip to `status='archived'` with `location = archive:<id>#<rel>`. Search includes archived rows (pill + Restore in the app, marker in the CLI); FTS rows and vectors were made while the file was live and are kept. `agent::archive::restore` streams chunks back (packs opened across archives as dedup requires), hash-verifies, never clobbers, and flips rows back to present; undoing the transaction restores the folder from Trash and clears the markers (the pack is kept — it is only ever redundant, never authoritative while the tree exists). RPC `archive.list/show/verify/restore`; CLI `filemind archive list|show|verify|restore [--member] [--to]`. Suggestion kind `archive_cold_project` (tier 1): one per cold project in the cached estimate with ≥ 10 MiB estimated saving, on every platform. Tier 2 follows the same shape.

### 4.8 Transaction Manager & Recovery
The heart of "Protect".
- Every mutating operation (move, rename, trash, archive, restore) is a **transaction** with a manifest written *before* execution: `txn_id`, mode, initiator (user/auto), list of `(file_id, from, to, hash_before)`, and a dry-run diff.
- Execution is idempotent and journaled step-by-step; a crash mid-transaction resumes or rolls back on next start.
- **Undo** replays the manifest in reverse and verifies hashes; if a target was modified since, the conflict is surfaced rather than overwritten.
- **Recovery history** is the union of the transaction log and the file event log: "what did this folder look like last Tuesday?" is a query, and "where did `budget.xlsx` go?" resolves through `file_id` lineage.
- Deletion is always **trash/Recycle Bin**, never `unlink`.

### 4.9 Search & Memory
- **Search**: hybrid — FTS5 BM25 + cosine over the in-memory vector index → reciprocal-rank fusion (k = 60) → metadata filters (type, date, project, folder, size, sensitive, duplicates). *As built*: `core::query` parses the natural-language part deterministically (seasons, months, "last 3 weeks", kinds, sizes, "in project X", "in downloads"); when the filters leave nothing, the search is retried with filters relaxed and says so. Embedder: `bge-small-en-v1.5` (ONNX, CLS pooling, query prefix) downloaded once with `filemind model download`; a hash-of-ngrams embedder stands in until then. Files are embedded from name + path words + category + the first 2 KB of extracted text (`file_text`); sensitive files are never embedded. Benchmark (`crates/agent/tests/semantic.rs`): top-5 hit rate 100 %, top-1 97 % on 40 paraphrased queries, ~10 ms per query.
- **Memory** (semantic layer over search): a `memory_notes` table where the system and user attach facts to files and projects ("sent to accountant 2026-03-04", "final version for client"), plus an event timeline. Natural-language queries are parsed by a small local intent model (or the AI adapter) into structured filters + free-text; results are explained ("matched because: edited March 3–5, project *Q1 Taxes*, mentions 'Schedule C'").

### 4.10 Policy / Mode engine
| Mode | Reads | Proposes | Executes |
|---|---|---|---|
| **Observe** | ✔ | ✔ (shown as suggestions) | ✘ |
| **Assist** | ✔ | ✔ | Only after per-transaction approval |
| **Automate** | ✔ | ✔ | Only actions on an explicit allow-list: e.g. *move screenshots > 30 days old from Desktop to `Pictures/Screenshots`*, *sort Downloads installers into `Downloads/Installers`*. Never trash, never touch project folders, batch size ≤ 50, always undoable, digest emailed/notified. |

Mode is global with per-root overrides. Every rule has a written rationale and risk tier; only tier-0 rules are eligible for Automate.

*As built in Phase 9*: the allow-list is code, not configuration — `core::rules::KINDS` = `archive_stale_downloads`, `collapse_versions`, `trash_exact_duplicates`, each with validated, bounded parameters (per-run cap ≤ 500, minimum ages, "keeper outside Downloads"). A rule is created in **preview**; every scheduler tick records a dry run (`rule_runs.dry_run = 1`, the manifest it would have executed) and the Automate screen / `filemind rule` shows "in the last 7 days this rule would have moved N files". **Arm** is accepted only after `automate.preview_days` (default 7) *and* at least one dry run; an armed rule executes only while the mode is Automate, as a normal journaled transaction with `Initiator::Rule` (undo works like any other). Any validation problem, failed step, or plan above `pause_above` **pauses** the rule with the reason; arming again is a deliberate act. The brief's "never trash" for tier 0 became "trash only exact copies whose keeper is verified present" — the safest kind of trash there is, and still reversible.

---

## 5. Data model (SQLite)

```sql
files(file_id PK, root_id, path, name, ext, size, mtime, ctime, birthtime,
      blob_id NULL, kind, is_link, status ENUM(present,moved,trashed,archived,missing),
      first_seen, last_seen)
blobs(blob_id PK, blake3, size, text_extracted BOOL, embedding_id NULL)
file_events(event_id PK, file_id, ts, type ENUM(created,modified,renamed,moved,deleted,restored),
            from_path, to_path, source ENUM(watcher,scan,txn))
classifications(file_id, category, confidence, signals JSON, source ENUM(rule,ml,llm,user), ts)
projects(project_id PK, name, suggested_name, start_ts, end_ts, activity_score, status)
project_files(project_id, file_id, role, confidence)
version_chains(chain_id PK, canonical_file_id)
version_members(chain_id, file_id, ordinal, mtime)
duplicate_groups(group_id PK, blob_id, keeper_file_id NULL)
transactions(txn_id PK, mode, initiator, rule_id NULL, state ENUM(planned,running,done,undone,failed),
             created_ts, executed_ts, manifest JSON)
txn_steps(txn_id, step_no, file_id, from_path, to_path, hash_before, state)
memory_notes(note_id PK, subject_type ENUM(file,project), subject_id, text, source, ts)
health_snapshots(ts, root_id NULL, score, components JSON)
settings(key PK, value JSON)
```
`embeddings(subject PK, model, dim, vec BLOB int8, input_hash, ts)` — `subject` is a `file_id` or `note:<id>`; `file_text(file_id PK, mtime, head)` keeps the first 2 KB of extracted text; `notes_fts` mirrors `memory_notes`; `ai_audit` gained `snippet_hash, local, ok, latency_ms`.

---

## 6. Privacy & security posture

- All indexing, hashing, embedding and classification run locally; the agent makes **zero network calls** unless an adapter or update check is enabled.
- Cloud adapter payload policy: filename + ≤ 2 KB snippet, never full files, never paths outside approved roots; every call is logged in `settings.ai_audit` and viewable in the GUI.
- Database encrypted at rest with SQLCipher (key in Keychain / DPAPI) — *as built in Phase 9.5*: `filemind-storage` feature `encrypt` (on for macOS/Linux builds), raw 32-byte key in the login Keychain (`ai.filemind` / `db-key`, `security-framework`) for the real database and a 0600 key file for any other path (tests, copies); a plaintext database is converted once on open via `sqlcipher_export`, the plaintext copy kept as `filemind.db.pre-sqlcipher` until a later process opens the encrypted file, then moved to the Trash. `filemind dev db-key` prints the key for the `sqlcipher` shell; `FILEMIND_DB_KEY` overrides, `FILEMIND_PLAINTEXT_DB=1` opts out. Windows stays plaintext until its OpenSSL build is sorted out.
- Sensitive-content detector (rule-based: SSN/card patterns, `.env`, keys) marks files `sensitive=true`, which excludes them from any adapter call and from text preview.
- Code signing + notarization (macOS) and Authenticode (Windows) from the first beta; auto-update via Tauri updater with signed manifests. *As built in Phase 9*: `.github/workflows/release.yml` (tag `v*`, both Apple targets, Developer ID + notarization via `tauri-action`, minisign-signed `latest.json`); `tauri-plugin-updater` behind Settings → Updates. The agent and the CLI ship as Tauri sidecars in `FileMind.app/Contents/MacOS/` (`scripts/build-sidecar.sh`, ORT linked statically); launchd "Start at login" is written by the app and re-pointed on every launch; the agent exits cleanly on SIGTERM.

## 7. Core safety rules → enforcement points

| Rule from brief | Enforced by |
|---|---|
| Never permanently delete automatically | `OsAdapter::move_to_trash` is the only deletion primitive; no `remove_file` call exists in `core` (CI lint). |
| Never mass-move without a transaction manifest | Transaction Manager is the only path to the adapter's move/rename; manifest written and fsynced before step 1. |
| Never overwrite existing files | Move steps check destination existence; conflicts produce `name (FileMind 2).ext` **and** a visible conflict note — never silent. |
| Never silently rename conflicts | Same as above; every rename is a `txn_step` shown in the approval diff. |
| Never follow unknown symlinks/junctions | Scanner resolves link targets; anything outside approved roots is recorded as `is_link` and not traversed. |
| Never modify protected OS/app internals | `protected_roots()` denylist applied at scanner, transaction planner, and adapter level (defense in depth). |

---

## 8. Phased delivery plan

Each phase ships something usable and ends with a written go/no-go check.

| Phase | Weeks | Deliverable | Exit criteria |
|---|---|---|---|
| **0. Foundations** | 1–2 | Repo, crate skeleton, CI (macOS + Windows runners), SQLite migrations, OsAdapter stubs, signing certs ordered | `cargo test` green on both OSes; empty agent starts/stops via launchd & Task Scheduler |
| **1. Read-only scanner (CLI)** | 3–5 | `filemind scan <root>` builds inventory + hashes; `filemind status` | 500 k files scanned < 10 min on a laptop; zero writes outside the DB; protected roots and links proven skipped by tests |
| **2. Live index + lexical search** | 6–8 | Watcher, event log, FTS5, `filemind search`, stable `file_id` across rename/move | Rename/move tracked correctly in 100/100 fixture scenarios; incremental update < 1 s |
| **3. Health scoring + duplicates + versions** | 9–11 | Health score, dup groups, version chains, first Observe-mode suggestions | Score reproducible; dup detection 100 % precision on exact, ≥ 90 % on version chains vs. labelled set |
| **4. Classification** | 12–14 | Rule + ONNX classifier, user corrections → rules, sensitive-content detector | ≥ 85 % top-1 on the eval set; corrections persist and re-apply |
| **5. Project detection** | 15–17 | Project clustering, naming suggestions, timeline | Users recognise ≥ 70 % of auto-projects in dogfood interviews |
| **6. Transactions, recovery & rollback (Assist mode)** | 18–21 | Transaction Manager, approval diff, undo, crash recovery, archive | Chaos test: kill −9 mid-transaction 200×, zero data loss; every txn undoable with hash verification |
| **7. Semantic search + Memory** | 22–25 | ONNX embedder, LanceDB, hybrid ranking, memory notes, NL query parsing, AI adapter (Ollama + cloud) | p95 query < 150 ms on 500 k files; top-5 hit rate ≥ 80 % on a 200-query benchmark |
| **8. GUI (Tauri)** | 26–31 | Onboarding, roots picker, health dashboard, project browser, search, approval queue, undo history, mode switch, AI audit log | Full flow usable without CLI; accessibility pass |
| **9. Packaging & Automate mode** | 32–34 | Signed/notarized installers, auto-update, Automate allow-list rules, SQLCipher | Clean install→uninstall on fresh macOS 14+/Win 10/11 VMs; AV scan clean |
| **10. Public beta** | 35–38 | Landing page, docs, telemetry (opt-in, aggregate only), feedback loop | 100 external users, crash-free rate ≥ 99 %, no data-loss reports |

Total ≈ 9 months for one strong engineer; ≈ 5–6 months with two (GUI can run parallel from Phase 6).

---

## 9. Testing strategy

- **Fixture file systems**: reproducible tarballs (10 k / 100 k / 1 M entries) with planted duplicates, version chains, symlink loops, junctions, unicode names, long paths (> 260 chars on Windows), locked files.
- **Property tests** on the Transaction Manager: for any manifest, `apply → undo` is the identity on the fixture tree.
- **Chaos harness**: process kill, disk-full, permission denied, root removed mid-scan.
- **Eval sets** (`research/`): classification (2 k labelled files), search (200 queries with gold answers), projects (10 real dogfood trees, hand-labelled).
- **Platform matrix in CI**: macOS 14/15 (Apple Silicon + Intel), Windows 10 22H2, Windows 11.

## 10. Key risks and mitigations

| Risk | Mitigation |
|---|---|
| Scanner hurts battery/perf and gets uninstalled | Adaptive throttling, idle-only hashing, "pause while on battery" default, visible resource meter |
| Windows file-identity edge cases (FAT/exFAT drives, OneDrive placeholders) | Treat non-NTFS and cloud-placeholder roots as *observe-only*; hash-based identity fallback |
| Users don't trust automated moves | Assist mode is the default forever; Automate is opt-in per rule with a mandatory 7-day Observe preview showing what it *would* have done |
| Embedding quality on non-English content | Swap to `multilingual-e5-small` behind the same interface; benchmark in Phase 7 |
| Scope creep (cloud sync, mobile, team features) | Out of scope for v1 by decision; captured in a parked-ideas list |

## 11. Immediate next steps

1. Create the repo with the crate layout above and CI on both OS runners (Phase 0).
2. Build the 10 k-entry fixture tree with planted duplicates, versions, links and protected paths.
3. Implement `Scanner` behind `OsAdapter` for macOS first, Windows one week behind.
4. Order Apple Developer and Authenticode certificates now — lead time is weeks.
5. Start the `research/` classification eval set from Ryan's own file tree (labelled locally, never uploaded).
