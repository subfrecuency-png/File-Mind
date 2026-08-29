//! `filemind` CLI. Drives the agent library directly; Phase 2 moves the
//! heavy commands behind the daemon's JSON-RPC socket.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::platform;
use filemind_agent::rpc::Client;
use filemind_core::{Mode, ScanOpts, Scanner};
use filemind_storage::Db;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "filemind", version, about = "AI Memory for Your Computer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Database location, mode, roots and inventory counts.
    Status,
    /// Manage the folders FileMind is allowed to index.
    Roots {
        #[command(subcommand)]
        cmd: RootsCmd,
    },
    /// Read-only scan. Persists the inventory unless --dry-run. Never writes to the scanned folder.
    Scan {
        /// Folder to scan; omit to scan every registered root.
        root: Option<PathBuf>,
        /// Only print the report; do not touch the database.
        #[arg(long)]
        dry_run: bool,
    },
    /// Hash file contents that have not been hashed yet (throttled).
    Hash {
        /// Busy fraction of wall time, 0.05–1.0.
        #[arg(long, default_value_t = 0.2)]
        duty: f32,
        /// Stop after this many minutes.
        #[arg(long)]
        minutes: Option<u64>,
    },
    /// Lexical search over names and paths (semantic search arrives in Phase 7).
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Get or set the operating mode (observe | assist | automate).
    Mode {
        #[arg(value_parser = parse_mode)]
        value: Option<Mode>,
    },
    /// Show what FileMind and Spotlight know about one file.
    Info { path: PathBuf },
    /// Rebuild duplicate groups, version chains, health and suggestions now.
    Analyze,
    /// Health score with a breakdown of what is costing points.
    Health,
    /// Exact-duplicate groups (largest waste first).
    Dupes {
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Version chains (report_v1, report_v2, …) with the newest marked.
    Versions {
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Classify files and extract searchable text (throttled).
    Classify {
        #[command(subcommand)]
        cmd: Option<ClassifyCmd>,
        /// Busy fraction of wall time, 0.05–1.0.
        #[arg(long, default_value_t = 0.2)]
        duty: f32,
        /// Stop after this many minutes.
        #[arg(long)]
        minutes: Option<u64>,
        /// Names and extensions only, no text extraction (fast first pass).
        #[arg(long)]
        names_only: bool,
    },
    /// How many files of each category, sensitive files, pending, and your rules.
    Categories,
    /// Detected projects, most active first.
    Projects {
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// One project: its files, categories, dates. Or rename it.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Observe-mode suggestions. Nothing is ever executed from here.
    Suggest {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// proposed | dismissed | stale
        #[arg(long, default_value = "proposed")]
        state: String,
        #[command(subcommand)]
        cmd: Option<SuggestCmd>,
    },
    /// Transactions FileMind has run (newest first).
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Show one transaction's steps.
        id: Option<String>,
    },
    /// Reverse a transaction. Files edited after the move are left alone and reported.
    Undo { txn_id: String },
    /// Manage the background agent.
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Developer tools.
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

#[derive(Subcommand)]
enum RootsCmd {
    /// List registered roots.
    List,
    /// Register a folder (nothing is scanned until `filemind scan`).
    Add { path: PathBuf },
    /// Forget a folder and its index entries. Files on disk are untouched.
    Remove { path: PathBuf },
}

#[derive(Subcommand)]
enum ClassifyCmd {
    /// Show how one file was classified and why.
    Show { path: PathBuf },
    /// Correct a file's category. --scope folder|ext|name turns it into a rule.
    Set {
        path: PathBuf,
        /// document, invoice, contract, photo, screenshot, design, code, archive, installer, media, data, other
        category: String,
        /// file (default) | folder | ext | name
        #[arg(long, default_value = "file")]
        scope: String,
        /// For --scope name: the name fragment the rule should match.
        #[arg(long)]
        token: Option<String>,
    },
    /// Delete a rule by id (see `filemind categories`).
    Forget { rule_id: i64 },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Show a project's files (newest first) and category mix.
    Show {
        id: i64,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Give a project your own name (kept across re-analysis).
    Rename { id: i64, name: String },
    /// Which project does this file belong to?
    Of { path: PathBuf },
}

#[derive(Subcommand)]
enum SuggestCmd {
    /// Hide a suggestion; it stays hidden across re-analysis.
    Dismiss { id: i64 },
    /// Show exactly what applying a suggestion would do. Touches nothing.
    Plan { id: i64 },
    /// Apply a suggestion as a reversible transaction (needs assist mode and your approval).
    Apply {
        id: i64,
        /// Approve without the interactive prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// Register filemind-agent to start at login (launchd / Task Scheduler).
    Install,
    /// Unregister the agent.
    Uninstall,
    /// Run one scan + hash tick in the foreground.
    RunOnce,
    /// Run the agent in the foreground (watcher + scheduler + socket) until stopped.
    Start,
    /// Ask a running agent to stop.
    Stop,
    /// Is the agent running? Prints its uptime and watcher counters.
    Status,
}

#[derive(Subcommand)]
enum DevCmd {
    /// Build a synthetic file tree with duplicates, versions, links and an ignored folder.
    Fixture {
        dir: PathBuf,
        #[arg(long, default_value_t = 10_000)]
        entries: usize,
    },
}

fn parse_mode(s: &str) -> Result<Mode, String> {
    match s.to_ascii_lowercase().as_str() {
        "observe" => Ok(Mode::Observe),
        "assist" => Ok(Mode::Assist),
        "automate" => Ok(Mode::Automate),
        other => Err(format!(
            "unknown mode '{other}' (expected observe, assist or automate)"
        )),
    }
}

/// Connect to a running agent, if any. When it is running, commands go through
/// it so there is a single writer and the watcher stays consistent. A running
/// agent from an older build is refused with a clear message.
fn agent() -> Option<Client> {
    let path = filemind_storage::default_db_path().ok()?;
    let mut c = Client::connect(&path)?;
    match c.call("ping", json!({})) {
        Ok(v) => {
            let build = v.get("build").and_then(Value::as_str).unwrap_or("");
            if build != filemind_agent::BUILD_ID {
                eprintln!(
                    "note: the running agent is from an older build ({}); restart it with `filemind agent stop` then `filemind agent start`. Working on the database directly for now.",
                    if build.is_empty() { "unknown" } else { build }
                );
                return None;
            }
            Some(c)
        }
        Err(_) => None,
    }
}

fn print_scan(v: &Value) {
    let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    println!(
        "{}  {} files, {} folders, {}  in {:.1}s",
        v.get("path").and_then(Value::as_str).unwrap_or("?"),
        g("files"),
        g("dirs"),
        human_bytes(g("bytes")),
        g("elapsed_ms") as f64 / 1000.0
    );
    println!(
        "   new {}  modified {}  renamed {}  moved {}  missing {}  unchanged {}  links {}  ignored {}  errors {}",
        g("new"), g("modified"), g("renamed"), g("moved"), g("missing"), g("unchanged"), g("links"), g("ignored"), g("errors")
    );
}

fn print_plan(v: &Value) {
    println!(
        "plan {}  ({} step(s), risk tier {}, mode {})",
        v["txn_id"].as_str().unwrap_or("?"),
        v["steps"],
        v["risk_tier"],
        v["mode"].as_str().unwrap_or("?")
    );
    println!("{}", v["diff"].as_str().unwrap_or(""));
    for p in v["problems"].as_array().cloned().unwrap_or_default() {
        println!("  problem: {}", p.as_str().unwrap_or(""));
    }
}

fn open_db() -> Result<(PathBuf, Db)> {
    let path = filemind_storage::default_db_path()?;
    let db = Db::open(&path)?;
    Ok((path, db))
}

fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let adapter = platform::adapter();

    match cli.cmd {
        Cmd::Status => {
            if let Some(mut a) = agent() {
                let v = a.call("status", json!({}))?;
                let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
                println!("agent         running ({}s up)", g("uptime_s"));
                println!(
                    "platform      {}",
                    v.get("platform").and_then(Value::as_str).unwrap_or("?")
                );
                println!(
                    "database      {}",
                    v.get("database").and_then(Value::as_str).unwrap_or("?")
                );
                println!(
                    "mode          {}",
                    v.get("mode").and_then(Value::as_str).unwrap_or("observe")
                );
                println!(
                    "files         {}  ({} hashed, {} missing)",
                    g("files"),
                    g("hashed"),
                    g("missing")
                );
                println!("folders       {}", g("dirs"));
                println!("bytes         {}", human_bytes(g("bytes")));
                println!("events        {}", g("events"));
                println!("transactions  {}", g("transactions"));
                if let Some(w) = v.get("watcher") {
                    let w = |k: &str| w.get(k).and_then(Value::as_u64).unwrap_or(0);
                    println!(
                        "watcher       {} roots, {} raw events, {} changes applied",
                        w("roots"),
                        w("raw_events"),
                        w("changes_applied")
                    );
                }
                println!("roots:");
                for r in v
                    .get("roots")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    println!("  {}", r.get("path").and_then(Value::as_str).unwrap_or("?"));
                }
                return Ok(());
            }
            let (path, db) = open_db()?;
            println!(
                "agent         not running (`filemind agent install` or `filemind agent start`)"
            );
            let c = db.counts()?;
            println!("platform      {}", adapter.platform());
            println!("database      {}", path.display());
            println!(
                "mode          {}",
                db.get_setting("mode")?
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "observe".into())
            );
            println!(
                "files         {}  ({} hashed, {} missing)",
                c.files, c.hashed, c.missing
            );
            println!("folders       {}", c.dirs);
            println!("bytes         {}", human_bytes(c.bytes as u64));
            println!("events        {}", c.events);
            println!("transactions  {}", c.transactions);
            let roots = db.list_roots()?;
            if roots.is_empty() {
                println!("roots         none — add one with `filemind roots add <folder>`");
                println!("suggested:");
                for r in adapter.default_roots() {
                    println!("  {}", r.display());
                }
            } else {
                println!("roots:");
                for r in roots {
                    let last = r
                        .last_scan
                        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        })
                        .unwrap_or_else(|| "never scanned".into());
                    println!("  {}  ({last})", r.path.display());
                }
            }
        }

        Cmd::Roots { cmd } => {
            if let Some(mut a) = agent() {
                match cmd {
                    RootsCmd::List => {
                        for p in a
                            .call("roots.list", json!({}))?
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                        {
                            println!("{}", p.as_str().unwrap_or("?"));
                        }
                    }
                    RootsCmd::Add { path } => {
                        let v = a.call("roots.add", json!({"path": path}))?;
                        println!(
                            "added {}  (root #{})",
                            v["path"].as_str().unwrap_or("?"),
                            v["root_id"]
                        );
                    }
                    RootsCmd::Remove { path } => {
                        let v = a.call("roots.remove", json!({"path": path}))?;
                        if v["removed"].as_bool() == Some(true) {
                            println!("forgot {} (files on disk untouched)", path.display());
                        } else {
                            bail!("{} is not a registered root", path.display());
                        }
                    }
                }
                return Ok(());
            }
            let (_, db) = open_db()?;
            match cmd {
                RootsCmd::List => {
                    for r in db.list_roots()? {
                        println!("{}", r.path.display());
                    }
                }
                RootsCmd::Add { path } => {
                    let path = path.canonicalize()?;
                    let scanner = Scanner::new(adapter.as_ref(), vec![]);
                    scanner.validate_root(&path)?;
                    let r = db.add_root(&path)?;
                    println!("added {}  (root #{})", r.path.display(), r.root_id);
                }
                RootsCmd::Remove { path } => {
                    let path = path.canonicalize().unwrap_or(path);
                    if db.remove_root(&path)? {
                        println!("forgot {} (files on disk untouched)", path.display());
                    } else {
                        bail!("{} is not a registered root", path.display());
                    }
                }
            }
        }

        Cmd::Scan { root, dry_run } => {
            if dry_run {
                let Some(root) = root else {
                    bail!("--dry-run needs a folder")
                };
                let root = root.canonicalize()?;
                let scanner = Scanner::new(adapter.as_ref(), vec![root.clone()]);
                let started = std::time::Instant::now();
                let report = scanner.scan_root(&root, &ScanOpts::default(), |_| {})?;
                println!("scanned {} in {:.1?}", root.display(), started.elapsed());
                println!(
                    "files {}  dirs {}  bytes {}  links skipped {}  protected skipped {}  ignored {}  errors {}",
                    report.files,
                    report.dirs,
                    human_bytes(report.bytes),
                    report.links_skipped,
                    report.protected_skipped,
                    report.ignored,
                    report.errors
                );
                println!("(dry run: database untouched)");
            } else if let Some(mut a) = agent() {
                let v = a.call("scan", json!({"path": root}))?;
                for r in v.as_array().cloned().unwrap_or_default() {
                    print_scan(&r);
                }
            } else {
                let (_, db) = open_db()?;
                let targets: Vec<PathBuf> = match root {
                    Some(r) => vec![r.canonicalize()?],
                    None => db.list_roots()?.into_iter().map(|r| r.path).collect(),
                };
                if targets.is_empty() {
                    bail!("no roots registered — `filemind roots add <folder>` or pass a folder");
                }
                for t in targets {
                    let o = pipeline::scan_root(adapter.as_ref(), &db, &t)?;
                    println!(
                        "{}  {} files, {} folders, {}  in {:.1}s",
                        t.display(),
                        o.report.files,
                        o.report.dirs,
                        human_bytes(o.report.bytes),
                        o.elapsed_ms as f64 / 1000.0
                    );
                    println!(
                        "   new {}  modified {}  renamed {}  moved {}  missing {}  unchanged {}  links {}  ignored {}  errors {}",
                        o.upsert.inserted,
                        o.upsert.updated,
                        o.upsert.renamed,
                        o.upsert.moved,
                        o.missing,
                        o.upsert.unchanged,
                        o.report.links_skipped,
                        o.report.ignored,
                        o.report.errors
                    );
                }
            }
        }

        Cmd::Hash { duty, minutes } => {
            if let Some(mut a) = agent() {
                let v = a.call("hash", json!({"duty": duty, "minutes": minutes}))?;
                let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
                println!(
                    "hashed {} files ({}) in {:.1}s  errors {}  still pending {}",
                    g("hashed"),
                    human_bytes(g("bytes")),
                    g("elapsed_ms") as f64 / 1000.0,
                    g("errors"),
                    g("remaining")
                );
                return Ok(());
            }
            let (_, db) = open_db()?;
            let o = pipeline::hash_pending(
                &db,
                HashOpts {
                    duty_cycle: duty,
                    max_files: 0,
                    max_wall: minutes.map(|m| Duration::from_secs(m * 60)),
                },
            )?;
            println!(
                "hashed {} files ({}) in {:.1}s  errors {}  still pending {}",
                o.hashed,
                human_bytes(o.bytes),
                o.elapsed_ms as f64 / 1000.0,
                o.errors,
                o.remaining
            );
        }

        Cmd::Search { query, limit } => {
            if let Some(mut a) = agent() {
                for r in a
                    .call("search", json!({"query": query, "limit": limit}))?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    let p = r["path"].as_str().unwrap_or("?");
                    match r["status"].as_str() {
                        Some("present") | None => println!("{p}"),
                        Some(s) => println!("{p}  [{s}]"),
                    }
                }
                return Ok(());
            }
            let (_, db) = open_db()?;
            for (p, status) in db.search_lexical(&query, limit)? {
                if status == "present" {
                    println!("{}", p.display());
                } else {
                    println!("{}  [{status}]", p.display());
                }
            }
        }

        Cmd::Mode { value } => {
            if let Some(mut a) = agent() {
                let v = match value {
                    Some(m) => a.call("mode.set", json!({"mode": serde_json::to_value(m)?}))?,
                    None => a.call("mode.get", json!({}))?,
                };
                println!("{}", v.as_str().unwrap_or("observe"));
                return Ok(());
            }
            let (_, db) = open_db()?;
            if let Some(m) = value {
                db.set_setting("mode", &serde_json::to_value(m)?)?;
            }
            println!(
                "{}",
                db.get_setting("mode")?
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "observe".into())
            );
        }

        Cmd::Info { path } => {
            let path = path.canonicalize()?;
            let (indexed, meta) = if let Some(mut a) = agent() {
                let v = a.call("info", json!({"path": path}))?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            } else {
                let (_, db) = open_db()?;
                (db.file_at_path(&path)?, adapter.native_metadata(&path)?)
            };
            println!("path          {}", path.display());
            println!(
                "indexed       {}",
                indexed
                    .map(|(id, kind)| format!("yes ({kind}, id {id})"))
                    .unwrap_or("no".into())
            );
            println!(
                "content type  {}",
                meta.content_type.unwrap_or_else(|| "-".into())
            );
            if !meta.where_from.is_empty() {
                println!("downloaded    {}", meta.where_from.join(", "));
            }
            if !meta.tags.is_empty() {
                println!("tags          {}", meta.tags.join(", "));
            }
            for (k, v) in meta.extra {
                println!("{:<13} {}", k.trim_start_matches("kMDItem"), v);
            }
        }

        Cmd::Analyze => {
            let v = if let Some(mut a) = agent() {
                a.call("analyze", json!({}))?
            } else {
                let (_, db) = open_db()?;
                let a = filemind_agent::analysis::run(&db)?;
                json!({"duplicate_groups": a.duplicate_groups, "duplicate_bytes": a.duplicate_bytes,
                       "version_chains": a.version_chains, "suggestions": a.suggestions, "projects": a.projects,
                       "health": a.health, "elapsed_ms": a.elapsed_ms})
            };
            let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            println!(
                "health {}  duplicate groups {} ({} reclaimable)  version chains {}  suggestions {}  projects {}  in {:.1}s",
                v["health"]["score"],
                g("duplicate_groups"),
                human_bytes(g("duplicate_bytes")),
                g("version_chains"),
                g("suggestions"),
                g("projects"),
                g("elapsed_ms") as f64 / 1000.0
            );
        }

        Cmd::Health => {
            let v = if let Some(mut a) = agent() {
                a.call("health", json!({}))?
            } else {
                let (_, db) = open_db()?;
                let h = filemind_core::health::score(&db.health_inputs(None)?);
                let mut roots = Vec::new();
                for r in db.list_roots()? {
                    let rh = filemind_core::health::score(&db.health_inputs(Some(r.root_id))?);
                    roots.push(json!({"path": r.path, "score": rh.score}));
                }
                json!({"health": h, "roots": roots, "history": db.health_history(None, 30)?})
            };
            let h = &v["health"];
            println!("health score  {} / 100", h["score"]);
            println!();
            for c in h["components"].as_array().cloned().unwrap_or_default() {
                let pen = c["penalty"].as_f64().unwrap_or(0.0);
                let w = c["weight"].as_f64().unwrap_or(0.0);
                println!(
                    "  {:<16} -{:>4.1} of {:>2}   {}",
                    c["name"].as_str().unwrap_or("?"),
                    pen,
                    w,
                    c["detail"].as_str().unwrap_or("")
                );
            }
            let roots = v["roots"].as_array().cloned().unwrap_or_default();
            if roots.len() > 1 {
                println!();
                for r in roots {
                    println!("  {:>3}  {}", r["score"], r["path"].as_str().unwrap_or("?"));
                }
            }
            let hist: Vec<u64> = v["history"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|e| e.get(1).and_then(Value::as_u64))
                .collect();
            if hist.len() > 1 {
                let s: Vec<String> = hist.iter().rev().map(|x| x.to_string()).collect();
                println!("\nrecent scores  {}", s.join(" → "));
            }
        }

        Cmd::Dupes { limit } => {
            let v = if let Some(mut a) = agent() {
                a.call("dupes", json!({"limit": limit}))?
            } else {
                let (_, db) = open_db()?;
                let groups = db.rebuild_duplicates()?;
                let wasted: u64 = groups.iter().map(|g| g.size * g.copies.len() as u64).sum();
                json!({"groups": groups.len(), "wasted_bytes": wasted,
                       "top": groups.iter().take(limit).map(|g| json!({"size": g.size, "keep": g.keeper, "copies": g.copies})).collect::<Vec<_>>()})
            };
            println!(
                "{} duplicate groups, {} reclaimable",
                v["groups"],
                human_bytes(v["wasted_bytes"].as_u64().unwrap_or(0))
            );
            for g in v["top"].as_array().cloned().unwrap_or_default() {
                println!();
                println!(
                    "  keep   {}   ({})",
                    g["keep"].as_str().unwrap_or("?"),
                    human_bytes(g["size"].as_u64().unwrap_or(0))
                );
                for c in g["copies"].as_array().cloned().unwrap_or_default() {
                    println!("  copy   {}", c.as_str().unwrap_or("?"));
                }
            }
        }

        Cmd::Versions { limit } => {
            let v = if let Some(mut a) = agent() {
                a.call("versions", json!({"limit": limit}))?
            } else {
                let (_, db) = open_db()?;
                let chains = db.rebuild_versions()?;
                json!({"chains": chains.len(),
                       "top": chains.iter().take(limit).map(|c| json!({"keep": c.canonical, "older": c.older})).collect::<Vec<_>>()})
            };
            println!("{} version chains", v["chains"]);
            for c in v["top"].as_array().cloned().unwrap_or_default() {
                println!();
                println!("  newest {}", c["keep"].as_str().unwrap_or("?"));
                for o in c["older"].as_array().cloned().unwrap_or_default() {
                    println!("  older  {}", o.as_str().unwrap_or("?"));
                }
            }
        }

        Cmd::Suggest { limit, state, cmd } => match cmd {
            None => {
                let v = if let Some(mut a) = agent() {
                    a.call("suggest.list", json!({"limit": limit, "state": state}))?
                } else {
                    let (_, db) = open_db()?;
                    let (count, bytes) = db.suggestion_totals()?;
                    json!({"proposed": count, "est_bytes": bytes, "items": db.list_suggestions(&state, limit)?})
                };
                println!(
                        "{} proposed suggestions, about {} reclaimable. Mode is observe: nothing runs without you.",
                        v["proposed"],
                        human_bytes(v["est_bytes"].as_u64().unwrap_or(0))
                    );
                for s in v["items"].as_array().cloned().unwrap_or_default() {
                    println!();
                    println!(
                        "  #{:<5} {:<18} {:>9}   tier {}",
                        s["id"],
                        s["kind"].as_str().unwrap_or("?"),
                        human_bytes(s["est_bytes"].as_u64().unwrap_or(0)),
                        s["risk_tier"]
                    );
                    println!("         {}", s["rationale"].as_str().unwrap_or(""));
                }
                if v["items"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("  (none — run `filemind analyze` after hashing has finished)");
                }
            }
            Some(SuggestCmd::Plan { id }) => {
                let v = if let Some(mut a) = agent() {
                    a.call("suggest.plan", json!({"id": id}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(filemind_agent::actions::plan(adapter.as_ref(), &db, id)?.1)
                };
                print_plan(&v);
            }
            Some(SuggestCmd::Apply { id, yes }) => {
                let plan = if let Some(mut a) = agent() {
                    a.call("suggest.plan", json!({"id": id}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(filemind_agent::actions::plan(adapter.as_ref(), &db, id)?.1)
                };
                print_plan(&plan);
                if !plan["problems"]
                    .as_array()
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
                {
                    bail!("not applying: fix the problems above or dismiss the suggestion");
                }
                if plan["mode"].as_str() == Some("observe") {
                    bail!("mode is observe — FileMind only proposes. Run `filemind mode assist` to allow approved actions.");
                }
                if !yes {
                    eprint!(
                        "Apply {} step(s)? Reversible with `filemind undo {}`. [y/N] ",
                        plan["steps"],
                        plan["txn_id"].as_str().unwrap_or("<id>")
                    );
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    if !matches!(line.trim(), "y" | "Y" | "yes") {
                        println!("not applied");
                        return Ok(());
                    }
                }
                let v = if let Some(mut a) = agent() {
                    a.call("suggest.apply", json!({"id": id, "approved": true}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(filemind_agent::actions::apply(
                        adapter.as_ref(),
                        &db,
                        id,
                        true
                    )?)
                };
                println!(
                    "{}: {} step(s) done, {} failed — undo with `filemind undo {}`",
                    v["state"].as_str().unwrap_or("?"),
                    v["done"],
                    v["failed"],
                    v["txn_id"].as_str().unwrap_or("?")
                );
            }
            Some(SuggestCmd::Dismiss { id }) => {
                let ok = if let Some(mut a) = agent() {
                    a.call("suggest.dismiss", json!({"id": id}))?["ok"].as_bool() == Some(true)
                } else {
                    let (_, db) = open_db()?;
                    db.set_suggestion_state(id, "dismissed")?
                };
                if ok {
                    println!("dismissed #{id}");
                } else {
                    bail!("no suggestion #{id}");
                }
            }
        },

        Cmd::Classify {
            cmd,
            duty,
            minutes,
            names_only,
        } => {
            match cmd {
                None => {
                    let v = if let Some(mut a) = agent() {
                        a.call(
                            "classify.run",
                            json!({"duty": duty, "minutes": minutes, "names_only": names_only}),
                        )?
                    } else {
                        let (_, db) = open_db()?;
                        let o = filemind_agent::classifier::classify_pending(
                            &db,
                            filemind_agent::classifier::ClassifyOpts {
                                duty_cycle: duty,
                                max_wall: minutes.map(|m| Duration::from_secs(m * 60)),
                                names_only,
                            },
                        )?;
                        json!({"classified": o.classified, "extracted": o.extracted, "sensitive": o.sensitive, "remaining": o.remaining, "elapsed_ms": o.elapsed_ms})
                    };
                    let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
                    println!(
                    "classified {} files ({} with text extracted, {} sensitive) in {:.1}s  still pending {}",
                    g("classified"),
                    g("extracted"),
                    g("sensitive"),
                    g("elapsed_ms") as f64 / 1000.0,
                    g("remaining")
                );
                }
                Some(ClassifyCmd::Show { path }) => {
                    let path = path.canonicalize()?;
                    let v = if let Some(mut a) = agent() {
                        a.call("classify.show", json!({"path": path}))?
                    } else {
                        let (_, db) = open_db()?;
                        match db.classification_of(&path)? {
                            Some((c, sens)) => {
                                json!({"indexed": true, "classification": c, "sensitive": sens})
                            }
                            None => {
                                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                                let rules: Vec<_> =
                                    db.list_rules()?.into_iter().map(|(_, r)| r).collect();
                                let (c, sens, _) = filemind_agent::classifier::classify_path(
                                    &path, size, &rules, false,
                                );
                                json!({"indexed": false, "classification": c, "sensitive": sens})
                            }
                        }
                    };
                    let c = &v["classification"];
                    println!("{}", path.display());
                    println!(
                        "category    {}  ({:.0}% via {}){}",
                        c["category"].as_str().unwrap_or("?"),
                        c["confidence"].as_f64().unwrap_or(0.0) * 100.0,
                        c["source"].as_str().unwrap_or("?"),
                        if v["indexed"].as_bool() == Some(true) {
                            ""
                        } else {
                            "  [not indexed — classified on the spot]"
                        }
                    );
                    for sgn in c["signals"].as_array().cloned().unwrap_or_default() {
                        println!("  · {}", sgn.as_str().unwrap_or(""));
                    }
                    if let Some(s) = v["sensitive"].as_str() {
                        println!("sensitive   yes ({s}) — never sent to any AI adapter, text not indexed");
                    }
                }
                Some(ClassifyCmd::Set {
                    path,
                    category,
                    scope,
                    token,
                }) => {
                    let path = path.canonicalize().unwrap_or(path);
                    let v = if let Some(mut a) = agent() {
                        a.call("classify.set", json!({"path": path, "category": category, "scope": scope, "token": token}))?
                    } else {
                        let (_, db) = open_db()?;
                        let cat = filemind_core::model::Category::parse(&category)
                            .ok_or_else(|| anyhow::anyhow!("unknown category '{category}'"))?;
                        let mut rule_id = None;
                        match scope.as_str() {
                            "folder" => {
                                let prefix = path
                                    .parent()
                                    .map(|d| d.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                rule_id = Some(db.add_rule(
                                    &filemind_core::classify::UserRule::PathPrefix {
                                        prefix,
                                        category: cat,
                                    },
                                )?);
                            }
                            "ext" => {
                                let ext = path
                                    .extension()
                                    .map(|e| e.to_string_lossy().to_lowercase())
                                    .unwrap_or_default();
                                rule_id =
                                    Some(db.add_rule(&filemind_core::classify::UserRule::Ext {
                                        ext,
                                        category: cat,
                                    })?);
                            }
                            "name" => {
                                let token = token
                                    .clone()
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("--token is required with --scope name")
                                    })?
                                    .to_lowercase();
                                rule_id = Some(db.add_rule(
                                    &filemind_core::classify::UserRule::NameContains {
                                        token,
                                        category: cat,
                                    },
                                )?);
                            }
                            _ => {}
                        }
                        let ok = db.set_user_category(&path, cat)?;
                        json!({"ok": ok || rule_id.is_some(), "rule_id": rule_id})
                    };
                    if v["ok"].as_bool() == Some(true) {
                        match v["rule_id"].as_i64() {
                        Some(id) => println!("set {} → {category}; rule #{id} will apply to matching files on the next classify pass", path.display()),
                        None => println!("set {} → {category}", path.display()),
                    }
                    } else {
                        bail!("{} is not in the index", path.display());
                    }
                }
                Some(ClassifyCmd::Forget { rule_id }) => {
                    let ok = if let Some(mut a) = agent() {
                        a.call("rules.remove", json!({"id": rule_id}))?["ok"].as_bool()
                            == Some(true)
                    } else {
                        let (_, db) = open_db()?;
                        db.remove_rule(rule_id)?
                    };
                    if ok {
                        println!("forgot rule #{rule_id}");
                    } else {
                        bail!("no rule #{rule_id}");
                    }
                }
            }
        }

        Cmd::Categories => {
            let v = if let Some(mut a) = agent() {
                a.call("categories", json!({}))?
            } else {
                let (_, db) = open_db()?;
                json!({
                    "categories": db.category_counts()?.into_iter().map(|(c, n, b)| json!({"category": c, "files": n, "bytes": b})).collect::<Vec<_>>(),
                    "sensitive": db.sensitive_count()?,
                    "pending": db.count_classify_pending()?,
                    "rules": db.list_rules()?.into_iter().map(|(id, r)| json!({"id": id, "rule": format!("{r:?}")})).collect::<Vec<_>>()
                })
            };
            for c in v["categories"].as_array().cloned().unwrap_or_default() {
                println!(
                    "  {:<13} {:>8} files  {:>10}",
                    c["category"].as_str().unwrap_or("?"),
                    c["files"],
                    human_bytes(c["bytes"].as_u64().unwrap_or(0))
                );
            }
            println!();
            println!(
                "  sensitive     {} files (excluded from AI adapters and text search)",
                v["sensitive"]
            );
            println!(
                "  pending       {} files not yet classified — `filemind classify`",
                v["pending"]
            );
            let rules = v["rules"].as_array().cloned().unwrap_or_default();
            if !rules.is_empty() {
                println!();
                println!("  your rules:");
                for r in rules {
                    println!("    #{:<4} {}", r["id"], r["rule"].as_str().unwrap_or(""));
                }
            }
        }

        Cmd::Projects { limit } => {
            let v = if let Some(mut a) = agent() {
                a.call("projects.list", json!({"limit": limit}))?
            } else {
                let (_, db) = open_db()?;
                json!(db.list_projects(limit)?)
            };
            let items = v.as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                println!("no projects yet — run `filemind analyze` after a scan");
            }
            for p in items {
                let name = p["name"]
                    .as_str()
                    .or(p["suggested_name"].as_str())
                    .unwrap_or("?");
                let end = p["end_ts"]
                    .as_i64()
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d")
                            .to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "  #{:<5} {:<8} {:<36} {:>6} files  {:>9}  last {}  {}",
                    p["project_id"],
                    p["status"].as_str().unwrap_or("?"),
                    if name.chars().count() > 36 {
                        format!("{}…", name.chars().take(35).collect::<String>())
                    } else {
                        name.to_string()
                    },
                    p["file_count"],
                    human_bytes(p["bytes"].as_u64().unwrap_or(0)),
                    end,
                    p["kind"].as_str().unwrap_or("")
                );
            }
        }

        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Show { id, limit } => {
                let v = if let Some(mut a) = agent() {
                    a.call("projects.show", json!({"id": id, "limit": limit}))?
                } else {
                    let (_, db) = open_db()?;
                    let Some(pr) = db.project(id)? else {
                        bail!("no project #{id}")
                    };
                    json!({
                        "project": pr,
                        "files": db.project_files(id, limit)?.into_iter().map(|(p, m, s)| json!({"path": p, "mtime": m, "size": s})).collect::<Vec<_>>(),
                        "categories": db.project_categories(id)?.into_iter().map(|(c, n)| json!({"category": c, "files": n})).collect::<Vec<_>>()
                    })
                };
                let p = &v["project"];
                let name = p["name"]
                    .as_str()
                    .or(p["suggested_name"].as_str())
                    .unwrap_or("?");
                let fmt = |t: Option<i64>| {
                    t.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                        .map(|t| {
                            t.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d")
                                .to_string()
                        })
                        .unwrap_or_default()
                };
                println!(
                    "{name}   [{} · {}]",
                    p["kind"].as_str().unwrap_or(""),
                    p["status"].as_str().unwrap_or("")
                );
                if p["name"].is_string() {
                    println!(
                        "suggested    {}",
                        p["suggested_name"].as_str().unwrap_or("")
                    );
                }
                println!("where        {}", p["key"].as_str().unwrap_or(""));
                println!(
                    "files        {}  ({})",
                    p["file_count"],
                    human_bytes(p["bytes"].as_u64().unwrap_or(0))
                );
                println!(
                    "active       {} → {}",
                    fmt(p["start_ts"].as_i64()),
                    fmt(p["end_ts"].as_i64())
                );
                let cats: Vec<String> = v["categories"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| format!("{} {}", c["files"], c["category"].as_str().unwrap_or("?")))
                    .collect();
                if !cats.is_empty() {
                    println!("contains     {}", cats.join(", "));
                }
                println!();
                for f in v["files"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  {}  {}",
                        fmt(f["mtime"].as_i64()),
                        f["path"].as_str().unwrap_or("?")
                    );
                }
            }
            ProjectCmd::Rename { id, name } => {
                let ok = if let Some(mut a) = agent() {
                    a.call("projects.rename", json!({"id": id, "name": name}))?["ok"].as_bool()
                        == Some(true)
                } else {
                    let (_, db) = open_db()?;
                    db.rename_project(id, Some(&name))?
                };
                if ok {
                    println!("project #{id} is now \"{name}\"");
                } else {
                    bail!("no project #{id}");
                }
            }
            ProjectCmd::Of { path } => {
                let path = path.canonicalize().unwrap_or(path);
                let v = if let Some(mut a) = agent() {
                    a.call("projects.of", json!({"path": path}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(db.project_of_path(&path)?)
                };
                if v.is_null() {
                    println!("{} is not part of any detected project", path.display());
                } else {
                    println!(
                        "#{}  {}",
                        v["project_id"],
                        v["name"]
                            .as_str()
                            .or(v["suggested_name"].as_str())
                            .unwrap_or("?")
                    );
                }
            }
        },

        Cmd::History { limit, id } => {
            if let Some(id) = id {
                let v = if let Some(mut a) = agent() {
                    a.call("txn.show", json!({"id": id}))?
                } else {
                    let (_, db) = open_db()?;
                    let Some((m, state, steps)) = db.load_txn(&id)? else {
                        bail!("unknown transaction {id}")
                    };
                    json!({"manifest": m, "state": state, "steps": steps, "diff": m.diff()})
                };
                println!("{}   [{}]", id, v["state"].as_str().unwrap_or("?"));
                println!("{}", v["manifest"]["rationale"].as_str().unwrap_or(""));
                println!();
                let states = v["steps"].as_array().cloned().unwrap_or_default();
                for line in v["diff"].as_str().unwrap_or("").lines() {
                    // step lines start with the step number; continuation lines are indented
                    match line
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<usize>().ok())
                    {
                        Some(i) if !line.starts_with("     ") => {
                            let st = states.get(i).and_then(Value::as_str).unwrap_or("");
                            println!("{line}   [{st}]");
                        }
                        _ => println!("{line}"),
                    }
                }
                return Ok(());
            }
            let v = if let Some(mut a) = agent() {
                a.call("txn.list", json!({"limit": limit}))?
            } else {
                let (_, db) = open_db()?;
                json!(db.list_txns(limit)?)
            };
            let items = v.as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                println!("no transactions yet — FileMind has not moved or trashed anything");
            }
            for t in items {
                let when = t["created_ts"]
                    .as_i64()
                    .and_then(|x| chrono::DateTime::from_timestamp(x, 0))
                    .map(|x| {
                        x.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "  {}  {:<8} {}/{} steps  {}  {}",
                    when,
                    t["state"].as_str().unwrap_or("?"),
                    t["done"],
                    t["steps"],
                    t["txn_id"].as_str().unwrap_or("?"),
                    t["rationale"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(70)
                        .collect::<String>()
                );
            }
        }

        Cmd::Undo { txn_id } => {
            let v = if let Some(mut a) = agent() {
                a.call("txn.undo", json!({"id": txn_id}))?
            } else {
                let (_, db) = open_db()?;
                json!(filemind_agent::actions::undo(
                    adapter.as_ref(),
                    &db,
                    &txn_id
                )?)
            };
            println!("restored {} step(s)", v["restored"]);
            for s in v["skipped"].as_array().cloned().unwrap_or_default() {
                println!("  left alone: {}", s.as_str().unwrap_or(""));
            }
        }

        Cmd::Agent { cmd } => match cmd {
            AgentCmd::Start => {
                if agent().is_some() {
                    bail!("an agent is already running");
                }
                let exe = std::env::current_exe()?;
                let bin = exe.with_file_name("filemind-agent");
                println!("starting {} (Ctrl-C to stop)", bin.display());
                let status = std::process::Command::new(bin).status()?;
                if !status.success() {
                    bail!("agent exited with {status}");
                }
            }
            AgentCmd::Stop => {
                let path = filemind_storage::default_db_path()?;
                if agent().is_none() {
                    println!("agent is not running");
                } else {
                    std::fs::write(path.with_file_name("agent.stop"), b"")?;
                    println!("stop requested");
                }
            }
            AgentCmd::Status => match agent() {
                Some(mut a) => {
                    let v = a.call("status", json!({}))?;
                    println!("running  uptime {}s", v["uptime_s"]);
                    if let Some(w) = v.get("watcher") {
                        println!(
                            "watcher  {} roots, {} raw events, {} changes applied",
                            w["roots"], w["raw_events"], w["changes_applied"]
                        );
                    }
                }
                None => println!("not running"),
            },
            AgentCmd::Install => {
                adapter.register_autostart(true)?;
                println!("agent registered to start at login");
            }
            AgentCmd::Uninstall => {
                adapter.register_autostart(false)?;
                println!("agent unregistered");
            }
            AgentCmd::RunOnce => {
                let (_, db) = open_db()?;
                let sched = filemind_agent::scheduler::schedule_from_settings(&db);
                println!(
                    "{}",
                    filemind_agent::scheduler::tick(adapter.as_ref(), &db, sched)?
                );
            }
        },

        Cmd::Dev { cmd } => match cmd {
            DevCmd::Fixture { dir, entries } => {
                let s = filemind_agent::fixture::build(&dir, entries)?;
                println!(
                    "fixture at {}: {} files, {} dirs, {} exact-duplicate files, {} version chains, {} links",
                    dir.display(),
                    s.files,
                    s.dirs,
                    s.duplicates,
                    s.version_chains,
                    s.links
                );
            }
        },
    }
    Ok(())
}
