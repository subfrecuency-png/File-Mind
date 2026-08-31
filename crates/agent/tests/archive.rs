//! Shrink tier 3 end to end: a cold project is packed into a verified
//! archive, chunks dedupe across archives, the original goes to Trash only
//! through the normal journaled transaction, search still sees the members,
//! restore brings back byte-identical files, and undo reverses everything.
#![cfg(unix)]

use filemind_adapter_macos::MacAdapter;
use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::{actions, archive};
use filemind_core::shrink::archive as fmt;
use filemind_storage::Db;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Both tests point `HOME` at their own tempdir (the pack lands under
/// `$HOME/FileMind Archive`), so they must not overlap.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Deterministic varied text so FastCDC has content to key on.
fn varied(lines: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::new();
    for i in 0..lines {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(format!("line {i} word {x:016x} in the project\n").as_bytes());
    }
    out
}

fn make_project(dir: &Path, seed: u64) {
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    std::fs::write(dir.join("src/main.rs"), varied(4_000, seed)).unwrap();
    std::fs::write(dir.join("src/lib.rs"), varied(6_000, seed + 1)).unwrap();
    std::fs::write(dir.join("src/deep/util.rs"), varied(2_000, seed + 2)).unwrap();
    std::fs::write(dir.join("README.md"), varied(300, seed + 3)).unwrap();
    for i in 0..12 {
        std::fs::write(
            dir.join(format!("src/mod{i}.rs")),
            varied(80, seed + 10 + i),
        )
        .unwrap();
    }
    std::fs::write(dir.join("notes.txt"), b"short\n").unwrap();
    std::os::unix::fs::symlink("src/main.rs", dir.join("entry")).unwrap();
}

fn tree_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(dir).min_depth(1) {
        let e = e.unwrap();
        let rel = e
            .path()
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .to_string();
        if e.file_type().is_file() {
            out.push((rel, std::fs::read(e.path()).unwrap()));
        } else if e.file_type().is_symlink() {
            out.push((
                format!("link:{rel}"),
                std::fs::read_link(e.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes(),
            ));
        } else {
            out.push((format!("dir:{rel}"), Vec::new()));
        }
    }
    out.sort();
    out
}

#[test]
fn build_verify_dedupe_and_restore() {
    let _guard = HOME_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let db = Db::open_in_memory().unwrap();

    let p1 = home.join("Documents/oldproj");
    make_project(&p1, 42);
    let before = tree_bytes(&p1);

    let b1 = archive::build(&db, &p1, "oldproj", None, |_, _| {}).unwrap();
    assert!(PathBuf::from(&b1.pack_path).exists());
    assert!(
        b1.bytes_stored < b1.bytes_raw / 2,
        "stored {} of raw {}",
        b1.bytes_stored,
        b1.bytes_raw
    );
    archive::verify(&db, &b1.archive_id).unwrap();
    assert_eq!(db.archive(&b1.archive_id).unwrap().unwrap().state, "ready");

    // a near-identical copy dedupes against the first pack
    let p2 = home.join("Documents/oldproj-v2");
    make_project(&p2, 42);
    std::fs::write(p2.join("CHANGES.md"), varied(200, 7)).unwrap();
    let b2 = archive::build(&db, &p2, "oldproj-v2", None, |_, _| {}).unwrap();
    assert!(
        b2.bytes_stored < b1.bytes_stored / 4,
        "second archive stored {} vs first {}",
        b2.bytes_stored,
        b1.bytes_stored
    );
    archive::verify(&db, &b2.archive_id).unwrap();

    // whole-archive restore refuses to clobber, restores byte-identically
    assert!(archive::restore(&db, &b1.archive_id, None, None).is_err());
    let out = home.join("Documents/restored");
    let r = archive::restore(&db, &b1.archive_id, None, Some(&out)).unwrap();
    assert!(r.files > 10);
    assert_eq!(tree_bytes(&out), before);

    // single-member restore
    let one = home.join("Documents/one");
    archive::restore(&db, &b1.archive_id, Some("src/lib.rs"), Some(&one)).unwrap();
    assert_eq!(
        std::fs::read(one.join("src/lib.rs")).unwrap(),
        varied(6_000, 43)
    );

    // corruption is caught, loudly
    let a1 = db.archive(&b1.archive_id).unwrap().unwrap();
    let mut pack = std::fs::read(&a1.pack_path).unwrap();
    let mid = pack.len() / 2;
    pack[mid] ^= 0xFF;
    std::fs::write(&a1.pack_path, &pack).unwrap();
    assert!(archive::verify(&db, &b1.archive_id).is_err());
}

#[test]
fn suggestion_flows_from_estimate_to_trash_search_and_undo() {
    let _guard = HOME_LOCK.lock().unwrap();
    let adapter = MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let root = home.join("Documents");
    let proj = root.join("coldproj");
    make_project(&proj, 9);
    let root = root.canonicalize().unwrap();
    let proj = root.join("coldproj");

    let db = Db::open_in_memory().unwrap();
    db.add_root(&root).unwrap();
    db.set_setting("mode", &serde_json::json!("assist"))
        .unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    pipeline::hash_pending(
        &db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();

    // a cached estimate that measured this project as cold
    let bytes: u64 = tree_bytes(&proj).iter().map(|(_, b)| b.len() as u64).sum();
    db.set_shrink_estimate(&serde_json::json!({
        "computed_ts": chrono::Utc::now().timestamp(),
        "tiers": [{
            "tier": 3, "kind": "cold_archive",
            "projects": [{
                "project_id": 1, "name": "coldproj",
                "root_path": proj.to_string_lossy(),
                "end_ts": chrono::Utc::now().timestamp() - 200 * 86_400,
                "files": 17, "bytes": bytes,
                "saving_bytes": 400u64 << 20
            }]
        }]
    }))
    .unwrap();
    db.refresh_suggestions(&[], &[]).unwrap();
    let s = db
        .list_suggestions("proposed", usize::MAX)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "archive_cold_project")
        .expect("archive suggestion");
    assert_eq!(s.risk_tier, 1);
    assert!(s.rationale.contains("archive"), "{}", s.rationale);

    // a sensitive file inside blocks the plan
    db.conn
        .execute(
            "UPDATE files SET sensitive = 1 WHERE path LIKE ?1 AND path LIKE '%main.rs'",
            [format!("{}/%", proj.to_string_lossy())],
        )
        .unwrap();
    let (_, plan) = actions::plan(&adapter, &db, s.id).unwrap();
    assert!(
        plan.problems.iter().any(|p| p.contains("sensitive")),
        "{:?}",
        plan.problems
    );
    db.conn
        .execute("UPDATE files SET sensitive = 0", [])
        .unwrap();

    let (_, plan) = actions::plan(&adapter, &db, s.id).unwrap();
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    assert_eq!(plan.steps, 1, "one Trash step for the whole tree");

    let applied = actions::apply(
        &adapter,
        &db,
        s.id,
        true,
        Some((&plan.txn_id, &plan.fingerprint)),
    )
    .unwrap();
    assert_eq!((applied.done, applied.failed), (1, 0));
    assert!(!proj.exists(), "original tree is in the Trash");
    let a = db.archives().unwrap().into_iter().next().unwrap();
    assert_eq!(a.state, "ready");
    assert_eq!(a.txn_id.as_deref(), Some(applied.txn_id.as_str()));

    // the index still knows every member, marked archived + located
    let (n, loc): (i64, String) = db
        .conn
        .query_row(
            "SELECT COUNT(*), MIN(location) FROM files WHERE status = 'archived'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(n >= 17, "{n}");
    let (aid, _) = fmt::parse_location(&loc).expect("location marker");
    assert_eq!(aid, a.archive_id);

    // restoring one member out of the archive makes it a present file again
    let r = archive::restore(&db, &a.archive_id, Some("README.md"), Some(&proj)).unwrap();
    assert_eq!(r.files, 1);
    assert_eq!(
        std::fs::read(proj.join("README.md")).unwrap(),
        varied(300, 12)
    );

    // undo puts the tree back and clears the markers
    std::fs::remove_dir_all(&proj).unwrap(); // clear the partial restore for Put Back
    let u = actions::undo(&adapter, &db, &applied.txn_id).unwrap();
    assert_eq!(u.restored, 1, "{:?}", u.skipped);
    assert!(proj.join("src/main.rs").exists());
    let archived: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE status = 'archived' OR location IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 0);
}
