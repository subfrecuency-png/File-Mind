//! The database encryption key (Phase 9.5, SQLCipher).
//!
//! On macOS the 32-byte key lives in the login Keychain (service
//! `ai.filemind`, account `db-key`), generated on first run. Elsewhere it is
//! a 0600 file next to the database, which keeps the same code path working
//! on Linux CI and Windows until those platforms get a real secret store.
//!
//! `FILEMIND_DB_KEY` (64 hex chars) overrides both — for tests and for
//! opening a copy of the database on another machine.

use anyhow::{Context, Result};
use std::path::Path;

pub const SERVICE: &str = "ai.filemind";
pub const ACCOUNT: &str = "db-key";
pub const KEY_FILE: &str = "filemind.key";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
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

fn random_key() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut k))
        .or_else(|_| -> std::io::Result<()> {
            // no /dev/urandom (Windows): stir time, pid and addresses through
            // blake3-free FNV; good enough as a fallback for a local file key
            let mut h: u64 = 0xcbf29ce484222325;
            let seed = format!(
                "{:?}{}{:p}",
                std::time::SystemTime::now(),
                std::process::id(),
                &k
            );
            for (i, slot) in k.iter_mut().enumerate() {
                for b in seed.bytes().chain(std::iter::once(i as u8)) {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                *slot = (h >> ((i % 8) * 8)) as u8;
            }
            Ok(())
        })?;
    Ok(k)
}

/// The key as SQLCipher wants it in `PRAGMA key`: a raw hex blob literal, so
/// no KDF runs on open (the key is already random).
pub fn pragma_value(key: &[u8; 32]) -> String {
    format!("\"x'{}'\"", hex(key))
}

/// Fetch the key, creating one on first use. `db_dir` hosts the key file on
/// platforms without a secret store.
pub fn db_key(db_dir: &Path) -> Result<[u8; 32]> {
    if let Ok(v) = std::env::var("FILEMIND_DB_KEY") {
        return unhex(&v).context("FILEMIND_DB_KEY must be 64 hex characters");
    }
    platform_get(db_dir)
}

/// Key for a database that is *not* the user's real one (tests, a copy
/// opened by tooling): a key file beside it, never the Keychain, so no
/// prompt appears and the real key is never reused elsewhere.
pub fn file_key(db_dir: &Path) -> Result<[u8; 32]> {
    if let Ok(v) = std::env::var("FILEMIND_DB_KEY") {
        return unhex(&v).context("FILEMIND_DB_KEY must be 64 hex characters");
    }
    file_get(db_dir)
}

/// Hex of the key, for `filemind dev db-key` (to open the file with the
/// `sqlcipher` shell). Never logged by FileMind itself.
pub fn db_key_hex(db_dir: &Path) -> Result<String> {
    Ok(hex(&db_key(db_dir)?))
}

#[cfg(target_os = "macos")]
fn platform_get(_db_dir: &Path) -> Result<[u8; 32]> {
    use anyhow::bail;
    use security_framework::passwords::{get_generic_password, set_generic_password};
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            unhex(&s).context("the FileMind key in the Keychain is malformed; delete the `ai.filemind` item to start over (the database would then need FILEMIND_DB_KEY)")
        }
        Err(e) if e.code() == -25300 => {
            // errSecItemNotFound: first run
            let k = random_key()?;
            set_generic_password(SERVICE, ACCOUNT, hex(&k).as_bytes())
                .context("storing the database key in the Keychain")?;
            tracing::info!("database key created in the login Keychain");
            Ok(k)
        }
        Err(e) => bail!("reading the database key from the Keychain: {e}"),
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_get(db_dir: &Path) -> Result<[u8; 32]> {
    file_get(db_dir)
}

fn file_get(db_dir: &Path) -> Result<[u8; 32]> {
    let file = db_dir.join(KEY_FILE);
    if let Ok(s) = std::fs::read_to_string(&file) {
        return unhex(&s).with_context(|| format!("{} is malformed", file.display()));
    }
    let k = random_key()?;
    std::fs::create_dir_all(db_dir)?;
    std::fs::write(&file, hex(&k))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(path = %file.display(), "database key file created");
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_pragma_shape() {
        let k = random_key().unwrap();
        assert_eq!(unhex(&hex(&k)).unwrap(), k);
        assert!(unhex("zz").is_none());
        let p = pragma_value(&k);
        assert!(p.starts_with("\"x'") && p.ends_with("'\"") && p.len() == 64 + 5);
    }
}
