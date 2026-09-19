//! Vault V0 — cryptographic sealing of private files (Protect).
//!
//! On-disk objects use **`fmseal/1`**. Content is XChaCha20-Poly1305 under a
//! random DEK; the DEK is AES-256-KW wrapped by the vault master key (MK).
//! The MK lives in the platform keystore (macOS Keychain,
//! `WhenUnlockedThisDeviceOnly`) or, in tests, a mock / `FILEMIND_VAULT_MK`.
//!
//! Phase B (not implemented): hybrid wrap can extend `wrap_blob` under
//! `wrap_id = 2` as `ClassicalWrap(DEK) || ML-KEM-768 encaps(...)` without
//! changing the content cipher or this header layout. V0 ships wrap_id = 1
//! only — no ML-KEM.

use crate::{CoreError, Result};
use aes::Aes256;
use aes_kw::Kek;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk format identifier (header + docs).
pub const FORMAT: &str = "fmseal/1";
/// Magic bytes at the start of every object.
pub const MAGIC: &[u8; 4] = b"FMS1";
pub const POLICY_VERSION: u16 = 1;
pub const AEAD_XCHACHA20_POLY1305: u16 = 1;
/// AES-256 key wrap (Phase A). Phase B will add `WRAP_HYBRID_MLKEM = 2`.
pub const WRAP_AES_KW_256: u16 = 1;
pub const ENV_MK: &str = "FILEMIND_VAULT_MK";
pub const ENV_DIR: &str = "FILEMIND_VAULT_DIR";
pub const ACCOUNT: &str = "vault-mk";
pub const SERVICE: &str = "ai.filemind";

/// Shared sensitivity labels (`secret|credential|pii|financial|health|other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Secret,
    Credential,
    Pii,
    Financial,
    Health,
    Other,
}

impl Sensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitivity::Secret => "secret",
            Sensitivity::Credential => "credential",
            Sensitivity::Pii => "pii",
            Sensitivity::Financial => "financial",
            Sensitivity::Health => "health",
            Sensitivity::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "secret" => Sensitivity::Secret,
            "credential" => Sensitivity::Credential,
            "pii" => Sensitivity::Pii,
            "financial" => Sensitivity::Financial,
            "health" => Sensitivity::Health,
            "other" => Sensitivity::Other,
            _ => return None,
        })
    }

    /// Wire value in the `fmseal/1` header.
    pub fn as_u8(self) -> u8 {
        match self {
            Sensitivity::Other => 0,
            Sensitivity::Secret => 1,
            Sensitivity::Credential => 2,
            Sensitivity::Pii => 3,
            Sensitivity::Financial => 4,
            Sensitivity::Health => 5,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Sensitivity::Secret,
            2 => Sensitivity::Credential,
            3 => Sensitivity::Pii,
            4 => Sensitivity::Financial,
            5 => Sensitivity::Health,
            _ => Sensitivity::Other,
        }
    }

    /// UX copy: "Likely credential" etc. Never include why-signals.
    pub fn label(self) -> &'static str {
        match self {
            Sensitivity::Credential => "Likely credential",
            Sensitivity::Secret => "Likely secret",
            Sensitivity::Pii => "Likely personal info",
            Sensitivity::Financial => "Likely financial",
            Sensitivity::Health => "Likely health-related",
            Sensitivity::Other => "Sensitive (review)",
        }
    }
}

/// Where the MK is fetched from. Production: Keychain. Tests: memory / env.
pub trait VaultKeystore: Send + Sync {
    fn get_or_create_mk(&self) -> Result<[u8; 32]>;
}

/// In-memory MK for unit tests (never used as the default production store).
#[derive(Clone)]
pub struct MemoryKeystore {
    mk: [u8; 32],
}

impl MemoryKeystore {
    pub fn new(mk: [u8; 32]) -> Self {
        Self { mk }
    }

    pub fn random() -> Result<Self> {
        Ok(Self {
            mk: random_bytes::<32>()?,
        })
    }
}

impl VaultKeystore for MemoryKeystore {
    fn get_or_create_mk(&self) -> Result<[u8; 32]> {
        Ok(self.mk)
    }
}

/// `FILEMIND_VAULT_MK` (64 hex chars) — test / CI override, same idea as
/// `FILEMIND_DB_KEY`. The MK is never written to the database.
pub fn mk_from_env() -> Result<Option<[u8; 32]>> {
    match std::env::var(ENV_MK) {
        Ok(v) => unhex(&v)
            .map(Some)
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("{ENV_MK} must be 64 hex characters"))),
        Err(_) => Ok(None),
    }
}

/// Hex helper shared with the file/env keystore.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).map_err(|e| CoreError::Other(anyhow::anyhow!("random: {e}")))?;
    Ok(b)
}

/// BLAKE3 of a UTF-8 path, used as AAD (not a secret).
pub fn path_fingerprint(path: &Path) -> [u8; 32] {
    *blake3::hash(path.to_string_lossy().as_bytes()).as_bytes()
}

/// Parsed `fmseal/1` object. Ciphertext stays opaque; MK/DEK are not stored here.
#[derive(Debug, Clone)]
pub struct SealHeader {
    pub policy_version: u16,
    pub sensitivity: Sensitivity,
    pub aead_id: u16,
    pub wrap_id: u16,
    pub seal_id: String,
    pub path_fingerprint: [u8; 32],
    pub plaintext_blake3: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SealObject {
    pub header: SealHeader,
    pub wrap_blob: Vec<u8>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// AAD = `seal_id || path_fingerprint || policy_version` (little-endian u16).
pub fn aad(seal_id: &str, path_fp: &[u8; 32], policy_version: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(seal_id.len() + 32 + 2);
    v.extend_from_slice(seal_id.as_bytes());
    v.extend_from_slice(path_fp);
    v.extend_from_slice(&policy_version.to_le_bytes());
    v
}

pub fn wrap_dek(mk: &[u8; 32], dek: &[u8; 32]) -> Result<Vec<u8>> {
    let kek = Kek::<Aes256>::from(*mk);
    let mut out = vec![0u8; dek.len() + 8];
    kek.wrap(dek, &mut out)
        .map_err(|e| CoreError::Other(anyhow::anyhow!("AES-KW wrap: {e:?}")))?;
    Ok(out)
}

pub fn unwrap_dek(mk: &[u8; 32], wrapped: &[u8]) -> Result<[u8; 32]> {
    let kek = Kek::<Aes256>::from(*mk);
    if wrapped.len() < 8 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "AES-KW unwrap: short blob"
        )));
    }
    let mut pt = vec![0u8; wrapped.len() - 8];
    kek.unwrap(wrapped, &mut pt)
        .map_err(|_| CoreError::Other(anyhow::anyhow!("AES-KW unwrap failed")))?;
    pt.try_into()
        .map_err(|_| CoreError::Other(anyhow::anyhow!("unwrapped DEK is not 32 bytes")))
}

fn encrypt_body(dek: &[u8; 32], nonce: &XNonce, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(dek.into());
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CoreError::Other(anyhow::anyhow!("AEAD encrypt failed")))
}

fn decrypt_body(dek: &[u8; 32], nonce: &XNonce, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(dek.into());
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CoreError::Other(anyhow::anyhow!("AEAD decrypt failed")))
}

/// Encrypt `plaintext` into a `fmseal/1` object. `seal_id` is caller-chosen.
pub fn seal_bytes(
    plaintext: &[u8],
    mk: &[u8; 32],
    seal_id: &str,
    path: &Path,
    sensitivity: Sensitivity,
) -> Result<(SealObject, [u8; 32])> {
    if seal_id.len() > 255 {
        return Err(CoreError::Other(anyhow::anyhow!("seal_id too long")));
    }
    let dek = random_bytes::<32>()?;
    let nonce_bytes = random_bytes::<24>()?;
    let nonce = XNonce::from(nonce_bytes);
    let path_fp = path_fingerprint(path);
    let aad_buf = aad(seal_id, &path_fp, POLICY_VERSION);
    let ciphertext = encrypt_body(&dek, &nonce, &aad_buf, plaintext)?;
    let wrap_blob = wrap_dek(mk, &dek)?;
    let plaintext_blake3 = *blake3::hash(plaintext).as_bytes();
    Ok((
        SealObject {
            header: SealHeader {
                policy_version: POLICY_VERSION,
                sensitivity,
                aead_id: AEAD_XCHACHA20_POLY1305,
                wrap_id: WRAP_AES_KW_256,
                seal_id: seal_id.to_string(),
                path_fingerprint: path_fp,
                plaintext_blake3,
            },
            wrap_blob,
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        },
        plaintext_blake3,
    ))
}

pub fn unseal_bytes(obj: &SealObject, mk: &[u8; 32]) -> Result<Vec<u8>> {
    if obj.header.aead_id != AEAD_XCHACHA20_POLY1305 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "unsupported aead_id {}",
            obj.header.aead_id
        )));
    }
    if obj.header.wrap_id != WRAP_AES_KW_256 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "unsupported wrap_id {} (Phase B hybrid wrap is not in V0)",
            obj.header.wrap_id
        )));
    }
    if obj.nonce.len() != 24 {
        return Err(CoreError::Other(anyhow::anyhow!("bad nonce length")));
    }
    let dek = unwrap_dek(mk, &obj.wrap_blob)?;
    let nonce = XNonce::from_slice(&obj.nonce);
    let aad_buf = aad(
        &obj.header.seal_id,
        &obj.header.path_fingerprint,
        obj.header.policy_version,
    );
    let pt = decrypt_body(&dek, nonce, &aad_buf, &obj.ciphertext)?;
    let got = blake3::hash(&pt);
    if got.as_bytes() != &obj.header.plaintext_blake3 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "plaintext BLAKE3 mismatch after decrypt"
        )));
    }
    Ok(pt)
}

/// Encode the object. Layout is length-prefixed so Phase B can grow `wrap_blob`.
pub fn encode(obj: &SealObject) -> Vec<u8> {
    let id = obj.header.seal_id.as_bytes();
    let mut out = Vec::with_capacity(
        4 + 2
            + 2
            + 1
            + 2
            + 2
            + 1
            + id.len()
            + 32
            + 2
            + obj.wrap_blob.len()
            + 1
            + obj.nonce.len()
            + 8
            + obj.ciphertext.len()
            + 32,
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u16.to_le_bytes()); // format_version
    out.extend_from_slice(&obj.header.policy_version.to_le_bytes());
    out.push(obj.header.sensitivity.as_u8());
    out.extend_from_slice(&obj.header.aead_id.to_le_bytes());
    out.extend_from_slice(&obj.header.wrap_id.to_le_bytes());
    out.push(id.len() as u8);
    out.extend_from_slice(id);
    out.extend_from_slice(&obj.header.path_fingerprint);
    out.extend_from_slice(&(obj.wrap_blob.len() as u16).to_le_bytes());
    out.extend_from_slice(&obj.wrap_blob);
    out.push(obj.nonce.len() as u8);
    out.extend_from_slice(&obj.nonce);
    out.extend_from_slice(&(obj.ciphertext.len() as u64).to_le_bytes());
    out.extend_from_slice(&obj.ciphertext);
    out.extend_from_slice(&obj.header.plaintext_blake3);
    out
}

pub fn decode(bytes: &[u8]) -> Result<SealObject> {
    let mut i = 0;
    let take = |i: &mut usize, n: usize| -> Result<&[u8]> {
        let end = i
            .checked_add(n)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("truncated fmseal")))?;
        let s = &bytes[*i..end];
        *i = end;
        Ok(s)
    };
    if take(&mut i, 4)? != MAGIC {
        return Err(CoreError::Other(anyhow::anyhow!("not an fmseal object")));
    }
    let format_version = u16::from_le_bytes(take(&mut i, 2)?.try_into().unwrap());
    if format_version != 1 {
        return Err(CoreError::Other(anyhow::anyhow!(
            "unsupported fmseal format {format_version}"
        )));
    }
    let policy_version = u16::from_le_bytes(take(&mut i, 2)?.try_into().unwrap());
    let sensitivity = Sensitivity::from_u8(take(&mut i, 1)?[0]);
    let aead_id = u16::from_le_bytes(take(&mut i, 2)?.try_into().unwrap());
    let wrap_id = u16::from_le_bytes(take(&mut i, 2)?.try_into().unwrap());
    let id_len = take(&mut i, 1)?[0] as usize;
    let seal_id = String::from_utf8(take(&mut i, id_len)?.to_vec())
        .map_err(|_| CoreError::Other(anyhow::anyhow!("seal_id is not utf-8")))?;
    let path_fingerprint: [u8; 32] = take(&mut i, 32)?.try_into().unwrap();
    let wrap_len = u16::from_le_bytes(take(&mut i, 2)?.try_into().unwrap()) as usize;
    let wrap_blob = take(&mut i, wrap_len)?.to_vec();
    let nonce_len = take(&mut i, 1)?[0] as usize;
    let nonce = take(&mut i, nonce_len)?.to_vec();
    let ct_len = u64::from_le_bytes(take(&mut i, 8)?.try_into().unwrap()) as usize;
    let ciphertext = take(&mut i, ct_len)?.to_vec();
    let plaintext_blake3: [u8; 32] = take(&mut i, 32)?.try_into().unwrap();
    if i != bytes.len() {
        return Err(CoreError::Other(anyhow::anyhow!(
            "trailing bytes on fmseal object"
        )));
    }
    Ok(SealObject {
        header: SealHeader {
            policy_version,
            sensitivity,
            aead_id,
            wrap_id,
            seal_id,
            path_fingerprint,
            plaintext_blake3,
        },
        wrap_blob,
        nonce,
        ciphertext,
    })
}

pub fn plaintext_blake3_hex(hash: &[u8; 32]) -> String {
    hex(hash)
}

pub fn parse_blake3_hex(s: &str) -> Option<[u8; 32]> {
    unhex(s)
}

/// Write a sealed object to `object_path` (parent created). Does not touch the
/// plaintext file. Caller journals, then trashs plaintext.
pub fn seal_file_to(
    src: &Path,
    object_path: &Path,
    seal_id: &str,
    sensitivity: Sensitivity,
    mk: &[u8; 32],
) -> Result<[u8; 32]> {
    let plaintext = std::fs::read(src)?;
    let (obj, hash) = seal_bytes(&plaintext, mk, seal_id, src, sensitivity)?;
    if let Some(parent) = object_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = object_path.with_extension("fmseal.part");
    std::fs::write(&part, encode(&obj))?;
    if object_path.exists() {
        let _ = std::fs::rename(&part, part.with_extension("fmseal.part.stale"));
        return Err(CoreError::DestinationExists(object_path.to_path_buf()));
    }
    std::fs::rename(&part, object_path)?;
    Ok(hash)
}

/// Sidecar next to `dest` that [`unseal_file_to_part`] writes. The transaction
/// manager then places it with [`crate::OsAdapter::rename_no_clobber`].
pub fn unseal_sidecar(dest: &Path) -> PathBuf {
    dest.with_file_name(format!(
        ".{}.unseal-part",
        dest.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ))
}

/// Decrypt `object_path` into `part` only. Does **not** occupy `dest` —
/// callers must `rename_no_clobber(part, dest)`.
pub fn unseal_file_to_part(object_path: &Path, part: &Path, mk: &[u8; 32]) -> Result<[u8; 32]> {
    let bytes = std::fs::read(object_path)?;
    let obj = decode(&bytes)?;
    let pt = unseal_bytes(&obj, mk)?;
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if part.exists() {
        let _ = std::fs::rename(part, part.with_extension("unseal-part.stale"));
    }
    std::fs::write(part, &pt)?;
    Ok(obj.header.plaintext_blake3)
}

pub fn inspect_file(object_path: &Path) -> Result<SealHeader> {
    Ok(decode(&std::fs::read(object_path)?)?.header)
}

pub fn is_vault_object_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".fmseal") || n.ends_with(".fmseal.part"))
}

/// Default object store: `~/Library/Application Support/FileMind/Vault/objects`
/// (or `%LOCALAPPDATA%\FileMind\Vault\objects`). Overridable with
/// `FILEMIND_VAULT_DIR` for tests.
pub fn default_objects_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_DIR) {
        let p = PathBuf::from(p);
        std::fs::create_dir_all(&p)?;
        return Ok(p);
    }
    let dirs = directories::ProjectDirs::from("", "", "FileMind")
        .ok_or_else(|| CoreError::Other(anyhow::anyhow!("could not resolve FileMind data dir")))?;
    let dir = dirs.data_local_dir().join("Vault").join("objects");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn new_seal_id() -> String {
    let now = chrono::Utc::now();
    let rand = random_bytes::<4>().map(u32::from_le_bytes).unwrap_or(0);
    format!("seal_{}_{:08x}", now.format("%Y%m%dT%H%M%S"), rand)
}

/// File-backed MK for Linux CI / Windows until a real store lands. 0600.
pub fn file_mk(dir: &Path) -> Result<[u8; 32]> {
    if let Some(k) = mk_from_env()? {
        return Ok(k);
    }
    let file = dir.join("vault-mk.key");
    if let Ok(s) = std::fs::read_to_string(&file) {
        return unhex(&s)
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("{} is malformed", file.display())));
    }
    let k = random_bytes::<32>()?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&file, hex(&k))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_keystore_wrap_roundtrip() {
        let ks = MemoryKeystore::random().unwrap();
        let mk = ks.get_or_create_mk().unwrap();
        let dek = random_bytes::<32>().unwrap();
        let wrapped = wrap_dek(&mk, &dek).unwrap();
        assert_ne!(wrapped, dek);
        assert_eq!(unwrap_dek(&mk, &wrapped).unwrap(), dek);
        let other = MemoryKeystore::random()
            .unwrap()
            .get_or_create_mk()
            .unwrap();
        assert!(unwrap_dek(&other, &wrapped).is_err());
    }

    #[test]
    fn fmseal_roundtrip_and_aad_bind() {
        let mk = MemoryKeystore::random()
            .unwrap()
            .get_or_create_mk()
            .unwrap();
        let path = Path::new("/Users/x/Desktop/server.pem");
        let body = b"-----BEGIN FAKE-RSA PRIVATE KEY-----\nFILEMIND_TEST_FIXTURE\n";
        let (obj, hash) =
            seal_bytes(body, &mk, "seal_test_1", path, Sensitivity::Credential).unwrap();
        assert_eq!(obj.header.wrap_id, WRAP_AES_KW_256);
        assert_eq!(hash, *blake3::hash(body).as_bytes());
        let encoded = encode(&obj);
        assert!(encoded.starts_with(MAGIC));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(unseal_bytes(&decoded, &mk).unwrap(), body);

        // tamper ciphertext
        let mut bad = decoded.clone();
        bad.ciphertext[0] ^= 1;
        assert!(unseal_bytes(&bad, &mk).is_err());

        // wrong path fingerprint (AAD)
        let mut wrong = decoded.clone();
        wrong.header.path_fingerprint[0] ^= 1;
        assert!(unseal_bytes(&wrong, &mk).is_err());
    }

    #[test]
    fn env_mk_override_shape() {
        assert!(unhex("zz").is_none());
        let k = [0xab; 32];
        assert_eq!(unhex(&hex(&k)).unwrap(), k);
    }

    #[test]
    fn unseal_writes_sidecar_not_dest() {
        let mk = MemoryKeystore::random()
            .unwrap()
            .get_or_create_mk()
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("server.pem");
        let obj = tmp.path().join("x.fmseal");
        let dest = tmp.path().join("restored.pem");
        let body = b"-----BEGIN FAKE-RSA PRIVATE KEY-----\nFILEMIND_TEST_FIXTURE\n";
        std::fs::write(&src, body).unwrap();
        seal_file_to(&src, &obj, "seal_sidecar", Sensitivity::Credential, &mk).unwrap();
        let part = unseal_sidecar(&dest);
        let hash = unseal_file_to_part(&obj, &part, &mk).unwrap();
        assert_eq!(hash, *blake3::hash(body).as_bytes());
        assert!(part.exists(), "plaintext lands on the sidecar");
        assert!(!dest.exists(), "dest is left for rename_no_clobber");
        assert_eq!(std::fs::read(&part).unwrap(), body);
    }
}
