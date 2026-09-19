# Shrink — storage hygiene, not the headline

**Status:** Product positioning (2026-09-17)  
**Audience:** Engineering, site copy, Grok ops  
**Related:** `VAULT_DESIGN_SPIKE.md`, `PHASE9_10_AND_SHRINK_PLAN.md`

## Headline vs hygiene

| Product story | Feature |
|---|---|
| **Protect** (headline) | Vault Seal / Unseal — custody for secrets |
| **Organize · Remember · Recover** | Index, search, projects, rules, undoable Archive |
| **Storage hygiene** (Settings) | **Shrink** — reclaim disk for non-secret cold data |

Shrink is useful. It is **not** a Vault competitor and must not be marketed as the breakthrough.

## Where Shrink lives

**Settings → Storage** (working name: Storage hygiene).

User-facing promise: reclaim space safely, reversibly, without touching secrets.

## What Shrink is

1. **APFS transparent compression** — file stays a normal file; bit-identical on read; reversible rewrite. Best for text/code/docs; skip already-compressed media.
2. **Cold packs (`.fmpack`)** — pack untouched projects into a searchable archive with one-click restore. Dedup/chunking optional later.

Optional later tiers (lossless recompress, deltas inside packs) stay behind the same Settings surface. Lossy recompress stays explicit opt-in only.

## What Shrink is not

- Not custody — compressed or packed files are still plaintext on disk.
- Not the place for PEMs, `.env`, wallets, tax PDFs, or other sensitivity hits — those go to **Vault Seal**, never plain Archive / cold pack first.
- Not a reason to delay Protect/Vault work.

## Rules of engagement

- Measure first (`shrink estimate`); promise nothing until numbers land.
- Every rewrite/pack is a journaled transaction with undo.
- Never Shrink: sensitive labels, app bundles, `Library`, open files, unverified formats.
- If a tree is both cold and sensitive: **seal first**, pack later (if ever) as one designed txn — default is seal only.

## Copy guidance

- Site / changelog: **Shrink = storage; Vault = trust.**
- Health UI: “Shrinkable” tile is fine next to reclaimable space — secondary to Protect cues.
- Do not frame Shrink as competing with Protect/Vault in demos or kickoff prompts.

## Build order (when scheduled)

Estimate job → APFS compress → cold packs → optional lossless media. See Phase 11 sketch in `PHASE9_10_AND_SHRINK_PLAN.md`. Vault V0 stays on its own track (cloud agent + Grok 3 review).
