//! sandbox-image: the sandbox image's build (images/sandbox, platform 5.2 and 5.3).
//!
//!   sandbox-image build BIN TAG [REVISION]   builds ghcr.io/de0ch/hermit-sandbox from images/sandbox and the static
//!                                           `supervisor` and `supervisor-keeper` in BIN, loaded into the local Docker
//!                                           as TAG; REVISION (40 hex) defaults to the checkout's HEAD
//!   sandbox-image pins SNAPSHOT             moves the Dockerfile's pins forward: Debian's snapshot to SNAPSHOT
//!                                           (`YYYYMMDDThhmmssZ`), Docker's and gh's newest releases, and
//!                                           requirements.txt re-locked from requirements.in (`uv` on PATH)
//!
//! Run from the repository's root. Extra `docker buildx build` flags (a cache, build arguments) in
//! `SANDBOX_BUILD_FLAGS`, separated by white space; the commit's time from `SOURCE_DATE_EPOCH`, else git.

mod build;
mod debver;
mod elf;
mod pins;

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

fn get(url: &str) -> Result<String> {
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|_| url.starts_with("https://api.github.com/"));
    let mut last = None;
    for attempt in 1..=5u64 {
        let mut req = ureq::get(url).header("User-Agent", "aviary-sandbox-image");
        if let Some(t) = &token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(mut r) => return r.body_mut().read_to_string().with_context(|| format!("GET {url} body")),
            Err(e) => {
                eprintln!("GET {url}: {e} (try {attempt} of 5)");
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_secs(attempt * 5));
            }
        }
    }
    Err(anyhow::anyhow!("GET {url}: {last:?}"))
}

fn pins(snapshot: &str) -> Result<()> {
    let ok = snapshot.len() == 16
        && snapshot.as_bytes()[8] == b'T'
        && snapshot.ends_with('Z')
        && snapshot[..8]
            .bytes()
            .chain(snapshot[9..15].bytes())
            .all(|b| b.is_ascii_digit());
    if !ok {
        bail!("SNAPSHOT is YYYYMMDDThhmmssZ, not {snapshot}");
    }
    let path = format!("{DIR}/Dockerfile");
    let mut d = std::fs::read_to_string(&path)?;
    let set = |d: &mut String, name: &str, value: &str| -> Result<()> {
        if pins::arg(d, name).as_deref() != Some(value) {
            eprintln!("{name}: {} -> {value}", pins::arg(d, name).unwrap_or_default());
        }
        *d = pins::set_arg(d, name, value)?;
        Ok(())
    };
    set(&mut d, "DEBIAN_SNAPSHOT", snapshot)?;
    let docker = get("https://download.docker.com/linux/debian/dists/trixie/stable/binary-amd64/Packages")?;
    let newest = |p: &str| pins::newest(&docker, p).with_context(|| format!("no {p} in Docker's repository"));
    let ce = newest("docker-ce")?;
    if pins::newest(&docker, "docker-ce-cli").as_deref() != Some(ce.as_str()) {
        bail!("docker-ce-cli's newest is not docker-ce's {ce}: the two are pinned to one version");
    }
    set(&mut d, "DOCKER_VERSION", &ce)?;
    set(&mut d, "CONTAINERD_VERSION", &newest("containerd.io")?)?;
    set(&mut d, "BUILDX_VERSION", &newest("docker-buildx-plugin")?)?;
    let latest: serde_json::Value =
        serde_json::from_str(&get("https://api.github.com/repos/cli/cli/releases/latest")?)?;
    let tag = latest["tag_name"].as_str().context("gh's latest release has no tag")?;
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let sums = get(&format!(
        "https://github.com/cli/cli/releases/download/{tag}/gh_{v}_checksums.txt"
    ))?;
    let file = format!("gh_{v}_linux_amd64.tar.gz");
    let sum = pins::checksum(&sums, &file).with_context(|| format!("no sha256 for {file}"))?;
    set(&mut d, "GH_VERSION", v)?;
    set(&mut d, "GH_SHA256", &sum)?;
    std::fs::write(&path, d)?;
    // the Python packages: requirements.in's, each at its newest, for the image's Python (3.13) on trixie's glibc
    let status = Command::new("uv")
        .args([
            "pip",
            "compile",
            &format!("{DIR}/requirements.in"),
            "--upgrade",
            "--python-version",
            "3.13",
        ])
        .args([
            "--python-platform",
            "x86_64-manylinux_2_40",
            "--generate-hashes",
            "--no-header",
            "--quiet",
        ])
        .args(["-o", &format!("{DIR}/requirements.txt")])
        .status()
        .context("uv")?;
    if !status.success() {
        bail!("uv pip compile: {status}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["build", bin, tag] => build(bin, tag, None),
        ["build", bin, tag, revision] => build(bin, tag, Some(revision)),
        ["pins", snapshot] => pins(snapshot),
        _ => bail!("usage: sandbox-image build BIN TAG [REVISION] | pins SNAPSHOT"),
    }
}
