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
