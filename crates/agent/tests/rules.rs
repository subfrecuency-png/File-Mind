//! Automate mode: a rule shows its work for the preview period, cannot be
//! armed early, runs only in Automate mode once armed, every run is a
//! journaled undoable transaction, and a conflict pauses it.
#![cfg(unix)]

use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::{actions, rules};
use filemind_storage::Db;
use serde_json::json;
use std::path::PathBuf;

const MB: usize = 1 << 20;

/// Both tests point $HOME at their own temp dir; keep them from interleaving.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

fn old(p: &std::path::Path, days: i64) {
    let t = filetime::FileTime::from_unix_time(chrono::Utc::now().timestamp() - days * 86_400, 0);
    filetime::set_file_mtime(p, t).unwrap();
}

/// A home with Documents + Downloads: one large duplicate pair, one old
/// loose download, one recent download, one sensitive-looking duplicate.
fn setup() -> (tempfile::TempDir, PathBuf, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let root = home.join("fx");
    std::fs::create_dir_all(root.join("Documents/Offers")).unwrap();
    std::fs::create_dir_all(root.join("Downloads/some project/src")).unwrap();
    let body: Vec<u8> = (0..2 * MB).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("Documents/Offers/offer sheet.pdf"), &body).unwrap();
    std::fs::write(root.join("Downloads/offer sheet.pdf"), &body).unwrap();
    old(&root.join("Downloads/offer sheet.pdf"), 60);
    std::fs::write(root.join("Downloads/old-installer.dmg"), vec![7u8; 300_000]).unwrap();
    old(&root.join("Downloads/old-installer.dmg"), 200);
    std::fs::write(root.join("Downloads/fresh.zip"), vec![9u8; 300_000]).unwrap();
    std::fs::write(root.join("Downloads/.DS_Store"), vec![1u8; 6_000]).unwrap();
    old(&root.join("Downloads/.DS_Store"), 300);
    std::fs::create_dir_all(root.join("Downloads/bundle")).unwrap();
    std::fs::write(
        root.join("Downloads/bundle/readme.txt"),
        b"part of a bundle",
    )
    .unwrap();
    old(&root.join("Downloads/bundle/readme.txt"), 300);
    std::fs::write(
        root.join("Downloads/some project/src/main.rs"),
        b"fn main(){}",
    )
    .unwrap();
    old(&root.join("Downloads/some project/src/main.rs"), 400);
    let root = root.canonicalize().unwrap();
    let db = Db::open_in_memory().unwrap();
    db.add_root(&root).unwrap();
    (tmp, root, db)
}

fn analyse(db: &Db, root: &std::path::Path) {
    let adapter = filemind_adapter_macos::MacAdapter;
    pipeline::scan_root(&adapter, db, root).unwrap();
    pipeline::hash_pending(
        db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    db.rebuild_duplicates().unwrap();
    db.rebuild_versions().unwrap();
}

#[test]
fn preview_then_arm_then_run_then_undo() {
    let _guard = serial();
    let adapter = filemind_adapter_macos::MacAdapter;
    let (_tmp, root, db) = setup();
    analyse(&db, &root);

    // unknown or over-wide rules are refused at the door
    assert!(rules::add(&db, "rm_rf", &json!({})).is_err());
    assert!(rules::add(
        &db,
        "trash_exact_duplicates",
        &json!({"max_items_per_run": 9999})
    )
    .is_err());

    let dup = rules::add(
        &db,
        "trash_exact_duplicates",
        &json!({"min_bytes": 1_000_000}),
    )
    .unwrap();
    let arch = rules::add(
        &db,
        "archive_stale_downloads",
        &json!({"older_than_days": 90}),
    )
    .unwrap();
    assert_eq!(dup.state, "preview");

    // planning finds exactly the expected files and touches nothing
    let (m, eval) = rules::plan(&adapter, &db, &dup).unwrap();
    assert_eq!(eval.steps, 1, "{}", eval.diff);
    assert!(m.diff().contains("Downloads/offer sheet.pdf"));
    assert!(m.keeps[0].ends_with("Documents/Offers/offer sheet.pdf"));
    let (m2, eval2) = rules::plan(&adapter, &db, &arch).unwrap();
    assert_eq!(eval2.steps, 1, "only the old loose file:\n{}", m2.diff());
    assert!(m2.diff().contains("old-installer.dmg"));
    assert!(!m2.diff().contains("fresh.zip"));
    assert!(
        !m2.diff().contains("main.rs"),
        "deep project trees are never archived"
    );
    assert!(
        !m2.diff().contains(".DS_Store"),
        "hidden files are never candidates"
    );
    assert!(
        !m2.diff().contains("bundle/readme.txt"),
        "files inside a subfolder are left to Assist mode"
    );
    assert!(root.join("Downloads/offer sheet.pdf").exists());

    // observe mode, preview state: a tick records dry runs, moves nothing
    db.set_setting("mode", &json!("observe")).unwrap();
    let t = rules::tick(&adapter, &db).unwrap();
    assert!(t.iter().all(|o| o.dry_run));
    assert!(root.join("Downloads/offer sheet.pdf").exists());
    let w = rules::would_have(&db, &dup, 7).unwrap();
    assert_eq!(w.dry_runs, 1);
    assert_eq!(w.files.len(), 1);
    assert!(!w.armable, "7-day preview not over");

    // cannot arm before the preview period
    assert!(rules::arm(&db, dup.rule_id).is_err());

    // shorten the preview period (test only) and arm
    db.set_setting("automate.preview_days", &json!(0)).unwrap();
    let armed = rules::arm(&db, dup.rule_id).unwrap();
    assert_eq!(armed.state, "armed");

    // armed but mode is assist: still dry runs only
    db.set_setting("mode", &json!("assist")).unwrap();
    let t = rules::tick(&adapter, &db).unwrap();
    assert!(t.iter().all(|o| o.dry_run));
    assert!(root.join("Downloads/offer sheet.pdf").exists());

    // automate: the armed rule executes as a journaled transaction
    db.set_setting("mode", &json!("automate")).unwrap();
    let t = rules::tick(&adapter, &db).unwrap();
    let ran = t.iter().find(|o| o.rule_id == dup.rule_id).unwrap();
    assert!(!ran.dry_run);
    let txn_id = ran.txn_id.clone().unwrap();
    assert!(
        !root.join("Downloads/offer sheet.pdf").exists(),
        "copy trashed"
    );
    assert!(
        root.join("Documents/Offers/offer sheet.pdf").exists(),
        "keeper untouched"
    );
    // the un-armed rule still only previewed
    let other = t.iter().find(|o| o.rule_id == arch.rule_id).unwrap();
    assert!(other.dry_run);
    assert!(root.join("Downloads/old-installer.dmg").exists());

    // it is in History with the rule as initiator, and undo brings the file back
    let (man, _, _) = db.load_txn(&txn_id).unwrap().unwrap();
    assert_eq!(man.initiator, filemind_core::txn::Initiator::Rule);
    assert_eq!(
        man.rule_id.as_deref(),
        Some(format!("rule:{}", dup.rule_id).as_str())
    );
    let undone = actions::undo(&adapter, &db, &txn_id).unwrap();
    assert_eq!(undone.restored, 1);
    assert!(root.join("Downloads/offer sheet.pdf").exists());

    let runs = db.list_automation_runs(dup.rule_id, 0, 100).unwrap();
    assert!(runs
        .iter()
        .any(|r| !r.dry_run && r.txn_id.as_deref() == Some(&txn_id)));
}

#[test]
fn a_conflict_pauses_the_rule() {
    let _guard = serial();
    let adapter = filemind_adapter_macos::MacAdapter;
    let (_tmp, root, db) = setup();
    analyse(&db, &root);
    db.set_setting("automate.preview_days", &json!(0)).unwrap();
    db.set_setting("mode", &json!("automate")).unwrap();
    let arch = rules::add(&db, "archive_stale_downloads", &json!({})).unwrap();
    rules::tick(&adapter, &db).unwrap(); // dry run so it becomes armable
    rules::arm(&db, arch.rule_id).unwrap();

    // the index says old-installer.dmg is there, but the disk disagrees
    std::fs::rename(
        root.join("Downloads/old-installer.dmg"),
        root.join("Downloads/renamed-by-user.dmg"),
    )
    .unwrap();
    let t = rules::tick(&adapter, &db).unwrap();
    let o = t.iter().find(|o| o.rule_id == arch.rule_id).unwrap();
    assert!(o.dry_run, "nothing executed");
    assert!(o.paused.is_some(), "rule paused: {o:?}");
    assert_eq!(
        db.get_automation(arch.rule_id).unwrap().unwrap().state,
        "paused"
    );
    // paused rules are skipped entirely on the next tick
    let t = rules::tick(&adapter, &db).unwrap();
    assert!(t.iter().all(|o| o.rule_id != arch.rule_id));
}
