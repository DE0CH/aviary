//! The tx rule (platform 4.3), compile-fail style: clippy, configured as CI's second lint pass
//! (`CLIPPY_CONF_DIR=crates/aviary-pg/tx-rule`, `-A clippy::disallowed_methods -D warnings`), rejects a `reqwest` call
//! inside the closure of `aviary_pg::tx!` and accepts the same call outside it. Each case is a scratch crate depending
//! on this crate and on the workspace's reqwest, checked offline with the workspace's lockfile and lint levels.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn tx_conf_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tx-rule")
}

/// The `[workspace.dependencies]` line for `name`, verbatim.
fn workspace_dep(root: &Path, name: &str) -> String {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    manifest
        .lines()
        .find(|l| l.starts_with(&format!("{name} = ")))
        .unwrap_or_else(|| panic!("no workspace dependency {name}"))
        .to_owned()
}

/// Runs clippy over a scratch crate whose `lib.rs` is `source`; `Some(stderr)` when it was rejected.
fn clippy(case: &str, source: &str, tx_conf: bool) -> Option<String> {
    let root = repo_root();
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tx-rule");
    let dir = base.join(case);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = format!(
        "[package]\nname = \"scratch-{case}\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[dependencies]\naviary-pg = {{ path = {:?} }}\n{}\n{}\n\n{}",
        Path::new(env!("CARGO_MANIFEST_DIR")).display().to_string(),
        workspace_dep(&root, "reqwest"),
        workspace_dep(&root, "tokio"),
        aviary_source_rules::workspace_lints_as_package_lints(&root).unwrap(),
    );
    fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    fs::copy(root.join("rust-toolchain.toml"), dir.join("rust-toolchain.toml")).unwrap();
    fs::write(dir.join("src/lib.rs"), source).unwrap();
    let conf = if tx_conf { tx_conf_dir() } else { root.clone() };
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["clippy", "--quiet", "--offline"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", base.join("target"))
        .env("CLIPPY_CONF_DIR", &conf)
        .env_remove("RUSTC_WRAPPER")
        .env("CARGO_TERM_COLOR", "never");
    if tx_conf {
        cmd.args(["--", "-A", "clippy::disallowed_methods", "-D", "warnings"]);
    } else {
        cmd.args(["--", "-D", "warnings"]);
    }
    let out = cmd.output().unwrap();
    if out.status.success() {
        None
    } else {
        Some(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

const INSIDE: &str = r#"//! seeded: a network call inside a transaction
/// Records a thing after asking another service about it.
///
/// # Errors
/// When the database fails.
pub async fn record(pool: &aviary_pg::Pool, http: &reqwest::Client) -> Result<u64, aviary_pg::Error> {
    aviary_pg::tx!(pool, |t| {
        let n = t.execute("insert into things default values", &[]).await?;
        let _ = http.post("http://hermit/v1/things").send().await;
        Ok(n)
    })
    .await
}
"#;

const INSIDE_TYPED: &str = r#"//! seeded: a network call inside a transaction with the caller's error type
/// The caller's error.
#[derive(Debug)]
pub struct Mine;
impl From<aviary_pg::Error> for Mine {
    fn from(_: aviary_pg::Error) -> Self {
        Self
    }
}
/// Reads a body inside a transaction.
///
/// # Errors
/// When anything fails.
pub async fn record(pool: &aviary_pg::Pool, http: &reqwest::Client) -> Result<String, Mine> {
    let resp = http.get("http://magpie/v1/x").send().await.map_err(|_| Mine)?;
    aviary_pg::tx!(pool, |t| -> Result<String, Mine> {
        t.execute("select 1", &[]).await?;
        resp.text().await.map_err(|_| Mine)
    })
    .await
}
"#;

const OUTSIDE: &str = r#"//! the same calls outside the transaction
/// Records a thing, then tells another service.
///
/// # Errors
/// When the database fails.
pub async fn record(pool: &aviary_pg::Pool, http: &reqwest::Client) -> Result<u64, aviary_pg::Error> {
    let n = aviary_pg::tx!(pool, |t| t.execute("insert into things default values", &[]).await).await?;
    let _ = http.post("http://hermit/v1/things").send().await;
    let _ = reqwest::get("http://hermit/v1/things").await;
    Ok(n)
}
"#;

#[test]
fn reqwest_inside_tx_is_rejected_and_outside_is_accepted() {
    let stderr = clippy("inside", INSIDE, true).expect("clippy accepted a reqwest call inside aviary_pg::tx!");
    for method in ["reqwest::Client::post", "reqwest::RequestBuilder::send"] {
        assert!(
            stderr.contains(&format!("error: use of a disallowed method `{method}`")),
            "{method} not reported: {stderr}"
        );
    }
    assert!(stderr.contains("no network call inside aviary_pg::tx!"), "{stderr}");

    let stderr = clippy("typed", INSIDE_TYPED, true).expect("clippy accepted a reqwest body read inside the typed tx!");
    assert!(
        stderr.contains("error: use of a disallowed method `reqwest::Response::text`"),
        "{stderr}"
    );
    // the call before the transaction is not reported
    assert!(!stderr.contains("reqwest::Client::get"), "{stderr}");

    assert_eq!(
        clippy("outside", OUTSIDE, true),
        None,
        "the tx rule rejected calls outside a transaction"
    );
}

#[test]
fn the_main_configuration_does_not_forbid_reqwest() {
    assert_eq!(clippy("main", OUTSIDE, false), None);
    // and the main pass alone does not see the tx rule: the second pass is needed
    assert_eq!(clippy("main-inside", INSIDE, false), None);
}

/// The tx rule's configuration is the repository's `clippy.toml` plus the tx entries, so the second pass judges
/// everything else exactly as the first.
#[test]
fn the_tx_configuration_extends_the_main_one() {
    let main = fs::read_to_string(repo_root().join("clippy.toml")).unwrap();
    let tx = fs::read_to_string(tx_conf_dir().join("clippy.toml")).unwrap();
    let main = main
        .trim_end()
        .strip_suffix(']')
        .expect("clippy.toml ends with the disallowed-methods list");
    let tail = tx
        .split_once(main)
        .map(|(_, t)| t)
        .expect("tx-rule/clippy.toml does not contain clippy.toml verbatim");
    assert!(tail.starts_with("  # the tx rule\n"), "{tail}");
    for line in tail.lines().skip(1).filter(|l| *l != "]") {
        assert!(
            line.contains("path = \"reqwest::") && line.contains("allow-invalid = true"),
            "{line}"
        );
    }
}
