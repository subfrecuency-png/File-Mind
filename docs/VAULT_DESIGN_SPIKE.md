# FileMind Vault — Design Spike

**Status:** V0 implemented (2026-09-19) — see `VAULT_V0_ACCEPTANCE.md`  
**Owner intent:** Deprioritize Shrink-as-headline; elevate **Protect** via cryptographically sealing private files (keys, credentials, sensitive docs), with a path to **quantum-safe** key wrap.  
**Partners:** FileMind engine (index, txn, classify) · File Mind Grok (steward / triage) · Quantum-safety work (crypto policy)

---

## 1. Problem

FileMind already **remembers** and can **organize** with undo. It does not yet **custody** secrets.

Today, PEM keys, `.env` files, wallet exports, tax PDFs, and similar can land in Desktop/Downloads and get swept into plain Archive folders. Compression / cold packs reclaim space; they do not stop a stolen laptop or sync folder from exposing plaintext.

Shrink (APFS compress, `.fmpack` cold packs) stays useful as **disk hygiene**, not as the product breakthrough.

## 2. North star

> FileMind remembers everything it’s allowed to see; seals what must not stay open; File Mind Grok keeps the messy disk honest to that policy.

**Custody states** (per file / object):

| State | Meaning |
|---|---|
| `open` | Normal indexed file; full search |
| `locked` | OS permissions / Keychain ACL hint only (soft) |
| `sealed` | Ciphertext at rest; content search off until Unseal |
| `archived` | Cold pack (Shrink tier); may also be sealed |

## 3. Goals / non-goals

### Goals (spike → MVP)
1. **Sensitivity classification** — label candidates: `secret`, `credential`, `pii`, `financial`, `health`, `other`.
2. **Seal transaction** — encrypt → trash/remove plaintext via existing txn/undo machinery → index metadata + handle only.
3. **Key custody** — content DEK wrapped; master unlock via **macOS Keychain / Secure Enclave**; DB remains SQLCipher.
4. **Unseal** — explicit, logged, gated; restores plaintext via txn (or ephemeral open).
5. **Grok contract** — steward never prints secret material; routes seal candidates to Vault queue.
6. **PQ hook** — design DEK wrap so hybrid ML-KEM can be added without format break.

### Non-goals (this spike)
- Miracle compression / Shrink marketing rewrite (separate one-pager).
- Cloud KMS, multi-user sharing, team vaults.
- Steganography, deniable encryption, full-disk FDE replacement.
- Automatic seal in `automate` on day one (preview week first).

## 4. Threat model (short)

| Adversary | Mitigate |
|---|---|
| Casual access to unlocked Mac | Seal + Keychain unlock; idle lock |
| Disk image / stolen SSD at rest | Sealed ciphertext + wrapped DEK |
| Sync/backup of Archive folder | Never store plaintext secrets in Archive trees |
| “Harvest now, decrypt later” (future CRQC) | Hybrid PQ wrap of DEK (Phase B) |
| Chat / agent exfil | Grok + CLI refuse to dump sealed plaintext into logs/chat |

Out of scope for v1: evil maid with live unlocked session and approved Unseal biometric.

## 5. Sensitivity classifier

### Signals (tier 0 — deterministic)
- Extensions / names: `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `*.p12`, `*.pfx`, `.env`, `.env.*`, `*credentials*`, `*secret*`, `wallet.dat`, `*.kdbx`
- Path hints: `~/.ssh/`, `~/Library/Keychains/` (never scan keychain DB itself — only user exports)
- Magic / header: `-----BEGIN .* PRIVATE KEY-----`, AWS AKIA patterns in small text files (cap read size)

### Signals (tier 1 — later)
- User corrections → path/ext/name rules (same loop as today’s `classify set --scope`)
- Optional LLM only on ambiguous + user-enabled AI adapter; payload stripped of key material (filename + path tokens only)

### UX
- Observe: badge “likely credential (0.91)”
- Assist: Seal / Ignore / Always ignore this path
- Grok: during Desktop/Downloads triage, **queue for Seal** instead of moving into plain Archive

## 6. Seal / Unseal as transactions

Reuse Phase 6 transaction manager.

### Seal steps (sketch)
1. `Read` + content hash (BLAKE3)
2. Generate random **DEK** (32 bytes)
3. AEAD encrypt file → write `*.fmseal` (or vault object store path)
4. Wrap DEK with vault MK (and later hybrid PQ wrap)
5. Index row: `custody=sealed`, `seal_id`, size, times, project_id, sensitivity — **no FTS body**
6. `move_to_trash` plaintext (only deletion primitive)
7. Manifest journaled; undo = Unseal path if ciphertext intact and trash put-back possible

### Unseal steps
1. Authn: LocalAuthentication / password gate
2. Unwrap DEK
3. Decrypt to original path or user-chosen path (`rename_no_clobber`)
4. Re-index as `open` (or leave sealed + ephemeral mount — v2)
5. Audit event: who/when/what (local only)

### Failure rules
- Never overwrite destination
- Never leave half-plaintext beside ciphertext without txn pause
- If trash put-back fails on undo, report remainder (same as today’s undo semantics)

## 7. Crypto design

### Phase A — classical (ship first)
- **Content:** XChaCha20-Poly1305 (or AES-256-GCM) with random nonce; AAD = `seal_id || path_fingerprint || policy_version`
- **DEK wrap:** AES-KW / HPKE-style wrap under vault **MK**
- **MK:** generated on first Vault enable; stored in Keychain (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` or stricter); optional Secure Enclave when available
- **Format version:** `fmseal/1`

### Phase B — quantum-safe wrap (quantum-safety project hook)
- Keep Phase A content cipher
- **Hybrid wrap:** `Wrap = ClassicalWrap(DEK) || ML-KEM-768 encaps(DEK or KEK)` (exact combiners per CFRG/NIST guidance — finalize in crypto review)
- On-disk: `fmseal/2` with both wraps; Unseal succeeds if either policy allows during migration
- **Rewrap job:** History entry “resealed to fmseal/2”; batch tool for vault objects

### Phase C — agility
- Algorithm registry in seal header
- Forbid decrypt-less rewrap; always verify plaintext hash on Unseal before deleting old ciphertext

### Explicit non-choices
- No password-only MK without Keychain for default path (password may *unlock* Keychain item)
- No storing MK in `filemind.db` or repo
- No logging DEK/MK/plaintext in agent.log

## 8. Object format (sketch)

```
fmseal/1
  magic: FMS1
  policy_version: u16
  sensitivity: u8
  aead_id: u16
  nonce: …
  wrap_blob: …          # Phase A; Phase B extends
  ciphertext: …
  plaintext_blake3: 32B # for undo verify
```

Store as: `~/Library/Application Support/FileMind/Vault/objects/<seal_id>.fmseal`  
Optional user-visible alias folder: `~/FileMind Archive/Vault/` containing **stubs** (bookmarks / `.fmseal-link`) — never plaintext.

## 9. Search & memory behavior

| Custody | Lexical FTS | Semantic | `ask` |
|---|---|---|---|
| open | yes | yes | yes |
| sealed | name/path/sensitivity only | embed of **metadata string only** | “you have a sealed credential from …” — no content |
| unsealed (session) | full until re-seal policy | full | full |

## 10. File Mind Grok — operating rules

1. On triage, treat sensitivity hits as **Seal candidates**, not Archive moves.
2. Never paste private key material, seed phrases, or `.env` bodies into chat.
3. Prefer `filemind vault seal <path>` / suggest plan over raw `mv`/`cp` for secrets.
4. After Seal, confirm only metadata (“sealed 3 PEMs from Desktop”).
5. Shared label vocabulary with classifier (`secret|credential|pii|financial|health`).

## 11. Mode matrix

| Mode | Vault behavior |
|---|---|
| observe | Detect + notify; no Seal |
| assist | Seal/Unseal require confirmation (CLI/GUI/Grok) |
| automate | Only armed rules after 7-day preview (e.g. “new `*.pem` in Downloads → Seal”) |

## 12. Shrink relationship

| Feature | Role |
|---|---|
| APFS compress / cold `.fmpack` | Settings → Storage; reclaim space for **non-secret** cold projects |
| Vault Seal | Headline **Protect** path for secrets |
| Sealed + archived | Allowed later: encrypt then pack, or pack-then-encrypt as one txn — decide in implementation; default **seal first** |

Update site/changelog language: Shrink = storage; Vault = trust.

## 13. Spike deliverables (engineering checklist)

- [ ] `docs/VAULT_DESIGN_SPIKE.md` (this doc)
- [x] Schema: `custody`, `seal_objects`, `vault_audit` migrations (`0011_vault`)
- [x] `vault seal|unseal|list|status` CLI
- [x] Keychain MK bootstrap + unit tests with mock keystore
- [x] Txn steps Seal/Unseal wired to undo
- [x] Classifier tier-0 rules + fixtures (`testdata/secrets/*` fake keys only)
- [x] Agent RPC: `vault.*` (MCP later)
- [ ] Crypto review note for Phase B hybrid wrap (link quantum-safety project)
- [x] Tests: Seal PEM → search metadata only → Unseal → hash matches → undo

## 14. Success criteria

1. A PEM on Desktop can be Sealed so plaintext is gone and content search returns nothing sensitive.
2. Unseal restores byte-identical content (BLAKE3 match).
3. MK never appears in DB dump or agent logs.
4. Grok Desktop cleanup demo routes a fake key to Seal, not Archive.
5. Phase B section is specific enough to implement without redesigning Phase A format.

## 15. Open questions (decide in spike week)

1. Ephemeral Unseal (RAM/temp) vs restore-to-disk default?
2. Stub files in `FileMind Archive/Vault/` for Finder visibility — yes/no?
3. iCloud/Desktop sync: exclude Vault objects path via `.filemindignore` + docs?
4. Windows v1 Keychain equivalent (DPAPI) timing — macOS first?
5. Hybrid combiner details — wait for internal crypto note vs ship A-only?

## 16. Recommended immediate next build slice

**Slice V0 (1 focused PR series):** tier-0 sensitivity + Seal/Unseal txn + Keychain MK + CLI `vault seal|unseal|list` + fake-key fixtures.  
**Slice V1:** assist UI + Grok runbook.  
**Slice V2:** hybrid ML-KEM wrap + rewrap job (quantum-safety milestone).

---

*Drafted for Ryan by File Mind Grok to capture the Protect-first synthesis. Not a commitment of algorithms until crypto review of Phase B.*
