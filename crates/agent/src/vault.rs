//! Vault V0: Seal / Unseal as journaled transactions.
//!
//! Observe: detect only. Assist / Automate: confirmation required. V0 never
//! auto-seals.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use filemind_core::classify::{self, vault_tier0_file};
use filemind_core::scanner::hash_file;
use filemind_core::txn::{self, CrashPoint, Initiator, Manifest, Step};
use filemind_core::vault::{self, Sensitivity, FORMAT};
use filemind_core::{Mode, OsAdapter, RiskTier};
use filemind_storage::vault::SealRow;
use filemind_storage::Db;
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct Sealed {
    pub txn_id: String,
    pub seal_id: String,
    pub path: PathBuf,
    pub sensitivity: String,
    pub plaintext_blake3: String,
    pub done: usize,
    pub failed: usize,
    pub state: String,
}

#[derive(Debug, serde::Serialize)]
pub struct Unsealed {
    pub txn_id: String,
    pub seal_id: String,
    pub dest: PathBuf,
    pub plaintext_blake3: String,
    pub done: usize,
    pub failed: usize,
    pub state: String,
}

fn current_mode(db: &Db) -> Result<Mode> {
    Ok(serde_json::from_value(
        db.get_setting("mode")?
            .unwrap_or(serde_json::json!("observe")),
    )
    .unwrap_or_default())
}

fn confirm_or_bail(mode: Mode, approved: bool) -> Result<()> {
    match mode {
        Mode::Observe => bail!(
            "mode is observe: FileMind flags seal candidates; it won't seal yet. `filemind mode assist` to allow approved actions."
        ),
        Mode::Assist | Mode::Automate => {
            if approved {
                Ok(())
            } else {
                bail!("Seal and Unseal need your OK each time.")
            }
        }
    }
}

fn roots_plus_vault(adapter: &dyn OsAdapter, db: &Db) -> Result<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = db.list_roots()?.into_iter().map(|r| r.path).collect();
    if let Ok(v) = adapter.vault_objects_dir() {
        roots.push(v);
        if let Some(p) = roots.last().and_then(|p| p.parent()).map(Path::to_path_buf) {
            roots.push(p);
        }
    }
    Ok(roots)
}

/// Plan + execute Seal. Plaintext leaves its folder only via `move_to_trash`.
pub fn seal(
    adapter: &dyn OsAdapter,
    db: &Db,
    path: &Path,
    approved: bool,
    crash: CrashPoint,
) -> Result<Sealed> {
    crate::actions::recover_all(adapter, db)?;
    let mode = current_mode(db)?;
    confirm_or_bail(mode, approved)?;

    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !path.is_file() {
        bail!("{} is not a file", path.display());
    }
    let sensitivity = vault_tier0_file(&path).unwrap_or(Sensitivity::Other);
    let hash = hash_file(&path)?;
    let size = std::fs::metadata(&path)?.len();
    let seal_id = vault::new_seal_id();
    let object_path = adapter
        .vault_objects_dir()?
        .join(format!("{seal_id}.fmseal"));

    let mut m = Manifest::new(
        mode,
        Initiator::User,
        RiskTier::Tier2,
        format!("Seal {} ({})", path.display(), sensitivity.as_str()),
    );
    m.steps.push(Step::Seal {
        path: path.clone(),
        seal_id: seal_id.clone(),
        object_path: object_path.clone(),
        sensitivity: sensitivity.as_str().to_string(),
        hash_before: Some(hash.clone()),
        trashed_to: None,
    });

    let mut allowed = roots_plus_vault(adapter, db)?;
    if let Some(p) = path.parent() {
        allowed.push(p.to_path_buf());
    }
    let problems = txn::validate(adapter, &allowed, &mut m)?;
    if !problems.is_empty() {
        bail!("refusing to seal:\n  {}", problems.join("\n  "));
    }

    let rep = txn::execute(adapter, db, &mut m, crash)?;
    let (m2, state, states) = db.load_txn(&m.txn_id)?.context("transaction vanished")?;
    db.note_txn_effects(&m2, &states)?;

    if rep.failed == 0 && object_path.exists() {
        let file_id = db.file_id_of_path(&path).ok().flatten();
        let row = SealRow {
            seal_id: seal_id.clone(),
            file_id,
            original_path: path.clone(),
            original_name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            object_path: object_path.clone(),
            sensitivity: sensitivity.as_str().to_string(),
            plaintext_blake3: hash.clone(),
            size,
            sealed_ts: Utc::now().timestamp(),
            format: FORMAT.to_string(),
            txn_id: Some(m.txn_id.clone()),
            unsealed_ts: None,
        };
        db.mark_sealed(&path, &seal_id, sensitivity.as_str(), &row)?;
    }

    Ok(Sealed {
        txn_id: m.txn_id,
        seal_id,
        path,
        sensitivity: sensitivity.as_str().to_string(),
        plaintext_blake3: hash,
        done: rep.done,
        failed: rep.failed,
        state: state.as_str().to_string(),
    })
}

pub fn unseal(
    adapter: &dyn OsAdapter,
    db: &Db,
    target: &str,
    dest: Option<&Path>,
    approved: bool,
    crash: CrashPoint,
) -> Result<Unsealed> {
    crate::actions::recover_all(adapter, db)?;
    let mode = current_mode(db)?;
    confirm_or_bail(mode, approved)?;

    let row = resolve_seal(db, target)?;
    if row.unsealed_ts.is_some() {
        bail!("that file is not sealed");
    }
    if !row.object_path.exists() {
        bail!("ciphertext missing");
    }
    let dest = dest
        .map(PathBuf::from)
        .unwrap_or_else(|| row.original_path.clone());
    if dest.exists() {
        bail!("destination {} already exists", dest.display());
    }

    let mut m = Manifest::new(
        mode,
        Initiator::User,
        RiskTier::Tier2,
        format!("Unseal {} → {}", row.seal_id, dest.display()),
    );
    m.steps.push(Step::Unseal {
        seal_id: row.seal_id.clone(),
        object_path: row.object_path.clone(),
        dest: dest.clone(),
        hash_before: Some(row.plaintext_blake3.clone()),
        object_trashed_to: None,
    });

    let mut allowed = roots_plus_vault(adapter, db)?;
    if let Some(p) = dest.parent() {
        allowed.push(p.to_path_buf());
    }
    let problems = txn::validate(adapter, &allowed, &mut m)?;
    if !problems.is_empty() {
        bail!("refusing to unseal:\n  {}", problems.join("\n  "));
    }

    let rep = txn::execute(adapter, db, &mut m, crash)?;
    let (m2, state, states) = db.load_txn(&m.txn_id)?.context("transaction vanished")?;
    db.note_txn_effects(&m2, &states)?;

    if rep.failed == 0 {
        db.mark_unsealed_file(&dest, &row.seal_id)?;
        let now = hash_file(&dest)?;
        if now != row.plaintext_blake3 {
            bail!("Restored file failed the bit-identical check");
        }
    }

    Ok(Unsealed {
        txn_id: m.txn_id,
        seal_id: row.seal_id,
        dest,
        plaintext_blake3: row.plaintext_blake3,
        done: rep.done,
        failed: rep.failed,
        state: state.as_str().to_string(),
    })
}

fn resolve_seal(db: &Db, target: &str) -> Result<SealRow> {
    if let Some(row) = db.seal_object(target)? {
        return Ok(row);
    }
    let p = PathBuf::from(target);
    let p = p.canonicalize().unwrap_or(p);
    db.seal_object_by_path(&p)?
        .with_context(|| format!("no sealed object for {target}"))
}

pub fn list(db: &Db) -> Result<Vec<SealRow>> {
    db.vault_audit("list", None, None, "list");
    db.list_sealed()
}

#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub mode: Mode,
    pub sealed: i64,
    pub candidates: i64,
    pub mk: &'static str,
    pub objects: PathBuf,
}

pub fn status(adapter: &dyn OsAdapter, db: &Db) -> Result<Status> {
    let (sealed, candidates) = db.vault_counts()?;
    db.vault_audit("status", None, None, "status");
    let mk = if filemind_core::vault::mk_from_env()?.is_some() {
        "env"
    } else if cfg!(target_os = "macos") {
        "keychain"
    } else {
        "file"
    };
    Ok(Status {
        mode: current_mode(db)?,
        sealed,
        candidates,
        mk,
        objects: adapter.vault_objects_dir()?,
    })
}

/// Peek a path: is it a seal candidate? Never prints bodies.
pub fn probe(path: &Path) -> Option<Sensitivity> {
    classify::vault_tier0_file(path)
}
