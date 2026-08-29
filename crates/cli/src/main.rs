//! `filemind` CLI. Drives the agent library directly; Phase 2 moves the
//! heavy commands behind the daemon's JSON-RPC socket.

mod fixture;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use filemind_agent::pipeline::{self, HashOpts};
use filemind_agent::platform;
use filemind_core::{Mode, ScanOpts, Scanner};
use filemind_storage::Db;
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
enum AgentCmd {
    /// Register filemind-agent to start at login (launchd / Task Scheduler).
    Install,
    /// Unregister the agent.
    Uninstall,
    /// Run one scan + hash tick in the foreground.
    RunOnce,
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
            let (path, db) = open_db()?;
            let c = db.counts()?;
            println!("platform      {}", adapter.platform());
            println!("database      {}", path.display());
            println!(
                "mode          {}",
                db.get_setting("mode")?.unwrap_or_default()
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
            let (_, db) = open_db()?;
            if let Some(m) = value {
                db.set_setting("mode", &serde_json::to_value(m)?)?;
            }
            println!("{}", db.get_setting("mode")?.unwrap_or_default());
        }

        Cmd::Agent { cmd } => match cmd {
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
                let s = fixture::build(&dir, entries)?;
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
