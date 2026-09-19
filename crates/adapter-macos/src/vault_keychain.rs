//! Vault MK custody.
//!
//! macOS: login Keychain, account `vault-mk` (separate from SQLCipher `db-key`).
//! Items are created with `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` when
//! the Security framework accepts the attribute; otherwise they still live in
//! the Keychain (never SQLite / logs).
//! Other Unix: 0600 file under the FileMind data dir (CI / Linux).
//! `FILEMIND_VAULT_MK` overrides both — tests only. Never log the key.

#[cfg(target_os = "macos")]
use filemind_core::CoreError;
use filemind_core::Result;
use std::path::Path;

#[cfg(target_os = "macos")]
const SERVICE: &str = filemind_core::vault::SERVICE;
#[cfg(target_os = "macos")]
const ACCOUNT: &str = filemind_core::vault::ACCOUNT;

pub fn vault_master_key(data_dir: &Path) -> Result<[u8; 32]> {
    if let Some(k) = filemind_core::vault::mk_from_env()? {
        return Ok(k);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = data_dir;
        keychain_get_or_create()
    }
    #[cfg(not(target_os = "macos"))]
    {
        filemind_core::vault::file_mk(data_dir)
    }
}

#[cfg(target_os = "macos")]
fn keychain_get_or_create() -> Result<[u8; 32]> {
    use security_framework::passwords::{get_generic_password, set_generic_password};
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            filemind_core::vault::unhex(&s).ok_or_else(|| {
                CoreError::Other(anyhow::anyhow!(
                    "Vault master key in the Keychain is malformed"
                ))
            })
        }
        Err(e) if e.code() == -25300 => {
            let k = filemind_core::vault::random_bytes::<32>()?;
            set_generic_password(SERVICE, ACCOUNT, filemind_core::vault::hex(&k).as_bytes())
                .map_err(|e| {
                    CoreError::Other(anyhow::anyhow!("storing the vault MK in the Keychain: {e}"))
                })?;
            Ok(k)
        }
        Err(e) => Err(CoreError::Other(anyhow::anyhow!(
            "Vault master key unavailable ({e})"
        ))),
    }
}
