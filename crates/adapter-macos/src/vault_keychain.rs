//! Vault MK custody.
//!
//! macOS: data-protection Keychain, account `vault-mk` (separate from
//! SQLCipher `db-key`). New items are created with
//! `SecAccessControl` + `ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly`
//! (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`) and
//! `synchronizable = false`. They never leave this device.
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

/// Lookup query: same store as create (data-protection Keychain, not iCloud).
/// Access-control attributes are set only on create — they are not search keys.
#[cfg(target_os = "macos")]
fn vault_lookup_options() -> security_framework::passwords::PasswordOptions {
    use security_framework::passwords::PasswordOptions;
    let mut options = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
    options.set_access_synchronized(Some(false));
    options.use_protected_keychain();
    options
}

#[cfg(target_os = "macos")]
fn vault_create_options() -> Result<security_framework::passwords::PasswordOptions> {
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
        0,
    )
    .map_err(|e| {
        CoreError::Other(anyhow::anyhow!(
            "Vault Keychain WhenUnlockedThisDeviceOnly: {e}"
        ))
    })?;
    let mut options = vault_lookup_options();
    options.set_access_control(access);
    options.set_label("FileMind Vault master key");
    Ok(options)
}

#[cfg(target_os = "macos")]
fn keychain_get_or_create() -> Result<[u8; 32]> {
    use security_framework::passwords::{generic_password, set_generic_password_options};
    match generic_password(vault_lookup_options()) {
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
            set_generic_password_options(
                filemind_core::vault::hex(&k).as_bytes(),
                vault_create_options()?,
            )
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
