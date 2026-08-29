//! Project detection: turn a flat inventory into the units people actually
//! think in ("the Calcium offer", "the laser-cutter job", "the site repo").
//!
//! Three detectors, applied in order so every file lands in at most one project:
//!
//! 1. **Marker folders** — a folder containing a build/workspace marker
//!    (`.git`, `package.json`, `Cargo.toml`, `*.xcodeproj`, `*.prproj` …) is a
//!    project; its whole subtree belongs to it.
//! 2. **Top-level folders** — inside each root, a direct child folder with at
//!    least [`MIN_FILES`] files is a project (Downloads/Client X, Documents/Taxes 2025).
//! 3. **Sessions of loose files** — files sitting directly in a root (or in a
//!    folder that was not claimed) are clustered by *when* they were touched:
//!    a run of files whose mtimes are within [`SESSION_GAP`] of each other, with
//!    at least [`MIN_FILES`] members, is one working session. Files sharing a
//!    distinctive name token across sessions are merged into a topic.
//!
//! Everything is pure over an in-memory list so it can be unit-tested without
//! a database.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub const MIN_FILES: usize = 5;
pub const SESSION_GAP: i64 = 30 * 60;
pub const ACTIVE_DAYS: i64 = 30;
pub const DORMANT_DAYS: i64 = 180;

const MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "CMakeLists.txt",
    "Podfile",
    "Package.swift",
    ".sln",
];
const MARKER_EXTS: &[&str] = &[
    "xcodeproj",
    "xcworkspace",
    "prproj",
    "aep",
    "drp",
    "blend",
    "uproject",
    "lbrn2",
    "fig",
    "sketch",
];
const SKIP_DIRS: &[&str] = crate::scanner::NOISE_DIRS;
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "from",
    "copy",
    "final",
    "draft",
    "new",
    "old",
    "file",
    "files",
    "doc",
    "docs",
    "pdf",
    "png",
    "jpg",
    "image",
    "img",
    "screenshot",
    "screen",
    "shot",
    "untitled",
    "document",
    "download",
    "downloads",
    "desktop",
    "documents",
    "sheet",
    "export",
    "scan",
    "version",
    "ver",
    "rev",
];

#[derive(Debug, Clone)]
pub struct FileIn {
    pub file_id: String,
    pub path: PathBuf,
    pub mtime: i64,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Marker,
    Folder,
    Session,
    Topic,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Marker => "marker",
            ProjectKind::Folder => "folder",
            ProjectKind::Session => "session",
            ProjectKind::Topic => "topic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    /// Stable identity across rebuilds: folder path, or `session:<root>:<start>`, `topic:<root>:<token>`.
    pub key: String,
    pub kind: ProjectKind,
    pub suggested_name: String,
    pub root_path: PathBuf,
    pub files: Vec<String>,
    pub bytes: u64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub activity_score: f32,
}

fn is_marker_dir(dir: &Path, children: &[&Path]) -> bool {
    children.iter().any(|c| {
        let name = c
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        MARKERS.contains(&name.as_str())
            || c.extension()
                .map(|e| MARKER_EXTS.contains(&e.to_string_lossy().as_ref()))
                .unwrap_or(false)
    }) || dir
        .extension()
        .map(|e| MARKER_EXTS.contains(&e.to_string_lossy().as_ref()))
        .unwrap_or(false)
}

fn in_skipped(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .map(|rel| {
            rel.components()
                .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
        })
        .unwrap_or(false)
}

/// "client-x_offer sheets" → "Client X Offer Sheets"
pub fn pretty_name(raw: &str) -> String {
    raw.replace(['_', '-'], " ")
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) if w.chars().all(|ch| ch.is_lowercase() || !ch.is_alphabetic()) => {
                    f.to_uppercase().collect::<String>() + c.as_str()
                }
                _ => w.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokens(name: &str) -> Vec<String> {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for t in stem.split(|c: char| !c.is_alphanumeric()).filter(|t| {
        t.len() >= 3
            && !t.chars().all(|c| c.is_ascii_digit())
            && !looks_like_id(t)
            && !STOPWORDS.contains(t)
    }) {
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Hex hashes, UUID fragments, base-36 ids: ≥ 8 chars, all hex, with digits in them.
fn looks_like_id(t: &str) -> bool {
    t.len() >= 8
        && t.chars().all(|c| c.is_ascii_hexdigit())
        && t.chars().any(|c| c.is_ascii_digit())
}

/// Longest run of leading words shared by at least 60 % of the names
/// ("OFFER SHEET Calcium", "OFFER SHEET Versa" → "OFFER SHEET").
fn common_prefix(names: &[String]) -> Option<String> {
    let words: Vec<Vec<String>> = names
        .iter()
        .map(|n| {
            Path::new(n)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
                .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
                .filter(|w| !w.is_empty())
                .map(String::from)
                .collect()
        })
        .collect();
    let need = (names.len() * 6).div_ceil(10).max(2);
    let mut prefix: Vec<String> = Vec::new();
    loop {
        let k = prefix.len();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for w in &words {
            if w.len() > k
                && w[..k]
                    .iter()
                    .map(|x| x.to_lowercase())
                    .eq(prefix.iter().map(|x| x.to_lowercase()))
            {
                *counts.entry(w[k].to_lowercase()).or_default() += 1;
            }
        }
        match counts
            .into_iter()
            .filter(|(_, n)| *n >= need)
            .max_by_key(|(_, n)| *n)
        {
            Some((w, _)) if !w.chars().all(|c| c.is_ascii_digit()) && prefix.len() < 4 => {
                prefix.push(w)
            }
            _ => break,
        }
    }
    if prefix.is_empty() || prefix.iter().all(|w| STOPWORDS.contains(&w.as_str())) {
        None
    } else {
        Some(prefix.join(" "))
    }
}

fn activity(files: &[&FileIn], now: i64) -> f32 {
    let recent = files
        .iter()
        .filter(|f| now - f.mtime <= ACTIVE_DAYS * 86_400)
        .count();
    let mid = files
        .iter()
        .filter(|f| now - f.mtime <= DORMANT_DAYS * 86_400)
        .count();
    (recent as f32 * 1.0 + mid as f32 * 0.25) / files.len().max(1) as f32
}

fn build(
    key: String,
    kind: ProjectKind,
    name: String,
    root: &Path,
    members: Vec<&FileIn>,
    now: i64,
) -> Project {
    let start_ts = members.iter().map(|f| f.mtime).min().unwrap_or(0);
    let end_ts = members.iter().map(|f| f.mtime).max().unwrap_or(0);
    Project {
        key,
        kind,
        suggested_name: name,
        root_path: root.to_path_buf(),
        bytes: members.iter().map(|f| f.size).sum(),
        activity_score: activity(&members, now),
        files: members.iter().map(|f| f.file_id.clone()).collect(),
        start_ts,
        end_ts,
    }
}

/// Detect projects for one root. `files` must all live under `root`.
pub fn detect(root: &Path, files: &[FileIn], now: i64) -> Vec<Project> {
    let mut out = Vec::new();
    let mut claimed: Vec<bool> = vec![false; files.len()];

    // children names per dir (files + subdirs) for marker detection
    let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for f in files {
        let mut cur = f.path.clone();
        while let Some(parent) = cur.parent().map(Path::to_path_buf) {
            if !parent.starts_with(root) || parent == *root && cur == *root {
                break;
            }
            let v = children.entry(parent.clone()).or_default();
            if !v.contains(&cur) {
                v.push(cur.clone());
            }
            if parent == *root {
                break;
            }
            cur = parent;
        }
    }

    // 1. marker folders: shallowest first so nested repos inside a repo do not split it
    let mut marker_dirs: Vec<PathBuf> = children
        .iter()
        .filter(|(d, kids)| {
            **d != *root
                && !in_skipped(d, root)
                && is_marker_dir(d, &kids.iter().map(|p| p.as_path()).collect::<Vec<_>>())
        })
        .map(|(d, _)| d.clone())
        .collect();
    marker_dirs.sort_by_key(|d| d.components().count());
    let mut claimed_dirs: Vec<PathBuf> = Vec::new();
    for d in marker_dirs {
        if claimed_dirs.iter().any(|c| d.starts_with(c)) {
            continue;
        }
        // claim the whole subtree, but generated/dependency files are not members
        let mut idxs: Vec<usize> = Vec::new();
        for (i, f) in files.iter().enumerate() {
            if !claimed[i] && f.path.starts_with(&d) {
                claimed[i] = true;
                if !in_skipped(&f.path, root) {
                    idxs.push(i);
                }
            }
        }
        if idxs.is_empty() {
            continue;
        }
        let members: Vec<&FileIn> = idxs.iter().map(|&i| &files[i]).collect();
        let name = pretty_name(
            &d.file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        out.push(build(
            d.to_string_lossy().to_string(),
            ProjectKind::Marker,
            name,
            root,
            members,
            now,
        ));
        claimed_dirs.push(d);
    }

    // 2. top-level folders with enough files. A folder that merely *holds*
    //    several project-sized folders (Clients/, Projects/) is a container:
    //    descend into it instead of making it one giant project.
    let subdirs_of = |d: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = children
            .get(d)
            .map(|kids| {
                kids.iter()
                    .filter(|k| children.contains_key(*k))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let mut queue: std::collections::VecDeque<PathBuf> = subdirs_of(root).into_iter().collect();
    while let Some(d) = queue.pop_front() {
        if claimed_dirs
            .iter()
            .any(|c| d.starts_with(c) || c.starts_with(&d))
            || in_skipped(&d, root)
        {
            continue;
        }
        let subs = subdirs_of(&d);
        let big_subs = subs
            .iter()
            .filter(|sd| {
                files
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| !claimed[*i] && f.path.starts_with(sd))
                    .count()
                    >= MIN_FILES
            })
            .count();
        let direct_files = files
            .iter()
            .enumerate()
            .filter(|(i, f)| !claimed[*i] && f.path.parent() == Some(d.as_path()))
            .count();
        if big_subs >= 3 && direct_files < MIN_FILES {
            queue.extend(subs);
            continue;
        }
        let members: Vec<&FileIn> = files
            .iter()
            .enumerate()
            .filter(|(i, f)| !claimed[*i] && f.path.starts_with(&d))
            .map(|(_, f)| f)
            .collect();
        if members.len() < MIN_FILES {
            continue;
        }
        for (i, f) in files.iter().enumerate() {
            if !claimed[i] && f.path.starts_with(&d) {
                claimed[i] = true;
            }
        }
        let name = pretty_name(
            &d.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        out.push(build(
            d.to_string_lossy().to_string(),
            ProjectKind::Folder,
            name,
            root,
            members,
            now,
        ));
        claimed_dirs.push(d);
    }

    // 3. sessions of loose files (unclaimed), by mtime proximity
    let mut loose: Vec<usize> = (0..files.len())
        .filter(|i| !claimed[*i] && !in_skipped(&files[*i].path, root))
        .collect();
    loose.sort_by_key(|i| files[*i].mtime);
    let mut sessions: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for i in loose {
        if let Some(&last) = cur.last() {
            if files[i].mtime - files[last].mtime > SESSION_GAP {
                sessions.push(std::mem::take(&mut cur));
            }
        }
        cur.push(i);
    }
    if !cur.is_empty() {
        sessions.push(cur);
    }

    // topic merge: files that carry a session's dominant token collapse into
    // one topic across sessions; the rest of the session stays a session.
    let mut by_token: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut plain_sessions: Vec<Vec<usize>> = Vec::new();
    for s in sessions.into_iter().filter(|s| s.len() >= MIN_FILES) {
        let toks: Vec<Vec<String>> = s
            .iter()
            .map(|&i| {
                tokens(
                    &files[i]
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for t in toks.iter().flatten() {
            *counts.entry(t.as_str()).or_default() += 1;
        }
        let dominant = counts
            .iter()
            .filter(|(_, n)| **n * 2 >= s.len())
            .max_by_key(|(t, n)| (**n, t.len()))
            .map(|(t, _)| t.to_string());
        match dominant {
            Some(t) => {
                let (with, without): (Vec<usize>, Vec<usize>) = s.iter().zip(toks.iter()).fold(
                    (Vec::new(), Vec::new()),
                    |(mut w, mut wo), (&i, tk)| {
                        if tk.contains(&t) {
                            w.push(i)
                        } else {
                            wo.push(i)
                        }
                        (w, wo)
                    },
                );
                by_token.entry(t).or_default().extend(with);
                if without.len() >= MIN_FILES {
                    plain_sessions.push(without);
                }
            }
            None => plain_sessions.push(s),
        }
    }
    for (token, idxs) in by_token {
        let members: Vec<&FileIn> = idxs.iter().map(|&i| &files[i]).collect();
        for &i in &idxs {
            claimed[i] = true;
        }
        let names: Vec<String> = members
            .iter()
            .map(|f| {
                f.path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let name = pretty_name(&common_prefix(&names).unwrap_or_else(|| token.clone()));
        out.push(build(
            format!("topic:{}:{}", root.display(), token),
            ProjectKind::Topic,
            name,
            root,
            members,
            now,
        ));
    }
    for s in plain_sessions {
        let members: Vec<&FileIn> = s.iter().map(|&i| &files[i]).collect();
        let start = members.iter().map(|f| f.mtime).min().unwrap_or(0);
        let day = chrono::DateTime::from_timestamp(start, 0)
            .map(|d| d.format("%b %-d, %Y").to_string())
            .unwrap_or_default();
        let name = format!("Working session, {day} ({} files)", members.len());
        out.push(build(
            format!("session:{}:{}", root.display(), start),
            ProjectKind::Session,
            name,
            root,
            members,
            now,
        ));
    }

    out.sort_by(|a, b| {
        b.activity_score
            .partial_cmp(&a.activity_score)
            .unwrap()
            .then(b.files.len().cmp(&a.files.len()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, mtime: i64) -> FileIn {
        FileIn {
            file_id: path.to_string(),
            path: PathBuf::from(path),
            mtime,
            size: 10,
        }
    }

    #[test]
    fn markers_folders_and_sessions() {
        let now = 1_700_000_000;
        let mut files = vec![
            // a repo (marker) with a nested node_modules that must not become its own project
            f("/r/site/package.json", now - 100),
            f("/r/site/src/index.ts", now - 90),
            f("/r/site/node_modules/x/index.js", now - 80),
            // a plain folder with enough files
            f("/r/Taxes 2025/w2.pdf", now - 86_400 * 200),
            f("/r/Taxes 2025/1099.pdf", now - 86_400 * 200),
            f("/r/Taxes 2025/receipts/a.pdf", now - 86_400 * 200),
            f("/r/Taxes 2025/receipts/b.pdf", now - 86_400 * 200),
            f("/r/Taxes 2025/notes.txt", now - 86_400 * 200),
            // a tiny folder: too small, its files become loose
            f("/r/misc/one.txt", now - 5000),
        ];
        // loose files: a topic ("offer" sheets) across two sessions, and an unrelated session
        for i in 0..6 {
            files.push(f(
                &format!("/r/OFFER SHEET client{i}.pdf"),
                now - 86_400 * 3 + i * 60,
            ));
        }
        for i in 0..5 {
            files.push(f(&format!("/r/IMG_00{i}.HEIC"), now - 86_400 * 10 + i * 60));
        }
        let ps = detect(Path::new("/r"), &files, now);
        let names: Vec<(&str, ProjectKind, usize)> = ps
            .iter()
            .map(|p| (p.suggested_name.as_str(), p.kind, p.files.len()))
            .collect();
        assert!(
            names.contains(&("Site", ProjectKind::Marker, 2)),
            "node_modules excluded from members: {names:?}"
        );
        assert!(
            names.contains(&("Taxes 2025", ProjectKind::Folder, 5)),
            "{names:?}"
        );
        assert!(
            names
                .iter()
                .any(|(n, k, c)| *n == "Offer Sheet" && *k == ProjectKind::Topic && *c == 6),
            "{names:?}"
        );
        assert!(
            names
                .iter()
                .any(|(_, k, c)| *k == ProjectKind::Session && *c == 5),
            "{names:?}"
        );
        // most active first: the recent ones lead, the 200-day-old tax folder is last
        assert_eq!(ps.last().unwrap().suggested_name, "Taxes 2025");
        assert!(ps[0].activity_score >= ps[1].activity_score);
        assert!(!ps.iter().any(|p| p.suggested_name == "Misc"));
    }

    #[test]
    fn container_folders_descend() {
        let now = 1_700_000_000;
        let mut files = Vec::new();
        for c in ["acme", "globex", "initech"] {
            for i in 0..6 {
                files.push(f(&format!("/r/Clients/{c}/doc{i}.pdf"), now - i * 100));
            }
        }
        let ps = detect(Path::new("/r"), &files, now);
        let names: Vec<&str> = ps.iter().map(|p| p.suggested_name.as_str()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(
            names.contains(&"Acme") && names.contains(&"Globex") && names.contains(&"Initech"),
            "{names:?}"
        );
    }

    #[test]
    fn naming() {
        assert_eq!(
            pretty_name("client-x_offer sheets"),
            "Client X Offer Sheets"
        );
        assert_eq!(pretty_name("mtg-commander-sim"), "Mtg Commander Sim");
        assert_eq!(pretty_name("UnrealEngine"), "UnrealEngine");
    }
}
