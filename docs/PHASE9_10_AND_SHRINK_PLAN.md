# FileMind — Phase 9, Phase 10 and the "Shrink" exploration

*Kickoff document for a fresh chat. Written 2026-08-30 after Phase 8 was verified on Ryan's Mac.*

## 0. Where things stand (read this first)

- Repo: `~/Documents/filemind` on Ryan's MacBook Air (Apple Silicon, Rust 1.98, Node 22). Latest commit `e60b5fe`. Cloud working copy lived in `/home/claude/filemind/repo` in the previous chat; a new chat starts from the Mac copy (stage `~/Documents/filemind` minus `target/`, `node_modules/`, `.git/`).
- Phases 0–8 are complete and verified on real data (188k files in `~/Downloads`, plus `~/fm-sandbox`). `claude/STATUS.md` (project doc) has the running log; `docs/ARCHITECTURE.md` is the design with as-built amendments.
- Workspace crates: `core` (domain: scanner, classify, extract, projects, versions, health, txn, query, rank, vectors), `storage` (SQLite, migrations 0001–0006, FTS5, journal, semantic), `ai` (bge-small via `ort`, hash fallback, Ollama + Anthropic adapters), `adapter-macos` (also compiles as "posix" on Linux), `adapter-win` (stubs), `agent` (daemon: watcher, scheduler, JSON-RPC over Unix socket, actions, semantic engine, jobs gate), `cli` (`filemind`). Desktop app in `apps/desktop` (Tauri 2 + Vite/TS, its own cargo workspace, dev port 14210).
- Conventions that must survive: the six safety rules (no hard deletes — CI lint forbids `fs::remove_*` outside the adapter except lines annotated `filemind:own-file`; transactions journaled before execution; undo hash-verified; mode gate observe/assist/automate). `cargo fmt`, `clippy -D warnings`, tests on macOS/Windows/Ubuntu in CI. Build stamp = hash of agent sources (`crates/agent/build.rs`); CLI and app refuse to talk to an agent with a different stamp.
- Working rhythm that has been productive: build and test in the cloud, ship a tarball, `device_commit_files` into `~/Documents/filemind/_to_delete/`, `tar --overwrite -xzf` in place, commit as Ryan, then Ryan runs the commands and pastes terminal output for verification. Git lock files in that mount can't be deleted — move them into `_to_delete/`. Ryan should empty `_to_delete/` at some point.
- Ryan's AI setup: Ollama with `nemotron-3-super:cloud` (hosted by Ollama; FileMind labels it `ollama-cloud`/CLOUD in the audit log) and `llama3.2:3b` local. Anthropic adapter defaults to `claude-opus-5` if he adds a key.

## 1. Phase 9 — packaging & Automate mode (plan weeks 32–34)

Goal: a `.app` Ryan double-clicks, that runs its agent without a terminal, updates itself, and can run a small set of rules unattended — each rule having *shown its work* for a week first.

### 9.1 Bundle the agent as a sidecar
- `tauri.conf.json` → `bundle.externalBin: ["binaries/filemind-agent"]`; Tauri expects `binaries/filemind-agent-aarch64-apple-darwin` (and `-x86_64-apple-darwin`). Add a `scripts/build-sidecar.sh` that `cargo build --release -p filemind-agent` and copies with the triple suffix. Also bundle the `filemind` CLI the same way (or expose `filemind` via a symlink installer step, optional).
- `agent_binary()` in `src-tauri/src/lib.rs` already probes `Contents/Resources`; sidecars land in `Contents/MacOS/`, so add that candidate. Prefer `tauri-plugin-shell`'s `sidecar()` API for spawning so entitlements/paths are handled.
- Model: keep downloading bge-small on first run (133 MB) but do it *from the app* with a progress bar (Settings → Search index → Download). Ship a quantised int8 ONNX later if size matters (convert with `onnxruntime.quantization.quantize_dynamic`; re-pin blake3 hashes in `ai::embed::BGE_SMALL`). ORT dynamic library: `ort` with `download-binaries` links `libonnxruntime.dylib` — make sure it is inside the bundle (`bundle.resources` or `macOS.frameworks`) and code-signed; otherwise Gatekeeper rejects it.

### 9.2 launchd from inside the app
- `adapter-macos::launchd` already writes `~/Library/LaunchAgents/ai.filemind.agent.plist` (`filemind agent install`). Point the plist at the sidecar path inside the app bundle; re-write it on every app launch (path changes when the app moves). Add `KeepAlive` + `ThrottleInterval`, log to `~/Library/Logs/FileMind/agent.log`, and a "Start at login" toggle in Settings.
- Handle SIGTERM in the agent (currently only the stop-file): install a signal handler (the `ctrlc` crate or `signal-hook`) that sets the stop flag so launchd unload is clean.
- Windows: `schtasks` registration and a real Recycle Bin (`IFileOperation`) are still stubs — decide whether Windows ships in beta or is explicitly "macOS first".

### 9.3 Signing, notarization, updater
- Developer ID Application cert; `tauri build` with `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID` → Tauri signs and notarizes the `.dmg`. Hardened runtime entitlements: none exotic needed (no JIT, no camera); the sidecar and `libonnxruntime.dylib` must be signed with the same identity.
- `tauri-plugin-updater` with a static JSON manifest (GitHub Releases works); minisign keypair via `tauri signer generate`; Settings → "Check for updates". The agent is restarted by the app after an update (stale-build detection already exists).
- CI: a `release.yml` on tags that builds the sidecar + app on `macos-14`, signs, notarizes, uploads the dmg and the updater manifest.

### 9.4 Automate mode with a mandatory preview
- New table `rules(rule_id, kind, params JSON, tier, state: preview|armed|paused, created_ts, armed_ts)` and `rule_runs(run_id, rule_id, ts, dry_run, txn_id NULL, manifest JSON, summary)`.
- Allow-listed **tier-0** rule kinds only (the brief's definition: reversible, low-blast-radius): `archive_stale_downloads(older_than_days, max_items_per_run)`, `collapse_versions(only_weak_markers=true)`, `trash_exact_duplicates(min_bytes, keeper_must_be_outside_downloads=true)`. Everything else stays Assist.
- **7-day observe preview**: a rule starts in `preview`; every scheduler tick evaluates it and records a *dry run* (`rule_runs.dry_run=1` with the manifest it would have executed). The app's Automate screen shows "In the last 7 days this rule would have moved N files (list)". Only after `now - created_ts ≥ 7 days` **and** the user presses Arm does it execute; each execution is a normal journaled transaction with `Initiator::Rule`, capped by `max_items_per_run`, and appears in History with undo. `txn::permitted` already gates automate to tier-0.
- Safety valve: any conflict/failed step pauses the rule; a rule that would touch > N files in one run pauses itself and asks.
- CLI: `filemind rule add|list|preview|arm|pause|rm`; RPC `rules.*`; desktop: Settings → Automate (or its own screen) with the would-have list.

### 9.5 SQLCipher
- `rusqlite` feature `bundled-sqlcipher-vendored-openssl` instead of `bundled`; key from the macOS Keychain (`security-framework` crate) generated on first run; `PRAGMA key` right after open; one-time migration: `ATTACH` new encrypted DB, `sqlcipher_export`, swap files (keep the plaintext copy renamed `.pre-sqlcipher` until the next successful open, then move it to Trash — never `remove_file`).
- The `ai.cloud_key` setting stops being plaintext at rest as a consequence. Also consider moving that key to the Keychain outright.

### 9.6 Loose ends worth folding into Phase 9
- `println!` EPIPE panic when piping CLI output (`| head`): reset SIGPIPE to default at CLI start.
- Desktop: long RPCs (scan, analyze, embed) block the window; move to `embed.run`-style background jobs with a `jobs.status` RPC and a progress bar. Notes screen. Tray icon with health score.
- Whole-folder duplicate suggestion ("creditos-v1 is a copy of creditos"): hash-of-hashes per directory; propose trashing the whole older tree as one step.
- Finder "Put Back" for FileMind-trashed items (`.DS_Store` metadata is private; the practical route is a `Put back` action in History, which already exists as undo).

Exit criteria (from the brief): clean install → uninstall on fresh macOS 14+ VMs, AV scan clean, agent survives reboot, an armed rule runs after its preview week and every run is undoable.

## 2. Phase 10 — public beta (plan weeks 35–38)

- Landing page + docs (what it does, the six rules, privacy: what stays local, what an adapter sees, the audit log). Screenshots from the real app.
- Telemetry: **opt-in, aggregate only** — health score bucket, counts of suggestions applied/undone, crash-free sessions; never paths, names or content. Implement as a daily POST with a schema doc in the repo; a Settings toggle off by default.
- Crash reporting: panic hook writes a local report; user chooses to send.
- Feedback loop: in-app "Send feedback" that opens a prefilled GitHub issue / mailto with version and (optionally) the local report.
- Beta cohort: 100 users, macOS first. Track crash-free rate ≥ 99% and zero data-loss reports; a "data-loss" is any undo that cannot restore a file.
- Pricing/licensing decision is out of scope for engineering but blocks the landing page copy.

## 3. Exploration — "Shrink": can FileMind compress your files?

Ryan's prompt: *Richard accidentally invents a breakthrough "middle-out" data compression algorithm that shrinks file sizes drastically without losing any quality.* The honest version of that for FileMind is below — no Weissman-score miracles, but a real, safe feature that can reclaim a lot of space losslessly, because most disks are full of things that were never compressed well in the first place.

### 3.1 What is physically possible
- Lossless compression cannot beat the entropy of the data. Already-compressed formats (JPEG, MP4, ZIP, HEIC, most PDFs) give ~0–5% with generic compressors. Text, code, CSV, logs, uncompressed TIFF/BMP/WAV, SQLite files, Office XML (already deflate, but poorly) give 2–10×.
- The real wins come from **format-aware** recompression and **deduplication across similar files**, not from a better general-purpose algorithm. That is where FileMind is unusually well placed: it already knows every file's type, hash, version chain, project, and how cold it is.

### 3.2 Tiers of "shrink", safest first
1. **Transparent APFS compression** (macOS): the same mechanism Apple uses for system files (`decmpfs`, LZFSE/ZLIB). The file stays a normal file, opens in every app, bit-identical on read; only the on-disk footprint shrinks. Implemented with the same calls `afsctool`/`ditto --hfsCompression` use. Typical: text/code/docs 2–4×, media 1.0× (skip). Fully reversible (rewrite uncompressed). *This is the "it just got smaller and nothing changed" feature.*
2. **Lossless media recompression**: JPEG → JPEG XL in lossless-JPEG-transcode mode (~20–30% smaller, and the original JPEG can be reconstructed **bit-exactly**); PNG → optimised PNG (`oxipng`, 10–40%); GIF/BMP/TIFF → PNG/WebP lossless. Requires the format to change (a `.jxl` isn't a `.jpg`), so this is opt-in per category, with the bit-exact reconstruction verified (hash) before the original goes to Trash via a normal transaction.
3. **Cold archive tiering**: for projects untouched > N months, pack the tree into a zstd archive (`-19`, long window, per-category dictionaries trained on the user's own files — that alone is often +20% over plain zstd), keep an index so search still finds files *inside* the archive, and offer one-click restore. Deduplicate chunks across archives with content-defined chunking (FastCDC) so `creditos-v1` and `creditos` cost one copy.
4. **Version-chain delta storage** (Phase 3's chains): store `report_v1..v6` as deltas against `v7` (xdelta/zstd `--patch-from`). Big win for design/CAD/PSD chains, but the riskiest: only in the archive tier, never on live files.
5. **Lossy** (HEIC/AVIF for photos, re-encoded video): *only* behind a separate, explicit, per-folder opt-in with a side-by-side preview, never suggested by default. The brief's "without losing any quality" means tiers 1–4 are the product; tier 5 is a setting some users will want.

### 3.3 How it fits FileMind's model
- **Measure first, promise nothing**: a `shrink estimate` job samples files per category, compresses a few KB each, and reports "Shrink could reclaim ~X GB: 9.1 GB in code/text via APFS compression, 3.2 GB in photos via lossless JXL, 14 GB by archiving 3 cold projects". Health dashboard gets a "Shrinkable" tile next to "Reclaimable".
- **Every shrink is a transaction**: new `Step::Rewrite { path, method, hash_before, hash_after_decoded }` — journaled, verified (decode → hash equals `hash_before`) before the original is replaced, and **undoable** (decompress / restore from Trash within the retention window). Tier-1 APFS rewrites are in-place and reversible without Trash at all.
- **Search keeps working**: archived files keep their FTS rows and vectors with a `location = archive:<id>#<member>` marker; results show an "in archive" pill and a Restore button.
- **Never touch**: sensitive files, files inside app bundles / `Library`, anything in a noise dir, files open by another process, files whose format isn't on the verified allow-list.

### 3.4 Suggested build order (Phase 11, ~3 weeks)
1. `core::shrink::estimate` + `filemind shrink estimate` + dashboard tile (2 days). Ground truth on Ryan's disk before writing a single rewrite.
2. Tier 1 APFS compression as a `Rewrite` step with undo (`adapter-macos::apfs`), suggestion kind `compress_cold_text` (tier 1 risk), preview shows per-file before/after (4 days).
3. Tier 3 cold-project archive with zstd + dictionary + in-archive search + restore (6 days). This is the headline number.
4. Tier 2 JPEG-XL/PNG lossless with bit-exact verification, opt-in (4 days).
5. Tier 4 deltas inside archives (stretch).

Expected on Ryan's Downloads (54.7 GB, 60k photos, 52k code files, one 34 GB stale set): tier 1 ≈ 1–2 GB, tier 3 ≈ 10–20 GB depending on how much of that 34 GB is already-compressed media, tier 2 ≈ 2–4 GB. Not middle-out, but real, and every byte of it reversible.

## 4. Kickoff prompt for the new chat

> Continue FileMind from `claude/STATUS.md` and `claude/FileMind_Phase9_10_and_Compress_Plan.md` (same text as `docs/PHASE9_10_AND_SHRINK_PLAN.md` in the repo). Start Phase 9 at section 9.1 (sidecar bundling) and work through 9.6 in order, keeping the six safety rules, CI lint and build-stamp conventions. Same workflow as before: build and test in the cloud, ship tarballs to `~/Documents/filemind/_to_delete/`, commit as Ryan, and let Ryan verify on his Mac. When Phase 9 is verified, do Phase 10, then propose Phase 11 "Shrink" starting with the estimate job.
