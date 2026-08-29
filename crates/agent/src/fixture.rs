//! Deterministic synthetic file tree for tests and benchmarks.
//!
//! Layout (for `entries` ≈ N):
//!   projects/<p>/...        ~70 % of files, grouped in "project" folders
//!   Downloads/              ~20 %, flat, with `name (1).ext` style duplicates
//!   versions/               ~5 %, `report_v1..vN` chains
//!   deep/a/b/c/...          depth 20 chain
//!   skip/.filemindignore    ignored folder
//!   loop -> ..              symlink loop (unix)
//!   Unicode and long names sprinkled in.

use anyhow::Result;
use std::fs;
use std::path::Path;

#[derive(Debug, Default)]
pub struct FixtureStats {
    pub files: usize,
    pub dirs: usize,
    pub duplicates: usize,
    pub version_chains: usize,
    pub links: usize,
}

/// Tiny deterministic PRNG so fixtures are reproducible without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const EXTS: [&str; 10] = [
    "pdf", "docx", "xlsx", "png", "jpg", "txt", "md", "csv", "zip", "mp4",
];
const WORDS: [&str; 12] = [
    "invoice",
    "contract",
    "budget",
    "photo",
    "screenshot",
    "notes",
    "draft",
    "final",
    "offer",
    "résumé",
    "設計",
    "plan",
];

fn write(path: &Path, rng: &mut Rng, stats: &mut FixtureStats) -> Result<Vec<u8>> {
    let len = 64 + rng.below(4096);
    let body: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
    fs::write(path, &body)?;
    stats.files += 1;
    Ok(body)
}

pub fn build(dir: &Path, entries: usize) -> Result<FixtureStats> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut s = FixtureStats::default();
    fs::create_dir_all(dir)?;

    // projects: 70 %
    let n_proj_files = entries * 7 / 10;
    let n_projects = (n_proj_files / 40).max(1);
    for p in 0..n_projects {
        let pdir = dir.join("projects").join(format!("project-{p:03}"));
        fs::create_dir_all(pdir.join("assets"))?;
        s.dirs += 2;
        for i in 0..(n_proj_files / n_projects) {
            let sub = if i % 3 == 0 {
                pdir.join("assets")
            } else {
                pdir.clone()
            };
            let name = format!(
                "{}-{}.{}",
                WORDS[rng.below(WORDS.len())],
                i,
                EXTS[rng.below(EXTS.len())]
            );
            write(&sub.join(name), &mut rng, &mut s)?;
        }
    }

    // downloads: 20 %, every 5th file an exact duplicate with "(1)" suffix
    let dl = dir.join("Downloads");
    fs::create_dir_all(&dl)?;
    s.dirs += 1;
    let n_dl = entries * 2 / 10;
    for i in 0..n_dl {
        let ext = EXTS[rng.below(EXTS.len())];
        let stem = format!("{}-{}", WORDS[rng.below(WORDS.len())], i);
        let body = write(&dl.join(format!("{stem}.{ext}")), &mut rng, &mut s)?;
        if i % 5 == 0 {
            fs::write(dl.join(format!("{stem} (1).{ext}")), &body)?;
            s.files += 1;
            s.duplicates += 1;
        }
    }

    // versions: 5 %, chains of 3–6
    let vdir = dir.join("versions");
    fs::create_dir_all(&vdir)?;
    s.dirs += 1;
    let mut left = entries * 5 / 100;
    let mut chain = 0;
    while left > 0 {
        let n = 3 + rng.below(4);
        let stem = format!("report-{chain}");
        for v in 1..=n {
            write(&vdir.join(format!("{stem}_v{v}.docx")), &mut rng, &mut s)?;
        }
        left = left.saturating_sub(n);
        chain += 1;
        s.version_chains += 1;
    }

    // deep chain
    let mut deep = dir.join("deep");
    for i in 0..20 {
        deep = deep.join(format!("d{i}"));
        s.dirs += 1;
    }
    fs::create_dir_all(&deep)?;
    write(&deep.join("bottom.txt"), &mut rng, &mut s)?;

    // long name (200 chars) and ignored folder
    let long = format!("{}.txt", "long-".repeat(40));
    write(&dir.join(long), &mut rng, &mut s)?;
    let skip = dir.join("skip");
    fs::create_dir_all(&skip)?;
    s.dirs += 1;
    fs::write(skip.join(".filemindignore"), b"")?;
    write(&skip.join("ignored.txt"), &mut rng, &mut s)?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("..", dir.join("loop"))?;
        std::os::unix::fs::symlink("/etc", dir.join("etc-link"))?;
        s.links += 2;
    }

    Ok(s)
}
