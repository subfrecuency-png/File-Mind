//! Local crash reports (Phase 10). A panic writes one plain-text file to
//! `<data dir>/crashes/`; nothing is sent anywhere unless the user attaches
//! it to feedback. Paths under the home folder are scrubbed to `~`.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn dir() -> Option<PathBuf> {
    filemind_storage::default_db_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("crashes")))
}

/// Replace the home directory in `s` with `~`.
pub fn scrub(s: &str) -> String {
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    match home {
        Some(h) => s.replace(&h.to_string_lossy().to_string(), "~"),
        None => s.to_string(),
    }
}

/// Install a panic hook for `component` (agent | desktop | cli). Prints the
/// quiet one-liner the old hook printed, and writes a report file.
pub fn install(component: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let short: String = msg.chars().take(160).collect();
        eprintln!("warning: internal panic caught at {loc}: {short}");
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        let report = format!(
            "FileMind crash report\ncomponent: {component}\nversion: {}\nbuild: {}\nos: {} {}\nwhen: {}\nlocation: {}\nmessage: {}\n\nbacktrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            crate::BUILD_ID,
            std::env::consts::OS,
            std::env::consts::ARCH,
            chrono::Utc::now().to_rfc3339(),
            scrub(&loc),
            scrub(&msg),
            scrub(&bt)
        );
        if let Some(d) = dir() {
            let _ = std::fs::create_dir_all(&d);
            let name = format!(
                "{}-{component}.txt",
                chrono::Utc::now().format("%Y%m%dT%H%M%S")
            );
            let _ = std::fs::write(d.join(name), report);
        }
    }));
}

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub name: String,
    pub bytes: u64,
    pub ts: i64,
    pub component: String,
    pub message: String,
}

/// Pending (not yet dismissed or sent) reports, newest first.
pub fn list() -> Result<Vec<Report>> {
    let Some(d) = dir() else { return Ok(vec![]) };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("txt") {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let md = e.metadata()?;
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let field = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default()
            };
            out.push(Report {
                name,
                bytes: md.len(),
                ts: md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                component: field("component:"),
                message: field("message:"),
            });
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.ts));
    Ok(out)
}

fn path_for(name: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !name.contains('/') && !name.contains('\\') && name.ends_with(".txt"),
        "bad report name"
    );
    Ok(dir()
        .ok_or_else(|| anyhow::anyhow!("no data dir"))?
        .join(name))
}

pub fn read(name: &str) -> Result<String> {
    Ok(std::fs::read_to_string(path_for(name)?)?)
}

/// Rename the report out of the pending list (`.txt` → `.txt.<state>`).
/// Never deleted.
pub fn settle(name: &str, state: &str) -> Result<()> {
    let p = path_for(name)?;
    anyhow::ensure!(matches!(state, "sent" | "dismissed"), "bad state");
    std::fs::rename(&p, Path::new(&format!("{}.{state}", p.display())))?;
    Ok(())
}
