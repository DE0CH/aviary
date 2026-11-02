//! The source rules of platform 4.5, as functions over a repository tree, so the tests can run them against the real
//! repository (`tests/repo.rs`) and against seeded violations (`tests/seeded.rs`).
//!
//! - No `std::env::var*` outside `aviary-core::env`: enforced by the `disallowed-methods` entries in `clippy.toml`
//!   (checked by `clippy_rejects`), backed by a text scan of non-test sources ([`env_var_violations`]).
//! - No crate under `crates/` except `hermit` depends on `aws-sdk-ec2` or `kube` ([`dependency_violations`], over
//!   `cargo metadata`; `deny.toml` bans the same with wrappers).
//! - `crates/pigeon`, `crates/magpie`, `crates/supervisor` and `crates/*-api` never name `yyc`, `juicefs`,
//!   `openrouter` or `gvisor`, and in `crates/pigeon` the literals `jarvis` and `yyc` appear only in
//!   `src/paseo/compat.rs` ([`name_violations`]).

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One broken rule: where, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Path relative to the repository root (or the crate name for dependency rules).
    pub place: String,
    /// 1-based line, 0 when the rule is not about a line.
    pub line: usize,
    /// What was found.
    pub what: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}", self.place, self.what)
        } else {
            write!(f, "{}:{}: {}", self.place, self.line, self.what)
        }
    }
}

/// Names that only deployment-specific code may use (platform 4.5).
pub const DEPLOYMENT_NAMES: &[&str] = &["yyc", "juicefs", "openrouter", "gvisor"];

/// Names pigeon may use only in its frozen wire constants.
pub const PIGEON_COMPAT_NAMES: &[&str] = &["jarvis", "yyc"];

/// The one pigeon file allowed to hold [`PIGEON_COMPAT_NAMES`].
pub const PIGEON_COMPAT_FILE: &str = "crates/pigeon/src/paseo/compat.rs";

/// The one file allowed to read the process environment.
pub const ENV_FILE: &str = "crates/aviary-core/src/env.rs";

/// Crates no crate under `crates/` other than hermit may depend on, directly or not.
pub const HERMIT_ONLY_CRATES: &[&str] = &["aws-sdk-ec2", "kube", "kube-client", "kube-runtime"];

/// The crate directories the deployment-name rule covers: `crates/pigeon`, `crates/magpie`, `crates/supervisor`
/// and every `crates/*-api`.
///
/// # Errors
/// When `crates/` cannot be listed.
pub fn neutral_crate_dirs(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let crates = root.join("crates");
    if !crates.is_dir() {
        return Ok(dirs);
    }
    for entry in fs::read_dir(&crates)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir()
            && (matches!(name.as_str(), "pigeon" | "magpie" | "supervisor") || name.ends_with("-api"))
        {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if entry.file_name() == "target" || entry.file_name() == "node_modules" {
                continue;
            }
            files_under(&path, out)?;
        } else if kind.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every forbidden name in the covered crates: in file contents (case-insensitive, any line) and in file paths.
///
/// # Errors
/// When a directory cannot be read. Files that are not UTF-8 are scanned lossily.
pub fn name_violations(root: &Path) -> io::Result<Vec<Violation>> {
    let mut found = Vec::new();
    for dir in neutral_crate_dirs(root)? {
        let in_pigeon = dir.file_name().is_some_and(|n| n == "pigeon");
        let mut files = Vec::new();
        files_under(&dir, &mut files)?;
        files.sort();
        for file in files {
            let rel = relative(root, &file);
            let mut names: BTreeSet<&str> = DEPLOYMENT_NAMES.iter().copied().collect();
            if in_pigeon {
                names.extend(PIGEON_COMPAT_NAMES.iter().copied());
                if rel == PIGEON_COMPAT_FILE {
                    for n in PIGEON_COMPAT_NAMES {
                        names.remove(n);
                    }
                }
            }
            let rel_in_crate = relative(&dir, &file).to_lowercase();
            for name in &names {
                if rel_in_crate.contains(name) {
                    found.push(Violation {
                        place: rel.clone(),
                        line: 0,
                        what: format!("path names `{name}`"),
                    });
                }
            }
            let text = String::from_utf8_lossy(&fs::read(&file)?).to_lowercase();
            for (i, line) in text.lines().enumerate() {
                for name in &names {
                    if line.contains(name) {
                        found.push(Violation {
                            place: rel.clone(),
                            line: i + 1,
                            what: format!("names `{name}`"),
                        });
                    }
                }
            }
        }
    }
    Ok(found)
}

/// Is this Rust file test-only code (an integration test, bench or example, or anything under `testing/`)?
fn is_test_source(rel: &str) -> bool {
    rel.starts_with("testing/")
        || rel
            .split('/')
            .any(|part| matches!(part, "tests" | "benches" | "examples"))
}

/// Every `env::var`, `env::var_os`, `env::vars` or `env::vars_os` in non-test Rust sources outside [`ENV_FILE`].
/// The clippy `disallowed-methods` entries are the full check (they also see `use std::env::var;` aliases); this scan
/// keeps the rule visible even where clippy is told to allow it.
///
/// # Errors
/// When a directory cannot be read.
pub fn env_var_violations(root: &Path) -> io::Result<Vec<Violation>> {
    let mut found = Vec::new();
    let crates = root.join("crates");
    if !crates.is_dir() {
        return Ok(found);
    }
    let mut files = Vec::new();
    files_under(&crates, &mut files)?;
    files.sort();
    for file in files {
        let rel = relative(root, &file);
        if file.extension().is_none_or(|e| e != "rs") || rel == ENV_FILE || is_test_source(&rel) {
            continue;
        }
        let text = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if code.contains("env::var") {
                found.push(Violation {
                    place: rel.clone(),
                    line: i + 1,
                    what: "reads the environment outside aviary_core::env".to_owned(),
                });
            }
        }
    }
    Ok(found)
}

/// Every crate under `crates/` other than hermit whose dependency closure (all kinds) holds one of
/// [`HERMIT_ONLY_CRATES`], from `cargo metadata --format-version 1` output.
///
/// # Errors
/// When the metadata does not have the expected shape.
pub fn dependency_violations(metadata: &serde_json::Value) -> Result<Vec<Violation>, String> {
    let packages = metadata["packages"].as_array().ok_or("metadata has no packages")?;
    let root = metadata["workspace_root"]
        .as_str()
        .ok_or("metadata has no workspace_root")?;
    let crates_dir = Path::new(root).join("crates");
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("metadata has no workspace_members")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut name_of: HashMap<&str, &str> = HashMap::new();
    let mut manifest_of: HashMap<&str, &str> = HashMap::new();
    for p in packages {
        let id = p["id"].as_str().ok_or("package without id")?;
        name_of.insert(id, p["name"].as_str().ok_or("package without name")?);
        manifest_of.insert(id, p["manifest_path"].as_str().ok_or("package without manifest_path")?);
    }
    let mut deps_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata has no resolve.nodes")?
    {
        let id = node["id"].as_str().ok_or("node without id")?;
        let deps = node["dependencies"].as_array().ok_or("node without dependencies")?;
        deps_of.insert(id, deps.iter().filter_map(serde_json::Value::as_str).collect());
    }
    let mut found = Vec::new();
    for member in &members {
        let name = name_of.get(member).copied().unwrap_or_default();
        let under_crates = manifest_of
            .get(member)
            .is_some_and(|m| Path::new(m).starts_with(&crates_dir));
        if !under_crates || name == "hermit" {
            continue;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack = vec![*member];
        while let Some(id) = stack.pop() {
            for dep in deps_of.get(id).into_iter().flatten() {
                if seen.insert(dep) {
                    stack.push(dep);
                }
            }
        }
        let banned: BTreeSet<&str> = seen
            .iter()
            .filter_map(|id| name_of.get(id).copied())
            .filter(|n| HERMIT_ONLY_CRATES.contains(n))
            .collect();
        for b in banned {
            found.push(Violation {
                place: name.to_owned(),
                line: 0,
                what: format!("depends on `{b}`"),
            });
        }
    }
    Ok(found)
}

/// Runs `cargo metadata` for the workspace at `root` with the given cargo binary.
///
/// # Errors
/// When cargo fails or prints something that is not JSON.
pub fn cargo_metadata(cargo: &str, root: &Path) -> Result<serde_json::Value, String> {
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("running {cargo} metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))
}

/// The root manifest's `[workspace.lints.*]` tables rewritten as a package's `[lints.*]` tables.
///
/// # Errors
/// When the root manifest cannot be read or has no `[workspace.lints.*]` table.
pub fn workspace_lints_as_package_lints(root: &Path) -> io::Result<String> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    let mut out = String::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            inside = trimmed.starts_with("[workspace.lints.");
        }
        if inside {
            out.push_str(&line.replacen("[workspace.lints.", "[lints.", 1));
            out.push('\n');
        }
    }
    if out.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Cargo.toml has no [workspace.lints.*] table",
        ));
    }
    Ok(out)
}

/// Runs `cargo clippy` over a scratch crate whose `lib.rs` is `source`, in `scratch` (created), with the
/// repository's `clippy.toml`, `rust-toolchain.toml` and lint levels (the root manifest's `[workspace.lints]`, copied
/// as the crate's `[lints]`), so a rejection comes from the repository's own configuration. Returns
/// `Ok(Some(stderr))` when clippy rejected the source, `Ok(None)` when it passed.
///
/// # Errors
/// When the scratch crate cannot be written or cargo cannot be started.
pub fn clippy_rejects(cargo: &str, root: &Path, scratch: &Path, source: &str) -> io::Result<Option<String>> {
    fs::create_dir_all(scratch.join("src"))?;
    fs::write(
        scratch.join("Cargo.toml"),
        format!(
            "[package]\nname = \"scratch\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n{}",
            workspace_lints_as_package_lints(root)?
        ),
    )?;
    fs::copy(root.join("clippy.toml"), scratch.join("clippy.toml"))?;
    fs::copy(root.join("rust-toolchain.toml"), scratch.join("rust-toolchain.toml"))?;
    fs::write(scratch.join("src/lib.rs"), source)?;
    let out = Command::new(cargo)
        .args(["clippy", "--quiet", "--offline"])
        .current_dir(scratch)
        .env("CARGO_TARGET_DIR", scratch.join("target"))
        .env_remove("RUSTC_WRAPPER")
        .output()?;
    if out.status.success() {
        Ok(None)
    } else {
        Ok(Some(String::from_utf8_lossy(&out.stderr).into_owned()))
    }
}
