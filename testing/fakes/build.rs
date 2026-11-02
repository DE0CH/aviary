//! Embeds the contract crates' golden fixtures (`crates/{magpie,hermit,supervisor}-api/fixtures/**/*.json`) into
//! the library, so the fixture helpers work from a built binary or image outside the checkout.
#![allow(clippy::disallowed_methods)] // a build script reads cargo's own variables (CARGO_MANIFEST_DIR, OUT_DIR)

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|e| panic!("{e}")));
    let crates = manifest.join("../../crates");
    let mut table = String::from(
        "/// (crate, path under its fixtures directory, contents)\npub(crate) const FIXTURES: &[(&str, &str, &str)] = &[\n",
    );
    for krate in ["magpie-api", "hermit-api", "supervisor-api"] {
        let root = crates.join(krate).join("fixtures");
        println!("cargo::rerun-if-changed={}", root.display());
        let mut files = Vec::new();
        walk(&root, &mut files);
        for f in files {
            let rel = f.strip_prefix(&root).unwrap_or_else(|e| panic!("{e}"));
            let rel = rel
                .to_str()
                .unwrap_or_else(|| panic!("{}: not UTF-8", f.display()))
                .replace('\\', "/");
            let abs = f.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            let _ = writeln!(
                table,
                "    ({krate:?}, {rel:?}, include_str!({:?})),",
                abs.display().to_string()
            );
        }
    }
    table.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|e| panic!("{e}"))).join("fixtures.rs");
    std::fs::write(&out, table).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
