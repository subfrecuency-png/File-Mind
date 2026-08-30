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
    /// Search by meaning and words: "offer sheet for the calcium supplier from last spring, pdf".
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Words only (FTS5), no vectors.
        #[arg(long, conflicts_with = "semantic")]
        lexical: bool,
        /// Vectors only.
        #[arg(long)]
        semantic: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Ask a question about your files; answered by the configured AI adapter over the top search hits.
    Ask { question: String },
    /// Remember something about a file or folder. Notes are searchable.
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
    },
    /// Build vectors for files that have none yet (throttled).
    Embed {
        #[arg(long, default_value_t = 0.5)]
        duty: f32,
        #[arg(long)]
        minutes: Option<u64>,
    },
    /// The local embedding model (bge-small, ~130 MB, downloaded once).
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Which AI adapter answers `ask`: none, ollama (local) or cloud (opt-in). Plus the audit log.
    Ai {
        #[command(subcommand)]
        cmd: AiCmd,
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
    /// Automate-mode rules: tier-0 actions that run unattended after a preview week.
    Rule {
        #[command(subcommand)]
        cmd: Option<RuleCmd>,
    },
    /// Shrink: reclaim disk space losslessly. Estimate first, rewrite later.
    Shrink {
        #[command(subcommand)]
        cmd: ShrinkCmd,
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
enum NoteCmd {
    /// Attach a note to a file or folder path.
    Add { path: PathBuf, text: String },
    /// Notes on one path, or all notes.
    List { path: Option<PathBuf> },
    /// Delete a note by id.
    Rm { id: i64 },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Is the model installed, and how much of the index has vectors?
    Status,
    /// Download and verify the model files.
    Download,
}

#[derive(Subcommand)]
enum AiCmd {
    /// Current adapter and whether Ollama is reachable.
    Status,
    /// Choose the adapter: none | ollama | cloud.
    Use {
        #[arg(value_parser = ["none", "ollama", "cloud"])]
        adapter: String,
        /// Model name (ollama: e.g. llama3.2; cloud: an Anthropic model id).
        #[arg(long)]
        model: Option<String>,
        /// API key for the cloud adapter (stored in FileMind's database).
        #[arg(long)]
        key: Option<String>,
        /// Ollama base URL.
        #[arg(long)]
        url: Option<String>,
    },
    /// What has been sent to which adapter (never the text itself).
    Audit {
        #[arg(long, default_value_t = 30)]
        limit: usize,
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
enum ShrinkCmd {
    /// Measure how much each Shrink tier could reclaim by sampling and
    /// compressing a few KB per candidate file. Reads only; writes nothing.
    Estimate {
        /// Recompute even if a fresh (< 24 h) estimate is cached.
        #[arg(long)]
        refresh: bool,
        /// Print the full report as JSON.
        #[arg(long)]
        json: bool,
        /// Cold projects to list in the archive breakdown.
        #[arg(long, default_value_t = 10)]
        projects: usize,
    },
    /// What one file looks like on disk: compressed or not, bytes used.
    Info { path: PathBuf },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// The allow-listed rule kinds and their default parameters.
    Kinds,
    /// Create a rule in preview state. `--set key=value` overrides a default.
    Add {
        /// archive_stale_downloads | collapse_versions | trash_exact_duplicates
        kind: String,
        /// e.g. --set older_than_days=120 --set max_items_per_run=20
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// What the rule would do right now, and what it would have done during the preview.
    Preview { id: i64 },
    /// Arm a rule once it has previewed for the required days. It runs only in automate mode.
    Arm { id: i64 },
    /// Pause a rule (it keeps previewing nothing; arm it again to resume).
    Pause { id: i64 },
    /// Remove a rule and its run log. No file is touched.
    Rm { id: i64 },
    /// The rule's run log (dry runs and real transactions).
    Runs { id: i64 },
    /// Evaluate every rule now (and run armed ones if the mode is automate).
    Tick,
    /// Change a rule's parameters (validated; bounds still apply).
    Set {
        id: i64,
        /// e.g. pause_above=1000 max_items_per_run=100
        #[arg(value_name = "KEY=VALUE", required = true)]
        set: Vec<String>,
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
    /// Print the database encryption key (hex) so `sqlcipher` can open the file:
    /// sqlcipher filemind.db  then  PRAGMA key = "x'<hex>'";
    DbKey,
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

/// Call a method on the running agent, or handle it in-process against the
/// database exactly as the agent would (same handler, same code).
fn rpc(adapter: &dyn filemind_core::OsAdapter, method: &str, params: Value) -> Result<Value> {
    if let Some(mut a) = agent() {
        return a.call(method, params);
    }
    let _ = adapter; // the in-process context builds its own (same platform)
    let adapter: std::sync::Arc<dyn filemind_core::OsAdapter> =
        std::sync::Arc::from(platform::adapter());
    let (db_path, db) = open_db()?;
    let _ = filemind_agent::actions::recover_all(adapter.as_ref(), &db);
    let engine = filemind_agent::semantic::Engine::open(&db)?;
    let ctx = filemind_agent::rpc::Context {
        adapter: adapter.clone(),
        db_path,
        db: std::sync::Mutex::new(db),
        engine: std::sync::Arc::new(engine),
        stats: std::sync::Arc::new(filemind_agent::watcher::WatchStats::default()),
        started: std::time::Instant::now(),
    };
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp = filemind_agent::rpc::handle(&ctx, &req);
    if let Some(e) = resp.get("error") {
        bail!("{}", e["message"].as_str().unwrap_or("rpc error"));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

fn print_rule(r: &Value, describe: &str, w: &Value) {
    let rule = r;
    let state = rule["state"].as_str().unwrap_or("?");
    println!(
        "  #{:<4} {:<8} {:<24} {}",
        rule["rule_id"],
        state,
        rule["kind"].as_str().unwrap_or("?"),
        describe
    );
    if !w.is_null() {
        println!(
            "        preview: would have touched {} file{} ({}) over {} dry run{}{}",
            w["files"].as_array().map(|a| a.len()).unwrap_or(0),
            if w["files"].as_array().map(|a| a.len()).unwrap_or(0) == 1 {
                ""
            } else {
                "s"
            },
            human_bytes(w["bytes"].as_u64().unwrap_or(0)),
            w["dry_runs"],
            if w["dry_runs"].as_u64() == Some(1) {
                ""
            } else {
                "s"
            },
            if w["real_runs"].as_u64().unwrap_or(0) > 0 {
                format!(", {} real run(s)", w["real_runs"])
            } else {
                String::new()
            }
        );
        if state == "preview" || state == "paused" {
            if w["armable"].as_bool() == Some(true) {
                println!(
                    "        ready to arm: `filemind rule arm {}`",
                    rule["rule_id"]
                );
            } else {
                println!(
                    "        not armable yet: {}",
                    w["armable_reason"].as_str().unwrap_or("")
                );
            }
        }
        if let Some(p) = w["last_problems"].as_array().filter(|a| !a.is_empty()) {
            for x in p {
                println!("        ! {}", x.as_str().unwrap_or(""));
            }
        }
    }
    if let Some(why) = rule["paused_reason"].as_str() {
        println!("        paused: {why}");
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

fn print_search(v: &Value) {
    let notes: Vec<String> = v["parsed"]["notes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let residual = v["parsed"]["text"].as_str().unwrap_or("");
    let mut head = Vec::new();
    if !residual.is_empty() {
        head.push(format!("looking for \"{residual}\""));
    }
    head.extend(notes);
    if !head.is_empty() {
        println!("{}", head.join("  ·  "));
    }
    if v["semantic"].as_bool() != Some(true) {
        println!("(embedding model not installed: word matching only — `filemind model download`)");
    }
    let hits = v["hits"].as_array().cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("no matches");
    }
    for (i, h) in hits.iter().enumerate() {
        let via: Vec<&str> = h["via"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        println!("{:>3}  {}", i + 1, h["path"].as_str().unwrap_or("?"));
        println!(
            "     {}  {:>9}  {}{}  via {}",
            fmt_ts(h["mtime"].as_i64().unwrap_or(0)),
            human_bytes(h["size"].as_u64().unwrap_or(0)),
            h["category"].as_str().unwrap_or("-"),
            if h["sensitive"].as_bool() == Some(true) {
                "  ⚠ sensitive"
            } else {
                ""
            },
            via.join(", ")
        );
    }
    let notes = v["notes"].as_array().cloned().unwrap_or_default();
    if !notes.is_empty() {
        println!("notes:");
        for n in notes {
            println!(
                "  #{} {}  — {}",
                n["note_id"],
                n["text"].as_str().unwrap_or(""),
                n["subject_id"].as_str().unwrap_or("")
            );
        }
    }
    println!("({} ms)", v["elapsed_ms"]);
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

fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "?".into())
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
    filemind_agent::crash::install("cli");
    // `filemind search … | head` closes our stdout early; the default Rust
    // behaviour is a `println!` panic on EPIPE. Restore SIGPIPE's default
    // disposition so the process simply ends, like every other CLI tool.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,pdf_extract=error,lopdf=error")
            }),
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

        Cmd::Search {
            query,
            limit,
            lexical,
            semantic,
            json: as_json,
        } => {
            let mode = if lexical {
                "lexical"
            } else if semantic {
                "semantic"
            } else {
                "hybrid"
            };
            let v = if let Some(mut a) = agent() {
                a.call(
                    "search",
                    json!({"query": query, "limit": limit, "mode": mode}),
                )?
            } else {
                let (_, db) = open_db()?;
                let engine = filemind_agent::semantic::Engine::open(&db)?;
                let m = match mode {
                    "lexical" => filemind_agent::semantic::Mode::Lexical,
                    "semantic" => filemind_agent::semantic::Mode::Semantic,
                    _ => filemind_agent::semantic::Mode::Hybrid,
                };
                json!(filemind_agent::semantic::search(
                    &db, &engine, &query, limit, m
                )?)
            };
            if as_json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            print_search(&v);
        }

        Cmd::Ask { question } => {
            let v = if let Some(mut a) = agent() {
                a.call("ask", json!({"question": question}))?
            } else {
                let (_, db) = open_db()?;
                let engine = filemind_agent::semantic::Engine::open(&db)?;
                let cfg = filemind_agent::semantic::ai_config(&db);
                let adapter = cfg.build();
                json!(filemind_agent::semantic::ask(
                    &db,
                    &engine,
                    adapter.as_ref(),
                    &question
                )?)
            };
            println!("{}", v["answer"].as_str().unwrap_or(""));
            println!();
            for (i, h) in v["hits"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                println!("  [{}] {}", i + 1, h["path"].as_str().unwrap_or("?"));
            }
            println!(
                "\n({} adapter, {} bytes sent{})",
                v["adapter"].as_str().unwrap_or("?"),
                v["bytes_sent"],
                if v["local"].as_bool() == Some(true) {
                    ", stayed on this machine"
                } else {
                    ", sent to the cloud"
                }
            );
        }

        Cmd::Note { cmd } => match cmd {
            NoteCmd::Add { path, text } => {
                let path = path.canonicalize().unwrap_or(path);
                let kind = if path.is_dir() { "folder" } else { "file" };
                let v = if let Some(mut a) = agent() {
                    a.call(
                        "notes.add",
                        json!({"subject": path, "text": text, "kind": kind}),
                    )?
                } else {
                    let (_, db) = open_db()?;
                    let n = db.add_note(kind, &path.to_string_lossy(), &text, "user")?;
                    let engine = filemind_agent::semantic::Engine::open(&db)?;
                    let _ = filemind_agent::semantic::embed_pending(
                        &db,
                        &engine,
                        filemind_agent::semantic::EmbedOpts {
                            max_wall: Some(std::time::Duration::from_millis(1)),
                            ..Default::default()
                        },
                    );
                    json!(n)
                };
                println!("note #{} on {}", v["note_id"], path.display());
            }
            NoteCmd::List { path } => {
                let subject = path.map(|p| p.canonicalize().unwrap_or(p));
                let v = if let Some(mut a) = agent() {
                    a.call("notes.list", json!({"subject": subject, "limit": 100}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(db.list_notes(
                        subject
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string())
                            .as_deref(),
                        100
                    )?)
                };
                let items = v.as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("(no notes)");
                }
                for n in items {
                    println!(
                        "#{:<5} {}  {}\n       {}",
                        n["note_id"],
                        fmt_ts(n["ts"].as_i64().unwrap_or(0)),
                        n["subject_id"].as_str().unwrap_or("?"),
                        n["text"].as_str().unwrap_or("")
                    );
                }
            }
            NoteCmd::Rm { id } => {
                let ok = if let Some(mut a) = agent() {
                    a.call("notes.remove", json!({"id": id}))?["ok"].as_bool() == Some(true)
                } else {
                    let (_, db) = open_db()?;
                    db.remove_note(id)?
                };
                println!("{}", if ok { "removed" } else { "no such note" });
            }
        },

        Cmd::Embed { duty, minutes } => {
            let v = if let Some(mut a) = agent() {
                a.call("embed.run", json!({"duty": duty, "minutes": minutes}))?
            } else {
                let (_, db) = open_db()?;
                let engine = filemind_agent::semantic::Engine::open(&db)?;
                json!(filemind_agent::semantic::embed_pending(
                    &db,
                    &engine,
                    filemind_agent::semantic::EmbedOpts {
                        duty_cycle: duty,
                        max_wall: minutes.map(|m| std::time::Duration::from_secs(m * 60)),
                        ..Default::default()
                    }
                )?)
            };
            println!(
                "embedded {} files (+{} notes), {} unchanged, {} still pending, in {:.1}s",
                v["embedded"],
                v["notes"],
                v["unchanged"],
                v["remaining"],
                v["elapsed_ms"].as_u64().unwrap_or(0) as f64 / 1000.0
            );
        }

        Cmd::Model { cmd } => match cmd {
            ModelCmd::Status => {
                let spec = filemind_ai::embed::BGE_SMALL;
                println!(
                    "{}  {}  ({})",
                    spec.id,
                    if spec.is_installed() {
                        "installed"
                    } else {
                        "NOT installed — run `filemind model download`"
                    },
                    spec.dir()?.display()
                );
                let v = if let Some(mut a) = agent() {
                    a.call("embed.status", json!({}))?
                } else {
                    let (_, db) = open_db()?;
                    let engine = filemind_agent::semantic::Engine::open(&db)?;
                    let (have, pending) = db.embedding_stats(engine.model_id())?;
                    json!({"model": engine.model_id(), "semantic": engine.is_semantic(), "vectors": engine.vectors(), "embedded": have, "pending": pending})
                };
                println!(
                    "index    {} vectors under {}  ({} files embedded, {} pending){}",
                    v["vectors"],
                    v["model"].as_str().unwrap_or("?"),
                    v["embedded"],
                    v["pending"],
                    if v["semantic"].as_bool() == Some(true) {
                        ""
                    } else {
                        "  [hash fallback: lexical-ish only]"
                    }
                );
            }
            ModelCmd::Download => {
                let spec = filemind_ai::embed::BGE_SMALL;
                let mut last = 0u64;
                let dir = spec.download(&mut |name, got, total| {
                    if got - last > (8 << 20) || got == total {
                        last = got;
                        eprint!("\r{name}: {} / {}   ", human_bytes(got), human_bytes(total));
                    }
                })?;
                eprintln!();
                println!("installed in {}", dir.display());
                if agent().is_some() {
                    println!("restart the agent (`filemind agent stop`, then start) so it picks the model up; vectors are built in the background.");
                }
            }
        },

        Cmd::Ai { cmd } => match cmd {
            AiCmd::Status => {
                let cfg: filemind_ai::llm::Config = if let Some(mut a) = agent() {
                    serde_json::from_value(a.call("ai.get", json!({}))?)?
                } else {
                    let (_, db) = open_db()?;
                    filemind_agent::semantic::ai_config(&db)
                };
                println!(
                    "adapter  {}",
                    serde_json::to_value(&cfg.adapter)?.as_str().unwrap_or("?")
                );
                let up = filemind_ai::llm::Ollama::reachable(&cfg.ollama_url);
                println!(
                    "ollama   {} at {}  (model {})",
                    if up { "reachable" } else { "not running" },
                    cfg.ollama_url,
                    cfg.ollama_model
                );
                if up {
                    if let Ok(m) = filemind_ai::llm::Ollama::models(&cfg.ollama_url) {
                        println!("         available: {}", m.join(", "));
                    }
                }
                println!(
                    "cloud    model {}  key {}",
                    cfg.cloud_model,
                    if !cfg.cloud_key.is_empty() {
                        "stored"
                    } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                        "from ANTHROPIC_API_KEY"
                    } else {
                        "none"
                    }
                );
            }
            AiCmd::Use {
                adapter: which,
                model,
                key,
                url,
            } => {
                let (_, db) = open_db()?;
                let mut cfg = filemind_agent::semantic::ai_config(&db);
                cfg.adapter = match which.as_str() {
                    "ollama" => filemind_ai::llm::Kind::Ollama,
                    "cloud" => filemind_ai::llm::Kind::Cloud,
                    _ => filemind_ai::llm::Kind::None,
                };
                match which.as_str() {
                    "ollama" => {
                        if let Some(m) = model {
                            cfg.ollama_model = m;
                        }
                        if let Some(u) = url {
                            cfg.ollama_url = u;
                        }
                        if filemind_ai::llm::Ollama::is_cloud_tag(&cfg.ollama_model) {
                            eprintln!(
                                "note: {} is an Ollama *cloud* model — Ollama forwards each request to its hosted service, so `filemind ask` text leaves this machine. The audit log will mark these calls as ollama-cloud / CLOUD.",
                                cfg.ollama_model
                            );
                        }
                    }
                    "cloud" => {
                        if let Some(m) = model {
                            cfg.cloud_model = m;
                        }
                        if let Some(k) = key {
                            cfg.cloud_key = k;
                        }
                        eprintln!("note: with the cloud adapter, `filemind ask` sends file names, folders, dates and short excerpts (≤ 2 KB per question) to Anthropic's API. Every call is listed in `filemind ai audit`.");
                    }
                    _ => {}
                }
                if let Some(mut a) = agent() {
                    a.call("ai.set", serde_json::to_value(&cfg)?)?;
                } else {
                    filemind_agent::semantic::set_ai_config(&db, &cfg)?;
                }
                println!("adapter set to {which}");
            }
            AiCmd::Audit { limit } => {
                let v = if let Some(mut a) = agent() {
                    a.call("ai.audit", json!({"limit": limit}))?
                } else {
                    let (_, db) = open_db()?;
                    json!(db.list_ai_audit(limit)?)
                };
                let rows = v.as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    println!("(nothing has been sent to any adapter)");
                }
                for r in rows {
                    println!(
                        "{}  {:<7} {:<8} {:>6} B  {}  {}{}",
                        fmt_ts(r["ts"].as_i64().unwrap_or(0)),
                        r["adapter"].as_str().unwrap_or("?"),
                        r["purpose"].as_str().unwrap_or("?"),
                        r["bytes_sent"],
                        if r["local"].as_bool() == Some(true) {
                            "local"
                        } else {
                            "CLOUD"
                        },
                        if r["ok"].as_bool() == Some(true) {
                            "ok"
                        } else {
                            "failed"
                        },
                        r["latency_ms"]
                            .as_i64()
                            .map(|m| format!("  {m} ms"))
                            .unwrap_or_default()
                    );
                }
            }
        },

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
                let txn_id = plan["txn_id"].as_str().unwrap_or_default().to_string();
                let fp = plan["fingerprint"].as_str().unwrap_or_default().to_string();
                let v = if let Some(mut a) = agent() {
                    a.call(
                        "suggest.apply",
                        json!({"id": id, "approved": true, "txn_id": txn_id, "fingerprint": fp}),
                    )?
                } else {
                    let (_, db) = open_db()?;
                    json!(filemind_agent::actions::apply(
                        adapter.as_ref(),
                        &db,
                        id,
                        true,
                        Some((txn_id.as_str(), fp.as_str()))
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

        Cmd::Shrink { cmd } => match cmd {
            ShrinkCmd::Estimate {
                refresh,
                json: as_json,
                projects,
            } => {
                let v = rpc(
                    adapter.as_ref(),
                    "shrink.estimate",
                    json!({"refresh": refresh}),
                )?;
                if as_json {
                    println!("{}", serde_json::to_string_pretty(&v["estimate"])?);
                    return Ok(());
                }
                let est: filemind_core::shrink::Estimate =
                    serde_json::from_value(v["estimate"].clone())?;
                println!("{}", est.headline());
                println!(
                    "  {} files · {} indexed · {} sampled ({}) in {:.1} s{}",
                    est.files_seen,
                    human_bytes(est.bytes_seen),
                    est.sampled_files,
                    human_bytes(est.sampled_bytes),
                    est.elapsed_ms as f64 / 1000.0,
                    if v["cached"].as_bool() == Some(true) {
                        format!(" · cached {} (--refresh to redo)", fmt_ts(est.computed_ts))
                    } else {
                        String::new()
                    }
                );
                for t in &est.tiers {
                    println!();
                    println!(
                        "tier {} · {}  {:>9}  of {} in {} files  (ratio {:.2}{})",
                        t.tier,
                        t.label,
                        human_bytes(t.saving_bytes),
                        human_bytes(t.candidate_bytes),
                        t.candidate_files,
                        t.ratio,
                        if t.measured { "" } else { ", assumed" }
                    );
                    println!("  {}", t.note);
                    for b in t.buckets.iter().filter(|b| b.files > 0) {
                        println!(
                            "  {:<14} {:>9}  of {:>9} in {:>7} files  ratio {:.2}  sampled {}",
                            b.name,
                            human_bytes(b.saving_bytes),
                            human_bytes(b.bytes),
                            b.files,
                            b.ratio,
                            b.sampled_files
                        );
                    }
                    if !t.projects.is_empty() {
                        println!("  cold projects:");
                        for pr in t.projects.iter().take(projects) {
                            println!(
                                "    {:>9}  of {:>9}  {}  (last touched {}){}",
                                human_bytes(pr.saving_bytes),
                                human_bytes(pr.bytes),
                                pr.name,
                                fmt_ts(pr.end_ts),
                                pr.root_path
                                    .as_deref()
                                    .map(|r| format!("  {r}"))
                                    .unwrap_or_default()
                            );
                        }
                        if t.projects.len() > projects {
                            println!("    … and {} more", t.projects.len() - projects);
                        }
                    }
                }
                println!();
                println!("Nothing was changed. Tier 1 arrives as `compress_cold_text` suggestions after the next analyze (`filemind suggest`); tiers 2–3 are not built yet.");
            }
            ShrinkCmd::Info { path } => {
                let path = path.canonicalize().unwrap_or(path);
                let md = std::fs::symlink_metadata(&path)?;
                let state = adapter.rewrite_state(&path, "apfs")?;
                let on_disk = adapter.on_disk_bytes(&path)?;
                println!("{}", path.display());
                println!("  {:<10} {}", "size", human_bytes(md.len()));
                println!(
                    "  {:<10} {}{}",
                    "on disk",
                    human_bytes(on_disk),
                    if md.len() > 0 {
                        format!("  ({:.0} %)", on_disk as f64 * 100.0 / md.len() as f64)
                    } else {
                        String::new()
                    }
                );
                println!(
                    "  {:<10} {}",
                    "apfs",
                    match state {
                        filemind_core::RewriteState::Original => "not compressed",
                        filemind_core::RewriteState::Rewritten =>
                            "compressed (transparent; reads are bit-identical)",
                        filemind_core::RewriteState::HalfDone =>
                            "UNFINISHED rewrite — run `filemind agent start` to recover",
                        filemind_core::RewriteState::Unsupported =>
                            "not available on this platform",
                    }
                );
                let (_, db) = open_db()?;
                let rw: Option<Option<String>> = db
                    .conn
                    .query_row(
                        "SELECT rewrite FROM files WHERE path = ?1 AND status = 'present'",
                        [path.to_string_lossy().to_string()],
                        |r| r.get(0),
                    )
                    .ok();
                println!(
                    "  {:<10} {}",
                    "index",
                    match rw {
                        None => "not indexed".to_string(),
                        Some(None) => "indexed, no rewrite recorded".to_string(),
                        Some(Some(m)) => format!("indexed, rewritten with {m} by FileMind"),
                    }
                );
            }
        },

        Cmd::Rule { cmd } => match cmd {
            None => {
                let v = rpc(adapter.as_ref(), "automate.list", json!({}))?;
                let rules = v["rules"].as_array().cloned().unwrap_or_default();
                println!(
                    "mode {}  ·  {} rule{}  ·  preview period {} days",
                    v["mode"].as_str().unwrap_or("?"),
                    rules.len(),
                    if rules.len() == 1 { "" } else { "s" },
                    v["preview_days"]
                );
                if v["mode"].as_str() != Some("automate") {
                    println!(
                        "(armed rules only execute in automate mode: `filemind mode automate`)"
                    );
                }
                for r in &rules {
                    println!();
                    print_rule(
                        &r["rule"],
                        r["describe"].as_str().unwrap_or(""),
                        &r["would_have"],
                    );
                }
                if rules.is_empty() {
                    println!("  (none — `filemind rule kinds`, then `filemind rule add <kind>`)");
                }
            }
            Some(RuleCmd::Kinds) => {
                for k in rpc(adapter.as_ref(), "automate.kinds", json!({}))?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    println!("{}", k["kind"].as_str().unwrap_or("?"));
                    println!("    {}", k["describe"].as_str().unwrap_or(""));
                    println!("    defaults: {}", k["defaults"]);
                }
            }
            Some(RuleCmd::Add { kind, set }) => {
                let mut params = serde_json::Map::new();
                for kv in set {
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| anyhow::anyhow!("--set wants KEY=VALUE, got {kv:?}"))?;
                    let v: Value = serde_json::from_str(v).unwrap_or(Value::String(v.to_string()));
                    params.insert(k.to_string(), v);
                }
                let v = rpc(
                    adapter.as_ref(),
                    "automate.add",
                    json!({"kind": kind, "params": params}),
                )?;
                let r = &v["rule"];
                println!("rule #{} created in preview state ({} days). It will run nothing until you arm it.", r["rule_id"], rpc(adapter.as_ref(), "automate.list", json!({}))?["preview_days"]);
                println!("params: {}", r["params"]);
                let e = &v["evaluation"];
                println!(
                    "right now it would touch {} file{} ({}){}",
                    e["steps"],
                    if e["steps"].as_u64() == Some(1) {
                        ""
                    } else {
                        "s"
                    },
                    human_bytes(e["bytes"].as_u64().unwrap_or(0)),
                    if e["capped"].as_bool() == Some(true) {
                        format!(" — {} qualify, capped per run", e["candidates"])
                    } else {
                        String::new()
                    }
                );
                if let Some(d) = e["diff"].as_str().filter(|d| !d.trim().is_empty()) {
                    println!("{d}");
                }
            }
            Some(RuleCmd::Preview { id }) => {
                let v = rpc(adapter.as_ref(), "automate.preview", json!({"id": id}))?;
                print_rule(&v["rule"], "", &v["would_have"]);
                let e = &v["now"];
                println!();
                println!(
                    "right now: {} file{} ({}){}",
                    e["steps"],
                    if e["steps"].as_u64() == Some(1) {
                        ""
                    } else {
                        "s"
                    },
                    human_bytes(e["bytes"].as_u64().unwrap_or(0)),
                    if e["capped"].as_bool() == Some(true) {
                        format!(" — {} qualify, capped per run", e["candidates"])
                    } else {
                        String::new()
                    }
                );
                for k in v["keeps"].as_array().cloned().unwrap_or_default() {
                    println!("  KEEP   {}", k.as_str().unwrap_or(""));
                }
                if let Some(d) = e["diff"].as_str().filter(|d| !d.trim().is_empty()) {
                    println!("{d}");
                }
                for p in e["problems"].as_array().cloned().unwrap_or_default() {
                    println!("  ! {}", p.as_str().unwrap_or(""));
                }
                let w = &v["would_have"];
                if let Some(files) = w["files"].as_array().filter(|f| !f.is_empty()) {
                    println!();
                    println!("during the preview it would have touched:");
                    for f in files.iter().take(200) {
                        println!("  {}", f.as_str().unwrap_or(""));
                    }
                    if files.len() > 200 {
                        println!("  … and {} more", files.len() - 200);
                    }
                }
            }
            Some(RuleCmd::Arm { id }) => {
                let v = rpc(adapter.as_ref(), "automate.arm", json!({"id": id}))?;
                println!("rule #{} armed. It executes on the agent's schedule while the mode is automate; every run is in `filemind history` and undoable.", v["rule_id"]);
            }
            Some(RuleCmd::Pause { id }) => {
                rpc(
                    adapter.as_ref(),
                    "automate.pause",
                    json!({"id": id, "reason": "paused by user"}),
                )?;
                println!("rule #{id} paused");
            }
            Some(RuleCmd::Rm { id }) => {
                let v = rpc(adapter.as_ref(), "automate.remove", json!({"id": id}))?;
                println!(
                    "{}",
                    if v["removed"].as_bool() == Some(true) {
                        "removed"
                    } else {
                        "no such rule"
                    }
                );
            }
            Some(RuleCmd::Runs { id }) => {
                for r in rpc(
                    adapter.as_ref(),
                    "automate.runs",
                    json!({"id": id, "limit": 50}),
                )?
                .as_array()
                .cloned()
                .unwrap_or_default()
                {
                    let s = &r["summary"];
                    println!(
                        "{}  {:<7}  {} file{} ({}){}{}",
                        chrono::DateTime::from_timestamp(r["ts"].as_i64().unwrap_or(0), 0)
                            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default(),
                        if r["dry_run"].as_bool() == Some(true) {
                            "dry"
                        } else {
                            "RAN"
                        },
                        s["files"],
                        if s["files"].as_u64() == Some(1) {
                            ""
                        } else {
                            "s"
                        },
                        human_bytes(s["bytes"].as_u64().unwrap_or(0)),
                        r["txn_id"]
                            .as_str()
                            .map(|t| format!("  {t}"))
                            .unwrap_or_default(),
                        s["problems"]
                            .as_array()
                            .filter(|p| !p.is_empty())
                            .map(|p| format!("  ! {}", p.len()))
                            .unwrap_or_default()
                    );
                }
            }
            Some(RuleCmd::Set { id, set }) => {
                let cur = rpc(adapter.as_ref(), "automate.preview", json!({"id": id}))?["rule"]
                    ["params"]
                    .clone();
                let mut params = cur.as_object().cloned().unwrap_or_default();
                for kv in set {
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| anyhow::anyhow!("wants KEY=VALUE, got {kv:?}"))?;
                    params.insert(
                        k.to_string(),
                        serde_json::from_str(v).unwrap_or(Value::String(v.to_string())),
                    );
                }
                let r = rpc(
                    adapter.as_ref(),
                    "automate.set_params",
                    json!({"id": id, "params": params}),
                )?;
                println!("rule #{id} params: {}", r["params"]);
            }
            Some(RuleCmd::Tick) => {
                for o in rpc(adapter.as_ref(), "automate.tick", json!({}))?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    println!(
                        "rule #{}: {} {} file{}{}{}",
                        o["rule_id"],
                        if o["dry_run"].as_bool() == Some(true) {
                            "would touch"
                        } else {
                            "ran on"
                        },
                        o["files"],
                        if o["files"].as_u64() == Some(1) {
                            ""
                        } else {
                            "s"
                        },
                        o["txn_id"]
                            .as_str()
                            .map(|t| format!("  ({t})"))
                            .unwrap_or_default(),
                        o["paused"]
                            .as_str()
                            .map(|p| format!("  PAUSED: {p}"))
                            .unwrap_or_default()
                    );
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
                let engine = filemind_agent::semantic::Engine::open(&db)?;
                println!(
                    "{}",
                    filemind_agent::scheduler::tick(adapter.as_ref(), &db, &engine, sched)?
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
            DevCmd::DbKey => {
                let path = filemind_storage::default_db_path()?;
                let dir = path.parent().unwrap_or(std::path::Path::new("."));
                println!("{}", filemind_storage::keyring::db_key_hex(dir)?);
                eprintln!(
                    "open with:  sqlcipher '{}'  then  PRAGMA key = \"x'<hex>'\";",
                    path.display()
                );
            }
        },
    }
    Ok(())
}
