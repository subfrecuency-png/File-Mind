// Stamp each build so the CLI and the desktop app can tell when a running
// agent is stale. The stamp is a hash of every source file that ends up in
// the agent, so two compilations of the same code (the `filemind-agent`
// binary and the copy linked into the desktop app) agree, and any edit
// changes it.
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("rs" | "sql" | "toml")
        ) {
            out.push(p);
        }
    }
}

fn main() {
    let roots = [
        "src",
        "Cargo.toml",
        "../core/src",
        "../storage/src",
        "../storage/migrations",
        "../ai/src",
        "../adapter-macos/src",
        "../adapter-win/src",
    ];
    let mut files = Vec::new();
    for r in roots {
        let p = Path::new(r);
        if p.is_dir() {
            walk(p, &mut files);
        } else if p.is_file() {
            files.push(p.to_path_buf());
        }
        println!("cargo:rerun-if-changed={r}");
    }
    files.sort();
    // FNV-1a over path + contents: no dependency needed in a build script.
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for f in &files {
        mix(f.to_string_lossy().as_bytes());
        mix(&std::fs::read(f).unwrap_or_default());
    }
    println!("cargo:rustc-env=FILEMIND_BUILD={h:016x}");
}
