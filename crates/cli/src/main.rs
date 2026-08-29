//! `filemind` CLI. In Phase 1 this talks to the agent over JSON-RPC; for
//! Phase 0 it drives the core library directly so the scanner can be
//! exercised before the daemon exists.

#[path = "../../agent/src/platform.rs"]
mod platform;

use anyhow::Result;
use clap::{Parser, Subcommand};
use filemind_core::{Mode, ScanOpts, Scanner};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "filemind", version, about = "AI Memory for Your Computer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show database location, mode and inventory counts.
    Status,
    /// Read-only scan of a root. Writes nothing to disk except the database (and not even that with --dry-run).
    Scan {
        root: PathBuf,
        /// Only print the report; do not touch the database.
        #[arg(long)]
        dry_run: bool,
        /// Also hash file contents (slower).
        #[arg(long)]
        hash: bool,
    },
    /// Get or set the operating mode (observe | assist | automate).
    Mode {
        #[arg(value_parser = parse_mode)]
        value: Option<Mode>,
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
            let path = filemind_storage::default_db_path()?;
            let db = filemind_storage::Db::open(&path)?;
            let c = db.counts()?;
            println!("platform      {}", adapter.platform());
            println!("database      {}", path.display());
            println!(
                "mode          {}",
                db.get_setting("mode")?.unwrap_or_default()
            );
            println!("roots         {}", c.roots);
            println!("files         {}", c.files);
            println!("transactions  {}", c.transactions);
            println!("default roots:");
            for r in adapter.default_roots() {
                println!("  {}", r.display());
            }
        }
        Cmd::Scan {
            root,
            dry_run,
            hash,
        } => {
            let root = root.canonicalize()?;
            let scanner = Scanner::new(adapter.as_ref(), vec![root.clone()]);
            let opts = ScanOpts {
                hash_contents: hash,
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let mut sample = Vec::new();
            let report = scanner.scan_root(&root, &opts, |e| {
                if sample.len() < 5 {
                    sample.push(e.path.clone());
                }
            })?;
            println!("scanned {} in {:.1?}", root.display(), started.elapsed());
            println!(
                "files {}  dirs {}  bytes {}  links skipped {}  protected skipped {}  ignored {}  errors {}",
                report.files,
                report.dirs,
                report.bytes,
                report.links_skipped,
                report.protected_skipped,
                report.ignored,
                report.errors
            );
            for p in sample {
                println!("  {}", p.display());
            }
            if dry_run {
                println!("(dry run: database untouched)");
            } else {
                println!("(Phase 1 will persist the inventory; nothing written yet)");
            }
        }
        Cmd::Mode { value } => {
            let path = filemind_storage::default_db_path()?;
            let db = filemind_storage::Db::open(&path)?;
            if let Some(m) = value {
                db.set_setting("mode", &serde_json::to_value(m)?)?;
            }
            println!("{}", db.get_setting("mode")?.unwrap_or_default());
        }
    }
    Ok(())
}
