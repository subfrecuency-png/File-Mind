//! Vault V0: Seal → trash → list → metadata search → Unseal → hash → undo,
//! plus mid-seal crash safety. FAKE fixtures only.
#![cfg(unix)]

use filemind_agent::classifier::{classify_pending, ClassifyOpts};
use filemind_agent::pipeline;
use filemind_agent::vault;
use filemind_core::txn::{CrashPoint, Journal};
use filemind_storage::Db;
use std::path::Path;
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/secrets")
}

fn setup(tmp: &Path) -> (filemind_adapter_macos::MacAdapter, std::path::PathBuf) {
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    std::env::set_var("FILEMIND_VAULT_MK", "ab".repeat(32));
    std::env::set_var("FILEMIND_VAULT_DIR", tmp.join("vault-objects"));
    (filemind_adapter_macos::MacAdapter, home)
}

#[test]
fn seal_unseal_search_undo_fake_pem() {
    let _lock = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (adapter, home) = setup(tmp.path());
    let root = home.join("Desktop");
    std::fs::create_dir_all(&root).unwrap();
    let src = fixtures().join("positives/server.pem");
    let pem = root.join("server.pem");
    std::fs::copy(&src, &pem).unwrap();
    let original = std::fs::read(&pem).unwrap();
    let want_hash = blake3::hash(&original).to_hex().to_string();
    let root = root.canonicalize().unwrap();
    let pem = root.join("server.pem");

    let db = Db::open_in_memory().unwrap();
    db.add_root(&root).unwrap();
    db.set_setting("mode", &serde_json::json!("observe"))
        .unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    classify_pending(
        &db,
        ClassifyOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();

    let err = vault::seal(&adapter, &db, &pem, true, CrashPoint::Never)
        .unwrap_err()
        .to_string();
    assert!(err.contains("observe"), "{err}");
    assert!(pem.exists(), "observe must not seal");

    db.set_setting("mode", &serde_json::json!("assist"))
        .unwrap();
    let err = vault::seal(&adapter, &db, &pem, false, CrashPoint::Never)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("OK") || err.contains("approval") || err.contains("need"),
        "{err}"
    );

    let sealed = vault::seal(&adapter, &db, &pem, true, CrashPoint::Never).unwrap();
    assert_eq!(sealed.failed, 0);
    assert!(!pem.exists(), "plaintext must go to Trash");
    let obj = tmp
        .path()
        .join("vault-objects")
        .join(format!("{}.fmseal", sealed.seal_id));
    assert!(obj.exists(), "ciphertext {}", obj.display());
    assert_eq!(sealed.plaintext_blake3, want_hash);

    let listed = vault::list(&db).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].seal_id, sealed.seal_id);
    assert_eq!(listed[0].sensitivity, "credential");

    // metadata search finds the name; body tokens from the PEM must not
    let hits = db.search_lexical("server", 10).unwrap();
    assert!(
        hits.iter()
            .any(|(p, st)| p.ends_with("server.pem") && st == "sealed"),
        "metadata hit: {hits:?}"
    );
    let body = db.search_lexical("FILEMIND_TEST_FIXTURE", 10).unwrap();
    assert!(body.is_empty(), "no FTS body while sealed: {body:?}");

    let unsealed = vault::unseal(
        &adapter,
        &db,
        &sealed.seal_id,
        None,
        true,
        CrashPoint::Never,
    )
    .unwrap();
    assert_eq!(unsealed.failed, 0);
    assert!(pem.exists());
    let got = std::fs::read(&pem).unwrap();
    assert_eq!(got, original);
    assert_eq!(blake3::hash(&got).to_hex().to_string(), want_hash);

    let undone = filemind_agent::actions::undo(&adapter, &db, &unsealed.txn_id).unwrap();
    assert_eq!(undone.restored, 1);
    assert!(!pem.exists(), "undo Unseal puts plaintext back in Trash");
    assert_eq!(vault::list(&db).unwrap().len(), 1);
}

#[test]
fn mid_seal_crash_pauses_without_orphan() {
    let _lock = ENV.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (adapter, home) = setup(tmp.path());
    let root = home.join("Desktop");
    std::fs::create_dir_all(&root).unwrap();
    let pem = root.join("id_ed25519");
    std::fs::copy(fixtures().join("positives/id_ed25519"), &pem).unwrap();
    let root = root.canonicalize().unwrap();
    let pem = root.join("id_ed25519");

    let db = Db::open_in_memory().unwrap();
    db.add_root(&root).unwrap();
    db.set_setting("mode", &serde_json::json!("assist"))
        .unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();

    let err = vault::seal(&adapter, &db, &pem, true, CrashPoint::AfterSealWrite(0))
        .unwrap_err()
        .to_string();
    assert!(err.contains("simulated crash"), "{err}");
    assert!(pem.exists(), "plaintext still in place");

    let ids = Journal::unfinished(&db).unwrap();
    assert!(!ids.is_empty(), "txn must stay unfinished");
    let (m, _, _) = db.load_txn(&ids[0]).unwrap().expect("manifest");
    let obj = match &m.steps[0] {
        filemind_core::txn::Step::Seal { object_path, .. } => object_path.clone(),
        other => panic!("expected Seal step, got {other:?}"),
    };
    assert!(obj.exists(), "ciphertext was written: {}", obj.display());

    let recovered = filemind_agent::actions::recover_all(&adapter, &db).unwrap();
    assert!(
        recovered
            .iter()
            .any(|(_, st)| matches!(st, filemind_core::txn::TxnState::Failed)),
        "paused txn: {recovered:?}"
    );
    assert!(pem.exists(), "recovery must not trash plaintext");
    assert!(
        obj.exists(),
        "ciphertext remains; txn is paused so this is not a silent orphan"
    );
}

#[test]
fn pub_near_miss_is_not_a_candidate() {
    let pubk = fixtures().join("near_misses/id_ed25519.pub");
    assert!(
        filemind_core::classify::vault_tier0_file(&pubk).is_none(),
        "id_ed25519.pub must not seal"
    );
    assert!(filemind_core::classify::sensitive_by_name(&pubk).is_none());
}
