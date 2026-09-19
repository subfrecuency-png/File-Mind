# Vault — UX copy guide

**Status:** Copy / product (2026-09-18)  
**Audience:** Desktop UI, CLI help, Grok chat phrasing, site  
**Related:** `VAULT_DESIGN_SPIKE.md`, `ARCHIVE_FOLDER_CONTRACT.md`, `SHRINK_ROLE.md`

## Positioning

- **Headline:** Protect — seal what must not stay open.
- **Not:** Shrink, Archive, or “encrypt your whole disk.”
- One line: *FileMind remembers what it’s allowed to see; Vault seals the rest.*

## Shared labels (user-facing)

Prefer plain words over enum dumps:

| Internal | Show as |
|---|---|
| `credential` | Likely credential |
| `secret` | Likely secret |
| `pii` | Likely personal info |
| `financial` | Likely financial |
| `health` | Likely health-related |
| `other` | Sensitive (review) |

Confidence: “Likely credential (high)” — never paste why-signals that include key material.

## Observe

- Badge on file/row: `Likely credential` (or label above).
- Toast / steward note: “Found N items that should be sealed, not archived.”
- No Seal until Assist confirmation.

## Assist — primary actions

Buttons / CLI confirm prompts:

- **Seal** — encrypt and remove plaintext (undoable).
- **Ignore once**
- **Always ignore this path**

Confirm Seal (short):

> Seal this file? Plaintext moves to Trash; only ciphertext stays. You can Unseal later.

After Seal (metadata only):

> Sealed 3 credentials from Desktop.

Never show key bodies, `.env` lines, or seed phrases in UI, CLI stdout, or chat.

## Unseal

- Gate copy: “Unlock Vault to restore this file” (LocalAuthentication / Keychain — don’t invent a password field unless product adds one).
- Success: “Restored [filename] — bit-identical check OK.”
- Failure: “Couldn’t Unseal — Vault unlock canceled” / “ciphertext missing” — no crypto dumps.

## Search & Ask while sealed

- Result pill: `Sealed`
- Snippet: name / path / label only — e.g. “Sealed credential · Desktop · yesterday”
- Ask: “You have a sealed credential from Desktop” — **never** content.

## Archive / Shrink cross-links

- If user tries Archive on a sensitive hit: “This looks sensitive — Seal instead of Archive.”
- Shrink Settings blurb stays storage-only; no “protect your files with Shrink.”

## Modes (one sentence each)

| Mode | Copy |
|---|---|
| observe | FileMind flags seal candidates; it won’t seal yet. |
| assist | Seal and Unseal need your OK each time. |
| automate | Auto-seal only after a preview week and Arm — not in V0. |

## CLI tone (`filemind vault …`)

- Help: `seal` / `unseal` / `list` — “Seal sensitive files (Protect). Not Archive.”
- `list`: path, label, sealed time — no bodies.
- Errors: human first (“Vault master key unavailable”), then code if useful.

## Out of scope for V0 copy

ML-KEM / “quantum-safe” marketing, team vaults, cloud KMS, Shrink expansion. Phase B can add one Settings footnote later — not V0 chrome.
