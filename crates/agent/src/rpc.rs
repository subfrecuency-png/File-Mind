//! Local control API: newline-delimited JSON-RPC 2.0 over a Unix domain
//! socket (a named pipe on Windows lands with the Windows adapter work).
//!
//! The socket lives next to the database and is only reachable by the
//! current user. Methods are deliberately small; the CLI and the GUI both
//! use this instead of opening the database themselves while the agent runs.

use crate::pipeline::{self, HashOpts};
use crate::watcher::WatchStats;
use anyhow::Result;
use filemind_core::OsAdapter;
use filemind_storage::Db;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn socket_path(db_path: &Path) -> PathBuf {
    db_path.with_file_name("agent.sock")
}

pub struct Context {
    pub adapter: Arc<dyn OsAdapter>,
    pub db_path: PathBuf,
    /// One writer connection shared by RPC handlers.
    pub db: Mutex<Db>,
    pub stats: Arc<WatchStats>,
    pub started: Instant,
}

fn err(id: Value, code: i64, msg: impl std::fmt::Display) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg.to_string()}})
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

pub fn handle(ctx: &Context, req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match dispatch(ctx, method, &params) {
        Ok(v) => ok(id, v),
        Err(e) => err(id, -32000, e),
    }
}

fn dispatch(ctx: &Context, method: &str, p: &Value) -> Result<Value> {
    let db = ctx
        .db
        .lock()
        .map_err(|_| anyhow::anyhow!("db lock poisoned"))?;
    match method {
        "ping" => Ok(
            json!({"pong": true, "version": env!("CARGO_PKG_VERSION"), "build": crate::BUILD_ID}),
        ),
        "status" => {
            let c = db.counts()?;
            let roots: Vec<Value> = db
                .list_roots()?
                .into_iter()
                .map(|r| json!({"path": r.path, "last_scan": r.last_scan}))
                .collect();
            Ok(json!({
                "platform": ctx.adapter.platform(),
                "database": ctx.db_path,
                "mode": db.get_setting("mode")?.unwrap_or(json!("observe")),
                "uptime_s": ctx.started.elapsed().as_secs(),
                "files": c.files, "dirs": c.dirs, "missing": c.missing, "hashed": c.hashed,
                "bytes": c.bytes, "events": c.events, "transactions": c.transactions,
                "roots": roots,
                "watcher": {
                    "roots": ctx.stats.watching_roots.load(Ordering::Relaxed),
                    "raw_events": ctx.stats.raw_events.load(Ordering::Relaxed),
                    "changes_applied": ctx.stats.changes_applied.load(Ordering::Relaxed),
                    "last_change_unix": ctx.stats.last_change_unix.load(Ordering::Relaxed),
                }
            }))
        }
        "roots.list" => Ok(json!(db
            .list_roots()?
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>())),
        "roots.add" => {
            let path = PathBuf::from(
                p.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("path required"))?,
            );
            let path = path.canonicalize()?;
            let scanner = filemind_core::Scanner::new(ctx.adapter.as_ref(), vec![]);
            scanner.validate_root(&path)?;
            let r = db.add_root(&path)?;
            Ok(json!({"path": r.path, "root_id": r.root_id}))
        }
        "roots.remove" => {
            let path = PathBuf::from(
                p.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("path required"))?,
            );
            let path = path.canonicalize().unwrap_or(path);
            Ok(json!({"removed": db.remove_root(&path)?}))
        }
        "scan" => {
            let targets: Vec<PathBuf> = match p.get("path").and_then(Value::as_str) {
                Some(s) => vec![PathBuf::from(s).canonicalize()?],
                None => db.list_roots()?.into_iter().map(|r| r.path).collect(),
            };
            let mut out = Vec::new();
            for t in targets {
                let o = pipeline::scan_root(ctx.adapter.as_ref(), &db, &t)?;
                out.push(json!({
                    "path": t, "files": o.report.files, "dirs": o.report.dirs, "bytes": o.report.bytes,
                    "new": o.upsert.inserted, "modified": o.upsert.updated, "renamed": o.upsert.renamed,
                    "moved": o.upsert.moved, "missing": o.missing, "unchanged": o.upsert.unchanged,
                    "links": o.report.links_skipped, "ignored": o.report.ignored, "errors": o.report.errors,
                    "elapsed_ms": o.elapsed_ms
                }));
            }
            Ok(json!(out))
        }
        "hash" => {
            let duty = p.get("duty").and_then(Value::as_f64).unwrap_or(0.2) as f32;
            let minutes = p.get("minutes").and_then(Value::as_u64);
            let o = pipeline::hash_pending(
                &db,
                HashOpts {
                    duty_cycle: duty,
                    max_files: 0,
                    max_wall: minutes.map(|m| Duration::from_secs(m * 60)),
                },
            )?;
            Ok(
                json!({"hashed": o.hashed, "bytes": o.bytes, "errors": o.errors, "remaining": o.remaining, "elapsed_ms": o.elapsed_ms}),
            )
        }
        "search" => {
            let q = p
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("query required"))?;
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            Ok(json!(db
                .search_lexical(q, limit)?
                .into_iter()
                .map(|(path, status)| json!({"path": path, "status": status}))
                .collect::<Vec<_>>()))
        }
        "mode.get" => Ok(db.get_setting("mode")?.unwrap_or(json!("observe"))),
        "mode.set" => {
            let m = p
                .get("mode")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("mode required"))?;
            if !["observe", "assist", "automate"].contains(&m) {
                anyhow::bail!("unknown mode {m}");
            }
            db.set_setting("mode", &json!(m))?;
            Ok(json!(m))
        }
        "info" => {
            let path = PathBuf::from(
                p.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("path required"))?,
            );
            let meta = ctx.adapter.native_metadata(&path)?;
            let row = db.file_at_path(&path)?;
            Ok(json!({
                "path": path, "indexed": row.is_some(), "file_id": row.as_ref().map(|r| r.0.clone()),
                "content_type": meta.content_type, "where_from": meta.where_from, "tags": meta.tags, "extra": meta.extra
            }))
        }
        "analyze" => {
            let a = crate::analysis::run(&db)?;
            Ok(json!({
                "duplicate_groups": a.duplicate_groups, "duplicate_bytes": a.duplicate_bytes,
                "version_chains": a.version_chains, "suggestions": a.suggestions,
                "health": a.health, "elapsed_ms": a.elapsed_ms
            }))
        }
        "health" => {
            let h = filemind_core::health::score(&db.health_inputs(None)?);
            let mut roots = Vec::new();
            for r in db.list_roots()? {
                let rh = filemind_core::health::score(&db.health_inputs(Some(r.root_id))?);
                roots.push(json!({"path": r.path, "score": rh.score}));
            }
            let history = db.health_history(None, 30)?;
            Ok(json!({"health": h, "roots": roots, "history": history}))
        }
        "dupes" => {
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let groups = db.rebuild_duplicates()?;
            let wasted: u64 = groups.iter().map(|g| g.size * g.copies.len() as u64).sum();
            Ok(json!({
                "groups": groups.len(), "wasted_bytes": wasted,
                "top": groups.iter().take(limit).map(|g| json!({
                    "size": g.size, "keep": g.keeper, "copies": g.copies
                })).collect::<Vec<_>>()
            }))
        }
        "versions" => {
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let chains = db.rebuild_versions()?;
            Ok(json!({
                "chains": chains.len(),
                "top": chains.iter().take(limit).map(|c| json!({"keep": c.canonical, "older": c.older})).collect::<Vec<_>>()
            }))
        }
        "suggest.list" => {
            let state = p.get("state").and_then(Value::as_str).unwrap_or("proposed");
            let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
            let (count, bytes) = db.suggestion_totals()?;
            Ok(
                json!({"proposed": count, "est_bytes": bytes, "items": db.list_suggestions(state, limit)?}),
            )
        }
        "suggest.dismiss" => {
            let id = p
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("id required"))?;
            Ok(json!({"ok": db.set_suggestion_state(id, "dismissed")?}))
        }
        "classify.run" => {
            let duty = p.get("duty").and_then(Value::as_f64).unwrap_or(0.2) as f32;
            let minutes = p.get("minutes").and_then(Value::as_u64);
            let names_only = p
                .get("names_only")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let o = crate::classifier::classify_pending(
                &db,
                crate::classifier::ClassifyOpts {
                    duty_cycle: duty,
                    max_wall: minutes.map(|m| Duration::from_secs(m * 60)),
                    names_only,
                },
            )?;
            Ok(
                json!({"classified": o.classified, "extracted": o.extracted, "sensitive": o.sensitive,
                      "remaining": o.remaining, "elapsed_ms": o.elapsed_ms}),
            )
        }
        "classify.show" => {
            let path = PathBuf::from(
                p.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("path required"))?,
            );
            match db.classification_of(&path)? {
                Some((c, sens)) => Ok(
                    json!({"path": path, "indexed": true, "classification": c, "sensitive": sens}),
                ),
                None => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let rules: Vec<_> = db.list_rules()?.into_iter().map(|(_, r)| r).collect();
                    let (c, sens, _) = crate::classifier::classify_path(&path, size, &rules, false);
                    Ok(
                        json!({"path": path, "indexed": false, "classification": c, "sensitive": sens}),
                    )
                }
            }
        }
        "classify.set" => {
            let path = PathBuf::from(
                p.get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("path required"))?,
            );
            let cat = p
                .get("category")
                .and_then(Value::as_str)
                .and_then(filemind_core::model::Category::parse)
                .ok_or_else(|| anyhow::anyhow!("category required (document, invoice, contract, photo, screenshot, design, code, archive, installer, media, data, other)"))?;
            let scope = p.get("scope").and_then(Value::as_str).unwrap_or("file");
            let mut rule_id = None;
            match scope {
                "folder" => {
                    let prefix = path
                        .parent()
                        .map(|d| d.to_string_lossy().to_string())
                        .unwrap_or_default();
                    rule_id =
                        Some(db.add_rule(&filemind_core::classify::UserRule::PathPrefix {
                            prefix,
                            category: cat,
                        })?);
                }
                "ext" => {
                    let ext = path
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    rule_id = Some(db.add_rule(&filemind_core::classify::UserRule::Ext {
                        ext,
                        category: cat,
                    })?);
                }
                "name" => {
                    let token = p
                        .get("token")
                        .and_then(Value::as_str)
                        .map(|t| t.to_lowercase())
                        .ok_or_else(|| anyhow::anyhow!("token required for scope=name"))?;
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
            Ok(json!({"ok": ok || rule_id.is_some(), "rule_id": rule_id}))
        }
        "categories" => Ok(json!({
            "categories": db.category_counts()?.into_iter().map(|(c, n, b)| json!({"category": c, "files": n, "bytes": b})).collect::<Vec<_>>(),
            "sensitive": db.sensitive_count()?,
            "pending": db.count_classify_pending()?,
            "rules": db.list_rules()?.into_iter().map(|(id, r)| json!({"id": id, "rule": format!("{r:?}")})).collect::<Vec<_>>()
        })),
        "rules.remove" => {
            let id = p
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("id required"))?;
            Ok(json!({"ok": db.remove_rule(id)?}))
        }
        other => anyhow::bail!("unknown method {other}"),
    }
}

#[cfg(unix)]
pub fn serve(ctx: Arc<Context>, stop: Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    use std::os::unix::net::UnixListener;
    let sock = socket_path(&ctx.db_path);
    let _ = std::fs::remove_file(&sock); // stale socket from a previous run (our own file, not user data)
    let listener = UnixListener::bind(&sock)?;
    listener.set_nonblocking(true)?;
    tracing::info!(socket = %sock.display(), "rpc listening");
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut stream = stream;
                    let mut line = String::new();
                    while let Ok(n) = reader.read_line(&mut line) {
                        if n == 0 {
                            break;
                        }
                        let resp = match serde_json::from_str::<Value>(&line) {
                            Ok(req) => handle(&ctx, &req),
                            Err(e) => err(Value::Null, -32700, e),
                        };
                        if writeln!(stream, "{resp}").is_err() {
                            break;
                        }
                        line.clear();
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
    }
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

#[cfg(not(unix))]
pub fn serve(_ctx: Arc<Context>, _stop: Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    tracing::warn!("rpc server not available on this platform yet");
    Ok(())
}

/// Client side: one request, one response. `None` if no agent is listening.
pub struct Client {
    #[cfg(unix)]
    stream: std::os::unix::net::UnixStream,
}

impl Client {
    #[cfg(unix)]
    pub fn connect(db_path: &Path) -> Option<Self> {
        let s = std::os::unix::net::UnixStream::connect(socket_path(db_path)).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(600))).ok()?;
        Some(Self { stream: s })
    }

    #[cfg(not(unix))]
    pub fn connect(_db_path: &Path) -> Option<Self> {
        None
    }

    #[cfg(unix)]
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let req = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        writeln!(self.stream, "{req}")?;
        let mut reader = BufReader::new(self.stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let v: Value = serde_json::from_str(&line)?;
        if let Some(e) = v.get("error") {
            anyhow::bail!(
                "{}",
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("rpc error")
            );
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    #[cfg(not(unix))]
    pub fn call(&mut self, _method: &str, _params: Value) -> Result<Value> {
        anyhow::bail!("rpc client not available on this platform yet")
    }
}
