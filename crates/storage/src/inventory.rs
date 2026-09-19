//! Inventory persistence: roots, files, file_events and the FTS mirror.
//!
//! Identity is the platform `FileId` (device + index), so a rename or move
//! updates the existing row and records a `renamed`/`moved` event instead of
//! creating a new file.

use crate::Db;
use anyhow::{Context, Result};
use chrono::Utc;
use filemind_core::adapter::Entry;
use filemind_core::model::{EntryKind, FileId, FileStatus};
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Root {
    pub root_id: i64,
    pub path: PathBuf,
    pub mode: Option<String>,
    pub last_scan: Option<i64>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UpsertStats {
    pub inserted: u64,
    pub updated: u64,
    pub renamed: u64,
    pub moved: u64,
    pub unchanged: u64,
    pub marked_missing: u64,
}

fn key(id: FileId) -> String {
    format!("{}:{}", id.device, id.index)
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
}

fn kind_str(k: EntryKind) -> &'static str {
    match k {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Link => "link",
        EntryKind::Other => "other",
    }
}

fn status_str(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Present => "present",
        FileStatus::Moved => "moved",
        FileStatus::Trashed => "trashed",
        FileStatus::Archived => "archived",
        FileStatus::Sealed => "sealed",
        FileStatus::Missing => "missing",
    }
}

/// Lower-cased path components joined by spaces, for the FTS `path_tokens` column.
pub fn path_tokens(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|s| !s.is_empty() && *s != "/")
        .map(|s| s.to_lowercase().replace(['_', '-', '.'], " "))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Db {
    // ----- roots --------------------------------------------------------

    pub fn add_root(&self, path: &Path) -> Result<Root> {
        let now = Utc::now().timestamp();
        let p = path.to_string_lossy();
        self.conn.execute(
            "INSERT OR IGNORE INTO roots(path, added_ts) VALUES (?1, ?2)",
            params![p, now],
        )?;
        self.root_by_path(path)?
            .context("root not found after insert")
    }

    pub fn root_by_path(&self, path: &Path) -> Result<Option<Root>> {
        let p = path.to_string_lossy();
        Ok(self
            .conn
            .query_row(
                "SELECT root_id, path, mode, last_scan FROM roots WHERE path = ?1",
                [p],
                |r| {
                    Ok(Root {
                        root_id: r.get(0)?,
                        path: PathBuf::from(r.get::<_, String>(1)?),
                        mode: r.get(2)?,
                        last_scan: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_roots(&self) -> Result<Vec<Root>> {
        let mut st = self
            .conn
            .prepare("SELECT root_id, path, mode, last_scan FROM roots ORDER BY root_id")?;
        let rows = st.query_map([], |r| {
            Ok(Root {
                root_id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                mode: r.get(2)?,
                last_scan: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_root(&self, path: &Path) -> Result<bool> {
        // Only forgets the index; never touches the file system.
        let Some(root) = self.root_by_path(path)? else {
            return Ok(false);
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM files_fts WHERE rowid IN (SELECT rowid FROM files WHERE root_id = ?1)",
            [root.root_id],
        )?;
        tx.execute("DELETE FROM files WHERE root_id = ?1", [root.root_id])?;
        tx.execute("DELETE FROM roots WHERE root_id = ?1", [root.root_id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Start a new scan generation for a root; returns the new sequence number.
    pub fn begin_scan(&self, root_id: i64) -> Result<i64> {
        self.conn.execute(
            "UPDATE roots SET scan_seq = scan_seq + 1 WHERE root_id = ?1",
            [root_id],
        )?;
        Ok(self.conn.query_row(
            "SELECT scan_seq FROM roots WHERE root_id = ?1",
            [root_id],
            |r| r.get(0),
        )?)
    }

    /// The registered root that contains `path` (longest prefix wins).
    pub fn root_for_path(&self, path: &Path) -> Result<Option<Root>> {
        let mut best: Option<Root> = None;
        for r in self.list_roots()? {
            if path.starts_with(&r.path)
                && best
                    .as_ref()
                    .is_none_or(|b| r.path.components().count() > b.path.components().count())
            {
                best = Some(r);
            }
        }
        Ok(best)
    }

    /// Current scan generation of a root (0 if never scanned).
    pub fn current_seq(&self, root_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT scan_seq FROM roots WHERE root_id = ?1",
            [root_id],
            |r| r.get(0),
        )?)
    }

    /// Mark `path` and everything beneath it missing (watcher delete/rename-away).
    pub fn mark_missing_path(&self, path: &Path, ts: i64, source: &str) -> Result<u64> {
        let p = path.to_string_lossy().to_string();
        let prefix = format!(
            "{}{}",
            p.trim_end_matches(std::path::MAIN_SEPARATOR),
            std::path::MAIN_SEPARATOR
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
             SELECT file_id, ?1, 'deleted', path, NULL, ?4 FROM files
             WHERE status = 'present' AND (path = ?2 OR path LIKE ?3 ESCAPE '\\')",
            params![ts, p, like_escape(&prefix) + "%", source],
        )?;
        let n = tx.execute(
            "UPDATE files SET status = 'missing' WHERE status = 'present' AND (path = ?1 OR path LIKE ?2 ESCAPE '\\')",
            params![p, like_escape(&prefix) + "%"],
        )?;
        tx.commit()?;
        Ok(n as u64)
    }

    // ----- files --------------------------------------------------------

    /// Upsert a batch of scanned entries for `root_id` inside one SQLite transaction.
    pub fn upsert_entries(
        &self,
        root_id: i64,
        seq: i64,
        entries: &[Entry],
        scan_ts: i64,
    ) -> Result<UpsertStats> {
        self.upsert_entries_from(root_id, seq, entries, scan_ts, "scan")
    }

    /// Like [`Db::upsert_entries`] but tagging events with `source` (`scan` or `watcher`).
    pub fn upsert_entries_from(
        &self,
        root_id: i64,
        seq: i64,
        entries: &[Entry],
        scan_ts: i64,
        source: &str,
    ) -> Result<UpsertStats> {
        let mut stats = UpsertStats::default();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut find = tx.prepare_cached(
                "SELECT rowid, path, name, size, mtime, status FROM files WHERE file_id = ?1",
            )?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO files(file_id, root_id, path, name, ext, size, mtime, ctime, birthtime,
                                   kind, is_link, status, first_seen, last_seen, last_scan_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'present', ?12, ?12, ?13)",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE files SET path = ?2, name = ?3, ext = ?4,
                                  rewrite = CASE WHEN size = ?5 AND mtime = ?6 THEN rewrite ELSE NULL END,
                                  size = ?5, mtime = ?6, ctime = ?7,
                                  status = 'present', last_seen = ?8, root_id = ?9, last_scan_seq = ?10
                 WHERE file_id = ?1",
            )?;
            let mut touch = tx.prepare_cached("UPDATE files SET last_seen = ?2, status = 'present', last_scan_seq = ?3 WHERE file_id = ?1")?;
            let mut event = tx.prepare_cached(
                "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut fts_del = tx.prepare_cached("DELETE FROM files_fts WHERE rowid = ?1")?;
            // A path can hold only one present identity: when a file is
            // overwritten by rename, the displaced identity is gone.
            let mut displace_ev = tx.prepare_cached(
                "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
                 SELECT file_id, ?2, 'deleted', path, NULL, ?4 FROM files
                 WHERE path = ?1 AND file_id <> ?3 AND status = 'present'",
            )?;
            let mut displace = tx.prepare_cached(
                "UPDATE files SET status = 'missing' WHERE path = ?1 AND file_id <> ?2 AND status = 'present'",
            )?;
            let mut fts_ins = tx.prepare_cached(
                "INSERT INTO files_fts(rowid, name, path_tokens, extracted_text) VALUES (?1, ?2, ?3, '')",
            )?;

            for e in entries {
                let id = key(e.file_id);
                let path = e.path.to_string_lossy().to_string();
                let name = e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ext = e
                    .path
                    .extension()
                    .map(|x| x.to_string_lossy().to_lowercase());
                let mtime = e.mtime.timestamp();
                let ctime = e.ctime.map(|t| t.timestamp());
                let birth = e.birthtime.map(|t| t.timestamp());

                let mut lookup = |id: &str| {
                    find.query_row([id], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, String>(5)?,
                        ))
                    })
                    .optional()
                };
                let mut id = id;
                let mut existing = lookup(&id)?;
                // Hard link: the identity is already recorded at another path
                // that still exists on disk. Give this path its own identity.
                if let Some((_, ref old_path, ..)) = existing {
                    if *old_path != path && Path::new(old_path).symlink_metadata().is_ok() {
                        id = format!("{id}@{:08x}", fnv(&path));
                        existing = lookup(&id)?;
                    }
                }

                match existing {
                    None => {
                        insert.execute(params![
                            id,
                            root_id,
                            path,
                            name,
                            ext,
                            e.size as i64,
                            mtime,
                            ctime,
                            birth,
                            kind_str(e.kind),
                            matches!(e.kind, EntryKind::Link) as i64,
                            scan_ts,
                            seq
                        ])?;
                        let rowid = tx.last_insert_rowid();
                        fts_ins.execute(params![rowid, name, path_tokens(&e.path)])?;
                        displace_ev.execute(params![path, scan_ts, id, source])?;
                        displace.execute(params![path, id])?;
                        event.execute(params![
                            id,
                            scan_ts,
                            "created",
                            Option::<String>::None,
                            path,
                            source
                        ])?;
                        stats.inserted += 1;
                    }
                    Some((rowid, old_path, old_name, old_size, old_mtime, old_status)) => {
                        let path_changed = old_path != path;
                        let content_changed = old_size != e.size as i64 || old_mtime != mtime;
                        let was_missing = old_status == "missing";
                        if !path_changed && !content_changed && !was_missing {
                            touch.execute(params![id, scan_ts, seq])?;
                            stats.unchanged += 1;
                            continue;
                        }
                        update.execute(params![
                            id,
                            path,
                            name,
                            ext,
                            e.size as i64,
                            mtime,
                            ctime,
                            scan_ts,
                            root_id,
                            seq
                        ])?;
                        if path_changed {
                            displace_ev.execute(params![path, scan_ts, id, source])?;
                            displace.execute(params![path, id])?;
                            let kind = if old_name != name
                                && Path::new(&old_path).parent() == e.path.parent()
                            {
                                stats.renamed += 1;
                                "renamed"
                            } else {
                                stats.moved += 1;
                                "moved"
                            };
                            event.execute(params![id, scan_ts, kind, old_path, path, source])?;
                            fts_del.execute([rowid])?;
                            fts_ins.execute(params![rowid, name, path_tokens(&e.path)])?;
                        } else if was_missing {
                            event.execute(params![
                                id,
                                scan_ts,
                                "restored",
                                Option::<String>::None,
                                path,
                                source
                            ])?;
                            stats.updated += 1;
                        } else {
                            event.execute(params![
                                id,
                                scan_ts,
                                "modified",
                                Option::<String>::None,
                                path,
                                source
                            ])?;
                            stats.updated += 1;
                        }
                    }
                }
            }
        }
        tx.commit()?;
        Ok(stats)
    }

    /// After a full scan generation `seq` of `root_id`, anything not seen in it is gone.
    pub fn mark_missing(&self, root_id: i64, seq: i64, scan_ts: i64) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO file_events(file_id, ts, type, from_path, to_path, source)
             SELECT file_id, ?3, 'deleted', path, NULL, 'scan' FROM files
             WHERE root_id = ?1 AND last_scan_seq < ?2 AND status = 'present'",
            params![root_id, seq, scan_ts],
        )?;
        let n = tx.execute(
            "UPDATE files SET status = 'missing' WHERE root_id = ?1 AND last_scan_seq < ?2 AND status = 'present'",
            params![root_id, seq],
        )?;
        tx.execute(
            "UPDATE roots SET last_scan = ?2 WHERE root_id = ?1",
            params![root_id, scan_ts],
        )?;
        tx.commit()?;
        Ok(n as u64)
    }

    /// Files (kind = file, present) that have no content hash yet, largest last.
    pub fn files_without_hash(&self, limit: usize) -> Result<Vec<(String, PathBuf, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT file_id, path, size FROM files
             WHERE kind = 'file' AND status = 'present' AND blob_id IS NULL
             ORDER BY size ASC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                PathBuf::from(r.get::<_, String>(1)?),
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Record a content hash for a file, creating the blob row if new.
    pub fn set_file_hash(&self, file_id: &str, blake3: &str, size: u64) -> Result<i64> {
        self.set_file_hashes(&[(file_id.to_string(), blake3.to_string(), size)])?;
        Ok(self.conn.query_row(
            "SELECT blob_id FROM blobs WHERE blake3 = ?1",
            [blake3],
            |r| r.get(0),
        )?)
    }

    /// Record many `(file_id, blake3, size)` results in one transaction.
    pub fn set_file_hashes(&self, results: &[(String, String, u64)]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut ins =
                tx.prepare_cached("INSERT OR IGNORE INTO blobs(blake3, size) VALUES (?1, ?2)")?;
            let mut upd = tx.prepare_cached(
                "UPDATE files SET blob_id = (SELECT blob_id FROM blobs WHERE blake3 = ?2) WHERE file_id = ?1",
            )?;
            for (file_id, blake3, size) in results {
                ins.execute(params![blake3, *size as i64])?;
                upd.execute(params![file_id, blake3])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Number of present files still lacking a content hash.
    pub fn count_without_hash(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE kind = 'file' AND status = 'present' AND blob_id IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Present row at exactly `path`, if any: (file_id, kind).
    pub fn file_at_path(&self, path: &Path) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT file_id, kind FROM files WHERE path = ?1 AND status = 'present'",
                [path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn file_status(&self, file_id: FileId) -> Result<Option<(PathBuf, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, status FROM files WHERE file_id = ?1",
                [key(file_id)],
                |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?)),
            )
            .optional()?)
    }

    /// Simple lexical search over the FTS mirror. Returns (path, status).
    pub fn search_lexical(&self, query: &str, limit: usize) -> Result<Vec<(PathBuf, String)>> {
        let mut st = self.conn.prepare(
            "SELECT f.path, f.status FROM files_fts
             JOIN files f ON f.rowid = files_fts.rowid
             WHERE files_fts MATCH ?1 ORDER BY bm25(files_fts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![query, limit as i64], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                r.get::<_, String>(1)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn status_label(s: FileStatus) -> &'static str {
        status_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(id: u64, path: &str, size: u64, mtime: i64) -> Entry {
        Entry {
            path: PathBuf::from(path),
            file_id: FileId {
                device: 1,
                index: id,
            },
            kind: EntryKind::File,
            size,
            mtime: Utc.timestamp_opt(mtime, 0).unwrap(),
            ctime: None,
            birthtime: None,
            depth: 1,
        }
    }

    #[test]
    fn tracks_rename_move_modify_and_missing() {
        let db = Db::open_in_memory().unwrap();
        let root = db.add_root(Path::new("/r")).unwrap();

        let s = db
            .upsert_entries(
                root.root_id,
                1,
                &[entry(1, "/r/a.txt", 5, 100), entry(2, "/r/b.txt", 5, 100)],
                1000,
            )
            .unwrap();
        assert_eq!((s.inserted, s.unchanged), (2, 0));

        // rename a -> a2 (same dir), modify b, and a new c
        let s = db
            .upsert_entries(
                root.root_id,
                2,
                &[
                    entry(1, "/r/a2.txt", 5, 100),
                    entry(2, "/r/b.txt", 9, 200),
                    entry(3, "/r/c.txt", 1, 1),
                ],
                2000,
            )
            .unwrap();
        assert_eq!((s.renamed, s.updated, s.inserted), (1, 1, 1));

        // move a2 into a subfolder; drop c
        let s = db
            .upsert_entries(
                root.root_id,
                3,
                &[
                    entry(1, "/r/sub/a2.txt", 5, 100),
                    entry(2, "/r/b.txt", 9, 200),
                ],
                3000,
            )
            .unwrap();
        assert_eq!((s.moved, s.unchanged), (1, 1));
        assert_eq!(db.mark_missing(root.root_id, 3, 3000).unwrap(), 1);

        let (p, st) = db
            .file_status(FileId {
                device: 1,
                index: 1,
            })
            .unwrap()
            .unwrap();
        assert_eq!(p, PathBuf::from("/r/sub/a2.txt"));
        assert_eq!(st, "present");
        let (_, st) = db
            .file_status(FileId {
                device: 1,
                index: 3,
            })
            .unwrap()
            .unwrap();
        assert_eq!(st, "missing");

        // event lineage for file 1: created, renamed, moved
        let kinds: Vec<String> = {
            let mut q = db
                .conn
                .prepare("SELECT type FROM file_events WHERE file_id = '1:1' ORDER BY event_id")
                .unwrap();
            q.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(kinds, ["created", "renamed", "moved"]);

        // FTS follows the move
        let hits = db.search_lexical("sub", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, PathBuf::from("/r/sub/a2.txt"));

        let c = db.counts().unwrap();
        assert_eq!((c.files, c.missing), (2, 1));
    }

    #[test]
    fn hashes_dedupe_into_blobs() {
        let db = Db::open_in_memory().unwrap();
        let root = db.add_root(Path::new("/r")).unwrap();
        db.upsert_entries(
            root.root_id,
            1,
            &[entry(1, "/r/a", 3, 1), entry(2, "/r/b", 3, 1)],
            1,
        )
        .unwrap();
        let b1 = db.set_file_hash("1:1", "abc", 3).unwrap();
        let b2 = db.set_file_hash("1:2", "abc", 3).unwrap();
        assert_eq!(b1, b2);
        assert!(db.files_without_hash(10).unwrap().is_empty());
    }
}
