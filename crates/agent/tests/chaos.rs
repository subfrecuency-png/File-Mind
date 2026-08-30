//! The Protect guarantee under fire.
//!
//! A transaction of moves and trashes is executed with a simulated crash at
//! every possible point (before and after each step's disk operation), then
//! recovered, then undone. After every run, every original file must exist
//! exactly once with its original contents, either where it started or where
//! the journal says it is — never lost, never duplicated, never overwritten.
#![cfg(unix)]

use filemind_core::txn::{
    self, CrashPoint, Initiator, Journal, Manifest, Step, StepState, TxnState,
};
use filemind_core::{Mode, RiskTier};
use filemind_storage::Db;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const N: usize = 8;

/// Make a fresh tree with N files (contents = index) and return their paths.
fn fresh_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let tag = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    (0..N)
        .map(|i| {
            let p = src.join(format!("file{i}.txt"));
            let body = format!("{tag}-content-{i}-{}", "x".repeat(i * 37)).into_bytes();
            std::fs::write(&p, &body).unwrap();
            (p, body)
        })
        .collect()
}

fn manifest(files: &[(PathBuf, Vec<u8>)], dir: &Path) -> Manifest {
    let mut m = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier2, "chaos");
    for (i, (p, _)) in files.iter().enumerate() {
        if i % 3 == 2 {
            m.steps.push(Step::Trash {
                path: p.clone(),
                hash_before: None,
                trashed_to: None,
            });
        } else {
            m.steps.push(Step::Move {
                from: p.clone(),
                to: dir.join("dst").join(format!("moved{i}.txt")),
                hash_before: None,
            });
        }
    }
    m
}

/// Every original body must be found exactly once across the tree + trash.
fn assert_all_present_once(bodies: &[Vec<u8>], roots: &[&Path]) {
    let mut found: HashMap<&[u8], usize> = HashMap::new();
    for r in roots {
        for e in walkdir::WalkDir::new(r).into_iter().flatten() {
            if e.file_type().is_file() {
                let b = std::fs::read(e.path()).unwrap();
                if let Some(orig) = bodies.iter().find(|x| **x == b) {
                    *found.entry(orig.as_slice()).or_default() += 1;
                }
            }
        }
    }
    for b in bodies {
        assert_eq!(
            found.get(b.as_slice()).copied().unwrap_or(0),
            1,
            "body {:?} count",
            String::from_utf8_lossy(b)
        );
    }
}

#[test]
fn crash_at_every_point_loses_nothing_and_undo_restores() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    // Trash lives under a fake HOME so the test never touches the real one.
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));

    let mut crash_points = vec![CrashPoint::Never];
    for i in 0..N {
        crash_points.push(CrashPoint::BeforeOp(i));
        crash_points.push(CrashPoint::AfterOp(i));
    }
    // 17 crash points × 12 repetitions ≈ 200 runs
    let mut runs = 0;
    for rep in 0..12 {
        for &cp in &crash_points {
            runs += 1;
            let dir = tmp.path().join(format!("run-{rep}-{runs}"));
            std::fs::create_dir_all(&dir).unwrap();
            let files = fresh_tree(&dir);
            let bodies: Vec<Vec<u8>> = files.iter().map(|(_, b)| b.clone()).collect();
            let db = Db::open(&dir.join("j.db")).unwrap();
            let mut m = manifest(&files, &dir);
            let problems = txn::validate(&adapter, std::slice::from_ref(&dir), &mut m).unwrap();
            assert!(problems.is_empty(), "{problems:?}");

            let res = txn::execute(&adapter, &db, &mut m, cp);
            // freedesktop Trash on Linux, ~/.Trash on macOS — both under the fake HOME
            let trash = home.join(".local/share/Trash");
            let mac_trash = home.join(".Trash");
            let roots: Vec<&Path> = vec![&dir, &trash, &mac_trash];
            match cp {
                CrashPoint::Never => assert!(res.is_ok()),
                _ => assert!(res.is_err(), "crash point {cp:?} should interrupt"),
            }
            // 1. nothing lost mid-crash
            assert_all_present_once(&bodies, &roots);

            // 2. recovery settles the journal without touching files
            let (_, st_before, _) = db.load_txn(&m.txn_id).unwrap().unwrap();
            if st_before != TxnState::Done {
                let r = txn::recover(&adapter, &db, &m.txn_id).unwrap();
                let (_, st, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
                assert!(matches!(st, TxnState::Done | TxnState::Failed), "{st:?}");
                // the Trash destination is journaled before the move, so even an
                // after-op crash on a Trash step settles cleanly
                assert_eq!(r.conflicts, 0, "cp {cp:?} steps {steps:?}");
                assert!(
                    !steps.contains(&StepState::Running),
                    "no step may stay running: {steps:?}"
                );
            }
            assert_all_present_once(&bodies, &roots);

            // 3. undo restores every done step; skipped ones are reported, not lost
            let u = txn::undo(&adapter, &db, &m.txn_id).unwrap();
            let (_, _, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
            let done_left = steps.iter().filter(|s| **s == StepState::Done).count();
            assert_eq!(
                done_left, 0,
                "undo must reverse every done step: {steps:?} skipped {:?}",
                u.skipped
            );
            assert_all_present_once(&bodies, &roots);
            for (i, (p, _)) in files.iter().enumerate() {
                if steps[i] == StepState::Undone || steps[i] == StepState::Planned {
                    assert!(
                        p.exists(),
                        "run {runs} cp {cp:?}: {} should be back",
                        p.display()
                    );
                }
            }
            assert!(db.unfinished().unwrap().is_empty());
        }
    }
    assert!(runs >= 200, "{runs} runs");
}

#[test]
fn undo_refuses_to_clobber_edited_or_occupied() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("t");
    let files = fresh_tree(&dir);
    let db = Db::open_in_memory().unwrap();
    let mut m = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier1, "edit test");
    m.steps.push(Step::Move {
        from: files[0].0.clone(),
        to: dir.join("a.txt"),
        hash_before: None,
    });
    m.steps.push(Step::Move {
        from: files[1].0.clone(),
        to: dir.join("b.txt"),
        hash_before: None,
    });
    assert!(txn::validate(&adapter, std::slice::from_ref(&dir), &mut m)
        .unwrap()
        .is_empty());
    txn::execute(&adapter, &db, &mut m, CrashPoint::Never).unwrap();

    // edit the first moved file, re-occupy the second's origin
    std::fs::write(dir.join("a.txt"), b"edited after move").unwrap();
    std::fs::write(&files[1].0, b"new file in old place").unwrap();

    let u = txn::undo(&adapter, &db, &m.txn_id).unwrap();
    assert_eq!(u.restored, 0);
    assert_eq!(u.skipped.len(), 2, "{:?}", u.skipped);
    assert_eq!(
        std::fs::read(dir.join("a.txt")).unwrap(),
        b"edited after move"
    );
    assert_eq!(
        std::fs::read(&files[1].0).unwrap(),
        b"new file in old place"
    );
    assert!(dir.join("b.txt").exists());
}

#[test]
fn mode_gate() {
    let m = Manifest::new(Mode::Observe, Initiator::User, RiskTier::Tier2, "x");
    assert!(txn::permitted(Mode::Observe, &m, true).is_err());
    assert!(txn::permitted(Mode::Assist, &m, false).is_err());
    assert!(txn::permitted(Mode::Assist, &m, true).is_ok());
    let mut r = Manifest::new(Mode::Automate, Initiator::Rule, RiskTier::Tier2, "x");
    assert!(txn::permitted(Mode::Automate, &r, false).is_err());
    r.risk_tier = RiskTier::Tier0;
    assert!(txn::permitted(Mode::Automate, &r, false).is_ok());
}
