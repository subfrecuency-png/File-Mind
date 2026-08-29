// Stamp each build so the CLI can tell when a running agent is stale.
fn main() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=FILEMIND_BUILD={ts}");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../core/src");
    println!("cargo:rerun-if-changed=../storage/src");
    println!("cargo:rerun-if-changed=../storage/migrations");
}
