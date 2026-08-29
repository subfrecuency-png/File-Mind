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
enum SuggestCmd {
    /// Hide a suggestion; it stays hidden across re-analysis.
    Dismiss { id: i64 },
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
/// it so there is a single writer and the watcher stays consistent.
fn agent() -> Option<Client> {
    let path = filemind_storage::default_db_path().ok()?;
    Client::connect(&path)
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

fn main() -> Result<()> {
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
            println!("{}", db.get_setting("mode")?.unwrap_or_default());
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
                       "version_chains": a.version_chains, "suggestions": a.suggestions,
                       "health": a.health, "elapsed_ms": a.elapsed_ms})
            };
            let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            println!(
                "health {}  duplicate groups {} ({} reclaimable)  version chains {}  suggestions {}  in {:.1}s",
                v["health"]["score"],
                g("duplicate_groups"),
                human_bytes(g("duplicate_bytes")),
                g("version_chains"),
                g("suggestions"),
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
