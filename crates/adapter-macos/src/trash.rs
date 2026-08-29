//! Trash on Unix. macOS: `~/.Trash` (Finder shows it; "Put Back" is not
//! wired, FileMind's own undo restores from the recorded path). Linux:
//! freedesktop `~/.local/share/Trash/{files,info}`.
//!
//! Only `rename` is used — same-volume, atomic, and never overwrites. A
//! cross-volume move would require copy + delete, which FileMind refuses.

use chrono::Utc;
use filemind_core::adapter::TrashReceipt;
use filemind_core::{CoreError, Result};
use std::path::{Path, PathBuf};

fn trash_dir(home: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    if cfg!(target_os = "macos") {
        let t = home.join(".Trash");
        std::fs::create_dir_all(&t)?;
        Ok((t, None))
    } else {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("Trash");
        let files = base.join("files");
        let info = base.join("info");
        std::fs::create_dir_all(&files)?;
        std::fs::create_dir_all(&info)?;
        Ok((files, Some(info)))
    }
}

/// `name`, `name 2`, `name 3` … first one that does not exist in `dir`.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2.. {
        let c = dir.join(format!("{stem} {n}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    unreachable!()
}

/// The path `move_to_trash` would use right now.
pub fn trash_target(home: &Path, path: &Path) -> Result<PathBuf> {
    let (files_dir, _) = trash_dir(home)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no file name: {}", path.display())))?;
    Ok(free_name(&files_dir, &name))
}

pub fn move_to_trash(home: &Path, path: &Path) -> Result<TrashReceipt> {
    let dest = trash_target(home, path)?;
    move_to_trash_at(home, path, &dest)
}

pub fn move_to_trash_at(home: &Path, path: &Path, dest: &Path) -> Result<TrashReceipt> {
    let md = std::fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() {
        return Err(CoreError::Other(anyhow::anyhow!(
            "refusing to trash a link: {}",
            path.display()
        )));
    }
    let (_, info_dir) = trash_dir(home)?;
    if dest.exists() {
        return Err(CoreError::DestinationExists(dest.to_path_buf()));
    }
    match std::fs::rename(path, dest) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV: different volume. Copy+delete is a delete in disguise; refuse.
            return Err(CoreError::Other(anyhow::anyhow!(
                "{} is on a different volume than the Trash; FileMind will not copy-and-delete",
                path.display()
            )));
        }
        Err(e) => return Err(e.into()),
    }
    if let Some(info) = info_dir {
        let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            path.display(),
            stamp
        );
        let info_file = info.join(format!(
            "{}.trashinfo",
            dest.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
        let _ = std::fs::write(info_file, body);
    }
    Ok(TrashReceipt {
        original: path.to_path_buf(),
        trashed_to: Some(dest.to_path_buf()),
        at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trashes_without_clobbering() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, b"one").unwrap();
        let r1 = move_to_trash(&home, &a).unwrap();
        assert!(!a.exists());
        let t1 = r1.trashed_to.unwrap();
        assert!(t1.exists());

        std::fs::write(&a, b"two").unwrap();
        let r2 = move_to_trash(&home, &a).unwrap();
        let t2 = r2.trashed_to.unwrap();
        assert_ne!(t1, t2, "second trash of the same name must not overwrite");
        assert_eq!(std::fs::read(&t1).unwrap(), b"one");
        assert_eq!(std::fs::read(&t2).unwrap(), b"two");
    }
}
