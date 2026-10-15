use sha2::{Digest, Sha256};
use std::path::Path;

fn hash_tree(path: &Path, hash: &mut Sha256) {
    let mut entries: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for entry in entries {
        hash.update(entry.file_name().unwrap().to_string_lossy().as_bytes());
        if entry.is_dir() {
            hash_tree(&entry, hash);
        } else {
            hash.update(std::fs::read(&entry).unwrap());
        }
    }
}

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut hash = Sha256::new();
    for relative in ["src", "../core/src"] {
        println!("cargo:rerun-if-changed={relative}");
        hash_tree(&manifest.join(relative), &mut hash);
    }
    for relative in [
        "Cargo.toml",
        "../core/Cargo.toml",
        "build.rs",
        "../Cargo.lock",
        // Bundle-only changes must also trigger installation of the new app.
        "../scripts/build.mjs",
        "../icons/icon.icns",
    ] {
        println!("cargo:rerun-if-changed={relative}");
        hash.update(std::fs::read(manifest.join(relative)).unwrap());
    }
    println!(
        "cargo:rustc-env=CRABCODE_COMPUTER_USE_BUILD={:x}",
        hash.finalize()
    );
}
