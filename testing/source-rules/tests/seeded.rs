//! Each rule fails on a seeded violation (and passes on the matching clean tree).

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

use std::fs;
use std::path::{Path, PathBuf};

use aviary_source_rules::{clippy_rejects, dependency_violations, env_var_violations, name_violations};
use serde_json::json;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A fresh scratch directory under the system temp dir, unique per test.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aviary-source-rules-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn names_rule_catches_each_seeded_name() {
    let root = scratch("names");
    write(
        &root,
        "crates/pigeon/src/paseo/compat.rs",
        "pub const APP: &str = \"jarvis\"; // YYC app id\n",
    );
    write(
        &root,
        "crates/hermit/src/main.rs",
        "// YYC, JuiceFS, OpenRouter and gVisor belong here\n",
    );
    write(
        &root,
        "crates/magpie-import/src/lib.rs",
        "// reads the jarvis database\n",
    );
    write(
        &root,
        "crates/sandboxd-api/src/lib.rs",
        "pub struct Host { pub gvisor_release: String } // the host agent's own contract\n",
    );
    assert_eq!(name_violations(&root).unwrap(), vec![], "clean tree");

    write(&root, "crates/pigeon/src/routes.rs", "const A: &str = \"Jarvis\";\n");
    write(&root, "crates/magpie/src/store.rs", "\n// mounted with juicefs\n");
    write(&root, "crates/supervisor/src/fuse.rs", "// works under gVisor\n");
    write(
        &root,
        "crates/gateway-api/src/lib.rs",
        "pub const UPSTREAM: &str = \"openrouter.ai\";\n",
    );
    write(&root, "crates/hermit-api/fixtures/yyc.json", "{}\n");
    write(&root, "crates/magpie/Cargo.toml", "description = \"YYC's store\"\n");
    let found: Vec<String> = name_violations(&root)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        found,
        vec![
            "crates/gateway-api/src/lib.rs:1: names `openrouter`",
            "crates/hermit-api/fixtures/yyc.json: path names `yyc`",
            "crates/magpie/Cargo.toml:1: names `yyc`",
            "crates/magpie/src/store.rs:2: names `juicefs`",
            "crates/pigeon/src/routes.rs:1: names `jarvis`",
            "crates/supervisor/src/fuse.rs:1: names `gvisor`",
        ]
    );
}

#[test]
fn names_rule_allows_compat_names_only_in_compat_rs() {
    let root = scratch("compat");
    write(
        &root,
        "crates/pigeon/src/paseo/compat.rs",
        "pub const A: &str = \"yyc\";\n// gvisor\n",
    );
    let found: Vec<String> = name_violations(&root)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(found, vec!["crates/pigeon/src/paseo/compat.rs:2: names `gvisor`"]);
}

#[test]
fn env_rule_catches_seeded_read() {
    let root = scratch("env");
    write(
        &root,
        "crates/aviary-core/src/env.rs",
        "pub fn required(n: &str) { let _ = std::env::var(n); }\n",
    );
    write(
        &root,
        "crates/sandboxd/src/env.rs",
        "pub fn config() -> Option<String> { std::env::var(\"SBX_CONFIG\").ok() }\n",
    );
    write(
        &root,
        "crates/magpie/tests/db.rs",
        "fn url() { let _ = std::env::var(\"AVIARY_TEST_DATABASE_URL\"); }\n",
    );
    write(
        &root,
        "crates/pigeon/src/main.rs",
        "// std::env::var is not used here\nfn main() {}\n",
    );
    assert_eq!(env_var_violations(&root).unwrap(), vec![], "clean tree");

    write(
        &root,
        "crates/pigeon/src/config.rs",
        "use std::env;\nfn p() { let _ = env::var_os(\"PORT\"); }\n",
    );
    let found: Vec<String> = env_var_violations(&root)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        found,
        vec!["crates/pigeon/src/config.rs:2: reads the environment outside aviary_core::env"]
    );
}

/// A minimal `cargo metadata` document: `deps` maps each package to its dependencies.
fn metadata(root: &str, packages: &[(&str, &str)], deps: &[(&str, &[&str])], members: &[&str]) -> serde_json::Value {
    json!({
        "workspace_root": root,
        "workspace_members": members.iter().map(|m| format!("{m}-id")).collect::<Vec<_>>(),
        "packages": packages.iter().map(|(name, dir)| json!({
            "id": format!("{name}-id"), "name": name, "manifest_path": format!("{dir}/Cargo.toml"),
        })).collect::<Vec<_>>(),
        "resolve": {"nodes": deps.iter().map(|(name, ds)| json!({
            "id": format!("{name}-id"),
            "dependencies": ds.iter().map(|d| format!("{d}-id")).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()},
    })
}

#[test]
fn dependency_rule_catches_seeded_transitive_kube() {
    let packages = [
        ("hermit", "/r/crates/hermit"),
        ("magpie", "/r/crates/magpie"),
        ("shim", "/registry/shim"),
        ("kube", "/registry/kube"),
        ("aws-sdk-ec2", "/registry/aws-sdk-ec2"),
        ("fake-sandboxd", "/r/testing/fake-sandboxd"),
    ];
    let members = ["hermit", "magpie", "fake-sandboxd"];
    let clean: &[(&str, &[&str])] = &[
        ("hermit", &["kube", "aws-sdk-ec2"]),
        ("magpie", &[]),
        ("fake-sandboxd", &["kube"]),
        ("shim", &["kube"]),
        ("kube", &[]),
        ("aws-sdk-ec2", &[]),
    ];
    assert_eq!(
        dependency_violations(&metadata("/r", &packages, clean, &members)).unwrap(),
        vec![]
    );

    let seeded: &[(&str, &[&str])] = &[
        ("hermit", &["kube", "aws-sdk-ec2"]),
        ("magpie", &["shim", "aws-sdk-ec2"]),
        ("fake-sandboxd", &["kube"]),
        ("shim", &["kube"]),
        ("kube", &[]),
        ("aws-sdk-ec2", &[]),
    ];
    let found: Vec<String> = dependency_violations(&metadata("/r", &packages, seeded, &members))
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        found,
        vec!["magpie: depends on `aws-sdk-ec2`", "magpie: depends on `kube`"]
    );
}

#[test]
fn scratch_crates_get_the_workspace_lint_levels() {
    let lints = aviary_source_rules::workspace_lints_as_package_lints(&repo_root()).unwrap();
    assert!(
        lints.contains("[lints.clippy]\nall = { level = \"deny\", priority = -1 }"),
        "{lints}"
    );
    assert!(!lints.contains("[workspace"), "{lints}");
}

/// The rule comes from the repository's configuration alone: `clippy.toml`'s `disallowed-methods` at the level
/// `[workspace.lints.clippy] all = "deny"` gives it (no `-D` flag is passed).
#[test]
fn clippy_config_rejects_env_reads_and_allows_the_marked_one() {
    let root = repo_root();
    let cargo = env!("CARGO");
    let dir = scratch("clippy");
    let seeded =
        "//! seeded\n/// reads\n#[must_use]\npub fn port() -> Option<String> {\n    std::env::var(\"PORT\").ok()\n}\n";
    let stderr = clippy_rejects(cargo, &root, &dir.join("seeded"), seeded).unwrap();
    let stderr = stderr.expect("clippy accepted std::env::var");
    assert!(
        stderr.contains("error: use of a disallowed method `std::env::var`"),
        "unexpected clippy output: {stderr}"
    );

    let aliased = "//! seeded\nuse std::env::vars_os;\n/// counts\n#[must_use]\npub fn n() -> usize {\n    vars_os().count()\n}\n";
    let stderr = clippy_rejects(cargo, &root, &dir.join("aliased"), aliased).unwrap();
    let stderr = stderr.expect("clippy accepted the aliased std::env::vars_os");
    assert!(
        stderr.contains("error: use of a disallowed method `std::env::vars_os`"),
        "unexpected clippy output: {stderr}"
    );

    let allowed = "//! the env module\n/// reads\n#[must_use]\n#[allow(clippy::disallowed_methods)]\npub fn port() -> Option<String> {\n    std::env::var(\"PORT\").ok()\n}\n";
    assert_eq!(
        clippy_rejects(cargo, &root, &dir.join("allowed"), allowed).unwrap(),
        None
    );
}
