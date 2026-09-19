# Grok × FileMind — operating rules

**Status:** Team ops (2026-09-17)  
**Audience:** File Mind Grok Leader + specialists; FileMind engine owners  
**Related:** `VAULT_DESIGN_SPIKE.md`, `SHRINK_ROLE.md`

## Division of labor

| Who | Owns | Does not own |
|---|---|---|
| **FileMind** | Index, search, classify, rules, transactions, Seal/Unseal, Shrink (Settings → Storage) | One-off human judgment about messy Desktop/Downloads |
| **File Mind Grok** | One-off triage and judgment; queue seal candidates; report metadata-only outcomes | Dumping file bodies into chat; inventing Vault crypto; silent Automate seals |

Grok proposes and stewards. FileMind executes through CLI/RPC/txn so every act is journaled and undoable.

## Shared sensitivity labels

Use the same vocabulary as the classifier:

`secret` · `credential` · `pii` · `financial` · `health` · `other`

Grok must not invent parallel taxonomies. Corrections feed FileMind rules (`classify set --scope …`), not chat-only notes.

## Seal candidates ≠ Archive moves

On Desktop / Downloads / loose-file triage:

1. Sensitivity hit → **queue for Seal** (Assist confirmation), never `mv` into a plain Archive folder.
2. Cold, non-secret clutter → Archive / Shrink cold pack as appropriate.
3. Ambiguous → leave in place; ask Ryan or leave an Observe badge — do not guess toward Archive.

Plain Archive and Shrink packs are **not** custody. Secrets in those trees are a product failure.

## Secrets never enter chat

- Never paste private keys, seed phrases, `.env` bodies, wallet exports, or other secret material into chat, logs, or agent messages.
- Prefer `filemind vault seal <path>` / suggest plan over raw `mv`/`cp` for sensitive paths.
- After Seal, confirm **metadata only** (e.g. “sealed 3 PEMs from Desktop”).
- Sealed content: search/ask may mention that a sealed item exists — never its plaintext.

## Modes

| Mode | Grok / Vault behavior |
|---|---|
| observe | Detect + notify; no Seal, no Archive apply |
| assist | Seal / Archive / Shrink require confirmation |
| automate | Only armed rules after preview week; Grok does not bypass that gate |

## Shrink reminder

Shrink = Settings → Storage hygiene (APFS / cold packs). Protect/Vault is the headline. See `SHRINK_ROLE.md`.

## Hand-off checklist (Grok → Leader / engine)

When reporting work:

1. Paths and counts, not contents.
2. Labels applied (`credential`, etc.).
3. Whether action was Seal, Archive, Shrink, or Observe-only.
4. No Vault V0 implementation from Grok specialists unless Leader assigns cloud agent + Grok 3 review.
