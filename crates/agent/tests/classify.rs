//! Classification eval on a labelled synthetic tree, content search, corrections → rules.
#![cfg(unix)]

use filemind_agent::classifier::{classify_pending, ClassifyOpts};
use filemind_agent::pipeline;
use filemind_core::model::Category;
use filemind_storage::Db;
use std::path::Path;

/// (relative path, contents, expected category). Contents are only read for text types.
fn eval_set() -> Vec<(&'static str, &'static str, Category)> {
    use Category::*;
    vec![
        ("Downloads/OFFER SHEET Calcium.pdf", "", Contract),
        ("Downloads/CA Offer Sheet (2025).docx", "", Contract),
        ("Downloads/Invoice_1042.pdf", "", Invoice),
        ("Downloads/amazon-receipt-march.pdf", "", Invoice),
        ("Documents/scan_0042.txt", "INVOICE #0042 Bill to: Acme Amount due: $400 Payment terms net 30 Total due", Invoice),
        ("Documents/agreement.txt", "This Agreement is made between the Parties. WHEREAS the parties hereby agree. Governing law: California. Signature:", Contract),
        ("Documents/notes.md", "# Meeting notes\n- talk about roadmap", Document),
        ("Documents/essay.txt", "Once upon a time there was a file organiser.", Document),
        ("Documents/README", "This project does things.", Document),
        ("Desktop/Screenshot 2026-08-29 at 1.02.03 PM.png", "", Screenshot),
        ("Desktop/CleanShot 2026-01-01.png", "", Screenshot),
        ("Pictures/IMG_4021.HEIC", "", Photo),
        ("Pictures/beach.jpg", "", Photo),
        ("Pictures/DSC_0001.NEF", "", Photo),
        ("Projects/site/src/main.rs", "fn main() {}", Code),
        ("Projects/site/package.json", "{}", Data),
        ("Projects/site/node_modules/x/.bin", "", Code),
        ("Projects/app/index.html", "<html></html>", Code),
        ("Downloads/app-1.2.dmg", "", Installer),
        ("Downloads/setup.exe", "", Installer),
        ("Downloads/archive.zip", "", Archive),
        ("Downloads/backup.tar.gz", "", Archive),
        ("Movies/clip.mp4", "", Media),
        ("Music/track.mp3", "", Media),
        ("Documents/data.csv", "a,b\n1,2", Data),
        ("Documents/export.xlsx", "", Data),
        ("Design/logo.ai", "", Design),
        ("Design/laser.lbrn2", "", Design),
        ("Design/mock.fig", "", Design),
        ("Documents/Contracts/2025 lease.pdf", "", Contract),
        ("Documents/Invoices/march.pdf", "", Invoice),
        ("Documents/mystery.xyz", "", Other),
        ("Documents/deck.pptx", "", Document),
        ("Downloads/1.png", "", Photo),
    ]
}

#[test]
fn rule_classifier_hits_target_and_search_finds_content() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    let set = eval_set();
    for (rel, body, _) in &set {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    // a secrets file and a private key that must be marked sensitive and not text-indexed
    std::fs::write(
        root.join("Projects/site/.env"),
        "API_KEY=sk-live-abcdefghijklmnopqrstuvwxyz123456\nDB_PASSWORD=hunter2",
    )
    .unwrap();
    std::fs::write(
        root.join("Documents/server.pem"),
        "-----BEGIN FAKE-RSA PRIVATE KEY-----\nMIIE...",
    )
    .unwrap();
    let root = root.canonicalize().unwrap();

    let db = Db::open_in_memory().unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    let o = classify_pending(
        &db,
        ClassifyOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(o.remaining, 0);
    assert!(o.sensitive >= 2, "sensitive {}", o.sensitive);

    let mut hits = 0;
    let mut misses = Vec::new();
    for (rel, _, want) in &set {
        let (c, _) = db.classification_of(&root.join(rel)).unwrap().expect(rel);
        if c.category == *want {
            hits += 1;
        } else {
            misses.push(format!(
                "{rel}: got {:?} want {:?} ({:?})",
                c.category, want, c.signals
            ));
        }
    }
    let acc = hits as f64 / set.len() as f64;
    eprintln!("top-1 accuracy {acc:.2}; misses: {misses:?}");
    assert!(
        acc >= 0.85,
        "top-1 accuracy {acc:.2}; misses:\n{}",
        misses.join("\n")
    );

    // content search: the invoice body is searchable, the secret is not
    let r = db.search_lexical("acme", 5).unwrap();
    assert!(r.iter().any(|(p, _)| p.ends_with("scan_0042.txt")), "{r:?}");
    let r = db.search_lexical("hunter2", 5).unwrap();
    assert!(r.is_empty(), "secrets must not be indexed: {r:?}");
    let (_, sens) = db
        .classification_of(&root.join("Projects/site/.env"))
        .unwrap()
        .unwrap();
    assert_eq!(sens.as_deref(), Some("secrets_file"));

    // correction → folder rule → reclassified on the next pass
    let mystery = root.join("Documents/mystery.xyz");
    db.add_rule(&filemind_core::classify::UserRule::PathPrefix {
        prefix: mystery.parent().unwrap().to_string_lossy().to_string(),
        category: Category::Contract,
    })
    .unwrap();
    assert!(
        db.count_classify_pending().unwrap() > 0,
        "rule must invalidate covered files"
    );
    classify_pending(
        &db,
        ClassifyOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let (c, _) = db.classification_of(&mystery).unwrap().unwrap();
    assert_eq!(c.category, Category::Contract);
    assert_eq!(c.source, filemind_core::model::ClassificationSource::User);

    // direct per-file correction wins in category counts
    assert!(db
        .set_user_category(&root.join("Pictures/beach.jpg"), Category::Design)
        .unwrap());
    let counts = db.category_counts().unwrap();
    let design = counts
        .iter()
        .find(|(c, ..)| c == "design")
        .map(|(_, n, _)| *n)
        .unwrap_or(0);
    assert!(design >= 4, "{counts:?}");
    let _ = Path::new("");
}
