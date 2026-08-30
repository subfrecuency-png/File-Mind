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

/// Where the agent binary lives. In a bundle the sidecar sits next to this
/// executable (`FileMind.app/Contents/MacOS/filemind-agent`, the triple
/// suffix stripped by Tauri); `tauri dev` copies it beside the debug
/// binary the same way. `FILEMIND_AGENT_BIN` and the repo target dir are
/// for development.
fn agent_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("FILEMIND_AGENT_BIN") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("filemind-agent")); // sidecar (Contents/MacOS)
            candidates.push(dir.join("../Resources/filemind-agent"));
            // apps/desktop/src-tauri/target/debug/filemind-desktop → repo target
            candidates.push(dir.join("../../../../../target/debug/filemind-agent"));
            candidates.push(dir.join("../../../../../target/release/filemind-agent"));
        }
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

/// The bundled `filemind` CLI, when this is an installed app.
fn cli_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("filemind");
    p.is_file().then_some(p)
}

/// launchd "start at login": the plist is the source of truth. On every
/// launch the app rewrites it when the sidecar path changed (the bundle was
/// moved or updated) so launchd never points at a binary that is gone.
#[cfg(target_os = "macos")]
mod autostart {
    use super::agent_binary;
    use filemind_adapter_macos::launchd;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    pub fn info() -> Value {
        let home = launchd::home().ok();
        let program = home.as_ref().and_then(|h| launchd::installed_program(h));
        json!({
            "supported": true,
            "enabled": program.is_some(),
            "program": program,
            "plist": home.map(|h| launchd::plist_path(&h)),
        })
    }

    pub fn set(enabled: bool) -> Result<Value, String> {
        let home = launchd::home().map_err(|e| e.to_string())?;
        if enabled {
            let agent = agent_binary().ok_or("agent binary not found — this build has no sidecar")?;
            launchd::install(&agent, &home).map_err(|e| e.to_string())?;
        } else {
            launchd::uninstall(&home).map_err(|e| e.to_string())?;
        }
        Ok(info())
    }

    /// Called at app start: keep an existing login item pointing at us.
    pub fn refresh() {
        let Ok(home) = launchd::home() else { return };
        let Some(current) = launchd::installed_program(&home) else { return };
        let Some(agent) = agent_binary() else { return };
        if current != agent || !current.is_file() {
            if let Err(e) = launchd::install(&agent, &home) {
                eprintln!("launchd refresh failed: {e}");
            }
        }
    }

    pub fn managed() -> bool {
        launchd::home()
            .map(|h| launchd::is_installed(&h))
            .unwrap_or(false)
    }

    pub fn start() -> bool {
        launchd::kickstart(false)
    }

    #[allow(dead_code)]
    pub fn plist() -> Option<PathBuf> {
        launchd::home().ok().map(|h| launchd::plist_path(&h))
    }
}

#[cfg(not(target_os = "macos"))]
mod autostart {
    use serde_json::{json, Value};
    pub fn info() -> Value {
        json!({"supported": false, "enabled": false, "program": null, "plist": null})
    }
    pub fn set(_enabled: bool) -> Result<Value, String> {
        Err("start at login is not available on this platform yet".into())
    }
    pub fn refresh() {}
    pub fn managed() -> bool {
        false
    }
    pub fn start() -> bool {
        false
    }
}

#[tauri::command]
fn autostart_get() -> Value {
    autostart::info()
}

#[tauri::command]
fn autostart_set(enabled: bool) -> Result<Value, String> {
    autostart::set(enabled)
}

/// Drop the in-process context so the next RPC re-opens the database and the
/// semantic engine (after the model download, for instance).
#[tauri::command]
fn local_reset(state: State<'_, AppState>) -> Result<bool, String> {
    let mut guard = state.local.lock().map_err(|_| "state poisoned")?;
    Ok(guard.take().is_some())
}

#[tauri::command]
fn agent_info() -> Value {
    let running = live_agent().is_some();
    let stale = !running && db_path().ok().and_then(|p| Client::connect(&p)).is_some();
    json!({
        "running": running,
        "stale": stale,
        "binary": agent_binary(),
        "cli": cli_binary(),
        "build": filemind_agent::BUILD_ID,
        "launchd": autostart::managed(),
    })
}

#[tauri::command]
fn agent_start() -> Result<Value, String> {
    if live_agent().is_some() {
        return Ok(json!({"started": false, "reason": "already running"}));
    }
    let bin = agent_binary().ok_or("agent binary not found — run `filemind agent start` in a terminal, or set FILEMIND_AGENT_BIN")?;
    // A login item exists: let launchd own the process so it survives the app.
    if autostart::managed() && autostart::start() {
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if live_agent().is_some() {
                return Ok(json!({"started": true, "launchd": true, "binary": bin}));
            }
        }
        return Ok(json!({"started": true, "launchd": true, "binary": bin, "note": "socket not up yet"}));
    }
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

// ---- updater (Phase 9.3) ----------------------------------------------------
//
// A static JSON manifest on GitHub Releases (`latest.json`, written by
// .github/workflows/release.yml) names the newest version and its signed
// bundle. Both commands run in Rust so the window needs no updater
// permissions; the pubkey lives in tauri.conf.json.

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<Value, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(u)) => Ok(json!({
            "available": true,
            "current": u.current_version,
            "version": u.version,
            "date": u.date.map(|d| d.to_string()),
            "notes": u.body,
        })),
        Ok(None) => Ok(json!({"available": false, "current": env!("CARGO_PKG_VERSION")})),
        Err(e) => Err(format!("update check failed: {e}")),
    }
}

/// Download, verify (minisign) and install the update, then relaunch. The
/// agent notices the stale build on the next connection and is restarted by
/// the app (see `agent_start`).
#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> Result<Value, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(u) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(json!({"installed": false, "reason": "already up to date"}));
    };
    u.download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| format!("update failed: {e}"))?;
    // stop a running agent so the relaunched app starts a fresh one
    let _ = agent_stop();
    app.restart()
}

// ---- tray icon (Phase 9.6): health score at a glance --------------------------

/// Current health score, through the same door the window uses.
fn health_score(state: &AppState) -> Option<u64> {
    let v = if let Some(mut a) = live_agent() {
        a.call("health", json!({})).ok()?
    } else {
        let ctx = local_context(state).ok()?;
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "health", "params": {}});
        rpc::handle(&ctx, &req).get("result").cloned()?
    };
    v.get("health")?.get("score")?.as_u64()
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;
    use tauri::Manager;

    let open = MenuItem::with_id(app, "open", "Open FileMind", true, None::<&str>)?;
    let health = MenuItem::with_id(app, "health", "Health: —", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit FileMind", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &health, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("FileMind")
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }
    let tray = builder.build(app)?;

    // refresh the score now and every five minutes
    let handle = app.handle().clone();
    std::thread::Builder::new()
        .name("tray-health".into())
        .spawn(move || loop {
            let score = {
                let state = handle.state::<AppState>();
                health_score(&state)
            };
            let label = score
                .map(|s| format!("Health: {s} / 100"))
                .unwrap_or_else(|| "Health: —".into());
            let _ = health.set_text(&label);
            let _ = tray.set_tooltip(Some(&format!("FileMind — {label}")));
            #[cfg(target_os = "macos")]
            let _ = tray.set_title(score.map(|s| s.to_string()));
            std::thread::sleep(std::time::Duration::from_secs(300));
        })?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    filemind_core::install_quiet_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState::default())
        .setup(|app| {
            autostart::refresh();
            if let Err(e) = setup_tray(app) {
                eprintln!("tray icon unavailable: {e}");
            }
            Ok(())
        })
        // closing the window keeps FileMind in the menu bar; Quit is in the tray menu
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if cfg!(target_os = "macos") {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            rpc,
            agent_info,
            agent_start,
            agent_stop,
            autostart_get,
            autostart_set,
            local_reset,
            update_check,
            update_install
        ])
        .run(tauri::generate_context!())
        .expect("error while running FileMind");
}
