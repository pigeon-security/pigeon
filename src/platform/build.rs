//! Embeds the repository's sources the architecture is read from: each crate's Rust under
//! `src/*/src`, each `src/*/wrangler.toml` and the map of Workers `ci/workers.toml`, sorted by
//! path, so the page always matches the code it was built with.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let root = root.canonicalize().unwrap();
    let src = root.join("src");
    let mut files: Vec<PathBuf> = Vec::new();
    for krate in fs::read_dir(&src).unwrap().flatten() {
        let dir = krate.path();
        if !dir.is_dir() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", dir.display());
        let toml = dir.join("wrangler.toml");
        if toml.exists() {
            files.push(toml);
        }
        collect(&dir.join("src"), &mut files);
    }
    // The map of Workers, which says which Workers there are and how each deploys.
    let map = root.join("ci/workers.toml");
    if map.exists() {
        files.push(map);
    }
    files.sort();
    let mut out = String::from(
        "/// The sources, by path from the repository root.\npub const SOURCES: &[(&str, &str)] = &[\n",
    );
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        println!("cargo:rerun-if-changed={}", f.display());
        writeln!(
            out,
            "    ({rel:?}, include_str!({:?})),",
            f.display().to_string()
        )
        .unwrap();
    }
    out.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("sources.rs"),
        out,
    )
    .unwrap();
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            println!("cargo:rerun-if-changed={}", p.display());
            collect(&p, files);
        } else if p.extension().is_some_and(|x| x == "rs") {
            files.push(p);
        }
    }
}
