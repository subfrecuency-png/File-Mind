//! Shrink estimate — measure first, promise nothing.
//!
//! The estimator walks the index once (`Estimator::add` per file), sorts each
//! file into at most one candidate bucket, keeps a size-weighted random sample
//! per bucket, then reads a few 64 KiB windows of every sampled file and
//! compresses them in memory (zlib ≈ what APFS stores, zstd -19 for archives).
//! The measured ratio of each bucket, applied to the bucket's total bytes, is
//! the estimate. Nothing on disk is written or read beyond those windows.
//!
//! Buckets are disjoint so the tiers never count a byte twice:
//!
//! * tier 3 `cold_archive` — every file of a *cold* project (untouched for
//!   `projects::DORMANT_DAYS`) that has a folder on disk;
//! * tier 1 `apfs` — files on the APFS allow-list, not in a cold project,
//!   untouched for `COLD_AGE_DAYS`, at least `MIN_FILE_BYTES`;
//! * tier 2 `media_lossless` — JPEG/PNG not in a cold project. Their ratio is
//!   not measured (that needs the real JXL / oxipng encoders) — it uses the
//!   well-documented typical figures and says so (`measured: false`).
//!
//! Never counted: sensitive files, anything in a noise directory, `Library`,
//! app bundles, files smaller than a block.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Extensions tier 1 may rewrite. Text and formats that are stored raw on
/// disk. Already-compressed containers (zip, jpg, mp4, pdf…) are left out —
/// APFS would gain nothing and the rewrite would only churn.
pub const APFS_EXTS: &[&str] = &[
    // code
    "rs",
    "py",
    "js",
    "jsx",
    "ts",
    "tsx",
    "mjs",
    "cjs",
    "go",
    "java",
    "kt",
    "swift",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "cs",
    "rb",
    "php",
    "sh",
    "zsh",
    "bash",
    "fish",
    "ps1",
    "sql",
    "ipynb",
    "toml",
    "yaml",
    "yml",
    "lock",
    "css",
    "scss",
    "less",
    "html",
    "htm",
    "vue",
    "svelte",
    "svg",
    "xml",
    "plist",
    "gradle",
    "cmake",
    "mk",
    "make",
    "ini",
    "cfg",
    "conf",
    "properties",
    "env",
    "graphql",
    "proto",
    "tf",
    "dockerfile",
    "lua",
    "r",
    "m",
    "mm",
    "pl",
    "scala",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "vim",
    "el",
    // text and data
    "txt",
    "md",
    "markdown",
    "rst",
    "tex",
    "bib",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "ndjson",
    "geojson",
    "srt",
    "vtt",
    "ass",
    "rtf",
    "eml",
    "mbox",
    "ics",
    "vcf",
    "nfo",
    "org",
    "adoc",
    // stored-raw binary formats
    "bmp",
    "tif",
    "tiff",
    "wav",
    "aiff",
    "aif",
    "pcm",
    "psd",
    "ai",
    "eps",
    "ps",
    "sqlite",
    "db",
    "sqlite3",
    "dat",
    "bin",
    "iso",
    "tar",
    "obj",
    "stl",
    "ply",
    "fbx",
    "dae",
    "blend",
    "ttf",
    "otf",
    "map",
    "pdb",
    "a",
    "o",
    "dylib",
    "so",
    "wasm",
    "class",
];

/// JPEG → JPEG XL in lossless-transcode mode: 20–25 % smaller, bit-exact
/// reconstruction. Assumed until tier 2 measures it with the real encoder.
pub const JXL_LOSSLESS_JPEG_RATIO: f64 = 0.78;
/// PNG → optimised PNG (oxipng): typically 10–40 % smaller; 15 % assumed.
pub const PNG_OPTIMISED_RATIO: f64 = 0.85;
/// Files below one block are not worth an APFS rewrite.
pub const MIN_FILE_BYTES: u64 = 4096;
/// Tier 1 only targets files untouched for this long.
pub const COLD_AGE_DAYS: i64 = 30;
/// Files sampled per bucket. 48 × 3 windows × 64 KiB ≈ 9 MB read per bucket.
pub const SAMPLES_PER_BUCKET: usize = 48;
/// One sampling window.
pub const WINDOW_BYTES: usize = 64 * 1024;
/// Windows per sampled file (head, middle, tail).
pub const WINDOWS_PER_FILE: u64 = 3;
/// Above this ratio a bucket is treated as incompressible (APFS would not
/// even store the compressed form).
pub const INCOMPRESSIBLE_RATIO: f64 = 0.95;
/// zstd level used for the archive probe (matches the tier 3 archiver).
pub const ARCHIVE_ZSTD_LEVEL: i32 = 19;

/// One indexed file as the estimator sees it.
#[derive(Debug, Clone)]
pub struct FileIn<'a> {
    pub path: &'a Path,
    pub ext: Option<&'a str>,
    pub size: u64,
    pub mtime: i64,
    pub sensitive: bool,
    /// Effective category (`Category::as_str()` or `unclassified`).
    pub category: &'a str,
    /// The cold project this file belongs to, if any.
    pub cold_project: Option<i64>,
    /// Already carries an in-place rewrite (tier 1 done): never a candidate again.
    pub rewritten: bool,
}

/// A cold project, for the per-project breakdown of tier 3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIn {
    pub project_id: i64,
    pub name: String,
    pub root_path: Option<String>,
    pub end_ts: i64,
}

/// Compressed sizes of one sample.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Probe {
    /// Bytes actually read.
    pub bytes: u64,
    /// zlib (deflate level 6) — the ratio APFS transparent compression stores.
    pub zlib: u64,
    /// zstd -19 — the ratio the cold archive gets, before dictionaries.
    pub zstd: u64,
}

impl Probe {
    /// Compress `buf` both ways.
    pub fn of(buf: &[u8]) -> Probe {
        if buf.is_empty() {
            return Probe::default();
        }
        let zlib = {
            let mut e = flate2::write::ZlibEncoder::new(
                Vec::with_capacity(buf.len() / 2),
                flate2::Compression::default(),
            );
            match e.write_all(buf).and_then(|_| e.finish()) {
                Ok(v) => v.len(),
                Err(_) => buf.len(),
            }
        };
        let zstd = zstd::bulk::compress(buf, ARCHIVE_ZSTD_LEVEL)
            .map(|v| v.len())
            .unwrap_or(buf.len());
        Probe {
            bytes: buf.len() as u64,
            zlib: zlib.min(buf.len()) as u64,
            zstd: zstd.min(buf.len()) as u64,
        }
    }

    fn ratio(&self, kind: Kind) -> f64 {
        if self.bytes == 0 {
            return 1.0;
        }
        let c = match kind {
            Kind::Apfs => self.zlib,
            Kind::Archive => self.zstd,
            Kind::Media => self.bytes,
        };
        c as f64 / self.bytes as f64
    }
}

/// Read up to three windows of `path` (head, middle, tail) and compress them.
/// Small files are read whole.
pub fn probe_file(path: &Path, size: u64) -> std::io::Result<Probe> {
    let mut f = std::fs::File::open(path)?;
    let win = WINDOW_BYTES as u64;
    let mut buf = Vec::with_capacity((win * WINDOWS_PER_FILE) as usize);
    if size <= win * WINDOWS_PER_FILE {
        f.take(win * WINDOWS_PER_FILE).read_to_end(&mut buf)?;
    } else {
        let offsets = [0, size / 2 - win / 2, size - win];
        for off in offsets {
            f.seek(SeekFrom::Start(off))?;
            let mut chunk = vec![0u8; WINDOW_BYTES];
            let mut got = 0;
            while got < WINDOW_BYTES {
                let n = f.read(&mut chunk[got..])?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            buf.extend_from_slice(&chunk[..got]);
        }
    }
    Ok(Probe::of(&buf))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    Apfs,
    Media,
    Archive,
}

impl Kind {
    fn tier(self) -> u8 {
        match self {
            Kind::Apfs => 1,
            Kind::Media => 2,
            Kind::Archive => 3,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Kind::Apfs => "apfs",
            Kind::Media => "media_lossless",
            Kind::Archive => "cold_archive",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Apfs => "APFS transparent compression",
            Kind::Media => "lossless JPEG XL / PNG recompression",
            Kind::Archive => "cold-project archives",
        }
    }
    fn note(self) -> &'static str {
        match self {
            Kind::Apfs => "files stay ordinary files; only the on-disk footprint shrinks; reversible in place",
            Kind::Media => "typical ratios, not measured yet: JPEG → JPEG XL lossless transcode (bit-exact), PNG → optimised PNG; opt-in per category",
            Kind::Archive => "projects untouched for 180 days packed with zstd -19; search still finds files inside; one-click restore. Dictionaries and cross-archive dedup add to this",
        }
    }
}

/// Paths the shrink tiers never touch, whatever the index says.
pub fn excluded_path(path: &Path) -> bool {
    if crate::scanner::in_noise_dir(path) {
        return true;
    }
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "Library"
            || s.ends_with(".app")
            || s.ends_with(".framework")
            || s.ends_with(".photoslibrary")
    })
}

fn ext_in(ext: Option<&str>, list: &[&str]) -> bool {
    match ext {
        Some(e) => {
            let e = e.to_ascii_lowercase();
            list.iter().any(|x| *x == e)
        }
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BucketKey {
    kind: Kind,
    name: String,
}

/// Which bucket a file lands in, if any. Public so the rewrite tiers can use
/// the very same policy when they pick candidates.
pub fn bucket_for(f: &FileIn, now: i64) -> Option<(u8, String)> {
    key_for(f, now).map(|k| (k.kind.tier(), k.name))
}

fn key_for(f: &FileIn, now: i64) -> Option<BucketKey> {
    if f.sensitive || f.rewritten || f.size < MIN_FILE_BYTES || excluded_path(f.path) {
        return None;
    }
    if f.cold_project.is_some() {
        return Some(BucketKey {
            kind: Kind::Archive,
            name: f.category.to_string(),
        });
    }
    if ext_in(f.ext, &["jpg", "jpeg"]) {
        return Some(BucketKey {
            kind: Kind::Media,
            name: "jpeg".into(),
        });
    }
    if ext_in(f.ext, &["png"]) {
        return Some(BucketKey {
            kind: Kind::Media,
            name: "png".into(),
        });
    }
    if ext_in(f.ext, APFS_EXTS) && now - f.mtime >= COLD_AGE_DAYS * 86_400 {
        return Some(BucketKey {
            kind: Kind::Apfs,
            name: f.category.to_string(),
        });
    }
    None
}

/// xorshift64* — deterministic sampling without a `rand` dependency.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // 53 random bits → (0, 1]
        ((v >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0)
    }
}

#[derive(Debug, Default)]
struct Bucket {
    files: u64,
    bytes: u64,
    /// Weighted reservoir (Efraimidis–Spirakis A-Res): (key, path, size).
    sample: Vec<(f64, PathBuf, u64)>,
    probes: Vec<Probe>,
}

/// One bucket in the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketEstimate {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
    /// compressed / original for this bucket (1.0 = nothing to gain).
    pub ratio: f64,
    pub saving_bytes: u64,
    pub sampled_files: u64,
    pub sampled_bytes: u64,
}

/// One cold project in the tier 3 breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEstimate {
    pub project_id: i64,
    pub name: String,
    pub root_path: Option<String>,
    pub end_ts: i64,
    pub files: u64,
    pub bytes: u64,
    pub saving_bytes: u64,
}

/// One tier in the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierEstimate {
    pub tier: u8,
    pub kind: String,
    pub label: String,
    pub note: String,
    /// `false` when the ratio is a documented typical value, not a probe.
    pub measured: bool,
    pub candidate_files: u64,
    pub candidate_bytes: u64,
    pub ratio: f64,
    pub saving_bytes: u64,
    pub buckets: Vec<BucketEstimate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectEstimate>,
}

/// The whole report. Stored as JSON (`settings.shrink.estimate`) and shown
/// by `filemind shrink estimate` and the Overview tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Estimate {
    pub computed_ts: i64,
    pub elapsed_ms: u64,
    pub files_seen: u64,
    pub bytes_seen: u64,
    pub sampled_files: u64,
    pub sampled_bytes: u64,
    /// Sum over tiers. Tiers are disjoint, so this is what Shrink could reclaim.
    pub saving_bytes: u64,
    pub tiers: Vec<TierEstimate>,
}

impl Estimate {
    fn tier(&self, kind: &str) -> Option<&TierEstimate> {
        self.tiers.iter().find(|t| t.kind == kind)
    }

    /// "Shrink could reclaim ~X: A in code/text via APFS compression, …"
    pub fn headline(&self) -> String {
        use crate::health::human;
        if self.saving_bytes == 0 {
            return "Shrink found nothing worth reclaiming.".into();
        }
        let mut parts = Vec::new();
        if let Some(t) = self.tier("apfs").filter(|t| t.saving_bytes > 0) {
            parts.push(format!(
                "{} in code/text via APFS compression",
                human(t.saving_bytes)
            ));
        }
        if let Some(t) = self.tier("media_lossless").filter(|t| t.saving_bytes > 0) {
            parts.push(format!(
                "~{} in photos via lossless JPEG XL/PNG",
                human(t.saving_bytes)
            ));
        }
        if let Some(t) = self.tier("cold_archive").filter(|t| t.saving_bytes > 0) {
            let n = t.projects.len();
            parts.push(format!(
                "{} by archiving {} cold project{}",
                human(t.saving_bytes),
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
        format!(
            "Shrink could reclaim ~{}: {}.",
            human(self.saving_bytes),
            parts.join(", ")
        )
    }
}

/// Accumulates the index, picks the samples, computes the report.
pub struct Estimator {
    now: i64,
    rng: Rng,
    buckets: HashMap<BucketKey, Bucket>,
    /// (project, category) → (files, bytes)
    project_cats: HashMap<(i64, String), (u64, u64)>,
    files_seen: u64,
    bytes_seen: u64,
    samples_per_bucket: usize,
}

impl Estimator {
    pub fn new(now: i64, seed: u64) -> Estimator {
        Estimator {
            now,
            rng: Rng(seed | 1),
            buckets: HashMap::new(),
            project_cats: HashMap::new(),
            files_seen: 0,
            bytes_seen: 0,
            samples_per_bucket: SAMPLES_PER_BUCKET,
        }
    }

    /// Override the sample size (tests).
    pub fn with_samples(mut self, n: usize) -> Estimator {
        self.samples_per_bucket = n.max(1);
        self
    }

    /// Feed one indexed file.
    pub fn add(&mut self, f: &FileIn) {
        self.files_seen += 1;
        self.bytes_seen += f.size;
        let Some(key) = key_for(f, self.now) else {
            return;
        };
        if let Some(pid) = f.cold_project {
            let e = self
                .project_cats
                .entry((pid, f.category.to_string()))
                .or_default();
            e.0 += 1;
            e.1 += f.size;
        }
        let k = self.samples_per_bucket;
        let b = self.buckets.entry(key).or_default();
        b.files += 1;
        b.bytes += f.size;
        // A-Res: key = u^(1/w); keep the k largest keys → P(pick) ∝ size.
        let u = self.rng.next_f64();
        let key = u.powf(1.0 / (f.size.max(1) as f64));
        if b.sample.len() < k {
            b.sample.push((key, f.path.to_path_buf(), f.size));
        } else if let Some((i, _)) = b
            .sample
            .iter()
            .enumerate()
            .min_by(|a, c| a.1 .0.total_cmp(&c.1 .0))
        {
            if b.sample[i].0 < key {
                b.sample[i] = (key, f.path.to_path_buf(), f.size);
            }
        }
    }

    /// The files that need probing: (path, size).
    pub fn samples(&self) -> Vec<(PathBuf, u64)> {
        let mut keys: Vec<_> = self.buckets.keys().collect();
        keys.sort();
        keys.iter()
            .flat_map(|k| self.buckets[*k].sample.iter())
            .map(|(_, p, s)| (p.clone(), *s))
            .collect()
    }

    /// Number of sampled files across buckets (for progress bars).
    pub fn sample_count(&self) -> usize {
        self.buckets.values().map(|b| b.sample.len()).sum()
    }

    /// Probe every sample with `probe` (return `None` to skip an unreadable
    /// file) and build the report. `projects` supplies names for tier 3.
    pub fn finish<F>(mut self, projects: &[ProjectIn], mut probe: F) -> Estimate
    where
        F: FnMut(&Path, u64) -> Option<Probe>,
    {
        let t0 = std::time::Instant::now();
        let mut keys: Vec<_> = self.buckets.keys().cloned().collect();
        keys.sort();
        for k in &keys {
            let b = self.buckets.get_mut(k).unwrap();
            if k.kind == Kind::Media {
                continue; // not measured
            }
            let picks: Vec<(PathBuf, u64)> =
                b.sample.iter().map(|(_, p, s)| (p.clone(), *s)).collect();
            for (p, s) in picks {
                if let Some(pr) = probe(&p, s) {
                    if pr.bytes > 0 {
                        b.probes.push(pr);
                    }
                }
            }
        }

        let mut tiers: Vec<TierEstimate> = Vec::new();
        let mut sampled_files = 0u64;
        let mut sampled_bytes = 0u64;
        let mut ratios: HashMap<BucketKey, f64> = HashMap::new();
        for kind in [Kind::Apfs, Kind::Media, Kind::Archive] {
            let mut buckets = Vec::new();
            for k in keys.iter().filter(|k| k.kind == kind) {
                let b = &self.buckets[k];
                let ratio = match kind {
                    Kind::Media => match k.name.as_str() {
                        "jpeg" => JXL_LOSSLESS_JPEG_RATIO,
                        _ => PNG_OPTIMISED_RATIO,
                    },
                    _ => {
                        if b.probes.is_empty() {
                            1.0
                        } else {
                            let r = b.probes.iter().map(|p| p.ratio(kind)).sum::<f64>()
                                / b.probes.len() as f64;
                            if r > INCOMPRESSIBLE_RATIO {
                                1.0
                            } else {
                                r
                            }
                        }
                    }
                };
                ratios.insert(k.clone(), ratio);
                let sf = b.probes.len() as u64;
                let sb: u64 = b.probes.iter().map(|p| p.bytes).sum();
                sampled_files += sf;
                sampled_bytes += sb;
                buckets.push(BucketEstimate {
                    name: k.name.clone(),
                    files: b.files,
                    bytes: b.bytes,
                    ratio,
                    saving_bytes: ((b.bytes as f64) * (1.0 - ratio)).round() as u64,
                    sampled_files: sf,
                    sampled_bytes: sb,
                });
            }
            buckets.sort_by_key(|b| std::cmp::Reverse(b.saving_bytes));
            let candidate_files = buckets.iter().map(|b| b.files).sum();
            let candidate_bytes: u64 = buckets.iter().map(|b| b.bytes).sum();
            let saving_bytes: u64 = buckets.iter().map(|b| b.saving_bytes).sum();
            let mut projects_out = Vec::new();
            if kind == Kind::Archive {
                let mut per: HashMap<i64, (u64, u64, u64)> = HashMap::new();
                for ((pid, cat), (files, bytes)) in &self.project_cats {
                    let r = ratios
                        .get(&BucketKey {
                            kind: Kind::Archive,
                            name: cat.clone(),
                        })
                        .copied()
                        .unwrap_or(1.0);
                    let e = per.entry(*pid).or_default();
                    e.0 += files;
                    e.1 += bytes;
                    e.2 += ((*bytes as f64) * (1.0 - r)).round() as u64;
                }
                for (pid, (files, bytes, saving)) in per {
                    let p = projects.iter().find(|p| p.project_id == pid);
                    projects_out.push(ProjectEstimate {
                        project_id: pid,
                        name: p
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| format!("project {pid}")),
                        root_path: p.and_then(|p| p.root_path.clone()),
                        end_ts: p.map(|p| p.end_ts).unwrap_or(0),
                        files,
                        bytes,
                        saving_bytes: saving,
                    });
                }
                projects_out.sort_by_key(|b| std::cmp::Reverse(b.saving_bytes));
            }
            tiers.push(TierEstimate {
                tier: kind.tier(),
                kind: kind.key().into(),
                label: kind.label().into(),
                note: kind.note().into(),
                measured: kind != Kind::Media,
                candidate_files,
                candidate_bytes,
                ratio: if candidate_bytes == 0 {
                    1.0
                } else {
                    1.0 - saving_bytes as f64 / candidate_bytes as f64
                },
                saving_bytes,
                buckets,
                projects: projects_out,
            });
        }
        Estimate {
            computed_ts: self.now,
            elapsed_ms: t0.elapsed().as_millis() as u64,
            files_seen: self.files_seen,
            bytes_seen: self.bytes_seen,
            sampled_files,
            sampled_bytes,
            saving_bytes: tiers.iter().map(|t| t.saving_bytes).sum(),
            tiers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn file<'a>(
        path: &'a Path,
        ext: &'a str,
        size: u64,
        age_days: i64,
        cat: &'a str,
    ) -> FileIn<'a> {
        FileIn {
            path,
            ext: Some(ext),
            size,
            mtime: 1_800_000_000 - age_days * DAY,
            sensitive: false,
            category: cat,
            cold_project: None,
            rewritten: false,
        }
    }

    #[test]
    fn probe_distinguishes_text_from_noise() {
        let text = "the quick brown fox jumps over the lazy dog\n".repeat(2000);
        let p = Probe::of(text.as_bytes());
        assert!(p.zlib < p.bytes / 10, "{p:?}");
        assert!(p.zstd < p.bytes / 10, "{p:?}");
        let mut rng = Rng(42);
        let noise: Vec<u8> = (0..65_536)
            .map(|_| (rng.next_f64() * 256.0) as u8)
            .collect();
        let p = Probe::of(&noise);
        assert!(p.zlib as f64 > p.bytes as f64 * 0.98, "{p:?}");
        assert!(p.zstd as f64 > p.bytes as f64 * 0.98, "{p:?}");
    }

    #[test]
    fn probe_file_reads_windows_of_big_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.log");
        let line = b"2026-08-30T10:00:00Z INFO scheduler tick ok\n";
        let mut data = Vec::new();
        while data.len() < 2 * 1024 * 1024 {
            data.extend_from_slice(line);
        }
        std::fs::write(&p, &data).unwrap();
        let pr = probe_file(&p, data.len() as u64).unwrap();
        assert_eq!(pr.bytes, WINDOW_BYTES as u64 * WINDOWS_PER_FILE);
        assert!(pr.zlib < pr.bytes / 20);
        let small = dir.path().join("small.txt");
        std::fs::write(&small, b"hello hello hello hello").unwrap();
        let pr = probe_file(&small, 23).unwrap();
        assert_eq!(pr.bytes, 23);
    }

    #[test]
    fn buckets_are_disjoint_and_policy_holds() {
        let now = 1_800_000_000;
        let p = PathBuf::from("/Users/r/Documents/notes.md");
        assert_eq!(
            bucket_for(&file(&p, "md", 10_000, 60, "document"), now),
            Some((1, "document".into()))
        );
        // too fresh for tier 1
        assert_eq!(
            bucket_for(&file(&p, "md", 10_000, 2, "document"), now),
            None
        );
        // too small
        assert_eq!(bucket_for(&file(&p, "md", 100, 60, "document"), now), None);
        // compressed container never on the APFS list
        let z = PathBuf::from("/Users/r/Downloads/x.zip");
        assert_eq!(
            bucket_for(&file(&z, "zip", 10_000_000, 400, "archive"), now),
            None
        );
        // photos are tier 2 regardless of age
        let j = PathBuf::from("/Users/r/Pictures/a.jpg");
        assert_eq!(
            bucket_for(&file(&j, "JPG", 3_000_000, 1, "photo"), now),
            Some((2, "jpeg".into()))
        );
        // a cold project wins over everything
        let mut f = file(&j, "jpg", 3_000_000, 1, "photo");
        f.cold_project = Some(7);
        assert_eq!(bucket_for(&f, now), Some((3, "photo".into())));
        // never: sensitive, noise dirs, Library, app bundles
        let mut s = file(&p, "md", 10_000, 60, "document");
        s.sensitive = true;
        assert_eq!(bucket_for(&s, now), None);
        let mut done = file(&p, "md", 10_000, 60, "document");
        done.rewritten = true;
        assert_eq!(bucket_for(&done, now), None);
        let n = PathBuf::from("/Users/r/code/node_modules/a/index.js");
        assert_eq!(bucket_for(&file(&n, "js", 10_000, 60, "code"), now), None);
        let l = PathBuf::from("/Users/r/Library/Caches/x.log");
        assert_eq!(
            bucket_for(&file(&l, "log", 10_000, 60, "document"), now),
            None
        );
        let a = PathBuf::from("/Applications/Foo.app/Contents/Resources/x.plist");
        assert_eq!(
            bucket_for(&file(&a, "plist", 10_000, 60, "code"), now),
            None
        );
    }

    #[test]
    fn estimate_applies_sampled_ratio_to_bucket_totals() {
        let now = 1_800_000_000;
        let mut e = Estimator::new(now, 7).with_samples(4);
        let paths: Vec<PathBuf> = (0..20)
            .map(|i| PathBuf::from(format!("/Users/r/src/f{i}.rs")))
            .collect();
        for p in &paths {
            e.add(&file(p, "rs", 100_000, 90, "code"));
        }
        let jp = PathBuf::from("/Users/r/Pictures/a.jpg");
        e.add(&file(&jp, "jpg", 1_000_000, 1, "photo"));
        let cold: Vec<PathBuf> = (0..5)
            .map(|i| PathBuf::from(format!("/Users/r/old/proj/{i}.csv")))
            .collect();
        for p in &cold {
            let mut f = file(p, "csv", 200_000, 400, "data");
            f.cold_project = Some(3);
            e.add(&f);
        }
        assert_eq!(e.sample_count(), 4 + 1 + 4);
        let projects = vec![ProjectIn {
            project_id: 3,
            name: "old proj".into(),
            root_path: Some("/Users/r/old/proj".into()),
            end_ts: now - 400 * DAY,
        }];
        let est = e.finish(&projects, |_, size| {
            Some(Probe {
                bytes: size.min(1000),
                zlib: size.min(1000) / 4,
                zstd: size.min(1000) / 5,
            })
        });
        assert_eq!(est.files_seen, 26);
        let t1 = est.tiers.iter().find(|t| t.kind == "apfs").unwrap();
        assert_eq!(t1.candidate_files, 20);
        assert_eq!(t1.candidate_bytes, 2_000_000);
        assert_eq!(t1.saving_bytes, 1_500_000);
        assert!(t1.measured);
        let t2 = est
            .tiers
            .iter()
            .find(|t| t.kind == "media_lossless")
            .unwrap();
        assert!(!t2.measured);
        assert_eq!(t2.saving_bytes, 220_000);
        let t3 = est.tiers.iter().find(|t| t.kind == "cold_archive").unwrap();
        assert_eq!(t3.candidate_bytes, 1_000_000);
        assert_eq!(t3.saving_bytes, 800_000);
        assert_eq!(t3.projects.len(), 1);
        assert_eq!(t3.projects[0].name, "old proj");
        assert_eq!(t3.projects[0].saving_bytes, 800_000);
        assert_eq!(est.saving_bytes, 1_500_000 + 220_000 + 800_000);
        assert!(est.headline().starts_with("Shrink could reclaim ~"));
        assert!(
            est.headline().contains("1 cold project."),
            "{}",
            est.headline()
        );
        // round-trips through JSON (stored in settings)
        let s = serde_json::to_string(&est).unwrap();
        let back: Estimate = serde_json::from_str(&s).unwrap();
        assert_eq!(back.saving_bytes, est.saving_bytes);
    }

    #[test]
    fn incompressible_bucket_saves_nothing() {
        let now = 1_800_000_000;
        let mut e = Estimator::new(now, 1);
        let p = PathBuf::from("/Users/r/x/a.bin");
        e.add(&file(&p, "bin", 50_000, 90, "other"));
        let est = e.finish(&[], |_, s| {
            Some(Probe {
                bytes: s,
                zlib: s - 10,
                zstd: s - 10,
            })
        });
        assert_eq!(est.saving_bytes, 0);
        assert_eq!(est.headline(), "Shrink found nothing worth reclaiming.");
    }

    #[test]
    fn sampling_prefers_big_files() {
        let now = 1_800_000_000;
        let mut e = Estimator::new(now, 99).with_samples(3);
        let big: Vec<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("/Users/r/big{i}.txt")))
            .collect();
        let small: Vec<PathBuf> = (0..300)
            .map(|i| PathBuf::from(format!("/Users/r/small{i}.txt")))
            .collect();
        for p in &small {
            e.add(&file(p, "txt", 5_000, 90, "document"));
        }
        for p in &big {
            e.add(&file(p, "txt", 500_000_000, 90, "document"));
        }
        let picked = e.samples();
        assert_eq!(picked.len(), 3);
        assert!(
            picked
                .iter()
                .all(|(p, _)| p.to_string_lossy().contains("big")),
            "{picked:?}"
        );
    }
}
