# Vault V0 — PR acceptance checklist

**Status:** Gate for cloud PR review (2026-09-18)  
**Source:** File Mind Grok 3 review bar + `VAULT_DESIGN_SPIKE.md` §16  
**Related:** `VAULT_UX_COPY.md`, `GROK_FILEMIND_OPS.md`, `ARCHIVE_FOLDER_CONTRACT.md`, `SHRINK_ROLE.md`

Fail the PR if any unchecked item is missed or waved without Ryan + Leader sign-off.

## Scope lock

- [ ] CLI-first: `filemind vault seal | unseal | list` (+ `status` only if cheap)
- [ ] Crates touched as needed: `filemind-core`, `filemind-storage`, `filemind-adapter-macos`, `filemind-cli` (agent `vault.*` thin/later OK)
- [ ] **Out:** Shrink expansion, ML-KEM impl, adapter-win DPAPI, automate-mode auto-seal, second full impl from Grok specialists

## Review gates

### 1. Tier-0 sensitivity (deterministic, no LLM)

- [ ] Positives: `*.pem`, `.env` / `.env.*`, `id_rsa*` / `id_ed25519*` **private** key names, path under `~/.ssh/` (private material), body `BEGIN … PRIVATE KEY`
- [ ] Near-misses do **not** seal: ordinary docs, public `*.crt` / `*.pub`, `BEGIN CERTIFICATE` only, prose mentioning “private key”
- [ ] **Known gap — must fix in V0:** today’s `sensitive_by_name` treats `id_ed25519.pub` as a hit via `starts_with("id_ed25519.")`. Seal candidates must **exclude** `*.pub` (and CERTIFICATE-only bodies) even when the basename shares an `id_rsa` / `id_ed25519` prefix.
- [ ] No cloud/LLM path required for V0 classify

### 2. Seal / Unseal are real transactions

- [ ] Journaled via existing Manifest / Journal
- [ ] Plaintext leaves folder only via `move_to_trash`
- [ ] **Unseal restore** uses `rename_no_clobber` (must not overwrite an existing path)
- [ ] Undo verifies BLAKE3 (bit-identical)

### 3. Key custody

- [ ] Vault MK in Keychain (`WhenUnlockedThisDeviceOnly` or stricter); separate account from DB key (e.g. `vault-mk`)
- [ ] Content: DEK + AEAD; on-disk `fmseal/1`
- [ ] MK never in SQLite, agent logs, chat, or CI artifacts

### 4. Search while sealed

- [ ] Metadata only: name / path / sensitivity
- [ ] No FTS body / content snippets for sealed objects

### 5. Fixtures & hygiene

- [ ] Fake keys only under `testdata/secrets/` (or `testdata/`)
- [ ] Every fixture marked **FAKE** in header
- [ ] No real user secrets committed
- [ ] CI refuses PEM-like material outside testdata

### 6. Modes

- [ ] observe: detect / notify only
- [ ] assist: Seal / Unseal need confirmation
- [ ] No automate seal on day one

### 7. Phase B hooks only

- [ ] Format / comments allow future hybrid wrap
- [ ] **No** ML-KEM implementation in V0

### 8. No Shrink scope creep

- [ ] Diff does not expand APFS / `.fmpack` / Shrink marketing surface
- [ ] Secrets never routed to Archive or Shrink first (see Archive contract)

## Tests (land with or immediately after PR)

- [ ] Unit: tier-0 classifier matrix (positives + near-miss negatives)
- [ ] Unit: mock keystore MK bootstrap / unwrap (env override pattern akin to `FILEMIND_DB_KEY`)
- [ ] Integration: Seal fake PEM → trash plaintext → list sealed → search metadata-only → Unseal → BLAKE3 match → undo
- [ ] Chaos: mid-seal failure leaves no orphan plaintext beside ciphertext without paused txn
- [ ] Fixture hygiene: refuse real-looking PEM outside testdata

## Manual smoke (Ryan / Leader)

- [ ] Seal a **FAKE** PEM on Desktop → plaintext gone → search shows sealed stub only
- [ ] Unseal → hash matches → undo works
- [ ] Grok/Assist copy does not print secret bodies (see `VAULT_UX_COPY.md`)

## Sign-off

| Role | Name | OK |
|---|---|---|
| Review bar | File Mind Grok 3 | [ ] |
| Docs / copy | File Mind Grok 2 | [ ] |
| Launch / merge | File Mind Grok Leader | [ ] |
| Product | Ryan | [ ] |
