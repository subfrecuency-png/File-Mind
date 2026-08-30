//! Shrink tier 1 through the transaction manager: a `Rewrite` step is
//! journaled, verified against `hash_before`, survives a crash at every
//! point, and undoes in place. The platform mechanism is simulated here (an
//! adapter that only *says* the file is compressed) so this runs on every
//! CI runner; the real APFS calls are exercised by
//! `filemind_adapter_macos::apfs` tests on macOS.
#![cfg(unix)]

use filemind_adapter_macos::MacAdapter;
use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::{actions, shrink};
use filemind_core::adapter::*;
use filemind_core::txn::{
    self, CrashPoint, Initiator, Journal, Manifest, Step, StepState, TxnState,
};
use filemind_core::{Mode, Result, RiskTier};
use filemind_storage::Db;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

/// `MacAdapter` plus a pretend compression layer: the file on disk never
/// changes; what changes is what the adapter reports about it.
#[derive(Default)]
struct SimAdapter {
    inner: MacAdapter,
    state: Mutex<HashMap<PathBuf, RewriteState>>,
    /// `rewrite` of this path stops after writing the payload (crash simulation).
    half_done_at: Mutex<Option<PathBuf>>,
}

impl SimAdapter {
    fn state_of(&self, p: &Path) -> RewriteState {
        *self
            .state
            .lock()
            .unwrap()
            .get(p)
            .unwrap_or(&RewriteState::Original)
    }
}

impl OsAdapter for SimAdapter {
    fn platform(&self) -> &'static str {
        "sim"
    }
    fn watch(&self, r: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
        self.inner.watch(r, tx)
    }
    fn enumerate(
        &self,
        root: &Path,
        opts: &ScanOptsNative,
    ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>> {
        self.inner.enumerate(root, opts)
    }
    fn stat(&self, p: &Path) -> Result<Option<Entry>> {
        self.inner.stat(p)
    }
    fn native_metadata(&self, p: &Path) -> Result<NativeMeta> {
        self.inner.native_metadata(p)
    }
    fn native_search(&self, q: &str) -> Result<Vec<PathBuf>> {
        self.inner.native_search(q)
    }
    fn move_to_trash(&self, p: &Path) -> Result<TrashReceipt> {
        self.inner.move_to_trash(p)
    }
    fn trash_target(&self, p: &Path) -> Result<PathBuf> {
        self.inner.trash_target(p)
    }
    fn move_to_trash_at(&self, p: &Path, t: &Path) -> Result<TrashReceipt> {
        self.inner.move_to_trash_at(p, t)
    }
    fn rename_no_clobber(&self, f: &Path, t: &Path) -> Result<()> {
        self.inner.rename_no_clobber(f, t)
    }
    fn protected_roots(&self) -> Vec<PathBuf> {
        self.inner.protected_roots()
    }
    fn register_autostart(&self, e: bool) -> Result<()> {
        self.inner.register_autostart(e)
    }
    fn link_kind(&self, p: &Path) -> Result<LinkKind> {
        self.inner.link_kind(p)
    }
    fn default_roots(&self) -> Vec<PathBuf> {
        self.inner.default_roots()
    }
    fn rewrite(&self, p: &Path, method: &str) -> Result<RewriteReceipt> {
        assert_eq!(method, "apfs");
        let len = std::fs::metadata(p)?.len();
        let mut st = self.state.lock().unwrap();
        match st.get(p) {
            Some(RewriteState::Rewritten) => {
                return Err(filemind_core::CoreError::Other(anyhow::anyhow!(
                    "already compressed"
                )))
            }
            Some(RewriteState::HalfDone) => {
                st.insert(p.to_path_buf(), RewriteState::Rewritten);
            }
            _ => {
                if self.half_done_at.lock().unwrap().as_deref() == Some(p) {
                    st.insert(p.to_path_buf(), RewriteState::HalfDone);
                } else {
                    st.insert(p.to_path_buf(), RewriteState::Rewritten);
                }
            }
        }
        Ok(RewriteReceipt {
            on_disk_before: len,
            on_disk_after: len / 3,
        })
    }
    fn rewrite_restore(&self, p: &Path, _method: &str) -> Result<()> {
        self.state.lock().unwrap().remove(p);
        Ok(())
    }
    fn rewrite_state(&self, p: &Path, _method: &str) -> Result<RewriteState> {
        Ok(self.state_of(p))
    }
    fn on_disk_bytes(&self, p: &Path) -> Result<u64> {
        let len = std::fs::metadata(p)?.len();
        Ok(match self.state_of(p) {
            RewriteState::Rewritten => len / 3,
            _ => len,
        })
    }
}

fn old(p: &Path, days: i64) {
    let t = filetime::FileTime::from_unix_time(chrono::Utc::now().timestamp() - days * 86_400, 0);
    filetime::set_file_mtime(p, t).unwrap();
}

fn text(n: usize, tag: &str) -> Vec<u8> {
    format!("{tag}: the quick brown fox jumps over the lazy dog\n")
        .repeat(n)
        .into_bytes()
}

fn rewrite_step(p: &Path) -> Step {
    Step::Rewrite {
        path: p.to_path_buf(),
        method: "apfs".into(),
        hash_before: None,
        hash_after_decoded: None,
        bytes_before: 0,
        bytes_after: None,
    }
}

#[test]
fn rewrite_is_verified_journaled_and_undone_in_place() {
    let adapter = SimAdapter::default();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("root");
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.log");
    let b = dir.join("b.csv");
    std::fs::write(&a, text(2000, "a")).unwrap();
    std::fs::write(&b, text(3000, "b")).unwrap();
    let db = Db::open_in_memory().unwrap();

    let mut m = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier1, "shrink");
    m.steps.push(rewrite_step(&a));
    m.steps.push(rewrite_step(&b));
    let problems = txn::validate(&adapter, std::slice::from_ref(&dir), &mut m).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    let diff = m.diff();
    assert!(
        diff.contains("SHRINK") && diff.contains("apfs") && diff.contains("→ ~"),
        "{diff}"
    );
    assert!(m.steps.iter().all(|s| s.hash_before().is_some()));

    let rep = txn::execute(&adapter, &db, &mut m, CrashPoint::Never).unwrap();
    assert_eq!((rep.done, rep.failed), (2, 0));
    // verified and recorded
    let (loaded, state, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
    assert_eq!(state, TxnState::Done);
    assert!(steps.iter().all(|s| *s == StepState::Done));
    for s in &loaded.steps {
        let Step::Rewrite {
            hash_before,
            hash_after_decoded,
            bytes_before,
            bytes_after,
            ..
        } = s
        else {
            panic!()
        };
        assert_eq!(hash_before, hash_after_decoded);
        assert!(*bytes_before > 0 && bytes_after.unwrap() < *bytes_before);
    }
    assert_eq!(adapter.state_of(&a), RewriteState::Rewritten);
    assert!(loaded.diff().contains("→ "), "{}", loaded.diff());
    assert!(
        !loaded.diff().contains("→ ~"),
        "real sizes after execution:\n{}",
        loaded.diff()
    );
    // contents untouched
    assert_eq!(std::fs::read(&a).unwrap(), text(2000, "a"));

    // a second plan for the same file is refused
    let mut again = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier1, "again");
    again.steps.push(rewrite_step(&a));
    let problems = txn::validate(&adapter, std::slice::from_ref(&dir), &mut again).unwrap();
    assert!(
        problems.iter().any(|p| p.contains("already rewritten")),
        "{problems:?}"
    );

    // edit b after the rewrite → undo leaves it alone, restores a
    std::fs::write(&b, b"edited").unwrap();
    let u = txn::undo(&adapter, &db, &m.txn_id).unwrap();
    assert_eq!(u.restored, 1);
    assert_eq!(u.skipped.len(), 1, "{:?}", u.skipped);
    assert!(u.skipped[0].contains("modified after the rewrite"));
    assert_eq!(adapter.state_of(&a), RewriteState::Original);
    assert_eq!(adapter.state_of(&b), RewriteState::Rewritten);
    let (_, _, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
    assert_eq!(steps, vec![StepState::Undone, StepState::Done]);
}

#[test]
fn rewrite_that_does_not_round_trip_is_restored_and_fails_the_step() {
    /// An adapter whose "compression" corrupts the file.
    struct Evil(SimAdapter);
    impl OsAdapter for Evil {
        fn platform(&self) -> &'static str {
            "evil"
        }
        fn watch(&self, r: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
            self.0.watch(r, tx)
        }
        fn enumerate(
            &self,
            root: &Path,
            opts: &ScanOptsNative,
        ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>> {
            self.0.enumerate(root, opts)
        }
        fn stat(&self, p: &Path) -> Result<Option<Entry>> {
            self.0.stat(p)
        }
        fn native_metadata(&self, p: &Path) -> Result<NativeMeta> {
            self.0.native_metadata(p)
        }
        fn native_search(&self, q: &str) -> Result<Vec<PathBuf>> {
            self.0.native_search(q)
        }
        fn move_to_trash(&self, p: &Path) -> Result<TrashReceipt> {
            self.0.move_to_trash(p)
        }
        fn trash_target(&self, p: &Path) -> Result<PathBuf> {
            self.0.trash_target(p)
        }
        fn move_to_trash_at(&self, p: &Path, t: &Path) -> Result<TrashReceipt> {
            self.0.move_to_trash_at(p, t)
        }
        fn rename_no_clobber(&self, f: &Path, t: &Path) -> Result<()> {
            self.0.rename_no_clobber(f, t)
        }
        fn protected_roots(&self) -> Vec<PathBuf> {
            self.0.protected_roots()
        }
        fn register_autostart(&self, e: bool) -> Result<()> {
            self.0.register_autostart(e)
        }
        fn link_kind(&self, p: &Path) -> Result<LinkKind> {
            self.0.link_kind(p)
        }
        fn default_roots(&self) -> Vec<PathBuf> {
            self.0.default_roots()
        }
        fn rewrite(&self, p: &Path, m: &str) -> Result<RewriteReceipt> {
            let r = self.0.rewrite(p, m)?;
            std::fs::write(p, b"garbage").unwrap();
            Ok(r)
        }
        fn rewrite_restore(&self, p: &Path, m: &str) -> Result<()> {
            self.0.rewrite_restore(p, m)
        }
        fn rewrite_state(&self, p: &Path, m: &str) -> Result<RewriteState> {
            self.0.rewrite_state(p, m)
        }
    }
    let adapter = Evil(SimAdapter::default());
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("root");
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.log");
    std::fs::write(&a, text(500, "a")).unwrap();
    let db = Db::open_in_memory().unwrap();
    let mut m = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier1, "shrink");
    m.steps.push(rewrite_step(&a));
    assert!(txn::validate(&adapter, std::slice::from_ref(&dir), &mut m)
        .unwrap()
        .is_empty());
    let rep = txn::execute(&adapter, &db, &mut m, CrashPoint::Never).unwrap();
    assert_eq!((rep.done, rep.failed), (0, 1));
    let (_, state, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
    assert_eq!(state, TxnState::Failed);
    assert_eq!(steps, vec![StepState::Failed]);
    assert_eq!(
        adapter.0.state_of(&a),
        RewriteState::Original,
        "restored on the spot"
    );
}

#[test]
fn crash_at_every_point_settles_without_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let n = 4;
    let mut points = vec![CrashPoint::Never];
    for i in 0..n {
        points.push(CrashPoint::BeforeOp(i));
        points.push(CrashPoint::AfterOp(i));
    }
    let mut run = 0;
    for half_done in [false, true] {
        for &cp in &points {
            run += 1;
            let adapter = SimAdapter::default();
            let dir = tmp.path().join(format!("run{run}"));
            std::fs::create_dir_all(dir.join("src")).unwrap();
            let files: Vec<(PathBuf, Vec<u8>)> = (0..n)
                .map(|i| {
                    let p = dir.join(format!("src/f{i}.txt"));
                    let body = text(100 + i * 50, &format!("{run}-{i}"));
                    std::fs::write(&p, &body).unwrap();
                    (p, body)
                })
                .collect();
            let db = Db::open(&dir.join("j.db")).unwrap();
            let mut m = Manifest::new(Mode::Assist, Initiator::User, RiskTier::Tier1, "chaos");
            for (i, (p, _)) in files.iter().enumerate() {
                if i == 1 {
                    m.steps.push(Step::Move {
                        from: p.clone(),
                        to: dir.join("dst/moved.txt"),
                        hash_before: None,
                    });
                } else {
                    m.steps.push(rewrite_step(p));
                }
            }
            assert!(txn::validate(&adapter, std::slice::from_ref(&dir), &mut m)
                .unwrap()
                .is_empty());
            if half_done {
                // the crash, when it comes, hits between "payload written" and "flag set"
                if let CrashPoint::AfterOp(i) = cp {
                    if let Step::Rewrite { path, .. } = &m.steps[i] {
                        *adapter.half_done_at.lock().unwrap() = Some(path.clone());
                    }
                }
            }
            let res = txn::execute(&adapter, &db, &mut m, cp);
            if cp == CrashPoint::Never {
                assert_eq!(res.unwrap().done, n);
            } else {
                assert!(res.is_err());
                let rep = txn::recover(&adapter, &db, &m.txn_id).unwrap();
                assert_eq!(rep.conflicts, 0, "run {run} {cp:?}");
                let (_, _, steps) = db.load_txn(&m.txn_id).unwrap().unwrap();
                assert!(steps.iter().all(|s| *s != StepState::Running), "{steps:?}");
                // a settled rewrite is really on (no HalfDone survives recovery)
                for (i, s) in steps.iter().enumerate() {
                    if let Step::Rewrite { path, .. } = &m.steps[i] {
                        match s {
                            StepState::Done => {
                                assert_eq!(adapter.state_of(path), RewriteState::Rewritten)
                            }
                            StepState::Planned => {
                                assert_eq!(adapter.state_of(path), RewriteState::Original)
                            }
                            other => panic!("{other:?}"),
                        }
                    }
                }
            }
            assert!(db.unfinished().unwrap().is_empty());
            let u = txn::undo(&adapter, &db, &m.txn_id).unwrap();
            assert!(u.skipped.is_empty(), "run {run} {cp:?}: {:?}", u.skipped);
            for (p, body) in &files {
                assert_eq!(adapter.state_of(p), RewriteState::Original);
                assert_eq!(std::fs::read(p).unwrap(), *body, "{}", p.display());
            }
        }
    }
}

#[test]
fn compress_suggestion_flows_from_estimate_to_undo() {
    std::env::set_var("FILEMIND_SHRINK_ANYWHERE", "1");
    let adapter = SimAdapter::default();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    let root = home.join("Documents");
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let mut paths = Vec::new();
    for i in 0..8 {
        let p = root.join(format!("logs/day{i}.log"));
        std::fs::write(&p, text(20_000, &format!("day {i}"))).unwrap(); // ~1 MB each
        old(&p, 90);
        paths.push(p);
    }
    let fresh = root.join("logs/today.log");
    std::fs::write(&fresh, text(20_000, "today")).unwrap();
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
    filemind_agent::classifier::classify_pending(
        &db,
        filemind_agent::classifier::ClassifyOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();

    // no estimate yet → no compress suggestion ("measure first")
    db.refresh_suggestions(&[], &[]).unwrap();
    assert!(db
        .list_suggestions("proposed", usize::MAX)
        .unwrap()
        .iter()
        .all(|s| s.kind != "compress_cold_text"));

    let est = shrink::run_estimate(&db, |_, _| {}).unwrap();
    let t1 = est.tiers.iter().find(|t| t.kind == "apfs").unwrap();
    assert_eq!(t1.candidate_files, 8, "today.log is too fresh");
    assert!(t1.ratio < 0.2);

    db.refresh_suggestions(&[], &[]).unwrap();
    let s = db
        .list_suggestions("proposed", usize::MAX)
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "compress_cold_text")
        .expect("one batch per (root, bucket)");
    assert_eq!(s.risk_tier, 1);
    assert_eq!(s.subject["files"].as_array().unwrap().len(), 8);
    assert!(s.est_bytes > 6_000_000, "{}", s.est_bytes);
    assert!(s.rationale.contains("APFS"), "{}", s.rationale);

    let (_, plan) = actions::plan(&adapter, &db, s.id).unwrap();
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    assert_eq!(plan.steps, 8);
    assert!(plan.diff.contains("SHRINK"), "{}", plan.diff);
    let applied = actions::apply(
        &adapter,
        &db,
        s.id,
        true,
        Some((&plan.txn_id, &plan.fingerprint)),
    )
    .unwrap();
    assert_eq!((applied.done, applied.failed), (8, 0));
    for p in &paths {
        assert_eq!(adapter.state_of(p), RewriteState::Rewritten);
    }
    // the inventory knows, so the next analysis does not propose them again
    let n: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE rewrite = 'apfs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 8);
    shrink::run_estimate(&db, |_, _| {}).unwrap();
    db.refresh_suggestions(&[], &[]).unwrap();
    assert!(db
        .list_suggestions("proposed", usize::MAX)
        .unwrap()
        .iter()
        .all(|s| s.kind != "compress_cold_text"));

    let undone = actions::undo(&adapter, &db, &plan.txn_id).unwrap();
    assert_eq!(undone.restored, 8);
    let n: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE rewrite IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
    for p in &paths {
        assert_eq!(adapter.state_of(p), RewriteState::Original);
    }
    std::env::remove_var("FILEMIND_SHRINK_ANYWHERE");
}
