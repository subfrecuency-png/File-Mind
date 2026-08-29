//! macOS adapter. The enumeration/metadata parts are plain POSIX and also
//! compile on Linux so the workspace can be tested on any Unix CI runner;
//! FSEvents, Spotlight, Trash and launchd integration are macOS-only and
//! land in Phases 1–2 and 6.

#![cfg(unix)]

mod notify_bridge;
pub mod spotlight;
pub mod trash;

use chrono::{DateTime, TimeZone, Utc};
use filemind_core::adapter::*;
use filemind_core::model::{EntryKind, FileId};
use filemind_core::{CoreError, Result};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Default)]
pub struct MacAdapter;

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

fn entry_from_meta(path: &Path, md: &std::fs::Metadata, depth: usize) -> Entry {
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Link
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Entry {
        path: path.to_path_buf(),
        file_id: FileId {
            device: md.dev(),
            index: md.ino(),
        },
        kind,
        size: md.len(),
        mtime: ts(md.mtime()),
        ctime: Some(ts(md.ctime())),
        birthtime: md.created().ok().map(DateTime::<Utc>::from),
        depth,
    }
}

impl OsAdapter for MacAdapter {
    fn platform(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "posix"
        }
    }

    fn watch(&self, roots: &[PathBuf], tx: Sender<FsEvent>) -> Result<Box<dyn WatchHandle>> {
        notify_bridge::watch(roots, tx)
    }

    fn stat(&self, path: &Path) -> Result<Option<Entry>> {
        let md = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(entry_from_meta(path, &md, 0)))
    }

    fn enumerate(
        &self,
        root: &Path,
        opts: &ScanOptsNative,
    ) -> Result<Box<dyn Iterator<Item = Result<Entry>> + Send>> {
        let it = walkdir::WalkDir::new(root)
            .min_depth(1)
            .max_depth(opts.max_depth)
            .follow_links(opts.follow_links)
            .into_iter()
            .map(|r| {
                let d = r.map_err(|e| CoreError::Other(e.into()))?;
                let md = std::fs::symlink_metadata(d.path())?;
                Ok(entry_from_meta(d.path(), &md, d.depth()))
            });
        Ok(Box::new(it))
    }

    fn native_metadata(&self, path: &Path) -> Result<NativeMeta> {
        spotlight::mdls(path)
    }

    fn native_search(&self, query: &str) -> Result<Vec<PathBuf>> {
        spotlight::mdfind(query)
    }

    fn move_to_trash(&self, path: &Path) -> Result<TrashReceipt> {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no home directory")))?;
        trash::move_to_trash(&home, path)
    }

    fn trash_target(&self, path: &Path) -> Result<PathBuf> {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no home directory")))?;
        trash::trash_target(&home, path)
    }

    fn move_to_trash_at(&self, path: &Path, target: &Path) -> Result<TrashReceipt> {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no home directory")))?;
        trash::move_to_trash_at(&home, path, target)
    }

    fn rename_no_clobber(&self, from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(CoreError::DestinationExists(to.to_path_buf()));
        }
        // Phase 6: use renameat2/RENAME_NOREPLACE semantics where available.
        std::fs::rename(from, to)?;
        Ok(())
    }

    fn protected_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = [
            "/System",
            "/Library",
            "/usr",
            "/bin",
            "/sbin",
            "/private/etc",
            "/private/var/db",
            "/Applications",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            v.push(home.join("Library"));
            v.push(home.join(".Trash"));
        }
        v
    }

    fn register_autostart(&self, enable: bool) -> Result<()> {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no home directory")))?;
        let plist = launchd::plist_path(&home);
        if enable {
            let agent = launchd::agent_binary()?;
            let log_dir = home.join("Library/Logs/FileMind");
            std::fs::create_dir_all(&log_dir)?;
            std::fs::create_dir_all(plist.parent().unwrap())?;
            std::fs::write(&plist, launchd::plist(&agent, &log_dir))?;
            launchd::launchctl(&["bootout", &launchd::domain(), &plist.to_string_lossy()]); // ignore failure
            launchd::launchctl(&["bootstrap", &launchd::domain(), &plist.to_string_lossy()]);
            tracing::info!(plist = %plist.display(), agent = %agent.display(), "launch agent installed");
        } else {
            launchd::launchctl(&["bootout", &launchd::domain(), &plist.to_string_lossy()]);
            if plist.exists() {
                // The plist is FileMind's own file, not user data; removing it is the uninstall.
                std::fs::rename(&plist, plist.with_extension("plist.removed"))?;
            }
            tracing::info!("launch agent removed");
        }
        Ok(())
    }

    fn link_kind(&self, path: &Path) -> Result<LinkKind> {
        let md = std::fs::symlink_metadata(path)?;
        Ok(if md.file_type().is_symlink() {
            LinkKind::Symlink
        } else {
            LinkKind::NotALink
        })
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        let Some(u) = directories::UserDirs::new() else {
            return vec![];
        };
        [
            u.desktop_dir(),
            u.document_dir(),
            u.download_dir(),
            u.picture_dir(),
        ]
        .into_iter()
        .flatten()
        .map(Path::to_path_buf)
        .collect()
    }
}

/// launchd integration. Only `launchctl` calls are macOS-specific; the plist
/// is written on any Unix so it can be inspected in tests.
pub mod launchd {
    use filemind_core::{CoreError, Result};
    use std::path::{Path, PathBuf};

    pub const LABEL: &str = "ai.filemind.agent";

    pub fn plist_path(home: &Path) -> PathBuf {
        home.join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    pub fn domain() -> String {
        #[cfg(unix)]
        let uid = unsafe { libc_getuid() };
        format!("gui/{uid}")
    }

    #[cfg(unix)]
    unsafe fn libc_getuid() -> u32 {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }

    /// The agent binary next to the current executable.
    pub fn agent_binary() -> Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dir = exe
            .parent()
            .ok_or_else(|| CoreError::Other(anyhow::anyhow!("no exe dir")))?;
        let agent = dir.join("filemind-agent");
        if !agent.exists() {
            return Err(CoreError::Other(anyhow::anyhow!(
                "filemind-agent not found next to {} — run `cargo build --workspace` first",
                exe.display()
            )));
        }
        Ok(agent)
    }

    pub fn plist(agent: &Path, log_dir: &Path) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{agent}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>Nice</key><integer>10</integer>
  <key>StandardOutPath</key><string>{log}/agent.log</string>
  <key>StandardErrorPath</key><string>{log}/agent.err.log</string>
  <key>EnvironmentVariables</key><dict><key>RUST_LOG</key><string>info</string></dict>
</dict>
</plist>
"#,
            agent = agent.display(),
            log = log_dir.display()
        )
    }

    pub fn launchctl(args: &[&str]) {
        if !cfg!(target_os = "macos") {
            return;
        }
        match std::process::Command::new("launchctl").args(args).output() {
            Ok(o) if !o.status.success() => {
                tracing::debug!(args = ?args, stderr = %String::from_utf8_lossy(&o.stderr), "launchctl")
            }
            Err(e) => tracing::warn!(error = %e, "launchctl not runnable"),
            _ => {}
        }
    }
}
