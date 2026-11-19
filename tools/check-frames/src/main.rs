//! The app build's contract check against pigeon (pigeon spec 17.1 step 5, 18.1): every frame of pigeon's frame corpus
//! must pass this checkout's `app/packages/protocol` (`WSOutboundMessageSchema`, the zod-aot validators the bundle runs,
//! the status payload schemas; client frames against `WSInboundMessageSchema`), with this repository's harness
//! `testing/paseo-contract` (validate-frames.mjs --protocol-dir).
//!
//!   check-frames <work dir> [extra.ndjson...]
//!   check-frames --self-test <work dir> <seeded.ndjson>   passes only when the check rejects the seeded frame alone
//!
//! The corpus is the newest aviary-* release's asset pigeon-frames.ndjson; with no release yet the check runs on the
//! harness's frame fixtures alone, with a warning. The harness's fixtures of frames the old `CloudCLI` bridge sends
//! (fixtures/cloudcli-bridge-*.ndjson) and of pigeon-only frames (fixtures/pigeon-spec-frames.ndjson) are checked too;
//! extra frame logs (the workflow's seeded incompatible frame) are added. Exit status: validate-frames.mjs's (0 valid,
//! 1 a frame invalid, 2 setup error). `GH_TOKEN` (optional) for GitHub's API.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

mod verdict;

const API: &str = "https://api.github.com/repos/DE0CH/aviary";

fn repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("git rev-parse")?;
    if !out.status.success() {
        bail!("not in a git checkout");
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let mut r = ureq::get(url).header("User-Agent", "aviary-check-frames");
    if let Ok(tok) = std::env::var("GH_TOKEN")
        && !tok.is_empty()
    {
        r = r.header("Authorization", &format!("Bearer {tok}"));
    }
    r.call().with_context(|| format!("GET {url}"))
}

/// the newest aviary-* release's pigeon-frames.ndjson, written into `corpus`; false when there is no release yet
fn fetch_corpus(corpus: &Path) -> Result<bool> {
    let releases: serde_json::Value = get(&format!("{API}/releases?per_page=100"))?.body_mut().read_json()?;
    let newest = releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| r["tag_name"].as_str().is_some_and(|t| t.starts_with("aviary-")))
        .max_by_key(|r| r["created_at"].as_str().unwrap_or_default().to_string());
    let Some(release) = newest else { return Ok(false) };
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let url = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == "pigeon-frames.ndjson")
        .and_then(|a| a["browser_download_url"].as_str())
        .with_context(|| format!("release {tag} has no pigeon-frames.ndjson"))?;
    let body = get(url)?.body_mut().with_config().limit(1 << 30).read_to_vec()?;
    std::fs::create_dir_all(corpus)?;
    std::fs::write(corpus.join("pigeon-frames.ndjson"), body)?;
    println!("check-frames: pigeon's frame corpus of DE0CH/aviary {tag}");
    Ok(true)
}

/// the check with the seeded frame added, its output captured; the verdict decides
fn self_test(args: &[String]) -> Result<ExitCode> {
    let exe = std::env::current_exe()?;
    let out = Command::new(exe).args(args).output().context("the check")?;
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    print!("{log}");
    match verdict::verdict(out.status.code().unwrap_or(-1), &log) {
        Ok(msg) => {
            println!("{msg}");
            Ok(ExitCode::SUCCESS)
        }
        Err(msg) => {
            println!("::error::the frame check's self-test: {msg}");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn run() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--self-test") {
        return self_test(&args[1..]);
    }
    let Some((work, extra)) = args.split_first() else {
        bail!("usage: check-frames <work dir> [extra.ndjson...]")
    };
    let work = PathBuf::from(work);
    std::fs::create_dir_all(&work)?;
    let work = work.canonicalize()?;
    let mut frames = Vec::new();
    let corpus = work.join("corpus");
    if !corpus.join("pigeon-frames.ndjson").exists() && !fetch_corpus(&corpus)? {
        println!(
            "::warning::DE0CH/aviary has no aviary-* release yet, so there is no pigeon frame corpus: checking the harness's frame fixtures only"
        );
    }
    if corpus.join("pigeon-frames.ndjson").exists() {
        frames.push(corpus.join("pigeon-frames.ndjson"));
    }
    let root = repo_root()?;
    let harness = root.join("testing/paseo-contract");
    if !harness.join("node_modules").exists() {
        let ok = Command::new("npm")
            .args(["ci", "--no-audit", "--no-fund", "--silent"])
            .current_dir(&harness)
            .status()?;
        if !ok.success() {
            bail!("npm ci in testing/paseo-contract failed");
        }
    }
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(harness.join("fixtures"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            (n.starts_with("cloudcli-bridge-") && n.ends_with(".ndjson")) || n == "pigeon-spec-frames.ndjson"
        })
        .collect();
    fixtures.sort();
    frames.extend(fixtures);
    for e in extra {
        let p = PathBuf::from(e);
        if !p.is_file() {
            eprintln!("check-frames: no frame log {e}");
            return Ok(ExitCode::from(2));
        }
        frames.push(p.canonicalize()?);
    }
    for f in &frames {
        println!("check-frames: {}", f.display());
    }
    let status = Command::new("node")
        .arg("validate-frames.mjs")
        .arg("--protocol-dir")
        .arg(root.join("app/packages/protocol"))
        .args(&frames)
        .current_dir(&harness)
        .status()
        .context("node validate-frames.mjs")?;
    Ok(ExitCode::from(u8::try_from(status.code().unwrap_or(2)).unwrap_or(2)))
}

fn main() -> ExitCode {
    match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("check-frames: {e:#}");
            ExitCode::from(2)
        }
    }
}
