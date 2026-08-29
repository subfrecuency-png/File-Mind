//! End-to-end: real file-system events → debouncer → incremental index updates.

#![cfg(unix)]

use filemind_agent::pipeline;
use filemind_agent::watcher::{self, WatchStats};
use filemind_storage::Db;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait_for<F: Fn() -> bool>(what: &str, f: F) {
    let t = Instant::now();
    while !f() {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn present(db_path: &Path, p: &Path) -> bool {
    let db = Db::open(db_path).unwrap();
    db.file_at_path(p).unwrap().is_some()
}

#[test]
fn watcher_keeps_index_current() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root").canonicalize().unwrap_or_else(|_| {
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        tmp.path().join("root").canonicalize().unwrap()
    });
    std::fs::create_dir_all(&root).unwrap();
    let db_path = tmp.path().join("t.db");
    let adapter = filemind_adapter_macos::MacAdapter;

    // initial scan registers the root
    {
        let db = Db::open(&db_path).unwrap();
        std::fs::write(root.join("seed.txt"), b"seed").unwrap();
        pipeline::scan_root(&adapter, &db, &root).unwrap();
    }

    let stats = Arc::new(WatchStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let th = {
        let (stats, stop, db_path) = (stats.clone(), stop.clone(), db_path.clone());
        std::thread::spawn(move || {
            let db = Db::open(&db_path).unwrap();
            let adapter = filemind_adapter_macos::MacAdapter;
            watcher::run(&adapter, &db, stats, stop).unwrap();
        })
    };
    wait_for("watcher start", || {
        stats.watching_roots.load(Ordering::Relaxed) == 1
    });
    std::thread::sleep(Duration::from_millis(300)); // let the OS watch settle

    // create a file and a directory with a child
    let a = root.join("a.txt");
    std::fs::write(&a, b"hello").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let child = root.join("sub/child.md");
    std::fs::write(&child, b"# hi").unwrap();
    wait_for("create indexed", || {
        present(&db_path, &a) && present(&db_path, &child)
    });

    // rename the file: identity must survive, old path must go
    let b = root.join("b.txt");
    std::fs::rename(&a, &b).unwrap();
    wait_for("rename indexed", || {
        present(&db_path, &b) && !present(&db_path, &a)
    });
    {
        let db = Db::open(&db_path).unwrap();
        let kinds: Vec<String> = {
            let mut q = db
                .conn
                .prepare("SELECT type FROM file_events WHERE to_path = ?1 OR from_path = ?1 ORDER BY event_id")
                .unwrap();
            q.query_map([a.to_string_lossy()], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(
            kinds.contains(&"renamed".to_string()) || kinds.contains(&"moved".to_string()),
            "{kinds:?}"
        );
        let c = db.counts().unwrap();
        assert_eq!(c.missing, 0, "rename must not leave a missing row");
    }

    // delete the directory tree
    std::fs::remove_dir_all(root.join("sub")).unwrap();
    wait_for("delete indexed", || !present(&db_path, &child));

    stop.store(true, Ordering::Relaxed);
    th.join().unwrap();
    assert!(stats.raw_events.load(Ordering::Relaxed) > 0);
}

#[test]
fn root_directory_events_do_not_rescan() {
    use filemind_agent::incremental::apply_changes;
    use filemind_core::watch::Change;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("d")).unwrap();
    for i in 0..50 {
        std::fs::write(root.join("d").join(format!("{i}.txt")), b"x").unwrap();
    }
    let root = root.canonicalize().unwrap();
    let adapter = filemind_adapter_macos::MacAdapter;
    let db = Db::open_in_memory().unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    let st = apply_changes(&adapter, &db, &[Change::Upsert(root.clone())]).unwrap();
    assert_eq!(
        st.upserted, 0,
        "an event on the root itself must not re-enumerate it"
    );
    // an event on a known subdirectory upserts just that directory row
    let st = apply_changes(&adapter, &db, &[Change::Upsert(root.join("d"))]).unwrap();
    assert_eq!(st.upserted, 1);
}
