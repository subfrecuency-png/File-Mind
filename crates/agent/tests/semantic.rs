//! Hybrid search end to end: index a small corpus, then ask for things the
//! way a person would. The hash embedder runs everywhere; the ONNX model
//! benchmark runs when `FILEMIND_MODEL_DIR` points at the downloaded model.
#![cfg(unix)]

use filemind_agent::classifier::{self, ClassifyOpts};
use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::semantic::{self, EmbedOpts, Engine, Mode};
use filemind_ai::embed::{Embedder, HashEmbedder};
use filemind_storage::Db;
use std::path::{Path, PathBuf};

/// (relative path, content, mtime as YYYY-MM-DD)
fn corpus() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Downloads/OFFER SHEET Calcium.pdf.txt", "Offer sheet: calcium carbonate, food grade. Price per metric ton FOB, minimum order 20 t, delivery 4 weeks. Supplier: Nordkalk trading.", "2026-04-12"),
        ("Documents/Finance/Q3 budget 2025.csv", "department,budget,actual\nmarketing,120000,98000\nengineering,400000,410000\nsales,90000,87000", "2025-10-02"),
        ("Documents/Finance/invoice_acme_2026-02.txt", "INVOICE #2026-018 from Acme Hosting. Amount due: $1,240.00. Due date 2026-03-01. Services: cloud servers February.", "2026-02-03"),
        ("Documents/Legal/NDA Northwind.txt", "MUTUAL NON-DISCLOSURE AGREEMENT between Northwind Traders and the Company. Confidential information shall not be disclosed for a period of three years.", "2025-06-20"),
        ("Documents/Legal/lease apartment 2024.txt", "RESIDENTIAL LEASE AGREEMENT. Tenant agrees to pay monthly rent of $2,100 for the apartment at 14 Harbor St. Term: 12 months beginning 2024-09-01.", "2024-08-15"),
        ("Documents/Travel/Bali itinerary.txt", "Bali trip itinerary: arrive Denpasar Aug 8, villa in Ubud, snorkeling Nusa Penida Aug 11, flight home Aug 15. Luxe week group.", "2025-07-30"),
        ("Documents/Travel/flight receipt LAX-DPS.txt", "E-ticket receipt. Passenger: R. Rodriguez. LAX to DPS via TPE, total paid $1,380. Booking ref X7KQ2.", "2025-07-02"),
        ("Documents/Health/blood test results march.txt", "Lab report: complete blood count and vitamin D. Vitamin D 24 ng/mL (low). Recommend supplementation and retest in 3 months.", "2026-03-18"),
        ("Documents/Recipes/grandma lasagna.txt", "Lasagna recipe: layers of pasta, beef ragu, bechamel, parmesan. Bake at 180C for 45 minutes. Serves 8.", "2023-12-24"),
        ("Documents/Recipes/sourdough starter notes.txt", "Sourdough starter: feed 1:1:1 flour water starter every 12 hours, keep at 24C. Ready when it doubles in 4-6 hours.", "2024-02-10"),
        ("Projects/creditos/package.json", "{\"name\": \"creditos\", \"scripts\": {\"dev\": \"next dev\"}}", "2026-05-01"),
        ("Projects/creditos/README.md", "# Creditos\nNext.js app for micro-credit scoring. Run `npm run dev`. Uses Supabase for auth and Postgres.", "2026-05-05"),
        ("Projects/creditos/src/scoring.ts", "export function score(applicant: Applicant): number { const income = applicant.income; const debt = applicant.debt; return Math.max(0, 700 - debt / income * 100); }", "2026-05-06"),
        ("Projects/creditos/docs/pitch deck notes.txt", "Pitch: Creditos gives small merchants in Latin America credit scores from sales data. Ask: $500k seed. Team of 3.", "2026-04-28"),
        ("Projects/filemind/docs/ARCHITECTURE.md", "# FileMind architecture\nRust core, OS adapters, SQLite with FTS5, transaction manager with undo, semantic search with bge-small.", "2026-08-20"),
        ("Projects/lightsaber/shots/LS_Plane_Title6_v008.txt", "After Effects render notes: title plane shot v008, 24fps, 1920x1080, lens flare pass, needs color grade.", "2025-11-11"),
        ("Pictures/Screenshots/Screenshot 2026-08-01 at 10.14.32.png.txt", "screenshot", "2026-08-01"),
        ("Pictures/2025 Bali/IMG_4021.jpg.txt", "photo", "2025-08-10"),
        ("Downloads/react-hooks-cheatsheet.txt", "React hooks cheat sheet: useState, useEffect, useMemo, useCallback, custom hooks, rules of hooks.", "2025-03-03"),
        ("Downloads/tax return 2024 draft.txt", "Form 1040 draft, tax year 2024. Adjusted gross income, itemized deductions, estimated refund $1,900. File before April 15.", "2025-04-01"),
        ("Downloads/car insurance policy renewal.txt", "Auto insurance policy renewal. Premium $1,120 per year, collision deductible $500, effective 2026-06-01. Policy 88213.", "2026-05-20"),
        ("Downloads/gym membership contract.txt", "Fitness club membership agreement, 12 month term, $49/month, cancellation requires 30 days notice.", "2026-01-09"),
        ("Documents/Work/performance review 2025.txt", "Annual performance review. Strengths: shipping, mentoring. Goals for 2026: lead the platform migration, present at two conferences.", "2025-12-15"),
        ("Documents/Work/resume 2026.txt", "Ryan Rodriguez - software engineer. Rust, TypeScript, distributed systems. Experience: FileMind (founder), Creditos (lead).", "2026-07-14"),
        ("Documents/Work/meeting notes platform migration.txt", "Migration kickoff: move services from Heroku to Kubernetes by Q4. Risks: database cutover, secrets management. Owner: Ryan.", "2026-06-18"),
        ("Documents/Home/wifi router manual.txt", "Router quick start: connect WAN port, default gateway 192.168.1.1, admin password on the sticker, enable WPA3.", "2024-05-05"),
        ("Documents/Home/dishwasher warranty.txt", "Warranty certificate, Bosch dishwasher model SMS4, 2 years from purchase 2025-02-02. Keep receipt.", "2025-02-02"),
        ("Documents/Kids/school enrollment form.txt", "Elementary school enrollment 2026-2027. Documents needed: birth certificate, proof of address, immunization record.", "2026-03-02"),
        ("Documents/Music/setlist summer show.txt", "Setlist: 1. Open Road 2. Harbor Lights 3. Calcium Moon 4. Encore: Late Train. Soundcheck 5pm.", "2025-07-19"),
        ("Documents/Writing/novel chapter 3 draft.txt", "Chapter 3. The lighthouse keeper had not spoken in eleven years, and the storm did not change that.", "2026-02-22"),
        ("Documents/Writing/blog post rust async.txt", "Why async Rust felt hard and what fixed it for me: pinning, Send bounds, and picking one runtime.", "2025-09-09"),
    ]
}

/// (query, expected relative path substring)
fn queries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "pricing quote from the calcium vendor",
            "OFFER SHEET Calcium",
        ),
        ("that offer sheet from last spring", "OFFER SHEET Calcium"),
        ("how much did we spend on marketing", "Q3 budget"),
        ("hosting bill from february", "invoice_acme"),
        ("acme invoice", "invoice_acme"),
        ("confidentiality agreement with northwind", "NDA Northwind"),
        ("what is my rent", "lease apartment"),
        ("apartment lease", "lease apartment"),
        ("bali trip plan", "Bali itinerary"),
        ("plane ticket to indonesia", "flight receipt"),
        ("vitamin d lab results", "blood test"),
        ("lasagna", "lasagna"),
        ("how to feed the sourdough", "sourdough"),
        ("credit scoring app readme", "creditos/README"),
        ("scoring function typescript", "scoring.ts"),
        ("seed round pitch for creditos", "pitch deck"),
        ("filemind architecture doc", "ARCHITECTURE"),
        ("after effects render notes title shot", "LS_Plane_Title6"),
        ("react hooks reference", "react-hooks"),
        ("1040 draft", "tax return"),
        ("car insurance renewal premium", "car insurance"),
        ("gym contract cancellation", "gym membership"),
        ("my annual review", "performance review"),
        ("resume", "resume 2026"),
        (
            "kubernetes migration kickoff notes",
            "meeting notes platform",
        ),
        ("router admin password", "wifi router"),
        ("dishwasher warranty", "dishwasher"),
        ("school enrollment documents", "school enrollment"),
        ("setlist for the summer concert", "setlist"),
        ("lighthouse keeper chapter", "novel chapter"),
        ("blog draft about async rust", "blog post rust"),
        ("what did I need for the kids school", "school enrollment"),
        ("supplier terms delivery weeks", "OFFER SHEET Calcium"),
        ("heroku to kubernetes", "meeting notes platform"),
        ("mutual nda", "NDA Northwind"),
        ("lab report", "blood test"),
        ("itemized deductions refund", "tax return"),
        ("engineering budget actual", "Q3 budget"),
        ("collision deductible", "car insurance"),
        ("open road harbor lights", "setlist"),
    ]
}

fn build(root: &Path) {
    for (rel, body, date) in corpus() {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let t = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let ft = filetime::FileTime::from_unix_time(t, 0);
        filetime::set_file_mtime(&p, ft).unwrap();
    }
}

fn index(root: &Path, embedder: Box<dyn Embedder>) -> (Db, Engine) {
    let adapter = filemind_adapter_macos::MacAdapter;
    let db = Db::open_in_memory().unwrap();
    db.add_root(root).unwrap();
    pipeline::scan_root(&adapter, &db, root).unwrap();
    pipeline::hash_pending(
        &db,
        HashOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    classifier::classify_pending(
        &db,
        ClassifyOpts {
            duty_cycle: 1.0,
            max_wall: None,
            names_only: false,
        },
    )
    .unwrap();
    let engine = Engine::with_embedder(&db, embedder).unwrap();
    let o = semantic::embed_pending(
        &db,
        &engine,
        EmbedOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(o.remaining, 0, "{o:?}");
    (db, engine)
}

fn hit_rate(db: &Db, engine: &Engine, k: usize) -> (f64, Vec<String>) {
    let qs = queries();
    let mut hits = 0;
    let mut misses = Vec::new();
    for (q, want) in &qs {
        let r = semantic::search(db, engine, q, k, Mode::Hybrid).unwrap();
        if r.hits
            .iter()
            .any(|h| h.card.path.to_string_lossy().contains(want))
        {
            hits += 1;
        } else {
            misses.push(format!(
                "{q:?} → {:?}",
                r.hits
                    .iter()
                    .map(|h| h.card.name.clone())
                    .collect::<Vec<_>>()
            ));
        }
    }
    (hits as f64 / qs.len() as f64, misses)
}

#[test]
fn hybrid_search_filters_notes_and_hash_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    build(&root);
    let root = root.canonicalize().unwrap();
    let (db, engine) = index(&root, Box::new(HashEmbedder::new(384)));
    assert!(!engine.is_semantic());

    // words + filters
    let r = semantic::search(&db, &engine, "lasagna", 5, Mode::Hybrid).unwrap();
    assert!(r.hits[0].card.name.contains("lasagna"), "{:?}", r.hits);

    let r = semantic::search(&db, &engine, "invoice in documents", 10, Mode::Hybrid).unwrap();
    assert!(r
        .hits
        .iter()
        .all(|h| h.card.path.to_string_lossy().contains("/documents/")
            || h.card.path.to_string_lossy().contains("/Documents/")));
    assert!(r.hits.iter().any(|h| h.card.name.contains("invoice_acme")));

    // structured-only query: everything from spring 2026 (no residual text)
    let r = semantic::search(&db, &engine, "files from spring 2026", 50, Mode::Hybrid).unwrap();
    assert!(r.parsed.text.is_empty());
    assert!(r.hits.iter().any(|h| h.card.name.contains("Calcium")));
    assert!(r.hits.iter().all(
        |h| h.card.mtime >= r.parsed.after.unwrap() && h.card.mtime < r.parsed.before.unwrap()
    ));

    // project scoping
    let dups = db.rebuild_duplicates().unwrap();
    let chains = db.rebuild_versions().unwrap();
    db.refresh_suggestions(&dups, &chains).unwrap();
    filemind_agent::analysis::run(&db).unwrap();
    let r = semantic::search(&db, &engine, "readme in project creditos", 10, Mode::Hybrid).unwrap();
    assert!(!r.hits.is_empty());
    assert!(
        r.hits
            .iter()
            .all(|h| h.card.path.to_string_lossy().contains("creditos")),
        "{:?}",
        r.hits
    );

    // memory notes are searchable and attach to a path
    let p = root.join("Downloads/OFFER SHEET Calcium.pdf.txt");
    db.add_note(
        "file",
        &p.to_string_lossy(),
        "this is the one we accepted; Nordkalk, signed in May",
        "user",
    )
    .unwrap();
    semantic::embed_pending(
        &db,
        &engine,
        EmbedOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let r = semantic::search(&db, &engine, "nordkalk signed", 5, Mode::Hybrid).unwrap();
    assert_eq!(r.notes.len(), 1);
    assert!(r.notes[0].text.contains("Nordkalk"));
    assert_eq!(
        db.list_notes(Some(&p.to_string_lossy()), 10).unwrap().len(),
        1
    );

    // the hash fallback is still decent on this corpus (words overlap)
    let (rate, misses) = hit_rate(&db, &engine, 5);
    assert!(
        rate >= 0.7,
        "hash fallback top-5 {rate:.2}\n{}",
        misses.join("\n")
    );

    // sensitive files are never embedded
    let sp = root.join("Documents/secrets.env");
    std::fs::write(
        &sp,
        "AWS_SECRET_ACCESS_KEY=abcd1234efgh5678ijkl9012mnop3456qrst7890",
    )
    .unwrap();
    pipeline::scan_root(&filemind_adapter_macos::MacAdapter, &db, &root).unwrap();
    classifier::classify_pending(
        &db,
        ClassifyOpts {
            duty_cycle: 1.0,
            max_wall: None,
            names_only: false,
        },
    )
    .unwrap();
    semantic::embed_pending(
        &db,
        &engine,
        EmbedOpts {
            duty_cycle: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let id = db.file_id_of_path(&sp).unwrap().unwrap();
    assert!(
        db.embedding_input_hash(&id).unwrap().is_none(),
        "sensitive file must not be embedded"
    );
}

/// `FILEMIND_MODEL_DIR=… cargo test -p filemind-agent --test semantic -- --ignored`
#[test]
#[ignore]
fn onnx_benchmark_top5_hit_rate() {
    let dir = PathBuf::from(std::env::var("FILEMIND_MODEL_DIR").expect("FILEMIND_MODEL_DIR"));
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("home");
    build(&root);
    let root = root.canonicalize().unwrap();
    let e =
        filemind_ai::embed::OnnxEmbedder::load_from(filemind_ai::embed::BGE_SMALL, &dir).unwrap();
    let (db, engine) = index(&root, Box::new(e));
    assert!(engine.is_semantic());
    let (rate5, misses) = hit_rate(&db, &engine, 5);
    let (rate1, _) = hit_rate(&db, &engine, 1);
    eprintln!(
        "top-1 {rate1:.2}  top-5 {rate5:.2}\nmisses:\n{}",
        misses.join("\n")
    );
    assert!(
        rate5 >= 0.8,
        "top-5 hit rate {rate5:.2}\n{}",
        misses.join("\n")
    );

    // latency: warm queries
    let t = std::time::Instant::now();
    let n = 20;
    for (q, _) in queries().iter().take(n) {
        semantic::search(&db, &engine, q, 10, Mode::Hybrid).unwrap();
    }
    eprintln!("avg query {:?}", t.elapsed() / n as u32);
}
