//! Duplicate groups, version chains, health and suggestions on the synthetic fixture.
#![cfg(unix)]

use filemind_agent::fixture;
use filemind_agent::pipeline::{self, HashOpts};
use filemind_storage::Db;

#[test]
fn finds_planted_duplicates_and_versions() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("fx");
    let planted = fixture::build(&root, 3000).unwrap();
    let root = root.canonicalize().unwrap();

    let db = Db::open_in_memory().unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    pipeline::hash_pending(
        &db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();

    // exact duplicates: every planted "(1)" copy must be found, and nothing else
    let groups = db.rebuild_duplicates().unwrap();
    let copies: usize = groups.iter().map(|g| g.copies.len()).sum();
    assert_eq!(
        copies, planted.duplicates,
        "every planted duplicate found, no false positives"
    );
    for g in &groups {
        // keeper is the clean name, the "(1)" copy is what gets suggested away
        assert!(
            !g.keeper.to_string_lossy().contains("(1)"),
            "keeper {:?}",
            g.keeper
        );
        assert!(g.copies.iter().all(|c| c.to_string_lossy().contains("(1)")));
    }

    // version chains: report-N_v1..vK in versions/
    let chains = db.rebuild_versions().unwrap();
    let found = chains
        .iter()
        .filter(|c| c.canonical.to_string_lossy().contains("/versions/"))
        .count();
    assert!(
        found as f64 >= planted.version_chains as f64 * 0.9,
        "found {found} of {} chains",
        planted.version_chains
    );
    for c in chains
        .iter()
        .filter(|c| c.canonical.to_string_lossy().contains("/versions/"))
    {
        // newest (highest v) is canonical because fixture writes v1..vN in order
        let name = c
            .canonical
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let max_v = c.older.len() + 1;
        assert!(
            name.ends_with(&format!("_v{max_v}.docx")),
            "canonical {name} for chain of {max_v}"
        );
    }
    // no chain outside versions/ (project files are "word-N.ext" with unique N — a
    // trailing ≤3-digit number is a legitimate marker, so allow but bound it)
    let elsewhere = chains.len() - found;
    assert!(elsewhere <= chains.len() / 2, "{elsewhere} spurious chains");

    // health + suggestions
    let h = db.refresh_health().unwrap();
    assert!(h.score < 100 && h.score > 0, "score {}", h.score);
    assert!(h
        .components
        .iter()
        .any(|c| c.name == "duplicates" && c.penalty > 0.0));
    let n = db.refresh_suggestions(&groups, &chains).unwrap();
    assert!(n > 0);
    let proposed = db.list_suggestions("proposed", 1000).unwrap();
    assert!(proposed.iter().any(|s| s.kind == "collapse_versions"));
    // dismiss one, regenerate, it stays dismissed
    let first = proposed[0].id;
    assert!(db.set_suggestion_state(first, "dismissed").unwrap());
    db.refresh_suggestions(&groups, &chains).unwrap();
    let dismissed = db.list_suggestions("dismissed", 10).unwrap();
    assert_eq!(dismissed[0].id, first);
}

/// A folder that is a byte-for-byte copy of another becomes one suggestion,
/// and its per-file duplicates are not listed separately.
#[test]
fn whole_folder_copies_are_one_suggestion() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    std::env::set_var("XDG_DATA_HOME", tmp.path().join(".local/share"));
    let root = tmp.path().join("r");
    for (dir, salt) in [("creditos", 0u8), ("creditos-v1", 0), ("other", 1)] {
        let d = root.join(dir).join("src");
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..6u8 {
            let body: Vec<u8> = (0..100_000u32).map(|x| (x as u8) ^ i ^ salt).collect();
            std::fs::write(d.join(format!("f{i}.bin")), body).unwrap();
        }
    }
    let root = root.canonicalize().unwrap();
    let db = Db::open_in_memory().unwrap();
    db.add_root(&root).unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    pipeline::hash_pending(
        &db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let folders = db.folder_duplicates(5).unwrap();
    assert_eq!(folders.len(), 1, "{folders:?}");
    assert!(
        folders[0].keeper.ends_with("creditos"),
        "unmarked name is kept: {:?}",
        folders[0]
    );
    assert_eq!(folders[0].copies.len(), 1);
    assert_eq!(folders[0].files, 6);

    let dups = db.rebuild_duplicates().unwrap();
    let chains = db.rebuild_versions().unwrap();
    db.refresh_suggestions(&dups, &chains).unwrap();
    let s = db.list_suggestions("proposed", usize::MAX).unwrap();
    assert_eq!(
        s.iter()
            .filter(|x| x.kind == "trash_duplicate_folder")
            .count(),
        1
    );
    assert_eq!(
        s.iter().filter(|x| x.kind == "trash_duplicates").count(),
        0,
        "per-file dups folded into the folder suggestion"
    );

    // the whole copy goes to Trash as one step, and comes back as one step
    db.set_setting("mode", &serde_json::json!("assist"))
        .unwrap();
    let sug = s
        .iter()
        .find(|x| x.kind == "trash_duplicate_folder")
        .unwrap();
    let (_, plan) = filemind_agent::actions::plan(&adapter, &db, sug.id).unwrap();
    assert_eq!(plan.steps, 1, "{}", plan.diff);
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    let applied = filemind_agent::actions::apply(&adapter, &db, sug.id, true, None).unwrap();
    assert_eq!(applied.done, 1);
    assert!(!root.join("creditos-v1").exists());
    assert!(root.join("creditos/src/f0.bin").exists());
    let trashed: i64 = db
        .conn
        .query_row(
            "SELECT count(*) FROM files WHERE status='trashed' AND kind='file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(trashed, 6, "files under the trashed folder are marked");
    let undone = filemind_agent::actions::undo(&adapter, &db, &applied.txn_id).unwrap();
    assert_eq!(undone.restored, 1);
    assert!(root.join("creditos-v1/src/f0.bin").exists());
    let trashed: i64 = db
        .conn
        .query_row(
            "SELECT count(*) FROM files WHERE status='trashed' AND kind='file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(trashed, 0);
}
