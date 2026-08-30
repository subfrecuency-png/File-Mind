//! FileMind desktop: a thin window over the same JSON-RPC the CLI uses.
//!
//! Every screen calls `rpc(method, params)`. When the agent is running (and
//! is the same build) the call is forwarded over its socket, so the window
//! sees live watcher state. Otherwise the request is handled in-process by
//! the agent crate's own handler against the database — search, projects,
//! suggestions, transactions and undo all work without the daemon; only the
//! live watcher needs it.

use filemind_agent::rpc::{self, Client, Context};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::State;

#[derive(Default)]
pub struct AppState {
    local: Mutex<Option<Arc<Context>>>,
}

fn db_path() -> Result<PathBuf, String> {
    filemind_storage::default_db_path().map_err(|e| e.to_string())
}

/// A connection to a running agent of the same build, if any.
fn live_agent() -> Option<Client> {
    let path = db_path().ok()?;
    let mut c = Client::connect(&path)?;
    let v = c.call("ping", json!({})).ok()?;
    let build = v.get("build").and_then(Value::as_str).unwrap_or("");
    if build != filemind_agent::BUILD_ID {
        return None;
    }
    Some(c)
}

fn local_context(state: &AppState) -> Result<Arc<Context>, String> {
    let mut guard = state.local.lock().map_err(|_| "state poisoned")?;
    if let Some(c) = guard.as_ref() {
        return Ok(c.clone());
    }
    let db_path = db_path()?;
    let adapter: Arc<dyn filemind_core::OsAdapter> =
        Arc::from(filemind_agent::platform::adapter());
    let db = filemind_storage::Db::open(&db_path).map_err(|e| e.to_string())?;
    // settle anything a crashed process left behind, exactly as the agent does
    let _ = filemind_agent::actions::recover_all(adapter.as_ref(), &db);
    let engine = filemind_agent::semantic::Engine::open(&db).map_err(|e| e.to_string())?;
    let ctx = Arc::new(Context {
        adapter,
        db_path,
        db: Mutex::new(db),
        engine: Arc::new(engine),
        stats: Arc::new(filemind_agent::watcher::WatchStats::default()),
        started: Instant::now(),
    });
    *guard = Some(ctx.clone());
    Ok(ctx)
}

#[tauri::command]
fn rpc(state: State<'_, AppState>, method: String, params: Value) -> Result<Value, String> {
    if let Some(mut a) = live_agent() {
        return a.call(&method, params).map_err(|e| format!("{e:#}"));
    }
    let ctx = local_context(&state)?;
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let resp = rpc::handle(&ctx, &req);
    if let Some(e) = resp.get("error") {
        return Err(e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("rpc error")
            .to_string());
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

/// Where the agent binary lives: next to this executable (bundled), in the
/// app's Resources folder, `FILEMIND_AGENT_BIN`, or the dev target dir.
fn agent_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("FILEMIND_AGENT_BIN") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("filemind-agent"));
            candidates.push(dir.join("../Resources/filemind-agent"));
            // apps/desktop/src-tauri/target/debug/filemind-desktop → repo target
            candidates.push(dir.join("../../../../../target/debug/filemind-agent"));
            candidates.push(dir.join("../../../../../target/release/filemind-agent"));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[tauri::command]
fn agent_info() -> Value {
    let running = live_agent().is_some();
    let stale = !running && db_path().ok().and_then(|p| Client::connect(&p)).is_some();
    json!({
        "running": running,
        "stale": stale,
        "binary": agent_binary(),
        "build": filemind_agent::BUILD_ID,
    })
}

#[tauri::command]
fn agent_start() -> Result<Value, String> {
    if live_agent().is_some() {
        return Ok(json!({"started": false, "reason": "already running"}));
    }
    let bin = agent_binary().ok_or("agent binary not found — run `filemind agent start` in a terminal, or set FILEMIND_AGENT_BIN")?;
    let log_dir = db_path()?
        .parent()
        .map(|p| p.join("logs"))
        .ok_or("no data dir")?;
    std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_dir.join("agent.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = std::process::Command::new(&bin)
        .stdout(log)
        .stderr(err)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    // give it a moment to bind the socket
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if live_agent().is_some() {
            return Ok(json!({"started": true, "pid": child.id(), "binary": bin}));
        }
    }
    Ok(json!({"started": true, "pid": child.id(), "binary": bin, "note": "socket not up yet"}))
}

#[tauri::command]
fn agent_stop() -> Result<bool, String> {
    if live_agent().is_none() && agent_info()["stale"].as_bool() != Some(true) {
        return Ok(false);
    }
    let stop = db_path()?.with_file_name("agent.stop");
    std::fs::write(&stop, b"").map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    filemind_core::install_quiet_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            rpc,
            agent_info,
            agent_start,
            agent_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running FileMind");
}
