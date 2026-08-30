//! APFS transparent compression as FileMind's tier-1 rewrite.
//!
//! `compress` turns a plain file into a `decmpfs` file in place: same path,
//! same inode, same bytes for every reader, fewer blocks on disk. The
//! sequence matters and was checked on a real APFS volume:
//!
//! 1. write `com.apple.ResourceFork` (type 4) and `com.apple.decmpfs`;
//! 2. truncate the data fork to zero;
//! 3. set `UF_COMPRESSED`.
//!
//! Setting the flag *before* truncating makes the kernel decompress on the
//! truncate and the data is gone — never do that. Between 2 and 3 the file
//! is `HalfDone` (empty data fork, payload in the xattrs, no flag); the
//! kernel does not touch such a file and `compress` finishes it.
//!
//! `restore` writes one byte of the file's own contents back at offset 0:
//! any write to a compressed file makes the kernel materialise the plain
//! data fork, drop the flag and both xattrs — in place, same inode.
//!
//! On anything but macOS every operation reports `Unsupported`; the format
//! code in `decmpfs` is portable and tested everywhere.

pub mod decmpfs;

use filemind_core::{CoreError, Result, RewriteReceipt, RewriteState};
use std::path::Path;

/// Files above this are not rewritten in memory (the payload is built whole).
pub const MAX_BYTES: u64 = 1 << 30;
pub const METHOD: &str = "apfs";
/// APFS allocation block; on-disk savings only come in whole blocks.
pub const BLOCK: u64 = 4096;

fn err(msg: impl Into<String>) -> CoreError {
    CoreError::Other(anyhow::anyhow!("{}", msg.into()))
}

/// On-disk footprint: allocated blocks, not the logical length.
pub fn on_disk_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::symlink_metadata(path)?;
    Ok(md.blocks() * 512)
}

#[cfg(target_os = "macos")]
mod sys {
    use super::*;
    use std::ffi::CString;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        // libproc, part of libSystem — no extra link flags
        fn proc_listpidspath(
            type_: u32,
            typeinfo: u32,
            path: *const libc::c_char,
            pathflags: u32,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    const PROC_ALL_PIDS: u32 = 1;
    const XATTR_FLAGS: libc::c_int = libc::XATTR_NOFOLLOW | libc::XATTR_SHOWCOMPRESSION;

    fn cpath(p: &Path) -> Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| err("path contains NUL"))
    }

    fn os_err(what: &str, p: &Path) -> CoreError {
        let e = std::io::Error::last_os_error();
        CoreError::Other(anyhow::anyhow!("{what} {}: {e}", p.display()))
    }

    fn flags(path: &Path) -> Result<(u32, u64)> {
        let md = std::fs::symlink_metadata(path)?;
        if !md.file_type().is_file() {
            return Err(err(format!("{} is not a regular file", path.display())));
        }
        // st_flags is not exposed by std; read it with lstat
        let c = cpath(path)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::lstat(c.as_ptr(), &mut st) } != 0 {
            return Err(os_err("lstat", path));
        }
        Ok((st.st_flags, md.len()))
    }

    pub(super) fn has_xattr(path: &Path, name: &str) -> Result<bool> {
        let c = cpath(path)?;
        let n = CString::new(name).map_err(|_| err("bad xattr name"))?;
        let r = unsafe {
            libc::getxattr(
                c.as_ptr(),
                n.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                XATTR_FLAGS,
            )
        };
        if r >= 0 {
            return Ok(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOATTR) => Ok(false),
            _ => Err(os_err("getxattr", path)),
        }
    }

    fn set_xattr(path: &Path, name: &str, value: &[u8]) -> Result<()> {
        let c = cpath(path)?;
        let n = CString::new(name).map_err(|_| err("bad xattr name"))?;
        let r = unsafe {
            libc::setxattr(
                c.as_ptr(),
                n.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
                XATTR_FLAGS,
            )
        };
        if r != 0 {
            return Err(os_err(&format!("setxattr {name}"), path));
        }
        Ok(())
    }

    fn remove_xattr(path: &Path, name: &str) {
        if let (Ok(c), Ok(n)) = (cpath(path), CString::new(name)) {
            unsafe { libc::removexattr(c.as_ptr(), n.as_ptr(), XATTR_FLAGS) };
        }
    }

    fn chflags(path: &Path, f: u32) -> Result<()> {
        let c = cpath(path)?;
        if unsafe { libc::chflags(c.as_ptr(), f) } != 0 {
            return Err(os_err("chflags", path));
        }
        Ok(())
    }

    /// Number of processes holding `path` open (ourselves excluded — we open
    /// nothing before asking).
    fn open_by_others(path: &Path) -> Result<usize> {
        let c = cpath(path)?;
        let mut buf = vec![0i32; 4096];
        let n = unsafe {
            proc_listpidspath(
                PROC_ALL_PIDS,
                0,
                c.as_ptr(),
                0,
                buf.as_mut_ptr() as *mut libc::c_void,
                (buf.len() * 4) as libc::c_int,
            )
        };
        if n < 0 {
            return Err(os_err("proc_listpidspath", path));
        }
        let me = std::process::id() as i32;
        Ok(buf[..(n as usize / 4)]
            .iter()
            .filter(|p| **p != me && **p != 0)
            .count())
    }

    pub fn state(path: &Path) -> Result<RewriteState> {
        let (f, len) = flags(path)?;
        if f & libc::UF_COMPRESSED != 0 {
            return Ok(RewriteState::Rewritten);
        }
        if len == 0 && has_xattr(path, decmpfs::XATTR_NAME)? {
            return Ok(RewriteState::HalfDone);
        }
        Ok(RewriteState::Original)
    }

    pub fn compress(path: &Path) -> Result<RewriteReceipt> {
        let (f, len) = flags(path)?;
        let before = on_disk_bytes(path)?;
        if f & libc::UF_COMPRESSED != 0 {
            return Err(err(format!("{} is already compressed", path.display())));
        }
        if f & (libc::UF_IMMUTABLE | libc::SF_IMMUTABLE | libc::UF_APPEND) != 0 {
            return Err(err(format!("{} is locked", path.display())));
        }
        if len == 0 && has_xattr(path, decmpfs::XATTR_NAME)? {
            // HalfDone: the payload is there, only the flag is missing
            chflags(path, f | libc::UF_COMPRESSED)?;
            return Ok(RewriteReceipt {
                on_disk_before: before,
                on_disk_after: on_disk_bytes(path)?,
            });
        }
        if len > MAX_BYTES {
            return Err(err(format!(
                "{} is too large to rewrite in memory",
                path.display()
            )));
        }
        if has_xattr(path, decmpfs::XATTR_NAME)? || has_xattr(path, decmpfs::RSRC_NAME)? {
            return Err(err(format!(
                "{} already has a resource fork or compression attribute",
                path.display()
            )));
        }
        let open = open_by_others(path)?;
        if open > 0 {
            return Err(err(format!(
                "{} is open by {open} other process{}",
                path.display(),
                if open == 1 { "" } else { "es" }
            )));
        }
        let data = std::fs::read(path)?;
        if data.len() as u64 != len {
            return Err(err(format!("{} changed while reading", path.display())));
        }
        let enc = decmpfs::encode(&data);
        // never trust the encoder blindly
        let back = decmpfs::decode(&enc.decmpfs, enc.rsrc.as_deref())?;
        if back != data {
            return Err(err(format!("{}: payload does not decode", path.display())));
        }
        drop(back);
        // the payload lands in whole allocation blocks too, so compare block
        // counts and insist on at least one block saved
        let after_blocks = (enc.len() as u64).div_ceil(BLOCK) * BLOCK;
        if after_blocks + BLOCK > before {
            // not worth it: leave the file exactly as it is
            return Ok(RewriteReceipt {
                on_disk_before: before,
                on_disk_after: before,
            });
        }
        write_payload(path, &enc)?;
        chflags(path, f | libc::UF_COMPRESSED)?;
        // the kernel must now serve the original bytes
        let (f2, len2) = flags(path)?;
        if f2 & libc::UF_COMPRESSED == 0 || len2 != len {
            return Err(err(format!(
                "{}: compression flag did not take (flags {f2:#x}, len {len2})",
                path.display()
            )));
        }
        Ok(RewriteReceipt {
            on_disk_before: before,
            on_disk_after: on_disk_bytes(path)?,
        })
    }

    /// Steps 1 and 2 of activation: xattrs in, data fork emptied, flag not
    /// yet set. A crash after this leaves the `HalfDone` shape that
    /// `compress` and `restore` both know how to finish.
    fn write_payload(path: &Path, enc: &decmpfs::Encoded) -> Result<()> {
        if let Some(r) = &enc.rsrc {
            set_xattr(path, decmpfs::RSRC_NAME, r)?;
        }
        if let Err(e) = set_xattr(path, decmpfs::XATTR_NAME, &enc.decmpfs) {
            remove_xattr(path, decmpfs::RSRC_NAME);
            return Err(e);
        }
        // the point of no return: from here the payload is authoritative
        let fd = std::fs::OpenOptions::new().write(true).open(path)?;
        if let Err(e) = fd.set_len(0) {
            remove_xattr(path, decmpfs::XATTR_NAME);
            remove_xattr(path, decmpfs::RSRC_NAME);
            return Err(e.into());
        }
        fd.sync_all()?;
        Ok(())
    }

    /// Leave `path` in the crash-between-steps shape (payload written, flag
    /// not set) so the tests can exercise recovery without faking it with
    /// `chflags(0)`, which the kernel treats differently from a file that
    /// was never activated.
    #[cfg(test)]
    pub fn leave_half_done(path: &Path) -> Result<()> {
        let data = std::fs::read(path)?;
        write_payload(path, &decmpfs::encode(&data))
    }

    pub fn restore(path: &Path) -> Result<()> {
        let (mut f, mut len) = flags(path)?;
        if f & libc::UF_COMPRESSED == 0 {
            if len == 0 && has_xattr(path, decmpfs::XATTR_NAME)? {
                // HalfDone: activate first so the kernel can materialise it,
                // then look again — the size is only known once it is live
                chflags(path, f | libc::UF_COMPRESSED)?;
                (f, len) = flags(path)?;
                if f & libc::UF_COMPRESSED == 0 {
                    return Err(err(format!(
                        "{}: could not activate the half-written payload",
                        path.display()
                    )));
                }
            } else {
                return Ok(()); // nothing of ours on it
            }
        }
        if len == 0 {
            // an empty compressed file: just drop the flag and payload
            chflags(path, f & !libc::UF_COMPRESSED)?;
            remove_xattr(path, decmpfs::XATTR_NAME);
            remove_xattr(path, decmpfs::RSRC_NAME);
            return Ok(());
        }
        let mut first = [0u8; 1];
        {
            let mut r = std::fs::File::open(path)?;
            r.seek(SeekFrom::Start(0))?;
            r.read_exact(&mut first)?;
        }
        // writing the file's own first byte back makes the kernel decompress
        // the whole file in place and clear the flag
        let w = std::fs::OpenOptions::new().write(true).open(path)?;
        std::os::unix::fs::FileExt::write_at(&w, &first, 0)?;
        w.sync_all()?;
        drop(w);
        let (f2, len2) = flags(path)?;
        if f2 & libc::UF_COMPRESSED != 0 || len2 != len {
            return Err(err(format!(
                "{}: still compressed after restore (flags {f2:#x}, len {len2})",
                path.display()
            )));
        }
        Ok(())
    }
}

/// What the disk says about `path`.
pub fn state(path: &Path) -> Result<RewriteState> {
    #[cfg(target_os = "macos")]
    {
        sys::state(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Ok(RewriteState::Unsupported)
    }
}

/// Compress `path` in place. Idempotent on a `HalfDone` file. A file that
/// would not get smaller is left untouched and reported with equal sizes.
pub fn compress(path: &Path) -> Result<RewriteReceipt> {
    #[cfg(target_os = "macos")]
    {
        sys::compress(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(err(format!(
            "APFS transparent compression is macOS-only ({})",
            path.display()
        )))
    }
}

/// Put the plain representation back, in place.
pub fn restore(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        sys::restore(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(err(format!(
            "APFS transparent compression is macOS-only ({})",
            path.display()
        )))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn blake(p: &Path) -> String {
        filemind_core::scanner::hash_file(p).unwrap()
    }

    #[test]
    fn compress_verify_restore_same_inode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.log");
        let data = b"2026-08-30T10:00:00Z INFO scheduler tick ok\n".repeat(30_000);
        std::fs::write(&p, &data).unwrap();
        let ino = std::fs::metadata(&p).unwrap().ino();
        let h = blake(&p);
        assert_eq!(state(&p).unwrap(), RewriteState::Original);

        let r = compress(&p).unwrap();
        assert!(r.on_disk_after < r.on_disk_before / 10, "{r:?}");
        assert_eq!(state(&p).unwrap(), RewriteState::Rewritten);
        assert_eq!(std::fs::read(&p).unwrap(), data, "bit-identical on read");
        assert_eq!(blake(&p), h);
        assert_eq!(std::fs::metadata(&p).unwrap().ino(), ino);
        assert!(compress(&p).is_err(), "never twice");

        restore(&p).unwrap();
        assert_eq!(state(&p).unwrap(), RewriteState::Original);
        assert_eq!(std::fs::read(&p).unwrap(), data);
        assert_eq!(std::fs::metadata(&p).unwrap().ino(), ino);
        assert!(on_disk_bytes(&p).unwrap() >= data.len() as u64);
        restore(&p).unwrap(); // no-op on a plain file
    }

    #[test]
    fn half_done_is_finished_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        let data = b"# notes\n\nsome markdown that repeats\n".repeat(5_000);
        std::fs::write(&p, &data).unwrap();
        let ino = std::fs::metadata(&p).unwrap().ino();
        // a crash between "payload written" and "flag set": empty data
        // fork, payload in the xattrs, no flag
        sys::leave_half_done(&p).unwrap();
        assert_eq!(state(&p).unwrap(), RewriteState::HalfDone);
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            0,
            "the kernel shows nothing"
        );
        // finishing forwards
        let r = compress(&p).unwrap();
        assert_eq!(state(&p).unwrap(), RewriteState::Rewritten);
        assert_eq!(std::fs::read(&p).unwrap(), data);
        assert!(r.on_disk_after <= r.on_disk_before);
        assert_eq!(std::fs::metadata(&p).unwrap().ino(), ino);
        restore(&p).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), data);

        // and rolling back from the same shape
        let q = dir.path().join("notes2.md");
        std::fs::write(&q, &data).unwrap();
        sys::leave_half_done(&q).unwrap();
        assert_eq!(state(&q).unwrap(), RewriteState::HalfDone);
        restore(&q).unwrap();
        assert_eq!(state(&q).unwrap(), RewriteState::Original);
        assert_eq!(std::fs::read(&q).unwrap(), data);
        assert!(!sys::has_xattr(&q, decmpfs::XATTR_NAME).unwrap());
    }

    #[test]
    fn incompressible_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("noise.bin");
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let data: Vec<u8> = (0..300_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        std::fs::write(&p, &data).unwrap();
        let r = compress(&p).unwrap();
        assert_eq!(r.on_disk_after, r.on_disk_before);
        assert_eq!(state(&p).unwrap(), RewriteState::Original);
        assert_eq!(std::fs::read(&p).unwrap(), data);
    }

    #[test]
    fn refuses_links_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"x".repeat(10_000)).unwrap();
        let l = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&p, &l).unwrap();
        assert!(compress(&l).is_err());
        assert!(compress(dir.path()).is_err());
        assert!(state(&l).is_err());
    }
}
