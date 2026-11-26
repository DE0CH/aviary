//! sandbox-image: the sandbox image's build (images/sandbox, platform 5.2 and 5.3).
//!
//!   sandbox-image build BIN TAG [REVISION]   builds ghcr.io/de0ch/hermit-sandbox from images/sandbox and the static
//!                                           `supervisor` and `supervisor-keeper` in BIN, loaded into the local Docker
//!                                           as TAG; REVISION (40 hex) defaults to the checkout's HEAD
//!
//! Run from the repository's root. Extra `docker buildx build` flags (a cache, build arguments) in
//! `SANDBOX_BUILD_FLAGS`, separated by white space; the commit's time from `SOURCE_DATE_EPOCH`, else git.

mod build;
mod elf;

use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

const DIR: &str = "images/sandbox";

fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("{cmd:?}"))?;
    if !out.status.success() {
        bail!("{cmd:?}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build(bin: &str, tag: &str, revision: Option<&str>) -> Result<()> {
    let bin = std::fs::canonicalize(bin).with_context(|| format!("the binaries' directory {bin}"))?;
    for b in ["supervisor", "supervisor-keeper"] {
        let p = bin.join(b);
        let bytes = std::fs::read(&p).with_context(|| format!("no {}", p.display()))?;
        if !elf::is_static(&bytes).with_context(|| p.display().to_string())? {
            bail!("{} is not static", p.display());
        }
    }
    let supervisor = bin.join("supervisor");
    let speaks = output(Command::new(&supervisor).arg("--speaks"))?;
    let sum = Sha256::digest(std::fs::read(&supervisor)?)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        });
    let revision = match revision {
        Some(r) => r.to_string(),
        None => output(Command::new("git").args(["rev-parse", "HEAD"]))?,
    };
    let epoch = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(e) if !e.is_empty() => e,
        _ => output(Command::new("git").args(["log", "-1", "--format=%ct"]))?,
    };
    let extra: Vec<String> = std::env::var("SANDBOX_BUILD_FLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();
    let bin = bin.to_str().context("the binaries' directory is not UTF-8")?;
    let a = build::args(&build::Build {
        dir: DIR,
        bin,
        tag,
        revision: &revision,
        speaks: &speaks,
        supervisor_sha256: &sum,
        epoch: &epoch,
        extra: &extra,
    });
    let status = Command::new("docker").args(&a).status().context("docker")?;
    if !status.success() {
        bail!("docker buildx build: {status}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["build", bin, tag] => build(bin, tag, None),
        ["build", bin, tag, revision] => build(bin, tag, Some(revision)),
        _ => bail!("usage: sandbox-image build BIN TAG [REVISION]"),
    }
}
