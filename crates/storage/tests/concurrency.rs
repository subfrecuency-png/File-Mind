//! Two connections writing at once must serialize, not fail with "database is locked".
use filemind_storage::Db;
use std::path::PathBuf;

#[test]
fn concurrent_writers_serialize() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("c.db");
    Db::open(&p).unwrap();
    let mk = |i: u64| {
        let p = p.clone();
        std::thread::spawn(move || {
            let db = Db::open(&p).unwrap();
            let root = db.add_root(&PathBuf::from(format!("/r{i}"))).unwrap();
            for round in 0..20 {
                let seq = db.begin_scan(root.root_id).unwrap();
                let entries: Vec<_> = (0..200)
                    .map(|n| filemind_core::adapter::Entry {
                        path: PathBuf::from(format!("/r{i}/{round}-{n}.txt")),
                        file_id: filemind_core::model::FileId {
                            device: i,
                            index: round * 1000 + n,
                        },
                        kind: filemind_core::model::EntryKind::File,
                        size: 1,
                        mtime: chrono::Utc::now(),
                        ctime: None,
                        birthtime: None,
                        depth: 1,
                    })
                    .collect();
                db.upsert_entries(root.root_id, seq, &entries, 1).unwrap();
                db.mark_missing(root.root_id, seq, 1).unwrap();
            }
        })
    };
    let hs: Vec<_> = (1..=4).map(mk).collect();
    for h in hs {
        h.join().expect("writer thread failed");
    }
    let db = Db::open(&p).unwrap();
    assert_eq!(db.counts().unwrap().roots, 4);
}
