use filemind_agent::pipeline::*;
use filemind_storage::Db;

#[cfg(unix)]
#[test]
fn scan_persists_and_tracks_rename_across_runs() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(root.join("a/one.txt"), b"one").unwrap();
    std::fs::write(root.join("two.txt"), b"two").unwrap();
    std::fs::write(root.join("dup.txt"), b"two").unwrap();

    let db = Db::open_in_memory().unwrap();
    let o = scan_root(&adapter, &db, &root).unwrap();
    assert_eq!(o.report.files, 3);
    assert_eq!(o.upsert.inserted, 4); // 3 files + 1 dir

    // rename + delete, rescan
    std::fs::rename(root.join("two.txt"), root.join("b").with_extension("txt")).unwrap();
    std::fs::remove_file(root.join("dup.txt")).unwrap();
    let o = scan_root(&adapter, &db, &root).unwrap();
    eprintln!("{:?} missing={}", o.upsert, o.missing);
    assert_eq!(o.upsert.renamed, 1);
    assert_eq!(o.missing, 1);

    let c = db.counts().unwrap();
    assert_eq!((c.files, c.missing), (2, 1));

    // hashing pass
    let h = hash_pending(
        &db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((h.hashed, h.errors, h.remaining), (2, 0, 0));
    let c = db.counts().unwrap();
    assert_eq!(c.hashed, 2);

    // lexical search finds the renamed file by its new name
    let hits = db.search_lexical("b", 5).unwrap();
    assert_eq!(hits[0].0, root.canonicalize().unwrap().join("b.txt"));
}

#[cfg(unix)]
#[test]
fn hard_links_get_their_own_identity() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("orig.txt"), b"same").unwrap();
    std::fs::hard_link(root.join("orig.txt"), root.join("link.txt")).unwrap();

    let db = Db::open_in_memory().unwrap();
    let o = scan_root(&adapter, &db, &root).unwrap();
    assert_eq!(o.report.files, 2);
    assert_eq!(o.upsert.moved, 0, "a hard link is not a move");
    let c = db.counts().unwrap();
    assert_eq!(c.files, 2);

    // a second scan is stable
    let o = scan_root(&adapter, &db, &root).unwrap();
    assert_eq!(
        (o.upsert.moved, o.upsert.inserted, o.upsert.unchanged),
        (0, 0, 2)
    );
}
