//! Renders a blog consumer repository to a directory, as a deploy would, for a local preview:
//! `cargo run -p pigeon-capabilities --example blog --target x86_64-unknown-linux-gnu -- <repo> <out> [base_url]`.
//! Static files are copied as they are. See `docs/plans/blog.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pigeon_core::blog::{Input, render};

fn markdown_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_file() {
            out.push((name, fs::read_to_string(entry.path()).unwrap_or_default()));
        }
    }
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.insert(rel.to_string_lossy().into_owned());
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(repo), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: blog <repo> <out> [base_url]");
        return ExitCode::FAILURE;
    };
    let repo = PathBuf::from(repo);
    let out = PathBuf::from(out);
    let mut statics = BTreeSet::new();
    walk(&repo.join("static"), &repo.join("static"), &mut statics);
    let input = Input {
        site_toml: fs::read_to_string(repo.join(".pigeon/site.toml")).unwrap_or_default(),
        posts: markdown_files(&repo.join("content/posts")),
        pages: markdown_files(&repo.join("content/pages")),
        statics,
        base_url: args
            .get(2)
            .cloned()
            .unwrap_or_else(|| "https://example.com".into()),
        commit: "local".into(),
        commit_year: 2026,
    };
    let site = match render(&input) {
        Ok(site) => site,
        Err(problems) => {
            for p in problems {
                eprintln!("{p}");
            }
            return ExitCode::FAILURE;
        }
    };
    let write = |path: &Path, bytes: &[u8]| {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, bytes)
    };
    for (path, (bytes, _)) in &site.files {
        if let Err(e) = write(&out.join(path), bytes) {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    for path in &site.statics {
        if let Err(e) =
            fs::read(repo.join("static").join(path)).and_then(|b| write(&out.join(path), &b))
        {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!(
        "{} {} ({}): {} files rendered, {} static",
        site.template,
        site.version,
        &site.digest[..12],
        site.files.len(),
        site.statics.len()
    );
    ExitCode::SUCCESS
}
