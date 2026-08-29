//! plan → apply on a real suggestion: the preview names the kept file, the
//! executed transaction carries the previewed id, and a stale preview is refused.
#![cfg(unix)]

use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::{actions, fixture};
use filemind_storage::Db;

#[test]
fn apply_runs_under_the_previewed_id_and_shows_the_keeper() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    // trash lives under $HOME (freedesktop on Linux); keep it on the same volume
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let root = home.join("fx");
    fixture::build(&root, 800).unwrap();
    // one big duplicate pair so a trash_duplicates suggestion clears the 64 KB bar
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::create_dir_all(root.join("Documents/Offers")).unwrap();
    std::fs::write(root.join("Documents/Offers/offer sheet.pdf"), &body).unwrap();
    std::fs::write(root.join("Downloads/offer sheet.pdf"), &body).unwrap();
    let root = root.canonicalize().unwrap();

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
    let dups = db.rebuild_duplicates().unwrap();
    let chains = db.rebuild_versions().unwrap();
    db.refresh_suggestions(&dups, &chains).unwrap();

    let s = db
        .list_suggestions("proposed", usize::MAX)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "trash_duplicates")
        .expect("fixture plants duplicates ≥ 64 KB");

    let (_, plan) = actions::plan(&adapter, &db, s.id).unwrap();
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    let keep = s.subject["keep"].as_str().unwrap();
    assert!(
        plan.diff.contains("KEEP") && plan.diff.contains(keep),
        "preview names the kept copy:\n{}",
        plan.diff
    );

    // a preview that no longer matches the steps must not run
    let err = actions::apply(&adapter, &db, s.id, true, Some((&plan.txn_id, "deadbeef")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed since"), "{err}");

    let applied = actions::apply(
        &adapter,
        &db,
        s.id,
        true,
        Some((&plan.txn_id, &plan.fingerprint)),
    )
    .unwrap();
    assert_eq!(applied.txn_id, plan.txn_id, "history id == previewed id");
    assert_eq!(applied.failed, 0);
    assert_eq!(applied.state, "done");
    assert!(std::path::Path::new(keep).exists(), "kept file untouched");

    // the same id cannot be reused for a second run
    let err = actions::apply(
        &adapter,
        &db,
        s.id,
        true,
        Some((&plan.txn_id, &plan.fingerprint)),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no proposed suggestion") || err.contains("already exists"),
        "{err}"
    );

    let undone = actions::undo(&adapter, &db, &plan.txn_id).unwrap();
    assert_eq!(undone.restored, applied.done);
}
