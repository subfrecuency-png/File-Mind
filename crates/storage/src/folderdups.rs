//! Whole-folder duplicates (Phase 9.6): `creditos-v1` is a byte-for-byte copy
//! of `creditos`. Reported as one suggestion per group so the copy can be
//! trashed as a single step instead of thousands of per-file ones.
//!
//! A folder's digest is a Merkle hash: sorted child names with the child
//! file's content hash or the subfolder's digest. Folders with any unhashed
//! file are skipped (their digest would be a guess), and only the top-most
//! matching pair is reported — the subfolders match trivially.

use crate::Db;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderDup {
    pub keeper: PathBuf,
    pub copies: Vec<PathBuf>,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Default)]
struct Node {
    /// name → blake3 hex of the file, or None when not hashed yet
    files: BTreeMap<String, (Option<String>, u64)>,
    dirs: BTreeMap<String, PathBuf>,
    newest: i64,
}

struct Digest {
    hex: String,
    files: u64,
    bytes: u64,
    complete: bool,
    newest: i64,
}

fn digest(dir: &Path, tree: &HashMap<PathBuf, Node>, out: &mut HashMap<PathBuf, Digest>) -> Digest {
    let node = match tree.get(dir) {
        Some(n) => n,
        None => {
            return Digest {
                hex: String::new(),
                files: 0,
                bytes: 0,
                complete: true,
                newest: 0,
            }
        }
    };
    let mut h = blake3::Hasher::new();
    let mut files = 0;
    let mut bytes = 0;
    let mut complete = true;
    let mut newest = node.newest;
    for (name, (hash, size)) in &node.files {
        h.update(b"f");
        h.update(name.as_bytes());
        h.update(b"\0");
        match hash {
            Some(x) => {
                h.update(x.as_bytes());
            }
            None => complete = false,
        }
        h.update(b"\n");
        files += 1;
        bytes += size;
    }
    for (name, path) in &node.dirs {
        let d = digest(path, tree, out);
        h.update(b"d");
        h.update(name.as_bytes());
        h.update(b"\0");
        h.update(d.hex.as_bytes());
        h.update(b"\n");
        files += d.files;
        bytes += d.bytes;
        complete &= d.complete;
        newest = newest.max(d.newest);
    }
    let d = Digest {
        hex: h.finalize().to_hex().to_string(),
        files,
        bytes,
        complete,
        newest,
    };
    out.insert(
        dir.to_path_buf(),
        Digest {
            hex: d.hex.clone(),
            files,
            bytes,
            complete,
            newest,
        },
    );
    d
}

impl Db {
    /// Groups of identical folders with at least `min_files` files, largest
    /// first. Nothing is written; the result feeds `refresh_suggestions`.
    pub fn folder_duplicates(&self, min_files: u64) -> Result<Vec<FolderDup>> {
        let roots: Vec<PathBuf> = self.list_roots()?.into_iter().map(|r| r.path).collect();
        let mut st = self.conn.prepare(
            "SELECT f.path, f.size, f.mtime, b.blake3 FROM files f LEFT JOIN blobs b ON b.blob_id = f.blob_id
             WHERE f.kind = 'file' AND f.status = 'present'",
        )?;
        let mut tree: HashMap<PathBuf, Node> = HashMap::new();
        let rows = st.query_map([], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (path, size, mtime, hash) = row?;
            if filemind_core::scanner::in_noise_dir(&path) {
                continue;
            }
            let Some(parent) = path.parent() else {
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let n = tree.entry(parent.to_path_buf()).or_default();
            n.files.insert(name, (hash, size));
            n.newest = n.newest.max(mtime);
            // register the directory chain up to its root
            let mut child = parent.to_path_buf();
            while let Some(p) = child.parent().map(Path::to_path_buf) {
                let stop = roots.contains(&child);
                let cname = child
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let pn = tree.entry(p.clone()).or_default();
                if pn.dirs.insert(cname, child.clone()).is_some() || stop {
                    break;
                }
                child = p;
            }
        }
        let mut digests: HashMap<PathBuf, Digest> = HashMap::new();
        for r in &roots {
            digest(r, &tree, &mut digests);
        }
        // group complete, big-enough folders by digest
        let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for (dir, d) in &digests {
            if !d.complete || d.files < min_files || roots.contains(dir) {
                continue;
            }
            groups.entry(d.hex.clone()).or_default().push(dir.clone());
        }
        let mut out: Vec<FolderDup> = Vec::new();
        let mut taken: Vec<PathBuf> = Vec::new();
        let mut candidates: Vec<(String, Vec<PathBuf>)> =
            groups.into_iter().filter(|(_, v)| v.len() >= 2).collect();
        // biggest, then shallowest, folders first so nested matches are
        // shadowed by their parents (a copied tree ties with its own subtrees)
        candidates.sort_by_key(|(hex, dirs)| {
            (
                std::cmp::Reverse(
                    digests
                        .values()
                        .find(|d| d.hex == *hex)
                        .map(|d| d.bytes)
                        .unwrap_or(0),
                ),
                dirs.iter()
                    .map(|d| d.components().count())
                    .min()
                    .unwrap_or(0),
            )
        });
        for (hex, mut dirs) in candidates {
            if dirs.iter().any(|d| taken.iter().any(|t| d.starts_with(t))) {
                continue;
            }
            // keeper: unmarked name first, then most recently touched, then shortest path
            dirs.sort_by_key(|d| {
                let name = d
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let (_, marked) = filemind_core::versions::normalize_stem(&name);
                let newest = digests.get(d).map(|x| x.newest).unwrap_or(0);
                (marked as u8, std::cmp::Reverse(newest), d.as_os_str().len())
            });
            let keeper = dirs.remove(0);
            let d = digests.values().find(|d| d.hex == hex).unwrap();
            for x in dirs.iter().chain(std::iter::once(&keeper)) {
                taken.push(x.clone());
            }
            out.push(FolderDup {
                keeper,
                copies: dirs,
                files: d.files,
                bytes: d.bytes,
            });
        }
        out.sort_by_key(|g| std::cmp::Reverse(g.bytes * g.copies.len() as u64));
        Ok(out)
    }
}
