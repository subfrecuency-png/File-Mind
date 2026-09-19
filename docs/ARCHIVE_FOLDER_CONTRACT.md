# Archive folder contract

**Status:** Product / ops contract (2026-09-18)  
**Audience:** FileMind engine, File Mind Grok, site copy  
**Related:** `GROK_FILEMIND_OPS.md`, `VAULT_DESIGN_SPIKE.md`, `SHRINK_ROLE.md`

## What Archive is

**Archive** is FileMind’s reversible home for **non-secret** clutter — stale Downloads, cold project trees, version keepers you still want findable.

It is organization + recoverability, **not** custody. Plaintext in Archive is still plaintext.

Typical landing place (user-visible): something like `~/FileMind Archive/` (exact path is an app setting; this doc is about rules, not branding).

## What may go into Archive

Allowed when labels and signals say **not** sensitive:

- Stale downloads and duplicates (keeper rules already exist)
- Cold projects / folders untouched past the rule threshold
- Non-secret clutter Grok/rules triage out of Desktop or Downloads

Every move is a journaled transaction with undo. Trash remains the only delete primitive.

## What must never go into Archive

Anything with sensitivity labels:

`secret` · `credential` · `pii` · `financial` · `health`

…or tier-0 hits (e.g. `*.pem`, `.env`, `id_rsa*`, `BEGIN PRIVATE KEY`, `~/.ssh/` exports).

Those are **Seal candidates** → Vault. Moving them into Archive is a product failure (sync/backup/stolen disk still see plaintext).

## Archive vs Vault vs Shrink

| Path | Purpose | At rest |
|---|---|---|
| **Archive** | Organize cold non-secret files | Plaintext in folder tree |
| **Vault Seal** | Protect secrets (headline Protect) | Ciphertext (`fmseal`); metadata-only search |
| **Shrink** (Settings → Storage) | APFS compress / `.fmpack` cold packs | Still not custody — never pack secrets first |

Optional later: sealed objects may get Finder **stubs** under an Archive/Vault alias folder — stubs only, never plaintext. Default for secrets: seal first; do not Archive-then-hope.

## Grok / Assist rules

1. Sensitivity hit → queue **Seal**, never Archive.
2. Cold + non-secret → Archive (or Shrink estimate for packs).
3. Ambiguous → Observe / ask; do not Archive by default.
4. After Archive, report paths and counts only — never file bodies that might be sensitive.

## Automate

Tier-0 Archive rules (e.g. `archive_stale_downloads`) stay behind preview → arm. They must **skip** sensitivity hits; a rule that would Archive a seal candidate pauses and asks.

## Acceptance checks

- [ ] Classifier/Grok cannot Archive a tier-0 credential without an explicit override Ryan approved.
- [ ] Docs and UI copy say Archive = clutter home, Vault = trust.
- [ ] Shrink packs never include sealed plaintext or unlabeled secrets.
