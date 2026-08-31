//! Cold-project archives: pack a whole project tree into one `.fmpack`
//! under the FileMind Archive folder, verify every member decodes back to
//! its original bytes, and only then let a normal journaled transaction
//! trash the original tree. Restore reverses it, member by member or whole.
//!
//! Building an archive touches nothing of the user's — it only writes
//! FileMind's own pack file — so it happens before the transaction, and a
//! failure anywhere leaves the original tree exactly as it was.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use filemind_core::shrink::archive as fmt;
use filemind_storage::archive::{ArchiveRow, ChunkLoc, MemberRow};
use filemind_storage::Db;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Largest member the builder will hold in memory.
pub const MAX_MEMBER_BYTES: u64 = 1 << 30;
/// Files up to this size feed the dictionary trainer.
const DICT_SAMPLE_MAX: u64 = 256 * 1024;

pub fn archive_root(home: &Path) -> PathBuf {
    home.join("FileMind Archive")
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .context("no home directory")
}

/// Dictionary category by extension — coarse on purpose; a dictionary only
/// has to be trained on *similar* data to help.
fn category(ext: &str) -> &'static str {
    match ext {
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "java" | "go"
        | "rb" | "php" | "swift" | "kt" | "cs" | "sh" | "zsh" | "lua" | "pl" | "scala" | "css"
        | "scss" | "vue" | "svelte" | "sql" => "code",
        "json" | "yaml" | "yml" | "toml" | "xml" | "plist" | "csv" | "tsv" | "ini" | "cfg"
        | "conf" | "lock" => "data",
        "md" | "txt" | "rst" | "org" | "log" | "html" | "htm" | "tex" | "srt" | "vtt" => "text",
        _ => "other",
    }
}

/// One entry of the tree walk, before anything is read.
struct WalkEntry {
    abs: PathBuf,
    rel: String,
    kind: &'static str, // file | dir | symlink
    size: u64,
    mtime: i64,
}

fn walk(folder: &Path) -> Result<Vec<WalkEntry>> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(folder)
        .follow_links(false)
        .min_depth(1)
    {
        let e = e?;
        let ft = e.file_type();
        let rel = e
            .path()
            .strip_prefix(folder)
            .unwrap_or(e.path())
            .to_string_lossy()
            .to_string();
        let md = e.path().symlink_metadata()?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if ft.is_dir() {
            out.push(WalkEntry {
                abs: e.path().to_path_buf(),
                rel,
                kind: "dir",
                size: 0,
                mtime,
            });
        } else if ft.is_symlink() {
            out.push(WalkEntry {
                abs: e.path().to_path_buf(),
                rel,
                kind: "symlink",
                size: 0,
                mtime,
            });
        } else if ft.is_file() {
            out.push(WalkEntry {
                abs: e.path().to_path_buf(),
                rel,
                kind: "file",
                size: md.len(),
                mtime,
            });
        } else {
            bail!(
                "{} is neither a file, folder nor symlink — not archiving this project",
                e.path().display()
            );
        }
    }
    Ok(out)
}

/// Why a project cannot be archived right now, before anything runs.
/// Empty means safe.
pub fn preflight(db: &Db, folder: &Path) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    if !folder.is_dir() {
        problems.push(format!("{} is not a folder", folder.display()));
        return Ok(problems);
    }
    let entries = match walk(folder) {
        Ok(e) => e,
        Err(e) => {
            problems.push(e.to_string());
            return Ok(problems);
        }
    };
    for e in &entries {
        if e.kind == "file" && e.size > MAX_MEMBER_BYTES {
            problems.push(format!(
                "{} is larger than 1 GiB — too large to archive in memory",
                e.abs.display()
            ));
        }
    }
    // the index knows which files are sensitive; those never leave the tree
    let n: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM files WHERE sensitive = 1 AND status = 'present'
         AND (path = ?1 OR path LIKE ?2)",
        rusqlite::params![
            folder.to_string_lossy(),
            format!("{}/%", folder.to_string_lossy())
        ],
        |r| r.get(0),
    )?;
    if n > 0 {
        problems.push(format!(
            "{} contains {n} file(s) marked sensitive — not archiving this project",
            folder.display()
        ));
    }
    Ok(problems)
}

#[derive(Debug, serde::Serialize)]
pub struct Built {
    pub archive_id: String,
    pub pack_path: PathBuf,
    pub members: usize,
    pub bytes_raw: u64,
    pub bytes_stored: u64,
}

/// Pack `folder` into a new archive. Reads every file, dedups chunks
/// against every existing archive, writes the pack, records everything in
/// the database, and verifies each member decodes to its original hash
/// before reporting success. The original tree is not touched.
pub fn build(
    db: &Db,
    folder: &Path,
    name: &str,
    project_id: Option<i64>,
    progress: impl Fn(u64, u64),
) -> Result<Built> {
    let problems = preflight(db, folder)?;
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    let entries = walk(folder)?;
    let archive_id = db.archive_new_id();
    let dir = archive_root(&home_dir()?).join("Projects");
    std::fs::create_dir_all(&dir)?;
    let pack_path = dir.join(fmt::pack_name(name, &archive_id));
    let part = pack_path.with_extension("fmpack.part");

    // 1. train dictionaries on this project's own small files, per category
    let mut samples: HashMap<&'static str, (Vec<Vec<u8>>, usize)> = HashMap::new();
    for e in entries.iter().filter(|e| e.kind == "file") {
        if e.size == 0 || e.size > DICT_SAMPLE_MAX {
            continue;
        }
        let cat = category(
            &e.abs
                .extension()
                .map(|x| x.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        );
        let (bufs, bytes) = samples.entry(cat).or_default();
        if *bytes < fmt::DICT_SAMPLE_BYTES {
            if let Ok(data) = std::fs::read(&e.abs) {
                *bytes += data.len();
                bufs.push(data);
            }
        }
    }
    let mut dicts: HashMap<&'static str, (String, Vec<u8>)> = HashMap::new();
    for (cat, (bufs, _)) in &samples {
        if let Some(d) = fmt::train_dict(bufs) {
            dicts.insert(cat, (format!("{archive_id}:{cat}"), d));
        }
    }
    drop(samples);

    // 2. chunk, dedup, compress, append to the pack
    let mut pack = File::create(&part)?;
    pack.write_all(fmt::PACK_MAGIC)?;
    let mut off = fmt::PACK_MAGIC.len() as u64;
    // chunks new in this pack (hash → loc) plus refs into older packs
    let mut new_chunks: HashMap<String, ChunkLoc> = HashMap::new();
    let mut members: Vec<(MemberRow, Vec<String>)> = Vec::new();
    let mut bytes_raw = 0u64;
    let total = entries.iter().filter(|e| e.kind == "file").count() as u64;
    let mut done = 0u64;
    for e in &entries {
        match e.kind {
            "dir" => members.push((
                MemberRow {
                    rel: e.rel.clone(),
                    kind: "dir".into(),
                    size: 0,
                    mtime: e.mtime,
                    hash: String::new(),
                },
                Vec::new(),
            )),
            "symlink" => {
                let target = std::fs::read_link(&e.abs)?;
                members.push((
                    MemberRow {
                        rel: e.rel.clone(),
                        kind: "symlink".into(),
                        size: 0,
                        mtime: e.mtime,
                        hash: target.to_string_lossy().to_string(),
                    },
                    Vec::new(),
                ));
            }
            _ => {
                let data = std::fs::read(&e.abs)?;
                if std::fs::symlink_metadata(&e.abs)?.len() != data.len() as u64 {
                    bail!("{} changed while being archived", e.abs.display());
                }
                bytes_raw += data.len() as u64;
                let hash = blake3::hash(&data).to_hex().to_string();
                let cat = category(
                    &e.abs
                        .extension()
                        .map(|x| x.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                );
                let dict = dicts.get(cat);
                let mut refs = Vec::new();
                for (co, cl) in fmt::chunk_spans(&data) {
                    let chunk = &data[co..co + cl];
                    let ch = blake3::hash(chunk).to_hex().to_string();
                    if !new_chunks.contains_key(&ch) && db.archive_chunk(&ch)?.is_none() {
                        let comp = fmt::compress(chunk, dict.map(|(_, d)| d.as_slice()))?;
                        pack.write_all(&comp)?;
                        new_chunks.insert(
                            ch.clone(),
                            ChunkLoc {
                                archive_id: archive_id.clone(),
                                off,
                                clen: comp.len(),
                                ulen: cl,
                                dict_id: dict.map(|(id, _)| id.clone()).unwrap_or_default(),
                            },
                        );
                        off += comp.len() as u64;
                    }
                    refs.push(ch);
                }
                members.push((
                    MemberRow {
                        rel: e.rel.clone(),
                        kind: "file".into(),
                        size: data.len() as u64,
                        mtime: e.mtime,
                        hash,
                    },
                    refs,
                ));
                done += 1;
                progress(done, total);
            }
        }
    }
    pack.sync_all()?;
    drop(pack);
    std::fs::rename(&part, &pack_path)?;
    let bytes_stored = off - fmt::PACK_MAGIC.len() as u64;

    // 3. record everything in one database transaction
    let tx = db.conn.unchecked_transaction()?;
    db.archive_insert(&ArchiveRow {
        archive_id: archive_id.clone(),
        project_id,
        name: name.to_string(),
        folder: folder.to_string_lossy().to_string(),
        pack_path: pack_path.to_string_lossy().to_string(),
        created_ts: Utc::now().timestamp(),
        bytes_raw,
        bytes_stored,
        members: members.len(),
        state: "building".into(),
        txn_id: None,
    })?;
    for (cat, (id, d)) in &dicts {
        db.archive_add_dict(id, &archive_id, cat, d)?;
    }
    for (h, loc) in &new_chunks {
        db.archive_add_chunk(h, loc)?;
    }
    for (m, refs) in &members {
        db.archive_add_member(&archive_id, m)?;
        for (i, h) in refs.iter().enumerate() {
            db.archive_add_member_chunk(&archive_id, &m.rel, i, h)?;
        }
    }
    tx.commit()?;

    // 4. read every member back out of the pack(s) and compare hashes;
    // only a fully verified archive is allowed to replace the original
    if let Err(e) = verify(db, &archive_id) {
        // our own pack file; the original tree is untouched
        let _ = std::fs::remove_file(&pack_path); // filemind:own-file
        bail!("archive verification failed, original left untouched: {e}");
    }
    db.archive_set_state(&archive_id, "ready")?;
    Ok(Built {
        archive_id,
        pack_path,
        members: members.len(),
        bytes_raw,
        bytes_stored,
    })
}

/// Open pack files as needed, cached per archive id.
struct Packs<'a> {
    db: &'a Db,
    open: HashMap<String, File>,
}

impl<'a> Packs<'a> {
    fn new(db: &'a Db) -> Self {
        Self {
            db,
            open: HashMap::new(),
        }
    }
    fn chunk(&mut self, hash: &str, dicts: &mut HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
        let loc = self
            .db
            .archive_chunk(hash)?
            .with_context(|| format!("chunk {hash} is not in any archive"))?;
        if !self.open.contains_key(&loc.archive_id) {
            let a = self
                .db
                .archive(&loc.archive_id)?
                .with_context(|| format!("archive {} vanished from the index", loc.archive_id))?;
            let mut f = File::open(&a.pack_path)
                .with_context(|| format!("cannot open pack {}", a.pack_path))?;
            fmt::check_magic(&mut f)?;
            self.open.insert(loc.archive_id.clone(), f);
        }
        let f = self.open.get_mut(&loc.archive_id).unwrap();
        let raw = fmt::read_chunk(f, loc.off, loc.clen)?;
        let dict = if loc.dict_id.is_empty() {
            None
        } else {
            if !dicts.contains_key(&loc.dict_id) {
                let d = self
                    .db
                    .archive_dict(&loc.dict_id)?
                    .with_context(|| format!("dictionary {} vanished", loc.dict_id))?;
                dicts.insert(loc.dict_id.clone(), d);
            }
            dicts.get(&loc.dict_id).map(|d| d.as_slice())
        };
        Ok(fmt::decompress(&raw, loc.ulen, dict)?)
    }
}

/// Decode every member and compare against the recorded hashes.
pub fn verify(db: &Db, archive_id: &str) -> Result<()> {
    let mut packs = Packs::new(db);
    let mut dicts = HashMap::new();
    for m in db.archive_members(archive_id)? {
        if m.kind != "file" {
            continue;
        }
        let mut h = blake3::Hasher::new();
        let mut n = 0u64;
        for ch in db.archive_member_chunks(archive_id, &m.rel)? {
            let data = packs.chunk(&ch, &mut dicts)?;
            n += data.len() as u64;
            h.update(&data);
        }
        if n != m.size || h.finalize().to_hex().to_string() != m.hash {
            bail!("{}: decoded bytes do not match the original", m.rel);
        }
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct Restored {
    pub files: usize,
    pub bytes: u64,
    pub to: PathBuf,
}

/// Extract one member (or the whole archive) back to disk, hash-verified,
/// never clobbering anything that exists.
pub fn restore(
    db: &Db,
    archive_id: &str,
    member: Option<&str>,
    to: Option<&Path>,
) -> Result<Restored> {
    let a = db
        .archive(archive_id)?
        .with_context(|| format!("no archive {archive_id}"))?;
    let base = to
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&a.folder));
    let members = match member {
        Some(rel) => vec![db
            .archive_member(archive_id, rel)?
            .with_context(|| format!("no member {rel} in {archive_id}"))?],
        None => {
            if base.exists() {
                bail!(
                    "{} already exists — restore to a different folder with --to, or move it aside",
                    base.display()
                );
            }
            db.archive_members(archive_id)?
        }
    };
    let mut packs = Packs::new(db);
    let mut dicts = HashMap::new();
    let mut files = 0usize;
    let mut bytes = 0u64;
    for m in &members {
        let dest = base.join(&m.rel);
        match m.kind.as_str() {
            "dir" => std::fs::create_dir_all(&dest)?,
            "symlink" => {
                if dest.exists() || dest.symlink_metadata().is_ok() {
                    bail!("{} already exists — not overwriting", dest.display());
                }
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&m.hash, &dest)?;
                #[cfg(not(unix))]
                bail!("symlink restore is not supported on this platform");
            }
            _ => {
                if dest.exists() {
                    bail!("{} already exists — not overwriting", dest.display());
                }
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let part = dest.with_extension(format!(
                    "{}.fmrestore",
                    dest.extension()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_default()
                ));
                let mut f = File::create(&part)?;
                let mut h = blake3::Hasher::new();
                for ch in db.archive_member_chunks(archive_id, &m.rel)? {
                    let data = packs.chunk(&ch, &mut dicts)?;
                    h.update(&data);
                    f.write_all(&data)?;
                }
                f.sync_all()?;
                drop(f);
                if h.finalize().to_hex().to_string() != m.hash {
                    let _ = std::fs::remove_file(&part); // filemind:own-file
                    bail!("{}: restored bytes do not match the archive record", m.rel);
                }
                std::fs::rename(&part, &dest)?;
                files += 1;
                bytes += m.size;
            }
        }
    }
    // the inventory: restored members are plain present files again
    match member {
        Some(rel) => {
            db.conn.execute(
                "UPDATE files SET status = 'present', location = NULL WHERE location = ?1",
                [fmt::location(archive_id, rel)],
            )?;
        }
        None => {
            db.mark_unarchived(archive_id)?;
        }
    }
    Ok(Restored {
        files,
        bytes,
        to: base,
    })
}
